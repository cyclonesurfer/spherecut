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
//!
//! **Ending.** The output's threads are the player's ([`PlayerThreads`]):
//! `audio-decode` ends when a newer open starts or the player shuts
//! down, its reads interrupted by the shutdown, and stops the output's
//! thread as it goes, so that [`Player::join`](crate::Player::join)
//! waits for both within its budget and reports them. Dropping the
//! output stops them too, without waiting, wherever it is dropped.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_audio::{Output, OutputKind, OutputStats, Sink};
use spherecut_core::{Clock, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{AudioOutFormat, AudioRequest, MediaBackend, NextAudio};

use crate::player::PlayerError;
use crate::shared::spawn_into;

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

/// The player's threads, for an [`AudioOutputFactory`]'s: a thread
/// started or adopted here is one that
/// [`Player::join`](crate::Player::join) waits for and reports.
#[derive(Debug, Clone)]
pub struct PlayerThreads {
    handles: Sender<(String, JoinHandle<()>)>,
    shutdown: Arc<AtomicBool>,
}

impl PlayerThreads {
    pub(crate) fn new(
        handles: Sender<(String, JoinHandle<()>)>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        PlayerThreads { handles, shutdown }
    }

    /// Start a thread named `name`.
    pub fn spawn(&self, name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
        spawn_into(&self.handles, name.into(), f)
    }

    /// A thread started elsewhere, ending: the player joins it with its
    /// own.
    pub fn adopt(&self, thread: JoinHandle<()>) {
        let name = thread.thread().name().unwrap_or("unnamed").to_owned();
        let _ = self.handles.send((name, thread));
    }

    /// The player's shutdown flag, for a decoder's interrupt.
    pub fn interrupt(&self) -> Arc<AtomicBool> {
        self.shutdown.clone()
    }
}

/// Opens a file's audio for playback.
pub trait AudioOutputFactory: Send + Sync + std::fmt::Debug {
    /// The audio of `path`, ready to play on its own clock, paused at 0;
    /// `Ok(None)` when the file has none, so the player keeps its own
    /// clock. `stop` stops when a newer open starts or the player shuts
    /// down: the audio's threads, started through `threads`, end then.
    /// Blocks: the player calls it on its open worker, never the UI
    /// thread.
    fn open(
        &self,
        path: &Path,
        stop: &StopToken,
        threads: &PlayerThreads,
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
        threads: &PlayerThreads,
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
            interrupt: threads.interrupt(),
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
            (output, sink),
            stop.clone(),
            threads,
        )?)))
    }
}

/// A [`DeviceAudio`] output: the [`Output`] and its `audio-decode`
/// thread (see the module docs on how they end).
#[derive(Debug)]
pub struct PlayingAudio {
    output: Arc<Output>,
    clock: Arc<dyn Clock>,
    origin: Mutex<Option<Sender<Option<Time>>>>,
    /// Set when this is dropped: `audio-decode` ends at its next chunk.
    dropped: Arc<AtomicBool>,
    threads: PlayerThreads,
}

impl PlayingAudio {
    fn start(
        backend: Arc<dyn MediaBackend>,
        path: PathBuf,
        (output, sink): (Output, Sink),
        token: StopToken,
        threads: &PlayerThreads,
    ) -> Result<Self, PlayerError> {
        let (tx, rx) = mpsc::channel();
        let output = Arc::new(output);
        let dropped = Arc::new(AtomicBool::new(false));
        let decode = Decode {
            backend,
            path,
            sink,
            output: output.clone(),
            token,
            dropped: dropped.clone(),
            threads: threads.clone(),
        };
        threads
            .spawn("audio-decode", move || decode.run(&rx))
            .map_err(|e| {
                PlayerError::new("thread_failed", format!("starting audio-decode: {e}"))
            })?;
        if output.kind() == OutputKind::Null {
            tracing::warn!("no audio output device: playing silently");
        }
        Ok(PlayingAudio {
            clock: output.clock(),
            output,
            origin: Mutex::new(Some(tx)),
            dropped,
            threads: threads.clone(),
        })
    }
}

impl Drop for PlayingAudio {
    /// Never waits: it can be dropped on the UI thread.
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
        self.origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(t) = self.output.stop() {
            self.threads.adopt(t);
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
    output: Arc<Output>,
    /// The open's: stops with a newer open, or at shutdown.
    token: StopToken,
    dropped: Arc<AtomicBool>,
    threads: PlayerThreads,
}

impl Decode {
    fn run(self, origin: &Receiver<Option<Time>>) {
        self.play(origin);
        // The output's thread ends with this one.
        if let Some(t) = self.output.stop() {
            self.threads.adopt(t);
        }
    }

    fn stopped(&self) -> bool {
        self.token.is_stopped() || self.dropped.load(Ordering::Acquire)
    }

    fn play(&self, origin: &Receiver<Option<Time>>) {
        let origin = loop {
            match origin.recv_timeout(Duration::from_millis(50)) {
                Ok(o) => break o,
                Err(RecvTimeoutError::Timeout) if !self.stopped() => {}
                Err(_) => return,
            }
        };
        if self.stopped() {
            return;
        }
        let req = AudioRequest {
            stream: None,
            out: AudioOutFormat {
                rate: self.sink.rate(),
                channels: self.sink.channels(),
            },
            origin,
            interrupt: self.threads.interrupt(),
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
        while !self.stopped() {
            let want = sink.want();
            if want.epoch != epoch {
                epoch = want.epoch;
                pending = None;
                ended = false;
                pos = want.start;
                if let Err(e) = dec.seek(want.at, &self.token) {
                    if self.stopped() {
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
        while !self.stopped() {
            let want = self.sink.want();
            if want.epoch != epoch {
                epoch = want.epoch;
                self.sink.end(epoch, want.start);
            }
            std::thread::sleep(FULL_SLEEP);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use spherecut_core::FrameRate;
    use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
    use spherecut_synth::fake::{SyntheticAudio, SyntheticBackend, SyntheticClip};

    #[test]
    fn dropped_on_the_ui_thread_the_output_hands_its_threads_to_the_player() {
        let (tx, rx) = mpsc::channel();
        let threads = PlayerThreads::new(tx, Arc::new(AtomicBool::new(false)));
        let clip = SyntheticClip {
            rate: FrameRate::FPS_30,
            frames: 30,
            ..SyntheticClip::default()
        };
        let backend = SyntheticBackend::with_audio(clip, SyntheticAudio::new(Time::from_secs(1)));
        let device = AudioDevice::Null {
            rate: 48_000,
            channels: 2,
        };
        let audio = DeviceAudio::new(Arc::new(backend), device)
            .open(Path::new("a.synthetic"), &StopToken::never(), &threads)
            .unwrap()
            .unwrap();
        audio.start(Some(Time::ZERO));
        std::thread::spawn(move || {
            mark_ui_thread_with(OnBlock::Panic);
            drop(audio);
            unmark_ui_thread();
        })
        .join()
        .expect("dropping it waited");
        let mut names: Vec<String> = (0..2)
            .map(|_| {
                let (name, thread) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
                thread.join().unwrap();
                name
            })
            .collect();
        names.sort();
        assert_eq!(names, ["audio-decode", "audio-null"]);
    }
}
