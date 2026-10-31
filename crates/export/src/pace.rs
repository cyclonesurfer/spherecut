//! An export's pace while the UI is busy (M2 plan amendment 2 item 22).
//!
//! An export from the app shares the GPU with the viewer: its own device
//! on the same adapter (D56), so the same shader cores and the same
//! video engine, which decodes the viewer's frames and the export's and
//! encodes the export's. While the viewer plays or a drag runs, this
//! keeps the export to one frame a UI pass, and keeps each of its stages
//! (decode, render, encode) from starting a frame while the viewer has
//! no frame in hand beyond the one on screen: the viewer's decodes go
//! first, and the export takes what they leave. While the viewer plays,
//! the export's decode and encode also take turns with it: each starts
//! a frame at most [`SHARE`] times for each frame the viewer shows, so
//! the video engine keeps room for the viewer's decodes. Otherwise the
//! export runs as fast as it can.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The longest a held export waits for the UI's next pass: a window
/// that stops drawing (minimised, occluded) doesn't stop the export.
pub const AT_MOST: Duration = Duration::from_millis(100);

/// While the viewer plays, the export's decode and encode each start a
/// frame at most this many times for each frame the viewer shows: odin's
/// runs had the export decoding 5.7K at 37 fps beside the viewer's 30,
/// and the viewer's decodes waiting 38 to 77 ms for the engine (D56).
pub const SHARE: f64 = 0.5;

/// The UI's side and the export's: shared through an `Arc`.
#[derive(Debug, Default)]
pub struct Pace {
    state: Mutex<State>,
    moved: Condvar,
}

#[derive(Debug)]
struct State {
    held: bool,
    /// The viewer has frames in hand beyond the one it shows: the export
    /// may start a frame's work. Always while not held.
    lead: bool,
    /// The UI's passes so far.
    passes: u64,
    /// Frames that waited for a pass, and the longest wait.
    waits: u64,
    longest: Duration,
    /// Times a stage waited for the viewer's lead, and the longest wait.
    yields: u64,
    longest_yield: Duration,
    /// While the viewer plays, the least time between the starts of two
    /// of a stage's frames.
    turn: Option<Duration>,
    /// Frames that waited their turn, and the time they waited in all.
    slowed: u64,
    slowed_for: Duration,
}

impl Default for State {
    fn default() -> Self {
        State {
            held: false,
            lead: true,
            passes: 0,
            waits: 0,
            longest: Duration::ZERO,
            yields: 0,
            longest_yield: Duration::ZERO,
            turn: None,
            slowed: 0,
            slowed_for: Duration::ZERO,
        }
    }
}

impl State {
    /// A stage must wait: held, and the viewer without a lead.
    fn yielding(&self) -> bool {
        self.held && !self.lead
    }
}

impl Pace {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The UI, once a pass: whether the export is held to a frame a pass
    /// now (the viewer plays, a drag runs), whether the viewer has frames
    /// in hand beyond the one it shows, and, while it plays, how long its
    /// frames are.
    pub fn pass(&self, held: bool, lead: bool, playing: Option<Duration>) {
        let mut s = self.state();
        s.held = held;
        s.lead = lead;
        s.turn = playing
            .filter(|f| held && !f.is_zero())
            .map(|f| f.div_f64(SHARE));
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

    /// Times a stage waited for the viewer's lead, and the longest wait.
    pub fn yields(&self) -> (u64, Duration) {
        let s = self.state();
        (s.yields, s.longest_yield)
    }

    /// Frames that waited their turn, and the time they waited in all.
    pub fn slowed(&self) -> (u64, Duration) {
        let s = self.state();
        (s.slowed, s.slowed_for)
    }

    /// The export's decode and encode, right before each frame's work,
    /// after [`yield_to_viewer`](Self::yield_to_viewer): while the viewer
    /// plays, a turn after the stage's frame before (`last` holds when
    /// that one went), or at once when the viewer stops; at once
    /// otherwise. Never on the UI thread.
    pub fn take_turn(&self, last: &mut Option<Instant>) -> Duration {
        spherecut_core::blocking::may_block("an export taking turns with the viewer");
        let start = Instant::now();
        let mut s = self.state();
        if let (Some(turn), Some(prev)) = (s.turn, *last) {
            let due = prev + turn;
            if due > start {
                s = self.wait_while(s, due, |s| {
                    s.turn.is_some_and(|t| Instant::now() < prev + t)
                });
                s.slowed += 1;
                s.slowed_for += start.elapsed();
            }
        }
        drop(s);
        let now = Instant::now();
        *last = Some(now);
        now - start
    }

    /// The export, right before it renders a frame: at once while not
    /// held; while held, once the UI has passed since the frame before
    /// (`last` holds the pass it went at) and the viewer has its lead,
    /// or after [`AT_MOST`]. Never on the UI thread.
    pub fn wait(&self, last: &mut u64) -> Duration {
        spherecut_core::blocking::may_block("an export waiting for the UI's pass");
        let start = Instant::now();
        let until = start + AT_MOST;
        let mut s = self.state();
        if s.held && s.passes == *last {
            s = self.wait_while(s, until, |s| s.held && s.passes == *last);
            let waited = start.elapsed();
            s.waits += 1;
            s.longest = s.longest.max(waited);
        }
        if s.yielding() {
            s = self.yield_in(s, until);
        }
        *last = s.passes;
        start.elapsed()
    }

    /// The export's decode and encode stages, right before each frame's
    /// work: at once while not held or while the viewer has its lead;
    /// otherwise once it has, or after [`AT_MOST`]. Never on the UI
    /// thread.
    pub fn yield_to_viewer(&self) -> Duration {
        spherecut_core::blocking::may_block("an export waiting for the viewer's frames");
        let start = Instant::now();
        let s = self.state();
        if s.yielding() {
            drop(self.yield_in(s, start + AT_MOST));
        }
        start.elapsed()
    }

    /// Wait while the viewer has no lead, until `until` at the latest,
    /// and count it.
    fn yield_in<'a>(&'a self, s: MutexGuard<'a, State>, until: Instant) -> MutexGuard<'a, State> {
        let start = Instant::now();
        let mut s = self.wait_while(s, until, State::yielding);
        let waited = start.elapsed();
        s.yields += 1;
        s.longest_yield = s.longest_yield.max(waited);
        s
    }

    fn wait_while<'a>(
        &'a self,
        mut s: MutexGuard<'a, State>,
        until: Instant,
        mut what: impl FnMut(&State) -> bool,
    ) -> MutexGuard<'a, State> {
        while what(&s) {
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
        s
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
        p.pass(true, true, None);
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
        p.pass(true, true, None);
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited < AT_MOST, "{waited:?}");
        assert_eq!(p.waits().0, 1);
    }

    #[test]
    fn a_ui_that_stops_passing_holds_a_frame_at_most_100_ms() {
        let p = Pace::default();
        p.pass(true, true, None);
        let mut last = 1;
        let w = p.wait(&mut last);
        assert!(w >= AT_MOST && w < AT_MOST * 3, "{w:?}");
    }

    #[test]
    fn the_stages_wait_for_the_viewers_lead_while_held() {
        let p = Arc::new(Pace::default());
        // Not held: no lead is needed.
        p.pass(false, false, None);
        assert!(p.yield_to_viewer() < Duration::from_millis(20));
        assert_eq!(p.yields().0, 0);
        // Held with a lead: at once.
        p.pass(true, true, None);
        assert!(p.yield_to_viewer() < Duration::from_millis(20));
        assert_eq!(p.yields().0, 0);
        // Held without: until the viewer has one again.
        p.pass(true, false, None);
        let q = p.clone();
        let t = std::thread::spawn(move || q.yield_to_viewer());
        std::thread::sleep(Duration::from_millis(30));
        p.pass(true, true, None);
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited < AT_MOST, "{waited:?}");
        assert_eq!(p.yields().0, 1);
        // A render after a pass waits for the lead too, and both waits
        // together are at most AT_MOST.
        p.pass(true, false, None);
        let mut last = 0;
        let w = p.wait(&mut last);
        assert!(w >= AT_MOST && w < AT_MOST * 3, "{w:?}");
        assert_eq!(p.yields().0, 2);
        // A viewer that never gets its lead holds a stage AT_MOST.
        let w = p.yield_to_viewer();
        assert!(w >= AT_MOST && w < AT_MOST * 3, "{w:?}");
    }

    #[test]
    fn while_the_viewer_plays_a_stage_takes_its_turn() {
        let p = Arc::new(Pace::default());
        let mut last = None;
        // Not playing: never slowed.
        p.pass(true, true, None);
        assert!(p.take_turn(&mut last) < Duration::from_millis(20));
        assert!(p.take_turn(&mut last) < Duration::from_millis(20));
        assert_eq!(p.slowed().0, 0);
        // Playing 20 ms frames: a turn of 40 ms between two frames.
        p.pass(true, true, Some(Duration::from_millis(20)));
        let start = Instant::now();
        p.take_turn(&mut last);
        p.take_turn(&mut last);
        let both = start.elapsed();
        assert!(both >= Duration::from_millis(35), "{both:?}");
        assert!(both < Duration::from_millis(120), "{both:?}");
        assert!(p.slowed().0 >= 1);
        // A stage waiting a long turn goes once the viewer stops.
        p.pass(true, true, Some(Duration::from_secs(5)));
        let q = p.clone();
        let mut l = Some(Instant::now());
        let t = std::thread::spawn(move || q.take_turn(&mut l));
        std::thread::sleep(Duration::from_millis(30));
        p.pass(false, true, Some(Duration::from_secs(5)));
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited < Duration::from_secs(1), "{waited:?}");
    }
}
