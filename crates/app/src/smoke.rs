//! The smoke run (M1 plan section 12, item 5): `spherecut --smoke <clip>`
//! opens the clip with the overlay on, waits for its first frame and the
//! Info pane's probe (a camera's lenses open together after it), plays
//! it for `--smoke-seconds` (or to its end), takes a screenshot, writes
//! what the player's numbers saw to `--smoke-out` as one JSON document
//! (`spherecut.smoke/1`, `docs/cli-json.md`), and closes the window.
//! hw-test's `gui_smoke` runs it on real desktops; kittest runs it here,
//! with no compositor.
//!
//! With `--smoke-export`, once the first second has passed, it exports
//! the clip's first [`EXPORT_SPAN`] through the export window's path
//! while the clip plays (M2 plan section 11), and the run goes on until
//! the export has ended: the document says how the export went and what
//! the UI did while it ran. `--smoke-export-preset` exports at another
//! output, and `--smoke-export-blur` with motion blur and a pan of
//! [`PAN_DEG`] over the span for it to sample (amendment 2 item 22
//! measures the heaviest export frames while the viewer plays).
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

use spherecut_core::project::{AnyKey, Ease, Edit, Key, KeyId, Orient};
use spherecut_core::time::{Rational, Time};

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

/// The run's first second, while the cache fills, which `settled` leaves
/// out as the playback benches do.
pub const SETTLE: Duration = Duration::from_secs(1);

/// The late UI passes a run lists ([`Late`]): the first, after
/// [`SETTLE`].
pub const LATE_KEPT: usize = 64;

/// The stages a [`Late`] pass gives the slowest of, from the samples
/// recorded since the pass before it.
const LATE_STAGES: [Stage; 5] = [
    Stage::Decode,
    Stage::Download,
    Stage::UploadCpu,
    Stage::UploadGpu,
    Stage::RenderGpu,
];

/// How much of the clip `--smoke-export` exports, from its start.
pub const EXPORT_SPAN: Duration = Duration::from_secs(2);

/// The longest the export may run past the run's own seconds before it
/// is cancelled.
const EXPORT_LONGEST: Duration = Duration::from_secs(60);

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
    /// An export of the clip's first [`EXPORT_SPAN`], made while it
    /// plays; none without it.
    pub export: Option<SmokeExport>,
}

/// `--smoke-export`: where the export goes, and what the run sets first.
#[derive(Debug, Clone, PartialEq)]
pub struct SmokeExport {
    pub output: PathBuf,
    /// The output to export at, in place of the untitled project's.
    pub preset: Option<spherecut_export::Preset>,
    /// Motion blur on, and the view panning [`PAN_DEG`] over the span so
    /// that it has movement to sample (a flat output's: a 360 output's
    /// view is its front, which the keys don't move).
    pub blur: bool,
}

/// How far `--smoke-export-blur`'s pan turns the view in yaw over the
/// exported span, at an even speed: 45° a second, a quick pan, a few
/// dozen blur samples a frame for a 4K flat output at 30 fps.
pub const PAN_DEG: f64 = 90.0;

/// The pan's edits: the first orientation key's ease made linear, and a
/// key at `end` turned [`PAN_DEG`] in yaw from it. None without a first
/// key that is a view (an aim follows its target).
fn pan(doc: &crate::doc::Doc, end: Time) -> Vec<Edit> {
    let Some(first) = doc.clip().view.orientation.keys.first() else {
        return Vec::new();
    };
    let Orient::View(o) = first.value else {
        return Vec::new();
    };
    let yaw = o.ypr().yaw.to_degrees();
    let Some(turned) = crate::doc::with_angle(o, 0, yaw + PAN_DEG) else {
        return Vec::new();
    };
    vec![
        Edit::SetKey {
            key: first.id,
            at: None,
            value: None,
            ease: Some(Ease::LINEAR),
            spin: None,
        },
        Edit::AddKey {
            clip: doc.clip().id,
            key: AnyKey::Orientation(Key {
                id: KeyId::NEW,
                at: end,
                value: Orient::View(turned),
                ease: Ease::LINEAR,
                spin: 0,
            }),
        },
    ]
}

impl SmokeExport {
    /// To `output`, at the project's output, without blur.
    pub fn to(output: PathBuf) -> SmokeExport {
        SmokeExport {
            output,
            preset: None,
            blur: false,
        }
    }
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
    /// eframe's count of each UI pass's own work, in ms.
    work: Vec<f32>,
    /// Where things stood [`SETTLE`] in.
    settled: Option<Mark>,
    /// The last UI pass's end, and per stage the slowest sample gathered
    /// since then.
    last_ui: Option<Instant>,
    slowest: [Option<f32>; Stage::ALL.len()],
    late: Vec<Late>,
    /// Where things stood when the export started and when it ended.
    export_from: Option<(Mark, Instant)>,
    export_to: Option<Mark>,
    /// Where things stood when the run's seconds were up and the viewer
    /// stopped for an export still running (D56): what the run reports
    /// is up to here.
    stopped: Option<Mark>,
}

/// The counters, UI samples and passes at a moment of the run.
#[derive(Debug, Clone)]
struct Mark {
    played_s: f64,
    counters: [u64; Counter::ALL.len()],
    ui_samples: usize,
    ui_frames: u64,
    passes: u64,
    work: usize,
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

    /// eframe's count of the last UI pass's own work, in seconds: its
    /// time without acquiring the surface, the submit and the present
    /// (`IntegrationInfo::cpu_usage`). With the intervals, it says
    /// whether a slow UI was its own work or waiting.
    pub fn set_work(&mut self, secs: Option<f32>) {
        if let (Phase::Playing(run), Some(s)) = (&mut self.phase, secs) {
            run.work.push(s * 1000.0);
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
                        work: Vec::new(),
                        settled: None,
                        last_ui: None,
                        slowest: [None; Stage::ALL.len()],
                        late: Vec::new(),
                        export_from: None,
                        export_to: None,
                        stopped: None,
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
                run.note_late(now, played, self.refresh_hz);
                if run.settled.is_none()
                    && played >= SETTLE.as_secs_f64()
                    && let Some(p) = &s.player
                {
                    run.gather(p.perf());
                    run.settled = Some(run.mark(played, p.perf()));
                }
                let over = played >= self.args.seconds || s.snapshot.at_end;
                if self.export_step(s, now, played, over) || !over {
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
                    // The run's own edits (an export's preset, its blur)
                    // aren't Pat's to save: closing doesn't ask.
                    s.reframe.may_close = true;
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
            Phase::Playing(run) if run.stopped.is_some() => "finishing its export",
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

impl SmokeDriver {
    /// The export `--smoke-export` asks for: started once the run has
    /// settled and a project is open, cancelled [`EXPORT_LONGEST`] past
    /// the run's seconds, and marked where it ended. Once the run is
    /// `over`, the viewer stops: the export stands aside while it plays
    /// (D56), so it finishes after. Whether the run waits for it.
    fn export_step(&mut self, s: &mut AppState, now: Instant, played: f64, over: bool) -> bool {
        let Some(export) = &self.args.export else {
            return false;
        };
        let Phase::Playing(run) = &mut self.phase else {
            return false;
        };
        match &run.export_from {
            None if run.settled.is_none() => true,
            None => {
                // No project: the run doesn't wait for one.
                let Some(doc) = &s.reframe.doc else {
                    return false;
                };
                let trim = doc.clip().source;
                let span = Time::seconds(
                    Rational::new(EXPORT_SPAN.as_millis() as i64, 1000).unwrap_or(Rational::ZERO),
                );
                let end = trim
                    .start
                    .checked_add(span)
                    .map_or(trim.end, |e| e.min(trim.end));
                let seq = doc.project.the_sequence();
                let output = export
                    .preset
                    .map(|p| doc.set_output(crate::inspector::preset_output(p, &seq.output)))
                    .unwrap_or_default();
                s.edit("Set output", &output);
                if export.blur
                    && let Some(doc) = &s.reframe.doc
                {
                    let settings = spherecut_core::project::ExportSettings {
                        motion_blur: true,
                        ..doc.project.the_sequence().export
                    };
                    let mut edits = doc.set_export(settings);
                    edits.extend(pan(doc, end));
                    s.edit("Export with blur", &edits);
                }
                s.export_span(export.output.clone(), Some((trim.start, end)), true);
                tracing::info!(
                    output = %export.output.display(),
                    preset = ?export.preset,
                    blur = export.blur,
                    "the smoke run exports"
                );
                let Some(p) = &s.player else {
                    return false;
                };
                run.gather(p.perf());
                run.export_from = Some((run.mark(played, p.perf()), now));
                true
            }
            Some(_) if run.export_to.is_some() => false,
            Some((_, since)) => {
                let since = *since;
                let longest = EXPORT_LONGEST + Duration::from_secs_f64(self.args.seconds);
                if s.export.running() {
                    if over
                        && run.stopped.is_none()
                        && let Some(p) = &s.player
                    {
                        run.gather(p.perf());
                        run.stopped = Some(run.mark(played, p.perf()));
                        p.pause();
                        tracing::info!(played, "the smoke run stops playing for its export");
                    }
                    if now.saturating_duration_since(since) > longest {
                        s.export.cancel();
                    }
                    return true;
                }
                let Some(p) = &s.player else {
                    return false;
                };
                run.gather(p.perf());
                run.export_to = Some(run.mark(played, p.perf()));
                false
            }
        }
    }
}

impl Run {
    /// Where things stand, `played_s` in.
    fn mark(&self, played_s: f64, perf: &spherecut_playback::PerfStats) -> Mark {
        Mark {
            played_s,
            counters: Counter::ALL.map(|c| perf.count(c)),
            ui_samples: self.samples(Stage::UiFrame).len(),
            ui_frames: self.ui_frames,
            passes: self.passes,
            work: self.work.len(),
        }
    }

    /// The UI and the clip's frames from `a` to `b`, as the gates judge
    /// them: `rates` is every stream's frames a second.
    fn between(&self, a: &Mark, b: &Mark, refresh_hz: Option<f64>, rates: Option<f64>) -> Settled {
        let since = |c: Counter| b.counters[c as usize].saturating_sub(a.counters[c as usize]);
        let samples = self.samples(Stage::UiFrame);
        let ui = Percentiles::of(
            &samples[a.ui_samples.min(b.ui_samples)..b.ui_samples.min(samples.len())],
            0,
        );
        let frames = b.ui_frames.saturating_sub(a.ui_frames);
        let passes = b.passes.saturating_sub(a.passes);
        let secs = (b.played_s - a.played_s).max(0.0);
        let expected = refresh_hz.map_or(passes as f64, |hz| hz * secs);
        Settled {
            from_s: a.played_s,
            to_s: b.played_s,
            ui: SettledUi {
                frames,
                passes,
                expected,
                presented: if expected > 0.0 {
                    frames as f64 / expected
                } else {
                    0.0
                },
                p50: ui.p50,
                p95: ui.p95,
                p99: ui.p99,
                max: ui.max,
            },
            frames: Frames {
                shown: since(Counter::FramesShown),
                dropped: since(Counter::Dropped),
                decode_dropped: since(Counter::DecodeDropped),
                expected: rates.map(|f| f * secs),
            },
        }
    }

    fn gather(&mut self, perf: &spherecut_playback::PerfStats) {
        for (i, st) in Stage::ALL.into_iter().enumerate() {
            let (recorded, new) = perf.samples_after(st, self.seen[i]);
            self.missed[i] += recorded.saturating_sub(self.seen[i]) - new.len() as u64;
            for &v in &new {
                self.slowest[i] = Some(self.slowest[i].map_or(v, |m| m.max(v)));
            }
            self.samples[i].extend(new);
            self.seen[i] = recorded;
        }
    }

    /// Lists the UI pass ending `now` when it came more than one and a
    /// half refresh periods after the last, once settled.
    fn note_late(&mut self, now: Instant, played_s: f64, refresh_hz: Option<f64>) {
        let last = self.last_ui.replace(now);
        let slowest = std::mem::take(&mut self.slowest);
        // From the first second until the viewer stopped for an export.
        let (Some(last), Some(hz), Some(_), None) =
            (last, refresh_hz, &self.settled, &self.stopped)
        else {
            return;
        };
        let interval = now.saturating_duration_since(last).as_secs_f64() * 1000.0;
        if interval <= 1500.0 / hz || self.late.len() >= LATE_KEPT {
            return;
        }
        self.late.push(Late {
            at_s: played_s,
            interval_ms: interval as f32,
            work_ms: self.work.last().copied(),
            slowest: LATE_STAGES
                .into_iter()
                .filter_map(|st| slowest[st as usize].map(|v| (st.name(), v)))
                .collect(),
        });
    }

    fn samples(&self, st: Stage) -> &[f32] {
        &self.samples[st as usize]
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
    /// The same after the first second ([`SETTLE`]), which the gates
    /// judge; `None` for a run that ended sooner.
    pub settled: Option<Settled>,
    /// The first [`LATE_KEPT`] UI passes after it that came more than one
    /// and a half refresh periods after the pass before.
    pub late: Vec<Late>,
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
    pub submit_gate: Gate,
    /// Where the screenshot went, once written.
    pub screenshot: Option<String>,
    pub screenshot_size: Option<[u32; 2]>,
    /// `--smoke-export`'s export, and the UI while it ran.
    pub export: Option<ExportReport>,
}

/// An export made while the clip played.
#[derive(Debug, Serialize)]
pub struct ExportReport {
    pub output: String,
    /// The preset asked for (`null`: the project's) and whether motion
    /// blur was.
    pub preset: Option<String>,
    pub motion_blur: bool,
    /// Where it rendered: `own` (a device of its own) or `window` (the
    /// window's), D56.
    pub device: Option<&'static str>,
    /// It ended with its file whole and checked.
    pub ok: bool,
    pub error: Option<String>,
    pub frames: Option<u64>,
    pub size: Option<[u32; 2]>,
    pub encoder: Option<&'static str>,
    /// The hardware encoder it didn't use, and why.
    pub fallback: Option<String>,
    pub elapsed_s: Option<f64>,
    /// Each stage's busy time in seconds, as the export's report has
    /// them: `decode`, `render`, `readback`, `encode`, `finish`.
    pub stages_s: Option<BTreeMap<&'static str, f64>>,
    /// Frames whose blur was capped at the most samples (D49).
    pub blur_capped: Option<u64>,
    /// Frames held for a UI pass while the clip played (M2 plan
    /// amendment 2 item 22), and the longest hold.
    pub held: u64,
    pub longest_hold_ms: f32,
    /// Times one of its stages waited for the viewer's frames (D56), and
    /// the longest wait.
    pub yielded: u64,
    pub longest_yield_ms: f32,
    /// Frames of its stages that stood aside while the viewer played
    /// (D56), and the time they stood aside in all.
    pub aside: u64,
    pub aside_ms: f32,
    /// How long it ran on after the run's seconds, the viewer stopped.
    pub after_playing_s: Option<f64>,
    /// The UI and the clip's frames while it ran and the viewer played,
    /// as `settled` has them.
    pub while_running: Option<Settled>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Adapter {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
    pub cpu: bool,
}

impl Adapter {
    /// `gpu`'s, as wgpu names it.
    pub fn of(gpu: &spherecut_render::Gpu) -> Adapter {
        let info = &gpu.caps.info;
        Adapter {
            name: info.name.clone(),
            vendor: info.vendor,
            device: info.device,
            device_type: format!("{:?}", info.device_type),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
            cpu: gpu.caps.is_cpu,
        }
    }
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
    /// Each pass's own work, as eframe counts it: without acquiring the
    /// surface, the submit and the present.
    pub work: Percentiles,
}

/// A UI pass that came late, and what ran while the UI waited.
#[derive(Debug, Clone, Serialize)]
pub struct Late {
    /// How far into the run it ended, in seconds.
    pub at_s: f64,
    /// Since the pass before, in milliseconds.
    pub interval_ms: f32,
    /// The pass before's own work, as eframe counts it.
    pub work_ms: Option<f32>,
    /// Per stage, by its name, the slowest sample recorded in between.
    pub slowest: BTreeMap<&'static str, f32>,
}

#[derive(Debug, Serialize)]
pub struct Settled {
    /// How far into the run it begins and ends, in seconds.
    pub from_s: f64,
    pub to_s: f64,
    pub ui: SettledUi,
    pub frames: Frames,
}

#[derive(Debug, Serialize)]
pub struct SettledUi {
    pub frames: u64,
    pub passes: u64,
    pub expected: f64,
    pub presented: f64,
    pub p50: Option<f32>,
    pub p95: Option<f32>,
    pub p99: Option<f32>,
    pub max: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct Frames {
    pub shown: u64,
    pub dropped: u64,
    pub decode_dropped: u64,
    /// Every stream's frames in `played_s`, at their rates, as `shown`
    /// counts each stream's.
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
    pub(crate) fn of(samples: &[f32], missed: u64) -> Percentiles {
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

/// What the submit gate held back since the device was made (D41).
#[derive(Debug, Serialize)]
pub struct Gate {
    /// Submissions from other threads that waited for the UI's present.
    pub waits: u64,
    pub longest_ms: f32,
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
    /// What `run` measured, `played_s` after it started: up to where it
    /// stopped playing for an export, if it did.
    fn report(&self, s: &AppState, ctx: &egui::Context, run: &Run, played_s: f64) -> Report {
        let refresh_hz = self.refresh_hz;
        let played_s = run.stopped.as_ref().map_or(played_s, |m| m.played_s);
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
        // Every stream's frames count among those shown.
        let rates = streams
            .iter()
            .map(|st| st.fps)
            .sum::<Option<f64>>()
            .filter(|_| !streams.is_empty());
        let (perf, audio) = match &s.player {
            Some(p) => (Some(p.perf().clone()), p.audio_stats()),
            None => (None, None),
        };
        let end = run
            .stopped
            .clone()
            .or_else(|| perf.as_ref().map(|p| run.mark(played_s, p)));
        let counted = |c: Counter| {
            end.as_ref().map_or(0, |m| {
                m.counters[c as usize].saturating_sub(run.start[c as usize])
            })
        };
        let (ui_frames, passes) = end
            .as_ref()
            .map_or((run.ui_frames, run.passes), |m| (m.ui_frames, m.passes));
        let ui_samples = end.as_ref().map_or(usize::MAX, |m| m.ui_samples);
        let work = &run.work[..end.as_ref().map_or(run.work.len(), |m| m.work)];
        let mut stages = BTreeMap::new();
        for st in Stage::ALL {
            if st != Stage::AvOffset {
                stages.insert(
                    st.name(),
                    Percentiles::of(run.samples(st), run.missed[st as usize]),
                );
            }
        }
        let ui_all = run.samples(Stage::UiFrame);
        let ui = Percentiles::of(&ui_all[..ui_samples.min(ui_all.len())], 0);
        let expected = refresh_hz.map_or(passes as f64, |hz| hz * played_s);
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
        let settled = run
            .settled
            .as_ref()
            .zip(end.as_ref())
            .map(|(m, end)| run.between(m, end, refresh_hz, rates));
        let export = self.args.export.as_ref().map(|export| {
            // While it ran and the viewer played.
            let to = match (&run.export_to, &end) {
                (Some(t), Some(e)) if e.played_s < t.played_s => Some(e),
                (t, e) => t.as_ref().or(e.as_ref()),
            };
            let while_running = run
                .export_from
                .as_ref()
                .zip(to)
                .map(|((a, _), b)| run.between(a, b, refresh_hz, rates));
            export_report(
                export,
                run.export_from.is_some(),
                s.export.last.as_ref(),
                s.export.device_used,
                &s.export.pace,
                while_running,
                run.stopped
                    .as_ref()
                    .zip(run.export_to.as_ref())
                    .map(|(a, b)| (b.played_s - a.played_s).max(0.0)),
            )
        });
        Report {
            version: spherecut_core::brand::VERSION,
            clip: self.args.clip.display().to_string(),
            seconds: self.args.seconds,
            played_s,
            ended: snap.at_end,
            adapter: Adapter::of(&s.gpu),
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
                expected: rates.map(|f| f * played_s),
            },
            streams,
            settled,
            late: run.late.clone(),
            ui: UiReport {
                frames: ui_frames,
                passes,
                expected,
                presented: if expected > 0.0 {
                    ui_frames as f64 / expected
                } else {
                    0.0
                },
                p50: ui.p50,
                p95: ui.p95,
                p99: ui.p99,
                max: ui.max,
                hitches: counted(Counter::Hitches),
                work: Percentiles::of(work, 0),
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
            submit_gate: {
                let g = s.gpu.submit_gate().stats();
                Gate {
                    waits: g.waits,
                    longest_ms: g.longest.as_secs_f32() * 1e3,
                }
            },
            screenshot: None,
            screenshot_size: None,
            export,
        }
    }
}

/// The export's part of the document: `last` is how the export window's
/// last export ended, if `started`.
fn export_report(
    export: &SmokeExport,
    started: bool,
    last: Option<&Result<spherecut_export::Report, spherecut_export::ExportError>>,
    device: Option<crate::export::ExportDevice>,
    pace: &spherecut_export::Pace,
    while_running: Option<Settled>,
    after_playing_s: Option<f64>,
) -> ExportReport {
    let (held, longest) = pace.waits();
    let (yielded, longest_yield) = pace.yields();
    let (aside, aside_for) = pace.aside();
    let mut r = ExportReport {
        output: export.output.display().to_string(),
        preset: export.preset.map(|p| p.to_string()),
        motion_blur: export.blur,
        device: device.map(crate::export::ExportDevice::name),
        ok: false,
        error: None,
        frames: None,
        size: None,
        encoder: None,
        fallback: None,
        elapsed_s: None,
        stages_s: None,
        blur_capped: None,
        held,
        longest_hold_ms: longest.as_secs_f32() * 1e3,
        yielded,
        longest_yield_ms: longest_yield.as_secs_f32() * 1e3,
        aside,
        aside_ms: aside_for.as_secs_f32() * 1e3,
        after_playing_s,
        while_running,
    };
    match (started, last) {
        (false, _) => r.error = Some("no project was open to export".into()),
        (true, None) => r.error = Some("the export didn't end".into()),
        (true, Some(Err(e))) => r.error = Some(e.to_string()),
        (true, Some(Ok(report))) => {
            r.ok = true;
            r.frames = Some(report.frames);
            r.encoder = Some(report.encoder);
            r.fallback = report
                .fallback
                .as_ref()
                .map(|f| format!("{}: {}", f.from, f.reason));
            r.elapsed_s = Some(report.elapsed.as_secs_f64());
            r.size = Some([report.size.0, report.size.1]);
            let st = report.stages;
            r.stages_s = Some(
                [
                    ("decode", st.decode),
                    ("render", st.render),
                    ("readback", st.readback),
                    ("encode", st.encode),
                    ("finish", st.finish),
                ]
                .into_iter()
                .map(|(k, d)| (k, d.as_secs_f64()))
                .collect(),
            );
            r.blur_capped = Some(report.blur_capped);
        }
    }
    r
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

pub(crate) fn write_png(path: &Path, image: &image::RgbImage) -> std::io::Result<()> {
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
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
    fn a_late_pass_is_listed_once_settled_with_the_slowest_stages_between() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut run = Run {
            since: t0,
            start: [0; Counter::ALL.len()],
            seen: [0; Stage::ALL.len()],
            samples: Default::default(),
            missed: [0; Stage::ALL.len()],
            ui_frames: 0,
            passes: 0,
            work: vec![1.5],
            settled: None,
            last_ui: None,
            slowest: [None; Stage::ALL.len()],
            late: Vec::new(),
            export_from: None,
            export_to: None,
            stopped: None,
        };
        // Before the mark, late or not, nothing is listed.
        run.note_late(at(0), 0.0, Some(60.0));
        run.note_late(at(50), 0.05, Some(60.0));
        assert!(run.late.is_empty());
        run.settled = Some(Mark {
            played_s: 1.0,
            counters: [0; Counter::ALL.len()],
            ui_samples: 0,
            ui_frames: 0,
            passes: 0,
            work: 0,
        });
        // On time at 60 Hz (17 ms): not listed, and the samples before it
        // go with it.
        run.slowest[Stage::Decode as usize] = Some(9.0);
        run.note_late(at(67), 1.0, Some(60.0));
        assert!(run.late.is_empty());
        // 50 ms: late; the UI's own interval isn't one of the stages.
        run.slowest[Stage::UploadGpu as usize] = Some(3.0);
        run.slowest[Stage::UiFrame as usize] = Some(50.0);
        run.note_late(at(117), 1.05, Some(60.0));
        assert_eq!(run.late.len(), 1);
        let late = &run.late[0];
        assert!((late.interval_ms - 50.0).abs() < 0.01, "{late:?}");
        assert_eq!(late.work_ms, Some(1.5));
        assert_eq!(late.slowest.len(), 1, "{late:?}");
        assert_eq!(late.slowest["upload_gpu"], 3.0);
        // Without a refresh rate there is no period to be late for.
        run.note_late(at(300), 1.3, None);
        assert_eq!(run.late.len(), 1);
    }

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
