//! The default output device through cpal, driven from the `audio-out`
//! thread (M1 plan section 9.2).
//!
//! * **Host:** cpal's default, PulseAudio when a server's socket is
//!   there (`pipewire-pulse` on Fedora and Arch), else ALSA.
//! * **Format:** the device's default rate and channel count, f32 when
//!   the device takes it, else its default sample format converted from
//!   f32 in the callback.
//! * **Buffer:** [`choose_buffer`]; `Default` if a fixed size is refused.
//! * **`audio-out`** opens the stream and keeps it, plays and pauses it
//!   as the clock does (pausing [`PAUSE_AFTER`] after the clock, so the
//!   device's queue is silence by then), runs the watchdog, and on an
//!   error or a stall that lasts drops the stream, and tries the default
//!   device again every [`RETRY`] at the same rate and channel count,
//!   which the decoder resamples to. cpal 0.18's `DeviceChanged` means
//!   the stream was rerouted and goes on, so it is only logged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    BufferSize, ErrorKind, FromSample, SampleFormat, SizedSample, StreamConfig,
    SupportedBufferSize, SupportedStreamConfig,
};

use crate::clock::{DeviceInfo, Fallback, Shared};
use crate::render::Callback;
use crate::{AudioError, Worker};

/// The buffer asked for: 21.3 ms at 48 kHz.
pub const PREFERRED_BUFFER: u32 = 1024;
/// How often `audio-out` tries the default device again after losing it.
pub const RETRY: Duration = Duration::from_secs(2);
/// How long after the clock pauses the stream is.
pub const PAUSE_AFTER: Duration = Duration::from_millis(200);
/// `audio-out`'s loop, which the clock and the error callback also wake.
const TICK: Duration = Duration::from_millis(50);
/// The callback converts and renders at most this many frames at a time.
const MAX_FRAMES: usize = 16_384;

/// [`PREFERRED_BUFFER`] if the device's range allows it, else the value
/// nearest it in the range; `Default` when the range is unknown.
pub fn choose_buffer(supported: &SupportedBufferSize) -> BufferSize {
    match *supported {
        SupportedBufferSize::Range { min, max } if min <= max && max > 0 => {
            BufferSize::Fixed(PREFERRED_BUFFER.clamp(min.max(1), max))
        }
        _ => BufferSize::Default,
    }
}

/// A device and the config to open it with.
struct Found {
    device: cpal::Device,
    config: StreamConfig,
    format: SampleFormat,
    info: DeviceInfo,
}

fn no_device(reason: String) -> AudioError {
    AudioError::NoDevice { reason }
}

fn stream_error(e: impl std::fmt::Display) -> AudioError {
    AudioError::Stream {
        reason: e.to_string(),
    }
}

/// Whether the callback can convert to `f`.
fn convertible(f: SampleFormat) -> bool {
    matches!(
        f,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I24
            | SampleFormat::I32
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U32
    )
}

/// The default output device, at `want` (rate, channels) or else its
/// default.
fn find(want: Option<(u32, u16)>) -> Result<Found, AudioError> {
    let host = cpal::default_host();
    let host_name = host.id().name().to_string();
    let device = host
        .default_output_device()
        .ok_or_else(|| no_device(format!("{host_name} has no default output device")))?;
    let default = device
        .default_output_config()
        .map_err(|e| no_device(format!("{host_name}: {e}")))?;
    let (rate, channels) = want.unwrap_or((default.sample_rate(), default.channels()));
    let fits = |c: &SupportedStreamConfig| c.sample_rate() == rate && c.channels() == channels;
    let ranges: Vec<_> = device
        .supported_output_configs()
        .map(Iterator::collect)
        .unwrap_or_default();
    let of = |format: SampleFormat| {
        ranges
            .iter()
            .filter(|r| r.channels() == channels && r.sample_format() == format)
            .find_map(|r| r.try_with_sample_rate(rate))
    };
    let picked = if default.sample_format() == SampleFormat::F32 && fits(&default) {
        Some(default)
    } else {
        of(SampleFormat::F32)
            .or_else(|| (fits(&default) && convertible(default.sample_format())).then_some(default))
            .or_else(|| {
                [SampleFormat::I32, SampleFormat::I16, SampleFormat::I24]
                    .into_iter()
                    .find_map(of)
            })
    };
    let picked = picked.ok_or_else(|| {
        stream_error(format!(
            "{host_name}: no sample format to play {rate} Hz, {channels} channels in"
        ))
    })?;
    let buffer_size = choose_buffer(picked.buffer_size());
    let info = DeviceInfo {
        host: host_name,
        device: device.description().ok().map(|d| d.name().to_string()),
        sample_format: Some(picked.sample_format().to_string()),
        buffer: match buffer_size {
            BufferSize::Fixed(n) => Some(n),
            BufferSize::Default => None,
        },
    };
    Ok(Found {
        device,
        config: StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size,
        },
        format: picked.sample_format(),
        info,
    })
}

/// Build a stream on `found` that renders through a new [`Callback`],
/// sending its errors to `errors`. A fixed buffer size refused, it tries
/// the device's default.
fn build(
    found: &mut Found,
    shared: &Arc<Shared>,
    errors: &Sender<cpal::Error>,
) -> Result<cpal::Stream, cpal::Error> {
    let first = build_as_format(found, shared, errors);
    match first {
        Err(e)
            if e.kind() == ErrorKind::UnsupportedConfig
                && found.config.buffer_size != BufferSize::Default =>
        {
            found.config.buffer_size = BufferSize::Default;
            found.info.buffer = None;
            build_as_format(found, shared, errors)
        }
        r => r,
    }
}

fn build_as_format(
    found: &Found,
    shared: &Arc<Shared>,
    errors: &Sender<cpal::Error>,
) -> Result<cpal::Stream, cpal::Error> {
    let (d, c) = (&found.device, found.config);
    match found.format {
        SampleFormat::F32 => build_as::<f32>(d, c, shared, errors),
        SampleFormat::F64 => build_as::<f64>(d, c, shared, errors),
        SampleFormat::I8 => build_as::<i8>(d, c, shared, errors),
        SampleFormat::I16 => build_as::<i16>(d, c, shared, errors),
        SampleFormat::I24 => build_as::<cpal::I24>(d, c, shared, errors),
        SampleFormat::I32 => build_as::<i32>(d, c, shared, errors),
        SampleFormat::U8 => build_as::<u8>(d, c, shared, errors),
        SampleFormat::U16 => build_as::<u16>(d, c, shared, errors),
        SampleFormat::U32 => build_as::<u32>(d, c, shared, errors),
        _ => Err(cpal::Error::new(ErrorKind::UnsupportedConfig)),
    }
}

fn build_as<T>(
    device: &cpal::Device,
    config: StreamConfig,
    shared: &Arc<Shared>,
    errors: &Sender<cpal::Error>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let mut callback = Callback::new(shared.clone());
    let ch = usize::from(config.channels.max(1));
    let rate = u64::from(config.sample_rate.max(1));
    // Allocated here, never in the callback.
    let mut scratch = vec![0.0f32; MAX_FRAMES * ch];
    let errors = errors.clone();
    let waker = shared.wake.get().cloned();
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let now = Instant::now();
            let ts = info.timestamp();
            let mut latency = ts.playback.duration_since(ts.callback);
            for piece in data.chunks_mut(MAX_FRAMES * ch) {
                let n = piece.len();
                callback.render(&mut scratch[..n], now, latency);
                for (o, &s) in piece.iter_mut().zip(&scratch[..n]) {
                    *o = T::from_sample(s);
                }
                latency += Duration::from_nanos((n / ch) as u64 * 1_000_000_000 / rate);
            }
        },
        move |e| {
            let _ = errors.send(e);
            if let Some(t) = &waker {
                t.unpark();
            }
        },
        None,
    )
}

/// Open the default device on a new `audio-out` thread, which keeps it.
pub(crate) fn open() -> Result<(Arc<Shared>, Worker), AudioError> {
    let (tx, rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || {
                let mut found = match find(None) {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                let shared = Shared::new(
                    found.config.sample_rate,
                    found.config.channels,
                    found.info.clone(),
                );
                let _ = shared.wake.set(std::thread::current());
                let (etx, erx) = mpsc::channel();
                let stream = match build(&mut found, &shared, &etx) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(stream_error(e)));
                        return;
                    }
                };
                *shared.device.lock().unwrap_or_else(PoisonError::into_inner) = found.info.clone();
                if let Err(e) = stream.play() {
                    let _ = tx.send(Err(stream_error(e)));
                    return;
                }
                let _ = tx.send(Ok(shared.clone()));
                drop(tx);
                tracing::info!(
                    host = %found.info.host,
                    device = ?found.info.device,
                    rate = found.config.sample_rate,
                    channels = found.config.channels,
                    format = ?found.format,
                    buffer = ?found.config.buffer_size,
                    "audio output opened"
                );
                AudioOut {
                    shared,
                    stream: Some(stream),
                    playing: true,
                    pause_failed: false,
                    paused_since: None,
                    retry_at: None,
                    errors: erx,
                    error_tx: etx,
                    fallbacks: 0,
                    system: false,
                }
                .run(&stop);
            })
            .map_err(stream_error)?
    };
    match rx.recv() {
        Ok(Ok(shared)) => Ok((
            shared,
            Worker {
                stop,
                thread: Some(thread),
            },
        )),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(stream_error("the audio-out thread ended"))
        }
    }
}

/// `audio-out`'s state.
struct AudioOut {
    shared: Arc<Shared>,
    stream: Option<cpal::Stream>,
    playing: bool,
    /// This stream can't pause: it plays silence instead.
    pause_failed: bool,
    paused_since: Option<Instant>,
    retry_at: Option<Instant>,
    errors: Receiver<cpal::Error>,
    error_tx: Sender<cpal::Error>,
    fallbacks: u64,
    system: bool,
}

impl AudioOut {
    fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            std::thread::park_timeout(TICK);
            let now = Instant::now();
            self.errors(now);
            self.shared.watch(now);
            self.rebuild_if_stalled(now);
            self.retry(now);
            self.follow_clock(now);
            self.log();
        }
    }

    fn lose(&mut self, now: Instant) {
        self.shared.to_system(now, Fallback::Lost);
        self.stream = None;
        self.retry_at = Some(now + RETRY);
    }

    fn errors(&mut self, now: Instant) {
        let mut lost = false;
        while let Ok(e) = self.errors.try_recv() {
            match e.kind() {
                ErrorKind::Xrun => {
                    self.shared.xruns.fetch_add(1, Ordering::Relaxed);
                }
                ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {
                    tracing::info!(error = %e, "audio output");
                }
                _ => {
                    tracing::warn!(error = %e, "audio output lost");
                    lost = true;
                }
            }
        }
        if lost && self.stream.is_some() {
            self.lose(now);
        }
    }

    /// On the system clock while playing, with no callback for
    /// [`RETRY`]: the stream won't come back by itself.
    fn rebuild_if_stalled(&mut self, now: Instant) {
        let s = self.shared.read();
        if self.stream.is_none() || !s.running || !s.system {
            return;
        }
        let last = self.shared.last_callback.load(Ordering::Acquire);
        let quiet = self.shared.nanos(now).saturating_sub(last.max(s.wall));
        if quiet > crate::clock::nanos_of(RETRY) {
            tracing::warn!("audio output stalled: opening it again");
            self.stream = None;
            self.retry_at = None;
        }
    }

    fn retry(&mut self, now: Instant) {
        if self.stream.is_some() || self.retry_at.is_some_and(|t| now < t) {
            return;
        }
        let want = (self.shared.rate, self.shared.channels);
        let opened = find(Some(want)).and_then(|mut found| {
            let stream = build(&mut found, &self.shared, &self.error_tx).map_err(stream_error)?;
            Ok((found, stream))
        });
        match opened {
            Ok((found, stream)) => {
                tracing::info!(host = %found.info.host, device = ?found.info.device, "audio output opened again");
                *self
                    .shared
                    .device
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = found.info;
                self.stream = Some(stream);
                self.playing = false;
                self.pause_failed = false;
                self.paused_since = None;
                self.retry_at = None;
            }
            Err(e) => {
                tracing::debug!(error = %e, "audio output: still none");
                self.retry_at = Some(now + RETRY);
            }
        }
    }

    /// Play and pause the stream as the clock does.
    fn follow_clock(&mut self, now: Instant) {
        let Some(stream) = &self.stream else {
            return;
        };
        if self.shared.read().running {
            self.paused_since = None;
            if !self.playing {
                match stream.play() {
                    Ok(()) => self.playing = true,
                    Err(e) => {
                        tracing::warn!(error = %e, "audio output won't play");
                        self.lose(now);
                    }
                }
            }
            return;
        }
        let since = *self.paused_since.get_or_insert(now);
        if self.playing && !self.pause_failed && now.duration_since(since) >= PAUSE_AFTER {
            match stream.pause() {
                Ok(()) => self.playing = false,
                Err(e) => {
                    tracing::debug!(error = %e, "audio output can't pause: playing silence");
                    self.pause_failed = true;
                }
            }
        }
    }

    fn log(&mut self) {
        let fallbacks = self.shared.fallbacks.load(Ordering::Relaxed);
        if fallbacks != self.fallbacks {
            self.fallbacks = fallbacks;
            tracing::warn!(why = ?self.shared.last_fallback(), "audio: playing on the system clock");
        }
        let system = self.shared.read().system;
        if self.system && !system {
            tracing::info!("audio: on the device's clock again");
        }
        self.system = system;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_is_1024_or_the_nearest_the_device_allows() {
        let range = |min, max| SupportedBufferSize::Range { min, max };
        assert_eq!(choose_buffer(&range(1, 100_000)), BufferSize::Fixed(1024));
        assert_eq!(choose_buffer(&range(2048, 8192)), BufferSize::Fixed(2048));
        assert_eq!(choose_buffer(&range(64, 512)), BufferSize::Fixed(512));
        assert_eq!(choose_buffer(&range(0, 0)), BufferSize::Default);
        assert_eq!(choose_buffer(&range(4096, 1024)), BufferSize::Default);
        assert_eq!(
            choose_buffer(&SupportedBufferSize::Unknown),
            BufferSize::Default
        );
    }
}
