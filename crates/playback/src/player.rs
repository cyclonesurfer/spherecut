//! The engine's face (M1 plan sections 8.2, 8.4 and 8.5, DECISIONS D34):
//! [`Player`], which the GUI's viewer drives from its UI thread, and the
//! Tier 2 benchmarks and M2's export drive headless.
//!
//! **Never blocks.** Every method but [`Player::join`] is for the UI
//! thread and returns at once: commands go to the threads on unbounded
//! channels, and what the threads report is read with `try_lock` and
//! atomics. A lock that is taken costs a frame's freshness, never a wait:
//! the presenter shows the frame on screen one refresh longer, and the
//! snapshot is the copy from before.
//!
//! **The presenter** ([`Player::present`]), once a UI frame, picks each
//! stream's slot for the clock's time when the frame will be on screen
//! (the time now plus one UI frame, plus the A/V offset while playing):
//! the Ready slot with the latest time at or before it, which suits
//! variable frame rates. After a seek or a step it keeps the frame on
//! screen until every stream's request has landed and its frame is
//! Ready, then shows them together, sets the clock to the time and, if
//! playback is wanted (and the audio, once there is some, holds
//! [`PREROLL_AUDIO`]), starts it. Showing every stream's new frame
//! together while the old ones stay up takes two slots a stream; with
//! fewer live (a small setting, or slots out of memory), each stream's
//! new frame goes up as soon as it is Ready, which gives its old slot
//! back for the next stream's.
//! It counts what the overlay shows: UI frame times and hitches (an
//! interval over 1.5 times the median of the last ones), frames shown,
//! dropped (passed over while playing, by run places, else by the time
//! between), the interval between frames shown and the A/V offset.
//!
//! **Steps** of one frame either way are instant when the slots hold the
//! frame next to the one shown in its run (which the upload thread sees
//! to while paused): it is shown at once, so a second step before the
//! next present goes on from it. Other steps go to the workers, which
//! follow the cache's run links. A step made while a seek or step is on
//! its way goes on from where that lands: its frames add to a step's,
//! and a seek's time is its frame.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_core::{Clock, Rational, StopSource, Time, may_block};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    DecoderEvent, Exactness, FallbackReason, HwKind, MediaBackend, SeekMode, VideoStreamInfo,
};
use spherecut_media::hw::GpuIds;
use spherecut_render::{Gpu, GpuFrame, RenderError};
use thiserror::Error;

use crate::audio::{AudioOutput, AudioOutputFactory, PREROLL_AUDIO};
use crate::budget::{AUTO_MIN, AutoBudget, CacheBudget, read_auto_budget};
use crate::cache::{CacheStats, FrameCache};
use crate::decoder::{DecoderPreference, HwSupport};
use crate::perf::{Counter, PerfSnapshot, PerfStats, Stage};
use crate::shared::{
    BudgetStatus, Command, Place, Repaint, Request, Shared, Status, StreamStatus, Want,
    frames_between, ms, spawn_into, time_of,
};
use crate::slots::{DEFAULT_SLOTS, SlotFrame, SlotRing, SlotState, SlotStats, SlotView};
use crate::upload::{UploadCommand, UploadThread};
use crate::worker::OpenJob;

/// Why something can't open, decode or show, with the stable code of the
/// CLI's JSON contract (DECISIONS D27) where it has one.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct PlayerError {
    pub code: &'static str,
    pub message: String,
}

impl PlayerError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        PlayerError {
            code,
            message: message.into(),
        }
    }
}

impl From<&MediaError> for PlayerError {
    fn from(e: &MediaError) -> Self {
        PlayerError::new(e.code(), e.to_string())
    }
}

impl From<&RenderError> for PlayerError {
    fn from(e: &RenderError) -> Self {
        PlayerError::new(e.code(), e.to_string())
    }
}

/// What happened, for the GUI's notices and logs, oldest first from
/// [`Player::events`]. Streams are numbered by their place in what was
/// opened.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Opened {
        stream: usize,
        info: Box<VideoStreamInfo>,
    },
    OpenFailed {
        stream: usize,
        error: PlayerError,
    },
    /// Hardware decoding went on in software (D14's notice).
    FellBack {
        stream: usize,
        from: HwKind,
        reason: FallbackReason,
    },
    /// What else a decoder said: damage, a layout change, a conversion.
    Decoder {
        stream: usize,
        event: DecoderEvent,
    },
    DecodeFailed {
        stream: usize,
        error: PlayerError,
    },
    /// A stream's decoder reached its end.
    StreamEnded {
        stream: usize,
    },
    /// Playback reached the end, and paused there.
    Ended,
    /// The file's audio can't be played; video plays on its own clock.
    AudioFailed {
        error: PlayerError,
    },
    /// The GPU had no memory for a slot: playback goes on with one fewer
    /// (risk 11).
    SlotDropped {
        slot: usize,
        error: PlayerError,
    },
    /// A frame can't be uploaded; it isn't tried again.
    UploadFailed {
        stream: usize,
        time: Time,
        error: PlayerError,
    },
    /// The GPU device failed: nothing more is uploaded.
    GpuFailed {
        error: PlayerError,
    },
    /// The cache's budget holds fewer than [`crate::MIN_FRAMES`] frames
    /// of the source; it plays on.
    CacheTooSmall {
        budget: u64,
        frame_bytes: u64,
    },
}

/// The player's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerConfig {
    /// Settings, `--cache-mb` or `SPHERECUT_FRAME_CACHE_MIB`, as
    /// [`CacheBudget::choose`] picks them.
    pub cache: CacheBudget,
    /// How far ahead of the playhead to decode.
    pub ahead: Duration,
    /// GPU slots, 3 to 6.
    pub gpu_slots: u8,
    /// For the files opened after it is set.
    pub decoder: DecoderPreference,
    /// While playing, shows the frame due this much after the clock's
    /// time: video this much ahead of the audio (behind it when
    /// negative). Paused, the frame at the clock's time shows, as a
    /// frame-exact seek or step put it.
    pub av_offset_ms: i32,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        PlayerConfig {
            cache: CacheBudget::Auto,
            ahead: Duration::from_secs(1),
            gpu_slots: DEFAULT_SLOTS,
            decoder: DecoderPreference::Auto,
            av_offset_ms: 0,
        }
    }
}

/// What a player is made of.
pub struct PlayerParts {
    pub backend: Arc<dyn MediaBackend>,
    /// The device frames are uploaded to and drawn with.
    pub gpu: Gpu,
    /// The clock video plays on when a file has no audio: a
    /// `SystemClock`, or a `ManualClock` in tests and M2's export.
    pub clock: Arc<dyn Clock>,
    /// Opens a file's audio, whose clock video then follows (slice 6).
    pub audio: Option<Arc<dyn AudioOutputFactory>>,
    pub repaint: Option<Repaint>,
    /// Doctor's decode checks, if the GUI ran them already; otherwise the
    /// first open that needs them runs them, on its own thread.
    pub hw: Option<HwSupport>,
    /// The render node hardware decoding uses (`--render-node`), for
    /// doctor's checks; otherwise the GPU's.
    pub render_node: Option<PathBuf>,
}

impl std::fmt::Debug for PlayerParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayerParts")
            .field("backend", &self.backend.name())
            .field("clock", &self.clock)
            .field("audio", &self.audio)
            .field("hw", &self.hw)
            .field("render_node", &self.render_node)
            .finish_non_exhaustive()
    }
}

impl PlayerParts {
    /// Parts with no audio, no repaint and doctor's checks left to run.
    pub fn new(backend: Arc<dyn MediaBackend>, gpu: Gpu, clock: Arc<dyn Clock>) -> Self {
        PlayerParts {
            backend,
            gpu,
            clock,
            audio: None,
            repaint: None,
            hw: None,
            render_node: None,
        }
    }
}

/// Where playback is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlayState {
    /// Nothing opened.
    #[default]
    Empty,
    /// A file is being opened: no frame of it yet.
    Opening,
    /// A seek or step hasn't landed, or its preroll isn't done.
    Seeking,
    Paused,
    Playing,
    /// No stream can be decoded: the streams' errors say why.
    Failed,
}

/// A frame on screen.
#[derive(Debug, Clone)]
pub struct ShownFrame {
    pub stream: usize,
    pub slot: usize,
    pub frame: SlotFrame,
    /// What the viewer draws from. While a clone is held (until the paint
    /// callback that draws it is submitted), the slot isn't reused.
    pub gpu: Arc<GpuFrame>,
}

/// What [`Player::present`] picked.
#[derive(Debug, Clone)]
pub struct Presented {
    /// Each stream's frame on screen, in stream order; a stream without
    /// one yet is left out.
    pub frames: Vec<ShownFrame>,
    /// Whether any stream's frame changed since the last call.
    pub changed: bool,
    /// The clock time the frames were picked for.
    pub time: Time,
}

/// The player's state, for the GUI and the overlay.
#[derive(Debug, Clone)]
pub struct PlayerSnapshot {
    pub state: PlayState,
    pub path: Option<PathBuf>,
    /// Bumped by every open.
    pub epoch: u64,
    /// The clock runs.
    pub playing: bool,
    /// Playback is wanted: it runs, or will once a seek lands.
    pub want_play: bool,
    pub at_end: bool,
    pub clock: Time,
    /// The frame on screen per stream.
    pub shown: Vec<Option<SlotFrame>>,
    pub streams: Vec<StreamStatus>,
    pub has_audio: bool,
    pub cache: Option<CacheStats>,
    pub budget: Option<BudgetStatus>,
    pub slots: SlotStats,
    pub perf: PerfSnapshot,
    pub gpu_error: Option<PlayerError>,
    /// Whether what the threads report was read now; false when its lock
    /// was taken, and it is the copy from before.
    pub fresh: bool,
}

impl Default for PlayerSnapshot {
    fn default() -> Self {
        PlayerSnapshot {
            state: PlayState::Empty,
            path: None,
            epoch: 0,
            playing: false,
            want_play: false,
            at_end: false,
            clock: Time::ZERO,
            shown: Vec::new(),
            streams: Vec::new(),
            has_audio: false,
            cache: None,
            budget: None,
            slots: SlotStats::default(),
            perf: PerfSnapshot::default(),
            gpu_error: None,
            fresh: false,
        }
    }
}

/// What [`Player::join`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Threads that ended, by name.
    pub joined: Vec<String>,
    pub panicked: Vec<String>,
    /// Threads still running when the budget ran out, left detached.
    pub still_running: Vec<String>,
    pub took: Duration,
}

impl ShutdownReport {
    /// Every thread ended, and none panicked.
    pub fn is_clean(&self) -> bool {
        self.panicked.is_empty() && self.still_running.is_empty()
    }
}

/// The longest a UI frame is taken to last, for the present latency.
const MAX_LATENCY: Duration = Duration::from_millis(50);

/// UI frame intervals kept for their median.
const INTERVALS: usize = 32;

/// A request the presenter waits for.
#[derive(Debug, Clone, Copy)]
struct Target {
    id: u64,
    /// A seek's time, or stream 0's frame a step starts from.
    at: Time,
    /// A step's frames, from the frames in `Ui::step_from`.
    step: Option<i32>,
}

/// Where a stream's frame for the request the presenter waits for is.
#[derive(Debug, Clone, Copy)]
enum TargetSlot {
    /// The stream failed the request, or can't open: it shows no new
    /// frame for it.
    Out,
    /// Not landed yet, or not in a slot yet.
    Waiting,
    Ready(SlotView),
}

/// A slot the presenter shows, and its frame for the viewer.
#[derive(Debug, Clone)]
struct Shown {
    view: SlotView,
    gpu: Arc<GpuFrame>,
}

/// What the presenter reads of a stream's status, each frame it can.
#[derive(Debug, Clone, Copy, Default)]
struct StreamView {
    opened: bool,
    error: bool,
    failed_request: Option<u64>,
    landed: Option<crate::shared::Landed>,
    end: Option<Time>,
}

/// The UI thread's own state.
struct Ui {
    config: PlayerConfig,
    /// The latest open, its path and its workers' channels.
    epoch: u64,
    path: Option<PathBuf>,
    workers: Vec<Sender<Command>>,
    last_id: u64,
    target: Option<Target>,
    /// Each stream's frame the step the presenter waits for starts from.
    step_from: Vec<Time>,
    /// A frame changed outside `present` (an instant step), which the
    /// next `present` reports.
    unpresented: bool,
    /// The target is shown, and playback waits for the audio.
    preroll: bool,
    want_play: bool,
    at_end: bool,
    /// The clock video plays on now: the player's, or the open file's
    /// audio's.
    clock: Arc<dyn Clock>,
    audio: Option<Arc<dyn AudioOutput>>,
    shown: Vec<Option<Shown>>,
    /// The status as last read: its open, and each stream's part.
    view_epoch: u64,
    view: Vec<StreamView>,
    last_present: Option<Instant>,
    intervals: VecDeque<f32>,
    /// When stream 0's frame last changed while playing.
    last_new: Option<Instant>,
    /// The playhead the presenter last gave the cache.
    playhead: Option<Time>,
    snapshot: PlayerSnapshot,
    /// The open the snapshot's streams are about.
    snapshot_epoch: u64,
}

impl Ui {
    fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }

    /// The median of the last UI frame intervals, in milliseconds.
    fn median_interval(&self) -> Option<f32> {
        let mut v: Vec<f32> = self.intervals.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).map(|m| m.max(0.0))
    }

    /// The median UI frame interval, at most [`MAX_LATENCY`]: how long
    /// until a frame presented now is on screen.
    fn latency(&self) -> Duration {
        self.median_interval().map_or(Duration::ZERO, |m| {
            Duration::from_secs_f32(m / 1000.0).min(MAX_LATENCY)
        })
    }
}

/// The playback engine. Make it, open a file, and call
/// [`present`](Player::present) once a UI frame.
pub struct Player {
    shared: Arc<Shared>,
    backend: Arc<dyn MediaBackend>,
    audio: Option<Arc<dyn AudioOutputFactory>>,
    discrete: bool,
    gpu_ids: GpuIds,
    render_node: Option<PathBuf>,
    base_clock: Arc<dyn Clock>,
    upload: Sender<UploadCommand>,
    handles_tx: Sender<(String, JoinHandle<()>)>,
    handles: Receiver<(String, JoinHandle<()>)>,
    events: Receiver<PlayerEvent>,
    ui: RefCell<Ui>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("backend", &self.backend.name())
            .finish_non_exhaustive()
    }
}

impl Player {
    /// A player with nothing open, and its upload thread started. Never
    /// waits: the upload thread builds its pipelines and reads the
    /// memory available on its own.
    pub fn new(parts: PlayerParts, config: PlayerConfig) -> Result<Player, PlayerError> {
        Self::with_memory(parts, config, read_auto_budget)
    }

    /// [`new`](Self::new), with the memory available read by `read`.
    pub(crate) fn with_memory(
        parts: PlayerParts,
        config: PlayerConfig,
        read: fn() -> AutoBudget,
    ) -> Result<Player, PlayerError> {
        let (events_tx, events) = mpsc::channel();
        let (handles_tx, handles) = mpsc::channel();
        let (upload, upload_rx) = mpsc::channel();
        let budget = match config.cache {
            CacheBudget::Bytes(b) => b,
            // Until the upload thread has read the memory available.
            CacheBudget::Auto => AUTO_MIN,
        };
        let shared = Arc::new(Shared {
            cache: Arc::new(FrameCache::new(budget, time_of(config.ahead))),
            slots: Arc::new(SlotRing::new(config.gpu_slots)),
            perf: Arc::new(PerfStats::new()),
            status: Mutex::new(Status::default()),
            stop: StopSource::new(),
            opens: StopSource::new(),
            epoch: AtomicU64::new(0),
            live: AtomicU64::new(0),
            target: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            open_lock: Mutex::new(()),
            hw: Mutex::new(parts.hw),
            events: events_tx,
            handles: handles_tx.clone(),
            repaint: parts.repaint,
        });
        let info = &parts.gpu.caps.info;
        let discrete = info.device_type == spherecut_render::wgpu::DeviceType::DiscreteGpu;
        let gpu_ids = GpuIds {
            pci_bus_id: info.device_pci_bus_id.clone(),
            vendor: info.vendor,
            device: info.device,
        };
        let thread = UploadThread {
            shared: shared.clone(),
            gpu: parts.gpu,
            rx: upload_rx,
            configured: config.cache,
            read,
        };
        spawn_into(&handles_tx, "upload".into(), move || thread.run()).map_err(|e| {
            PlayerError::new("thread_failed", format!("starting the upload thread: {e}"))
        })?;
        let clock = parts.clock;
        Ok(Player {
            shared,
            backend: parts.backend,
            audio: parts.audio,
            discrete,
            gpu_ids,
            render_node: parts.render_node,
            base_clock: clock.clone(),
            upload,
            handles_tx,
            handles,
            events,
            ui: RefCell::new(Ui {
                config,
                epoch: 0,
                path: None,
                workers: Vec::new(),
                last_id: 0,
                target: None,
                step_from: Vec::new(),
                unpresented: false,
                preroll: false,
                want_play: false,
                at_end: false,
                clock,
                audio: None,
                shown: Vec::new(),
                view_epoch: 0,
                view: Vec::new(),
                last_present: None,
                intervals: VecDeque::with_capacity(INTERVALS),
                last_new: None,
                playhead: None,
                snapshot: PlayerSnapshot::default(),
                snapshot_epoch: 0,
            }),
        })
    }

    /// Open `path`'s best video stream, paused at its first frame.
    pub fn open(&self, path: PathBuf) {
        self.open_streams(path, vec![None]);
    }

    /// Open these video streams of `path`, by their indices in the file
    /// (`None` for the best), paused at their first frames.
    pub fn open_streams(&self, path: PathBuf, streams: Vec<Option<usize>>) {
        let now = Instant::now();
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return;
        };
        if self.shared.is_shut_down() || streams.is_empty() {
            return;
        }
        ui.epoch += 1;
        let epoch = ui.epoch;
        self.shared.epoch.store(epoch, Ordering::Release);
        let token = self.shared.stop.next();
        for w in ui.workers.drain(..) {
            let _ = w.send(Command::Close);
        }
        self.shared.cache.notify();
        for s in std::mem::take(&mut ui.shown).into_iter().flatten() {
            self.shared.slots.unshow(s.view.index, s.view.word);
        }
        ui.shown = vec![None; streams.len()];
        ui.clock = self.base_clock.clone();
        ui.clock.pause(now);
        ui.clock.set(Time::ZERO, now);
        ui.audio = None;
        ui.want_play = false;
        ui.at_end = false;
        ui.preroll = false;
        ui.view_epoch = 0;
        ui.view.clear();
        ui.playhead = None;
        ui.last_new = None;
        ui.path = Some(path.clone());
        let id = ui.next_id();
        ui.target = Some(Target {
            id,
            at: Time::ZERO,
            step: None,
        });
        self.shared.target.store(id, Ordering::Release);
        self.shared.playing.store(false, Ordering::Release);
        let mut channels = Vec::with_capacity(streams.len());
        for asked in streams {
            let (tx, rx) = mpsc::channel();
            let _ = tx.send(Command::Request(Request {
                id,
                want: Want::Seek {
                    at: Time::ZERO,
                    mode: SeekMode::Exact,
                },
                token: token.clone(),
            }));
            ui.workers.push(tx);
            channels.push((asked, rx));
        }
        let job = OpenJob {
            shared: self.shared.clone(),
            backend: self.backend.clone(),
            audio: self.audio.clone(),
            path,
            epoch,
            streams: channels,
            decoder: ui.config.decoder,
            token,
            open_token: self.shared.opens.next(),
            clock: self.base_clock.clone(),
            discrete: self.discrete,
            gpu_ids: self.gpu_ids.clone(),
            render_node: self.render_node.clone(),
        };
        if let Err(e) = spawn_into(&self.handles_tx, "open".into(), move || job.run()) {
            self.shared.event(PlayerEvent::OpenFailed {
                stream: 0,
                error: PlayerError::new("thread_failed", format!("starting the open: {e}")),
            });
        }
    }

    /// Play, from the start when at the end. A seek in flight plays once
    /// it lands.
    pub fn play(&self) {
        let now = Instant::now();
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return;
        };
        if ui.path.is_none() {
            return;
        }
        ui.want_play = true;
        self.shared.playing.store(true, Ordering::Release);
        if ui.at_end {
            self.seek_with(&mut ui, Time::ZERO, SeekMode::Exact, now);
        }
        if ui.target.is_none() && !ui.preroll {
            ui.clock.play(now);
        }
    }

    pub fn pause(&self) {
        let now = Instant::now();
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return;
        };
        Self::pause_ui(&self.shared, &mut ui, now);
    }

    fn pause_ui(shared: &Shared, ui: &mut Ui, now: Instant) {
        ui.want_play = false;
        ui.preroll = false;
        shared.playing.store(false, Ordering::Release);
        ui.clock.pause(now);
    }

    /// Show the frame at `at`: [`SeekMode::Exact`], or the keyframe a seek
    /// lands on for scrubbing. Playback, if it runs, goes on from there
    /// once the frame is ready.
    pub fn seek(&self, at: Time, mode: SeekMode) {
        let now = Instant::now();
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return;
        };
        if ui.path.is_some() {
            self.seek_with(&mut ui, at, mode, now);
        }
    }

    fn seek_with(&self, ui: &mut Ui, at: Time, mode: SeekMode, now: Instant) {
        let shared = &self.shared;
        let token = shared.stop.next();
        let id = ui.next_id();
        shared.perf.add(Counter::Seeks, 1);
        if ui.target.is_some() {
            shared.perf.add(Counter::SeeksSuperseded, 1);
        }
        ui.target = Some(Target { id, at, step: None });
        ui.preroll = false;
        ui.at_end = false;
        shared.target.store(id, Ordering::Release);
        ui.clock.pause(now);
        ui.clock.set(at, now);
        for w in &ui.workers {
            let _ = w.send(Command::Request(Request {
                id,
                want: Want::Seek { at, mode },
                token: token.clone(),
            }));
        }
        shared.cache.notify();
        Self::give_playhead(shared, ui, at);
    }

    /// Pause, and show the frame `frames` on (back, when negative) from
    /// the one shown, or from where the seek or step on its way lands: a
    /// step's frames add to it, and a seek's time is the frame shown
    /// there (where an exact seek lands).
    pub fn step(&self, frames: i32) {
        let now = Instant::now();
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return;
        };
        let ui = &mut *ui;
        if frames == 0 || ui.path.is_none() {
            return;
        }
        Self::pause_ui(&self.shared, ui, now);
        ui.at_end = false;
        if frames.abs() == 1
            && ui.target.is_none()
            && let Some(next) = self.adjacent(ui, frames)
        {
            // Shown now, so that the next step goes on from it though no
            // present came between.
            for (i, v) in next.iter().enumerate() {
                ui.unpresented |= self.show(ui, i, v, now, false);
            }
            if let Some(t) = next.first().map(|v| v.frame.time) {
                ui.clock.set(t, now);
                Self::give_playhead(&self.shared, ui, t);
            }
            return;
        }
        let token = self.shared.stop.next();
        let id = ui.next_id();
        let clock = ui.clock.time_at(now);
        let n = ui.workers.len();
        let (frames, from): (i32, Vec<Time>) = match ui.target {
            Some(Target { step: Some(f), .. }) => (f.saturating_add(frames), ui.step_from.clone()),
            Some(Target { at, step: None, .. }) => (frames, vec![at; n]),
            None => (
                frames,
                (0..n)
                    .map(|i| {
                        ui.shown
                            .get(i)
                            .and_then(|s| s.as_ref())
                            .map_or(clock, |s| s.view.frame.time)
                    })
                    .collect(),
            ),
        };
        ui.target = Some(Target {
            id,
            at: from.first().copied().unwrap_or(clock),
            step: Some(frames),
        });
        ui.step_from.clone_from(&from);
        self.shared.target.store(id, Ordering::Release);
        for (w, from) in ui.workers.iter().zip(from) {
            let _ = w.send(Command::Request(Request {
                id,
                want: Want::Step { frames, from },
                token: token.clone(),
            }));
        }
        self.shared.cache.notify();
    }

    /// The slots of the frames next to each shown one in its run, `step`
    /// (±1) away, if every stream's is Ready.
    fn adjacent(&self, ui: &Ui, step: i32) -> Option<Vec<SlotView>> {
        let views = self.shared.slots.views();
        ui.shown
            .iter()
            .enumerate()
            .map(|(i, shown)| {
                let s = shown.as_ref()?.view.frame;
                let seq = s.seq.checked_add_signed(i64::from(step))?;
                views
                    .iter()
                    .find(|v| {
                        v.state == SlotState::Ready
                            && v.frame.epoch == ui.epoch
                            && v.frame.stream == i
                            && v.frame.run == s.run
                            && v.frame.seq == seq
                    })
                    .copied()
            })
            .collect::<Option<Vec<_>>>()
            .filter(|next| !next.is_empty())
    }

    /// The frames to draw now, picked for the clock's time when they will
    /// be on screen: `now` plus one UI frame. Call it once a UI frame.
    /// Never waits.
    pub fn present(&self, now: Instant) -> Option<Presented> {
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return None;
        };
        let ui = &mut *ui;
        let shared = &self.shared;
        let perf = &shared.perf;
        if let Some(prev) = ui.last_present.replace(now) {
            let interval = now.saturating_duration_since(prev).as_secs_f32() * 1000.0;
            perf.record(Stage::UiFrame, interval);
            // Against the intervals as they are: a steady UI slower than
            // the latency's cap has no hitches.
            if ui.intervals.len() >= INTERVALS / 2
                && let Some(median) = ui.median_interval()
                && interval > 1.5 * median
            {
                perf.add(Counter::Hitches, 1);
            }
            if ui.intervals.len() == INTERVALS {
                ui.intervals.pop_front();
            }
            ui.intervals.push_back(interval);
        }
        self.read_status(ui);
        if ui.view_epoch != ui.epoch || ui.path.is_none() {
            return None;
        }
        let offset =
            Time(Rational::new(i64::from(ui.config.av_offset_ms), 1000).unwrap_or(Rational::ZERO));
        // The offset applies while playing: paused, the frame at the
        // clock's time shows, as a frame-exact seek or step put it.
        let at = |ui: &Ui| {
            let t = ui.clock.time_at(now + ui.latency());
            if ui.clock.is_running() {
                t.checked_add(offset).unwrap_or(t)
            } else {
                t
            }
        };
        let mut changed = std::mem::take(&mut ui.unpresented);
        if let Some(target) = ui.target {
            if let Some(found) = self.target_slots(ui, target) {
                if found.iter().all(|f| !matches!(f, TargetSlot::Waiting)) {
                    let picked = found
                        .iter()
                        .map(|f| match f {
                            TargetSlot::Ready(v) => Some(*v),
                            _ => None,
                        })
                        .collect();
                    changed |= self.show_target(ui, target, picked, now);
                } else if shared.slots.stats().active < 2 * ui.shown.len() {
                    // Too few slots for every stream's frame on screen and
                    // its new one at once: each stream's goes up as it
                    // comes, which gives its old slot back for the next.
                    for (i, f) in found.iter().enumerate() {
                        if let TargetSlot::Ready(v) = f {
                            changed |= self.show(ui, i, v, now, false);
                        }
                    }
                }
            }
        } else {
            if ui.preroll
                && ui.want_play
                && ui.audio.as_ref().is_none_or(|a| a.ready(PREROLL_AUDIO))
            {
                ui.preroll = false;
                ui.clock.play(now);
            }
            let t = at(ui);
            changed |= self.pick(ui, t, now);
            self.check_end(ui, t, now);
        }
        let t = at(ui);
        let running = ui.clock.is_running();
        if running
            && let Some(Some(s)) = ui.shown.first()
            && let Ok(d) = t.checked_sub(s.view.frame.time)
        {
            perf.record(Stage::AvOffset, ms(d));
        }
        // The playhead: the clock's time, or a seek's while it is on its
        // way. A step's moves with the worker, frame by frame.
        let playhead = match ui.target {
            None => Some(t),
            Some(target) if target.step.is_none() => Some(target.at),
            Some(_) => None,
        };
        if let Some(playhead) = playhead
            && ui.playhead != Some(playhead)
        {
            Self::give_playhead(shared, ui, playhead);
        }
        let frames: Vec<ShownFrame> = ui
            .shown
            .iter()
            .enumerate()
            .filter_map(|(stream, s)| {
                let s = s.as_ref()?;
                Some(ShownFrame {
                    stream,
                    slot: s.view.index,
                    frame: s.view.frame,
                    gpu: s.gpu.clone(),
                })
            })
            .collect();
        (!frames.is_empty()).then_some(Presented {
            frames,
            changed,
            time: t,
        })
    }

    /// Give the cache the playhead `at`, if its lock is free. If it
    /// isn't, the next [`present`](Self::present) tries again, and a
    /// repaint is asked for so that one comes: a paused GUI draws no
    /// other, and the upload thread would go on choosing frames around
    /// the old playhead.
    fn give_playhead(shared: &Shared, ui: &mut Ui, at: Time) {
        if shared.cache.try_set_playhead(at) {
            ui.playhead = Some(at);
        } else {
            ui.playhead = None;
            shared.repaint();
        }
    }

    /// Read what the presenter needs of the status, if its lock is free.
    fn read_status(&self, ui: &mut Ui) {
        let Some(st) = self.shared.try_status() else {
            return;
        };
        if st.epoch != ui.epoch {
            return;
        }
        ui.view_epoch = st.epoch;
        ui.view.resize(st.streams.len(), StreamView::default());
        for (v, s) in ui.view.iter_mut().zip(&st.streams) {
            *v = StreamView {
                opened: s.opened,
                error: s.error.is_some(),
                failed_request: s.failed_request,
                landed: s.landed,
                end: s.end.or_else(|| s.info.as_ref().and_then(|i| i.duration)),
            };
        }
        if ui.audio.is_none()
            && let Some(audio) = &st.audio
        {
            // The open's audio: video follows its clock from now on.
            let now = Instant::now();
            let clock = audio.clock();
            clock.set(ui.clock.time_at(now), now);
            ui.clock = clock;
            ui.audio = Some(audio.clone());
        }
    }

    /// Where each stream's frame for `target` is; `None` until the
    /// status of this open has been read.
    fn target_slots(&self, ui: &Ui, target: Target) -> Option<Vec<TargetSlot>> {
        if ui.view.len() != ui.shown.len() {
            return None;
        }
        let views = self.shared.slots.views();
        let found = ui.view.iter().enumerate().map(|(i, s)| {
            if s.failed_request == Some(target.id) || (!s.opened && s.error) {
                return TargetSlot::Out;
            }
            s.landed
                .filter(|l| l.id == target.id)
                .and_then(|landed| {
                    views.iter().find(|v| {
                        v.frame.epoch == ui.epoch
                            && v.frame.stream == i
                            && v.frame.time == landed.time
                    })
                })
                .map_or(TargetSlot::Waiting, |v| TargetSlot::Ready(*v))
        });
        Some(found.collect())
    }

    /// Show the target's frames, set the clock to its time, and play if
    /// that is wanted. Whether a frame changed.
    fn show_target(
        &self,
        ui: &mut Ui,
        target: Target,
        picked: Vec<Option<SlotView>>,
        now: Instant,
    ) -> bool {
        let mut changed = false;
        for (i, v) in picked.iter().enumerate() {
            if let Some(v) = v {
                changed |= self.show(ui, i, v, now, false);
            }
        }
        let landed = ui
            .view
            .iter()
            .find_map(|s| s.landed.filter(|l| l.id == target.id));
        if let Some(l) = landed {
            let t = if target.step.is_some()
                || l.time > target.at
                || matches!(
                    l.exactness,
                    Some(
                        Exactness::AfterLast
                            | Exactness::Truncated { .. }
                            | Exactness::KeyframeOnly
                    )
                ) {
                l.time
            } else {
                target.at
            };
            ui.clock.set(t, now);
        }
        ui.target = None;
        self.shared.target.store(0, Ordering::Release);
        ui.playhead = None;
        if ui.want_play {
            if ui.audio.as_ref().is_none_or(|a| a.ready(PREROLL_AUDIO)) {
                ui.clock.play(now);
            } else {
                ui.preroll = true;
            }
        }
        self.shared.repaint();
        changed
    }

    /// Show each stream's latest Ready frame at or before `t`. Whether a
    /// frame changed.
    fn pick(&self, ui: &mut Ui, t: Time, now: Instant) -> bool {
        let views = self.shared.slots.views();
        let mut changed = false;
        for i in 0..ui.shown.len() {
            let best = views
                .iter()
                .filter(|v| v.frame.epoch == ui.epoch && v.frame.stream == i && v.frame.time <= t)
                .max_by_key(|v| v.frame.time);
            if let Some(v) = best {
                changed |= self.show(ui, i, v, now, true);
            }
        }
        changed
    }

    /// Show slot `v` for stream `i`, giving back the one shown before.
    /// `count`: whether frames passed over count as dropped.
    fn show(&self, ui: &mut Ui, i: usize, v: &SlotView, now: Instant, count: bool) -> bool {
        let slots = &self.shared.slots;
        let perf = &self.shared.perf;
        let before = ui.shown.get(i).and_then(|s| s.as_ref()).map(|s| s.view);
        if before.is_some_and(|b| b.index == v.index && b.frame == v.frame) {
            return false;
        }
        let Some(gpu) = slots.try_show(v) else {
            return false;
        };
        // Kept as read: giving it back compares against its version.
        let view = *v;
        if let Some(b) = before {
            slots.unshow(b.index, b.word);
        }
        let running = ui.clock.is_running();
        if count
            && running
            && let Some(b) = before
            && v.frame.time > b.frame.time
        {
            perf.add(
                Counter::Dropped,
                frames_between(&Place::of_slot(&b.frame), &Place::of_slot(&v.frame)),
            );
        }
        perf.add(Counter::FramesShown, 1);
        if i == 0 {
            if running && let Some(last) = ui.last_new {
                perf.record(
                    Stage::ShownInterval,
                    now.saturating_duration_since(last).as_secs_f32() * 1000.0,
                );
            }
            ui.last_new = running.then_some(now);
        }
        if let Some(s) = ui.shown.get_mut(i) {
            *s = Some(Shown { view, gpu });
        }
        true
    }

    /// Pause at the end, once the clock passes the first stream's.
    fn check_end(&self, ui: &mut Ui, t: Time, now: Instant) {
        if !ui.clock.is_running() {
            return;
        }
        let Some(end) = ui.view.first().and_then(|s| s.end) else {
            return;
        };
        if t >= end {
            Self::pause_ui(&self.shared, ui, now);
            if let Some(Some(s)) = ui.shown.first() {
                ui.clock.set(s.view.frame.time, now);
            }
            ui.at_end = true;
            self.shared.event(PlayerEvent::Ended);
        }
    }

    /// The player's state. Never waits: when the threads' status is
    /// locked, their part is the copy from before.
    pub fn snapshot(&self) -> PlayerSnapshot {
        let status = self.shared.try_status();
        self.snapshot_from(status.as_deref())
    }

    /// [`snapshot`](Self::snapshot), waiting for the status lock: the
    /// negative control of the UI thread's guard (M1 plan section 8.6),
    /// which must panic on a marked thread.
    #[doc(hidden)]
    #[track_caller]
    pub fn snapshot_blocking(&self) -> PlayerSnapshot {
        let status = self.shared.status();
        self.snapshot_from(Some(&status))
    }

    fn snapshot_from(&self, status: Option<&Status>) -> PlayerSnapshot {
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return PlayerSnapshot::default();
        };
        let ui = &mut *ui;
        let now = Instant::now();
        let shared = &self.shared;
        let mut s = std::mem::take(&mut ui.snapshot);
        s.fresh = status.is_some();
        if let Some(st) = status {
            s.budget = st.budget;
            s.gpu_error = st.gpu_error.clone();
            s.streams = st.streams.clone();
            s.has_audio = st.audio.is_some();
            ui.snapshot_epoch = st.epoch;
        }
        s.epoch = ui.epoch;
        let current = ui.snapshot_epoch == ui.epoch;
        if !current {
            s.streams.clear();
            s.has_audio = false;
        }
        s.path = ui.path.clone();
        s.playing = ui.clock.is_running();
        s.want_play = ui.want_play;
        s.at_end = ui.at_end;
        s.clock = ui.clock.time_at(now);
        s.shown = ui
            .shown
            .iter()
            .map(|x| x.as_ref().map(|x| x.view.frame))
            .collect();
        if let Some(c) = shared.cache.try_stats() {
            s.cache = Some(c);
        }
        s.slots = shared.slots.stats();
        s.perf = shared.perf.snapshot();
        s.state = if ui.path.is_none() {
            PlayState::Empty
        } else if !current || s.streams.is_empty() {
            PlayState::Opening
        } else if s.streams.iter().all(|x| x.error.is_some()) {
            PlayState::Failed
        } else if s.streams.iter().any(|x| !x.opened && x.error.is_none()) {
            PlayState::Opening
        } else if ui.target.is_some() || ui.preroll {
            PlayState::Seeking
        } else if s.playing {
            PlayState::Playing
        } else {
            PlayState::Paused
        };
        ui.snapshot = s.clone();
        s
    }

    /// What happened since the last call, oldest first. Never waits.
    pub fn events(&self) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            out.push(e);
        }
        out
    }

    /// New settings: the budget and the window ahead at once (on the
    /// upload thread), the slot count as slots come free, the decoder for
    /// the next open, the A/V offset from the next frame.
    pub fn set_config(&self, config: PlayerConfig) {
        let Ok(mut ui) = self.ui.try_borrow_mut() else {
            return;
        };
        if config.cache != ui.config.cache {
            let _ = self.upload.send(UploadCommand::Budget(config.cache));
        }
        if config.ahead != ui.config.ahead {
            let _ = self.upload.send(UploadCommand::Ahead(config.ahead));
        }
        self.shared.slots.set_active(config.gpu_slots);
        ui.config = config;
    }

    pub fn config(&self) -> PlayerConfig {
        self.ui
            .try_borrow()
            .map(|ui| ui.config.clone())
            .unwrap_or_default()
    }

    /// The GPU slots, for the viewer's paint callbacks and the tests.
    pub fn slots(&self) -> &Arc<SlotRing> {
        &self.shared.slots
    }

    pub fn cache(&self) -> &Arc<FrameCache> {
        &self.shared.cache
    }

    pub fn perf(&self) -> &Arc<PerfStats> {
        &self.shared.perf
    }

    /// Stop every thread, and return at once: the threads end on their
    /// own, within a frame or a read. [`join`](Self::join) waits for them.
    pub fn begin_shutdown(&self) {
        let shared = &self.shared;
        if shared.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        shared.stop.shutdown();
        shared.opens.shutdown();
        if let Ok(mut ui) = self.ui.try_borrow_mut() {
            for w in ui.workers.drain(..) {
                let _ = w.send(Command::Close);
            }
            ui.clock.pause(Instant::now());
        }
        shared.cache.notify();
    }

    /// Shut down, and wait as long as `budget` for every thread to end.
    /// Blocks: off the UI thread (after eframe's `run_native` returns, or
    /// in a headless caller).
    #[track_caller]
    pub fn join(self, budget: Duration) -> ShutdownReport {
        may_block("joining the player's threads");
        self.begin_shutdown();
        let start = Instant::now();
        let deadline = start + budget;
        let mut report = ShutdownReport::default();
        let mut running: Vec<(String, JoinHandle<()>)> = Vec::new();
        loop {
            while let Ok(h) = self.handles.try_recv() {
                running.push(h);
            }
            let mut i = 0;
            while i < running.len() {
                if running[i].1.is_finished() {
                    let (name, handle) = running.swap_remove(i);
                    match handle.join() {
                        Ok(()) => report.joined.push(name),
                        Err(_) => report.panicked.push(name),
                    }
                } else {
                    i += 1;
                }
            }
            // Every thread that could start another has ended, and sent
            // its handles before it did.
            if running.is_empty() {
                match self.handles.try_recv() {
                    Ok(h) => {
                        running.push(h);
                        continue;
                    }
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        report.still_running = running.into_iter().map(|(name, _)| name).collect();
        report.took = start.elapsed();
        if !report.is_clean() {
            tracing::warn!(
                "player shutdown: {:?} still running, {:?} panicked",
                report.still_running,
                report.panicked
            );
        }
        report
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.begin_shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_is_send() {
        fn send<T: Send>() {}
        send::<Player>();
    }

    #[test]
    fn a_playhead_the_cache_was_too_busy_for_asks_for_a_repaint() {
        use std::sync::atomic::AtomicUsize;

        use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
        use spherecut_core::{FrameRate, ManualClock};
        use spherecut_synth::fake::{SyntheticBackend, SyntheticClip};

        // The repaints asked for on this thread, the presenter's; the
        // upload thread asks for its own as frames come up.
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let me = std::thread::current().id();
        let clock = Arc::new(ManualClock::new(FrameRate::FPS_30.frame_duration()));
        let gpu = spherecut_render::Gpu::headless(&spherecut_render::AdapterChoice::Cpu)
            .expect("lavapipe");
        let backend = SyntheticBackend::new(SyntheticClip {
            frames: 30,
            ..SyntheticClip::default()
        });
        let parts = PlayerParts {
            repaint: Some(Arc::new(move || {
                if std::thread::current().id() == me {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            })),
            ..PlayerParts::new(Arc::new(backend), gpu, clock.clone())
        };
        let config = PlayerConfig {
            decoder: DecoderPreference::Software,
            ..PlayerConfig::default()
        };
        let player = Player::new(parts, config).expect("a player");
        player.open("clip.synthetic".into());
        let start = Instant::now();
        while player.present(Instant::now()).is_none() {
            assert!(start.elapsed() < Duration::from_secs(20), "no first frame");
            std::thread::sleep(Duration::from_millis(1));
        }
        // Half a frame on, paused: the same frame stays up, and nothing
        // but the playhead changes, so a GUI would draw no other frame.
        let half = Time::seconds(Rational::new(1, 60).expect("1/60"));
        clock.set(half, Instant::now());
        let before = player.cache().playhead();
        let asked_before = asked.load(Ordering::SeqCst);
        {
            let _held = player.cache().hold_lock();
            mark_ui_thread_with(OnBlock::Panic);
            let shown = player.present(Instant::now());
            unmark_ui_thread();
            assert!(shown.is_some());
        }
        assert_eq!(player.cache().playhead(), before);
        assert_eq!(
            asked.load(Ordering::SeqCst),
            asked_before + 1,
            "a lost playhead asked for no repaint"
        );
        // The next frame gives it.
        let start = Instant::now();
        while player.cache().playhead() != half {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the playhead stayed"
            );
            let _ = player.present(Instant::now());
            std::thread::sleep(Duration::from_millis(1));
        }
        join(player);
    }

    /// Shut `player` down, failing with the report unless every thread
    /// ends in 10 s: under load the upload thread on lavapipe takes a
    /// while (tests/common's `JOIN`).
    fn join(player: Player) {
        let report = player.join(Duration::from_secs(10));
        assert!(report.is_clean(), "{report:?}");
    }

    #[test]
    fn two_streams_land_with_a_slot_dead() {
        use spherecut_core::{FrameRate, ManualClock};
        use spherecut_synth::fake::{SyntheticBackend, SyntheticClip};

        // Four slots, one of them dead before the open (risk 11: playback
        // goes on with one fewer). Two streams' frames on screen and the
        // two new ones need four, so each stream's goes up as it comes.
        let rate = FrameRate::FPS_30;
        let at = |k: i64| rate.frame_to_time(k).expect("a frame time");
        let gpu = spherecut_render::Gpu::headless(&spherecut_render::AdapterChoice::Cpu)
            .expect("lavapipe");
        let backend = SyntheticBackend::new(SyntheticClip {
            frames: 90,
            ..SyntheticClip::default()
        });
        let parts = PlayerParts::new(
            Arc::new(backend),
            gpu,
            Arc::new(ManualClock::new(rate.frame_duration())),
        );
        let config = PlayerConfig {
            cache: CacheBudget::Bytes(64 << 20),
            decoder: DecoderPreference::Software,
            ..PlayerConfig::default()
        };
        let player = Player::new(parts, config).expect("a player");
        let claim = player.shared.slots.claim().expect("a free slot");
        player.shared.slots.kill(claim);
        assert_eq!(player.slots().stats().active, 3);
        player.open_streams("two.synthetic".into(), vec![Some(0), Some(1)]);
        let shows = |k: i64| {
            let start = Instant::now();
            loop {
                let shown = player
                    .present(Instant::now())
                    .map(|p| p.frames.iter().map(|f| f.frame.pts).collect::<Vec<_>>());
                if shown.as_deref() == Some(&[k, k][..]) {
                    return;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "frame {k} never came up: showing {shown:?}, slots {:?}",
                    player.slots().views()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        shows(0);
        player.seek(at(40), SeekMode::Exact);
        shows(40);
        player.step(1);
        shows(41);
        player.step(-3);
        shows(38);
        join(player);
    }

    #[test]
    fn two_streams_keyframe_seek_back_with_a_slot_dead_and_the_cache_full() {
        use spherecut_core::{Clock, FrameRate, ManualClock};
        use spherecut_synth::fake::{SyntheticBackend, SyntheticClip};

        // Four slots, one dead, a 16-frame cache full after playing: a
        // keyframe seek back shows each stream's keyframe as it comes, and
        // the one up first must stay in the cache until the other is.
        let rate = FrameRate::FPS_30;
        let at = |k: i64| rate.frame_to_time(k).expect("a frame time");
        let frame = 64 * 32 * 3 / 2;
        let gpu = spherecut_render::Gpu::headless(&spherecut_render::AdapterChoice::Cpu)
            .expect("lavapipe");
        let backend = SyntheticBackend::new(SyntheticClip {
            frames: 120,
            frame_cost: Duration::from_millis(5),
            ..SyntheticClip::default()
        });
        let clock = Arc::new(ManualClock::new(rate.frame_duration()));
        let parts = PlayerParts::new(Arc::new(backend), gpu, clock.clone());
        let config = PlayerConfig {
            cache: CacheBudget::Bytes(16 * frame),
            decoder: DecoderPreference::Software,
            ..PlayerConfig::default()
        };
        let player = Player::new(parts, config).expect("a player");
        let claim = player.shared.slots.claim().expect("a free slot");
        player.shared.slots.kill(claim);
        assert_eq!(player.slots().stats().active, 3);
        player.open_streams("two.synthetic".into(), vec![Some(0), Some(1)]);
        let shows = |k: i64| {
            let start = Instant::now();
            loop {
                let shown = player
                    .present(Instant::now())
                    .map(|p| p.frames.iter().map(|f| f.frame.pts).collect::<Vec<_>>());
                if shown.as_deref() == Some(&[k, k][..]) {
                    return;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "frame {k} never came up: showing {shown:?}, slots {:?}",
                    player.slots().views()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        shows(0);
        player.play();
        for k in 1..=60 {
            clock.set(at(k), Instant::now());
            shows(k);
        }
        player.pause();
        for (k, key) in [(45, 40), (29, 24), (21, 16)] {
            player.seek(at(k), SeekMode::Keyframe);
            shows(key);
        }
        join(player);
    }

    #[test]
    fn a_set_budget_shrinks_too_when_memory_is_short() {
        // 512 MiB available, under the 1 GiB of pressure: a recheck sets
        // a quarter of it, 128 MiB, below the 256 MiB set (D34).
        fn short() -> AutoBudget {
            AutoBudget {
                bytes: AUTO_MIN,
                mem_available: Some(512 << 20),
                cgroup_room: None,
            }
        }
        let gpu = spherecut_render::Gpu::headless(&spherecut_render::AdapterChoice::Cpu)
            .expect("lavapipe");
        let parts = PlayerParts::new(
            Arc::new(spherecut_media::backend::FfmpegBackend::new()),
            gpu,
            Arc::new(spherecut_core::ManualClock::new(
                spherecut_core::FrameRate::FPS_30.frame_duration(),
            )),
        );
        let config = PlayerConfig {
            cache: CacheBudget::Bytes(256 << 20),
            ..PlayerConfig::default()
        };
        let player = Player::with_memory(parts, config, short).expect("a player");
        let start = Instant::now();
        while player.cache().stats().budget != 128 << 20 {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "the budget stayed {}",
                player.cache().stats().budget
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let status = player.shared.status().budget.expect("a budget");
        assert_eq!(status.auto, Some(short()));
        join(player);
    }
}
