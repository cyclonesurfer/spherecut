//! Audio output (M1 plan section 9.2, DECISIONS D35): the device, the
//! chunk ring the decoder fills for it, and the audio clock that video
//! follows, so that the DAC's clock is the master.
//!
//! An [`Output`] plays on one of three devices, each running the same
//! callback code ([`render`]):
//!
//! * the default output device through cpal ([`Output::open_device`]),
//!   on PulseAudio's protocol (which `pipewire-pulse` speaks) or else
//!   ALSA, driven from the `audio-out` thread;
//! * [`Output::null`], a thread that plays nothing at the real-time rate,
//!   for a machine with no output device, so the engine runs the same
//!   code there;
//! * [`FakeAudioDevice`], whose callbacks come at scripted instants, for
//!   the A/V sync tests.
//!
//! The decode side holds the output's [`Sink`]: it asks which epoch to
//! decode and from where ([`Sink::want`]), and pushes the chunks into the
//! ring. The engine holds the [`Output`] and its [`AudioClock`].

#![forbid(unsafe_code)]

pub mod clock;
mod device;
pub mod fake;
mod null;
pub mod render;
pub mod ring;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_core::Time;

pub use clock::{AudioClock, Fallback, RESYNC_LEAD, STALL, WAIT_LIMIT};
pub use device::{PREFERRED_BUFFER, choose_buffer};
pub use fake::{FakeAudioDevice, FakeConfig, Played};
pub use render::Rendered;
pub use ring::{CHUNK_FRAMES, ChunkRing, Fill};

use crate::clock::{DeviceInfo, Shared};

/// Why an output couldn't be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    /// No output device: the engine plays silently on [`Output::null`].
    #[error("no audio output device: {reason}")]
    NoDevice { reason: String },
    /// A device, but no stream on it.
    #[error("audio output failed: {reason}")]
    Stream { reason: String },
}

/// What an output plays on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Device,
    Null,
    Fake,
}

/// An output's numbers, for the overlay and the hardware report.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputStats {
    pub kind: OutputKind,
    /// cpal's host ("PulseAudio", "ALSA"), or "null" or "fake".
    pub host: String,
    pub device: Option<String>,
    /// The device's sample format, which f32 is converted to.
    pub sample_format: Option<String>,
    pub rate: u32,
    pub channels: u16,
    /// The buffer size asked for, when one was ([`choose_buffer`]).
    pub buffer: Option<u32>,
    /// Frames in the last callback's buffer.
    pub period: u32,
    /// Between the last two callbacks.
    pub interval: Duration,
    /// From the last callback to its first frame being heard.
    pub latency: Duration,
    pub callbacks: u64,
    /// Dry spells while playing ([`render`]'s module docs).
    pub underruns: u64,
    /// Underruns the host reported.
    pub xruns: u64,
    pub on_system_clock: bool,
    /// Times the clock fell back to the system clock.
    pub fallbacks: u64,
    pub last_fallback: Option<Fallback>,
}

/// A thread an output runs, stopped and joined on drop.
#[derive(Debug)]
struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// An audio output (see the crate docs). Dropping it stops its device.
#[derive(Debug)]
pub struct Output {
    shared: Arc<Shared>,
    clock: Arc<AudioClock>,
    kind: OutputKind,
    _worker: Option<Worker>,
}

impl Output {
    fn new(shared: Arc<Shared>, kind: OutputKind, worker: Option<Worker>) -> (Output, Sink) {
        let sink = Sink {
            shared: shared.clone(),
        };
        let out = Output {
            clock: Arc::new(AudioClock(shared.clone())),
            shared,
            kind,
            _worker: worker,
        };
        (out, sink)
    }

    /// The default output device, at its default rate and channel count:
    /// PulseAudio if a server is there, else ALSA. Blocks while the
    /// device opens; [`AudioError::NoDevice`] when there is none.
    pub fn open_device() -> Result<(Output, Sink), AudioError> {
        let (shared, worker) = device::open()?;
        Ok(Self::new(shared, OutputKind::Device, Some(worker)))
    }

    /// An output that plays nothing, at the real-time rate.
    pub fn null(rate: u32, channels: u16) -> (Output, Sink) {
        let shared = Shared::new(
            rate,
            channels,
            DeviceInfo {
                host: "null".into(),
                ..DeviceInfo::default()
            },
        );
        let worker = null::spawn(shared.clone());
        Self::new(shared, OutputKind::Null, Some(worker))
    }

    /// The clock video follows while this plays.
    pub fn clock(&self) -> Arc<AudioClock> {
        self.clock.clone()
    }

    pub fn kind(&self) -> OutputKind {
        self.kind
    }

    pub fn rate(&self) -> u32 {
        self.shared.rate
    }

    pub fn channels(&self) -> u16 {
        self.shared.channels
    }

    /// Whether at least `ahead` of audio from the clock's time is queued
    /// (capped at what the ring holds), or the audio ends first. Always
    /// on the system clock. Never waits.
    pub fn ready(&self, ahead: Duration) -> bool {
        self.shared.ready(ahead, Instant::now())
    }

    pub fn stats(&self) -> OutputStats {
        let sh = &self.shared;
        let info = sh.device.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let duration = |ns: i64| Duration::from_nanos(u64::try_from(ns).unwrap_or(0));
        OutputStats {
            kind: self.kind,
            host: info.host,
            device: info.device,
            sample_format: info.sample_format,
            rate: sh.rate,
            channels: sh.channels,
            buffer: info.buffer,
            period: sh.period.load(Ordering::Relaxed),
            interval: duration(sh.interval.load(Ordering::Relaxed)),
            latency: duration(sh.latency.load(Ordering::Relaxed)),
            callbacks: sh.callbacks.load(Ordering::Relaxed),
            underruns: sh.underruns.load(Ordering::Relaxed),
            xruns: sh.xruns.load(Ordering::Relaxed),
            on_system_clock: sh.read().system,
            fallbacks: sh.fallbacks.load(Ordering::Relaxed),
            last_fallback: sh.last_fallback(),
        }
    }
}

/// What the decoder is to decode: the clock's epoch and where its audio
/// starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Want {
    pub epoch: u64,
    /// Seek here ...
    pub at: Time,
    /// ... and the first chunk starts at this sample, round(at × rate).
    pub start: i64,
}

/// The decode side of an output: the ring's only producer.
#[derive(Debug)]
pub struct Sink {
    shared: Arc<Shared>,
}

impl Sink {
    pub fn rate(&self) -> u32 {
        self.shared.rate
    }

    pub fn channels(&self) -> u16 {
        self.shared.channels
    }

    pub fn want(&self) -> Want {
        let s = self.shared.read();
        Want {
            epoch: s.epoch,
            at: s.seek,
            start: self.shared.sample_at(s.seek),
        }
    }

    /// Push `data` (interleaved, the output's channels) of `epoch`, its
    /// first frame sample `start`. Returns the frames taken: fewer when
    /// the ring fills (the decoder sleeps 5 ms and pushes the rest), none
    /// when `epoch` isn't the clock's any more.
    pub fn push(&self, epoch: u64, start: i64, data: &[f32]) -> usize {
        let ch = usize::from(self.shared.channels);
        let mut taken = 0;
        while taken * ch < data.len() {
            if self.shared.read().epoch != epoch {
                break;
            }
            let n = self
                .shared
                .ring
                .push(epoch, start + taken as i64, &data[taken * ch..]);
            if n == 0 {
                break;
            }
            taken += n;
            self.shared.filled_to(epoch, start + taken as i64);
        }
        taken
    }

    /// The audio of `epoch` ends at sample `at`: the ring running dry
    /// after it isn't an underrun, and a seek past it is ready at once.
    pub fn end(&self, epoch: u64, at: i64) {
        self.shared.set_end(epoch, at);
    }
}
