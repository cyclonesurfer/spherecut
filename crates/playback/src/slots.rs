//! GPU slots (M1 plan sections 4.2, 4.4 and 8.3, DECISIONS D34): the
//! frames the upload thread has put on the GPU for the viewer to draw,
//! shared by the upload thread and the UI.
//!
//! Each slot holds one [`GpuFrame`], made for a frame's shape and reused
//! for every frame of that shape, and says which frame is in it. Its
//! state is one atomic word, a state and a version that every claim
//! bumps:
//!
//! ```text
//!  Free ──(upload claims, the ring alone holds the GpuFrame)──► Writing
//!    ▲                                                            │ submitted
//!    │ unwanted (the upload thread)                                ▼
//!    └──────────────────────────────────────────────────────── Ready ◄──┐
//!                                       the presenter shows it │       │ shown no more
//!                                                              ▼       │
//!                                                            Shown ────┘
//! ```
//!
//! - The upload thread writes only into a slot it claimed, from Free,
//!   when the ring's `Arc` is the only one: no draw that sampled the slot
//!   can still be waiting to be submitted, since the viewer's paint
//!   callbacks hold a clone until eframe's submit, and wgpu orders the
//!   upload's submission after every earlier one on the queue (plan
//!   section 4.4). A slot shown before and freed while a callback still
//!   holds it waits, Free, until the callback lets go: the plan's
//!   "Released".
//! - The presenter, on the UI thread, shows a Ready slot by compare and
//!   swap, and gives it back as Ready when it shows another, so a frame
//!   shown before can be shown again (a step back) while it is still in
//!   its slot. It reads a slot's frame without a lock: the fields are
//!   atomics written while the slot is Writing, and a read is kept only
//!   if the state word is the same before and after it, so a slot freed
//!   and filled again meanwhile (its version moved on) is never mistaken
//!   for the frame it held.
//! - The upload thread frees the Ready slots it no longer wants (frames
//!   of a stale run after a seek, frames the presenter has passed), never
//!   a Shown one, so the frame on screen stays up until the next one is
//!   Ready: no black flash.
//! - A slot the GPU has no memory for is Dead (risk 11): the ring plays
//!   on with one fewer.
//!
//! The UI thread only loads, compares and swaps, and `try_lock`s the
//! mutex that holds a slot's `Arc<GpuFrame>`, which the upload thread
//! locks only for a slot it is claiming or has claimed.

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering, fence};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use spherecut_core::{Rational, Time, may_block};
use spherecut_render::GpuFrame;

/// The fewest, default and most GPU slots (M1 plan section 4.2).
pub const MIN_SLOTS: u8 = 3;
pub const DEFAULT_SLOTS: u8 = 4;
pub const MAX_SLOTS: u8 = 6;

/// Where a slot is in its cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Free,
    Writing,
    Ready,
    Shown,
    /// The GPU had no memory for it.
    Dead,
}

const FREE: u64 = 0;
const WRITING: u64 = 1;
const READY: u64 = 2;
const SHOWN: u64 = 3;
const DEAD: u64 = 4;
const KIND: u64 = 0b111;
const VERSION: u64 = 1 << 3;

fn kind(word: u64) -> u64 {
    word & KIND
}

fn state_of(word: u64) -> SlotState {
    match kind(word) {
        FREE => SlotState::Free,
        WRITING => SlotState::Writing,
        READY => SlotState::Ready,
        SHOWN => SlotState::Shown,
        _ => SlotState::Dead,
    }
}

/// Which frame a slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotFrame {
    /// The stream, by its place in what was opened (the frame cache's
    /// key), and the open it belongs to.
    pub stream: usize,
    pub epoch: u64,
    /// The frame's clip time, pts, run and place in it (DECISIONS D29).
    pub time: Time,
    pub pts: i64,
    pub run: u64,
    pub seq: u64,
    pub duration: Option<Time>,
}

/// A slot that is Ready or Shown, read at one version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotView {
    pub index: usize,
    pub state: SlotState,
    /// The state word: what [`SlotRing::try_show`] compares against.
    pub word: u64,
    pub frame: SlotFrame,
}

#[derive(Debug)]
struct Slot {
    state: AtomicU64,
    stream: AtomicU64,
    epoch: AtomicU64,
    time: [AtomicI64; 2],
    pts: AtomicI64,
    run: AtomicU64,
    seq: AtomicU64,
    /// The duration's numerator and denominator; a denominator of 0 is
    /// none.
    duration: [AtomicI64; 2],
    gpu: Mutex<Option<Arc<GpuFrame>>>,
}

impl Slot {
    fn new() -> Slot {
        Slot {
            state: AtomicU64::new(FREE),
            stream: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            time: [AtomicI64::new(0), AtomicI64::new(1)],
            pts: AtomicI64::new(0),
            run: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            duration: [AtomicI64::new(0), AtomicI64::new(0)],
            gpu: Mutex::new(None),
        }
    }

    fn read_frame(&self) -> SlotFrame {
        let r = Ordering::Relaxed;
        let rational = |p: &[AtomicI64; 2]| Rational::new(p[0].load(r), p[1].load(r)).ok();
        SlotFrame {
            stream: self.stream.load(r) as usize,
            epoch: self.epoch.load(r),
            time: Time(rational(&self.time).unwrap_or(Rational::ZERO)),
            pts: self.pts.load(r),
            run: self.run.load(r),
            seq: self.seq.load(r),
            duration: rational(&self.duration).map(Time),
        }
    }

    fn write_frame(&self, f: &SlotFrame) {
        let r = Ordering::Relaxed;
        self.stream.store(f.stream as u64, r);
        self.epoch.store(f.epoch, r);
        self.time[0].store(f.time.0.num(), r);
        self.time[1].store(f.time.0.den(), r);
        self.pts.store(f.pts, r);
        self.run.store(f.run, r);
        self.seq.store(f.seq, r);
        let (n, d) = f.duration.map_or((0, 0), |d| (d.0.num(), d.0.den()));
        self.duration[0].store(n, r);
        self.duration[1].store(d, r);
    }

    #[track_caller]
    fn lock(&self) -> MutexGuard<'_, Option<Arc<GpuFrame>>> {
        may_block("taking a GPU slot's lock");
        self.gpu.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// How many slots are in each state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotStats {
    /// Slots in use: the setting, less the dead ones.
    pub active: usize,
    pub free: usize,
    pub writing: usize,
    pub ready: usize,
    pub shown: usize,
    pub dead: usize,
    /// Free slots whose GPU frame a draw still holds.
    pub held: usize,
}

/// The GPU slots. Share it in an `Arc` between the upload thread and the
/// UI.
#[derive(Debug)]
pub struct SlotRing {
    slots: Vec<Slot>,
    /// The setting: how many slots, from the first, are used.
    active: AtomicUsize,
}

/// A slot the upload thread claimed: it alone writes it until it
/// publishes or gives it back.
#[derive(Debug)]
pub(crate) struct Claim {
    pub(crate) index: usize,
    word: u64,
}

impl SlotRing {
    /// A ring of [`MAX_SLOTS`] slots, `active` of them in use (clamped
    /// to [`MIN_SLOTS`]..=[`MAX_SLOTS`]). Slots get their GPU frames when
    /// the first frame is uploaded into them.
    pub fn new(active: u8) -> SlotRing {
        SlotRing {
            slots: (0..MAX_SLOTS).map(|_| Slot::new()).collect(),
            active: AtomicUsize::new(usize::from(active.clamp(MIN_SLOTS, MAX_SLOTS))),
        }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// The setting, how many slots are used. Never waits.
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    /// Use `n` slots from now on (clamped). One beyond the new count is
    /// given up once it is free and nothing holds its frame.
    pub fn set_active(&self, n: u8) {
        self.active.store(
            usize::from(n.clamp(MIN_SLOTS, MAX_SLOTS)),
            Ordering::Release,
        );
    }

    pub fn state(&self, index: usize) -> Option<SlotState> {
        self.slots
            .get(index)
            .map(|s| state_of(s.state.load(Ordering::Acquire)))
    }

    /// Slot `index`'s frame, when it is Ready or Shown, read without a
    /// lock at one version. Never waits.
    pub fn view(&self, index: usize) -> Option<SlotView> {
        let slot = self.slots.get(index)?;
        let before = slot.state.load(Ordering::Acquire);
        if !matches!(kind(before), READY | SHOWN) {
            return None;
        }
        let frame = slot.read_frame();
        fence(Ordering::Acquire);
        (slot.state.load(Ordering::Relaxed) == before).then_some(SlotView {
            index,
            state: state_of(before),
            word: before,
            frame,
        })
    }

    /// Every Ready or Shown slot. Never waits.
    pub fn views(&self) -> Vec<SlotView> {
        (0..self.slots.len()).filter_map(|i| self.view(i)).collect()
    }

    /// Show the slot `view` read, if it still holds that frame: its GPU
    /// frame, `None` if it moved on, or its lock was busy (it is then left
    /// as it was, to try next frame). For the UI thread: never waits.
    pub fn try_show(&self, view: &SlotView) -> Option<Arc<GpuFrame>> {
        let slot = self.slots.get(view.index)?;
        let shown = (view.word & !KIND) | SHOWN;
        if view.state == SlotState::Ready {
            slot.state
                .compare_exchange(view.word, shown, Ordering::AcqRel, Ordering::Acquire)
                .ok()?;
        } else if slot.state.load(Ordering::Acquire) != view.word {
            return None;
        }
        let gpu = match slot.gpu.try_lock() {
            Ok(g) => g.clone(),
            Err(TryLockError::Poisoned(p)) => p.into_inner().clone(),
            Err(TryLockError::WouldBlock) => None,
        };
        if gpu.is_none() && view.state == SlotState::Ready {
            self.unshow(view.index, shown);
        }
        gpu
    }

    /// Give back a slot shown at `word` as Ready: the presenter shows
    /// another. Never waits.
    pub fn unshow(&self, index: usize, word: u64) {
        if let Some(slot) = self.slots.get(index) {
            let ready = (word & !KIND) | READY;
            let _ = slot.state.compare_exchange(
                (word & !KIND) | SHOWN,
                ready,
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
        }
    }

    /// Free a Ready slot the upload thread no longer wants, if it is still
    /// as `view` read it. Never a Shown one.
    pub(crate) fn discard(&self, view: &SlotView) -> bool {
        let Some(slot) = self.slots.get(view.index) else {
            return false;
        };
        if view.state != SlotState::Ready {
            return false;
        }
        slot.state
            .compare_exchange(
                view.word,
                (view.word & !KIND) | FREE,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Claim a free slot among the active ones whose GPU frame nothing
    /// but the ring holds, for the upload thread to write.
    #[track_caller]
    pub(crate) fn claim(&self) -> Option<Claim> {
        let active = self.active();
        for (index, slot) in self.slots.iter().enumerate().take(active) {
            let word = slot.state.load(Ordering::Acquire);
            if kind(word) != FREE {
                continue;
            }
            let g = slot.lock();
            if g.as_ref().is_some_and(|f| Arc::strong_count(f) > 1) {
                continue;
            }
            let claimed = (word.wrapping_add(VERSION) & !KIND) | WRITING;
            if slot
                .state
                .compare_exchange(word, claimed, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                // As the clocks' sequence lock does: the fields are
                // written after the claim is seen.
                fence(Ordering::Release);
                return Some(Claim {
                    index,
                    word: claimed,
                });
            }
        }
        None
    }

    /// The GPU frame a claimed slot holds, to upload into or replace.
    #[track_caller]
    pub(crate) fn frame_of(&self, claim: &Claim) -> Option<Arc<GpuFrame>> {
        let slot = self.slots.get(claim.index)?;
        slot.lock().clone()
    }

    /// Give a claimed slot a new GPU frame, dropping the old one.
    #[track_caller]
    pub(crate) fn replace(&self, claim: &Claim, gpu: Arc<GpuFrame>) {
        if let Some(s) = self.slots.get(claim.index) {
            *s.lock() = Some(gpu);
        }
    }

    /// The claimed slot holds `frame` now: Ready.
    pub(crate) fn publish(&self, claim: Claim, frame: &SlotFrame) {
        if let Some(s) = self.slots.get(claim.index) {
            s.write_frame(frame);
            s.state
                .store((claim.word & !KIND) | READY, Ordering::Release);
        }
    }

    /// Give a claimed slot back unwritten.
    pub(crate) fn abandon(&self, claim: Claim) {
        if let Some(s) = self.slots.get(claim.index) {
            s.state
                .store((claim.word & !KIND) | FREE, Ordering::Release);
        }
    }

    /// A claimed slot the GPU has no memory for: Dead from now on, with no
    /// GPU frame.
    #[track_caller]
    pub(crate) fn kill(&self, claim: Claim) {
        if let Some(s) = self.slots.get(claim.index) {
            *s.lock() = None;
            s.state
                .store((claim.word & !KIND) | DEAD, Ordering::Release);
        }
    }

    /// Drop the GPU frames of free slots past the active count that
    /// nothing else holds, which gives their memory back.
    #[track_caller]
    pub(crate) fn trim(&self) {
        let active = self.active();
        for slot in self.slots.iter().skip(active) {
            if kind(slot.state.load(Ordering::Acquire)) != FREE {
                continue;
            }
            let mut g = slot.lock();
            if g.as_ref().is_some_and(|f| Arc::strong_count(f) == 1) {
                *g = None;
            }
        }
    }

    /// Free every Ready slot: a new file was opened. Shown ones stay until
    /// the presenter gives them back.
    pub(crate) fn discard_all(&self) {
        for i in 0..self.slots.len() {
            if let Some(v) = self.view(i) {
                self.discard(&v);
            }
        }
    }

    /// How many slots are in each state. Never waits: a slot whose lock is
    /// busy isn't counted as held.
    pub fn stats(&self) -> SlotStats {
        let active = self.active();
        let mut s = SlotStats::default();
        for (i, slot) in self.slots.iter().enumerate() {
            let k = kind(slot.state.load(Ordering::Acquire));
            if k == DEAD {
                s.dead += 1;
                continue;
            }
            if i >= active {
                continue;
            }
            match k {
                FREE => {
                    s.free += 1;
                    if let Ok(g) = slot.gpu.try_lock()
                        && g.as_ref().is_some_and(|f| Arc::strong_count(f) > 1)
                    {
                        s.held += 1;
                    }
                }
                WRITING => s.writing += 1,
                READY => s.ready += 1,
                _ => s.shown += 1,
            }
        }
        s.active = active.saturating_sub(
            self.slots
                .iter()
                .take(active)
                .filter(|sl| kind(sl.state.load(Ordering::Acquire)) == DEAD)
                .count(),
        );
        s
    }

    /// How many `Arc`s hold slot `index`'s GPU frame, the ring's own
    /// included; `None` without one, or while its lock is busy. For
    /// tests of the reuse rule.
    pub fn holders(&self, index: usize) -> Option<usize> {
        let slot = self.slots.get(index)?;
        let g = slot.gpu.try_lock().ok()?;
        g.as_ref().map(Arc::strong_count)
    }

    /// Slot `index`'s GPU frame, whatever its state, for tests that read
    /// it back. Takes the slot's lock.
    #[track_caller]
    pub fn gpu_frame(&self, index: usize) -> Option<Arc<GpuFrame>> {
        // Not in a closure, which would be the caller `lock` names.
        let slot = self.slots.get(index)?;
        slot.lock().clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use spherecut_core::video::{ColorFamily, PixelLayout, PlaneDesc, SampleBytes};
    use spherecut_render::{AdapterChoice, Gpu, SlotShape};

    use super::*;

    fn gpu() -> &'static Gpu {
        static GPU: OnceLock<Gpu> = OnceLock::new();
        GPU.get_or_init(|| match Gpu::headless(&AdapterChoice::Cpu) {
            Ok(g) => g,
            Err(e) => panic!("lavapipe is needed for the slot tests: {e}"),
        })
    }

    fn gpu_frame() -> Arc<GpuFrame> {
        let layout = PixelLayout {
            name: "gray".into(),
            planes: vec![PlaneDesc {
                components: 1,
                bytes: SampleBytes::One,
                log2_sub: (0, 0),
            }],
            family: ColorFamily::Gray,
            bit_depth: 8,
            msb_aligned: false,
        };
        let shape = SlotShape::of(&layout, (16, 8)).unwrap_or_else(|e| panic!("{e}"));
        Arc::new(GpuFrame::new(gpu(), &shape).unwrap_or_else(|e| panic!("{e}")))
    }

    fn frame(k: i64) -> SlotFrame {
        SlotFrame {
            stream: 0,
            epoch: 1,
            time: Time(Rational::from_integer(k)),
            pts: k,
            run: 1,
            seq: k as u64,
            duration: Some(Time(Rational::ONE)),
        }
    }

    /// Fill a free slot with frame `k`; its index.
    fn fill(ring: &SlotRing, k: i64) -> usize {
        let claim = ring
            .claim()
            .unwrap_or_else(|| panic!("no free slot for {k}"));
        if ring.frame_of(&claim).is_none() {
            ring.replace(&claim, gpu_frame());
        }
        let i = claim.index;
        ring.publish(claim, &frame(k));
        i
    }

    #[test]
    fn a_slot_goes_round_and_is_claimed_only_when_nothing_else_holds_it() {
        let ring = SlotRing::new(3);
        let a = fill(&ring, 0);
        let v = ring
            .view(a)
            .unwrap_or_else(|| panic!("slot {a} isn't Ready"));
        assert_eq!((v.state, v.frame), (SlotState::Ready, frame(0)));
        let shown = ring.try_show(&v).unwrap_or_else(|| panic!("not shown"));
        assert_eq!(ring.state(a), Some(SlotState::Shown));
        // A shown slot is never freed.
        let sv = ring
            .view(a)
            .unwrap_or_else(|| panic!("slot {a} isn't Shown"));
        assert!(!ring.discard(&sv));
        ring.unshow(a, v.word);
        assert_eq!(ring.state(a), Some(SlotState::Ready));
        let v = ring
            .view(a)
            .unwrap_or_else(|| panic!("slot {a} isn't Ready"));
        assert!(ring.discard(&v));
        assert_eq!(ring.state(a), Some(SlotState::Free));
        // Free, but a draw holds its frame: not claimed until let go.
        assert_eq!(ring.holders(a), Some(2));
        let b = fill(&ring, 1);
        let c = fill(&ring, 2);
        assert!(a != b && a != c && b != c);
        assert!(ring.claim().is_none(), "the held slot was claimed");
        assert_eq!(ring.stats().held, 1);
        drop(shown);
        assert_eq!(ring.holders(a), Some(1));
        assert_eq!(fill(&ring, 3), a);
    }

    #[test]
    fn a_view_of_a_slot_that_moved_on_is_refused() {
        let ring = SlotRing::new(3);
        let a = fill(&ring, 0);
        let old = ring
            .view(a)
            .unwrap_or_else(|| panic!("slot {a} isn't Ready"));
        assert!(ring.discard(&old));
        // The first free slot is `a` again: filled with another frame.
        assert_eq!(fill(&ring, 5), a);
        assert!(
            ring.try_show(&old).is_none(),
            "showed a view of a frame gone"
        );
        assert!(!ring.discard(&old));
        let new = ring
            .view(a)
            .unwrap_or_else(|| panic!("slot {a} isn't Ready"));
        assert_eq!(new.frame, frame(5));
        assert_ne!(new.word, old.word);
        assert!(ring.try_show(&new).is_some());
    }

    #[test]
    fn a_slot_without_memory_is_out_for_good() {
        let ring = SlotRing::new(3);
        let claim = ring.claim().unwrap_or_else(|| panic!("no slot"));
        let dead = claim.index;
        ring.kill(claim);
        assert_eq!(ring.state(dead), Some(SlotState::Dead));
        let s = ring.stats();
        assert_eq!((s.active, s.dead, s.free), (2, 1, 2));
        let b = fill(&ring, 0);
        let c = fill(&ring, 1);
        assert!(b != dead && c != dead);
        assert!(ring.claim().is_none());
    }

    #[test]
    fn slots_past_a_smaller_setting_give_their_memory_back() {
        let ring = SlotRing::new(4);
        let all: Vec<usize> = (0..4).map(|k| fill(&ring, k)).collect();
        for &i in &all {
            let v = ring
                .view(i)
                .unwrap_or_else(|| panic!("slot {i} isn't Ready"));
            assert!(ring.discard(&v));
        }
        ring.set_active(3);
        assert_eq!(ring.active(), 3);
        ring.trim();
        assert_eq!(ring.holders(3), None, "slot 3 kept its frame");
        assert_eq!(ring.holders(0), Some(1));
        // Clamped to the plan's range.
        ring.set_active(1);
        assert_eq!(ring.active(), usize::from(MIN_SLOTS));
        ring.set_active(9);
        assert_eq!(ring.active(), usize::from(MAX_SLOTS));
    }
}
