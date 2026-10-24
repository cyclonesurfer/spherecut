//! Frame-exact seeking on FFmpeg's software decoder (M1 plan section 3.3,
//! DECISIONS D29): [`FfmpegVideo`], the [`VideoDecode`] that
//! [`FfmpegBackend`](crate::backend::FfmpegBackend) opens, on top of
//! [`VideoDecoder`].
//!
//! `seek(at, Exact)` returns the frame with the greatest pts at or before
//! T = origin + ⌊at / tb⌋, later frames winning ties. A BACKWARD seek can
//! land on a keyframe whose dts is at or before T but whose pts is after
//! it: its leading pictures (HEVC RASL pictures after a CRA, open-GOP
//! B-frames) are then dropped without a word, and so would be the target.
//! So when the first frame out is after T, it seeks again to just before
//! the landing keyframe, up to [`MAX_ATTEMPTS`] times, and then decodes
//! from the start. A target in the GOP being decoded is reached by
//! decoding forward, with no seek at all.
//!
//! Seeks stop through a [`StopToken`] at packet and frame boundaries,
//! which leaves the demuxer in a known state; the decoder then seeks
//! again before it goes on. FFmpeg's interrupt flag is for shutdown only
//! (D13).

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use spherecut_core::video::PixelLayout;
use spherecut_core::{FrameRate, Rational, Rounding, StopToken, Time};

use crate::backend::{
    DecodePath, DecodeStats, DecoderEvent, Exactness, NextFrame, SeekCost, SeekMode, SeekOutcome,
    SourceMapping, Timing, VideoDecode, VideoFrame, VideoRequest, VideoStreamInfo,
};
use crate::color::{self, ColorContext};
use crate::decode::{Received, VideoDecoder};
use crate::ffi::{self, FrameFields, Scaler};
use crate::mp4::{self, FirstBox};
use crate::spherical::{self, Projection, StereoMode};
use crate::{MediaError, ff};

/// Seeks to before the landing keyframe, before decoding from the start.
pub const MAX_ATTEMPTS: u32 = 8;

/// The rates a Matroska track's default duration is snapped to: FFmpeg
/// reads it as a rate reduced to a denominator of at most 30000, which is
/// a little off for the NTSC rates (59.94 reads as 19001/317).
const STANDARD_RATES: [FrameRate; 8] = [
    FrameRate::FPS_23_976,
    FrameRate::FPS_24,
    FrameRate::FPS_25,
    FrameRate::FPS_29_97,
    FrameRate::FPS_30,
    FrameRate::FPS_50,
    FrameRate::FPS_59_94,
    FrameRate::FPS_60,
];

/// A decoded frame not yet handed out.
struct Raw {
    frame: ff::frame::Video,
    fields: FrameFields,
    pts: i64,
    /// Its place in decode-output order since the last seek.
    seq: u64,
    /// It came out after reading had failed, as the decoder drained: a
    /// frame the damage lost may come before it.
    after_failure: bool,
}

/// How the decoder's frames become [`VideoFrame`]s.
enum Conversion {
    /// Uploaded as they are.
    Native {
        source: (i32, u32, u32),
        layout: Arc<PixelLayout>,
    },
    /// Through swscale.
    Scaled {
        scaler: Scaler,
        layout: Arc<PixelLayout>,
    },
}

impl Conversion {
    fn source(&self) -> (i32, u32, u32) {
        match self {
            Conversion::Native { source, .. } => *source,
            Conversion::Scaled { scaler, .. } => scaler.source(),
        }
    }

    fn layout(&self) -> &Arc<PixelLayout> {
        match self {
            Conversion::Native { layout, .. } | Conversion::Scaled { layout, .. } => layout,
        }
    }

    /// The conversion for frames of format `fmt` at `width` × `height`.
    fn new(path: &Path, fmt: i32, width: u32, height: u32) -> Result<Self, MediaError> {
        let unsupported = || MediaError::UnsupportedFormat {
            path: path.to_path_buf(),
            pix_fmt: ffi::pix_fmt_name(fmt).unwrap_or_else(|| format!("format {fmt}")),
        };
        if let Some(layout) = ffi::pixel_layout(fmt) {
            return Ok(Conversion::Native {
                source: (fmt, width, height),
                layout: Arc::new(layout),
            });
        }
        let to = ffi::conversion_target(fmt).ok_or_else(unsupported)?;
        let layout = ffi::pixel_layout(to).ok_or_else(unsupported)?;
        let scaler = Scaler::new(fmt, to, width, height).map_err(|e| {
            tracing::debug!(path = %path.display(), "{e}");
            unsupported()
        })?;
        Ok(Conversion::Scaled {
            scaler,
            layout: Arc::new(layout),
        })
    }
}

/// Where decoding goes on from, against the last frame returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// The next frame decoded follows `pending`, or the last frame
    /// returned.
    InStep,
    /// A seek stopped part way: seek again to just after the last frame
    /// returned (or to the start) before decoding on.
    Lost,
}

/// [`VideoDecode`] on FFmpeg's software decoder.
pub struct FfmpegVideo {
    dec: VideoDecoder,
    info: VideoStreamInfo,
    decode_path: DecodePath,
    conv: Conversion,
    /// FFmpeg's name for the decoder's pixel format, and its codec name,
    /// for the colour rules.
    source_name: String,
    /// The first frame after the last one returned, when it has been
    /// decoded.
    pending: Option<Raw>,
    /// The last frame returned.
    current: Option<Arc<VideoFrame>>,
    position: Position,
    /// The run frames decoded now belong to, the last run number given
    /// out, and the next frame's place in the run (see
    /// [`VideoFrame::run`]). A run starts at each seek of the demuxer, so
    /// a seek that decodes forward goes on with the one before.
    run: u64,
    runs: u64,
    seq: u64,
    /// Frames decoded since decoding last started at the start of the
    /// stream, when it did; counts positions in a stream whose timestamps
    /// go backwards.
    from_start: Option<u64>,
    /// The pts of the last frame decoded, to see timestamps go backwards.
    last_decoded: Option<i64>,
    not_seekable: bool,
    /// The stream has no timestamps: pts are counted, as frame index times
    /// `frame_ticks`.
    no_timestamps: bool,
    /// The (average) frame duration in ticks, for the last frame's
    /// interval when the decoder gives it none.
    frame_ticks: i64,
    catch_up: Option<i64>,
    events: Vec<DecoderEvent>,
    stats: DecodeStats,
    decode_ms_total: f64,
}

impl std::fmt::Debug for FfmpegVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfmpegVideo")
            .field("dec", &self.dec)
            .field("origin_pts", &self.info.origin_pts)
            .field("layout", &self.info.layout.name)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

/// What [`FfmpegVideo::decode`] got.
enum Got {
    Frame(Raw),
    End,
    Stopped,
}

/// How a seek starts decoding.
enum Start {
    /// From where the decoder is.
    Forward,
    /// After seeking, with the first frame out when the landing loop found
    /// one at or before the target.
    Seeked(Option<Raw>),
}

impl FfmpegVideo {
    /// Open the stream and decode its first presented frame, which fixes
    /// the origin of clip time.
    pub fn open(path: &Path, req: &VideoRequest) -> Result<Self, MediaError> {
        let mut dec =
            VideoDecoder::open_software(path, req.stream, req.threads, req.interrupt.clone())?;
        let index = dec.stream_index();
        let codec = dec.codec_name();
        let tb = dec.time_base();
        let never = StopToken::never();
        let first = loop {
            match dec.receive(Some(&never))? {
                Received::Frame(frame) => {
                    let fields = ffi::frame_fields(&frame);
                    if fields.corrupt {
                        continue;
                    }
                    break Some((frame, fields));
                }
                Received::End | Received::Stopped => break None,
            }
        };
        let Some((frame, fields)) = first else {
            return Err(MediaError::Decode {
                path: path.to_path_buf(),
                reason: "the video stream has no frames".into(),
            });
        };
        let no_timestamps = fields.pts.is_none();
        let origin_pts = fields.pts.unwrap_or(0);
        let conv = Conversion::new(path, fields.format, fields.width, fields.height)?;
        let source_name = ffi::pix_fmt_name(fields.format).unwrap_or_default();
        let mut events = Vec::new();
        if let Conversion::Scaled { layout, .. } = &conv {
            tracing::info!(
                path = %path.display(),
                "converting {source_name} frames to {} for upload",
                layout.name
            );
            events.push(DecoderEvent::Converted {
                from: source_name.clone(),
                to: layout.name.clone(),
            });
        }
        let color = color::resolve(
            fields.color,
            &ColorContext {
                family: conv.layout().family,
                pix_fmt: &source_name,
                codec: &codec,
                width: fields.width,
                height: fields.height,
            },
        );
        let facts = StreamFacts::read(path, &dec, origin_pts);
        let frame_ticks = facts.frame_ticks(tb);
        let par = dec
            .input()
            .stream(index)
            .map(|s| ffi::codecpar_fields(&s.parameters()));
        let info = VideoStreamInfo {
            index,
            codec: codec.clone(),
            size: (fields.width, fields.height),
            time_base: tb,
            origin_pts,
            duration: facts.duration,
            timing: facts.timing,
            frame_count: facts.frame_count,
            layout: conv.layout().clone(),
            source_pix_fmt: source_name.clone(),
            color,
            mapping: facts.mapping,
            rotation_deg: par.and_then(|p| p.rotation_deg),
        };
        tracing::debug!(
            path = %path.display(),
            origin_pts,
            timing = ?info.timing,
            frames = ?info.frame_count,
            layout = %info.layout.name,
            "opened video stream"
        );
        let mut v = FfmpegVideo {
            dec,
            info,
            decode_path: DecodePath::Software {
                threads: req.threads,
            },
            conv,
            source_name,
            pending: None,
            current: None,
            position: Position::InStep,
            run: 0,
            runs: 0,
            seq: 1,
            from_start: Some(1),
            last_decoded: Some(origin_pts),
            not_seekable: false,
            no_timestamps,
            frame_ticks,
            catch_up: None,
            events,
            stats: DecodeStats::default(),
            decode_ms_total: 0.0,
        };
        let after_failure = v.dec.reading_failed();
        v.pending = Some(Raw {
            frame,
            fields,
            pts: origin_pts,
            seq: 0,
            after_failure,
        });
        Ok(v)
    }

    fn path(&self) -> &Path {
        self.dec.path()
    }

    /// The next frame from the decoder, with its pts. Frames FFmpeg flags
    /// corrupt are dropped: decoded from references a seek skipped, they
    /// are not part of the stream as a decode from the start shows it.
    /// Notes timestamps that go backwards.
    fn decode(&mut self, stop: &StopToken) -> Result<Got, MediaError> {
        loop {
            let started = Instant::now();
            let got = self.dec.receive(Some(stop));
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let frame = match got? {
                Received::Frame(frame) => frame,
                Received::End => return Ok(Got::End),
                Received::Stopped => return Ok(Got::Stopped),
            };
            self.decode_ms_total += ms;
            self.stats.last_decode_ms = ms;
            let fields = ffi::frame_fields(&frame);
            if fields.corrupt {
                continue;
            }
            return Ok(self.decoded(frame, fields));
        }
    }

    fn decoded(&mut self, frame: ff::frame::Video, fields: FrameFields) -> Got {
        let index = self.from_start.unwrap_or(0);
        self.from_start = self.from_start.map(|n| n + 1);
        let pts = match fields.pts {
            Some(p) if !self.no_timestamps => p,
            // Counted from the start, which is where every seek in such a
            // stream begins.
            _ => {
                self.no_timestamps = true;
                i64::try_from(index)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(self.frame_ticks)
            }
        };
        if let Some(last) = self.last_decoded
            && pts < last
            && !self.not_seekable
        {
            self.not_seekable = true;
            tracing::warn!(
                path = %self.path().display(),
                "timestamps go backwards after pts {last}: exact seeks decode from \
                 the start of the stream from now on"
            );
            self.events
                .push(DecoderEvent::NotExactlySeekable { at_pts: last });
        }
        self.last_decoded = Some(pts);
        let seq = self.seq;
        self.seq += 1;
        Got::Frame(Raw {
            frame,
            fields,
            pts,
            seq,
            after_failure: self.dec.reading_failed(),
        })
    }

    /// Forget what was decoded before a seek, and start a run.
    fn after_seek(&mut self, to_start: bool) {
        self.pending = None;
        self.last_decoded = None;
        self.from_start = to_start.then_some(0);
        self.runs += 1;
        self.run = self.runs;
        self.seq = 0;
    }

    fn seek_to_start(&mut self, cost: &mut SeekCost) -> Result<(), MediaError> {
        self.dec.seek_to_start()?;
        cost.seeks += 1;
        self.after_seek(true);
        Ok(())
    }

    /// Whether decoding forward from where the decoder is reaches `t`
    /// with no more work than a seek: `t` is ahead, and the keyframe a
    /// seek to it would land on has already been read, so `t` lies in the
    /// GOP being decoded. Without an index, `t` at most a second ahead.
    fn forward_is_cheaper(&self, t: i64) -> bool {
        if self.position != Position::InStep {
            return false;
        }
        let ahead = match (&self.current, &self.pending) {
            (Some(c), _) => t >= c.pts,
            (None, Some(p)) => t >= p.pts,
            (None, None) => false,
        };
        if !ahead {
            return false;
        }
        let input = self.dec.input();
        let stream = self.dec.stream_index();
        if ffi::index_len(input, stream) == 0 {
            let here = self
                .current
                .as_ref()
                .map(|c| c.pts)
                .or(self.pending.as_ref().map(|p| p.pts))
                .unwrap_or(t);
            let second = Time::from_secs(1)
                .to_pts(self.info.time_base, Rounding::Floor)
                .unwrap_or(i64::MAX);
            return t.saturating_sub(here) <= second;
        }
        match ffi::index_keyframe_before(input, stream, t) {
            Some(k) => self.dec.read_position().is_some_and(|read| k <= read),
            // Before every keyframe: no seek gets closer.
            None => true,
        }
    }

    /// Seek so that the first frame out is at or before `t`, or is the
    /// first frame of the stream, as the module docs describe: that frame
    /// in [`Start::Seeked`], or nothing there after falling back to the
    /// start. `None` on a stop.
    fn land(
        &mut self,
        t: i64,
        stop: &StopToken,
        cost: &mut SeekCost,
    ) -> Result<Option<Start>, MediaError> {
        let mut ts = t;
        let mut prev: Option<i64> = None;
        for attempt in 0..MAX_ATTEMPTS {
            if stop.is_stopped() {
                return Ok(None);
            }
            self.dec.seek(ts)?;
            cost.seeks += 1;
            if attempt > 0 {
                cost.reseeks += 1;
            }
            self.after_seek(false);
            // The first frame out. A read that fails here (a seek past the
            // cut of a truncated file) is cleared by the next seek.
            let first = loop {
                match self.decode(stop) {
                    Ok(Got::Frame(r)) => {
                        cost.frames_decoded += 1;
                        break Some(r);
                    }
                    Ok(Got::End) => break None,
                    Ok(Got::Stopped) => return Ok(None),
                    Err(_) if self.dec.has_failed() => break None,
                    Err(e) => self.events.push(DecoderEvent::Damaged {
                        reason: e.to_string(),
                        last_good_pts: None,
                    }),
                }
            };
            let landed = self.dec.landing();
            if let Some(r) = first
                && (r.pts <= t || r.pts == self.info.origin_pts)
            {
                return Ok(Some(Start::Seeked(Some(r))));
            }
            match (landed, prev) {
                // No timestamp to seek before, or no progress.
                (None, _) => break,
                (Some(d), Some(p)) if d >= p => break,
                (Some(d), _) => {
                    prev = Some(d);
                    ts = d.saturating_sub(1);
                }
            }
        }
        tracing::debug!(
            path = %self.path().display(),
            target = t,
            "no seek landed at or before the target: decoding from the start"
        );
        self.seek_to_start(cost)?;
        Ok(Some(Start::Seeked(None)))
    }

    /// The pts at which `frame`'s interval ends.
    fn end_of(&self, pts: i64, duration: i64) -> i64 {
        let d = if duration > 0 {
            duration
        } else {
            self.frame_ticks
        };
        pts.saturating_add(d)
    }

    /// Make the frame handed out for `raw`, followed by `next_pts` when
    /// that is known.
    fn make_frame(
        &mut self,
        raw: Raw,
        next_pts: Option<i64>,
    ) -> Result<Arc<VideoFrame>, MediaError> {
        let f = raw.fields;
        if self.conv.source() != (f.format, f.width, f.height) {
            let conv = Conversion::new(self.dec.path(), f.format, f.width, f.height)?;
            self.source_name = ffi::pix_fmt_name(f.format).unwrap_or_default();
            if let Conversion::Scaled { layout, .. } = &conv
                && !matches!(&self.conv, Conversion::Scaled { scaler, .. } if scaler.source().0 == f.format)
            {
                self.events.push(DecoderEvent::Converted {
                    from: self.source_name.clone(),
                    to: layout.name.clone(),
                });
            }
            self.events.push(DecoderEvent::LayoutChanged {
                layout: conv.layout().clone(),
                size: (f.width, f.height),
            });
            self.conv = conv;
        }
        let color = color::resolve(
            f.color,
            &ColorContext {
                family: self.conv.layout().family,
                pix_fmt: &self.source_name,
                codec: &self.info.codec,
                width: f.width,
                height: f.height,
            },
        );
        let tb = self.info.time_base;
        let time = |pts: i64| {
            Time::from_pts(pts.saturating_sub(self.info.origin_pts), tb).map_err(|e| {
                MediaError::Decode {
                    path: self.dec.path().to_path_buf(),
                    reason: format!("timestamp {pts}: {e}"),
                }
            })
        };
        let at = time(raw.pts)?;
        let layout = self.conv.layout().clone();
        let frame = match &mut self.conv {
            Conversion::Native { .. } => raw.frame,
            Conversion::Scaled { scaler, .. } => {
                let started = Instant::now();
                let out = scaler.scale(&raw.frame).map_err(|e| MediaError::Decode {
                    path: self.dec.path().to_path_buf(),
                    reason: format!("converting {} frame: {e}", self.source_name),
                })?;
                self.stats.last_convert_ms = started.elapsed().as_secs_f64() * 1000.0;
                out
            }
        };
        let mut out = VideoFrame::from_ffmpeg(frame, layout, color, raw.pts, at);
        let duration = next_pts
            .map(|n| n.saturating_sub(raw.pts))
            .filter(|&d| d > 0)
            .or((f.duration > 0).then_some(f.duration));
        out.duration = duration.and_then(|d| Time::from_pts(d, tb).ok());
        out.decoded_on = self.decode_path.clone();
        out.run = self.run;
        out.seq = raw.seq;
        self.stats.frames_returned += 1;
        let frame = Arc::new(out);
        self.current = Some(frame.clone());
        Ok(frame)
    }

    /// Decode one frame ahead into `pending`, if it isn't there, so the
    /// frame about to be handed out knows its duration. A stop, the end
    /// or an error leaves `pending` empty: an error that ends the stream
    /// comes back from the next call, and one that doesn't becomes an
    /// event.
    fn fill_pending(&mut self, stop: &StopToken) {
        if self.pending.is_some() {
            return;
        }
        match self.decode(stop) {
            Ok(Got::Frame(r)) => self.pending = Some(r),
            Ok(Got::End | Got::Stopped) => {}
            Err(_) if self.dec.has_failed() => {}
            Err(e) => self.events.push(DecoderEvent::Damaged {
                reason: e.to_string(),
                last_good_pts: self.current.as_ref().map(|c| c.pts),
            }),
        }
    }

    fn exact(
        &mut self,
        t: i64,
        stop: &StopToken,
    ) -> Result<Option<(Arc<VideoFrame>, Exactness, SeekCost)>, MediaError> {
        let mut cost = SeekCost::default();
        // Already showing it, unless the next frame came out after a read
        // failure, when frames between the two may be lost.
        if self.position == Position::InStep
            && let (Some(c), Some(p)) = (&self.current, &self.pending)
            && c.pts <= t
            && t < p.pts
            && !p.after_failure
        {
            cost.forward_only = true;
            return Ok(Some((c.clone(), Exactness::Exact, cost)));
        }
        if self.not_seekable {
            return self.counted(t, stop, cost);
        }
        let start = if self.forward_is_cheaper(t) {
            cost.forward_only = true;
            Start::Forward
        } else if self.no_timestamps {
            self.seek_to_start(&mut cost)?;
            Start::Seeked(None)
        } else {
            match self.land(t, stop, &mut cost)? {
                Some(s) => s,
                None => {
                    self.position = Position::Lost;
                    return Ok(None);
                }
            }
        };
        let previous = match start {
            Start::Forward => self.current.clone().filter(|c| c.pts <= t),
            Start::Seeked(_) => None,
        };
        let mut best = match start {
            Start::Forward => {
                let p = self.pending.take();
                match p {
                    Some(p) if p.pts > t => {
                        self.pending = Some(p);
                        None
                    }
                    p => p,
                }
            }
            Start::Seeked(first) => first,
        };
        let mut at_end = false;
        let mut damage: Option<String> = None;
        if self.pending.is_none() {
            loop {
                match self.decode(stop) {
                    Ok(Got::Frame(r)) => {
                        cost.frames_decoded += 1;
                        if r.pts <= t {
                            best = Some(r);
                        } else {
                            self.pending = Some(r);
                            break;
                        }
                    }
                    Ok(Got::End) => {
                        at_end = true;
                        break;
                    }
                    Ok(Got::Stopped) => {
                        self.position = Position::Lost;
                        return Ok(None);
                    }
                    Err(e) if self.dec.has_failed() => {
                        damage = Some(e.to_string());
                        break;
                    }
                    Err(e) => self.events.push(DecoderEvent::Damaged {
                        reason: e.to_string(),
                        last_good_pts: best.as_ref().map(|b| b.pts),
                    }),
                }
            }
        }
        self.position = Position::InStep;
        // The first frame after the target came out as the decoder drained
        // after a read failure: the frame shown at the target may be one
        // the damage lost, which a later frame coming out doesn't rule out.
        if damage.is_none() && self.pending.as_ref().is_some_and(|p| p.after_failure) {
            damage = Some(
                self.dec
                    .read_failure()
                    .map_or_else(|| "reading failed".into(), |e| e.to_string()),
            );
        }
        let next_pts = self.pending.as_ref().map(|p| p.pts);
        let frame = match (best, previous) {
            (Some(b), _) => self.make_frame(b, next_pts)?,
            (None, Some(c)) => c,
            (None, None) => match self.pending.take() {
                // Nothing at or before the target: the first frame there is.
                Some(p) => {
                    self.fill_pending(stop);
                    let next = self.pending.as_ref().map(|q| q.pts);
                    self.make_frame(p, next)?
                }
                None => {
                    return Err(MediaError::Decode {
                        path: self.path().to_path_buf(),
                        reason: damage.unwrap_or_else(|| "no frame decoded".into()),
                    });
                }
            },
        };
        let exactness = if let Some(reason) = damage {
            self.events.push(DecoderEvent::Damaged {
                reason: reason.clone(),
                last_good_pts: Some(frame.pts),
            });
            if frame.pts == t {
                // The damage is after the target, which is shown as it is.
                Exactness::Exact
            } else {
                tracing::warn!(
                    path = %self.path().display(),
                    "the file is damaged before the time asked for: showing the last \
                     frame before the damage, at pts {} ({reason})",
                    frame.pts
                );
                Exactness::Truncated {
                    last_good_pts: frame.pts,
                }
            }
        } else if at_end
            && self.pending.is_none()
            && t >= self.end_of(frame.pts, self.frame_duration_ticks(&frame))
        {
            Exactness::AfterLast
        } else {
            Exactness::Exact
        };
        Ok(Some((frame, exactness, cost)))
    }

    /// A frame's duration in ticks, 0 when unknown.
    fn frame_duration_ticks(&self, frame: &VideoFrame) -> i64 {
        frame
            .duration
            .and_then(|d| d.to_pts(self.info.time_base, Rounding::Nearest).ok())
            .unwrap_or(0)
    }

    /// An exact seek in a stream whose timestamps go backwards: decode the
    /// whole stream from the start for the frame with the greatest pts at
    /// or before `t`, later frames winning ties, then decode up to it
    /// again. Correct, and slow.
    fn counted(
        &mut self,
        t: i64,
        stop: &StopToken,
        mut cost: SeekCost,
    ) -> Result<Option<(Arc<VideoFrame>, Exactness, SeekCost)>, MediaError> {
        self.seek_to_start(&mut cost)?;
        let mut best: Option<(u64, i64)> = None;
        let mut first: Option<i64> = None;
        let mut index = 0u64;
        loop {
            match self.decode(stop)? {
                Got::Frame(r) => {
                    cost.frames_decoded += 1;
                    first.get_or_insert(r.pts);
                    if r.pts <= t && best.is_none_or(|(_, p)| r.pts >= p) {
                        best = Some((index, r.pts));
                    }
                    index += 1;
                }
                Got::End => break,
                Got::Stopped => {
                    self.position = Position::Lost;
                    return Ok(None);
                }
            }
        }
        let target = best.map_or(0, |(i, _)| i);
        self.seek_to_start(&mut cost)?;
        let Some(frame) = self.skip_to(target, stop, &mut cost)? else {
            self.position = Position::Lost;
            return Ok(None);
        };
        let exactness = if best.is_none() && first.is_some_and(|f| t < f) {
            Exactness::BeforeFirst
        } else {
            Exactness::Exact
        };
        Ok(Some((frame, exactness, cost)))
    }

    /// From the start, decode up to frame `index` in output order and hand
    /// it out. `None` on a stop.
    fn skip_to(
        &mut self,
        index: u64,
        stop: &StopToken,
        cost: &mut SeekCost,
    ) -> Result<Option<Arc<VideoFrame>>, MediaError> {
        let mut n = 0u64;
        loop {
            match self.decode(stop)? {
                Got::Frame(r) if n == index => {
                    self.position = Position::InStep;
                    // Durations aren't known in such a stream.
                    return self.make_frame(r, None).map(Some);
                }
                Got::Frame(_) => {
                    cost.frames_decoded += 1;
                    n += 1;
                }
                Got::End => {
                    return Err(MediaError::Decode {
                        path: self.path().to_path_buf(),
                        reason: format!("the stream ended before frame {index}"),
                    });
                }
                Got::Stopped => return Ok(None),
            }
        }
    }

    /// One BACKWARD seek and its first frame, whatever its pts: the
    /// keyframe a seek to `t` lands on, or a frame after it when `t` is
    /// one of that keyframe's leading pictures. From the start when the
    /// seek finds no frame. The decoder stays where it is, so an exact
    /// seek to `t` after it decodes forward when the frame is at or before
    /// `t`.
    fn keyframe(
        &mut self,
        t: i64,
        stop: &StopToken,
    ) -> Result<Option<(Arc<VideoFrame>, Exactness, SeekCost)>, MediaError> {
        if self.not_seekable || self.no_timestamps {
            return self.exact(t, stop);
        }
        let mut cost = SeekCost::default();
        self.dec.seek(t)?;
        cost.seeks += 1;
        self.after_seek(false);
        let mut from_start = false;
        let first = loop {
            match self.decode(stop) {
                Ok(Got::Frame(r)) => {
                    cost.frames_decoded += 1;
                    break r;
                }
                Ok(Got::Stopped) => {
                    self.position = Position::Lost;
                    return Ok(None);
                }
                Err(e) if !self.dec.has_failed() => self.events.push(DecoderEvent::Damaged {
                    reason: e.to_string(),
                    last_good_pts: None,
                }),
                Ok(Got::End) | Err(_) if !from_start => {
                    from_start = true;
                    self.seek_to_start(&mut cost)?;
                }
                Ok(Got::End) => {
                    return Err(MediaError::Decode {
                        path: self.path().to_path_buf(),
                        reason: "no frame decoded".into(),
                    });
                }
                Err(e) => return Err(e),
            }
        };
        self.position = Position::InStep;
        self.fill_pending(stop);
        let next = self.pending.as_ref().map(|p| p.pts);
        let frame = self.make_frame(first, next)?;
        Ok(Some((frame, Exactness::KeyframeOnly, cost)))
    }

    /// Put the decoder back after a stopped seek: just after the last
    /// frame handed out, or at the start.
    fn resume(&mut self, stop: &StopToken) -> Result<bool, MediaError> {
        match self.position {
            Position::InStep => Ok(true),
            Position::Lost => {
                let Some(current) = self.current.clone() else {
                    let mut cost = SeekCost::default();
                    self.seek_to_start(&mut cost)?;
                    self.position = Position::InStep;
                    return Ok(true);
                };
                // Seek to the frame handed out last; what follows it is
                // then `pending`, and goes on with its run.
                let returned = self.stats.frames_returned;
                let got = self.exact(current.pts, stop)?;
                self.stats.frames_returned = returned;
                self.run = current.run;
                self.seq = current.seq + 1;
                if let Some(p) = &mut self.pending {
                    p.seq = self.seq;
                    self.seq += 1;
                }
                self.current = Some(current);
                Ok(got.is_some())
            }
        }
    }
}

impl FfmpegVideo {
    /// The seek tests' negative control, and nothing else: one BACKWARD
    /// seek to clip time `at` and a decode forward, without seeking again
    /// when the landing keyframe's pts is past the target, as a naive
    /// player would. Returns the pts of the frame it would show. It drops
    /// open-GOP leading pictures, which is why `seek` doesn't work so.
    #[doc(hidden)]
    pub fn naive_seek_for_tests(&mut self, at: Time) -> Result<Option<i64>, MediaError> {
        let ticks = at
            .to_pts(self.info.time_base, Rounding::Floor)
            .map_err(|e| MediaError::Decode {
                path: self.path().to_path_buf(),
                reason: e.to_string(),
            })?;
        let t = self.info.origin_pts.saturating_add(ticks);
        self.dec.seek(t)?;
        self.after_seek(false);
        self.position = Position::Lost;
        let never = StopToken::never();
        let mut best = None;
        while let Got::Frame(r) = self.decode(&never)? {
            if r.pts > t {
                break;
            }
            best = Some(r.pts);
        }
        self.pending = None;
        Ok(best)
    }
}

impl VideoDecode for FfmpegVideo {
    fn info(&self) -> &VideoStreamInfo {
        &self.info
    }

    fn decode_path(&self) -> &DecodePath {
        &self.decode_path
    }

    fn seek(
        &mut self,
        at: Time,
        mode: SeekMode,
        stop: &StopToken,
    ) -> Result<SeekOutcome, MediaError> {
        let tb = self.info.time_base;
        let ticks = at
            .to_pts(tb, Rounding::Floor)
            .map_err(|e| MediaError::Decode {
                path: self.path().to_path_buf(),
                reason: format!("seeking to {at:?}: {e}"),
            })?;
        let before_first = ticks < 0;
        let t = self.info.origin_pts.saturating_add(ticks.max(0));
        // A seek ends catching up: its decode skips nothing.
        if self.catch_up.take().is_some() {
            self.dec.skip_nonref_before(None);
        }
        let got = match mode {
            SeekMode::Exact => self.exact(t, stop)?,
            SeekMode::Keyframe => self.keyframe(t, stop)?,
        };
        Ok(match got {
            None => SeekOutcome::Stopped,
            Some((frame, exactness, cost)) => SeekOutcome::Frame {
                frame,
                exactness: if before_first && !matches!(exactness, Exactness::Truncated { .. }) {
                    Exactness::BeforeFirst
                } else {
                    exactness
                },
                cost,
            },
        })
    }

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        if !self.resume(stop)? {
            return Ok(NextFrame::Stopped);
        }
        loop {
            let raw = match self.pending.take() {
                Some(r) => r,
                None => match self.decode(stop)? {
                    Got::Frame(r) => r,
                    Got::End => return Ok(NextFrame::End),
                    Got::Stopped => return Ok(NextFrame::Stopped),
                },
            };
            if let Some(until) = self.catch_up
                && raw.pts < until
            {
                continue;
            }
            if self.catch_up.take().is_some() {
                self.dec.skip_nonref_before(None);
            }
            self.fill_pending(stop);
            let next = self.pending.as_ref().map(|p| p.pts);
            return self.make_frame(raw, next).map(NextFrame::Frame);
        }
    }

    fn catch_up(&mut self, until: Option<Time>) {
        let t = until.and_then(|t| t.to_pts(self.info.time_base, Rounding::Ceil).ok());
        self.catch_up = t.map(|t| self.info.origin_pts.saturating_add(t));
        self.dec.skip_nonref_before(self.catch_up);
    }

    fn take_events(&mut self) -> Vec<DecoderEvent> {
        std::mem::take(&mut self.events)
    }

    fn stats(&self) -> DecodeStats {
        let frames = self.dec.frames_decoded();
        DecodeStats {
            frames_decoded: frames,
            mean_decode_ms: if frames > 0 {
                self.decode_ms_total / frames as f64
            } else {
                0.0
            },
            ..self.stats
        }
    }
}

/// What a stream says about its timing and mapping, besides the frames.
struct StreamFacts {
    duration: Option<Time>,
    timing: Timing,
    frame_count: Option<u64>,
    mapping: SourceMapping,
}

impl StreamFacts {
    fn read(path: &Path, dec: &VideoDecoder, origin_pts: i64) -> StreamFacts {
        let input = dec.input();
        let index = dec.stream_index();
        let tb = dec.time_base();
        let mov = input.format().name().starts_with("mov");
        let st = input.stream(index);
        let rate = |r: ff::Rational| {
            Rational::new(i64::from(r.numerator()), i64::from(r.denominator()))
                .ok()
                .and_then(|r| FrameRate::new(r).ok())
        };
        let average = st.as_ref().and_then(|s| rate(s.avg_frame_rate()));
        let real = st.as_ref().and_then(|s| rate(s.rate()));
        let duration = st.as_ref().and_then(|s| {
            let (start, d) = (s.start_time(), s.duration());
            if d == ff::ffi::AV_NOPTS_VALUE || d <= 0 {
                return None;
            }
            let start = if start == ff::ffi::AV_NOPTS_VALUE {
                origin_pts
            } else {
                start
            };
            Time::from_pts(start.saturating_add(d).saturating_sub(origin_pts), tb).ok()
        });
        let trak_facts = if mov {
            let id = st.as_ref().map(|s| s.id());
            mp4::read_file_with(path, FirstBox::Any)
                .ok()
                .and_then(|tree| {
                    let trak = tree
                        .tracks()
                        .find(|t| mp4::track_id(t).and_then(|i| i32::try_from(i).ok()) == id)?;
                    Some((mp4::sample_timing(trak), spherical::read_track(trak)))
                })
        } else {
            None
        };
        let constant = match &trak_facts {
            Some((Some(timing), _)) => timing.constant.and_then(|d| {
                // The stream's time base is 1 / the track's timescale.
                let tick = Rational::new(1, i64::from(timing.timescale)).ok()?;
                (tick == tb)
                    .then(|| Rational::new(i64::from(timing.timescale), i64::from(d)).ok())
                    .flatten()
                    .and_then(|r| FrameRate::new(r).ok())
            }),
            _ if !mov => match (average, real) {
                (Some(a), Some(r)) if a == r => snap(a),
                _ => None,
            },
            _ => None,
        };
        let timing = match constant {
            Some(r) => Timing::Constant(r),
            None => Timing::Variable { average },
        };
        let frame_count = mov
            .then(|| ffi::index_shown(input, index))
            .filter(|&n| n > 0);
        let mapping = trak_facts.map(|(_, s)| mapping(&s)).unwrap_or_default();
        StreamFacts {
            duration,
            timing,
            frame_count,
            mapping,
        }
    }

    /// The frame duration in ticks of `tb`: exact for a constant rate, the
    /// average otherwise, and one tick when nothing says.
    fn frame_ticks(&self, tb: Rational) -> i64 {
        let rate = match self.timing {
            Timing::Constant(r) => Some(r),
            Timing::Variable { average } => average,
        };
        rate.and_then(|r| r.frame_duration().to_pts(tb, Rounding::Nearest).ok())
            .filter(|&t| t > 0)
            .unwrap_or(1)
    }
}

/// The standard rate within a millionth of `r`, if there is one.
fn snap(r: FrameRate) -> Option<FrameRate> {
    STANDARD_RATES
        .into_iter()
        .find(|s| ((s.to_f64() - r.to_f64()) / s.to_f64()).abs() < 1e-6)
}

/// How the frame maps onto the sphere, from a track's spherical metadata.
fn mapping(s: &spherical::TrackSpherical) -> SourceMapping {
    let mut m = SourceMapping::default();
    if let Some(v2) = &s.v2 {
        m.projection = v2.projection.clone();
        if let Some(Projection::Equirect {
            top,
            bottom,
            left,
            right,
        }) = &v2.projection
        {
            let f = |v: u32| f64::from(v) / 4_294_967_296.0;
            m.cover = [f(*left), f(*top), 1.0 - f(*right), 1.0 - f(*bottom)];
        }
        m.stereo = v2.stereo.unwrap_or(StereoMode::Mono);
        m.pose_present = v2.pose_yaw != 0.0 || v2.pose_pitch != 0.0 || v2.pose_roll != 0.0;
    } else if let Some(v1) = &s.v1 {
        if v1.projection_type.as_deref() == Some("equirectangular") {
            m.projection = Some(Projection::Equirect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 0,
            });
        }
        m.stereo = match v1.stereo_mode.as_deref() {
            Some("top-bottom") => StereoMode::TopBottom,
            Some("left-right") => StereoMode::LeftRight,
            _ => StereoMode::Mono,
        };
    }
    m
}

/// Where `--frame N` and the GUI's frame counter address frame `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAddress {
    /// A constant rate: seek to this time, (n + ½) frame durations, which
    /// lies inside frame n's interval even when the container rounds
    /// timestamps (Matroska's milliseconds).
    At(Time),
    /// A variable rate: count frames from the start (exact, and slow).
    Counted(u64),
}

/// How to reach frame `n` of a stream.
pub fn frame_address(
    info: &VideoStreamInfo,
    n: u64,
) -> Result<FrameAddress, spherecut_core::TimeError> {
    match info.timing {
        Timing::Constant(rate) => {
            let half = Rational::new(1, 2)?;
            let index = i64::try_from(n).map_err(|_| spherecut_core::TimeError::Overflow)?;
            let at = Rational::from_integer(index)
                .checked_add(half)?
                .checked_mul(rate.frame_duration().as_rational())?;
            Ok(FrameAddress::At(Time::seconds(at)))
        }
        Timing::Variable { .. } => Ok(FrameAddress::Counted(n)),
    }
}

/// Frame `n` of `dec`'s stream, by [`frame_address`]: a seek for a
/// constant rate, or the first frame and `n` more for a variable one.
/// `None` on a stop, and `Err` past the end of a variable-rate stream.
pub fn seek_frame(
    dec: &mut dyn VideoDecode,
    n: u64,
    stop: &StopToken,
) -> Result<Option<(Arc<VideoFrame>, Exactness)>, MediaError> {
    let address = frame_address(dec.info(), n).map_err(|e| MediaError::Decode {
        path: Default::default(),
        reason: format!("frame {n}: {e}"),
    })?;
    match address {
        FrameAddress::At(t) => match dec.seek(t, SeekMode::Exact, stop)? {
            SeekOutcome::Frame {
                frame, exactness, ..
            } => Ok(Some((frame, exactness))),
            SeekOutcome::Stopped => Ok(None),
        },
        FrameAddress::Counted(n) => {
            let mut frame = match dec.seek(Time::ZERO, SeekMode::Exact, stop)? {
                SeekOutcome::Frame { frame, .. } => frame,
                SeekOutcome::Stopped => return Ok(None),
            };
            for i in 0..n {
                frame = match dec.next_frame(stop)? {
                    NextFrame::Frame(f) => f,
                    NextFrame::Stopped => return Ok(None),
                    NextFrame::End => {
                        return Err(MediaError::Decode {
                            path: Default::default(),
                            reason: format!("the stream has {} frames, not {}", i + 1, n + 1),
                        });
                    }
                };
            }
            Ok(Some((frame, Exactness::Exact)))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::ff::ffi::AVPixelFormat as P;

    #[test]
    fn formats_are_uploaded_converted_or_unsupported() {
        let path = Path::new("clip.mov");
        let conv = |fmt: i32| Conversion::new(path, fmt, 64, 32);
        assert!(matches!(
            conv(P::AV_PIX_FMT_NV12 as i32),
            Ok(Conversion::Native { .. })
        ));
        match conv(P::AV_PIX_FMT_UYVY422 as i32) {
            Ok(Conversion::Scaled { scaler, layout }) => {
                assert_eq!(layout.name, "yuv422p");
                assert_eq!(scaler.source(), (P::AV_PIX_FMT_UYVY422 as i32, 64, 32));
            }
            _ => panic!("uyvy422 is not converted"),
        }
        // swscale takes no hardware frames, and FFmpeg knows no format
        // 100000.
        for (fmt, name) in [
            (P::AV_PIX_FMT_VAAPI as i32, "vaapi"),
            (100_000, "format 100000"),
        ] {
            match conv(fmt) {
                Err(e @ MediaError::UnsupportedFormat { .. }) => {
                    assert_eq!(e.code(), "unsupported_pixel_format");
                    assert!(e.to_string().contains(name), "{e}");
                }
                Err(e) => panic!("{name}: {e}"),
                Ok(_) => panic!("{name} is converted"),
            }
        }
    }

    #[test]
    fn matroska_rates_snap_to_the_standard_ones() {
        let r = |n, d| FrameRate::new(Rational::new(n, d).unwrap()).unwrap();
        // FFmpeg's reading of a 59.94 fps track's default duration.
        assert_eq!(snap(r(19001, 317)), Some(FrameRate::FPS_59_94));
        assert_eq!(snap(r(24000, 1001)), Some(FrameRate::FPS_23_976));
        assert_eq!(snap(r(30, 1)), Some(FrameRate::FPS_30));
        assert_eq!(snap(r(125, 2)), None);
        assert_eq!(snap(r(5994, 100)), None);
    }
}
