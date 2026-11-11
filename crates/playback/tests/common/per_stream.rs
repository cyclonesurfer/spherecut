//! `PerStream`, the frame lock's test backend (M3 plan section 9): a
//! [`SyntheticBackend`] a stream, so that a camera's two lenses can
//! differ in frame count, keyframe interval, size, rate, decode cost and
//! timing tables, and fail apart.
//!
//! `SyntheticBackend` opens its one clip whatever stream is asked for,
//! as stream 0. `PerStream` opens the [`Lens`] that `req.stream` names
//! (`None` is stream 0), its info carrying that index, and answers
//! [`MediaBackend::track_timing`] with that lens's tables. The clip's
//! frame cost is charged for every frame decoded, handed out or passed
//! over, as at 8K, where every frame is a reference frame; a lens's
//! jitter adds to it, from a seeded stream, so that a run can be
//! replayed. A lens behind a [`Hold`] hands out no frame past the one it
//! lets go, so that it lags the other by as much as a test says, whatever
//! the machine's load.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use spherecut_core::{FrameRate, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioRequest, DecodePath, DecodeStats, DecoderEvent, MediaBackend, NextFrame,
    SeekMode, SeekOutcome, VideoDecode, VideoFrame, VideoRequest, VideoStreamInfo,
};
use spherecut_media::mp4::TrackTiming;
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_synth::fake::{Gate, SyntheticBackend, SyntheticClip};

/// One stream of a [`PerStream`]: its clip, what it charges beyond the
/// clip's frame cost, how it fails, and the tables it answers. Built
/// from [`Lens::new`] or the default clip with the methods below.
#[derive(Debug, Clone)]
pub struct Lens {
    /// The frames the lens decodes, at `frame_cost` each.
    pub clip: SyntheticClip,
    /// The frame cost of the lens's opens by their order, from 0, where
    /// it isn't the clip's: a decoder reopened slower or faster.
    pub cost_at_open: Vec<(usize, Duration)>,
    /// A seed, and the most a frame decoded costs beyond the clip's
    /// frame cost: each frame adds a duration up to it, in
    /// microseconds, drawn from the seed and the open's order.
    pub jitter: Option<(u64, Duration)>,
    /// A read error from this frame on: every frame at or after it fails
    /// to come out, as from a file cut off there, while a seek before it
    /// works.
    pub read_error_at: Option<u64>,
    /// Every open fails.
    pub open_fails: bool,
    /// Opens wait for this gate, or the request's shutdown flag, on
    /// which they fail.
    pub held_by: Option<Gate>,
    /// The lens hands out no frame past the one this lets go.
    pub hold: Option<Hold>,
    /// What [`MediaBackend::track_timing`] answers.
    pub tables: Tables,
}

/// How far a [`Lens`] behind it may go: the last frame it hands out, by
/// pts (a frame's index). A frame past it waits, decoded, until the hold
/// lets it go or the decoder is told to stop; stopped, it is kept for the
/// next `next_frame`, so that none is lost, and a seek drops it. Clones
/// share it.
#[derive(Debug, Clone)]
pub struct Hold(Arc<AtomicI64>);

impl Hold {
    /// A hold at frame `frame`.
    pub fn at(frame: i64) -> Self {
        Hold(Arc::new(AtomicI64::new(frame)))
    }

    /// Let the lens hand out its frames up to `frame`, and hold those
    /// after it.
    pub fn release(&self, frame: i64) {
        self.0.store(frame, Ordering::Release);
    }

    /// Whether the lens may hand out the frame at pts `pts`.
    pub fn lets(&self, pts: i64) -> bool {
        pts <= self.0.load(Ordering::Acquire)
    }
}

/// The timing tables a [`Lens`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tables {
    /// Those of an MP4 track holding the clip, as [`SyntheticBackend`]
    /// derives them.
    Derived,
    /// These, whatever the clip: the decoder still starts at pts 0 (its
    /// origin) and decodes the clip's frames, so a fault planted here is
    /// in the tables only.
    Planted(TrackTiming),
    /// `Ok(None)`: a track with no tables.
    None,
}

impl Default for Lens {
    /// The default [`SyntheticClip`], with nothing added.
    fn default() -> Self {
        Lens::new(SyntheticClip::default())
    }
}

impl Lens {
    /// `clip`, charging its frame cost only, failing nowhere, with its
    /// derived tables.
    pub fn new(clip: SyntheticClip) -> Self {
        Lens {
            clip,
            cost_at_open: Vec::new(),
            jitter: None,
            read_error_at: None,
            open_fails: false,
            held_by: None,
            hold: None,
            tables: Tables::Derived,
        }
    }

    pub fn frames(mut self, frames: u64) -> Self {
        self.clip.frames = frames;
        self
    }

    pub fn keyint(mut self, keyint: u64) -> Self {
        self.clip.keyint = keyint;
        self
    }

    pub fn rate(mut self, rate: FrameRate) -> Self {
        self.clip.rate = rate;
        self
    }

    pub fn size(mut self, width: u32, height: u32) -> Self {
        (self.clip.width, self.clip.height) = (width, height);
        self
    }

    /// Each frame decoded takes `cost`.
    pub fn cost(mut self, cost: Duration) -> Self {
        self.clip.frame_cost = cost;
        self
    }

    /// The lens's `open`th open (from 0) decodes at `cost` a frame.
    pub fn cost_at_open(mut self, open: usize, cost: Duration) -> Self {
        self.cost_at_open.push((open, cost));
        self
    }

    /// Each frame decoded takes up to `max` more, drawn from `seed`.
    pub fn jitter(mut self, seed: u64, max: Duration) -> Self {
        self.jitter = Some((seed, max));
        self
    }

    /// Frame `frame` and every one after it fail to come out.
    pub fn read_error_at(mut self, frame: u64) -> Self {
        self.read_error_at = Some(frame);
        self
    }

    /// Every open fails.
    pub fn open_fails(mut self) -> Self {
        self.open_fails = true;
        self
    }

    /// Opens wait until `gate` is released.
    pub fn held_by(mut self, gate: &Gate) -> Self {
        self.held_by = Some(gate.clone());
        self
    }

    /// The lens hands out no frame past the one `hold` lets go.
    pub fn hold(mut self, hold: &Hold) -> Self {
        self.hold = Some(hold.clone());
        self
    }

    /// The tables derived from the clip as it is now, changed by
    /// `plant`: set the clip before planting.
    pub fn plant(mut self, plant: impl FnOnce(&mut TrackTiming)) -> Self {
        let mut tables = self.derived_tables();
        plant(&mut tables);
        self.tables = Tables::Planted(tables);
        self
    }

    /// No tables: `track_timing` answers `Ok(None)`.
    pub fn no_tables(mut self) -> Self {
        self.tables = Tables::None;
        self
    }

    /// The tables [`SyntheticBackend`] derives from the clip.
    pub fn derived_tables(&self) -> TrackTiming {
        SyntheticBackend::new(self.clip.clone())
            .track_timing(Path::new("lens"), 0)
            .expect("the lens's clip has frames and a keyframe interval")
            .expect("a synthetic clip has tables")
    }
}

/// Opens each stream as its own [`Lens`], and counts what it is asked.
/// Clones share the counts, so a test keeps one and hands the player
/// another.
#[derive(Debug, Clone)]
pub struct PerStream {
    lenses: Arc<[Lens]>,
    counts: Arc<Counts>,
}

/// A count a stream, of every call, failed ones included.
#[derive(Debug)]
struct Counts {
    opens: Vec<AtomicUsize>,
    timing: Vec<AtomicUsize>,
}

impl PerStream {
    /// A stream a lens, numbered from 0.
    pub fn new(lenses: Vec<Lens>) -> Self {
        let counts = Counts {
            opens: lenses.iter().map(|_| AtomicUsize::new(0)).collect(),
            timing: lenses.iter().map(|_| AtomicUsize::new(0)).collect(),
        };
        PerStream {
            lenses: lenses.into(),
            counts: Arc::new(counts),
        }
    }

    /// A camera's two lenses, as streams 0 and 1.
    pub fn pair(lens0: Lens, lens1: Lens) -> Self {
        PerStream::new(vec![lens0, lens1])
    }

    pub fn lens(&self, stream: usize) -> &Lens {
        &self.lenses[stream]
    }

    /// How often `stream` has been opened.
    pub fn opens(&self, stream: usize) -> usize {
        self.counts.opens[stream].load(Ordering::Acquire)
    }

    /// How often `stream`'s tables have been asked for.
    pub fn timing_calls(&self, stream: usize) -> usize {
        self.counts.timing[stream].load(Ordering::Acquire)
    }

    fn get(&self, path: &Path, stream: usize) -> Result<&Lens, MediaError> {
        self.lenses.get(stream).ok_or_else(|| MediaError::NoStream {
            path: path.to_path_buf(),
            kind: "video",
        })
    }
}

impl MediaBackend for PerStream {
    fn name(&self) -> &'static str {
        "per-stream"
    }

    /// Always an error, as `SyntheticBackend`'s: a lens has no file.
    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        SyntheticBackend::default().probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let stream = req.stream.unwrap_or(0);
        let lens = self.get(path, stream)?;
        let open = self.counts.opens[stream].fetch_add(1, Ordering::AcqRel);
        let planted = |what: &str| MediaError::Decode {
            path: path.to_path_buf(),
            reason: format!("stream {stream}: {what}"),
        };
        if let Some(gate) = &lens.held_by {
            spherecut_core::may_block("an open held by its lens's gate");
            while !gate.is_open() {
                if req.interrupt.load(Ordering::Relaxed) {
                    return Err(planted("shut down while the open was held"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if lens.open_fails {
            return Err(planted("a planted open failure"));
        }
        let mut clip = lens.clip.clone();
        if let Some(&(_, cost)) = lens.cost_at_open.iter().find(|(k, _)| *k == open) {
            clip.frame_cost = cost;
        }
        let inner = SyntheticBackend::new(clip).open_video(path, req)?;
        let mut info = inner.info().clone();
        info.index = stream;
        Ok(Box::new(LensVideo {
            inner,
            info,
            path: path.to_path_buf(),
            jitter: lens.jitter.map(|(seed, max)| Jitter {
                state: seed ^ (open as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15),
                max_us: max.as_micros() as u64,
            }),
            read_error_at: lens.read_error_at,
            decoded: 0,
            hold: lens.hold.clone(),
            pending: None,
        }))
    }

    /// No lens has audio.
    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        SyntheticBackend::default().open_audio(path, req)
    }

    fn track_timing(&self, path: &Path, stream: usize) -> Result<Option<TrackTiming>, MediaError> {
        let lens = self.get(path, stream)?;
        self.counts.timing[stream].fetch_add(1, Ordering::AcqRel);
        match &lens.tables {
            Tables::Derived => SyntheticBackend::new(lens.clip.clone()).track_timing(path, 0),
            Tables::Planted(tables) => Ok(Some(tables.clone())),
            Tables::None => Ok(None),
        }
    }
}

/// A seeded stream of a frame's extra cost (SplitMix64).
#[derive(Debug)]
struct Jitter {
    state: u64,
    max_us: u64,
}

impl Jitter {
    fn next(&mut self) -> Duration {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Duration::from_micros(z % (self.max_us + 1))
    }
}

/// A lens being decoded: the clip's decoder, its info carrying the
/// stream's index, charging the lens's jitter, failing at its read error
/// and waiting at its hold.
#[derive(Debug)]
struct LensVideo {
    inner: Box<dyn VideoDecode>,
    info: VideoStreamInfo,
    path: PathBuf,
    jitter: Option<Jitter>,
    read_error_at: Option<u64>,
    /// The inner decoder's frames decoded, as last charged for.
    decoded: u64,
    hold: Option<Hold>,
    /// A frame past the hold when a stop came, for the next `next_frame`.
    pending: Option<Arc<VideoFrame>>,
}

impl LensVideo {
    /// Charge the jitter of every frame decoded since the last charge,
    /// cut short by a stop: the frames are decoded, so what came of them
    /// is kept.
    fn charge(&mut self, stop: &StopToken) {
        let decoded = self.inner.stats().frames_decoded;
        let new = decoded.saturating_sub(self.decoded);
        self.decoded = decoded;
        let Some(jitter) = &mut self.jitter else {
            return;
        };
        let extra: Duration = (0..new).map(|_| jitter.next()).sum();
        let started = Instant::now();
        while !stop.is_stopped() {
            let left = extra.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(1)));
        }
    }

    /// Wait until the hold lets the frame at `pts` go (true), or `stop`
    /// says to stop (false), looking every millisecond.
    fn wait(&self, pts: i64, stop: &StopToken) -> bool {
        let Some(hold) = &self.hold else {
            return true;
        };
        if hold.lets(pts) {
            return true;
        }
        spherecut_core::may_block("a lens held back by its test");
        while !hold.lets(pts) {
            if stop.is_stopped() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// The read error, if frame pts `pts` is at or after it.
    fn read(&self, pts: i64) -> Result<(), MediaError> {
        match self.read_error_at {
            Some(n) if u64::try_from(pts).is_ok_and(|p| p >= n) => Err(MediaError::Decode {
                path: self.path.clone(),
                reason: format!(
                    "stream {}: a planted read error at frame {n}",
                    self.info.index
                ),
            }),
            _ => Ok(()),
        }
    }
}

impl VideoDecode for LensVideo {
    fn info(&self) -> &VideoStreamInfo {
        &self.info
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
        self.pending = None;
        let got = self.inner.seek(at, mode, stop);
        self.charge(stop);
        let got = got?;
        if let SeekOutcome::Frame { frame, .. } = &got {
            self.read(frame.pts)?;
            if !self.wait(frame.pts, stop) {
                return Ok(SeekOutcome::Stopped);
            }
        }
        Ok(got)
    }

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        let frame = match self.pending.take() {
            Some(frame) => frame,
            None => {
                let got = self.inner.next_frame(stop);
                self.charge(stop);
                match got? {
                    NextFrame::Frame(frame) => {
                        self.read(frame.pts)?;
                        frame
                    }
                    other => return Ok(other),
                }
            }
        };
        if !self.wait(frame.pts, stop) {
            self.pending = Some(frame);
            return Ok(NextFrame::Stopped);
        }
        Ok(NextFrame::Frame(frame))
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
