//! An export's pace while the UI is busy (M2 plan amendment 2 item 22).
//!
//! An export from the app shares the GPU with the viewer: its own device
//! on the same adapter (D56), so the same shader cores and the same
//! video engine, which decodes the viewer's frames and the export's and
//! encodes the export's. While the viewer plays, the export stands
//! aside: each of its stages (decode, render, encode) starts no frame
//! until the viewer stops, so the engine and the cores are the
//! viewer's. While a drag runs, this keeps the export to one frame a UI
//! pass, and keeps each stage from starting a frame while the viewer
//! has no frame in hand beyond the one on screen. Otherwise the export
//! runs as fast as it can.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The longest a held export waits for the UI's next pass: a window
/// that stops drawing (minimised, occluded) doesn't stop the export, and
/// a UI that stops passing doesn't keep it standing aside.
pub const AT_MOST: Duration = Duration::from_millis(100);

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
    /// The viewer plays: the stages stand aside.
    playing: bool,
    /// When the UI last passed.
    last_pass: Option<Instant>,
    /// Frames that stood aside, and the time they stood aside in all.
    aside: u64,
    aside_for: Duration,
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
            playing: false,
            last_pass: None,
            aside: 0,
            aside_for: Duration::ZERO,
        }
    }
}

impl State {
    /// A stage must wait: held, and the viewer without a lead.
    fn yielding(&self) -> bool {
        self.held && !self.lead
    }

    /// Until when a stage stands aside, as things are at `now`: while the
    /// viewer plays, until [`AT_MOST`] past the UI's last pass.
    fn aside_until(&self, now: Instant) -> Option<Instant> {
        let until = self.last_pass? + AT_MOST;
        (self.playing && now < until).then_some(until)
    }
}

impl Pace {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The UI, once a pass: whether the export is held to a frame a pass
    /// now (the viewer plays, a drag runs), whether the viewer has frames
    /// in hand beyond the one it shows, and whether it plays.
    pub fn pass(&self, held: bool, lead: bool, playing: bool) {
        let mut s = self.state();
        s.held = held;
        s.lead = lead;
        s.playing = playing;
        s.last_pass = Some(Instant::now());
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

    /// Frames that stood aside while the viewer played, and the time they
    /// stood aside in all.
    pub fn aside(&self) -> (u64, Duration) {
        let s = self.state();
        (s.aside, s.aside_for)
    }

    /// Each of the export's stages, right before each frame's work, ahead
    /// of the pace's other waits: while the viewer plays, until it stops,
    /// `stopped` says the export stops, or the UI hasn't passed for
    /// [`AT_MOST`]; at once otherwise. `stopped` is asked at each of the
    /// UI's passes. Never on the UI thread.
    pub fn stand_aside(&self, stopped: impl Fn() -> bool) -> Duration {
        spherecut_core::blocking::may_block("an export standing aside while the viewer plays");
        let start = Instant::now();
        let mut s = self.state();
        if s.aside_until(start).is_none() {
            return Duration::ZERO;
        }
        while let Some(until) = s.aside_until(Instant::now()) {
            if stopped() {
                break;
            }
            let now = Instant::now();
            s = self
                .moved
                .wait_timeout(s, until.saturating_duration_since(now))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let waited = start.elapsed();
        s.aside += 1;
        s.aside_for += waited;
        waited
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
        p.pass(true, true, false);
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
        p.pass(true, true, false);
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited < AT_MOST, "{waited:?}");
        assert_eq!(p.waits().0, 1);
    }

    #[test]
    fn a_ui_that_stops_passing_holds_a_frame_at_most_100_ms() {
        let p = Pace::default();
        p.pass(true, true, false);
        let mut last = 1;
        let w = p.wait(&mut last);
        assert!(w >= AT_MOST && w < AT_MOST * 3, "{w:?}");
    }

    #[test]
    fn the_stages_wait_for_the_viewers_lead_while_held() {
        let p = Arc::new(Pace::default());
        // Not held: no lead is needed.
        p.pass(false, false, false);
        assert!(p.yield_to_viewer() < Duration::from_millis(20));
        assert_eq!(p.yields().0, 0);
        // Held with a lead: at once.
        p.pass(true, true, false);
        assert!(p.yield_to_viewer() < Duration::from_millis(20));
        assert_eq!(p.yields().0, 0);
        // Held without: until the viewer has one again.
        p.pass(true, false, false);
        let q = p.clone();
        let t = std::thread::spawn(move || q.yield_to_viewer());
        std::thread::sleep(Duration::from_millis(30));
        p.pass(true, true, false);
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(25), "{waited:?}");
        assert!(waited < AT_MOST, "{waited:?}");
        assert_eq!(p.yields().0, 1);
        // A render after a pass waits for the lead too, and both waits
        // together are at most AT_MOST.
        p.pass(true, false, false);
        let mut last = 0;
        let w = p.wait(&mut last);
        assert!(w >= AT_MOST && w < AT_MOST * 3, "{w:?}");
        assert_eq!(p.yields().0, 2);
        // A viewer that never gets its lead holds a stage AT_MOST.
        let w = p.yield_to_viewer();
        assert!(w >= AT_MOST && w < AT_MOST * 3, "{w:?}");
    }

    #[test]
    fn while_the_viewer_plays_the_stages_stand_aside_until_it_stops() {
        let p = Arc::new(Pace::default());
        // Not playing, a drag held or not: never.
        p.pass(true, true, false);
        assert!(p.stand_aside(|| false) < Duration::from_millis(20));
        p.pass(false, true, false);
        assert!(p.stand_aside(|| false) < Duration::from_millis(20));
        assert_eq!(p.aside().0, 0);
        // Playing: for as long as it plays, past AT_MOST while it passes.
        p.pass(true, true, true);
        let q = p.clone();
        let t = std::thread::spawn(move || q.stand_aside(|| false));
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(20));
            p.pass(true, true, true);
        }
        p.pass(true, true, false);
        let waited = t.join().unwrap();
        assert!(waited >= Duration::from_millis(180), "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");
        assert_eq!(p.aside().0, 1);
        assert!(p.aside().1 >= Duration::from_millis(180));
    }

    #[test]
    fn a_stage_goes_when_the_export_stops_or_the_ui_stops_passing() {
        let p = Arc::new(Pace::default());
        // A UI that stops passing holds a stage AT_MOST past its last
        // pass, and none after that until it passes again.
        p.pass(true, true, true);
        let w = p.stand_aside(|| false);
        assert!(w >= AT_MOST / 2 && w < AT_MOST * 3, "{w:?}");
        assert!(p.stand_aside(|| false) < Duration::from_millis(20));
        // An export that stops goes at the UI's next pass.
        p.pass(true, true, true);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (q, st) = (p.clone(), stop.clone());
        let t = std::thread::spawn(move || {
            q.stand_aside(|| st.load(std::sync::atomic::Ordering::Relaxed))
        });
        std::thread::sleep(Duration::from_millis(30));
        p.pass(true, true, true);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        p.pass(true, true, true);
        let waited = t.join().unwrap();
        assert!(waited < AT_MOST, "{waited:?}");
    }
}
