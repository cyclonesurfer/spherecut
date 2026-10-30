//! The smoke run (M1 plan section 12, item 5): `spherecut --smoke <clip>`
//! opens the clip with the overlay on, waits for its first frame and the
//! Info pane's probe (a camera's lenses open together after it), plays
//! it for `--smoke-seconds` (or to its end), takes a screenshot, writes
//! what the player's numbers saw to `--smoke-out` as one JSON document
//! (`spherecut.smoke/1`, `docs/cli-json.md`), and closes the window.
//! hw-test's `gui_smoke` runs it on real desktops; kittest runs it here,
//! with no compositor.
//!
//! The numbers are the overlay's ([`PerfStats`]), over the whole run: each
//! pass gathers the samples every stage recorded since the last
//! ([`PerfStats::samples_after`]), and counts as missed those a ring's
//! worth too late to read. The screenshot is downscaled to at most
//! [`SCREENSHOT_WIDTH`] pixels wide, and the files are written on a
//! worker: the UI thread never waits on the disk.
//!
//! The run's own limits are checked in its UI passes, which stop where
//! the compositor draws nothing of the window: eframe then runs only its
//! `logic`, or the UI thread waits in presenting (a locked GNOME session,
//! 2026-10-30). A watchdog thread ([`SmokeDriver::watch`]) ends such a
//! run: after [`STALL`] with no UI pass it writes a `timed_out` document
//! saying when the passes stopped and whether eframe's logic still ran,
//! and exits the process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;
use spherecut_core::fs::TempFile;
use spherecut_media::backend::DecodePath;
use spherecut_playback::{Counter, PlayState, Stage};

use crate::app::AppState;
use crate::jobs::{Job, Poll};

/// The document's schema.
pub const SCHEMA: &str = "spherecut.smoke/1";

/// The screenshot's widest, in pixels.
pub const SCREENSHOT_WIDTH: u32 = 960;

/// The longest the first frame is waited for.
const FIRST_FRAME: Duration = Duration::from_secs(60);

/// The longest the screenshot is: eframe reads it back with the frame it
/// was asked in.
const SCREENSHOT: Duration = Duration::from_secs(5);

/// The longest the window may go without a UI pass before the watchdog
/// ends the run.
pub const STALL: Duration = Duration::from_secs(15);

/// How the watchdog ends the process: with this exit status.
pub type ExitHook = Box<dyn FnOnce(i32) + Send>;

/// What `--smoke` and its flags ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct SmokeArgs {
    pub clip: PathBuf,
    /// How long to play, from the first frame.
    pub seconds: f64,
    /// Where the JSON goes.
    pub out: PathBuf,
    /// Where the screenshot goes, as a PNG; none without it.
    pub screenshot: Option<PathBuf>,
}

/// How the run ended, once its document was written: `Err` with a code
/// and a message when it failed, or when the document couldn't be
/// written.
pub type Outcome = Result<(), (&'static str, String)>;

/// The smoke run, driven by the app's passes.
#[derive(Debug)]
pub struct SmokeDriver {
    args: SmokeArgs,
    /// egui-wgpu's target format: the window surface's.
    surface_format: String,
    /// The window's monitor's, from winit; none under kittest.
    refresh_hz: Option<f64>,
    phase: Phase,
    outcome: Option<Outcome>,
    watchdog: Option<Watchdog>,
}

#[derive(Debug)]
enum Phase {
    Waiting {
        since: Instant,
        /// The Info pane's probe had ended by a pass before this one, so
        /// a camera's lenses have opened (`AppState::tick`).
        probed: bool,
    },
    Playing(Box<Run>),
    Shooting {
        doc: Box<Document>,
        asked: Instant,
        shot: mpsc::Receiver<Arc<egui::ColorImage>>,
    },
    Writing {
        job: Job<Outcome>,
    },
    Done {
        closed: bool,
    },
}

/// What the run gathers while it plays.
#[derive(Debug)]
struct Run {
    since: Instant,
    /// The counters when it started.
    start: [u64; Counter::ALL.len()],
    /// Per stage, in [`Stage::ALL`]'s order: the samples recorded so far,
    /// those gathered since the start, and those missed.
    seen: [u64; Stage::ALL.len()],
    samples: [Vec<f32>; Stage::ALL.len()],
    missed: [u64; Stage::ALL.len()],
    /// The passes the UI ran, and all passes, the hidden ones (eframe's
    /// `logic` alone) with them.
    ui_frames: u64,
    passes: u64,
}

impl SmokeDriver {
    pub fn new(args: SmokeArgs, surface_format: String) -> SmokeDriver {
        SmokeDriver {
            args,
            surface_format,
            refresh_hz: None,
            phase: Phase::Waiting {
                since: Instant::now(),
                probed: false,
            },
            outcome: None,
            watchdog: None,
        }
    }

    /// Start the watchdog: from now, `stall` with no UI pass ends the run
    /// with a `timed_out` document and `exit(1)`, unless its document is
    /// being written already, when only `exit(1)` is left (the window
    /// didn't close). Its thread is never joined: it ends with the
    /// process, or once the driver has gone without the run's end.
    pub fn watch(&mut self, stall: Duration, exit: ExitHook) {
        let (beats, rx) = mpsc::channel();
        let args = self.args.clone();
        let spawned = std::thread::Builder::new()
            .name("smoke-watchdog".into())
            .spawn(move || watchdog(&rx, stall, &args, exit));
        match spawned {
            Ok(_detached) => {
                self.watchdog = Some(Watchdog {
                    beats,
                    written: false,
                });
            }
            Err(e) => tracing::warn!(error = %e, "the smoke run's watchdog didn't start"),
        }
    }

    pub fn args(&self) -> &SmokeArgs {
        &self.args
    }

    /// The window's refresh rate, as the monitor it is on has it.
    pub fn set_refresh_hz(&mut self, hz: Option<f64>) {
        if hz.is_some() {
            self.refresh_hz = hz;
        }
    }

    /// Whether the run has ended and its document is written.
    pub fn done(&self) -> bool {
        matches!(self.phase, Phase::Done { .. })
    }

    /// How it ended, once [`done`](Self::done).
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// Every pass, the window shown or not: the stages' new samples.
    pub fn logic(&mut self, s: &AppState) {
        if let Some(w) = &self.watchdog {
            let _ = w.beats.send(Beat::Logic);
        }
        if let Phase::Playing(run) = &mut self.phase {
            run.passes += 1;
            if let Some(p) = &s.player {
                run.gather(p.perf());
            }
        }
    }

    /// Each UI pass, after the UI: start, end, shoot, write, close.
    pub fn ui(&mut self, s: &mut AppState, ctx: &egui::Context) {
        self.step(s, ctx);
        if let Some(w) = &mut self.watchdog {
            w.beat(&self.phase);
        }
    }

    fn step(&mut self, s: &mut AppState, ctx: &egui::Context) {
        let now = Instant::now();
        match &mut self.phase {
            Phase::Waiting { since, probed } => {
                if let Some((code, message)) = failed(s) {
                    self.fail(code, message);
                } else if !*probed {
                    // The pass the probe ends in reopens a camera's file
                    // with its lenses after its snapshot: the next pass's
                    // is the reopened file's.
                    *probed = !s.info.pending();
                } else if s.shown.is_some()
                    && matches!(s.snapshot.state, PlayState::Paused | PlayState::Playing)
                {
                    let Some(p) = &s.player else { return };
                    let perf = p.perf();
                    let mut run = Run {
                        since: now,
                        start: Counter::ALL.map(|c| perf.count(c)),
                        seen: Stage::ALL.map(|st| perf.samples_after(st, 0).0),
                        samples: Default::default(),
                        missed: [0; Stage::ALL.len()],
                        ui_frames: 0,
                        passes: 0,
                    };
                    run.gather(perf);
                    if !s.snapshot.want_play {
                        p.play();
                    }
                    tracing::info!(
                        clip = %self.args.clip.display(),
                        seconds = self.args.seconds,
                        "the smoke run plays"
                    );
                    self.phase = Phase::Playing(Box::new(run));
                } else if now.saturating_duration_since(*since) > FIRST_FRAME {
                    self.fail(
                        "timed_out",
                        format!("no frame of the clip within {} s", FIRST_FRAME.as_secs()),
                    );
                }
            }
            Phase::Playing(run) => {
                run.ui_frames += 1;
                if let Some((code, message)) = failed(s) {
                    self.fail(code, message);
                    return;
                }
                let played = now.saturating_duration_since(run.since).as_secs_f64();
                if played < self.args.seconds && !s.snapshot.at_end {
                    return;
                }
                let Phase::Playing(mut run) =
                    std::mem::replace(&mut self.phase, Phase::Done { closed: false })
                else {
                    return;
                };
                if let Some(p) = &s.player {
                    run.gather(p.perf());
                }
                let doc = Box::new(Document::ok(self.report(s, ctx, &run, played)));
                if self.args.screenshot.is_some() {
                    let (tx, rx) = mpsc::channel();
                    ctx.request_screenshot(move |image| {
                        let _ = tx.send(image);
                    });
                    self.phase = Phase::Shooting {
                        doc,
                        asked: now,
                        shot: rx,
                    };
                } else {
                    self.write(doc, None);
                }
            }
            Phase::Shooting { asked, shot, .. } => match shot.try_recv() {
                Ok(image) => {
                    let Phase::Shooting { doc, .. } =
                        std::mem::replace(&mut self.phase, Phase::Done { closed: false })
                    else {
                        return;
                    };
                    self.write(doc, Some(image));
                }
                Err(mpsc::TryRecvError::Empty)
                    if now.saturating_duration_since(*asked) < SCREENSHOT => {}
                Err(_) => {
                    let Phase::Shooting { mut doc, .. } =
                        std::mem::replace(&mut self.phase, Phase::Done { closed: false })
                    else {
                        return;
                    };
                    doc.warnings.push(format!(
                        "no screenshot came within {} s",
                        SCREENSHOT.as_secs()
                    ));
                    self.write(doc, None);
                }
            },
            Phase::Writing { job } => match job.poll() {
                Poll::Pending => {}
                Poll::Done(outcome) => {
                    if let Err((code, message)) = &outcome {
                        tracing::error!(code, message, "the smoke run failed");
                    }
                    self.outcome = Some(outcome);
                    self.phase = Phase::Done { closed: false };
                }
                Poll::Lost => {
                    self.outcome = Some(Err((
                        "internal",
                        "the smoke run's writer ended without a result".into(),
                    )));
                    self.phase = Phase::Done { closed: false };
                }
            },
            Phase::Done { closed } => {
                if !*closed {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    *closed = true;
                }
            }
        }
        // Until it has closed, a pass keeps the next coming.
        if !matches!(self.phase, Phase::Done { closed: true }) {
            ctx.request_repaint();
        }
    }

    fn fail(&mut self, code: &'static str, message: String) {
        tracing::error!(code, message, "the smoke run failed");
        self.write(Box::new(Document::failed(code, message)), None);
    }

    /// Write `doc`, and the screenshot with it, on a worker.
    fn write(&mut self, doc: Box<Document>, shot: Option<Arc<egui::ColorImage>>) {
        let args = self.args.clone();
        self.phase = Phase::Writing {
            job: Job::spawn("smoke-out", move || write_out(&args, *doc, shot)),
        };
    }

    /// The writer's thread, for the app's shutdown to join: the files it
    /// writes are whole or not there. The window is closing, so the
    /// watchdog writes nothing from now.
    pub fn take_thread(&mut self) -> Option<JoinHandle<()>> {
        if let Some(w) = &mut self.watchdog {
            w.written();
        }
        if !matches!(self.phase, Phase::Writing { .. }) {
            return None;
        }
        match std::mem::replace(&mut self.phase, Phase::Done { closed: true }) {
            Phase::Writing { job } => job.into_thread(),
            _ => None,
        }
    }
}

impl Phase {
    /// As the watchdog's document says it.
    fn name(&self) -> &'static str {
        match self {
            Phase::Waiting { .. } => "waiting for the first frame",
            Phase::Playing(_) => "playing",
            Phase::Shooting { .. } => "taking the screenshot",
            Phase::Writing { .. } => "writing its document",
            Phase::Done { .. } => "done",
        }
    }
}

/// The driver's side of the watchdog.
#[derive(Debug)]
struct Watchdog {
    beats: mpsc::Sender<Beat>,
    /// The run's document is being written, or the window is closing.
    written: bool,
}

impl Watchdog {
    /// A UI pass ran, and left the run in `phase`.
    fn beat(&mut self, phase: &Phase) {
        if matches!(phase, Phase::Writing { .. } | Phase::Done { .. }) {
            self.written();
        } else if !self.written {
            let _ = self.beats.send(Beat::Ui(phase.name()));
        }
    }

    fn written(&mut self) {
        if !std::mem::replace(&mut self.written, true) {
            let _ = self.beats.send(Beat::Written);
        }
    }
}

/// What the driver tells the watchdog.
#[derive(Debug)]
enum Beat {
    /// A UI pass ran, and the run is in this phase.
    Ui(&'static str),
    /// eframe's logic ran, the window shown or not.
    Logic,
    /// The run's document is being written, or the window is closing.
    Written,
}

/// The watchdog's thread: [`SmokeDriver::watch`].
#[expect(
    clippy::disallowed_methods,
    reason = "the watchdog's own thread, never the UI's: it waits out the window's close"
)]
fn watchdog(beats: &mpsc::Receiver<Beat>, stall: Duration, args: &SmokeArgs, exit: ExitHook) {
    let opened = Instant::now();
    let mut deadline = opened + stall;
    // When the last UI pass ran, after the window opened, and the phase
    // it left; logic passes since.
    let mut last: Option<(Duration, &'static str)> = None;
    let mut logic = 0u64;
    let mut written = false;
    loop {
        match beats.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Beat::Ui(phase)) => {
                last = Some((opened.elapsed(), phase));
                logic = 0;
                deadline = Instant::now() + stall;
            }
            Ok(Beat::Logic) => logic += 1,
            Ok(Beat::Written) => {
                written = true;
                deadline = Instant::now() + stall;
            }
            // The driver went before the run ended: nothing to watch.
            Err(mpsc::RecvTimeoutError::Disconnected) if !written => return,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
        }
    }
    if written {
        tracing::error!(
            "the window didn't close within {} s of the smoke run's end",
            stall.as_secs_f64()
        );
    } else {
        let message = stalled(stall, last, logic);
        tracing::error!(code = "timed_out", message, "the smoke run failed");
        match write_out(args, Document::failed("timed_out", message), None) {
            Err((code, e)) if code != "timed_out" => {
                tracing::error!(error = e, "the watchdog's document");
            }
            _ => {}
        }
    }
    exit(1);
}

/// The watchdog's message: no UI pass for `stall` since `last` (when
/// the last ran, after the window opened, and the phase it left), with
/// `logic` passes of eframe's since.
fn stalled(stall: Duration, last: Option<(Duration, &str)>, logic: u64) -> String {
    let stall = stall.as_secs_f64();
    let head = match last {
        None => format!("the window had no UI pass in the {stall} s after it opened"),
        Some((at, phase)) => format!(
            "the window's UI passes stopped {:.1} s after it opened, while {phase}, and none \
             came for {stall} s",
            at.as_secs_f64()
        ),
    };
    if logic > 0 {
        format!(
            "{head}, though eframe's logic ran {logic} times: the compositor shows nothing of \
             the window (a locked or blanked screen, a minimized window, another virtual \
             terminal)"
        )
    } else {
        format!(
            "{head}, nor did eframe's logic: the UI thread is stuck, likely presenting to a \
             compositor that draws nothing of the window (a locked or blanked screen)"
        )
    }
}

impl Run {
    fn gather(&mut self, perf: &spherecut_playback::PerfStats) {
        for (i, st) in Stage::ALL.into_iter().enumerate() {
            let (recorded, new) = perf.samples_after(st, self.seen[i]);
            self.missed[i] += recorded.saturating_sub(self.seen[i]) - new.len() as u64;
            self.samples[i].extend(new);
            self.seen[i] = recorded;
        }
    }

    fn samples(&self, st: Stage) -> &[f32] {
        &self.samples[st as usize]
    }

    fn counted(&self, perf: &spherecut_playback::PerfStats, c: Counter) -> u64 {
        perf.count(c).saturating_sub(self.start[c as usize])
    }
}

/// Why the run can't go on: no stream decodes, or the GPU failed.
fn failed(s: &AppState) -> Option<(&'static str, String)> {
    let snap = &s.snapshot;
    if let Some(e) = &snap.gpu_error {
        return Some((e.code, e.message.clone()));
    }
    if snap.state == PlayState::Failed {
        let e = snap.streams.iter().find_map(|st| st.error.as_ref());
        return Some(e.map_or_else(
            || ("open_failed", "no stream of the clip can be shown".into()),
            |e| (e.code, e.message.clone()),
        ));
    }
    s.player
        .is_none()
        .then(|| ("internal", "playback couldn't start".into()))
}

/// The document: the CLI's envelope (`docs/cli-json.md`).
#[derive(Debug, Serialize)]
pub struct Document {
    schema: &'static str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Report>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl Document {
    fn ok(report: Report) -> Document {
        Document {
            schema: SCHEMA,
            ok: true,
            result: Some(report),
            error: None,
            warnings: Vec::new(),
        }
    }

    fn failed(code: &'static str, message: String) -> Document {
        Document {
            schema: SCHEMA,
            ok: false,
            result: None,
            error: Some(ErrorBody { code, message }),
            warnings: Vec::new(),
        }
    }
}

/// What the run measured.
#[derive(Debug, Serialize)]
pub struct Report {
    pub version: &'static str,
    pub clip: String,
    /// Asked for, and played: less when the clip ended first.
    pub seconds: f64,
    pub played_s: f64,
    pub ended: bool,
    pub adapter: Adapter,
    pub surface_format: String,
    /// Pixels a point, and the window's own scale factor.
    pub scale: f32,
    pub native_scale: Option<f32>,
    /// The window's inner size and the viewer's, in physical pixels.
    pub window: [u32; 2],
    pub viewer: [u32; 2],
    pub refresh_hz: Option<f64>,
    pub raw_view: bool,
    pub streams: Vec<StreamReport>,
    pub ui: UiReport,
    pub frames: Frames,
    /// Each stage's milliseconds by its name ([`Stage::name`]).
    pub stages: BTreeMap<&'static str, Percentiles>,
    pub av: Option<Av>,
    pub audio: Option<Audio>,
    pub cache: Option<Cache>,
    pub slots: Slots,
    pub memory: Option<Memory>,
    /// Blocking calls on the UI thread since the start: 0 unless
    /// something is wrong (`core::blocking`).
    pub blocked_on_ui_thread: u64,
    /// Where the screenshot went, once written.
    pub screenshot: Option<String>,
    pub screenshot_size: Option<[u32; 2]>,
}

#[derive(Debug, Serialize)]
pub struct Adapter {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub cpu: bool,
}

#[derive(Debug, Serialize)]
pub struct StreamReport {
    /// "hevc 5760×2880 nv12 limited/709", as the overlay has it.
    pub format: Option<String>,
    pub fps: Option<f64>,
    /// "software", "vaapi" or "vulkan": where it decodes.
    pub decoder: Option<&'static str>,
    /// The hardware it was asked for, when it fell back to software.
    pub fell_back_from: Option<&'static str>,
    /// The decoder as the Info pane says it.
    pub decode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UiReport {
    /// UI passes run, and all passes: eframe runs only the logic while
    /// the compositor shows nothing of the window.
    pub frames: u64,
    pub passes: u64,
    /// The UI frames a display at `refresh_hz` shows in `played_s`, or
    /// without a rate, the passes; and the share of them that ran.
    pub expected: f64,
    pub presented: f64,
    /// Between one pass and the next, in milliseconds.
    pub p50: Option<f32>,
    pub p95: Option<f32>,
    pub p99: Option<f32>,
    pub max: Option<f32>,
    pub hitches: u64,
}

#[derive(Debug, Serialize)]
pub struct Frames {
    pub shown: u64,
    pub dropped: u64,
    pub decode_dropped: u64,
    /// The first stream's frames in `played_s`, at its rate.
    pub expected: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct Percentiles {
    pub samples: usize,
    pub missed: u64,
    pub p50: Option<f32>,
    pub p95: Option<f32>,
    pub p99: Option<f32>,
    pub max: Option<f32>,
}

impl Percentiles {
    fn of(samples: &[f32], missed: u64) -> Percentiles {
        let mut v = samples.to_vec();
        v.sort_by(f32::total_cmp);
        Percentiles {
            samples: v.len(),
            missed,
            p50: rank(&v, 0.50),
            p95: rank(&v, 0.95),
            p99: rank(&v, 0.99),
            max: v.last().copied(),
        }
    }
}

/// The `p` quantile of sorted `v` by the nearest rank, as the stages'
/// summaries are.
fn rank(v: &[f32], p: f32) -> Option<f32> {
    let n = v.len();
    (n > 0).then(|| v[((p * n as f32).ceil() as usize).clamp(1, n) - 1])
}

#[derive(Debug, Serialize)]
pub struct Av {
    /// How far the clock is from the shown frame's interval: signed, and
    /// its size.
    pub median_ms: f32,
    pub p95_abs_ms: f32,
    pub samples: usize,
}

#[derive(Debug, Serialize)]
pub struct Audio {
    pub host: String,
    pub device: Option<String>,
    pub rate: u32,
    pub latency_ms: f64,
    pub underruns: u64,
    pub on_system_clock: bool,
}

#[derive(Debug, Serialize)]
pub struct Cache {
    pub budget: u64,
    pub bytes: u64,
    pub frames: usize,
}

#[derive(Debug, Serialize)]
pub struct Slots {
    pub active: usize,
    pub dead: usize,
}

#[derive(Debug, Serialize)]
pub struct Memory {
    pub rss: Option<u64>,
    pub gpu_allocated: Option<u64>,
    pub gpu_reserved: Option<u64>,
}

fn decoder_name(p: &DecodePath) -> (&'static str, Option<&'static str>) {
    use spherecut_media::backend::HwKind;
    let kind = |k: HwKind| match k {
        HwKind::VaApi => "vaapi",
        HwKind::Vulkan => "vulkan",
    };
    match p {
        DecodePath::Software { .. } => ("software", None),
        DecodePath::Hardware { kind: k } => (kind(*k), None),
        DecodePath::FellBack { from, .. } => ("software", Some(kind(*from))),
    }
}

impl SmokeDriver {
    /// What `run` measured, `played_s` after it started.
    fn report(&self, s: &AppState, ctx: &egui::Context, run: &Run, played_s: f64) -> Report {
        let refresh_hz = self.refresh_hz;
        let info = &s.gpu.caps.info;
        let snap = &s.snapshot;
        let ppp = ctx.pixels_per_point();
        let px = |v: f32| (v * ppp).round() as u32;
        let screen = ctx.content_rect().size();
        let viewer = s
            .viewer_rect
            .map_or([0, 0], |r| [px(r.width()), px(r.height())]);
        let streams = snap
            .streams
            .iter()
            .map(|st| {
                let named = st.decode.as_ref().map(decoder_name);
                StreamReport {
                    format: st.info.as_ref().map(crate::overlay::stream_line),
                    fps: st
                        .info
                        .as_ref()
                        .and_then(|i| i.frame_rate())
                        .map(|r| r.to_f64()),
                    decoder: named.map(|n| n.0),
                    fell_back_from: named.and_then(|n| n.1),
                    decode: st.decode.as_ref().map(crate::panes::decode_path),
                }
            })
            .collect::<Vec<_>>();
        let mut stages = BTreeMap::new();
        for st in Stage::ALL {
            if st != Stage::AvOffset {
                stages.insert(
                    st.name(),
                    Percentiles::of(run.samples(st), run.missed[st as usize]),
                );
            }
        }
        let ui = Percentiles::of(run.samples(Stage::UiFrame), 0);
        let expected = refresh_hz.map_or(run.passes as f64, |hz| hz * played_s);
        let av = {
            let signed = run.samples(Stage::AvOffset);
            let mut sorted = signed.to_vec();
            sorted.sort_by(f32::total_cmp);
            let mut size: Vec<f32> = signed.iter().map(|v| v.abs()).collect();
            size.sort_by(f32::total_cmp);
            rank(&sorted, 0.5).zip(rank(&size, 0.95)).map(|(m, p)| Av {
                median_ms: m,
                p95_abs_ms: p,
                samples: signed.len(),
            })
        };
        let (perf, audio) = match &s.player {
            Some(p) => (Some(p.perf().clone()), p.audio_stats()),
            None => (None, None),
        };
        let counted = |c| perf.as_ref().map_or(0, |p| run.counted(p, c));
        Report {
            version: spherecut_core::brand::VERSION,
            clip: self.args.clip.display().to_string(),
            seconds: self.args.seconds,
            played_s,
            ended: snap.at_end,
            adapter: Adapter {
                name: info.name.clone(),
                vendor: info.vendor,
                device: info.device,
                device_type: format!("{:?}", info.device_type),
                driver: info.driver.clone(),
                driver_info: info.driver_info.clone(),
                cpu: s.gpu.caps.is_cpu,
            },
            surface_format: self.surface_format.clone(),
            scale: ppp,
            native_scale: ctx.input(|i| i.viewport().native_pixels_per_point),
            window: [px(screen.x), px(screen.y)],
            viewer,
            refresh_hz,
            raw_view: s.raw_view(),
            frames: Frames {
                shown: counted(Counter::FramesShown),
                dropped: counted(Counter::Dropped),
                decode_dropped: counted(Counter::DecodeDropped),
                expected: streams.first().and_then(|st| st.fps).map(|f| f * played_s),
            },
            streams,
            ui: UiReport {
                frames: run.ui_frames,
                passes: run.passes,
                expected,
                presented: if expected > 0.0 {
                    run.ui_frames as f64 / expected
                } else {
                    0.0
                },
                p50: ui.p50,
                p95: ui.p95,
                p99: ui.p99,
                max: ui.max,
                hitches: counted(Counter::Hitches),
            },
            stages,
            av,
            audio: audio.map(|a| Audio {
                host: a.host,
                device: a.device,
                rate: a.rate,
                latency_ms: a.latency.as_secs_f64() * 1000.0,
                underruns: a.underruns,
                on_system_clock: a.on_system_clock,
            }),
            cache: snap.cache.map(|c| Cache {
                budget: c.budget,
                bytes: c.bytes,
                frames: c.frames,
            }),
            slots: Slots {
                active: snap.slots.active,
                dead: snap.slots.dead,
            },
            memory: snap.memory.map(|m| Memory {
                rss: m.rss,
                gpu_allocated: m.gpu_allocated,
                gpu_reserved: m.gpu_reserved,
            }),
            blocked_on_ui_thread: spherecut_core::blocking::blocked_on_ui_thread(),
            screenshot: None,
            screenshot_size: None,
        }
    }
}

/// The screenshot, its alpha dropped, scaled to at most `width` pixels
/// wide with its aspect kept.
pub fn downscale(image: &egui::ColorImage, width: u32) -> image::RgbImage {
    let [w, h] = image.size.map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    let rgb: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|c| {
            let [r, g, b, _] = c.to_srgba_unmultiplied();
            [r, g, b]
        })
        .collect();
    let full = image::RgbImage::from_raw(w, h, rgb).unwrap_or_default();
    if w <= width || w == 0 {
        return full;
    }
    let nh = ((u64::from(h) * u64::from(width) + u64::from(w) / 2) / u64::from(w)).max(1);
    image::imageops::resize(
        &full,
        width,
        u32::try_from(nh).unwrap_or(1),
        image::imageops::FilterType::Triangle,
    )
}

/// The writer: the screenshot, then the document that names it.
fn write_out(args: &SmokeArgs, mut doc: Document, shot: Option<Arc<egui::ColorImage>>) -> Outcome {
    if let (Some(path), Some(image)) = (&args.screenshot, shot) {
        let small = downscale(&image, SCREENSHOT_WIDTH);
        match write_png(path, &small) {
            Ok(()) => {
                if let Some(r) = &mut doc.result {
                    r.screenshot = Some(path.display().to_string());
                    r.screenshot_size = Some([small.width(), small.height()]);
                }
            }
            Err(e) => doc
                .warnings
                .push(format!("the screenshot can't be written: {e}")),
        }
    }
    let failed = doc.error.as_ref().map(|e| (e.code, e.message.clone()));
    let mut text = serde_json::to_string_pretty(&doc)
        .map_err(|e| ("internal", format!("the smoke run's JSON: {e}")))?;
    text.push('\n');
    write_file(&args.out, text.as_bytes())
        .map_err(|e| ("io_error", format!("writing {}: {e}", args.out.display())))?;
    failed.map_or(Ok(()), Err)
}

fn write_png(path: &Path, image: &image::RgbImage) -> std::io::Result<()> {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(std::io::Error::other)?;
    write_file(path, &png)
}

/// `bytes` into `path` whole, or the file as it was.
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dest = spherecut_core::fs::follow_links(path)?;
    let mut tmp = TempFile::create(&dest)?;
    tmp.file_mut().write_all(bytes)?;
    tmp.persist_replace()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screenshot_keeps_its_aspect_at_most_960_wide() {
        let img =
            |w: usize, h: usize| egui::ColorImage::new([w, h], vec![egui::Color32::RED; w * h]);
        let small = downscale(&img(2560, 1600), SCREENSHOT_WIDTH);
        assert_eq!(small.dimensions(), (960, 600));
        assert_eq!(small.get_pixel(480, 300).0, [255, 0, 0]);
        // An odd size rounds to the nearest row.
        assert_eq!(downscale(&img(1001, 333), 960).dimensions(), (960, 319));
        // Narrow enough already: as it is.
        assert_eq!(downscale(&img(800, 500), 960).dimensions(), (800, 500));
    }

    #[test]
    fn the_watchdog_says_when_and_how_the_window_stalled() {
        let stall = Duration::from_secs(15);
        assert_eq!(
            stalled(stall, None, 0),
            "the window had no UI pass in the 15 s after it opened, nor did eframe's logic: the \
             UI thread is stuck, likely presenting to a compositor that draws nothing of the \
             window (a locked or blanked screen)"
        );
        assert_eq!(
            stalled(stall, Some((Duration::from_millis(22_140), "playing")), 840),
            "the window's UI passes stopped 22.1 s after it opened, while playing, and none \
             came for 15 s, though eframe's logic ran 840 times: the compositor shows nothing \
             of the window (a locked or blanked screen, a minimized window, another virtual \
             terminal)"
        );
    }

    #[test]
    fn percentiles_by_nearest_rank() {
        let v: Vec<f32> = (1..=200).map(|v| v as f32).collect();
        let p = Percentiles::of(&v, 3);
        assert_eq!(
            (p.samples, p.missed, p.p50, p.p95, p.p99, p.max),
            (200, 3, Some(100.0), Some(190.0), Some(198.0), Some(200.0))
        );
        let none = Percentiles::of(&[], 0);
        assert_eq!((none.p50, none.max), (None, None));
    }
}
