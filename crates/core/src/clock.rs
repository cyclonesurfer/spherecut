//! The clocks playback runs on (M1 plan sections 2.2, 8.2 and 9.3).
//!
//! A [`Clock`] says which media time it is at a given instant: the
//! presenter shows the newest frame whose time is at or before
//! `clock.time_at(now + latency)`. The engine starts, stops and sets it
//! (play, pause, seek). Two clocks are here:
//!
//! * [`SystemClock`] runs on the monotonic system clock, for video
//!   without audio, and for the time after an audio device is lost or
//!   stalls (the audio clock is slice 6's).
//! * [`ManualClock`] moves only when told to, by a time or a number of
//!   frames: for tests, and for M2's export, which renders frame after
//!   frame as fast as it can.
//!
//! Times are exact (DECISIONS D6): a clock set to a frame's time shows
//! that time exactly, and [`SystemClock`] adds the nanoseconds since its
//! anchor as a fraction of a second.
//!
//! Reading a clock never blocks. The UI thread reads it every frame, so
//! its state sits in a sequence lock of atomics: a reader retries while a
//! write is under way, which takes nanoseconds, and nothing ever waits on
//! a mutex.

use std::fmt;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering, fence};
use std::time::Instant;

use crate::time::{Rational, Rounding, Time};

/// Which media time it is. Every method returns at once, from any
/// thread.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The media time at the instant `at`.
    fn time_at(&self, at: Instant) -> Time;
    /// Whether the time moves.
    fn is_running(&self) -> bool;
    /// Start moving, from the time shown at `at`.
    fn play(&self, at: Instant);
    /// Stop, keeping the time shown at `at`.
    fn pause(&self, at: Instant);
    /// Show `time` from the instant `at` on (a seek), running or paused as
    /// before.
    fn set(&self, time: Time, at: Instant);
}

/// A sequence lock over `N` integers, in safe Rust: writers take turns
/// through the sequence number (odd while one writes), and readers retry
/// until they read the integers between two equal, even sequence
/// numbers. Neither ever sleeps, so a real-time audio callback may use
/// one (`spherecut-audio`'s clock).
pub struct SeqLock<const N: usize> {
    seq: AtomicU64,
    data: [AtomicI64; N],
}

impl<const N: usize> SeqLock<N> {
    pub fn new(data: [i64; N]) -> Self {
        SeqLock {
            seq: AtomicU64::new(0),
            data: data.map(AtomicI64::new),
        }
    }

    /// The integers, as one write left them.
    pub fn read(&self) -> [i64; N] {
        let mut spins = 0u32;
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before.is_multiple_of(2) {
                let data = std::array::from_fn(|i| self.data[i].load(Ordering::Relaxed));
                fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == before {
                    return data;
                }
            }
            backoff(&mut spins);
        }
    }

    /// Replace the integers with `f` of them, as one write.
    pub fn update(&self, f: impl FnOnce([i64; N]) -> [i64; N]) {
        let mut spins = 0u32;
        let start = loop {
            let s = self.seq.load(Ordering::Relaxed);
            if s.is_multiple_of(2)
                && self
                    .seq
                    .compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                break s;
            }
            backoff(&mut spins);
        };
        fence(Ordering::Release);
        let old = std::array::from_fn(|i| self.data[i].load(Ordering::Relaxed));
        for (cell, v) in self.data.iter().zip(f(old)) {
            cell.store(v, Ordering::Relaxed);
        }
        self.seq.store(start + 2, Ordering::Release);
    }
}

/// Spin briefly, then yield: another thread is in the middle of a write
/// that takes nanoseconds, unless it was preempted.
fn backoff(spins: &mut u32) {
    *spins += 1;
    if *spins < 64 {
        std::hint::spin_loop();
    } else {
        std::thread::yield_now();
    }
}

impl<const N: usize> fmt::Debug for SeqLock<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.read()).finish()
    }
}

const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// `t` plus `nanos` nanoseconds: exactly, unless the sum's denominator
/// doesn't fit, when `t` is first rounded down to a nanosecond.
pub fn plus_nanos(t: Time, nanos: i64) -> Time {
    let ns = Time(Rational::new(nanos, NANOS_PER_SECOND).unwrap_or(Rational::ZERO));
    t.checked_add(ns).unwrap_or_else(|_| {
        let tick = Rational::new(1, NANOS_PER_SECOND).unwrap_or(Rational::ONE);
        t.to_pts(tick, Rounding::Floor)
            .ok()
            .and_then(|n| n.checked_add(nanos))
            .and_then(|n| Rational::new(n, NANOS_PER_SECOND).ok())
            .map_or(t, Time)
    })
}

fn time_of(num: i64, den: i64) -> Time {
    Time(Rational::new(num, den).unwrap_or(Rational::ZERO))
}

/// Runs on the monotonic system clock while playing.
///
/// Its state is an anchor, the media time it showed at an instant, and
/// whether it runs: running, it shows the anchor's time plus the time
/// since the anchor's instant (never less than the anchor's time, for an
/// instant before it); paused, the anchor's time.
pub struct SystemClock {
    /// Instants are kept as signed nanoseconds from this one.
    base: Instant,
    /// The anchor's time (numerator, denominator), its instant, and 1
    /// while running.
    state: SeqLock<4>,
}

impl SystemClock {
    /// Paused at 0.
    pub fn new() -> Self {
        Self::at(Time::ZERO)
    }

    /// Paused at `time`.
    pub fn at(time: Time) -> Self {
        SystemClock {
            base: Instant::now(),
            state: SeqLock::new([time.0.num(), time.0.den(), 0, 0]),
        }
    }

    fn nanos(&self, at: Instant) -> i64 {
        let ns = |d: std::time::Duration| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX);
        match at.checked_duration_since(self.base) {
            Some(d) => ns(d),
            None => -ns(self.base.duration_since(at)),
        }
    }

    /// The time shown at the instant `now` (nanoseconds from `base`), in
    /// the state `s`.
    fn shown(s: [i64; 4], now: i64) -> Time {
        let [num, den, anchor, running] = s;
        let t = time_of(num, den);
        if running == 0 {
            return t;
        }
        plus_nanos(t, now.saturating_sub(anchor).max(0))
    }

    /// Set the anchor to the time shown at `at` (or `time`), running or
    /// not.
    fn anchor(&self, at: Instant, time: Option<Time>, running: Option<bool>) {
        let now = self.nanos(at);
        self.state.update(|s| {
            let t = time.unwrap_or_else(|| Self::shown(s, now));
            let running = running.map_or(s[3], i64::from);
            [t.0.num(), t.0.den(), now, running]
        });
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn time_at(&self, at: Instant) -> Time {
        Self::shown(self.state.read(), self.nanos(at))
    }

    fn is_running(&self) -> bool {
        self.state.read()[3] != 0
    }

    fn play(&self, at: Instant) {
        self.anchor(at, None, Some(true));
    }

    fn pause(&self, at: Instant) {
        self.anchor(at, None, Some(false));
    }

    fn set(&self, time: Time, at: Instant) {
        self.anchor(at, Some(time), None);
    }
}

impl fmt::Debug for SystemClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [num, den, _, running] = self.state.read();
        f.debug_struct("SystemClock")
            .field("anchor", &time_of(num, den))
            .field("running", &(running != 0))
            .finish_non_exhaustive()
    }
}

/// Moves only when told to: by [`advance`](ManualClock::advance), or by
/// whole frames with [`step`](ManualClock::step), and only while it runs,
/// as a clock that runs does. Instants are ignored. For tests, and for
/// M2's export, which renders each frame as fast as it can.
pub struct ManualClock {
    /// One frame, for [`step`](ManualClock::step).
    frame: Time,
    /// The time (numerator, denominator), and 1 while running.
    state: SeqLock<3>,
}

impl ManualClock {
    /// Paused at 0, stepping by `frame` (a frame's duration).
    pub fn new(frame: Time) -> Self {
        ManualClock {
            frame,
            state: SeqLock::new([0, 1, 0]),
        }
    }

    /// The duration [`step`](ManualClock::step) moves by, per frame.
    pub fn frame(&self) -> Time {
        self.frame
    }

    /// The time now: the clock ignores instants.
    pub fn now(&self) -> Time {
        let [num, den, _] = self.state.read();
        time_of(num, den)
    }

    /// Move on by `by` if it runs, and return the time then. A sum past
    /// what a [`Time`] holds leaves the time where it was.
    pub fn advance(&self, by: Time) -> Time {
        let mut after = Time::ZERO;
        self.state.update(|[num, den, running]| {
            let t = time_of(num, den);
            after = if running != 0 {
                t.checked_add(by).unwrap_or(t)
            } else {
                t
            };
            [after.0.num(), after.0.den(), running]
        });
        after
    }

    /// Move on by `frames` frames if it runs (back, for a negative
    /// count), and return the time then.
    pub fn step(&self, frames: i64) -> Time {
        match Rational::from_integer(frames).checked_mul(self.frame.0) {
            Ok(by) => self.advance(Time(by)),
            Err(_) => self.now(),
        }
    }

    fn put(&self, time: Option<Time>, running: Option<bool>) {
        self.state.update(|[num, den, was]| {
            let t = time.unwrap_or_else(|| time_of(num, den));
            [t.0.num(), t.0.den(), running.map_or(was, i64::from)]
        });
    }
}

impl Clock for ManualClock {
    fn time_at(&self, _at: Instant) -> Time {
        self.now()
    }

    fn is_running(&self) -> bool {
        self.state.read()[2] != 0
    }

    fn play(&self, _at: Instant) {
        self.put(None, Some(true));
    }

    fn pause(&self, _at: Instant) {
        self.put(None, Some(false));
    }

    fn set(&self, time: Time, _at: Instant) {
        self.put(Some(time), None);
    }
}

impl fmt::Debug for ManualClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManualClock")
            .field("time", &self.now())
            .field("running", &self.is_running())
            .field("frame", &self.frame)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use proptest::prelude::*;

    use super::*;
    use crate::time::FrameRate;

    fn t(num: i64, den: i64) -> Time {
        Time(Rational::new(num, den).unwrap())
    }

    #[test]
    fn a_system_clock_runs_only_while_playing() {
        let c = SystemClock::new();
        let t0 = Instant::now();
        assert!(!c.is_running());
        assert_eq!(c.time_at(t0 + Duration::from_secs(5)), Time::ZERO);
        c.play(t0);
        assert!(c.is_running());
        assert_eq!(c.time_at(t0 + Duration::from_millis(1500)), t(3, 2));
        // An instant before the anchor shows the anchor's time.
        assert_eq!(c.time_at(t0 - Duration::from_secs(1)), Time::ZERO);
        c.pause(t0 + Duration::from_secs(2));
        assert!(!c.is_running());
        assert_eq!(c.time_at(t0 + Duration::from_secs(9)), Time::from_secs(2));
        c.play(t0 + Duration::from_secs(10));
        assert_eq!(c.time_at(t0 + Duration::from_secs(11)), Time::from_secs(3));
    }

    #[test]
    fn a_seek_shows_the_time_exactly() {
        // 1001/30000 isn't a whole number of nanoseconds.
        let frame = FrameRate::FPS_29_97.frame_duration();
        let c = SystemClock::new();
        let t0 = Instant::now();
        c.set(frame, t0);
        assert_eq!(c.time_at(t0 + Duration::from_secs(1)), frame);
        c.play(t0);
        assert_eq!(
            c.time_at(t0 + Duration::from_nanos(1)),
            frame.checked_add(t(1, 1_000_000_000)).unwrap()
        );
        // A seek while playing keeps playing from the new time.
        c.set(Time::from_secs(7), t0 + Duration::from_secs(1));
        assert!(c.is_running());
        assert_eq!(c.time_at(t0 + Duration::from_secs(3)), Time::from_secs(9));
        assert_eq!(SystemClock::at(frame).time_at(Instant::now()), frame);
    }

    #[test]
    fn instants_before_the_clock_was_made_work() {
        let early = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
        let c = SystemClock::new();
        c.play(early);
        let shown = c.time_at(early + Duration::from_millis(1));
        assert_eq!(shown, t(1, 1000));
    }

    #[test]
    fn a_sum_past_exact_holds_rounds_to_a_nanosecond() {
        // A denominator that is prime and near i64's limit can't be
        // brought over 10^9 exactly.
        let odd = t(1, 9_223_372_036_854_775_783);
        let sum = plus_nanos(odd, 5);
        assert_eq!(sum, t(5, 1_000_000_000));
        assert_eq!(plus_nanos(Time::from_secs(1), 0), Time::from_secs(1));
    }

    #[test]
    fn a_manual_clock_moves_only_when_told_and_running() {
        let c = ManualClock::new(FrameRate::FPS_29_97.frame_duration());
        assert_eq!(c.step(3), Time::ZERO, "paused");
        c.play(Instant::now());
        assert_eq!(c.step(3), t(3003, 30000));
        assert_eq!(
            c.advance(t(1, 2)),
            t(3003, 30000).checked_add(t(1, 2)).unwrap()
        );
        assert_eq!(c.step(-3), t(1, 2));
        c.pause(Instant::now());
        assert_eq!(c.advance(Time::from_secs(1)), t(1, 2));
        c.set(Time::from_secs(4), Instant::now());
        assert_eq!(
            c.time_at(Instant::now() + Duration::from_secs(60)),
            Time::from_secs(4)
        );
        assert!(!c.is_running());
        // Past what a time holds, it stays put.
        c.play(Instant::now());
        c.set(Time(Rational::from_integer(i64::MAX)), Instant::now());
        assert_eq!(c.step(1), Time(Rational::from_integer(i64::MAX)));
        assert_eq!(c.step(i64::MAX), Time(Rational::from_integer(i64::MAX)));
    }

    #[test]
    fn frame_steps_land_on_frame_times() {
        let rate = FrameRate::FPS_59_94;
        let c = ManualClock::new(rate.frame_duration());
        c.play(Instant::now());
        for k in 1..=600 {
            assert_eq!(c.step(1), rate.frame_to_time(k).unwrap());
        }
    }

    #[test]
    fn readers_never_see_a_torn_write() {
        // Writers move between times whose numerator and denominator are
        // tied (n/(n+1)); a reader that saw one half of a write would see
        // a pair that isn't.
        let c = Arc::new(ManualClock::new(Time::from_secs(1)));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writers: Vec<_> = (0..2)
            .map(|w| {
                let (c, stop) = (c.clone(), stop.clone());
                std::thread::spawn(move || {
                    let mut n = 1 + w;
                    while !stop.load(Ordering::Relaxed) {
                        c.set(t(n, n + 1), Instant::now());
                        n = n % 1_000_000 + 2;
                    }
                })
            })
            .collect();
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut reads = 0u64;
        while Instant::now() < deadline {
            let r = c.now().0;
            assert_eq!(r.den(), r.num() + 1, "{r:?}");
            reads += 1;
        }
        stop.store(true, Ordering::Relaxed);
        for w in writers {
            w.join().unwrap();
        }
        assert!(reads > 1000, "{reads}");
    }

    #[test]
    fn clocks_are_shared_across_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<SystemClock>();
        send_sync::<ManualClock>();
        send_sync::<Arc<dyn Clock>>();
        let c: Arc<dyn Clock> = Arc::new(SystemClock::new());
        let c2 = c.clone();
        std::thread::spawn(move || c2.play(Instant::now()))
            .join()
            .unwrap();
        assert!(c.is_running());
        assert!(format!("{c:?}").contains("running: true"));
    }

    proptest! {
        /// Whatever the plays, pauses and seeks, the clock shows the time
        /// the anchors say, exactly: the last seek's time plus the time
        /// it ran since.
        #[test]
        fn a_system_clock_adds_up(ops in proptest::collection::vec((0u8..3, 0u64..5_000_000_000, 0i64..100_000, 1i64..90_000), 1..40)) {
            let c = SystemClock::new();
            let t0 = Instant::now();
            let mut at = 0u64;
            let mut shown = Time::ZERO;
            let mut running = false;
            for (op, gap, num, den) in ops {
                let next = at + gap;
                if running {
                    shown = shown.checked_add(t(i64::try_from(gap).unwrap(), 1_000_000_000)).unwrap();
                }
                at = next;
                let when = t0 + Duration::from_nanos(at);
                prop_assert_eq!(c.time_at(when), shown);
                match op {
                    0 => { c.play(when); running = true; }
                    1 => { c.pause(when); running = false; }
                    _ => { shown = t(num, den); c.set(shown, when); }
                }
                prop_assert_eq!(c.time_at(when), shown);
                prop_assert_eq!(c.is_running(), running);
            }
        }
    }
}
