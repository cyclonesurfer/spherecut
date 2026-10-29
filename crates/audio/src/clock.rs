//! The audio clock (M1 plan section 9.2): the media time is the time of
//! the sample the device plays.
//!
//! Everything the engine, the output's callback and the decode thread
//! share is in [`Shared`]: the clock's state in a [`SeqLock`], the chunk
//! ring, how far the decoder has filled it, and the callback's numbers.
//! The engine writes the state through [`AudioClock`]'s [`Clock`]
//! methods, the callback publishes its anchor into it, and a watchdog
//! moves it to the system clock when the device is lost or stalls.
//!
//! **Epochs.** A seek ([`Clock::set`]) and a pause start a new epoch:
//! the decoder seeks to the epoch's time and fills the ring with its
//! chunks, and the callback drops chunks of every other. Pausing seeks
//! too, to the paused time, so that playing again starts exactly at
//! sample round(t × rate) with the samples already queued in the device
//! never heard.
//!
//! **The time shown.** Paused, the base time. Playing, it waits at the
//! base time until audio of the epoch starts, so video waits for audio.
//! Then each callback anchors it: at the callback's instant the sample
//! heard is the buffer's first less the latency, and the clock runs on
//! from there, but no further than the end of the buffer just written
//! plus one more buffer, so a stalled callback freezes it instead of
//! letting it run ahead of the sound. It never shows less than it showed
//! when the anchor was made (the floor): the start of an epoch holds the
//! base time until its first sample is heard, and an anchor a little
//! behind the last one, by the jitter of the callback's timing, holds
//! the clock until it catches up instead of moving it back.
//!
//! **The system clock.** When the device is lost, or its callbacks stop
//! for [`STALL`] while playing, or no audio arrives for [`WAIT_LIMIT`]
//! after playing starts, the clock runs on the monotonic system clock
//! from the time it showed, as `SystemClock` does, and asks the decoder
//! for audio [`RESYNC_LEAD`] ahead. A callback that finds the clock
//! there plays the sample due at the system clock's time when it is
//! heard and anchors the clock to the audio again, without a jump.

use std::fmt;
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::Thread;
use std::time::{Duration, Instant};

use spherecut_core::clock::{SeqLock, plus_nanos};
use spherecut_core::{Clock, Rational, Rounding, Time};

use crate::ring::ChunkRing;

/// How far ahead of the clock audio is asked for when the clock falls
/// back to the system clock, so that it is decoded before it is due.
pub const RESYNC_LEAD: Duration = Duration::from_millis(300);
/// No callback for this long while playing: the device stalled (a
/// suspended sink), and the clock falls back to the system clock.
pub const STALL: Duration = Duration::from_millis(500);
/// Playing for this long with no audio of the epoch yet: the clock runs
/// on without it.
pub const WAIT_LIMIT: Duration = Duration::from_secs(2);

const NANOS: i64 = 1_000_000_000;
/// `last_callback` before the first.
const NEVER: i64 = i64::MIN;

/// `d` as signed nanoseconds, saturating.
pub(crate) fn nanos_of(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

/// `samples` at `rate` in nanoseconds, to the nearest.
pub(crate) fn samples_nanos(samples: i64, rate: u32) -> i64 {
    let r = i128::from(rate.max(1));
    let n = i128::from(samples) * i128::from(NANOS);
    let q = (n + n.signum() * r / 2) / r;
    i64::try_from(q).unwrap_or(if q < 0 { i64::MIN } else { i64::MAX })
}

fn time_of(num: i64, den: i64) -> Time {
    Time(Rational::new(num, den).unwrap_or(Rational::ZERO))
}

/// `b − a` in nanoseconds, rounded up (0 if it can't be computed).
fn nanos_between(a: Time, b: Time) -> i64 {
    let tick = Rational::new(1, NANOS).unwrap_or(Rational::ONE);
    b.checked_sub(a)
        .and_then(|d| d.to_pts(tick, Rounding::Ceil))
        .unwrap_or(0)
}

/// The clock's state (see the module docs), as one write leaves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct State {
    /// Starts at 1; chunks of epoch 0 are the ring's empty slots.
    pub epoch: u64,
    /// The time the clock counts from.
    pub base: Time,
    /// Where the epoch's audio starts: the decoder seeks here.
    pub seek: Time,
    pub running: bool,
    /// On the system clock.
    pub system: bool,
    /// Audio of the epoch plays and anchors the clock; otherwise the
    /// clock waits for it at `base`.
    pub audio: bool,
    /// An instant, in nanoseconds from [`Shared`]'s origin: the anchor's
    /// callback while `audio`; the system clock's anchor while `system`;
    /// otherwise when playing (or waiting) began.
    pub wall: i64,
    /// While `audio`: the time heard at `wall`, in nanoseconds after
    /// `base`.
    pub heard: i64,
    /// While `audio`: how far past `wall` the clock may run.
    pub limit: i64,
    /// While `audio`: the least the clock shows, in nanoseconds after
    /// `base`.
    pub floor: i64,
}

const LEN: usize = 12;

impl State {
    fn new() -> Self {
        State {
            epoch: 1,
            base: Time::ZERO,
            seek: Time::ZERO,
            running: false,
            system: false,
            audio: false,
            wall: 0,
            heard: 0,
            limit: 0,
            floor: 0,
        }
    }

    fn pack(self) -> [i64; LEN] {
        [
            self.epoch as i64,
            self.base.0.num(),
            self.base.0.den(),
            self.seek.0.num(),
            self.seek.0.den(),
            i64::from(self.running),
            i64::from(self.system),
            i64::from(self.audio),
            self.wall,
            self.heard,
            self.limit,
            self.floor,
        ]
    }

    fn unpack(a: [i64; LEN]) -> Self {
        State {
            epoch: a[0] as u64,
            base: time_of(a[1], a[2]),
            seek: time_of(a[3], a[4]),
            running: a[5] != 0,
            system: a[6] != 0,
            audio: a[7] != 0,
            wall: a[8],
            heard: a[9],
            limit: a[10],
            floor: a[11],
        }
    }

    /// The time shown at the instant `now` (nanoseconds from the origin).
    pub fn shown(&self, now: i64) -> Time {
        if !self.running {
            return self.base;
        }
        if self.system {
            return plus_nanos(self.base, now.saturating_sub(self.wall).max(0));
        }
        if !self.audio {
            return self.base;
        }
        let ran = now.saturating_sub(self.wall).clamp(0, self.limit.max(0));
        plus_nanos(self.base, self.heard.saturating_add(ran).max(self.floor))
    }
}

/// Why the clock fell back to the system clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Fallback {
    /// The device reported an error, or went away.
    Lost,
    /// Its callbacks stopped for [`STALL`] while playing.
    Stalled,
    /// No audio came for [`WAIT_LIMIT`] after playing started.
    Starved,
}

/// What the device is, for [`OutputStats`](crate::OutputStats).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeviceInfo {
    pub host: String,
    pub device: Option<String>,
    pub sample_format: Option<String>,
    pub buffer: Option<u32>,
}

/// What the engine, the callback and the decode thread share (see the
/// module docs).
pub(crate) struct Shared {
    pub rate: u32,
    pub channels: u16,
    /// Instants are kept as signed nanoseconds from this one.
    origin: Instant,
    state: SeqLock<LEN>,
    pub ring: ChunkRing,
    /// [epoch, the sample after the last one pushed].
    filled: SeqLock<2>,
    /// [epoch, the sample its audio ends at]; epoch 0 for none.
    end: SeqLock<2>,
    pub last_callback: AtomicI64,
    pub callbacks: AtomicU64,
    pub underruns: AtomicU64,
    pub fallbacks: AtomicU64,
    /// Device-level underruns the host reported (cpal's `Xrun`).
    pub xruns: AtomicU64,
    pub period: AtomicU32,
    pub interval: AtomicI64,
    pub latency: AtomicI64,
    /// The thread to unpark when playing starts or stops: `audio-out`.
    pub wake: OnceLock<Thread>,
    pub device: Mutex<DeviceInfo>,
    /// The last [`Fallback`], as its index plus 1; 0 for none.
    why: AtomicU8,
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shared")
            .field("rate", &self.rate)
            .field("channels", &self.channels)
            .field("state", &self.read())
            .field("ring", &self.ring)
            .finish_non_exhaustive()
    }
}

impl Shared {
    pub fn new(rate: u32, channels: u16, device: DeviceInfo) -> Arc<Self> {
        Arc::new(Shared {
            rate: rate.max(1),
            channels: channels.max(1),
            origin: Instant::now(),
            state: SeqLock::new(State::new().pack()),
            ring: ChunkRing::new(rate.max(1), channels.max(1)),
            filled: SeqLock::new([0, 0]),
            end: SeqLock::new([0, 0]),
            last_callback: AtomicI64::new(NEVER),
            callbacks: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            period: AtomicU32::new(0),
            interval: AtomicI64::new(0),
            latency: AtomicI64::new(0),
            wake: OnceLock::new(),
            device: Mutex::new(device),
            why: AtomicU8::new(0),
        })
    }

    pub fn nanos(&self, at: Instant) -> i64 {
        match at.checked_duration_since(self.origin) {
            Some(d) => nanos_of(d),
            None => -nanos_of(self.origin.duration_since(at)),
        }
    }

    pub fn read(&self) -> State {
        State::unpack(self.state.read())
    }

    /// Replace the state with `f` of it.
    pub fn update(&self, f: impl FnOnce(State) -> State) {
        self.state.update(|a| f(State::unpack(a)).pack());
    }

    /// The output sample at `t`: round(t × rate), as the decoder's seek
    /// lands (`AudioDecode::seek`).
    pub fn sample_at(&self, t: Time) -> i64 {
        Rational::new(1, i64::from(self.rate))
            .and_then(|tick| t.to_pts(tick, Rounding::Nearest))
            .unwrap_or(0)
    }

    pub fn samples(&self, d: Duration) -> i64 {
        i64::try_from(d.as_nanos() * u128::from(self.rate) / 1_000_000_000).unwrap_or(i64::MAX)
    }

    /// Unpark `audio-out` to play or pause the stream.
    fn wake(&self) {
        if let Some(t) = self.wake.get() {
            t.unpark();
        }
    }

    /// The decoder: frames of `epoch` from `start` were pushed.
    pub fn filled_to(&self, epoch: u64, to: i64) {
        self.filled.update(|_| [epoch as i64, to]);
    }

    /// [epoch, the sample after the last one pushed].
    pub fn filled(&self) -> (u64, i64) {
        let [e, to] = self.filled.read();
        (e as u64, to)
    }

    /// The decoder: the audio of `epoch` ends at sample `at`.
    pub fn set_end(&self, epoch: u64, at: i64) {
        self.end.update(|_| [epoch as i64, at]);
    }

    /// Where the audio of `epoch` ends, once the decoder has said.
    pub fn end_of(&self, epoch: u64) -> Option<i64> {
        let [e, at] = self.end.read();
        (e as u64 == epoch).then_some(at)
    }

    /// Fall back to the system clock from the time shown at `now`,
    /// asking the decoder for audio [`RESYNC_LEAD`] ahead if playing.
    /// Whether it did: not if it was there already.
    pub fn to_system(&self, now: Instant, why: Fallback) -> bool {
        let at = self.nanos(now);
        let mut switched = false;
        self.update(|s| {
            if s.system {
                return s;
            }
            switched = true;
            let t = s.shown(at);
            let (epoch, seek) = if s.running {
                (
                    s.epoch + 1,
                    t.checked_add(Time::seconds(lead())).unwrap_or(t),
                )
            } else {
                (s.epoch, s.seek)
            };
            State {
                epoch,
                base: t,
                seek,
                system: true,
                audio: false,
                wall: at,
                ..s
            }
        });
        if switched {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            self.why.store(why as u8 + 1, Ordering::Relaxed);
        }
        switched
    }

    /// Why the clock last fell back to the system clock.
    pub fn last_fallback(&self) -> Option<Fallback> {
        match self.why.load(Ordering::Relaxed) {
            1 => Some(Fallback::Lost),
            2 => Some(Fallback::Stalled),
            3 => Some(Fallback::Starved),
            _ => None,
        }
    }

    /// The watchdog, every 50 ms or so from a thread that isn't the
    /// callback's: fall back to the system clock if the callbacks
    /// stalled, or no audio came, while playing.
    pub fn watch(&self, now: Instant) -> Option<Fallback> {
        let s = self.read();
        if !s.running || s.system {
            return None;
        }
        let at = self.nanos(now);
        let last = self.last_callback.load(Ordering::Acquire);
        let quiet = at.saturating_sub(last.max(s.wall));
        let why = if quiet > nanos_of(STALL) {
            Fallback::Stalled
        } else if !s.audio && at.saturating_sub(s.wall) > nanos_of(WAIT_LIMIT) {
            Fallback::Starved
        } else {
            return None;
        };
        self.to_system(now, why).then_some(why)
    }

    /// Whether at least `ahead` of audio from the time shown is queued,
    /// or the audio ends before then. On the system clock, always: video
    /// doesn't wait for audio there. `ahead` is capped below the ring's
    /// size, which is all that can be queued.
    pub fn ready(&self, ahead: Duration, now: Instant) -> bool {
        let s = self.read();
        if s.system {
            return true;
        }
        let ring = (self.ring.capacity().saturating_sub(1) * crate::ring::CHUNK_FRAMES) as i64;
        let from = self.sample_at(s.shown(self.nanos(now)));
        let want = from.saturating_add(self.samples(ahead).min(ring));
        let (e, to) = self.filled();
        (e == s.epoch && to >= want) || self.end_of(s.epoch).is_some_and(|end| end <= want)
    }

    /// Publish an anchor (see the module docs) for the callback at `now`
    /// (nanoseconds) of epoch `epoch`, on the system clock or not as the
    /// callback found it: `base`, and the buffer's first sample `d`
    /// samples after it, heard `latency` after `now`; `frames` in the
    /// buffer. Whether it was published: not if the state moved on since
    /// the callback read it.
    #[allow(clippy::too_many_arguments)]
    pub fn anchor(
        &self,
        epoch: u64,
        system: bool,
        base: Time,
        d: i64,
        now: i64,
        latency: i64,
        frames: usize,
    ) -> bool {
        let heard = samples_nanos(d, self.rate).saturating_sub(latency);
        let limit = latency.saturating_add(2 * samples_nanos(frames as i64, self.rate));
        let mut published = false;
        self.update(|s| {
            if s.epoch != epoch || s.system != system || !s.running {
                return s;
            }
            published = true;
            let floor = nanos_between(base, s.shown(now));
            State {
                base,
                system: false,
                audio: true,
                wall: now,
                heard,
                limit,
                floor,
                ..s
            }
        });
        published
    }

    /// On the system clock, with the epoch's audio not at the time due:
    /// ask the decoder for audio [`RESYNC_LEAD`] ahead of `due`.
    pub fn resync(&self, epoch: u64, due: Time) {
        self.update(|s| {
            if s.epoch != epoch || !s.system {
                return s;
            }
            State {
                epoch: s.epoch + 1,
                seek: due.checked_add(Time::seconds(lead())).unwrap_or(due),
                ..s
            }
        });
    }
}

fn lead() -> Rational {
    Rational::new(nanos_of(RESYNC_LEAD), NANOS).unwrap_or(Rational::ZERO)
}

/// The audio clock: a [`Clock`] over the output's [`Shared`] state (see
/// the module docs). Reading it never blocks.
#[derive(Clone)]
pub struct AudioClock(pub(crate) Arc<Shared>);

impl Clock for AudioClock {
    fn time_at(&self, at: Instant) -> Time {
        self.0.read().shown(self.0.nanos(at))
    }

    fn is_running(&self) -> bool {
        self.0.read().running
    }

    fn play(&self, at: Instant) {
        let now = self.0.nanos(at);
        self.0.update(|s| {
            if s.running {
                return s;
            }
            State {
                running: true,
                audio: false,
                wall: now,
                ..s
            }
        });
        self.0.wake();
    }

    fn pause(&self, at: Instant) {
        let now = self.0.nanos(at);
        self.0.update(|s| {
            if !s.running {
                return s;
            }
            let t = s.shown(now);
            State {
                epoch: s.epoch + 1,
                base: t,
                seek: t,
                running: false,
                audio: false,
                wall: now,
                ..s
            }
        });
        self.0.wake();
    }

    fn set(&self, time: Time, at: Instant) {
        let now = self.0.nanos(at);
        self.0.update(|s| {
            if !s.running && s.base == time && s.seek == time {
                // Paused there already, with the epoch's audio queued
                // from it.
                return s;
            }
            State {
                epoch: s.epoch + 1,
                base: time,
                seek: time,
                audio: false,
                wall: now,
                ..s
            }
        });
    }
}

impl fmt::Debug for AudioClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.read();
        f.debug_struct("AudioClock")
            .field("epoch", &s.epoch)
            .field("base", &s.base)
            .field("running", &s.running)
            .field("system", &s.system)
            .field("audio", &s.audio)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_to_the_nearest_nanosecond() {
        assert_eq!(samples_nanos(48_000, 48_000), NANOS);
        assert_eq!(samples_nanos(1, 48_000), 20_833);
        assert_eq!(samples_nanos(1024, 44_100), 23_219_955);
        assert_eq!(samples_nanos(-1024, 44_100), -23_219_955);
    }

    #[test]
    fn the_state_survives_its_packing() {
        let s = State {
            epoch: 7,
            base: time_of(1001, 30_000),
            seek: time_of(-3, 2),
            running: true,
            system: false,
            audio: true,
            wall: -5,
            heard: -20_000_000,
            limit: 62_000_000,
            floor: 9,
        };
        assert_eq!(State::unpack(s.pack()), s);
    }

    #[test]
    fn shown_holds_its_floor_and_stops_at_its_limit() {
        let s = State {
            epoch: 1,
            base: Time::from_secs(10),
            seek: Time::from_secs(10),
            running: true,
            system: false,
            audio: true,
            wall: 1_000,
            heard: -20_000_000,
            limit: 60_000_000,
            floor: 0,
        };
        let ms = |n: i64| 1_000 + n * 1_000_000;
        // Held at the base until the first sample is heard, 20 ms on.
        assert_eq!(s.shown(0), Time::from_secs(10));
        assert_eq!(s.shown(ms(20)), Time::from_secs(10));
        assert_eq!(s.shown(ms(30)), time_of(1001, 100));
        // Frozen at the limit: 60 ms after the callback.
        assert_eq!(s.shown(ms(60)), time_of(1004, 100));
        assert_eq!(s.shown(ms(900)), time_of(1004, 100));
        // On the system clock, from its anchor.
        let sys = State { system: true, ..s };
        assert_eq!(sys.shown(ms(500)), time_of(105, 10));
        let paused = State {
            running: false,
            ..s
        };
        assert_eq!(paused.shown(ms(500)), Time::from_secs(10));
    }
}
