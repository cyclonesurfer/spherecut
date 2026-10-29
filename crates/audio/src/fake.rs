//! A device whose callbacks come at scripted instants (M1 plan section
//! 9.3), for the A/V sync tests: a fixed period and a stated latency,
//! with stalls and device loss when a test asks. It runs the real
//! callback code, and the watchdog every 50 ms of scripted time, as
//! `audio-out` does.
//!
//! Scripted time runs ahead of real time, and the decoder runs in real
//! time, so before each callback the fake waits (for at most
//! [`FakeAudioDevice::set_wait`], in real time) for the audio the
//! callback will play: the ring then runs dry only when a test stalls
//! the decoder.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clock::{DeviceInfo, Shared};
use crate::render::{Callback, Rendered};
use crate::{Output, OutputKind, Sink};

/// The watchdog's interval.
const WATCH: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeConfig {
    pub rate: u32,
    pub channels: u16,
    /// Frames per callback.
    pub period: u32,
    /// From a callback to its first frame being heard.
    pub latency: Duration,
}

impl Default for FakeConfig {
    fn default() -> Self {
        FakeConfig {
            rate: 48_000,
            channels: 2,
            period: 1024,
            latency: Duration::from_millis(20),
        }
    }
}

/// One callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Played {
    /// The callback's instant.
    pub at: Instant,
    /// When its first frame is heard.
    pub heard: Instant,
    pub rendered: Rendered,
}

/// The fake device (see the module docs).
#[derive(Debug)]
pub struct FakeAudioDevice {
    shared: Arc<Shared>,
    cfg: FakeConfig,
    callback: Callback,
    /// Callbacks come at `start + delay + done × period`.
    start: Instant,
    delay: Duration,
    done: u64,
    next_watch: Instant,
    lost: bool,
    wait: Duration,
    buf: Vec<f32>,
    played: Vec<Played>,
}

impl FakeAudioDevice {
    /// An output on a fake device whose first callback comes at `start`.
    pub fn new(cfg: FakeConfig, start: Instant) -> (Output, Sink, FakeAudioDevice) {
        let shared = Shared::new(
            cfg.rate,
            cfg.channels,
            DeviceInfo {
                host: "fake".into(),
                buffer: Some(cfg.period),
                ..DeviceInfo::default()
            },
        );
        let fake = FakeAudioDevice {
            callback: Callback::new(shared.clone()),
            shared: shared.clone(),
            cfg,
            start,
            delay: Duration::ZERO,
            done: 0,
            next_watch: start,
            lost: false,
            wait: Duration::from_secs(5),
            buf: vec![0.0; cfg.period as usize * usize::from(cfg.channels.max(1))],
            played: Vec::new(),
        };
        let (out, sink) = Output::new(shared, OutputKind::Fake, None);
        (out, sink, fake)
    }

    pub fn config(&self) -> FakeConfig {
        self.cfg
    }

    /// How long, in real time, a callback waits for its audio; 0 not to.
    pub fn set_wait(&mut self, wait: Duration) {
        self.wait = wait;
    }

    /// When the next callback comes; `None` while the device is lost.
    pub fn next_callback(&self) -> Option<Instant> {
        if self.lost {
            return None;
        }
        let ns = u128::from(self.done) * u128::from(self.cfg.period) * 1_000_000_000
            / u128::from(self.cfg.rate.max(1));
        Some(self.start + self.delay + Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX)))
    }

    /// The next callback, after waiting for its audio; `None` while the
    /// device is lost.
    pub fn tick(&mut self) -> Option<Played> {
        let at = self.next_callback()?;
        self.wait_for_audio();
        let rendered = self.callback.render(&mut self.buf, at, self.cfg.latency);
        self.done += 1;
        let played = Played {
            at,
            heard: at + self.cfg.latency,
            rendered,
        };
        self.played.push(played);
        Some(played)
    }

    /// Run the callbacks, and the watchdog every 50 ms, up to and at `t`.
    pub fn run_until(&mut self, t: Instant) {
        loop {
            let watch = self.next_watch;
            match self.next_callback() {
                Some(c) if c <= t && c <= watch => {
                    self.tick();
                }
                _ if watch <= t => {
                    self.shared.watch(watch);
                    self.next_watch = watch + WATCH;
                }
                _ => break,
            }
        }
    }

    /// The callbacks from the next one on come `d` later.
    pub fn stall(&mut self, d: Duration) {
        self.delay += d;
    }

    /// The device goes away at `at`: the clock falls back to the system
    /// clock, and no callbacks come until [`restore`](Self::restore).
    pub fn lose(&mut self, at: Instant) {
        self.lost = true;
        self.shared.to_system(at, crate::Fallback::Lost);
    }

    /// A new stream, its first callback at `at`, with the callback's
    /// state new, as `audio-out` rebuilds one.
    pub fn restore(&mut self, at: Instant) {
        self.lost = false;
        self.callback = Callback::new(self.shared.clone());
        self.start = at;
        self.delay = Duration::ZERO;
        self.done = 0;
    }

    /// The last callback's buffer.
    pub fn buffer(&self) -> &[f32] {
        &self.buf
    }

    /// Every callback so far.
    pub fn played(&self) -> &[Played] {
        &self.played
    }

    pub fn take_played(&mut self) -> Vec<Played> {
        std::mem::take(&mut self.played)
    }

    /// Wait until the ring holds what the next callback will play, or
    /// the audio ends, or the epoch moves on, or [`set_wait`]'s time is
    /// up.
    ///
    /// [`set_wait`]: Self::set_wait
    fn wait_for_audio(&self) {
        let Some((epoch, first)) = self.callback.wants() else {
            return;
        };
        let need = first + i64::from(self.cfg.period);
        let deadline = Instant::now() + self.wait;
        loop {
            let (e, to) = self.shared.filled();
            if (e == epoch && to >= need)
                || self.shared.end_of(epoch).is_some()
                || self.shared.read().epoch != epoch
                || self.shared.ring.queued() >= self.shared.ring.capacity()
                || Instant::now() >= deadline
            {
                return;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}
