//! The export window (M2 plan section 10.6): the sequence's output and
//! export settings, the range, the output file, and the export running
//! as a background job with its progress and Cancel.
//!
//! Each choice is an edit to the sequence, an undo step as the
//! Inspector's are, so the project saves what the export used. The
//! export runs on a clone of the project (the range applied to its
//! trim): edits made while it runs don't reach it. One runs at a time;
//! the window can be closed and opened again while it does. Starting it
//! looks at the output path and the render node, so that is a job too:
//! the UI thread never waits on a file (section 7.9).
//!
//! While the viewer plays, the export stands aside, and while a drag
//! runs it is held to a frame a UI pass, through its [`Pace`] (amendment
//! 2 item 22). It renders on a
//! GPU device of its own beside the window's (D56), so that nothing it
//! submits or waits for shares a lock or a queue with the window's.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use spherecut_core::project::{AudioChoice, Codec, Edit, EncoderChoice, Output, Project, Quality};
use spherecut_core::time::Time;
use spherecut_export::{Export, ExportError, ExportJob, Pace, Progress, Report};
use spherecut_media::doctor::{DoctorReport, Severity};
use spherecut_render::Gpu;

use crate::app::{AppState, DoctorState};
use crate::jobs::{Job, Poll, Threads};
use crate::notices::{Notice, NoticeKind};
use crate::transport::format_time;

/// What the window exports: the trim, or from the first selected key to
/// the end of the last one's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Span {
    #[default]
    Trim,
    Selection,
}

/// Frames the viewer has ready beyond the one it shows before the
/// export's stages start a frame's work while it plays (D56): two, about
/// 67 ms at 30 fps.
pub const LEAD_FRAMES: usize = 2;

/// Where the app's export renders (D56).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportDevice {
    /// A device of its own on the window's adapter: the default.
    #[default]
    Own,
    /// The window's own device, as M2.7 had it: for comparing the two
    /// (`SPHERECUT_EXPORT_DEVICE=window`), and what an export falls back
    /// to when a second device can't be had.
    Window,
}

impl ExportDevice {
    /// The environment variable that puts the export on the window's
    /// device: `window`; anything else, or nothing, is a device of its own.
    pub const ENV: &'static str = "SPHERECUT_EXPORT_DEVICE";

    pub fn parse(value: Option<&str>) -> ExportDevice {
        match value.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("window") => ExportDevice::Window,
            _ => ExportDevice::Own,
        }
    }

    /// `own` or `window`, as the smoke document has it.
    pub fn name(self) -> &'static str {
        match self {
            ExportDevice::Own => "own",
            ExportDevice::Window => "window",
        }
    }
}

/// The window's state and the export running.
#[derive(Debug, Default)]
pub struct ExportUi {
    pub open: bool,
    /// The output file's name, `.mp4` added; empty until the window
    /// first opens with a project, which names it.
    pub name: String,
    pub span: Span,
    /// Whether the clip's audio can be copied into the output, and why
    /// not: for the media file and the output kind it was asked for.
    audio: Option<AudioCheck>,
    /// An export refused for a file at its output: the "Replace it?"
    /// window, and the project it would export.
    pub replace: Option<Replace>,
    run: Option<Running>,
    /// How the last export ended.
    pub last: Option<Result<Report, ExportError>>,
    /// Where the next export renders.
    pub device: ExportDevice,
    /// Where the last one to start rendered.
    pub device_used: Option<ExportDevice>,
    /// The UI's side of the pace: [`SpherecutApp::logic`](crate::app::SpherecutApp::logic)
    /// passes it each pass.
    pub pace: Arc<Pace>,
    threads: Threads,
}

#[derive(Debug)]
struct AudioCheck {
    media: PathBuf,
    sphere: bool,
    state: Check,
}

#[derive(Debug)]
enum Check {
    Running(Job<Option<String>>),
    /// Why Copy can't be, or `None` when it can.
    Done(Option<String>),
}

/// A file at the output, to be replaced or not: Replace exports the
/// project as Start found it, not as it is now.
#[derive(Debug)]
pub struct Replace {
    pub path: PathBuf,
    project: Project,
}

#[derive(Debug)]
struct Running {
    output: PathBuf,
    /// The project the export was given, kept for "Replace it?".
    project: Project,
    state: Run,
    /// Cancel asked for: an export waiting to start doesn't, and one
    /// still starting is cancelled once it has.
    cancelled: Arc<AtomicBool>,
}

impl Running {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
enum Run {
    Starting(Job<(Result<Export, ExportError>, ExportDevice)>),
    Exporting(Box<Export>),
}

impl ExportUi {
    /// An export is starting or running.
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Cancel the export running: it ends within about a frame's work,
    /// leaving nothing at its output.
    pub fn cancel(&mut self) {
        if let Some(r) = &mut self.run {
            r.cancelled.store(true, Ordering::Relaxed);
            if let Run::Exporting(e) = &r.state {
                e.cancel();
            }
        }
    }

    /// The running export's progress, once it has started.
    pub fn progress(&self) -> Option<Progress> {
        match &self.run.as_ref()?.state {
            Run::Exporting(e) => Some(e.progress()),
            Run::Starting(_) => None,
        }
    }

    /// The jobs' threads still running, for the join at exit: an export
    /// running is cancelled, and dropped (which waits for its threads)
    /// on a thread of its own.
    pub fn take_threads(&mut self) -> Vec<JoinHandle<()>> {
        if let Some(r) = self.run.take() {
            r.cancelled.store(true, Ordering::Relaxed);
            match r.state {
                Run::Starting(job) => self.threads.keep(job),
                Run::Exporting(export) => {
                    export.cancel();
                    self.threads
                        .keep(Job::spawn("export-stop", move || drop(export)));
                }
            }
        }
        if let Some(AudioCheck {
            state: Check::Running(job),
            ..
        }) = self.audio.take()
        {
            self.threads.keep(job);
        }
        self.threads.take()
    }
}

impl AppState {
    /// `Ctrl+E` and File ▸ Export…: the window, named for the project;
    /// without one, while an export runs, to follow or cancel it.
    pub fn open_export(&mut self) {
        let Some(doc) = &self.reframe.doc else {
            self.export.open = self.export.running();
            return;
        };
        if self.export.name.is_empty() {
            self.export.name = default_name(&doc.name());
        }
        self.export.open = true;
    }

    /// Whether the viewer has frames in hand beyond the one it shows, so
    /// that the export's stages may start a frame's work while it plays
    /// (D56): [`LEAD_FRAMES`] ready in their slots, or as many as the
    /// slots hold beside the shown frame and one a draw holds. Always
    /// while it doesn't play.
    pub(crate) fn viewer_lead(&self) -> bool {
        lead(self.snapshot.want_play, &self.snapshot.slots)
    }

    /// Whether the export is held to a frame a UI pass: the viewer
    /// plays, or a drag runs.
    pub(crate) fn export_held(&self) -> bool {
        self.snapshot.want_play
            || self.drag.is_some()
            || self.scrub.pending()
            || self.reframe.strip.dragging()
            || self.reframe.curves.dragging()
            || self.reframe.front_drag.is_some()
    }

    /// The export started, its end turned into a notice, the audio
    /// check taken. Never waits.
    pub(crate) fn export_tick(&mut self) {
        let ui = &mut self.export;
        if let Some(a) = &mut ui.audio
            && let Check::Running(job) = &a.state
        {
            match job.poll() {
                Poll::Pending => {}
                Poll::Done(why) => a.state = Check::Done(why),
                Poll::Lost => a.state = Check::Done(Some("the audio couldn't be read".into())),
            }
        }
        let Some(run) = &mut ui.run else {
            return;
        };
        let ended = match &mut run.state {
            Run::Starting(job) => match job.poll().map(|(started, device)| {
                ui.device_used = Some(device);
                started
            }) {
                Poll::Pending => None,
                Poll::Done(Ok(export)) => {
                    if run.cancelled() {
                        export.cancel();
                    }
                    run.state = Run::Exporting(Box::new(export));
                    None
                }
                // Cancelled before it found the file: nothing to ask.
                Poll::Done(Err(ExportError::OutputExists(_))) if run.cancelled() => {
                    Some(Err(ExportError::Cancelled))
                }
                Poll::Done(Err(ExportError::OutputExists(path))) => {
                    ui.replace = Some(Replace {
                        path,
                        project: run.project.clone(),
                    });
                    ui.run = None;
                    return;
                }
                Poll::Done(Err(e)) => Some(Err(e)),
                Poll::Lost => Some(Err(ExportError::Invalid(
                    "the export didn't start: its thread ended".into(),
                ))),
            },
            Run::Exporting(export) => export.wait(Duration::ZERO),
        };
        let Some(result) = ended else {
            return;
        };
        let output = run.output.clone();
        ui.run = None;
        ui.last = Some(result.clone());
        self.notices.retain(|k| *k != NoticeKind::Export);
        self.notices.push(match result {
            Ok(report) => done_notice(&report),
            Err(ExportError::Cancelled) => Notice::new(
                NoticeKind::Export,
                Severity::Info,
                format!("The export to {} was cancelled", output.display()),
            )
            .impact("nothing was written"),
            Err(e) => Notice::new(
                NoticeKind::Export,
                Severity::Error,
                format!("The export to {} failed", output.display()),
            )
            .impact(format!("{e}; nothing was written")),
        });
    }

    /// Start exporting the window's range to `output`.
    fn start_export(&mut self, output: PathBuf, overwrite: bool) {
        let span = match (&self.reframe.doc, self.export.span) {
            (Some(doc), Span::Selection) => selection_span(doc),
            _ => None,
        };
        self.export_span(output, span, overwrite);
    }

    /// Start exporting `span` of the clip (its trim, for `None`) to
    /// `output`, replacing a file there if `overwrite`: Start's, the
    /// smoke run's and the tests'. The checks that look at files run on
    /// the job; one export runs at a time.
    pub fn export_span(&mut self, output: PathBuf, span: Option<(Time, Time)>, overwrite: bool) {
        if self.export.running() {
            return;
        }
        let Some(doc) = &self.reframe.doc else {
            return;
        };
        let mut project = doc.project.clone();
        if let Some((start, end)) = span {
            let trim = Edit::SetTrim {
                clip: doc.clip().id,
                start: Some(start),
                end: Some(end),
            };
            if let Err(e) = project.apply(&[trim]) {
                self.notices.push(
                    Notice::new(
                        NoticeKind::Export,
                        Severity::Warning,
                        "That span can't be exported",
                    )
                    .impact(e.problem.to_string()),
                );
                return;
            }
        }
        self.export_project(project, output, overwrite);
    }

    /// Start exporting `project` to `output`, replacing a file there if
    /// `overwrite`.
    fn export_project(&mut self, project: Project, output: PathBuf, overwrite: bool) {
        if self.export.running() {
            return;
        }
        let window = self.gpu.clone();
        let mut job = ExportJob::new(project.clone(), &output, window.clone());
        job.overwrite = overwrite;
        job.pace = Some(self.export.pace.clone());
        let device = self.export.device;
        let pace = self.export.pace.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        tracing::info!(output = %output.display(), device = device.name(), "exporting");
        let starting = Job::spawn("export-start", {
            let cancelled = cancelled.clone();
            move || {
                // While the viewer plays, the export doesn't start: its
                // device's opening and its first allocations stalled the
                // viewer's uploads 60 ms on odin (D56).
                pace.stand_aside(|| cancelled.load(Ordering::Relaxed));
                if cancelled.load(Ordering::Relaxed) {
                    return (Err(ExportError::Cancelled), device);
                }
                let (gpu, used) = export_device(window, device);
                job.gpu = gpu;
                (Export::start(job), used)
            }
        });
        self.export.run = Some(Running {
            output,
            project,
            state: Run::Starting(starting),
            cancelled,
        });
    }
}

/// [`AppState::viewer_lead`], from what the player says.
fn lead(playing: bool, slots: &spherecut_playback::slots::SlotStats) -> bool {
    !playing || slots.ready >= LEAD_FRAMES.min(slots.active.saturating_sub(2)).max(1)
}

/// The device an export renders on: a second one on the window's adapter,
/// or the window's when asked for or when a second can't be had. On the
/// export's starting thread: opening a device takes a while.
fn export_device(window: Gpu, want: ExportDevice) -> (Gpu, ExportDevice) {
    match want {
        ExportDevice::Window => (window, ExportDevice::Window),
        ExportDevice::Own => match window.beside("exporting") {
            Ok(gpu) => (gpu, ExportDevice::Own),
            Err(e) => {
                tracing::warn!(error = %e, "the export renders on the window's device");
                (window, ExportDevice::Window)
            }
        },
    }
}

/// The notice an export that ended well leaves: where, how, what was
/// checked.
fn done_notice(r: &Report) -> Notice {
    let mut how = format!(
        "{} frames ({}) in {}, with {}",
        r.frames,
        format_time(r.duration),
        format_duration(r.elapsed),
        r.encoder
    );
    if let Some(f) = &r.fallback {
        how.push_str(&format!(" ({} wasn't used: {})", f.from, f.reason));
    }
    if let Some(f) = &r.decode.fallback {
        how.push_str(&format!("; decoded in software ({})", f.reason));
    }
    let mut fix: Vec<String> = r.warnings.iter().map(|w| w.message.clone()).collect();
    if !r.checks.is_empty() {
        let names: Vec<&str> = r.checks.iter().map(|c| c.name).collect();
        fix.push(format!("Checked: {}", names.join(", ")));
    }
    let mut n = Notice::new(
        NoticeKind::Export,
        Severity::Info,
        format!("Exported {}", r.output.display()),
    )
    .impact(how);
    if !fix.is_empty() {
        n = n.fix(fix.join("\n"));
    }
    n
}

/// "1:05" or "42 s".
fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

/// The output's name for a project named `project`: its stem, without
/// " (untitled)".
fn default_name(project: &str) -> String {
    let stem = project.trim_end_matches(" (untitled)");
    let stem = Path::new(stem)
        .file_stem()
        .map_or_else(|| "export".to_owned(), |s| s.to_string_lossy().into_owned());
    format!("{stem}.mp4")
}

/// From the first selected key to the end of the last one's frame, if
/// that is a span.
fn selection_span(doc: &crate::doc::Doc) -> Option<(Time, Time)> {
    let times: Vec<Time> = doc
        .keys()
        .into_iter()
        .filter(|k| doc.selection.contains(&k.id))
        .map(|k| k.at)
        .collect();
    let start = times.iter().copied().min()?;
    let last = times.iter().copied().max()?;
    let end = last
        .checked_add(doc.rate().frame_duration())
        .ok()?
        .min(doc.duration());
    (end > start && last > start).then_some((start, end))
}

/// The codecs in the order the window lists them: those this machine
/// can encode first (as doctor found), and the encoder `auto` uses for
/// each, `None` when there is none; doctor's answer not in yet, all of
/// them, unknown.
fn codecs(report: Option<&DoctorReport>) -> Vec<(Codec, Option<Option<String>>)> {
    let all = [Codec::H264, Codec::Hevc, Codec::Av1];
    let Some(report) = report else {
        return all.into_iter().map(|c| (c, None)).collect();
    };
    let mut v: Vec<_> = all
        .into_iter()
        .map(|c| {
            let prefix = codec_label(c);
            let encoder = report
                .export_encoders
                .iter()
                .filter(|e| e.format.starts_with(prefix))
                .find_map(|e| e.encoder.clone());
            (c, Some(encoder))
        })
        .collect();
    v.sort_by_key(|(_, e)| matches!(e, Some(None)));
    v
}

pub fn codec_label(c: Codec) -> &'static str {
    match c {
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
        Codec::Av1 => "AV1",
    }
}

fn quality_label(q: Quality) -> &'static str {
    match q {
        Quality::High => "High",
        Quality::Medium => "Medium",
        Quality::Small => "Small",
    }
}

fn encoder_label(e: EncoderChoice) -> &'static str {
    match e {
        EncoderChoice::Auto => "Auto (the GPU's, else software)",
        EncoderChoice::Vaapi => "The GPU's (VA-API)",
        EncoderChoice::Software => "Software",
    }
}

fn audio_label(a: AudioChoice) -> &'static str {
    match a {
        AudioChoice::Copy => "Copy",
        AudioChoice::Aac => "AAC",
        AudioChoice::None => "None",
    }
}

/// The folder the output goes in: the Files pane's, else the project's,
/// else the home directory.
fn output_dir(s: &AppState) -> PathBuf {
    s.files
        .dir()
        .map(Path::to_path_buf)
        .or_else(|| {
            let doc = s.reframe.doc.as_ref()?;
            doc.path.as_deref()?.parent().map(Path::to_path_buf)
        })
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The name field's file in `dir`, `.mp4` added; `None` for no name or
/// one with a slash.
pub fn output_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    let mut path = dir.join(name);
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    {
        path.as_mut_os_string().push(".mp4");
    }
    Some(path)
}

/// Check, on a job, whether the clip's audio can be copied into the
/// output, when the media or the output's kind has changed.
fn check_audio(s: &mut AppState) {
    let Some(doc) = &s.reframe.doc else {
        return;
    };
    let Some(media) = doc.media() else {
        return;
    };
    let output = doc.project.the_sequence().output;
    let sphere = matches!(output, Output::Equirect { .. });
    if s.export
        .audio
        .as_ref()
        .is_some_and(|a| a.media == media.path && a.sphere == sphere)
    {
        return;
    }
    if let Some(AudioCheck {
        state: Check::Running(job),
        ..
    }) = s.export.audio.take()
    {
        s.export.threads.keep(job);
    }
    let path = media.path.clone();
    s.export.audio = Some(AudioCheck {
        media: path.clone(),
        sphere,
        state: Check::Running(Job::spawn("export-audio", move || {
            spherecut_export::copy_refusal(&path, &output)
        })),
    });
}

/// The export window and its "Replace it?" window.
pub fn window(s: &mut AppState, ctx: &egui::Context) {
    replace_window(s, ctx);
    if !s.export.open {
        return;
    }
    if s.reframe.doc.is_none() && !s.export.running() {
        s.export.open = false;
        return;
    }
    check_audio(s);
    let mut open = true;
    egui::Window::new("Export")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_pos(ctx.content_rect().center() - egui::vec2(200.0, 200.0))
        .show(ctx, |ui| {
            ui.set_min_width(380.0);
            if s.reframe.doc.is_some() {
                settings_ui(s, ui);
                ui.separator();
            }
            run_ui(s, ui);
        });
    if !open {
        s.export.open = false;
    }
}

/// The choices: each an edit to the sequence.
fn settings_ui(s: &mut AppState, ui: &mut egui::Ui) {
    let report = match &s.doctor {
        DoctorState::Done(r) => Some(r.clone()),
        _ => None,
    };
    let busy = s.export.running();
    let mut edit: Option<(&str, Vec<Edit>)> = None;
    let dir = output_dir(s);
    let Some(doc) = s.reframe.doc.as_ref() else {
        return;
    };
    let mut settings = doc.project.the_sequence().export;
    let before = settings;
    let selection = selection_span(doc);
    let audio_why = match s.export.audio.as_ref().map(|a| &a.state) {
        Some(Check::Done(why)) => why.clone(),
        _ => None,
    };
    let checking = matches!(
        s.export.audio.as_ref().map(|a| &a.state),
        Some(Check::Running(_))
    );
    ui.add_enabled_ui(!busy, |ui| {
        egui::Grid::new("export-settings")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Output");
                if let Some(edits) = crate::inspector::output_menu(ui, doc, "export-output") {
                    edit = Some(("Set the output", edits));
                }
                ui.end_row();

                ui.label("Codec");
                let list = codecs(report.as_deref());
                let label = |c: Codec, e: &Option<Option<String>>| match e {
                    None => codec_label(c).to_owned(),
                    Some(Some(name)) => format!("{} ({name})", codec_label(c)),
                    Some(None) => format!("{} (no encoder here)", codec_label(c)),
                };
                let shown = list.iter().find(|(c, _)| *c == settings.codec).map_or_else(
                    || codec_label(settings.codec).to_owned(),
                    |(c, e)| label(*c, e),
                );
                egui::ComboBox::from_id_salt("export-codec")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (c, e) in &list {
                            ui.selectable_value(&mut settings.codec, *c, label(*c, e));
                        }
                    });
                ui.end_row();

                ui.label("Quality");
                egui::ComboBox::from_id_salt("export-quality")
                    .selected_text(quality_label(settings.quality))
                    .show_ui(ui, |ui| {
                        for q in [Quality::High, Quality::Medium, Quality::Small] {
                            ui.selectable_value(&mut settings.quality, q, quality_label(q));
                        }
                    });
                ui.end_row();

                ui.label("Encoder");
                egui::ComboBox::from_id_salt("export-encoder")
                    .selected_text(encoder_label(settings.encoder))
                    .show_ui(ui, |ui| {
                        for e in [
                            EncoderChoice::Auto,
                            EncoderChoice::Vaapi,
                            EncoderChoice::Software,
                        ] {
                            ui.selectable_value(&mut settings.encoder, e, encoder_label(e));
                        }
                    });
                ui.end_row();

                ui.label("Audio");
                ui.vertical(|ui| {
                    egui::ComboBox::from_id_salt("export-audio")
                        .selected_text(audio_label(settings.audio))
                        .show_ui(ui, |ui| {
                            let copy = ui.add_enabled(
                                audio_why.is_none(),
                                egui::Button::selectable(
                                    settings.audio == AudioChoice::Copy,
                                    audio_label(AudioChoice::Copy),
                                ),
                            );
                            if copy.clicked() {
                                settings.audio = AudioChoice::Copy;
                            }
                            if let Some(why) = &audio_why {
                                copy.on_disabled_hover_text(why.as_str());
                            }
                            for a in [AudioChoice::Aac, AudioChoice::None] {
                                ui.selectable_value(&mut settings.audio, a, audio_label(a));
                            }
                        });
                    if settings.audio == AudioChoice::Copy {
                        if checking {
                            ui.weak("Looking at the audio…");
                        } else if let Some(why) = &audio_why {
                            ui.weak(format!("Not copied: {why}"));
                        }
                    }
                });
                ui.end_row();

                ui.label("Motion blur");
                ui.checkbox(&mut settings.motion_blur, "Blur moves across a frame");
                ui.end_row();

                ui.label("Range");
                ui.vertical(|ui| {
                    let clip = doc.clip();
                    ui.radio_value(
                        &mut s.export.span,
                        Span::Trim,
                        format!(
                            "The trim, {} to {}",
                            format_time(clip.source.start),
                            format_time(clip.source.end)
                        ),
                    );
                    let text = match selection {
                        Some((a, b)) => {
                            format!(
                                "The selected keys, {} to {}",
                                format_time(a),
                                format_time(b)
                            )
                        }
                        None => "The selected keys (select two or more)".to_owned(),
                    };
                    ui.add_enabled_ui(selection.is_some(), |ui| {
                        ui.radio_value(&mut s.export.span, Span::Selection, text);
                    });
                });
                ui.end_row();

                let file = ui.label("File");
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut s.export.name)
                            .desired_width(260.0)
                            .hint_text("name.mp4"),
                    )
                    .labelled_by(file.id);
                    ui.weak(format!("In {}", dir.display()));
                });
                ui.end_row();
            });
    });
    if settings != before {
        edit = Some(("Export settings", doc.set_export(settings)));
    }
    if selection.is_none() && s.export.span == Span::Selection {
        s.export.span = Span::Trim;
    }
    if let Some((label, edits)) = edit
        && !edits.is_empty()
    {
        s.edit(label, &edits);
    }
}

/// Start, or the running export's progress and Cancel.
fn run_ui(s: &mut AppState, ui: &mut egui::Ui) {
    let dir = output_dir(s);
    let playing = s.snapshot.want_play;
    match &s.export.run {
        None => {
            let path = output_path(&dir, &s.export.name);
            let ready = s.reframe.doc.is_some() && path.is_some();
            ui.horizontal(|ui| {
                if ui.add_enabled(ready, egui::Button::new("Start")).clicked()
                    && let Some(path) = path
                {
                    s.start_export(path, false);
                }
                if ui.button("Close").clicked() {
                    s.export.open = false;
                }
            });
        }
        Some(r) => {
            let output = r.output.clone();
            ui.label(format!("Exporting to {}", output.display()));
            match &r.state {
                Run::Starting(_) => {
                    ui.add(egui::ProgressBar::new(0.0).text(if playing {
                        "Starting once the viewer stops"
                    } else {
                        "Starting…"
                    }));
                }
                Run::Exporting(e) => progress_ui(ui, &e.progress(), playing),
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    s.export.cancel();
                }
                if ui
                    .button("Close")
                    .on_hover_text("The export goes on")
                    .clicked()
                {
                    s.export.open = false;
                }
            });
        }
    }
}

/// `playing`: the viewer plays, so the export stands aside (D56).
fn progress_ui(ui: &mut egui::Ui, p: &Progress, playing: bool) {
    let (fraction, text) = match p.finishing {
        Some((done, total)) if total > 0 => (
            done as f32 / total as f32,
            format!(
                "Writing the file: {:.0}%",
                100.0 * done as f64 / total as f64
            ),
        ),
        _ => {
            let fraction = if p.frames_total == 0 {
                0.0
            } else {
                p.frames_done as f32 / p.frames_total as f32
            };
            let mut text = format!("{} of {} frames", p.frames_done, p.frames_total);
            if playing {
                text.push_str(", waiting while the viewer plays");
            } else if p.fps > 0.0 {
                text.push_str(&format!(", {:.1} fps", p.fps));
            }
            if let Some(eta) = p.eta.filter(|_| !playing) {
                text.push_str(&format!(", {} left", format_duration(eta)));
            }
            (fraction, text)
        }
    };
    ui.add(egui::ProgressBar::new(fraction).text(text));
    let mut how = format!("With {}", p.encoder);
    if let Some(f) = &p.fallback {
        how.push_str(&format!(" ({} wasn't used: {})", f.from, f.reason));
    }
    ui.weak(how);
}

fn replace_window(s: &mut AppState, ctx: &egui::Context) {
    let Some(path) = s.export.replace.as_ref().map(|r| r.path.clone()) else {
        return;
    };
    let mut answer = None;
    egui::Window::new("Replace the file?")
        .id(egui::Id::unique("export-replace"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!("{} is there already.", path.display()));
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
    match answer {
        Some(true) => {
            if let Some(r) = s.export.replace.take() {
                s.export_project(r.project, r.path, true);
            }
        }
        Some(false) => s.export.replace = None,
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viewer_has_its_lead_with_two_frames_ready_or_while_it_doesnt_play() {
        use spherecut_playback::slots::SlotStats;
        let slots = |active, ready| SlotStats {
            active,
            ready,
            ..SlotStats::default()
        };
        assert!(lead(false, &slots(4, 0)));
        assert!(!lead(true, &slots(4, 0)));
        assert!(!lead(true, &slots(4, 1)));
        assert!(lead(true, &slots(4, 2)));
        assert!(lead(true, &slots(6, 3)));
        // Three slots hold one ready beside the shown frame and a held one.
        assert!(lead(true, &slots(3, 1)));
        assert!(!lead(true, &slots(3, 0)));
    }

    #[test]
    fn the_export_has_a_device_of_its_own_unless_told_the_windows() {
        assert_eq!(ExportDevice::parse(None), ExportDevice::Own);
        assert_eq!(ExportDevice::parse(Some("own")), ExportDevice::Own);
        assert_eq!(ExportDevice::parse(Some("whatever")), ExportDevice::Own);
        assert_eq!(ExportDevice::parse(Some(" Window ")), ExportDevice::Window);
        assert_eq!(ExportDevice::Own.name(), "own");
        assert_eq!(ExportDevice::Window.name(), "window");
    }

    #[test]
    fn a_device_beside_the_windows_is_another_on_its_adapter() {
        let window = Gpu::headless(&spherecut_render::AdapterChoice::Cpu).expect("lavapipe");
        let (gpu, used) = export_device(window.clone(), ExportDevice::Own);
        assert_eq!(used, ExportDevice::Own);
        assert_ne!(gpu.device, window.device);
        assert_eq!(gpu.caps.info.name, window.caps.info.name);
        let (gpu, used) = export_device(window.clone(), ExportDevice::Window);
        assert_eq!(used, ExportDevice::Window);
        assert_eq!(gpu.device, window.device);
    }

    #[test]
    fn the_output_gets_mp4_and_refuses_a_path() {
        let dir = Path::new("/videos");
        assert_eq!(
            output_path(dir, "trip"),
            Some(PathBuf::from("/videos/trip.mp4"))
        );
        assert_eq!(
            output_path(dir, " trip.MP4 "),
            Some(PathBuf::from("/videos/trip.MP4"))
        );
        assert_eq!(
            output_path(dir, "trip.mov"),
            Some(PathBuf::from("/videos/trip.mov.mp4"))
        );
        assert_eq!(output_path(dir, ""), None);
        assert_eq!(output_path(dir, "a/b"), None);
        assert_eq!(output_path(dir, ".."), None);
    }

    #[test]
    fn the_default_name_is_the_projects() {
        assert_eq!(default_name("ride.spherecut"), "ride.mp4");
        assert_eq!(default_name("VID_0001 (untitled)"), "VID_0001.mp4");
        assert_eq!(default_name(""), "export.mp4");
    }

    #[test]
    fn durations_read_as_minutes_past_one() {
        assert_eq!(format_duration(Duration::from_millis(4200)), "4.2 s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1:05");
    }
}
