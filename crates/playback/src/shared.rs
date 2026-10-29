//! What the player's threads share: the frame cache, the GPU slots, the
//! numbers, and the status the workers report, which the UI thread reads
//! with `try_lock` only.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::JoinHandle;

use spherecut_core::{Rational, Rounding, StopSource, StopToken, Time, may_block};
use spherecut_media::backend::{
    DecodePath, Exactness, FallbackReason, HwKind, SeekMode, VideoFrame, VideoStreamInfo,
};

use crate::audio::AudioOutput;
use crate::budget::{AutoBudget, CacheBudget};
use crate::cache::FrameCache;
use crate::decoder::HwSupport;
use crate::perf::PerfStats;
use crate::player::{PlayerError, PlayerEvent};
use crate::slots::SlotRing;

/// The GUI's repaint request: called when a frame is ready to show or
/// the snapshot changed.
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

/// What a decode worker is asked to do: one request at a time, the latest
/// winning.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    /// The presenter's id for it, which the worker's landing names.
    pub(crate) id: u64,
    pub(crate) want: Want,
    /// Stops when a newer request is made, or the file is closed.
    pub(crate) token: StopToken,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Want {
    /// The frame shown at `at`.
    Seek { at: Time, mode: SeekMode },
    /// `frames` frames on (back, when negative) from the stream's frame
    /// at `from` (`Exact`), or from where a keyframe seek to it lands
    /// (`Keyframe`: the step came while that seek was on its way).
    Step {
        frames: i32,
        from: Time,
        mode: SeekMode,
    },
}

#[derive(Debug)]
pub(crate) enum Command {
    Request(Request),
    /// The file is closed: the worker ends.
    Close,
}

/// The frame a worker found for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Landed {
    /// The request's id.
    pub id: u64,
    pub time: Time,
    pub pts: i64,
    /// What the seek said, when it decoded; `None` from the cache or a
    /// step.
    pub exactness: Option<Exactness>,
}

/// How a stream's requests went, for the snapshot and the seek storm.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekCounts {
    /// Requests the worker took on.
    pub started: u64,
    /// Requests a newer one replaced before the worker took them on, or
    /// stopped while it worked on them.
    pub superseded: u64,
    /// Seeks the decoder ran to the end.
    pub decoded: u64,
    /// Requests answered from the frame cache.
    pub from_cache: u64,
}

/// One stream's status, as its worker reports it.
#[derive(Debug, Clone, Default)]
pub struct StreamStatus {
    /// The stream asked for, by its index in the file; `None` for the best.
    pub asked: Option<usize>,
    pub opened: bool,
    pub info: Option<VideoStreamInfo>,
    /// The decoder chosen, and why.
    pub choice: Option<String>,
    /// Where it decodes now.
    pub decode: Option<DecodePath>,
    /// Why hardware decoding went on in software, once it did.
    pub fallback: Option<(HwKind, FallbackReason)>,
    /// Why it can't open or go on.
    pub error: Option<PlayerError>,
    /// The request the error stopped, if one was being worked on.
    pub failed_request: Option<u64>,
    /// The clip time its last frame ends at, once decoding reached it.
    pub end: Option<Time>,
    pub landed: Option<Landed>,
    pub seeks: SeekCounts,
}

/// The budget in force, and what it was worked out from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetStatus {
    pub configured: CacheBudget,
    pub bytes: u64,
    /// The last reading of the memory available, every 5 s.
    pub auto: Option<AutoBudget>,
}

/// What the workers and the upload thread report.
#[derive(Debug, Default)]
pub(crate) struct Status {
    /// The open the rest is about: [`Shared::live`].
    pub(crate) epoch: u64,
    pub(crate) path: Option<PathBuf>,
    pub(crate) streams: Vec<StreamStatus>,
    /// The open file's audio, when it has some (slice 6), whose clock
    /// video follows.
    pub(crate) audio: Option<Arc<dyn AudioOutput>>,
    pub(crate) budget: Option<BudgetStatus>,
    pub(crate) gpu_error: Option<PlayerError>,
}

/// Shared by every thread of a player.
pub(crate) struct Shared {
    pub(crate) cache: Arc<FrameCache>,
    pub(crate) slots: Arc<SlotRing>,
    pub(crate) perf: Arc<PerfStats>,
    pub(crate) status: Mutex<Status>,
    /// The generations of requests, and shutdown.
    pub(crate) stop: StopSource,
    /// The generations of opens: what an open worker waits on (the
    /// audio's open) stops with a newer open, never with a seek.
    pub(crate) opens: StopSource,
    /// The latest open, bumped by the UI.
    pub(crate) epoch: AtomicU64,
    /// The open whose frames the cache holds and whose status
    /// [`Status`] has: set by the open worker once it has cleared what
    /// the open before left, so that the upload thread never takes an
    /// old file's frame for the new one's.
    pub(crate) live: AtomicU64,
    /// The request the presenter waits for (0 for none), and whether
    /// playback is wanted: the upload thread's guide to what to upload.
    pub(crate) target: AtomicU64,
    pub(crate) playing: AtomicBool,
    pub(crate) shutdown: AtomicBool,
    /// Held by an open worker while it clears the cache and starts the
    /// decode workers, so that two opens never interleave.
    pub(crate) open_lock: Mutex<()>,
    /// Doctor's checks, once given or run.
    pub(crate) hw: Mutex<Option<HwSupport>>,
    pub(crate) events: Sender<PlayerEvent>,
    pub(crate) handles: Sender<(String, JoinHandle<()>)>,
    pub(crate) repaint: Option<Repaint>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("epoch", &self.epoch)
            .field("live", &self.live)
            .field("target", &self.target)
            .field("shutdown", &self.shutdown)
            .finish_non_exhaustive()
    }
}

/// `m`, locked; never on the UI thread.
#[track_caller]
pub(crate) fn lock<'a, T>(m: &'a Mutex<T>, what: &str) -> MutexGuard<'a, T> {
    may_block(what);
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `m`, if its lock is free. Never waits.
pub(crate) fn try_lock<T>(m: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match m.try_lock() {
        Ok(g) => Some(g),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl Shared {
    /// The status, locked. Never on the UI thread.
    #[track_caller]
    pub(crate) fn status(&self) -> MutexGuard<'_, Status> {
        lock(&self.status, "taking the player's status lock")
    }

    /// The status if its lock is free. Never waits.
    pub(crate) fn try_status(&self) -> Option<MutexGuard<'_, Status>> {
        try_lock(&self.status)
    }

    /// Change stream `stream`'s status, if the status is still about
    /// open `epoch`.
    pub(crate) fn stream(&self, epoch: u64, stream: usize, f: impl FnOnce(&mut StreamStatus)) {
        let mut st = self.status();
        if st.epoch == epoch
            && let Some(s) = st.streams.get_mut(stream)
        {
            f(s);
        }
    }

    pub(crate) fn is_shut_down(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Whether open `epoch` is the latest, and the player runs.
    pub(crate) fn is_current(&self, epoch: u64) -> bool {
        self.epoch.load(Ordering::Acquire) == epoch && !self.is_shut_down()
    }

    /// Tell the player's owner: an event, then a repaint.
    pub(crate) fn event(&self, e: PlayerEvent) {
        let _ = self.events.send(e);
        self.repaint();
    }

    pub(crate) fn repaint(&self) {
        if let Some(r) = &self.repaint {
            r();
        }
    }

    /// Start a thread named `name`, whose handle the player's `join`
    /// collects.
    pub(crate) fn spawn(
        &self,
        name: String,
        f: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<()> {
        spawn_into(&self.handles, name, f)
    }
}

/// Start a thread named `name`, sending its handle on `handles`.
pub(crate) fn spawn_into(
    handles: &Sender<(String, JoinHandle<()>)>,
    name: String,
    f: impl FnOnce() + Send + 'static,
) -> std::io::Result<()> {
    let handle = std::thread::Builder::new().name(name.clone()).spawn(f)?;
    let _ = handles.send((name, handle));
    Ok(())
}

/// A frame's place in its stream: what [`frames_between`] compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) run: u64,
    pub(crate) seq: u64,
    pub(crate) time: Time,
    pub(crate) duration: Option<Time>,
}

impl Place {
    pub(crate) fn of_frame(f: &VideoFrame) -> Place {
        Place {
            run: f.run,
            seq: f.seq,
            time: f.time,
            duration: f.duration,
        }
    }

    pub(crate) fn of_slot(f: &crate::slots::SlotFrame) -> Place {
        Place {
            run: f.run,
            seq: f.seq,
            time: f.time,
            duration: f.duration,
        }
    }
}

/// How many frames of a stream lie between `prev` and `next`, two frames
/// of it in time order: from their places in a run when they share one
/// (DECISIONS D29), else from the time between them over `prev`'s
/// duration, rounded. What the presenter counts as dropped and the decode
/// worker as decode-dropped.
pub(crate) fn frames_between(prev: &Place, next: &Place) -> u64 {
    if prev.run == next.run && next.seq > prev.seq {
        return next.seq - prev.seq - 1;
    }
    let (Some(d), Ok(gap)) = (prev.duration, next.time.checked_sub(prev.time)) else {
        return 0;
    };
    match gap.0.checked_div(d.0) {
        Ok(r) if d.0.is_positive() => {
            u64::try_from(r.round(Rounding::Nearest).saturating_sub(1)).unwrap_or(0)
        }
        _ => 0,
    }
}

/// `d` as a clip time.
pub(crate) fn time_of(d: std::time::Duration) -> Time {
    let nanos = i64::try_from(d.as_nanos()).unwrap_or(i64::MAX);
    Time(Rational::new(nanos, 1_000_000_000).unwrap_or(Rational::ZERO))
}

/// `t` in milliseconds, for the numbers.
pub(crate) fn ms(t: Time) -> f32 {
    (t.to_secs_f64() * 1000.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(run: u64, seq: u64, k: i64, duration: Option<i64>) -> Place {
        let t = |k: i64| Time(Rational::new(k, 30).unwrap_or(Rational::ZERO));
        Place {
            run,
            seq,
            time: t(k),
            duration: duration.map(t),
        }
    }

    #[test]
    fn frames_between_counts_by_run_place_else_by_time() {
        // One run: its places say it, whatever the times.
        assert_eq!(
            frames_between(&place(1, 4, 4, Some(1)), &place(1, 5, 5, Some(1))),
            0
        );
        assert_eq!(
            frames_between(&place(1, 4, 4, Some(1)), &place(1, 7, 9, Some(1))),
            2
        );
        // Another run (a decoder that caught up starts one): the time
        // between over the frame's duration, rounded.
        assert_eq!(
            frames_between(&place(1, 4, 4, Some(1)), &place(2, 0, 8, Some(1))),
            3
        );
        assert_eq!(
            frames_between(&place(1, 4, 4, Some(2)), &place(2, 0, 9, Some(2))),
            2
        );
        assert_eq!(
            frames_between(&place(1, 4, 4, Some(1)), &place(2, 0, 5, Some(1))),
            0
        );
        // No duration, or a frame back: none.
        assert_eq!(
            frames_between(&place(1, 4, 4, None), &place(2, 0, 9, None)),
            0
        );
        assert_eq!(
            frames_between(&place(1, 4, 9, Some(1)), &place(2, 0, 4, Some(1))),
            0
        );
    }
}
