//! Diagnostics for odin's late UI passes (branch diag/late-ui, not for
//! main): switches read once from `SPHERECUT_DIAG` (a comma list), the
//! UI pass's marks, and the upload thread's calls.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

pub const ENV: &str = "SPHERECUT_DIAG";

static WORDS: OnceLock<Vec<String>> = OnceLock::new();

/// The switches `SPHERECUT_DIAG` names.
pub fn words() -> &'static [String] {
    WORDS.get_or_init(|| {
        std::env::var(ENV)
            .unwrap_or_default()
            .split(',')
            .map(|w| w.trim().to_owned())
            .filter(|w| !w.is_empty())
            .collect()
    })
}

/// Whether `SPHERECUT_DIAG` names `word`.
pub fn has(word: &str) -> bool {
    words().iter().any(|w| w == word)
}

/// A point in a UI pass.
#[derive(Debug, Clone, Copy)]
pub enum Mark {
    /// eframe's `logic`, at the pass's start.
    Update = 0,
    /// The viewer's `prepare`, before the surface is acquired.
    Prepare = 1,
    /// The viewer's `paint`, after it is acquired.
    Paint = 2,
}

static MARKS: Mutex<[Option<Instant>; 3]> = Mutex::new([None; 3]);
static PASSES: Mutex<u64> = Mutex::new(0);
static PASSED: Condvar = Condvar::new();
static CALLS: Mutex<VecDeque<(&'static str, Instant, f32)>> = Mutex::new(VecDeque::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn mark(m: Mark) {
    let now = Instant::now();
    lock(&MARKS)[m as usize] = Some(now);
    if matches!(m, Mark::Update) {
        *lock(&PASSES) += 1;
        PASSED.notify_all();
    }
}

/// The latest of each mark, by [`Mark`]'s order.
pub fn marks() -> [Option<Instant>; 3] {
    *lock(&MARKS)
}

/// Waits for the next UI pass to start, `timeout` at most.
pub fn wait_next_pass(timeout: Duration) {
    let n = lock(&PASSES);
    let at = *n;
    let _ = PASSED.wait_timeout_while(n, timeout, |n| *n == at);
}

/// An upload-thread call `what` that began at `start` and ends now.
pub fn note_call(what: &'static str, start: Instant) {
    let ms = start.elapsed().as_secs_f32() * 1e3;
    let mut c = lock(&CALLS);
    if c.len() >= 512 {
        c.pop_front();
    }
    c.push_back((what, start, ms));
}

/// The calls that overlap `from..to`: what, start in ms after `from`
/// (negative before it), and how long.
pub fn calls_between(from: Instant, to: Instant) -> Vec<(&'static str, f32, f32)> {
    lock(&CALLS)
        .iter()
        .filter(|(_, s, ms)| *s < to && *s + Duration::from_secs_f32(ms / 1e3) > from)
        .map(|&(w, s, ms)| {
            let off = if s >= from {
                s.duration_since(from).as_secs_f32() * 1e3
            } else {
                -(from.duration_since(s).as_secs_f32() * 1e3)
            };
            (w, off, ms)
        })
        .collect()
}
