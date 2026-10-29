//! The runtime half of "the UI thread never blocks" (M1 plan section
//! 8.6; clippy's per-crate bans are the static half).
//!
//! The GUI calls [`mark_ui_thread`] on its thread once, in eframe's
//! creation callback, and every test harness that stands in for the UI
//! does the same with [`mark_ui_thread_with`]. Every helper in media,
//! render and playback that can block calls [`may_block`] first: taking
//! the frame cache's lock or waiting on it, a channel's `recv`, a
//! thread's `join`, `Device::poll` with a wait, a readback, file I/O. On
//! a marked thread that is a bug: in debug builds (which `cargo test`
//! builds) it panics, naming what would have blocked; in release builds
//! it is counted, and logged once, since a stall is better than a crash
//! in front of the user. On any other thread it costs a thread-local
//! read.
//!
//! The policy goes with the mark, so a test harness asks for
//! [`OnBlock::Panic`] and catches a blocking call in a release build too.

use std::cell::Cell;
use std::panic::Location;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// What [`may_block`] does on a marked thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnBlock {
    /// Panic, naming what would have blocked and where.
    Panic,
    /// Count it, and log the first one.
    Log,
}

impl OnBlock {
    /// [`Panic`](OnBlock::Panic) in builds with debug assertions (debug
    /// builds, and `cargo test`), [`Log`](OnBlock::Log) in release builds.
    pub const fn for_this_build() -> OnBlock {
        if cfg!(debug_assertions) {
            OnBlock::Panic
        } else {
            OnBlock::Log
        }
    }
}

thread_local! {
    static UI_THREAD: Cell<Option<OnBlock>> = const { Cell::new(None) };
}

/// Blocking calls made on a marked thread, since the process started.
static BLOCKED: AtomicU64 = AtomicU64::new(0);
/// Whether one has been logged.
static LOGGED: AtomicBool = AtomicBool::new(false);

/// Mark this thread as the UI thread, with this build's policy
/// ([`OnBlock::for_this_build`]).
pub fn mark_ui_thread() {
    mark_ui_thread_with(OnBlock::for_this_build());
}

/// Mark this thread as the UI thread, with the policy `on`.
pub fn mark_ui_thread_with(on: OnBlock) {
    UI_THREAD.with(|m| m.set(Some(on)));
}

/// Take the mark off this thread, as a test harness does when it is done.
pub fn unmark_ui_thread() {
    UI_THREAD.with(|m| m.set(None));
}

/// Whether this thread is marked as the UI thread.
pub fn is_ui_thread() -> bool {
    UI_THREAD.with(Cell::get).is_some()
}

/// The blocking calls made on a marked thread so far, for the overlay and
/// the smoke run: 0 unless something is wrong.
pub fn blocked_on_ui_thread() -> u64 {
    BLOCKED.load(Ordering::Relaxed)
}

/// Say that what follows can block, as `what` describes it ("waiting for
/// room in the frame cache"). On the UI thread that is a bug, which this
/// reports by its policy (module docs); anywhere else it does nothing.
#[track_caller]
pub fn may_block(what: &str) {
    let Some(on) = UI_THREAD.with(Cell::get) else {
        return;
    };
    BLOCKED.fetch_add(1, Ordering::Relaxed);
    let at = Location::caller();
    match on {
        OnBlock::Panic => panic!("{what} would block the UI thread (at {at})"),
        OnBlock::Log => {
            if !LOGGED.swap(true, Ordering::Relaxed) {
                tracing::error!(
                    what,
                    %at,
                    "a blocking call on the UI thread; later ones are only counted"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `f` on a fresh thread, so that marks never outlive a test
    /// whatever thread the harness runs it on.
    fn on_thread<T: Send + 'static>(
        f: impl FnOnce() -> T + Send + 'static,
    ) -> std::thread::Result<T> {
        std::thread::spawn(f).join()
    }

    #[test]
    fn unmarked_threads_may_block() {
        on_thread(|| {
            assert!(!is_ui_thread());
            may_block("reading a file");
        })
        .unwrap();
    }

    /// The negative control: the guard is live. A marked thread that
    /// calls a blocking helper panics, naming it and its caller.
    #[test]
    fn a_marked_thread_panics_when_it_would_block() {
        let e = on_thread(|| {
            mark_ui_thread_with(OnBlock::Panic);
            assert!(is_ui_thread());
            may_block("waiting for room in the frame cache");
        })
        .unwrap_err();
        let message = e.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(
            message
                .starts_with("waiting for room in the frame cache would block the UI thread (at ")
                && message.contains("blocking.rs"),
            "{message}"
        );
    }

    #[test]
    fn this_builds_policy_panics_in_tests() {
        assert_eq!(OnBlock::for_this_build(), OnBlock::Panic);
        assert!(
            on_thread(|| {
                mark_ui_thread();
                may_block("joining a thread");
            })
            .is_err()
        );
    }

    #[test]
    fn the_mark_is_per_thread_and_comes_off() {
        on_thread(|| {
            mark_ui_thread_with(OnBlock::Panic);
            // Another thread isn't marked by this one's mark.
            std::thread::spawn(|| {
                assert!(!is_ui_thread());
                may_block("a channel recv");
            })
            .join()
            .unwrap();
            unmark_ui_thread();
            assert!(!is_ui_thread());
            may_block("a channel recv");
        })
        .unwrap();
    }

    #[test]
    fn the_log_policy_counts_and_goes_on() {
        let before = blocked_on_ui_thread();
        on_thread(|| {
            mark_ui_thread_with(OnBlock::Log);
            may_block("a readback");
            may_block("a readback");
        })
        .unwrap();
        // Other tests may count too, at the same time.
        assert!(blocked_on_ui_thread() >= before + 2);
        assert!(LOGGED.load(Ordering::Relaxed));
    }
}
