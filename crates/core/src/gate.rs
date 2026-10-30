//! The submit gate (DECISIONS D41): while the UI thread holds a
//! swapchain image, from acquiring it to presenting it, no other thread
//! submits work to the device it renders with.
//!
//! RADV lists an image the app holds in every submission's buffer list,
//! and amdgpu marks each submission a writer of every buffer in its
//! list, so a compositor that waits for the image to be written waits
//! for whatever else was submitted meanwhile: a decode, the copier's
//! copy, an upload with its mips. On odin a 5.7K decode in that window
//! cost one to four refreshes. The viewer closes the gate before the
//! surface is acquired ([`SubmitGate::close`]) and opens it [`HOLD`]
//! after ([`SubmitGate::acquired`]), which covers the submit and the
//! present that follow; FFmpeg's queue locks, the copier and the
//! uploader wait at it ([`SubmitGate::wait`]) right before they submit.
//!
//! The UI thread never waits: closing and opening only take a lock no
//! one holds for longer than a check, and a wait on the UI thread (its
//! own submissions, or an FFmpeg frame freed there) passes at once. A
//! gate closed and not acquired (a viewer that prepared and wasn't
//! painted) opens at the next pass's logic, which follows the present
//! ([`SubmitGate::open`]), or by itself after [`CLOSED_AT_MOST`].

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long the gate stays closed after the surface is acquired: the
/// rest of egui's pass, its submit and the present take well under 1 ms
/// on odin, and a hold of 2 ms left no submission in the window there.
pub const HOLD: Duration = Duration::from_millis(2);

/// How long a gate closed and never opened stays closed.
pub const CLOSED_AT_MOST: Duration = Duration::from_millis(20);

/// One device's gate. Open until the UI closes it.
#[derive(Debug)]
pub struct SubmitGate {
    hold: Duration,
    at_most: Duration,
    state: Mutex<State>,
    moved: Condvar,
}

#[derive(Debug, Default)]
struct State {
    /// Closed until then.
    until: Option<Instant>,
    /// When it was last closed, and when the surface was last acquired.
    closed_at: Option<Instant>,
    acquired_at: Option<Instant>,
    waits: u64,
    longest: Duration,
}

/// What the gate held back: how many submissions waited, and the
/// longest wait.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateStats {
    pub waits: u64,
    pub longest: Duration,
}

impl Default for SubmitGate {
    fn default() -> Self {
        SubmitGate::with_times(HOLD, CLOSED_AT_MOST)
    }
}

impl SubmitGate {
    /// A gate that holds `hold` after the acquire, and `at_most` after a
    /// close that no acquire follows.
    pub fn with_times(hold: Duration, at_most: Duration) -> SubmitGate {
        SubmitGate {
            hold,
            at_most,
            state: Mutex::new(State::default()),
            moved: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The UI, before it acquires the surface: other submissions wait.
    pub fn close(&self) {
        let now = Instant::now();
        let mut s = self.state();
        s.until = Some(now + self.at_most);
        s.closed_at = Some(now);
        drop(s);
        self.moved.notify_all();
    }

    /// The UI, once it holds the surface: the gate opens [`HOLD`] from
    /// now, after its submit and present.
    pub fn acquired(&self) {
        let now = Instant::now();
        let mut s = self.state();
        s.until = Some(now + self.hold);
        s.acquired_at = Some(now);
        drop(s);
        self.moved.notify_all();
    }

    /// The UI, once it has presented (its next pass): the gate opens now.
    pub fn open(&self) {
        let mut s = self.state();
        if s.until.take().is_none() {
            return;
        }
        drop(s);
        self.moved.notify_all();
    }

    /// Any other thread, right before it submits to the device: returns
    /// at once while the gate is open, else when it opens, and says how
    /// long it waited. On the UI thread it returns at once: what the UI
    /// submits is its own frame's.
    pub fn wait(&self) -> Duration {
        if crate::blocking::is_ui_thread() {
            return Duration::ZERO;
        }
        let mut s = self.state();
        let start = Instant::now();
        if s.until.is_none_or(|u| u <= start) {
            return Duration::ZERO;
        }
        loop {
            let now = Instant::now();
            match s.until {
                Some(u) if u > now => {
                    s = self
                        .moved
                        .wait_timeout(s, u - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                _ => break,
            }
        }
        let waited = start.elapsed();
        s.waits += 1;
        s.longest = s.longest.max(waited);
        waited
    }

    pub fn stats(&self) -> GateStats {
        let s = self.state();
        GateStats {
            waits: s.waits,
            longest: s.longest,
        }
    }

    /// When the gate was last closed, and when the surface was last
    /// acquired: the UI's prepare and paint, for the smoke run.
    pub fn marks(&self) -> (Option<Instant>, Option<Instant>) {
        let s = self.state();
        (s.closed_at, s.acquired_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const HOLD_T: Duration = Duration::from_millis(40);
    const AT_MOST_T: Duration = Duration::from_millis(120);

    fn gate() -> Arc<SubmitGate> {
        Arc::new(SubmitGate::with_times(HOLD_T, AT_MOST_T))
    }

    /// `wait` on another thread, started now: how long it took, and
    /// when it returned.
    fn wait_on_thread(g: &Arc<SubmitGate>) -> std::thread::JoinHandle<(Duration, Instant)> {
        let g = g.clone();
        std::thread::spawn(move || (g.wait(), Instant::now()))
    }

    #[test]
    fn an_open_gate_lets_a_submission_through_and_counts_nothing() {
        let g = gate();
        assert_eq!(g.wait(), Duration::ZERO);
        g.close();
        g.acquired();
        std::thread::sleep(HOLD_T + Duration::from_millis(10));
        assert_eq!(g.wait(), Duration::ZERO);
        assert_eq!(g.stats(), GateStats::default());
    }

    #[test]
    fn a_submission_waits_from_the_close_to_the_hold_after_the_acquire() {
        let g = gate();
        g.close();
        let waiter = wait_on_thread(&g);
        std::thread::sleep(Duration::from_millis(30));
        let acquired = Instant::now();
        g.acquired();
        let (waited, returned) = waiter.join().unwrap();
        // It waited through the close and the whole hold after the
        // acquire, and not for the close's own bound.
        assert!(returned >= acquired + HOLD_T, "{waited:?}");
        assert!(waited < AT_MOST_T, "{waited:?}");
        let stats = g.stats();
        assert_eq!(stats.waits, 1);
        assert_eq!(stats.longest, waited);
    }

    #[test]
    fn a_close_with_no_acquire_opens_by_itself() {
        let g = gate();
        let closed = Instant::now();
        g.close();
        let (waited, returned) = wait_on_thread(&g).join().unwrap();
        assert!(returned >= closed + AT_MOST_T, "{waited:?}");
        assert!(waited < AT_MOST_T * 3, "{waited:?}");
    }

    #[test]
    fn opening_lets_a_waiting_submission_through_at_once() {
        let g = gate();
        g.close();
        g.acquired();
        let waiter = wait_on_thread(&g);
        std::thread::sleep(Duration::from_millis(5));
        g.open();
        let (waited, _) = waiter.join().unwrap();
        assert!(waited < HOLD_T, "{waited:?}");
        assert_eq!(g.wait(), Duration::ZERO);
    }

    #[test]
    fn the_marks_are_the_last_close_and_acquire() {
        let g = gate();
        assert_eq!(g.marks(), (None, None));
        let before = Instant::now();
        g.close();
        g.acquired();
        let (closed, acquired) = g.marks();
        let (closed, acquired) = (closed.unwrap(), acquired.unwrap());
        assert!(before <= closed && closed <= acquired);
    }

    #[test]
    fn the_ui_thread_never_waits_at_a_closed_gate() {
        let g = gate();
        g.close();
        let (waited, blocked) = std::thread::spawn(move || {
            crate::blocking::mark_ui_thread_with(crate::blocking::OnBlock::Panic);
            let start = Instant::now();
            let waited = g.wait();
            (waited, start.elapsed())
        })
        .join()
        .unwrap();
        assert_eq!(waited, Duration::ZERO);
        assert!(blocked < HOLD_T, "{blocked:?}");
    }
}
