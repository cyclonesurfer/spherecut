//! [`Held`], the backend of this suite's tests that step the decoders
//! frame by frame: a [`PerStream`] whose lenses hand each frame they
//! decode on out only for a permit, so that a test knows which frame
//! each lens holds and when it goes in, while the frames a catch-up
//! passes over cost none, as an 8K decoder passes frames over for less
//! than it decodes them; and whose exact seeks into a lens's planted gap
//! land on the frame after it, as on damage a decoder can't land in;
//! and whose next seek may be made to fail once. Seeks hand their frame
//! out without a permit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use spherecut_core::{FrameRate, ManualClock, Rounding, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioRequest, DecodePath, DecodeStats, DecoderEvent, MediaBackend, NextFrame,
    SeekMode, SeekOutcome, VideoDecode, VideoRequest, VideoStreamInfo,
};
use spherecut_media::mp4::TrackTiming;
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_playback::{CacheBudget, PlayerConfig};

use crate::common::per_stream::PerStream;

pub const RATE: FrameRate = FrameRate::FPS_30;

/// What `PerStream` opens: any name will do.
pub const LENSES: &str = "lenses.synthetic";

pub fn at(k: i64) -> Time {
    RATE.frame_to_time(k).unwrap()
}

/// The frame shown at clip time `t`.
pub fn frame_of(t: Time) -> i64 {
    RATE.time_to_frame(t, Rounding::Floor).unwrap()
}

/// A paused `ManualClock`.
pub fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(RATE.frame_duration()))
}

/// The config of a player holding 64 MiB of frames, on six GPU slots,
/// as a locked pair takes whatever the config says.
pub fn config() -> PlayerConfig {
    PlayerConfig {
        cache: CacheBudget::Bytes(64 << 20),
        gpu_slots: 6,
        ..PlayerConfig::default()
    }
}

/// [`PerStream`]'s first two lenses, handing out frames for permits and
/// passing over planted gaps. Clones share the permits.
#[derive(Debug, Clone)]
pub struct Held {
    inner: PerStream,
    /// Frames each lens may still hand out: `u64::MAX` for any.
    left: Arc<[AtomicU64; 2]>,
    /// The frame each lens holds for a permit, and the last it handed out
    /// on, -1 for none.
    holding: Arc<[AtomicI64; 2]>,
    released: Arc<[AtomicI64; 2]>,
    /// Each lens's gap, `[from, to)` by frame: an exact seek to a frame in
    /// it lands on frame `to`.
    gaps: [Option<(i64, i64)>; 2],
    /// Whether each lens's next seek fails.
    fail_seek: Arc<[AtomicBool; 2]>,
}

impl Held {
    /// `inner`, its lenses let hand out `left` frames to begin with.
    pub fn new(inner: PerStream, left: [u64; 2]) -> Self {
        Held {
            inner,
            left: Arc::new(left.map(AtomicU64::new)),
            holding: Arc::new([AtomicI64::new(-1), AtomicI64::new(-1)]),
            released: Arc::new([AtomicI64::new(-1), AtomicI64::new(-1)]),
            gaps: [None; 2],
            fail_seek: Arc::new([AtomicBool::new(false), AtomicBool::new(false)]),
        }
    }

    /// Every frame let out, with each lens's gap.
    pub fn with_gaps(inner: PerStream, gaps: [Option<(i64, i64)>; 2]) -> Self {
        Held {
            gaps,
            ..Held::new(inner, [u64::MAX; 2])
        }
    }

    /// Let lens `stream` hand out `n` more frames.
    pub fn grant(&self, stream: usize, n: u64) {
        let left = &self.left[stream];
        let _ = left.try_update(Ordering::AcqRel, Ordering::Acquire, |l| {
            Some(l.saturating_add(n))
        });
    }

    /// Let lens `stream` hand out every frame from now on.
    pub fn free(&self, stream: usize) {
        self.left[stream].store(u64::MAX, Ordering::Release);
    }

    /// Make lens `stream`'s next seek fail, as on read damage.
    pub fn fail_next_seek(&self, stream: usize) {
        self.fail_seek[stream].store(true, Ordering::Release);
    }

    /// The frame lens `stream` holds for a permit.
    pub fn holding(&self, stream: usize) -> Option<i64> {
        Some(self.holding[stream].load(Ordering::Acquire)).filter(|&f| f >= 0)
    }

    /// The last frame lens `stream` handed out on decoding.
    pub fn released(&self, stream: usize) -> Option<i64> {
        Some(self.released[stream].load(Ordering::Acquire)).filter(|&f| f >= 0)
    }

    /// Let lens `stream` hand out its frame held now, and wait until it
    /// holds the next, failing after `wait`: the frame it handed out.
    pub fn step(&self, stream: usize, wait: Duration) -> i64 {
        let start = Instant::now();
        let was = loop {
            if let Some(h) = self.holding(stream) {
                break h;
            }
            assert!(start.elapsed() < wait, "lens {stream} never held a frame");
            std::thread::sleep(Duration::from_millis(1));
        };
        self.grant(stream, 1);
        while self.holding(stream).is_none_or(|h| h == was) {
            assert!(
                start.elapsed() < wait,
                "lens {stream} never went on from frame {was}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        was
    }
}

impl MediaBackend for Held {
    fn name(&self) -> &'static str {
        "held"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.inner.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let inner = self.inner.open_video(path, req)?;
        Ok(Box::new(HeldVideo {
            stream: inner.info().index,
            inner,
            held: self.clone(),
        }))
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }

    fn track_timing(&self, path: &Path, stream: usize) -> Result<Option<TrackTiming>, MediaError> {
        self.inner.track_timing(path, stream)
    }
}

/// A lens's decoder under [`Held`].
#[derive(Debug)]
struct HeldVideo {
    stream: usize,
    inner: Box<dyn VideoDecode>,
    held: Held,
}

impl VideoDecode for HeldVideo {
    fn info(&self) -> &VideoStreamInfo {
        self.inner.info()
    }

    fn decode_path(&self) -> &DecodePath {
        self.inner.decode_path()
    }

    /// An exact seek into the lens's gap lands on the frame after it; a
    /// seek made to fail fails.
    fn seek(
        &mut self,
        at: Time,
        mode: SeekMode,
        stop: &StopToken,
    ) -> Result<SeekOutcome, MediaError> {
        if self.held.fail_seek[self.stream].swap(false, Ordering::AcqRel) {
            return Err(MediaError::Decode {
                path: PathBuf::from(LENSES),
                reason: format!("stream {}: a planted seek failure", self.stream),
            });
        }
        let gap = self.held.gaps.get(self.stream).copied().flatten();
        let at = match gap {
            Some((from, to)) if mode == SeekMode::Exact && (from..to).contains(&frame_of(at)) => {
                self::at(to)
            }
            _ => at,
        };
        self.inner.seek(at, mode, stop)
    }

    /// The next frame, once a permit lets it out; stopped meanwhile, the
    /// frame is lost, as at shutdown.
    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        let got = self.inner.next_frame(stop)?;
        if let NextFrame::Frame(f) = &got {
            spherecut_core::may_block("a frame held for its permit");
            let i = self.stream;
            let (left, holding) = (&self.held.left[i], &self.held.holding[i]);
            holding.store(f.pts, Ordering::Release);
            loop {
                let n = left.load(Ordering::Acquire);
                if n == u64::MAX
                    || (n > 0
                        && left
                            .compare_exchange(n, n - 1, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok())
                {
                    break;
                }
                if stop.is_stopped() {
                    holding.store(-1, Ordering::Release);
                    return Ok(NextFrame::Stopped);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            self.held.released[i].store(f.pts, Ordering::Release);
            holding.store(-1, Ordering::Release);
        }
        Ok(got)
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
