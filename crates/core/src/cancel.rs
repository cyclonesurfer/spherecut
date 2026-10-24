//! Stopping work that a newer request has made pointless, or that the
//! program no longer wants at all.
//!
//! A [`StopSource`] hands out [`StopToken`]s, one generation at a time.
//! [`StopSource::next`] starts a new generation, and every token of an
//! earlier one reads as stopped from then on: a newer seek stops an older
//! one at its next packet or frame boundary, which leaves the decoder in
//! a state it can seek again from. [`StopSource::shutdown`] stops every
//! token, earlier and later ones alike.
//!
//! The decoders' own interrupt flag, which FFmpeg checks inside a read,
//! is for shutdown only: an interrupted read ends the stream for good
//! (DECISIONS D13, D29). [`StopSource::shutdown_flag`] is the flag to
//! give them, so that a shutdown reaches both.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Hands out [`StopToken`]s and stops them. Clones share the generation
/// and the shutdown flag.
#[derive(Debug, Clone, Default)]
pub struct StopSource {
    generation: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
}

/// Whether the work it was handed out for should stop: a newer
/// generation has started, or the source was shut down.
#[derive(Debug, Clone)]
pub struct StopToken {
    generation: Arc<AtomicU64>,
    mine: u64,
    shutdown: Arc<AtomicBool>,
}

impl StopSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new generation, which stops every token handed out so far,
    /// and return a token for it.
    pub fn next(&self) -> StopToken {
        let mine = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.token(mine)
    }

    /// A token for the current generation, which stops with it.
    pub fn current(&self) -> StopToken {
        self.token(self.generation.load(Ordering::Acquire))
    }

    fn token(&self, mine: u64) -> StopToken {
        StopToken {
            generation: self.generation.clone(),
            mine,
            shutdown: self.shutdown.clone(),
        }
    }

    /// Stop every token, of every generation, now and from now on.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    pub fn is_shut_down(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// The flag [`shutdown`](Self::shutdown) sets, for a decoder's
    /// interrupt callback (`VideoRequest::interrupt` in spherecut-media).
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }
}

impl StopToken {
    /// A token that never stops, for work nobody will cancel.
    pub fn never() -> Self {
        StopSource::new().current()
    }

    pub fn is_stopped(&self) -> bool {
        self.generation.load(Ordering::Acquire) != self.mine
            || self.shutdown.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_generation_stops_the_older_ones() {
        let s = StopSource::new();
        let first = s.next();
        let also_first = s.current();
        assert!(!first.is_stopped() && !also_first.is_stopped());
        let second = s.next();
        assert!(first.is_stopped() && also_first.is_stopped());
        assert!(!second.is_stopped());
        // A clone of the source starts generations for the same tokens.
        let third = s.clone().next();
        assert!(second.is_stopped() && !third.is_stopped());
        // A clone of a token is of its generation.
        assert!(!third.clone().is_stopped());
    }

    #[test]
    fn shutdown_stops_every_token_and_raises_the_flag() {
        let s = StopSource::new();
        let a = s.next();
        let flag = s.shutdown_flag();
        assert!(!flag.load(Ordering::Acquire) && !s.is_shut_down());
        s.shutdown();
        assert!(a.is_stopped() && s.is_shut_down());
        assert!(flag.load(Ordering::Acquire));
        // Tokens handed out after the shutdown are stopped too.
        assert!(s.next().is_stopped());
        assert!(s.current().is_stopped());
    }

    #[test]
    fn a_token_that_never_stops() {
        let t = StopToken::never();
        assert!(!t.is_stopped());
        // Other sources don't reach it.
        StopSource::new().next();
        assert!(!t.is_stopped());
    }

    #[test]
    fn tokens_stop_across_threads() {
        let s = StopSource::new();
        let t = s.next();
        let s2 = s.clone();
        std::thread::spawn(move || s2.next()).join().unwrap();
        assert!(t.is_stopped());
        fn send_sync<T: Send + Sync>() {}
        send_sync::<StopSource>();
        send_sync::<StopToken>();
    }
}
