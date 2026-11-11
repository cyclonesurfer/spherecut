//! What the player's threads share: the frame cache, the GPU slots, the
//! numbers, and the status the workers report, which the UI thread reads
//! with `try_lock` only.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
use std::thread::JoinHandle;

use spherecut_core::{Rational, Rounding, StopSource, StopToken, Time, may_block};
use spherecut_media::HwDevice;
use spherecut_media::backend::{
    DecodePath, Exactness, FallbackReason, HwKind, SeekMode, VideoFrame, VideoStreamInfo,
};
use spherecut_media::mp4::TrackTiming;
use spherecut_render::vkshare::SharedVideo;

use crate::audio::AudioOutput;
use crate::budget::{AutoBudget, CacheBudget};
use crate::cache::FrameCache;
use crate::decoder::{HwPromise, HwSupport};
use crate::lock::{LockSpec, LockStatus, field};
use crate::perf::{MemoryStats, PerfStats};
use crate::player::{PlayerError, PlayerEvent};
use crate::slots::{SLOT_FLOOR, SlotRing};

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
    /// The decoder skips frames to catch up with the clock (section 8.2).
    pub catching_up: bool,
    /// Why hardware decoding went on in software, once it did.
    pub fallback: Option<(HwKind, FallbackReason)>,
    /// Why it can't open or go on.
    pub error: Option<PlayerError>,
    /// The request the error stopped, if one was being worked on.
    pub failed_request: Option<u64>,
    /// The clip time its last frame ends at, once decoding reached it.
    pub end: Option<Time>,
    /// The clip time its last frame starts at, as `end`.
    pub last: Option<Time>,
    pub landed: Option<Landed>,
    pub seeks: SeekCounts,
    /// Frames the worker handed out, from seeks, steps and decoding on,
    /// and the clip time of the last of them.
    pub handed_out: u64,
    pub newest: Option<Time>,
    /// Frames its decoder decoded, handed out or passed over (catching
    /// up), as [`DecodeStats::frames_decoded`] counts them: the decodes
    /// the stream cost, which a lens that passes frames over still pays
    /// (M3 plan amendment 4, K2).
    ///
    /// [`DecodeStats::frames_decoded`]: spherecut_media::backend::DecodeStats::frames_decoded
    pub decoded: u64,
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
    /// Why a frame left on its decoder's device couldn't be taken by the
    /// GPU (slice 5b: an import of DMA-BUFs, a copy out of a Vulkan
    /// image, or FFmpeg's context on the renderer's device), the first
    /// time: the opens after it download.
    pub(crate) import_refused: Option<String>,
    /// The upload thread's last reading of the memory, once a second.
    pub(crate) memory: Option<MemoryStats>,
    /// The open's lock group, when it opened a locked pair.
    pub(crate) lock: Option<LockGroup>,
}

/// A locked pair's group (M3 plan section 9): what it was asked for, its
/// members, where it is, and the one time its members catch up to.
#[derive(Debug, Clone)]
pub(crate) struct LockGroup {
    pub(crate) spec: LockSpec,
    /// The player streams locked together, by their place in what was
    /// opened: the frame cache's keys for them.
    pub(crate) members: [usize; 2],
    pub(crate) status: LockStatus,
    /// The group's catch-up time, raised when a member falls more than 2
    /// frames behind the clock: to the clock's time when every member
    /// has, else to the clock's time or the newest frame every other
    /// member holds, whichever is earlier. Cleared by a seek or a step.
    /// No member catches up on its own.
    pub(crate) until: Option<Time>,
    /// Each member's timing tables, as the open read them through the
    /// backend before it chose the decoders: `None` for a track with no
    /// tables, or whose tables couldn't be read.
    pub(crate) timing: [Option<TrackTiming>; 2],
}

impl LockGroup {
    /// The group of a pair opened locked, player streams 0 and 1, with
    /// their `timing` tables: pending until both members' decoders have
    /// opened.
    pub(crate) fn pending(spec: LockSpec, timing: [Option<TrackTiming>; 2]) -> LockGroup {
        LockGroup {
            spec,
            members: [0, 1],
            status: LockStatus::Pending,
            until: None,
            timing,
        }
    }

    /// The first [`field`] in which the members differ, given each one's
    /// decoder's `info`, or `Ok` when they agree (M3 plan section 9): the
    /// tables (`"timing"` when a member has none); their time base;
    /// their first frames' times, lens 1's less lens 0's, which must be
    /// the spec's offset exactly; each decoder's frame rate, which must
    /// be constant and the same; the pictures' size; and `stts` and
    /// `stss` over the samples both have from the first each shows. The
    /// frame count isn't compared: a lens with fewer frames ends the
    /// pair.
    fn differs(&self, info: [&VideoStreamInfo; 2]) -> Result<(), &'static str> {
        let [Some(t0), Some(t1)] = &self.timing else {
            return Err(field::TIMING);
        };
        if t0.timescale != t1.timescale {
            return Err(field::TIME_BASE);
        }
        let start = |t: &TrackTiming| {
            Rational::new(1, i64::from(t.timescale)).and_then(|tb| Time::from_pts(t.first_pts, tb))
        };
        let apart = start(t1).and_then(|s1| s1.checked_sub(start(t0)?));
        if !apart.is_ok_and(|d| d == self.spec.offset) {
            return Err(field::START);
        }
        match (info[0].frame_rate(), info[1].frame_rate()) {
            (Some(r0), Some(r1)) if r0 == r1 => {}
            _ => return Err(field::RATE),
        }
        if info[0].size != info[1].size {
            return Err(field::SIZE);
        }
        if t0.stts_mismatch(t1).is_some() {
            return Err(field::STTS);
        }
        if t0.stss_mismatch(t1).is_some() {
            return Err(field::STSS);
        }
        Ok(())
    }

    /// Refused by `field`: the members play unlocked. What the player
    /// says of it.
    fn refuse(&mut self, field: &'static str) -> PlayerEvent {
        match field {
            field::OPEN => tracing::warn!(
                "the lenses don't lock: a lens couldn't open or decode, so the other shows unlocked"
            ),
            field::TIMING => tracing::warn!(
                "the lenses don't lock: a lens has no timing tables, or they couldn't be read, \
                 so they show unlocked"
            ),
            field::SLOTS => tracing::warn!(
                "the GPU has too few slots for a locked pair, two a lens: the lenses show unlocked"
            ),
            field::TIME_BASE
            | field::START
            | field::RATE
            | field::SIZE
            | field::STTS
            | field::STSS => {
                tracing::warn!(
                    "the lenses don't lock: their {field} differs, so they show unlocked"
                )
            }
            _ => tracing::warn!("the lenses don't lock ({field}), so they show unlocked"),
        }
        self.status = LockStatus::Refused { field };
        PlayerEvent::LockRefused { field }
    }
}

impl Status {
    /// Lock the open's pending group, or refuse it by the field that
    /// differs ([`LockGroup::differs`]), once both members' decoders have
    /// opened; what the player says of it. Locked, a lens with fewer
    /// frames than the other, as each decoder counts the frames it shows,
    /// is said once, since the pair ends at its last frame. Nothing while
    /// a member is opening, or once the group has formed or been refused.
    pub(crate) fn settle_lock(&mut self) -> Vec<PlayerEvent> {
        let Some(g) = self.lock.as_mut() else {
            return Vec::new();
        };
        if g.status != LockStatus::Pending {
            return Vec::new();
        }
        let info = g
            .members
            .map(|m| self.streams.get(m).and_then(|s| s.info.as_ref()));
        let [Some(i0), Some(i1)] = info else {
            return Vec::new();
        };
        if let Err(field) = g.differs([i0, i1]) {
            return vec![g.refuse(field)];
        }
        tracing::info!("the lenses agree: they show as pairs");
        g.status = LockStatus::Locked;
        let short = match (i0.frame_count, i1.frame_count) {
            (Some(n0), Some(n1)) if n0 > n1 => Some((g.members[1], n0 - n1)),
            (Some(n0), Some(n1)) if n1 > n0 => Some((g.members[0], n1 - n0)),
            _ => None,
        };
        short
            .map(|(stream, frames)| {
                tracing::warn!(
                    stream,
                    "a lens has {frames} fewer frames: the pair ends at its last"
                );
                PlayerEvent::LensShort { stream, frames }
            })
            .into_iter()
            .collect()
    }

    /// The members of the open's lock group while it is locked with its
    /// group base on, whose cache rules run from the base and who catch
    /// up to the group's `until`: `None` for an open not locked, and
    /// under the test switch that turns the base off.
    pub(crate) fn grouped(&self) -> Option<[usize; 2]> {
        let g = self.lock.as_ref()?;
        (g.status == LockStatus::Locked && !g.spec.test.no_group_base).then_some(g.members)
    }

    /// Player stream `stream` couldn't open, or decode after any
    /// fallback, before it handed out a frame of the open (`handed_out`
    /// false): a group it is a member of, pending or locked, is refused
    /// with `"open"`, since the pair has no last frame of it to end at
    /// (M3 plan section 9), and the group's `until` goes with it. Which
    /// of the member's first decode and its partner's open came first
    /// doesn't change it. A member that handed out a frame ends the pair
    /// instead ([`end_member`](Self::end_member)). What the player says
    /// of it, for the caller to tell once the lock is let go; the caller
    /// lets the cache's group go too.
    pub(crate) fn member_failed(&mut self, stream: usize, handed_out: bool) -> Option<PlayerEvent> {
        let g = self.lock.as_mut()?;
        let formed = matches!(g.status, LockStatus::Pending | LockStatus::Locked);
        (formed && !handed_out && g.members.contains(&stream)).then(|| {
            g.until = None;
            g.refuse(field::OPEN)
        })
    }

    /// Player stream `stream`, a member of the open's lock group, locked
    /// or still pending, can't decode on after any fallback, its last
    /// frame of the open ending at `end` and starting at `last`: the pair
    /// ends there, as at a shorter lens (M3 plan section 9), its end set
    /// for the presenter's check of the end unless an earlier end is set
    /// already. Without this, nothing ends the pair and `pick_pair` holds
    /// its last while the clock runs on. Pending, the end is set for the
    /// pair the group may become, so that which of the failure and the
    /// partner's open came first doesn't change it. The end and last
    /// frame it replaced, to put back if the member decodes past `end`
    /// after all (a seek past damage); `None` when nothing changed: not a
    /// member of a group that is pending or locked, or the test switch
    /// that keeps the pair going.
    pub(crate) fn end_member(
        &mut self,
        stream: usize,
        end: Time,
        last: Time,
    ) -> Option<(Option<Time>, Option<Time>)> {
        let g = self.lock.as_ref()?;
        if matches!(g.status, LockStatus::Refused { .. })
            || g.spec.test.no_end_on_failure
            || !g.members.contains(&stream)
        {
            return None;
        }
        let s = self.streams.get_mut(stream)?;
        if s.end.is_some_and(|e| e <= end) {
            return None;
        }
        tracing::warn!(
            stream,
            "a lens can't decode on: the pair ends at its last frame"
        );
        let before = (s.end, s.last);
        s.end = Some(end);
        s.last = Some(last);
        Some(before)
    }

    /// `live` slots are left, as a slot dies or as the lenses lock on a
    /// ring some of whose slots died before: a locked group with fewer
    /// than [`SLOT_FLOOR`] live slots a member unlocks, refused with
    /// `"slots"`, and its streams show unlocked as each one's frame
    /// comes (M3 plan section 9), unless the test switch keeps it locked.
    /// What the player says of it, for the caller to tell once the lock
    /// is let go; the caller lets the cache's group go too, or never
    /// sets it.
    pub(crate) fn below_floor(&mut self, live: usize) -> Option<PlayerEvent> {
        let g = self.lock.as_mut()?;
        if g.status != LockStatus::Locked
            || g.spec.test.no_floor
            || live >= SLOT_FLOOR * g.members.len()
        {
            return None;
        }
        g.until = None;
        Some(g.refuse(field::SLOTS))
    }
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
    /// The slot allocations still to fail as the GPU having no memory
    /// for them: a test's hook ([`Player::fail_slot_allocations`]).
    ///
    /// [`Player::fail_slot_allocations`]: crate::Player::fail_slot_allocations
    pub(crate) slot_faults: AtomicU64,
    /// A test's hook ([`Player::old_idle_rule`]): the decode workers keep
    /// the idle rules from before a request the cache answered could take
    /// them on from the end or a failure.
    ///
    /// [`Player::old_idle_rule`]: crate::Player::old_idle_rule
    pub(crate) old_idle: AtomicBool,
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
    /// The GUI's checks, still running when the player started.
    pub(crate) hw_later: Option<HwPromise>,
    /// FFmpeg's Vulkan context on the renderer's device (slice 5b's path
    /// B), made by the first open that wants it: `None` when FFmpeg
    /// refused the device, which the status's `import_refused` says.
    pub(crate) vulkan_device: OnceLock<Option<Arc<HwDevice>>>,
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
    /// FFmpeg's Vulkan context on the renderer's device, `video`, made
    /// the first time (blocking on the driver, off the UI thread); `None`
    /// when FFmpeg refuses it, which the status keeps as the reason zero
    /// copy is off.
    pub(crate) fn vulkan_device(&self, video: &SharedVideo) -> Option<Arc<HwDevice>> {
        self.vulkan_device
            .get_or_init(|| match HwDevice::vulkan_shared(&video.handles) {
                Ok(d) => {
                    tracing::info!("Vulkan Video decodes on the renderer's own device");
                    Some(Arc::new(d))
                }
                Err(e) => {
                    let why = format!("FFmpeg can't decode on the renderer's Vulkan device: {e}");
                    tracing::warn!("{why}");
                    let mut st = self.status();
                    if st.import_refused.is_none() {
                        st.import_refused = Some(why);
                    }
                    None
                }
            })
            .clone()
    }

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

    /// Change stream `stream`'s status by `f`, as [`stream`](Self::stream)
    /// does, and under the same lock the open's lock group by `group`
    /// ([`Status::settle_lock`] or [`Status::member_failed`]): what came
    /// of the group, for the caller to tell once the lock is let go.
    pub(crate) fn stream_and_group<E: IntoIterator<Item = PlayerEvent>>(
        &self,
        epoch: u64,
        stream: usize,
        f: impl FnOnce(&mut StreamStatus),
        group: impl FnOnce(&mut Status) -> E,
    ) -> Vec<PlayerEvent> {
        let mut st = self.status();
        if st.epoch != epoch {
            return Vec::new();
        }
        if let Some(s) = st.streams.get_mut(stream) {
            f(s);
        }
        group(&mut st).into_iter().collect()
    }

    pub(crate) fn is_shut_down(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Whether the slot allocation now is one a test planted to fail
    /// ([`slot_faults`](Self::slot_faults)), counting it if so.
    pub(crate) fn slot_fault(&self) -> bool {
        self.slot_faults
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .is_ok()
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

/// How far `t`, the clock's time a present is for, is from the shown
/// frame's interval, from its `time` for its `duration`: 0 inside it,
/// positive past its end (the video is late), negative before its start
/// (early). A frame without a duration is a point. Showing each frame
/// for its interval is in sync, so the frame rate's own steps count as
/// none.
pub(crate) fn av_offset(t: Time, time: Time, duration: Option<Time>) -> Option<Time> {
    let from_start = t.checked_sub(time).ok()?;
    if from_start < Time::ZERO {
        return Some(from_start);
    }
    let past_end = from_start
        .checked_sub(duration.unwrap_or(Time::ZERO))
        .ok()?;
    Some(past_end.max(Time::ZERO))
}

/// `t` in milliseconds, for the numbers.
pub(crate) fn ms(t: Time) -> f32 {
    (t.to_secs_f64() * 1000.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_av_offset_is_the_clocks_distance_from_the_frames_interval() {
        let t = |n: i64, d: i64| Time(Rational::new(n, d).unwrap_or(Rational::ZERO));
        let ms_at = |clock: Time| av_offset(clock, t(1, 1), Some(t(1001, 30000))).map(ms);
        // Anywhere inside the frame's 33.37 ms: in sync.
        assert_eq!(ms_at(t(1, 1)), Some(0.0));
        assert_eq!(ms_at(t(1033, 1000)), Some(0.0));
        assert_eq!(ms_at(t(31000, 30000)), Some(0.0));
        assert!(ms_at(t(31002, 30000)).unwrap() > 0.0);
        // Past its end: late by the rest; before its start: early.
        assert!((ms_at(t(1043, 1000)).unwrap() - 9.633).abs() < 0.001);
        assert!((ms_at(t(990, 1000)).unwrap() + 10.0).abs() < 0.001);
        // No duration: a point.
        assert_eq!(av_offset(t(1, 1), t(1, 1), None), Some(Time::ZERO));
        assert!((ms(av_offset(t(1020, 1000), t(1, 1), None).unwrap()) - 20.0).abs() < 0.001);
    }

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
