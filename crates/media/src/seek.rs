//! Frame-exact seeking on FFmpeg's decoders (M1 plan section 3.3,
//! DECISIONS D29): [`FfmpegVideo`], the [`VideoDecode`] that
//! [`FfmpegBackend`](crate::backend::FfmpegBackend) opens, on top of
//! [`VideoDecoder`], in software or on a hardware device.
//!
//! `seek(at, Exact)` returns the frame with the greatest pts at or before
//! T = origin + ⌊at / tb⌋, later frames winning ties. A BACKWARD seek can
//! land on a keyframe whose dts is at or before T but whose pts is after
//! it: its leading pictures (HEVC RASL pictures after a CRA, open-GOP
//! B-frames) are then dropped, by FFmpeg or here, and so would be the
//! target. So when the first frame out is after T, it seeks again to just
//! before the landing keyframe, up to [`MAX_ATTEMPTS`] times, and then
//! decodes from the start. A target in the GOP being decoded is reached
//! by decoding forward, with no seek at all.
//!
//! Seeks stop through a [`StopToken`] at packet and frame boundaries,
//! which leaves the demuxer in a known state; the decoder then seeks
//! again before it goes on. FFmpeg's interrupt flag is for shutdown only
//! (D13).
//!
//! On a device, frames stay on it until they are handed out: a seek holds
//! its best candidate and the frame after it as device frames, and only
//! the one it returns is downloaded, into the decoder's pool (D30). That
//! is two surfaces held at most, which `extra_hw_frames` keeps spare. The
//! device refusing the stream or failing on it is returned at once and
//! never taken for damage, for the fallback (`fallback.rs`) to act on.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use spherecut_core::video::PixelLayout;
use spherecut_core::{FrameRate, Rational, Rounding, StopToken, Time};

use crate::backend::{
    DamageCause, DecodePath, DecodeStats, DecoderEvent, Exactness, FallbackReason, HwKind,
    NextFrame, SeekCost, SeekMode, SeekOutcome, SourceMapping, Timing, VideoDecode, VideoFrame,
    VideoRequest, VideoStreamInfo,
};
#[cfg(feature = "fault-injection")]
use crate::backend::{Fault, FaultPlan};
use crate::color::{self, ColorContext};
use crate::decode::{Received, VideoDecoder};
use crate::ffi::{self, FrameFields, Scaler};
use crate::mp4::{self, FirstBox};
use crate::spherical::{self, Projection, StereoMode};
use crate::{HwDevice, MediaError, ff};

/// Seeks to before the landing keyframe, before decoding from the start.
pub const MAX_ATTEMPTS: u32 = 8;

/// Packets of a Matroska stream whose timestamps are read at open, to
/// see whether its default duration is its frame rate.
const SCAN_PACKETS: usize = 64;

/// Of those, the last ones in pts order that aren't checked, since frames
/// shown before them may come later in decode order: the most frames
/// H.264 and HEVC reorder.
const REORDER_WINDOW: usize = 16;

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
    /// In system memory, or still on the device.
    frame: ff::frame::Video,
    /// For a device frame, as it will be once downloaded (as far as can
    /// be told before: `ffi::as_downloaded`).
    fields: FrameFields,
    on_device: bool,
    /// Its place among the frames the decoder made since opening, from
    /// 0, those dropped included.
    #[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
    made: u64,
    pts: i64,
    /// Its place in decode-output order since the last seek.
    seq: u64,
    /// It came out after reading had failed, as the decoder drained: a
    /// frame the damage lost may come before it.
    after_failure: bool,
    /// Its place in output order from the start of the stream, when
    /// decoding started there.
    index: Option<u64>,
    /// How many frames came out just before it with the same pts.
    tie: u32,
    /// `tie` counts every frame of the stream before it with its pts:
    /// decoding began before the first of them. Not so for the first
    /// frame out after a seek (bar one to the start), nor for the frames
    /// that tie with it.
    anchored: bool,
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
    /// A seek stopped part way, or catching up was called off after it
    /// passed frames over: seek again to just after the last frame
    /// returned (or to the start) before decoding on.
    Lost,
}

/// Playback catching up ([`VideoDecode::catch_up`]).
#[derive(Debug, Clone, Copy)]
struct CatchUp {
    /// No frame before this pts is handed out.
    until: i64,
    /// [`VideoDecoder::skipping_sent`] when it began, or when decoding
    /// last found its place again.
    sent: u64,
    /// A frame decoded since then has been dropped.
    dropped: bool,
}

impl CatchUp {
    /// The decoder may have skipped a frame without a word.
    fn skipped(&self, dec: &VideoDecoder) -> bool {
        dec.skipping_sent() > self.sent
    }

    /// The decoder has gone past the frame after the last one returned.
    fn moved(&self, dec: &VideoDecoder) -> bool {
        self.dropped || self.skipped(dec)
    }
}

/// What an [`FfmpegVideo`] decodes on.
#[derive(Debug)]
pub(crate) enum On {
    Software,
    Device {
        kind: HwKind,
        dev: Arc<HwDevice>,
    },
    /// Software, single-threaded, in a device's place and labelled as on
    /// it, for fault injection where no device decodes.
    #[cfg(feature = "fault-injection")]
    StandIn(HwKind),
}

/// A fault armed by a [`FaultPlan`].
#[cfg(feature = "fault-injection")]
#[derive(Debug)]
struct Injection {
    plan: FaultPlan,
    /// "VA-API" or "Vulkan", as the error names the device.
    kind: String,
    /// Why the device "failed", once it has: every later call fails too,
    /// as a device's failure is sticky.
    fired: Option<String>,
}

#[cfg(feature = "fault-injection")]
impl Injection {
    /// `HwFailed`, once it has fired, or now when frame `made` is the
    /// plan's and `at` its fault.
    fn check(
        &mut self,
        path: &Path,
        made: Option<u64>,
        at: Option<Fault>,
    ) -> Result<(), MediaError> {
        if self.fired.is_none() && made == Some(self.plan.frame) && at == Some(self.plan.fault) {
            let reason = format!(
                "fault injected: frame {} {}",
                self.plan.frame,
                match self.plan.fault {
                    Fault::Decode => "failed to decode",
                    Fault::Download => "failed to download",
                }
            );
            tracing::warn!(path = %path.display(), "hardware decoding failed: {reason}");
            self.fired = Some(reason);
        }
        match &self.fired {
            Some(reason) => Err(MediaError::HwFailed {
                path: path.to_path_buf(),
                kind: self.kind.clone(),
                reason: reason.clone(),
            }),
            None => Ok(()),
        }
    }
}

/// [`VideoDecode`] on FFmpeg's decoders, in software or on a device.
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
    /// a seek that decodes forward goes on with the one before, and at
    /// the first frame handed out after catching up skipped frames.
    run: u64,
    runs: u64,
    seq: u64,
    /// Frames decoded since decoding last started at the start of the
    /// stream, when it did; counts positions in a stream whose timestamps
    /// go backwards.
    from_start: Option<u64>,
    /// The pts of the last frame decoded, to see timestamps go backwards
    /// and tie, how many frames before it had the same, and whether that
    /// count is anchored (see [`Raw::anchored`]).
    last_decoded: Option<i64>,
    last_tie: u32,
    last_anchored: bool,
    /// Two frames have come out with the same pts.
    ties_seen: bool,
    /// The last frame returned's place from the start of the stream, when
    /// known; its rank among frames that tie with it, and whether that is
    /// anchored; and whether the frame after it was known not to tie with
    /// it.
    current_index: Option<u64>,
    current_tie: u32,
    current_anchored: bool,
    current_last_tie: bool,
    not_seekable: bool,
    /// Since the last seek away from the start, no frame at or after the
    /// keyframe it landed on has come out: one before it is one of its
    /// leading pictures, and is dropped.
    leading: bool,
    /// Since opening or the last seek, a frame has come out that FFmpeg
    /// didn't flag corrupt and that isn't a leading picture: a frame
    /// flagged corrupt after it refers to a picture the file's damage
    /// lost, not to one before where decoding began.
    settled: bool,
    /// The frames coming out are flagged corrupt after a settled one, and
    /// the damage has been said: the rest are dropped without another
    /// word, up to the next frame that isn't.
    losing: bool,
    /// Since the last seek, the failure reading stopped with has been
    /// said as a `Damaged` event.
    failure_said: bool,
    /// The stream has no timestamps: pts are counted, as frame index times
    /// `frame_ticks`.
    no_timestamps: bool,
    /// The (average) frame duration in ticks, for the last frame's
    /// interval when the decoder gives it none.
    frame_ticks: i64,
    /// Where the last frame's interval ends, in ticks: as the container
    /// says, or once the last frame has been decoded when it says nothing
    /// or something before that frame.
    end_pts: Option<i64>,
    /// The greatest pts decoded since opening, and `AVFrame.duration` of
    /// its frame.
    greatest: Option<(i64, i64)>,
    catch_up: Option<CatchUp>,
    events: Vec<DecoderEvent>,
    stats: DecodeStats,
    decode_ms_total: f64,
    #[cfg(feature = "fault-injection")]
    injection: Option<Injection>,
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

/// What an error from the decoder means where it was met.
enum Failed {
    /// The device turned the stream down or failed on it: returned at
    /// once, never taken for damage, so that a fallback can act (D30).
    Device(MediaError),
    /// Reading the file failed: the stream ends there, until a seek.
    Reading(MediaError),
    /// Damage the decoder goes on past, said with a `Damaged` event.
    Damage(MediaError),
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
    /// Open the stream to decode in software, and decode its first
    /// presented frame, which fixes the origin of clip time. A hardware
    /// [`DecodeMode`](crate::backend::DecodeMode) in `req` is not looked
    /// at: the backend opens those.
    pub fn open(path: &Path, req: &VideoRequest) -> Result<Self, MediaError> {
        Self::open_on(path, req, On::Software)
    }

    /// [`open`](Self::open), decoding on `on`. With a device, a stream it
    /// can't decode is [`MediaError::HwUnsupported`] and its failure
    /// [`MediaError::HwFailed`], from here or later, never damage.
    pub(crate) fn open_on(path: &Path, req: &VideoRequest, on: On) -> Result<Self, MediaError> {
        let (mut dec, decode_path) = match &on {
            On::Software => (
                VideoDecoder::open_software(path, req.stream, req.threads, req.interrupt.clone())?,
                DecodePath::Software {
                    threads: req.threads,
                },
            ),
            On::Device { kind, dev } => (
                VideoDecoder::open_hardware(
                    path,
                    req.stream,
                    dev.clone(),
                    req.extra_hw_frames,
                    req.interrupt.clone(),
                )?,
                DecodePath::Hardware { kind: *kind },
            ),
            #[cfg(feature = "fault-injection")]
            On::StandIn(kind) => (
                VideoDecoder::open_software(
                    path,
                    req.stream,
                    std::num::NonZeroU32::new(1),
                    req.interrupt.clone(),
                )?,
                DecodePath::Hardware { kind: *kind },
            ),
        };
        // A fault is injected where a device decodes, or stands in.
        #[cfg(feature = "fault-injection")]
        let mut injection = match (&decode_path, req.fault) {
            (DecodePath::Hardware { kind }, Some(plan)) => Some(Injection {
                plan,
                kind: kind.to_string(),
                fired: None,
            }),
            _ => None,
        };
        // A Matroska track's default duration makes a rate whether its
        // frames come at it or not, so its first packets' timestamps are
        // read to check it.
        let scan = match dec.input().format().name().starts_with("matroska") {
            true => Some(dec.scan_timestamps(SCAN_PACKETS)?),
            false => None,
        };
        let index = dec.stream_index();
        let codec = dec.codec_name();
        let tb = dec.time_base();
        let never = StopToken::never();
        let first = loop {
            match dec.receive(Some(&never))? {
                Received::Frame(frame) => {
                    #[cfg(feature = "fault-injection")]
                    if let Some(i) = &mut injection {
                        i.check(
                            path,
                            dec.frames_decoded().checked_sub(1),
                            Some(Fault::Decode),
                        )?;
                    }
                    let fields = Self::fields(&mut dec, &frame)?;
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
                chroma_sub: conv.layout().chroma_sub(),
                pix_fmt: &source_name,
                codec: &codec,
                width: fields.width,
                height: fields.height,
            },
        );
        let facts = StreamFacts::read(path, &dec, origin_pts, scan.as_ref());
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
            duration: facts
                .end_pts
                .and_then(|e| Time::from_pts(e.saturating_sub(origin_pts), tb).ok()),
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
        let made = dec.frames_decoded().saturating_sub(1);
        let on_device = dec.on_device(&frame);
        let mut v = FfmpegVideo {
            dec,
            info,
            decode_path,
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
            last_tie: 0,
            last_anchored: true,
            ties_seen: false,
            current_index: None,
            current_tie: 0,
            current_anchored: true,
            current_last_tie: false,
            not_seekable: false,
            leading: false,
            settled: true,
            losing: false,
            failure_said: false,
            no_timestamps,
            frame_ticks,
            end_pts: facts.end_pts,
            greatest: Some((origin_pts, fields.duration)),
            catch_up: None,
            events,
            stats: DecodeStats::default(),
            decode_ms_total: 0.0,
            #[cfg(feature = "fault-injection")]
            injection,
        };
        let after_failure = v.dec.reading_failed();
        v.pending = Some(Raw {
            frame,
            fields,
            on_device,
            made,
            pts: origin_pts,
            seq: 0,
            after_failure,
            index: Some(0),
            tie: 0,
            anchored: true,
        });
        Ok(v)
    }

    fn path(&self) -> &Path {
        self.dec.path()
    }

    /// A frame's fields; for a device frame, as it will be downloaded.
    fn fields(dec: &mut VideoDecoder, frame: &ff::frame::Video) -> Result<FrameFields, MediaError> {
        let fields = ffi::frame_fields(frame);
        if !dec.on_device(frame) {
            return Ok(fields);
        }
        ffi::as_downloaded(frame, fields)
            .ok_or_else(|| dec.fail_device("its frame has no frames context"))
    }

    /// The injected fault's error, when it has fired or fires now, for
    /// frame `made` at `at`.
    #[cfg(feature = "fault-injection")]
    fn inject(&mut self, made: Option<u64>, at: Option<Fault>) -> Result<(), MediaError> {
        match &mut self.injection {
            Some(i) => i.check(self.dec.path(), made, at),
            None => Ok(()),
        }
    }

    /// What `e`, an error from [`decode`](Self::decode), means.
    fn failed(&self, e: MediaError) -> Failed {
        if e.is_device_failure() {
            Failed::Device(e)
        } else if self.dec.has_failed() {
            Failed::Reading(e)
        } else {
            Failed::Damage(e)
        }
    }

    /// The next frame from the decoder, with its pts. Frames FFmpeg flags
    /// corrupt are dropped. Before the first good frame since opening or
    /// a seek, they were decoded from references a seek skipped (or that
    /// come before the start of the stream), and are not part of the
    /// stream as a decode from the start shows it. After it, they refer
    /// to a picture the file's damage lost, which FFmpeg 7.1 and later's
    /// HEVC decoder skips without an error, flagging (from 8.0) the
    /// pictures that refer to it: the first of a run of them is returned
    /// as a decode error, for the caller to say as damage. Also dropped
    /// are the leading pictures of the keyframe a seek landed on, the
    /// frames shown before it that come out before it: open-GOP B-frames
    /// and RASL pictures decoded from references before it, which FFmpeg
    /// 6.1's H.264 decoder doesn't flag as it drains at the end of the
    /// stream, and HEVC's RADL pictures, decoded from it alone. A target
    /// among them is reached by seeking to before that keyframe.
    /// Notes timestamps that go backwards.
    fn decode(&mut self, stop: &StopToken) -> Result<Got, MediaError> {
        #[cfg(feature = "fault-injection")]
        self.inject(None, None)?;
        loop {
            let started = Instant::now();
            let got = self.dec.receive(Some(stop));
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let frame = match got? {
                Received::Frame(frame) => frame,
                Received::End => return Ok(Got::End),
                Received::Stopped => return Ok(Got::Stopped),
            };
            #[cfg(feature = "fault-injection")]
            self.inject(
                self.dec.frames_decoded().checked_sub(1),
                Some(Fault::Decode),
            )?;
            self.decode_ms_total += ms;
            self.stats.last_decode_ms = ms;
            let fields = Self::fields(&mut self.dec, &frame)?;
            if fields.corrupt {
                if self.settled && !self.losing {
                    self.losing = true;
                    return Err(self.lost_reference(fields.pts));
                }
                continue;
            }
            if self.leading
                && let (Some(key), Some(pts)) = (self.dec.landing_key(), fields.pts)
            {
                if pts < key {
                    continue;
                }
                self.leading = false;
            }
            self.settled = true;
            self.losing = false;
            return Ok(self.decoded(frame, fields));
        }
    }

    /// The damage [`decode`](Self::decode) returns for the first of a run
    /// of frames flagged corrupt, at `pts`, after a good one.
    fn lost_reference(&self, pts: Option<i64>) -> MediaError {
        let at = pts.map_or_else(|| "a frame".into(), |p| format!("the frame at pts {p}"));
        let after = self
            .last_decoded
            .map_or_else(String::new, |p| format!(" after pts {p}"));
        tracing::warn!(
            path = %self.path().display(),
            "{at} refers to a picture the file's damage lost{after}: it is dropped, \
             with the frames after it that FFmpeg flags corrupt"
        );
        MediaError::Decode {
            path: self.path().to_path_buf(),
            reason: format!(
                "decoding: {at} refers to a picture lost to damage (FFmpeg flags it corrupt)"
            ),
        }
    }

    fn decoded(&mut self, frame: ff::frame::Video, fields: FrameFields) -> Got {
        let on_device = self.dec.on_device(&frame);
        let made = self.dec.frames_decoded().saturating_sub(1);
        let from_start = self.from_start;
        let index = from_start.unwrap_or(0);
        self.from_start = from_start.map(|n| n + 1);
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
            // Its frames are counted from the start from now on.
            self.dec.skip_nonref_before(None);
            // Frame n is then the nth decoded, not the one at n frame
            // durations.
            if let Timing::Constant(rate) = self.info.timing {
                self.info.timing = Timing::Variable {
                    average: Some(rate),
                };
            }
        }
        let (tie, anchored) = match self.last_decoded {
            Some(last) if last == pts => (self.last_tie + 1, self.last_anchored),
            Some(_) => (0, true),
            // The first frame out after a seek: frames before it with its
            // pts weren't decoded, unless it is the first of the stream.
            None => (0, from_start == Some(0)),
        };
        self.ties_seen |= tie > 0;
        if self.greatest.is_none_or(|(g, _)| pts >= g) {
            self.greatest = Some((pts, fields.duration));
        }
        self.last_tie = tie;
        self.last_anchored = anchored;
        self.last_decoded = Some(pts);
        let seq = self.seq;
        self.seq += 1;
        Got::Frame(Raw {
            frame,
            fields,
            on_device,
            made,
            pts,
            seq,
            after_failure: self.dec.reading_failed(),
            index: from_start,
            tie,
            anchored,
        })
    }

    /// Forget what was decoded before a seek, and start a run.
    fn after_seek(&mut self, to_start: bool) {
        self.pending = None;
        self.last_decoded = None;
        self.from_start = to_start.then_some(0);
        self.leading = !to_start;
        self.settled = false;
        self.losing = false;
        self.failure_said = false;
        self.start_run();
    }

    /// Say the failure reading stopped with as a `Damaged` event, after
    /// the last frame handed out, with its cause, unless it has been said
    /// since the last seek: [`next_frame`](VideoDecode::next_frame) does
    /// before the first frame the decoder gives up after it, which may
    /// come after frames the damage lost, and before the error the stream
    /// then ends with.
    fn say_failure(&mut self) {
        if self.failure_said {
            return;
        }
        if let Some(e) = self.dec.read_failure() {
            self.failure_said = true;
            self.events.push(DecoderEvent::Damaged {
                reason: e.to_string(),
                last_good_pts: self.last_returned(),
                cause: self.dec.read_cause().unwrap_or(DamageCause::Read),
            });
        }
    }

    /// Start a run: the next frame decoded may not follow the last one.
    fn start_run(&mut self) {
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
    /// with no more work than a seek: `t` is ahead, and the last keyframe
    /// shown at or before it has already been read, so `t` lies in the
    /// GOP being decoded. The index lists keyframes by dts, so a
    /// keyframe's pts is taken as its dts plus the least lag of pts
    /// behind dts seen in the keyframes read, as the MOV demuxer itself
    /// shifts a seek's target. Without an index that lists what lies
    /// ahead, `t` at most a second ahead.
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
        let read = self.dec.read_position();
        // The index says what lies ahead only when it lists something
        // after what has been read: a Matroska demuxer's lists only the
        // keyframes read until it has read its Cues, and a file written to
        // a pipe has none.
        let ahead_listed =
            read.is_some_and(|r| ffi::index_last(input, stream).is_some_and(|l| l > r));
        if !ahead_listed {
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
        let lag = self.dec.key_lag();
        match ffi::index_keyframe_before(input, stream, t.saturating_sub(lag)) {
            Some(k) => read.is_some_and(|read| k <= read),
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
        // The last keyframe a cut file holds, once a seek to it was tried.
        let mut inside: Option<i64> = None;
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
                    Err(e) => match self.failed(e) {
                        Failed::Device(e) => return Err(e),
                        Failed::Reading(_) => break None,
                        Failed::Damage(e) => self.events.push(DecoderEvent::Damaged {
                            reason: e.to_string(),
                            last_good_pts: None,
                            cause: DamageCause::Corrupt,
                        }),
                    },
                }
            };
            let landed = self.dec.landing();
            let got = first.is_some();
            if let Some(r) = first
                && (r.pts <= t || r.pts == self.info.origin_pts)
            {
                return Ok(Some(Start::Seeked(Some(r))));
            }
            // Nothing of the stream could be read: the keyframe landed on
            // lies past the cut of a file cut short of its index. The last
            // keyframe the file holds is the nearest place to decode from,
            // and the start may be far before it.
            if !got
                && landed.is_none()
                && self.dec.has_failed()
                && let Some(k) = self.dec.keyframe_inside(inside)
            {
                inside = Some(k);
                // The index lists keyframes by dts, and the demuxer shifts
                // a seek's target back by the least lag of pts behind it
                // (see `forward_is_cheaper`).
                ts = k.saturating_add(self.dec.key_lag());
                continue;
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

    /// The pts at which the interval of the last frame, at `pts` and
    /// lasting `duration` ticks (0 when unknown), ends: where the
    /// container says the stream ends, when that is after the frame, as
    /// `info().duration` says. Else after its duration, or the average
    /// frame duration, which `info().duration` then says from now on.
    fn last_end(&mut self, pts: i64, duration: i64) -> i64 {
        if let Some(end) = self.end_pts
            && end > pts
        {
            return end;
        }
        let d = if duration > 0 {
            duration
        } else {
            self.frame_ticks
        };
        let end = pts.saturating_add(d);
        self.end_pts = Some(end);
        self.info.duration = self.info.clip_time(end).ok();
        end
    }

    /// Make the frame handed out for `raw`, followed by `next_pts` when
    /// that is known.
    fn make_frame(
        &mut self,
        raw: Raw,
        next_pts: Option<i64>,
    ) -> Result<Arc<VideoFrame>, MediaError> {
        #[cfg(feature = "fault-injection")]
        self.inject(Some(raw.made), Some(Fault::Download))?;
        // Only now off the device: what a seek decoded on the way stayed
        // there.
        let (picture, f) = if raw.on_device {
            let started = Instant::now();
            let down = self.dec.download(&raw.frame)?;
            self.stats.last_download_ms = started.elapsed().as_secs_f64() * 1000.0;
            let d = ffi::frame_fields(&down);
            let f = FrameFields {
                format: d.format,
                width: d.width,
                height: d.height,
                ..raw.fields
            };
            (down, f)
        } else {
            (raw.frame, raw.fields)
        };
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
                chroma_sub: self.conv.layout().chroma_sub(),
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
            Conversion::Native { .. } => picture,
            Conversion::Scaled { scaler, .. } => {
                let started = Instant::now();
                let out = scaler.scale(&picture).map_err(|e| MediaError::Decode {
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
        self.current_index = raw.index;
        self.current_tie = raw.tie;
        self.current_anchored = raw.anchored;
        self.current_last_tie = next_pts.is_some_and(|n| n != raw.pts);
        self.stats.frames_returned += 1;
        let frame = Arc::new(out);
        self.current = Some(frame.clone());
        Ok(frame)
    }

    /// Decode one frame ahead into `pending`, if it isn't there, so the
    /// frame about to be handed out knows its duration. A stop, the end
    /// or an error leaves `pending` empty: a read error that ends the
    /// stream comes back from the next call, and damage that doesn't
    /// becomes an event, after the frame about to be handed out, the last
    /// one decoded. The device turning the stream down or failing on it is
    /// returned. `true` on a stop.
    fn fill_pending(&mut self, stop: &StopToken) -> Result<bool, MediaError> {
        if self.pending.is_some() {
            return Ok(false);
        }
        match self.decode(stop) {
            Ok(Got::Frame(r)) => self.pending = Some(r),
            Ok(Got::Stopped) => return Ok(true),
            Ok(Got::End) => {}
            Err(e) => match self.failed(e) {
                Failed::Device(e) => return Err(e),
                Failed::Reading(_) => {}
                Failed::Damage(e) => self.events.push(DecoderEvent::Damaged {
                    reason: e.to_string(),
                    last_good_pts: self.last_decoded,
                    cause: DamageCause::Corrupt,
                }),
            },
        }
        Ok(false)
    }

    fn exact(
        &mut self,
        t: i64,
        stop: &StopToken,
    ) -> Result<Option<(Arc<VideoFrame>, Exactness, SeekCost)>, MediaError> {
        let mut cost = SeekCost::default();
        // In a stream whose timestamps go backwards, a frame between the
        // two shown last can be shown at `t`.
        if self.not_seekable {
            return self.counted(t, stop, cost);
        }
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
                            // The greatest pts, later frames winning ties.
                            if best.as_ref().is_none_or(|b| r.pts >= b.pts) {
                                best = Some(r);
                            }
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
                    Err(e) => match self.failed(e) {
                        Failed::Device(e) => return Err(e),
                        Failed::Reading(e) => {
                            damage = Some(e.to_string());
                            break;
                        }
                        Failed::Damage(e) => self.events.push(DecoderEvent::Damaged {
                            reason: e.to_string(),
                            last_good_pts: best.as_ref().map(|b| b.pts),
                            cause: DamageCause::Corrupt,
                        }),
                    },
                }
            }
        }
        if self.not_seekable {
            // This seek found the timestamps going backwards: the frame it
            // stopped at may not be the one shown at `t`.
            return self.counted(t, stop, cost);
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
                    self.fill_pending(stop)?;
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
            self.failure_said = true;
            self.events.push(DecoderEvent::Damaged {
                reason: reason.clone(),
                last_good_pts: Some(frame.pts),
                cause: self.dec.read_cause().unwrap_or(DamageCause::Read),
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
            && t >= self.last_end(frame.pts, self.frame_duration_ticks(&frame))
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
        // The frame shown at `t`, by its place in output order, and its
        // pts; and the frame with the greatest pts, with its duration.
        let mut best: Option<(u64, i64)> = None;
        let mut greatest: Option<(u64, i64, i64)> = None;
        let mut damage = None;
        let mut index = 0u64;
        loop {
            match self.decode(stop) {
                Ok(Got::Frame(r)) => {
                    cost.frames_decoded += 1;
                    if r.pts <= t && best.is_none_or(|(_, p)| r.pts >= p) {
                        best = Some((index, r.pts));
                    }
                    if greatest.is_none_or(|(_, p, _)| r.pts >= p) {
                        greatest = Some((index, r.pts, r.fields.duration));
                    }
                    index += 1;
                }
                Ok(Got::End) => break,
                Ok(Got::Stopped) => {
                    self.position = Position::Lost;
                    return Ok(None);
                }
                Err(e) => match self.failed(e) {
                    Failed::Device(e) => return Err(e),
                    Failed::Reading(e) => {
                        damage = Some(e.to_string());
                        break;
                    }
                    Failed::Damage(e) => self.events.push(DecoderEvent::Damaged {
                        reason: e.to_string(),
                        last_good_pts: None,
                        cause: DamageCause::Corrupt,
                    }),
                },
            }
        }
        let last = greatest.map(|(i, pts, d)| (i, self.last_end(pts, d)));
        // Before every frame: the first one.
        let target = best.map_or(0, |(i, _)| i);
        self.seek_to_start(&mut cost)?;
        let Some(frame) = self.skip_to(target, stop, &mut cost)? else {
            self.position = Position::Lost;
            return Ok(None);
        };
        let exactness = match (best, last, damage) {
            (_, _, Some(reason)) => {
                self.failure_said = true;
                self.events.push(DecoderEvent::Damaged {
                    reason,
                    last_good_pts: Some(frame.pts),
                    cause: self.dec.read_cause().unwrap_or(DamageCause::Read),
                });
                if frame.pts == t {
                    Exactness::Exact
                } else {
                    Exactness::Truncated {
                        last_good_pts: frame.pts,
                    }
                }
            }
            (None, _, None) => Exactness::BeforeFirst,
            (Some((i, _)), Some((last, end)), None) if i == last && t >= end => {
                Exactness::AfterLast
            }
            _ => Exactness::Exact,
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
            match self.decode(stop) {
                Ok(Got::Frame(r)) if n == index => {
                    self.position = Position::InStep;
                    // Durations aren't known in such a stream.
                    return self.make_frame(r, None).map(Some);
                }
                Ok(Got::Frame(_)) => {
                    cost.frames_decoded += 1;
                    n += 1;
                }
                Ok(Got::End) => {
                    return Err(MediaError::Decode {
                        path: self.path().to_path_buf(),
                        reason: format!("the stream ended before frame {index}"),
                    });
                }
                Ok(Got::Stopped) => return Ok(None),
                Err(e) => match self.failed(e) {
                    Failed::Device(e) | Failed::Reading(e) => return Err(e),
                    // Said by the first decode of the stream already.
                    Failed::Damage(_) => {}
                },
            }
        }
    }

    /// One BACKWARD seek and its first frame, whatever its pts: the
    /// keyframe a seek to `t` lands on, since its leading pictures are
    /// dropped, which is after `t` when `t` is one of them. Past the cut
    /// of a file cut short of its index, the last keyframe the file holds,
    /// with a `Damaged` event. From the start when the seek finds no frame.
    /// The decoder stays where it is, so an exact seek to `t` after it
    /// decodes forward when the frame is at or before `t`.
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
        // The last keyframe a cut file holds, once a seek to it was tried.
        let mut inside: Option<i64> = None;
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
                Err(e) if e.is_device_failure() => return Err(e),
                Err(e) if !self.dec.has_failed() => self.events.push(DecoderEvent::Damaged {
                    reason: e.to_string(),
                    last_good_pts: None,
                    cause: DamageCause::Corrupt,
                }),
                // Nothing of the stream could be read: the seek landed past
                // the cut of a file cut short of its index, as in `land`.
                // The last keyframe the file holds is the one to show, with
                // the damage said.
                Err(e) if !from_start && self.dec.landing().is_none() => {
                    match self.dec.keyframe_inside(inside) {
                        Some(k) => {
                            if inside.is_none() {
                                self.events.push(DecoderEvent::Damaged {
                                    reason: e.to_string(),
                                    last_good_pts: None,
                                    cause: DamageCause::Cut,
                                });
                            }
                            inside = Some(k);
                            self.dec.seek(k.saturating_add(self.dec.key_lag()))?;
                            cost.seeks += 1;
                            cost.reseeks += 1;
                            self.after_seek(false);
                        }
                        None => {
                            from_start = true;
                            self.seek_to_start(&mut cost)?;
                        }
                    }
                }
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
        self.fill_pending(stop)?;
        let next = self.pending.as_ref().map(|p| p.pts);
        let frame = self.make_frame(first, next)?;
        Ok(Some((frame, Exactness::KeyframeOnly, cost)))
    }

    /// Put the decoder back after a stopped seek: just after the last
    /// frame handed out, or at the start. `false` when stopped again.
    fn resume(&mut self, stop: &StopToken) -> Result<bool, MediaError> {
        if self.position == Position::InStep {
            return Ok(true);
        }
        // Finding the place again skips no frames: catching up waits.
        self.dec.skip_nonref_before(None);
        let found = self.reposition(stop);
        if let Some(c) = &mut self.catch_up
            && matches!(found, Ok(true))
        {
            c.sent = self.dec.skipping_sent();
            c.dropped = false;
        }
        self.set_skipping();
        found
    }

    /// Have the decoder skip frames nothing refers to while catching up,
    /// except in a stream whose frames are counted from the start: one
    /// whose timestamps go backwards, or that has none. A frame skipped
    /// leaves no trace, so places counted from the start are lost.
    fn set_skipping(&mut self) {
        let until = self
            .catch_up
            .as_ref()
            .map(|c| c.until)
            .filter(|_| !self.not_seekable && !self.no_timestamps);
        if until.is_some() {
            self.from_start = None;
        }
        self.dec.skip_nonref_before(until);
    }

    /// [`resume`](Self::resume)'s work, once it is known to be needed.
    fn reposition(&mut self, stop: &StopToken) -> Result<bool, MediaError> {
        let mut cost = SeekCost::default();
        let Some(current) = self.current.clone() else {
            self.seek_to_start(&mut cost)?;
            self.position = Position::InStep;
            return Ok(true);
        };
        let (index, tie, anchored) = (self.current_index, self.current_tie, self.current_anchored);
        let last_tie = self.current_last_tie;
        // From the start where seeks go there, else from a seek that lands
        // at or before it. The frames tied with it are counted as they
        // were: from before the first of them when its rank is anchored,
        // so the seek goes before its pts; else from the same landing, at
        // its pts. A frame known to be the last of its ties needs no rank.
        let from_start = self.not_seekable || self.no_timestamps;
        let mut next = if from_start {
            self.seek_to_start(&mut cost)?;
            None
        } else {
            let t = match self.ties_seen && anchored && !last_tie {
                true => current.pts.saturating_sub(1),
                false => current.pts,
            };
            match self.land(t, stop, &mut cost)? {
                Some(Start::Seeked(first)) => first,
                Some(Start::Forward) => None,
                None => return Ok(false),
            }
        };
        // Decode up to it, without making it again: what follows it is then
        // `pending`, and goes on with its run when it is known to follow
        // it. Without its place from the start, a stream whose timestamps
        // go backwards may show its pts twice, and the first is taken.
        let mut sure = index.is_some() || !from_start;
        // The frame decoded last is it, or has its pts.
        let (mut was_it, mut at_its_pts) = (false, false);
        loop {
            let r = match next.take() {
                Some(r) => r,
                None => match self.decode(stop) {
                    Ok(Got::Frame(r)) => r,
                    // It was the last frame; an error that ends the stream
                    // comes back from the next decode.
                    Ok(Got::End) => break,
                    Ok(Got::Stopped) => return Ok(false),
                    Err(e) => match self.failed(e) {
                        Failed::Device(e) => return Err(e),
                        Failed::Reading(_) => break,
                        Failed::Damage(e) => {
                            self.events.push(DecoderEvent::Damaged {
                                reason: e.to_string(),
                                last_good_pts: Some(current.pts),
                                cause: DamageCause::Corrupt,
                            });
                            continue;
                        }
                    },
                },
            };
            let after = match (index, r.index) {
                (Some(i), Some(j)) if from_start => j > i,
                _ if was_it => true,
                // Past its ties without telling which it was: after the
                // last of them, as it was when known to be.
                _ if at_its_pts && r.pts != current.pts => {
                    sure &= last_tie;
                    true
                }
                // Past it, in a stream whose timestamps don't go backwards.
                _ if !from_start && r.pts > current.pts => {
                    sure &= !self.ties_seen;
                    true
                }
                _ => false,
            };
            if after {
                self.pending = Some(r);
                break;
            }
            // Ranks counted alike, or none to count.
            let comparable = r.anchored == anchored || !self.ties_seen;
            was_it = r.pts == current.pts && comparable && r.tie == tie;
            at_its_pts = r.pts == current.pts;
        }
        self.position = Position::InStep;
        if sure {
            self.run = current.run;
            self.seq = current.seq + 1;
        } else {
            // The frame after it, as far as can be told: in a run of its own.
            self.start_run();
        }
        if let Some(p) = &mut self.pending {
            p.seq = self.seq;
            self.seq += 1;
        }
        Ok(true)
    }
}

impl FfmpegVideo {
    /// The pts of the last frame returned, if any.
    pub(crate) fn last_returned(&self) -> Option<i64> {
        self.current.as_ref().map(|c| c.pts)
    }

    /// Become the software decode that a hardware decode of kind `from`
    /// fell back to for `reason`, just opened on the same stream: frames
    /// say so ([`DecodePath::FellBack`]), and a [`DecoderEvent::FellBack`]
    /// comes before this decoder's own events.
    ///
    /// When the device failed mid-stream, `failed` is that decoder, and
    /// this one goes on where it left off (D30): from the frame after the
    /// last one it returned, found as after a stopped seek, which keeps
    /// its run when the frame is known to follow on, and so with no gap
    /// and no repeat; catching up as it was; and knowing what it learned
    /// of the stream (its end, timestamps that go backwards, ties). Its
    /// events not yet taken come first, and `LayoutChanged` after the
    /// fallback's when software's frames have another layout or size than
    /// the last ones it made.
    pub(crate) fn fell_back(
        &mut self,
        from: HwKind,
        reason: FallbackReason,
        failed: Option<&mut FfmpegVideo>,
    ) {
        let mut events = Vec::new();
        let mut changed = None;
        if let Some(old) = failed {
            events.append(&mut old.events);
            self.take_over(old);
            let (_, w, h) = self.conv.source();
            let (_, ow, oh) = old.conv.source();
            if **self.conv.layout() != **old.conv.layout() || (w, h) != (ow, oh) {
                changed = Some(DecoderEvent::LayoutChanged {
                    layout: self.conv.layout().clone(),
                    size: (w, h),
                });
            }
        }
        events.push(DecoderEvent::FellBack {
            from,
            reason: reason.clone(),
        });
        events.append(&mut self.events);
        events.extend(changed);
        self.events = events;
        self.decode_path = DecodePath::FellBack { from, reason };
    }

    /// Take over from `old` (see [`fell_back`](Self::fell_back)).
    fn take_over(&mut self, old: &FfmpegVideo) {
        // Run numbers given out stay unique.
        self.runs = self.runs.max(old.runs);
        self.ties_seen |= old.ties_seen;
        if old.not_seekable && !self.not_seekable {
            self.not_seekable = true;
            self.info.timing = old.info.timing;
        }
        if self
            .end_pts
            .is_none_or(|e| old.end_pts.is_some_and(|o| o > e))
        {
            self.end_pts = old.end_pts;
            self.info.duration = old.info.duration;
        }
        if let Some((g, d)) = old.greatest
            && self.greatest.is_none_or(|(s, _)| g > s)
        {
            self.greatest = Some((g, d));
        }
        if let Some(current) = &old.current {
            self.current = Some(current.clone());
            self.current_index = old.current_index;
            self.current_tie = old.current_tie;
            self.current_anchored = old.current_anchored;
            self.current_last_tie = old.current_last_tie;
            self.run = old.run;
            self.seq = old.seq;
            // Found again before the next frame is decoded, from the frame
            // just decoded at open.
            self.pending = None;
            self.position = Position::Lost;
        }
        self.catch_up = old.catch_up.map(|c| CatchUp {
            until: c.until,
            sent: self.dec.skipping_sent(),
            dropped: false,
        });
        self.set_skipping();
    }

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
        // A seek ends catching up: its decode skips nothing, and it can't
        // decode forward over frames catching up passed over.
        if let Some(c) = self.catch_up.take() {
            self.dec.skip_nonref_before(None);
            if c.moved(&self.dec) {
                self.position = Position::Lost;
            }
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
            let mut raw = match self.pending.take() {
                Some(r) => r,
                None => match self.decode(stop) {
                    Ok(Got::Frame(r)) => r,
                    Ok(Got::End) => {
                        // Every frame has been decoded: the last one is known.
                        if let Some((pts, duration)) = self.greatest {
                            self.last_end(pts, duration);
                        }
                        return Ok(NextFrame::End);
                    }
                    Ok(Got::Stopped) => return Ok(NextFrame::Stopped),
                    Err(e) => match self.failed(e) {
                        Failed::Device(e) => return Err(e),
                        Failed::Reading(e) => {
                            self.say_failure();
                            return Err(e);
                        }
                        // One that doesn't end the stream is an event, as
                        // where the frame after this one is decoded ahead.
                        Failed::Damage(e) => {
                            self.events.push(DecoderEvent::Damaged {
                                reason: e.to_string(),
                                last_good_pts: self.last_decoded,
                                cause: DamageCause::Corrupt,
                            });
                            continue;
                        }
                    },
                },
            };
            if let Some(c) = &mut self.catch_up
                && raw.pts < c.until
            {
                c.dropped = true;
                continue;
            }
            // Caught up: nothing after this frame is skipped.
            if self.catch_up.is_some() {
                self.dec.skip_nonref_before(None);
            }
            if self.fill_pending(stop)? {
                // Handed out once the frame after it is known. Until then
                // catching up isn't over: a seek, or catching up to before
                // it, finds the frames it passed over again.
                self.pending = Some(raw);
                return Ok(NextFrame::Stopped);
            }
            if self.catch_up.as_ref().is_some_and(|c| c.skipped(&self.dec)) {
                // The frame before this one may be one the decoder skipped.
                self.start_run();
                raw.seq = self.seq;
                self.seq += 1;
                if let Some(p) = &mut self.pending {
                    p.seq = self.seq;
                    self.seq += 1;
                }
            }
            if raw.after_failure {
                self.say_failure();
            }
            let next = self.pending.as_ref().map(|p| p.pts);
            let frame = self.make_frame(raw, next)?;
            // Caught up once it is handed out: a device that fails to
            // download it leaves catching up for the fallback to go on.
            self.catch_up = None;
            return Ok(NextFrame::Frame(frame));
        }
    }

    fn catch_up(&mut self, until: Option<Time>) {
        let t = until
            .and_then(|t| t.to_pts(self.info.time_base, Rounding::Ceil).ok())
            .map(|t| self.info.origin_pts.saturating_add(t));
        let sent = self.dec.skipping_sent();
        self.catch_up = match (self.catch_up.take(), t) {
            // Further on: frames passed over are passed over still.
            (Some(c), Some(until)) if until >= c.until => Some(CatchUp { until, ..c }),
            (old, t) => {
                // Called off, or to before where it got to: decoding goes
                // on from the frame after the last one returned.
                if old.is_some_and(|c| c.moved(&self.dec)) {
                    self.position = Position::Lost;
                }
                t.map(|until| CatchUp {
                    until,
                    sent,
                    dropped: false,
                })
            }
        };
        self.set_skipping();
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
            download_buffers: self.dec.download_buffers(),
            ..self.stats
        }
    }
}

/// What a stream says about its timing and mapping, besides the frames.
struct StreamFacts {
    /// Where the last frame's interval ends, in the stream's ticks, as
    /// the container says.
    end_pts: Option<i64>,
    timing: Timing,
    frame_count: Option<u64>,
    mapping: SourceMapping,
}

impl StreamFacts {
    /// `scan`: the pts of a Matroska stream's first packets, and whether
    /// the stream ended within them.
    fn read(
        path: &Path,
        dec: &VideoDecoder,
        origin_pts: i64,
        scan: Option<&(Vec<i64>, bool)>,
    ) -> StreamFacts {
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
        // The stream's own duration (MP4), else the duration Matroska
        // muxers tag each track with, else the file's, which takes in every
        // stream.
        let nopts = ff::ffi::AV_NOPTS_VALUE;
        let stream_end = st.as_ref().and_then(|s| {
            let (start, d) = (s.start_time(), s.duration());
            if d == nopts || d <= 0 {
                return None;
            }
            let start = if start == nopts { origin_pts } else { start };
            Some(start.saturating_add(d))
        });
        let tagged_end = || {
            let tag = st.as_ref()?.metadata().get("DURATION").map(str::to_owned)?;
            tag_time(&tag)?.to_pts(tb, Rounding::Nearest).ok()
        };
        let file_end = || {
            let d = input.duration();
            if d == nopts || d <= 0 {
                return None;
            }
            let start = match ffi::format_start_time(input) {
                s if s == nopts => 0,
                s => s,
            };
            let micros = Rational::new(start.saturating_add(d), 1_000_000).ok()?;
            Time::seconds(micros).to_pts(tb, Rounding::Nearest).ok()
        };
        let end_pts = stream_end
            .or_else(tagged_end)
            .or_else(file_end)
            .filter(|&e| e > origin_pts);
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
            // FFmpeg sets both rates from the default duration, so they
            // always agree when there is one: the blocks' timestamps say
            // whether frames come at it.
            _ if !mov => match (average, real) {
                (Some(a), Some(r)) if a == r => snap(a).filter(|&rate| {
                    let (scanned, ended) = scan.map_or((&[][..], false), |(p, e)| (&p[..], *e));
                    let cues = ffi::index_timestamps(input, index);
                    on_rate(rate, tb, scanned, ended, &cues)
                }),
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
            end_pts,
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

/// Whether a Matroska stream's timestamps, in ticks of `tb`, come at
/// `rate`, to within a tick (Matroska rounds them to its timestamp
/// scale): `scanned`, the pts of its first packets, sorted, are one frame
/// apart, all but those the reorder window may still fill unless the
/// stream `ended` within them; and every entry of its index (the Cues,
/// keyframes) lies on the same grid. A stream that drops or repeats
/// frames further on with its timestamps on the grid passes.
fn on_rate(rate: FrameRate, tb: Rational, scanned: &[i64], ended: bool, cues: &[i64]) -> bool {
    let mut pts = scanned.to_vec();
    pts.sort_unstable();
    if !ended {
        pts.truncate(pts.len().saturating_sub(REORDER_WINDOW));
    }
    let Some(&first) = pts.first() else {
        return false;
    };
    // Tick p is frame k when |p × tb − k / rate| ≤ tb, that is
    // |p × tn × num − k × den × td| ≤ tn × num.
    let r = rate.as_rational();
    let (num, den) = (i128::from(r.num()), i128::from(r.den()));
    let (tn, td) = (i128::from(tb.num()), i128::from(tb.den()));
    let tick = tn * num;
    let frame = den * td;
    // Two pts can be nearly 2^64 apart, and `tb` holds FFmpeg's ints and
    // `rate` a standard one, so these stay well inside an i128.
    let since = |p: i64| i128::from(p) - i128::from(first);
    let off = |p: i64, k: i128| (since(p) * tick - k * frame).abs();
    let consecutive = pts
        .iter()
        .enumerate()
        .all(|(k, &p)| off(p, k as i128) <= tick);
    // The nearest frame to each cue.
    let on_grid = cues.iter().all(|&c| {
        let k = (since(c) * tick + frame / 2).div_euclid(frame);
        off(c, k) <= tick
    });
    consecutive && on_grid
}

/// A Matroska `DURATION` tag's time, `HH:MM:SS.fraction` as FFmpeg's
/// and mkvmerge's muxers write it.
fn tag_time(tag: &str) -> Option<Time> {
    let mut parts = tag.trim().splitn(3, ':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    let int = |p: &str| -> Option<i64> {
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    // Nanoseconds at most.
    let digits = frac.get(..frac.len().min(9))?;
    let frac = match digits {
        "" => Rational::from_integer(0),
        d => Rational::new(int(d)?, 10i64.pow(d.len() as u32)).ok()?,
    };
    let secs = int(h)?
        .checked_mul(3600)?
        .checked_add(int(m)?.checked_mul(60)?)?
        .checked_add(int(whole)?)?;
    Some(Time::seconds(
        Rational::from_integer(secs).checked_add(frac).ok()?,
    ))
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
    fn matroska_duration_tags_are_read_to_the_nanosecond() {
        let t = |s: &str| tag_time(s).map(|t| t.as_rational());
        let r = |n, d| Some(Rational::new(n, d).unwrap());
        assert_eq!(t("00:00:01.601000000"), r(1601, 1000));
        assert_eq!(t("01:02:03.5"), r(7447, 2));
        assert_eq!(t("00:00:07"), r(7, 1));
        // Beyond nanoseconds is left out.
        assert_eq!(t("00:00:00.0000000019"), r(1, 1_000_000_000));
        for bad in ["", "1.5", "00:00:-1.0", "00:00:01.5e3", "a:b:c"] {
            assert_eq!(t(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_matroska_rate_needs_blocks_that_come_at_it() {
        let ms = Rational::new(1, 1000).unwrap();
        let at = |rate: FrameRate, n: i64| {
            (0..n)
                .map(|k| {
                    rate.frame_to_time(k)
                        .unwrap()
                        .to_pts(ms, Rounding::Nearest)
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };
        let ntsc = at(FrameRate::FPS_59_94, 30);
        assert!(on_rate(FrameRate::FPS_59_94, ms, &ntsc, true, &ntsc[..1]));
        // The last frames in pts order may still be to come in decode
        // order, unless the stream ended.
        let mut unread = at(FrameRate::FPS_30, 64);
        unread.remove(60);
        assert!(on_rate(FrameRate::FPS_30, ms, &unread, false, &[0, 267]));
        assert!(!on_rate(FrameRate::FPS_30, ms, &unread, true, &[0, 267]));
        // A missing frame, two at one millisecond, and a burst.
        let mut missing = at(FrameRate::FPS_30, 30);
        missing.remove(5);
        assert!(!on_rate(FrameRate::FPS_30, ms, &missing, true, &[]));
        let mut tied = at(FrameRate::FPS_30, 30);
        tied[5] = tied[4];
        assert!(!on_rate(FrameRate::FPS_30, ms, &tied, true, &[]));
        assert!(!on_rate(
            FrameRate::FPS_30,
            ms,
            &[0, 11, 22, 100, 111],
            true,
            &[]
        ));
        // A keyframe off the grid further on, and no blocks at all.
        let thirty = at(FrameRate::FPS_30, 30);
        assert!(!on_rate(FrameRate::FPS_30, ms, &thirty, false, &[0, 1250]));
        assert!(!on_rate(FrameRate::FPS_30, ms, &[], false, &[]));
        // Cues as far off as an i64 goes, on the grid and off it.
        let later: Vec<i64> = thirty.iter().map(|p| p + 1000).collect();
        let far = i64::MIN + 8;
        assert!(on_rate(FrameRate::FPS_30, ms, &later, false, &[far]));
        assert!(!on_rate(FrameRate::FPS_30, ms, &later, false, &[i64::MIN]));
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
