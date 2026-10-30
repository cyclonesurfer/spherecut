//! The null output: a thread that plays nothing at the real-time rate
//! (M1 plan section 9.2), for a machine with no output device. Its
//! callbacks come every [`PERIOD`] frames on a schedule kept from its
//! start, so oversleeping doesn't add up, with no latency.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::Worker;
use crate::clock::Shared;
use crate::render::Callback;

/// Frames per callback.
pub(crate) const PERIOD: usize = 1024;
/// Behind schedule by this many periods (a suspended machine): start the
/// schedule again from now instead of catching up in a burst.
const BEHIND: u32 = 8;

pub(crate) fn spawn(shared: Arc<Shared>) -> Worker {
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("audio-null".into())
            .spawn(move || run(&shared, &stop))
            .ok()
    };
    if thread.is_none() {
        tracing::warn!("audio: the null output's thread didn't start");
    }
    Worker::new(stop, thread)
}

fn run(shared: &Arc<Shared>, stop: &AtomicBool) {
    let rate = u64::from(shared.rate);
    let mut cb = Callback::new(shared.clone());
    let mut buf = vec![0.0f32; PERIOD * usize::from(shared.channels)];
    let period = Duration::from_nanos(PERIOD as u64 * 1_000_000_000 / rate);
    let mut start = Instant::now();
    let mut done: u64 = 0;
    while !stop.load(Ordering::Acquire) {
        let due = start + Duration::from_nanos(done * PERIOD as u64 * 1_000_000_000 / rate);
        let now = Instant::now();
        if now < due {
            // Parked, so that dropping the output wakes it at once.
            std::thread::park_timeout(due - now);
            continue;
        }
        // The callback's instant is the one its schedule gives, as a
        // device's would be, not the one the thread woke at.
        let at = if now - due > period * BEHIND {
            start = now;
            done = 0;
            now
        } else {
            due
        };
        cb.render(&mut buf, at, Duration::ZERO);
        done += 1;
    }
}
