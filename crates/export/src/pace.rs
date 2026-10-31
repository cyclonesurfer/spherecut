//! An export's pace while the UI is busy (M2 plan amendment 2 item 22).
//!
//! An export from the app renders on the viewer's device and queue, and
//! an 8K frame with motion blur is a lot of work to put between two of
//! the viewer's presents. The submit gate (D41) keeps it out of the
//! window between the UI's acquire and its present; this keeps it to one
//! frame a UI pass while the viewer plays or a drag runs, so the work
//! between two presents is at most one export frame. Otherwise the
//! export runs as fast as it can.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The longest a held export waits for the UI's next pass: a window
/// that stops drawing (minimised, occluded) doesn't stop the export.
pub const AT_MOST: Duration = Duration::from_millis(100);

/// The UI's side and the export's: shared through an `Arc`.
#[derive(Debug, Default)]
pub struct Pace {
    state: Mutex<State>,
    moved: Condvar,
}

#[derive(Debug, Default)]
struct State {
    held: bool,
    /// The UI's passes so far.
    passes: u64,
    /// Frames that waited for a pass, and the longest wait.
    waits: u64,
    longest: Duration,
}

impl Pace {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The UI, once a pass: whether the export is held to a frame a pass
    /// now (the viewer plays, a drag runs).
    pub fn pass(&self, held: bool) {
        let mut s = self.state();
        s.held = held;
        s.passes += 1;
        drop(s);
        self.moved.notify_all();
    }

    pub fn held(&self) -> bool {
        self.state().held
    }

    /// Frames that waited for a UI pass, and the longest wait.
    pub fn waits(&self) -> (u64, Duration) {
        let s = self.state();
        (s.waits, s.longest)
    }

    /// The export, right before it renders a frame: at once while not
    /// held; while held, once the UI has passed since the frame before
    /// (`last` holds the pass it went at), or after [`AT_MOST`]. Never
    /// on the UI thread.
    pub fn wait(&self, last: &mut u64) -> Duration {
        spherecut_core::blocking::may_block("an export waiting for the UI's pass");
        let start = Instant::now();
        let mut s = self.state();
        if s.held && s.passes == *last {
            let until = start + AT_MOST;
            while s.held && s.passes == *last {
                let now = Instant::now();
                if now >= until {
                    break;
                }
                s = self
                    .moved
                    .wait_timeout(s, until - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            let waited = start.elapsed();
            s.waits += 1;
            s.longest = s.longest.max(waited);
        }
        *last = s.passes;
        start.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_free_export_never_waits_and_a_held_one_waits_for_a_pass() {
        let p = Arc::new(Pace::default());
        let mut last = 0;
        assert!(p.wait(&mut last) < Duration::from_millis(20));
        assert!(p.wait(&mut last) < Duration::from_millis(20));
        assert_eq!(p.waits().0, 0);
        p.pass(true);
        // A pass since the last frame: the next goes at once.
        assert!(p.wait(&mut last) < Duration::from_millis(20));
        // None since: it waits for the next.
        let q = p.clone();
        let t = std::thread::spawn(move || {
            let start = Instant::now();
            q.wait(&mut last);
            start.elapsed()
        });
        std::thread::sleep(Duration::from_millis(30));
        p.pass(true);
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited < AT_MOST, "{waited:?}");
        assert_eq!(p.waits().0, 1);
    }

    #[test]
    fn a_ui_that_stops_passing_holds_a_frame_at_most_100_ms() {
        let p = Pace::default();
        p.pass(true);
        let mut last = 1;
        let w = p.wait(&mut last);
        assert!(w >= AT_MOST && w < AT_MOST * 3, "{w:?}");
    }
}
