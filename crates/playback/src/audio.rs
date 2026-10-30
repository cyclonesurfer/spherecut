//! The file's audio while it plays (M1 plan sections 8.1, 8.4, 9.2 and
//! 9.3, DECISIONS D34 and D35).
//!
//! With an [`AudioOutputFactory`], the player opens the file's audio
//! when it opens the file, on its open worker, and then follows the
//! output's clock, the audio device's, which is what makes the DAC the
//! master (plan section 9.3: video repeats or drops frames to follow
//! it, so there is no drift to correct). Playing, pausing and seeking go
//! through that [`Clock`]: an output's `set` is its seek, which restarts
//! audio at the new time (the ring's epoch, section 9.2). Preroll after
//! a seek ends when the video target is ready and the output says
//! [`AudioOutput::ready`] for [`PREROLL_AUDIO`], or after
//! [`PREROLL_LIMIT`] without it, when video plays on the system clock
//! until the audio comes ([`AudioOutput::fall_back`]). Without a
//! factory, or for a file with no audio, the player runs video alone on
//! the clock it was given: a `SystemClock` while playing for real, a
//! `ManualClock` in tests and M2's export.
//!
//! [`DeviceAudio`] is the factory: it opens an [`Output`] (the default
//! device, else the null output that plays silently) and an
//! `audio-decode` thread, which decodes the file's audio at the
//! output's rate and channel count into its ring, seeking to each epoch
//! the clock starts. The audio's clip time 0 is the first video
//! stream's first frame, which the decoder knows only once it opens, so
//! the thread waits for [`AudioOutput::start`] to open its decoder.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_audio::{Output, OutputKind, OutputStats, Sink};
use spherecut_core::{Clock, StopSource, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{AudioOutFormat, AudioRequest, MediaBackend, NextAudio};

use crate::player::PlayerError;

/// How much audio from the seek's time must be queued before playback
/// resumes after a seek (plan section 8.4, step 6).
pub const PREROLL_AUDIO: Duration = Duration::from_millis(100);

/// How long a preroll waits for the audio before video plays without it.
pub const PREROLL_LIMIT: Duration = Duration::from_secs(2);

/// How long `audio-decode` sleeps while the ring is full.
const FULL_SLEEP: Duration = Duration::from_millis(5);

/// The null output's format when no device opens.
const NULL_RATE: u32 = 48_000;
const NULL_CHANNELS: u16 = 2;

/// Opens a file's audio for playback.
pub trait AudioOutputFactory: Send + Sync + std::fmt::Debug {
    /// The audio of `path`, ready to play on its own clock, paused at 0;
    /// `Ok(None)` when the file has none, so the player keeps its own
    /// clock. Blocks: the player calls it on its open worker, never the
    /// UI thread.
    fn open(
        &self,
        path: &Path,
        stop: &StopToken,
    ) -> Result<Option<Arc<dyn AudioOutput>>, PlayerError>;
}

/// A file's audio, playing out.
pub trait AudioOutput: Send + Sync + std::fmt::Debug {
    /// The clock video follows while this plays. Its `set` restarts the
    /// audio at that time, and its `play` and `pause` start and stop it.
    fn clock(&self) -> Arc<dyn Clock>;
    /// Whether at least `ahead` of audio from the clock's time is queued
    /// to play. Never waits: the UI thread asks it every frame of a
    /// preroll.
    fn ready(&self, ahead: Duration) -> bool;
    /// Start decoding, with clip time 0 at `origin` in the file's time:
    /// the first video stream's first frame, which its decode worker
    /// gives once it opens (`None` if it failed to). Only the first call
    /// counts.
    fn start(&self, _origin: Option<Time>) {}
    /// Play on the system clock from the time shown at `at`, without
    /// waiting for the audio: a preroll waited [`PREROLL_LIMIT`].
    fn fall_back(&self, _at: Instant) {}
    /// The output's numbers, if it is a device's.
    fn stats(&self) -> Option<OutputStats> {
        None
    }
}

/// The device a [`DeviceAudio`]'s outputs play on.
#[derive(Debug)]
pub enum AudioDevice {
    /// The default output device, or the null output if there is none.
    Default,
    /// The null output.
    Null { rate: u32, channels: u16 },
    /// An output made beforehand, for the next open only: the fake
    /// device's, in the A/V tests.
    Given(Mutex<Option<(Output, Sink)>>),
}

/// The factory (see the module docs).
#[derive(Debug)]
pub struct DeviceAudio {
    backend: Arc<dyn MediaBackend>,
    device: AudioDevice,
}

impl DeviceAudio {
    pub fn new(backend: Arc<dyn MediaBackend>, device: AudioDevice) -> Self {
        DeviceAudio { backend, device }
    }

    /// The next open plays on `output`, whose decode side is `sink`.
    pub fn given(backend: Arc<dyn MediaBackend>, output: Output, sink: Sink) -> Self {
        Self::new(
            backend,
            AudioDevice::Given(Mutex::new(Some((output, sink)))),
        )
    }

    fn output(&self) -> Result<(Output, Sink), PlayerError> {
        match &self.device {
            AudioDevice::Default => Ok(Output::open_device().unwrap_or_else(|e| {
                tracing::warn!("{e}: playing silently");
                Output::null(NULL_RATE, NULL_CHANNELS)
            })),
            AudioDevice::Null { rate, channels } => Ok(Output::null(*rate, *channels)),
            AudioDevice::Given(given) => given
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .ok_or_else(|| PlayerError::new("audio_failed", "the given output is in use")),
        }
    }
}

impl AudioOutputFactory for DeviceAudio {
    fn open(
        &self,
        path: &Path,
        stop: &StopToken,
    ) -> Result<Option<Arc<dyn AudioOutput>>, PlayerError> {
        // Whether the file has audio the backend can decode: open it once
        // to see, at any format.
        let check = AudioRequest {
            stream: None,
            out: AudioOutFormat {
                rate: NULL_RATE,
                channels: NULL_CHANNELS,
            },
            origin: None,
            interrupt: StopSource::new().shutdown_flag(),
        };
        match self.backend.open_audio(path, &check) {
            Ok(dec) => tracing::info!(info = ?dec.info(), "audio"),
            Err(MediaError::NoStream { .. }) => return Ok(None),
            Err(e) => return Err(PlayerError::from(&e)),
        }
        if stop.is_stopped() {
            return Ok(None);
        }
        let (output, sink) = self.output()?;
        Ok(Some(Arc::new(PlayingAudio::start(
            self.backend.clone(),
            path.to_path_buf(),
            output,
            sink,
        )?)))
    }
}

/// A [`DeviceAudio`] output: the [`Output`] and its `audio-decode`
/// thread, stopped and joined on drop.
#[derive(Debug)]
pub struct PlayingAudio {
    output: Output,
    clock: Arc<dyn Clock>,
    origin: Mutex<Option<Sender<Option<Time>>>>,
    stop: StopSource,
    thread: Option<JoinHandle<()>>,
}

impl PlayingAudio {
    fn start(
        backend: Arc<dyn MediaBackend>,
        path: PathBuf,
        output: Output,
        sink: Sink,
    ) -> Result<Self, PlayerError> {
        let (tx, rx) = mpsc::channel();
        let stop = StopSource::new();
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("audio-decode".into())
                .spawn(move || {
                    Decode {
                        backend,
                        path,
                        sink,
                        token: stop.current(),
                        stop,
                    }
                    .run(&rx);
                })
                .map_err(|e| {
                    PlayerError::new("thread_failed", format!("starting audio-decode: {e}"))
                })?
        };
        if output.kind() == OutputKind::Null {
            tracing::warn!("no audio output device: playing silently");
        }
        Ok(PlayingAudio {
            clock: output.clock(),
            output,
            origin: Mutex::new(Some(tx)),
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for PlayingAudio {
    fn drop(&mut self) {
        self.stop.shutdown();
        self.origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioOutput for PlayingAudio {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    fn ready(&self, ahead: Duration) -> bool {
        self.output.ready(ahead)
    }

    fn start(&self, origin: Option<Time>) {
        if let Some(tx) = self
            .origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = tx.send(origin);
        }
    }

    fn fall_back(&self, at: Instant) {
        self.output.fall_back(at);
    }

    fn stats(&self) -> Option<OutputStats> {
        Some(self.output.stats())
    }
}

/// `audio-decode`.
struct Decode {
    backend: Arc<dyn MediaBackend>,
    path: PathBuf,
    sink: Sink,
    stop: StopSource,
    token: StopToken,
}

impl Decode {
    fn run(self, origin: &Receiver<Option<Time>>) {
        let origin = loop {
            match origin.recv_timeout(Duration::from_millis(50)) {
                Ok(o) => break o,
                Err(RecvTimeoutError::Timeout) if !self.token.is_stopped() => {}
                Err(_) => return,
            }
        };
        let req = AudioRequest {
            stream: None,
            out: AudioOutFormat {
                rate: self.sink.rate(),
                channels: self.sink.channels(),
            },
            origin,
            interrupt: self.stop.shutdown_flag(),
        };
        match self.backend.open_audio(&self.path, &req) {
            Ok(dec) => self.decode(dec),
            Err(e) => {
                tracing::warn!("no audio from {}: {e}", self.path.display());
                self.silent();
            }
        }
    }

    /// Decode each epoch the clock starts into the ring.
    fn decode(&self, mut dec: Box<dyn spherecut_media::backend::AudioDecode>) {
        let sink = &self.sink;
        let ch = usize::from(sink.channels());
        let mut epoch = 0;
        let mut pos = 0;
        // A chunk, and how many of its frames the ring has taken.
        let mut pending: Option<(spherecut_media::backend::AudioChunk, usize)> = None;
        let mut ended = false;
        while !self.token.is_stopped() {
            let want = sink.want();
            if want.epoch != epoch {
                epoch = want.epoch;
                pending = None;
                ended = false;
                pos = want.start;
                if let Err(e) = dec.seek(want.at, &self.token) {
                    if self.token.is_stopped() {
                        return;
                    }
                    tracing::warn!("audio: seeking to {:?}: {e}", want.at);
                    sink.end(epoch, pos);
                    ended = true;
                }
                continue;
            }
            if ended {
                std::thread::sleep(FULL_SLEEP);
                continue;
            }
            let Some((chunk, taken)) = &mut pending else {
                match dec.next_chunk(&self.token) {
                    Ok(NextAudio::Chunk(c)) => pending = Some((c, 0)),
                    Ok(NextAudio::End) => {
                        sink.end(epoch, pos);
                        ended = true;
                    }
                    Ok(NextAudio::Stopped) => return,
                    Err(e) => {
                        tracing::warn!("audio: {e}");
                        sink.end(epoch, pos);
                        ended = true;
                    }
                }
                continue;
            };
            let start = chunk.start + *taken as i64;
            let n = sink.push(epoch, start, &chunk.data[*taken * ch..]);
            *taken += n;
            pos = start + n as i64;
            if *taken * ch >= chunk.data.len() {
                pending = None;
            } else if n == 0 {
                std::thread::sleep(FULL_SLEEP);
            }
        }
    }

    /// No decoder: every epoch ends where it starts, so the clock runs
    /// on silence.
    fn silent(&self) {
        let mut epoch = 0;
        while !self.token.is_stopped() {
            let want = self.sink.want();
            if want.epoch != epoch {
                epoch = want.epoch;
                self.sink.end(epoch, want.start);
            }
            std::thread::sleep(FULL_SLEEP);
        }
    }
}
