//! Backends that stand in for `FfmpegBackend` in tests (M1 plan section
//! 3.2): [`SyntheticBackend`] makes frames in memory, as slowly as a test
//! asks, and [`GatedBackend`] holds another backend's opens and decodes
//! until its [`Gate`] is released.
//!
//! Neither reads a file: the path a test opens is only a name. They let
//! the playback engine's tests (a decoder that takes 50 ms a frame, one
//! that never answers) run without FFmpeg or a clock that matters.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use spherecut_core::video::{ColorFamily, PixelLayout, PlaneDesc, SampleBytes, VideoPlanes};
use spherecut_core::{FrameRate, Rounding, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioRequest, DecodePath, DecodeStats, DecoderEvent, Exactness, MediaBackend,
    NextFrame, SeekCost, SeekMode, SeekOutcome, SourceMapping, Timing, VideoDecode, VideoFrame,
    VideoRequest, VideoStreamInfo,
};
use spherecut_media::color::{self, ColorContext};
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_media::spherical::Projection;

use crate::media::CLIP_COLOR;

/// A clip [`SyntheticBackend`] decodes: `frames` frames of `width` ×
/// `height` 8-bit 4:2:0 at `rate`, a keyframe every `keyint`, each taking
/// `frame_cost` to decode. Frame `n` has pts `n` in a time base of one
/// frame, and says which it is (see [`frame_index`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticClip {
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    pub frames: u64,
    pub keyint: u64,
    pub frame_cost: Duration,
}

impl Default for SyntheticClip {
    /// 30 frames of 64x32 at 30 fps, a keyframe every 8, decoded at once.
    fn default() -> Self {
        SyntheticClip {
            width: 64,
            height: 32,
            rate: FrameRate::FPS_30,
            frames: 30,
            keyint: 8,
            frame_cost: Duration::ZERO,
        }
    }
}

/// Decodes [`SyntheticClip`]s, whatever the path. A seek decodes from the
/// keyframe before its target, or forward from the frame last handed out
/// when that is closer, at `frame_cost` a frame, and stops between frames
/// (and every millisecond within one) once its token says to.
#[derive(Debug, Clone, Default)]
pub struct SyntheticBackend {
    pub clip: SyntheticClip,
}

impl SyntheticBackend {
    pub fn new(clip: SyntheticClip) -> Self {
        SyntheticBackend { clip }
    }
}

impl MediaBackend for SyntheticBackend {
    fn name(&self) -> &'static str {
        "synthetic"
    }

    /// Always an error: a synthetic clip has no file.
    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        Err(ProbeError::from(MediaError::Decode {
            path: path.to_path_buf(),
            reason: "a synthetic clip has no file to probe".into(),
        }))
    }

    fn open_video(
        &self,
        path: &Path,
        _req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        Ok(Box::new(SyntheticVideo::new(path, self.clip.clone())?))
    }

    fn open_audio(
        &self,
        _path: &Path,
        _req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        Err(MediaError::NotYet {
            what: "synthetic audio (M1 slice 6)",
        })
    }
}

/// The frame a [`SyntheticBackend`] frame is: the first 8 bytes of its
/// luma plane, little-endian.
pub fn frame_index(frame: &VideoFrame) -> Option<u64> {
    let luma = frame.plane(0)?;
    let bytes = luma.bytes.get(..8)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// A [`SyntheticClip`] being decoded.
#[derive(Debug)]
struct SyntheticVideo {
    clip: SyntheticClip,
    path: std::path::PathBuf,
    info: VideoStreamInfo,
    path_used: DecodePath,
    /// The frame last handed out.
    current: Option<Arc<VideoFrame>>,
    catch_up: Option<u64>,
    /// The run frames decoded now belong to, the last run number given
    /// out, and the next frame's place in the run: every frame decoded
    /// takes one, handed out or not, so that seqs that follow on are
    /// frames that follow on (D29).
    runs: u64,
    run: u64,
    seq: u64,
    stats: DecodeStats,
}

impl SyntheticVideo {
    fn new(path: &Path, clip: SyntheticClip) -> Result<Self, MediaError> {
        let invalid = |reason: &str| MediaError::Decode {
            path: path.to_path_buf(),
            reason: reason.into(),
        };
        if clip.frames == 0 || clip.keyint == 0 || clip.width < 4 || clip.height < 2 {
            return Err(invalid(
                "a synthetic clip needs frames, a keyframe interval and 4x2 pixels",
            ));
        }
        let frames = i64::try_from(clip.frames).map_err(|_| invalid("too many frames"))?;
        let one = PlaneDesc {
            components: 1,
            bytes: SampleBytes::One,
            log2_sub: (0, 0),
        };
        let chroma = PlaneDesc {
            log2_sub: (1, 1),
            ..one
        };
        let layout = Arc::new(PixelLayout {
            name: "yuv420p".into(),
            planes: vec![one, chroma, chroma],
            family: ColorFamily::Yuv,
            bit_depth: 8,
            msb_aligned: false,
        });
        let color = color::resolve(
            CLIP_COLOR.into(),
            &ColorContext {
                family: ColorFamily::Yuv,
                chroma_sub: (1, 1),
                pix_fmt: "yuv420p",
                codec: "synthetic",
                width: clip.width,
                height: clip.height,
            },
        );
        let info = VideoStreamInfo {
            index: 0,
            codec: "synthetic".into(),
            size: (clip.width, clip.height),
            time_base: clip.rate.frame_duration().as_rational(),
            origin_pts: 0,
            duration: clip.rate.frame_to_time(frames).ok(),
            timing: Timing::Constant(clip.rate),
            frame_count: Some(clip.frames),
            layout,
            source_pix_fmt: "yuv420p".into(),
            color,
            mapping: SourceMapping {
                projection: Some(Projection::Equirect {
                    top: 0,
                    bottom: 0,
                    left: 0,
                    right: 0,
                }),
                ..SourceMapping::default()
            },
            rotation_deg: None,
        };
        Ok(SyntheticVideo {
            clip,
            path: path.to_path_buf(),
            info,
            path_used: DecodePath::Software { threads: None },
            current: None,
            catch_up: None,
            runs: 0,
            run: 0,
            seq: 0,
            stats: DecodeStats::default(),
        })
    }

    /// Spend one frame's decode time, in slices of a millisecond so that a
    /// stop is seen soon. False when stopped.
    fn decode_one(&mut self, stop: &StopToken) -> bool {
        let started = Instant::now();
        loop {
            if stop.is_stopped() {
                return false;
            }
            let left = self.clip.frame_cost.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(1)));
        }
        self.stats.frames_decoded += 1;
        self.stats.last_decode_ms = started.elapsed().as_secs_f64() * 1000.0;
        let n = self.stats.frames_decoded as f64;
        self.stats.mean_decode_ms += (self.stats.last_decode_ms - self.stats.mean_decode_ms) / n;
        true
    }

    /// Frame `n`: luma 16 + n × 37 mod 220 with `n` in its first 8 bytes,
    /// and grey chroma.
    fn frame(&mut self, n: u64) -> Result<Arc<VideoFrame>, MediaError> {
        let (w, h) = (self.clip.width as usize, self.clip.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut luma = vec![16 + (n.wrapping_mul(37) % 220) as u8; w * h];
        luma[..8].copy_from_slice(&n.to_le_bytes());
        let planes = vec![luma, vec![128; cw * ch], vec![128; cw * ch]];
        let pts = i64::try_from(n).unwrap_or(i64::MAX);
        let time = self
            .clip
            .rate
            .frame_to_time(pts)
            .map_err(|e| MediaError::Decode {
                path: self.path.clone(),
                reason: format!("frame {n}: {e}"),
            })?;
        let mut f = VideoFrame::owned(
            self.info.layout.clone(),
            self.info.size,
            planes,
            vec![w, cw, cw],
            self.info.color,
            (pts, time),
        )
        .map_err(|e| MediaError::Decode {
            path: self.path.clone(),
            reason: e.to_string(),
        })?;
        f.duration = Some(self.clip.rate.frame_duration());
        f.key = n.is_multiple_of(self.clip.keyint);
        f.run = self.run;
        f.seq = self.seq;
        self.seq += 1;
        self.stats.frames_returned += 1;
        let f = Arc::new(f);
        self.current = Some(f.clone());
        Ok(f)
    }

    /// Decode frames `from..=to`, handing out the last; `None` when
    /// stopped, leaving the frame last handed out where it was, and the
    /// run.
    fn decode_to(
        &mut self,
        from: u64,
        to: u64,
        stop: &StopToken,
        cost: &mut SeekCost,
    ) -> Result<Option<Arc<VideoFrame>>, MediaError> {
        for _ in from..=to {
            if !self.decode_one(stop) {
                return Ok(None);
            }
            cost.frames_decoded += 1;
        }
        self.seq += to - from;
        self.frame(to).map(Some)
    }

    /// Start a run at a seek, giving back the one before if it stops.
    fn start_run(&mut self) -> (u64, u64) {
        let before = (self.run, self.seq);
        self.runs += 1;
        (self.run, self.seq) = (self.runs, 0);
        before
    }
}

impl VideoDecode for SyntheticVideo {
    fn info(&self) -> &VideoStreamInfo {
        &self.info
    }

    fn decode_path(&self) -> &DecodePath {
        &self.path_used
    }

    fn seek(
        &mut self,
        at: Time,
        mode: SeekMode,
        stop: &StopToken,
    ) -> Result<SeekOutcome, MediaError> {
        self.catch_up = None;
        let last = self.clip.frames - 1;
        let index = self
            .clip
            .rate
            .time_to_frame(at, Rounding::Floor)
            .map_err(|e| MediaError::Decode {
                path: self.path.clone(),
                reason: format!("seeking to {at:?}: {e}"),
            })?;
        let n = u64::try_from(index.max(0)).unwrap_or(0).min(last);
        let exactness = match mode {
            _ if index < 0 => Exactness::BeforeFirst,
            SeekMode::Keyframe => Exactness::KeyframeOnly,
            SeekMode::Exact if index > last as i64 => Exactness::AfterLast,
            SeekMode::Exact => Exactness::Exact,
        };
        let key = n - n % self.clip.keyint;
        let mut cost = SeekCost::default();
        let here = self
            .current
            .as_ref()
            .and_then(|c| u64::try_from(c.pts).ok());
        let frame = match (mode, here) {
            (SeekMode::Exact, Some(c)) if c == n => self.current.clone(),
            // Forward within the GOP being decoded.
            (SeekMode::Exact, Some(c)) if c < n && key <= c => {
                cost.forward_only = true;
                self.decode_to(c + 1, n, stop, &mut cost)?
            }
            // From the keyframe.
            _ => {
                cost.seeks = 1;
                let before = self.start_run();
                let to = if mode == SeekMode::Keyframe { key } else { n };
                let frame = self.decode_to(key, to, stop, &mut cost)?;
                if frame.is_none() {
                    // Stopped: decoding goes on from the frame last handed
                    // out, in its run.
                    (self.run, self.seq) = before;
                }
                frame
            }
        };
        Ok(match frame {
            Some(frame) => SeekOutcome::Frame {
                frame,
                exactness,
                cost,
            },
            None => SeekOutcome::Stopped,
        })
    }

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        let next = self
            .current
            .as_ref()
            .and_then(|c| u64::try_from(c.pts).ok())
            .map_or(0, |c| c + 1);
        // Catching up passes frames over until one is handed out; stopped,
        // it starts again from the frame after the last one.
        let mut n = next;
        if let Some(until) = self.catch_up {
            while n < until.min(self.clip.frames) {
                if !self.decode_one(stop) {
                    return Ok(NextFrame::Stopped);
                }
                n += 1;
            }
        }
        if n >= self.clip.frames {
            return Ok(NextFrame::End);
        }
        if !self.decode_one(stop) {
            return Ok(NextFrame::Stopped);
        }
        self.catch_up = None;
        self.seq += n - next;
        self.frame(n).map(NextFrame::Frame)
    }

    fn catch_up(&mut self, until: Option<Time>) {
        self.catch_up = until.and_then(|t| {
            let n = self.clip.rate.time_to_frame(t, Rounding::Ceil).ok()?;
            u64::try_from(n.max(0)).ok()
        });
    }

    fn take_events(&mut self) -> Vec<DecoderEvent> {
        Vec::new()
    }

    fn stats(&self) -> DecodeStats {
        self.stats
    }
}

/// Opens or closes a [`GatedBackend`]. Clones share it.
#[derive(Debug, Clone, Default)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    /// A closed gate.
    pub fn new() -> Self {
        Gate::default()
    }

    /// Let everything waiting through, and everything after.
    pub fn release(&self) {
        let (open, cv) = &*self.0;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cv.notify_all();
    }

    pub fn is_open(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait until the gate opens (true) or `stopped` says to stop (false),
    /// looking every 2 ms.
    fn wait(&self, stopped: impl Fn() -> bool) -> bool {
        let (open, cv) = &*self.0;
        let mut g = open.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *g {
                return true;
            }
            if stopped() {
                return false;
            }
            g = cv
                .wait_timeout(g, Duration::from_millis(2))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// `inner`, with every video open and decode held until `gate` is
/// released: an open waits for the gate or the request's shutdown flag
/// (and fails on the flag), a seek or `next_frame` for the gate or its
/// stop token. Probing and audio pass straight through.
#[derive(Debug, Clone)]
pub struct GatedBackend<B> {
    pub inner: B,
    pub gate: Gate,
}

impl<B: MediaBackend> GatedBackend<B> {
    /// `inner` behind a closed gate.
    pub fn new(inner: B) -> Self {
        GatedBackend {
            inner,
            gate: Gate::new(),
        }
    }
}

impl<B: MediaBackend> MediaBackend for GatedBackend<B> {
    fn name(&self) -> &'static str {
        "gated"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.inner.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let shutdown: &AtomicBool = &req.interrupt;
        if !self.gate.wait(|| shutdown.load(Ordering::Relaxed)) {
            return Err(MediaError::Decode {
                path: path.to_path_buf(),
                reason: "shut down while the gate was closed".into(),
            });
        }
        Ok(Box::new(GatedVideo {
            inner: self.inner.open_video(path, req)?,
            gate: self.gate.clone(),
        }))
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }
}

#[derive(Debug)]
struct GatedVideo {
    inner: Box<dyn VideoDecode>,
    gate: Gate,
}

impl VideoDecode for GatedVideo {
    fn info(&self) -> &VideoStreamInfo {
        self.inner.info()
    }

    fn decode_path(&self) -> &DecodePath {
        self.inner.decode_path()
    }

    fn seek(
        &mut self,
        at: Time,
        mode: SeekMode,
        stop: &StopToken,
    ) -> Result<SeekOutcome, MediaError> {
        if !self.gate.wait(|| stop.is_stopped()) {
            return Ok(SeekOutcome::Stopped);
        }
        self.inner.seek(at, mode, stop)
    }

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        if !self.gate.wait(|| stop.is_stopped()) {
            return Ok(NextFrame::Stopped);
        }
        self.inner.next_frame(stop)
    }

    fn catch_up(&mut self, until: Option<Time>) {
        self.inner.catch_up(until);
    }

    fn take_events(&mut self) -> Vec<DecoderEvent> {
        self.inner.take_events()
    }

    fn stats(&self) -> DecodeStats {
        self.inner.stats()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use spherecut_core::StopSource;

    use super::*;

    fn seek(d: &mut dyn VideoDecode, frame: i64, mode: SeekMode) -> (u64, Exactness, SeekCost) {
        let t = FrameRate::FPS_30.frame_to_time(frame).unwrap();
        match d.seek(t, mode, &StopToken::never()).unwrap() {
            SeekOutcome::Frame {
                frame,
                exactness,
                cost,
            } => (frame_index(&frame).unwrap(), exactness, cost),
            SeekOutcome::Stopped => panic!("stopped"),
        }
    }

    fn next(d: &mut dyn VideoDecode) -> Option<u64> {
        match d.next_frame(&StopToken::never()).unwrap() {
            NextFrame::Frame(f) => Some(frame_index(&f).unwrap()),
            NextFrame::End => None,
            NextFrame::Stopped => panic!("stopped"),
        }
    }

    #[test]
    fn synthetic_frames_seek_exactly_and_say_which_they_are() {
        let mut d = SyntheticBackend::default()
            .open_video(Path::new("synthetic"), &VideoRequest::default())
            .unwrap();
        assert_eq!(d.info().frame_count, Some(30));
        assert_eq!(d.info().timing, Timing::Constant(FrameRate::FPS_30));
        assert_eq!(next(&mut *d), Some(0));
        assert_eq!(next(&mut *d), Some(1));
        let (n, e, cost) = seek(&mut *d, 13, SeekMode::Exact);
        assert_eq!((n, e), (13, Exactness::Exact));
        // From the keyframe at 8.
        assert_eq!((cost.seeks, cost.frames_decoded), (1, 6));
        // Forward in the same GOP.
        let (n, _, cost) = seek(&mut *d, 15, SeekMode::Exact);
        assert_eq!(n, 15);
        assert!(cost.forward_only && cost.frames_decoded == 2);
        assert_eq!(next(&mut *d), Some(16));
        assert_eq!(seek(&mut *d, 20, SeekMode::Keyframe).0, 16);
        assert_eq!(seek(&mut *d, -3, SeekMode::Exact).1, Exactness::BeforeFirst);
        let (n, e, _) = seek(&mut *d, 31, SeekMode::Exact);
        assert_eq!((n, e), (29, Exactness::AfterLast));
        assert_eq!(next(&mut *d), None);
        // Catching up skips to the first frame at or after the time.
        seek(&mut *d, 0, SeekMode::Exact);
        d.catch_up(Some(FrameRate::FPS_30.frame_to_time(10).unwrap()));
        assert_eq!(next(&mut *d), Some(10));
        assert_eq!(next(&mut *d), Some(11));
    }

    /// Frames of one run whose seq follow on are frames that follow on, as
    /// `FfmpegVideo`'s are: a forward seek and catching up count each frame
    /// they pass over, and a stopped seek leaves the run as it was.
    #[test]
    fn synthetic_runs_link_only_frames_that_follow_on() {
        let mut d = SyntheticBackend::default()
            .open_video(Path::new("runs"), &VideoRequest::default())
            .unwrap();
        let d = &mut *d;
        let never = StopToken::never();
        let source = StopSource::new();
        let stopped = source.current();
        source.next();
        let at = |n: i64| FrameRate::FPS_30.frame_to_time(n).unwrap();
        let seek_to = |d: &mut dyn VideoDecode, n: i64, mode: SeekMode| {
            let got = d.seek(at(n), mode, &never).unwrap();
            let SeekOutcome::Frame { frame, .. } = got else {
                panic!("stopped");
            };
            frame
        };
        let next = |d: &mut dyn VideoDecode| match d.next_frame(&never).unwrap() {
            NextFrame::Frame(f) => f,
            got => panic!("{got:?}"),
        };
        // Frame by (run, seq), checked against the frames next to it.
        let mut runs = std::collections::HashMap::new();
        let mut note = |f: &VideoFrame| {
            let n = frame_index(f).unwrap();
            let key = (f.run, f.seq);
            if let Some(&m) = runs.get(&key) {
                assert_eq!(m, n, "run {key:?} was frame {m}");
            }
            if let Some(&m) = f.seq.checked_sub(1).and_then(|s| runs.get(&(f.run, s))) {
                assert_eq!(m + 1, n, "run {key:?} follows frame {m}");
            }
            if let Some(&m) = runs.get(&(f.run, f.seq + 1)) {
                assert_eq!(n + 1, m, "run {key:?} comes before frame {m}");
            }
            runs.insert(key, n);
            n
        };

        // Frame 9, forward in its GOP to 13, then catching up to 20.
        assert_eq!(note(&seek_to(d, 9, SeekMode::Exact)), 9);
        assert_eq!(note(&seek_to(d, 13, SeekMode::Exact)), 13);
        d.catch_up(Some(at(20)));
        assert_eq!(note(&next(d)), 20);
        let f21 = next(d);
        assert_eq!(note(&f21), 21);
        // A seek stopped before it began: the next frame goes on in the run.
        assert!(matches!(
            d.seek(at(3), SeekMode::Exact, &stopped).unwrap(),
            SeekOutcome::Stopped
        ));
        let f22 = next(d);
        assert_eq!(note(&f22), 22);
        assert_eq!((f22.run, f22.seq), (f21.run, f21.seq + 1));
        // Catching up stopped, then called off: the frame after 22.
        d.catch_up(Some(at(28)));
        assert!(matches!(
            d.next_frame(&stopped).unwrap(),
            NextFrame::Stopped
        ));
        d.catch_up(None);
        assert_eq!(note(&next(d)), 23);
        // A keyframe, then forward from it.
        assert_eq!(note(&seek_to(d, 27, SeekMode::Keyframe)), 24);
        assert_eq!(note(&seek_to(d, 26, SeekMode::Exact)), 26);
        assert_eq!(note(&next(d)), 27);
        // Back, and forward again over a catch-up to the end.
        assert_eq!(note(&seek_to(d, 2, SeekMode::Exact)), 2);
        assert_eq!(note(&seek_to(d, 5, SeekMode::Exact)), 5);
        d.catch_up(Some(at(29)));
        assert_eq!(note(&next(d)), 29);
        assert!(matches!(d.next_frame(&never).unwrap(), NextFrame::End));
    }

    #[test]
    fn a_slow_synthetic_seek_stops_between_frames() {
        let backend = SyntheticBackend::new(SyntheticClip {
            frame_cost: Duration::from_millis(50),
            ..SyntheticClip::default()
        });
        let mut d = backend
            .open_video(Path::new("slow"), &VideoRequest::default())
            .unwrap();
        let source = StopSource::new();
        let token = source.next();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            source.next();
        });
        let started = Instant::now();
        // Frames 0 to 7: 400 ms unstopped.
        let t = FrameRate::FPS_30.frame_to_time(7).unwrap();
        assert!(matches!(
            d.seek(t, SeekMode::Exact, &token).unwrap(),
            SeekOutcome::Stopped
        ));
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "{:?}",
            started.elapsed()
        );
        stopper.join().unwrap();
    }

    #[test]
    fn a_gate_holds_opens_and_decodes_until_it_is_released() {
        let gated = Arc::new(GatedBackend::new(SyntheticBackend::default()));
        // A shutdown while the gate is closed fails the open.
        let req = VideoRequest::default();
        req.interrupt.store(true, Ordering::Relaxed);
        assert!(gated.open_video(Path::new("a"), &req).is_err());

        let (tx, rx) = std::sync::mpsc::channel();
        let g = gated.clone();
        let opener = std::thread::spawn(move || {
            let d = g.open_video(Path::new("b"), &VideoRequest::default());
            tx.send(d.is_ok()).unwrap();
            d
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "opened while closed"
        );
        gated.gate.release();
        assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap());
        let mut d = opener.join().unwrap().unwrap();
        assert_eq!(next(&mut *d), Some(0));

        // A decode behind a closed gate waits, and stops with its token.
        let gate = Gate::new();
        let mut d = GatedVideo {
            inner: SyntheticBackend::default()
                .open_video(Path::new("c"), &VideoRequest::default())
                .unwrap(),
            gate: gate.clone(),
        };
        let source = StopSource::new();
        let token = source.next();
        let started = Instant::now();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            source.next();
        });
        assert!(matches!(d.next_frame(&token).unwrap(), NextFrame::Stopped));
        assert!(started.elapsed() >= Duration::from_millis(20));
        stopper.join().unwrap();
        gate.release();
        assert_eq!(next(&mut d), Some(0));
    }
}
