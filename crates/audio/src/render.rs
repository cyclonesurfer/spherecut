//! The output callback's work, the same for the cpal device, the null
//! output and the fake device (M1 plan section 9.2): fill a buffer from
//! the ring by the clock's state and anchor the clock to it.
//!
//! It runs on the real-time thread, so it takes no lock, allocates
//! nothing and logs nothing: the seqlock and the ring are atomics, and
//! its numbers go into [`Shared`]'s counters for `audio-out` to read.
//!
//! **Underruns.** When the ring runs dry while playing, the buffer's
//! rest is silence and the clock runs on regardless: the samples that
//! weren't there are skipped when they come, because the next buffer
//! asks for the sample due then (the ring's placement), so the next
//! sample heard is at the right time. A dry spell counts once, and not
//! at all past the end of the audio.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use spherecut_core::Time;

use crate::clock::{Fallback, Shared, State, nanos_of};
use crate::ring::Fill;

/// An audio run: the epoch's audio playing from `base`, whose sample is
/// `first`.
#[derive(Debug, Clone, Copy)]
struct Run {
    base: Time,
    first: i64,
    /// The next buffer's first sample.
    next: i64,
}

/// What one callback played.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rendered {
    pub epoch: u64,
    /// The buffer's first sample on the clip's axis at the output rate,
    /// when it was audio (perhaps with silence for want of it); `None`
    /// for silence while paused or waiting for audio.
    pub from: Option<i64>,
    pub fill: Fill,
}

/// The callback's state, owned by the thread the callback runs on.
#[derive(Debug)]
pub struct Callback {
    shared: Arc<Shared>,
    epoch: u64,
    run: Option<Run>,
    /// In a dry spell already counted.
    dry: bool,
    last: Option<i64>,
}

/// The latency is taken as at most this: a host reporting more is wrong.
const MAX_LATENCY: Duration = Duration::from_secs(1);

impl Callback {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Callback {
            shared,
            epoch: 0,
            run: None,
            dry: false,
            last: None,
        }
    }

    /// The epoch and sample the next buffer, at `now` and heard
    /// `latency` later, will start with, if it will play audio: for the
    /// fake device, which waits for it.
    pub(crate) fn wants(&self, now: Instant, latency: Duration) -> Option<(u64, i64)> {
        let sh = &self.shared;
        let s = sh.read();
        if !s.running {
            return None;
        }
        if s.system {
            let due = s.shown(sh.nanos(now + latency.min(MAX_LATENCY)));
            return Some((s.epoch, sh.sample_at(due).max(sh.sample_at(s.seek))));
        }
        match self.run {
            Some(run) if s.epoch == self.epoch => Some((s.epoch, run.next)),
            _ if !s.audio => Some((s.epoch, sh.sample_at(s.seek))),
            _ => None,
        }
    }

    /// Fill `out` (interleaved, the output's channels) for the callback
    /// at `now` whose first frame is heard `latency` later.
    pub fn render(&mut self, out: &mut [f32], now: Instant, latency: Duration) -> Rendered {
        let sh = self.shared.clone();
        let ch = usize::from(sh.channels);
        let frames = out.len() / ch;
        let out = &mut out[..frames * ch];
        let at = sh.nanos(now);
        let latency = nanos_of(latency.min(MAX_LATENCY));
        self.note(at, frames, latency);
        let s = sh.read();
        if s.epoch != self.epoch {
            self.epoch = s.epoch;
            self.run = None;
            self.dry = false;
        }
        let silent = Rendered {
            epoch: s.epoch,
            from: None,
            fill: Fill {
                dry: frames,
                ..Fill::default()
            },
        };
        if !s.running {
            out.fill(0.0);
            return silent;
        }
        if s.system {
            return self.resync(out, &s, at, latency).unwrap_or_else(|| {
                out.fill(0.0);
                silent
            });
        }
        let (run, start) = match self.run {
            Some(run) => (run, false),
            None if s.audio => {
                // Anchored by a callback that isn't this one: a stream
                // was rebuilt without the clock leaving the device's.
                // Find the audio again from the system clock.
                sh.to_system(now, Fallback::Lost);
                out.fill(0.0);
                return silent;
            }
            None => {
                let first = sh.sample_at(s.seek);
                (
                    Run {
                        base: s.base,
                        first,
                        next: first,
                    },
                    true,
                )
            }
        };
        let from = run.next;
        let fill = sh.ring.fill(out, from, s.epoch);
        if start && fill.copied + fill.gap == 0 && sh.end_of(s.epoch).is_none() {
            // Waiting: nothing of the epoch yet.
            return silent;
        }
        if sh.anchor(
            s.epoch,
            false,
            run.base,
            from - run.first,
            at,
            latency,
            frames,
        ) {
            self.run = Some(Run {
                next: from + frames as i64,
                ..run
            });
        }
        self.count(&fill, s.epoch, from, frames);
        Rendered {
            epoch: s.epoch,
            from: Some(from),
            fill,
        }
    }

    /// On the system clock: play the sample due when this buffer is
    /// heard if the ring has it, and anchor the clock to it; otherwise
    /// `None` for silence, asking for the audio again if it isn't
    /// coming.
    fn resync(&mut self, out: &mut [f32], s: &State, at: i64, latency: i64) -> Option<Rendered> {
        let sh = self.shared.clone();
        let frames = out.len() / usize::from(sh.channels);
        let due = s.shown(at.saturating_add(latency));
        let from = sh.sample_at(due);
        let fill = sh.ring.fill(out, from, s.epoch);
        let ended = sh.end_of(s.epoch).is_some_and(|end| end <= from);
        if fill.copied == 0 && !ended {
            let behind = from - sh.sample_at(s.seek);
            if fill.gap == 0 && behind > 2 * sh.samples(crate::clock::RESYNC_LEAD) {
                // The epoch's audio should have been here by now.
                sh.resync(s.epoch, due);
            }
            return None;
        }
        if sh.anchor(s.epoch, true, due, 0, at, latency, frames) {
            self.epoch = s.epoch;
            self.run = Some(Run {
                base: due,
                first: from,
                next: from + frames as i64,
            });
        }
        self.count(&fill, s.epoch, from, frames);
        Some(Rendered {
            epoch: s.epoch,
            from: Some(from),
            fill,
        })
    }

    /// Count an underrun at the start of a dry spell that isn't past the
    /// end of the audio.
    fn count(&mut self, fill: &Fill, epoch: u64, from: i64, frames: usize) {
        if fill.dry == 0 {
            self.dry = false;
            return;
        }
        let dry_from = from + (frames - fill.dry) as i64;
        let past_end = self.shared.end_of(epoch).is_some_and(|end| dry_from >= end);
        if !past_end && !self.dry {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            self.dry = true;
        }
    }

    fn note(&mut self, at: i64, frames: usize, latency: i64) {
        let sh = &self.shared;
        sh.last_callback.store(at, Ordering::Release);
        sh.callbacks.fetch_add(1, Ordering::Relaxed);
        sh.period
            .store(u32::try_from(frames).unwrap_or(u32::MAX), Ordering::Relaxed);
        sh.latency.store(latency, Ordering::Relaxed);
        if let Some(last) = self.last {
            sh.interval
                .store(at.saturating_sub(last), Ordering::Relaxed);
        }
        self.last = Some(at);
    }
}
