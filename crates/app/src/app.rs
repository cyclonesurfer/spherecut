//! The app (M1 plan section 7.1): the tiles, the viewer and its
//! transport, the panes, the notices and the startup doctor check. All of
//! it is [`SpherecutApp::ui`], which takes an `egui::Ui` and no eframe
//! type, so that kittest drives the same code eframe does.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use egui_tiles::{Container, Tile, TileId, Tiles, Tree};
use spherecut_core::Time;
use spherecut_core::clock::Clock;
use spherecut_core::project::Orientation;
use spherecut_media::backend::{MediaBackend, SeekMode, VideoStreamInfo};
use spherecut_media::doctor::{DoctorReport, Issue, Severity};
use spherecut_playback::{
    AudioOutputFactory, HwPromise, HwSupport, PlayState, Player, PlayerConfig, PlayerEvent,
    PlayerParts, PlayerSnapshot, Presented, Repaint,
};
use spherecut_render::{Filter, Framing, Gpu, ViewParams};
use spherecut_sphere::grab::{GrabFit, GrabStart};

use crate::jobs::{Job, Poll};
use crate::notices::{Notice, NoticeKind, Notices};
use crate::overlay::OverlayState;
use crate::panes::{FilesPane, InfoPane};
use crate::reframe::{Look, NOT_KEYED, Reframe};
use crate::settings::{self, Settings};
use crate::smoke::SmokeDriver;
use crate::transport::{self, Scrub};
use crate::view::{Preset, ViewState, ZOOM_PER_POINT, ZOOM_STEP};
use crate::viewer::{self, DrawKey, RawDraw, RawKey, ViewerCallback};

/// The panes the tiles hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Pane {
    Viewer,
    Info,
    Doctor,
    Files,
    /// M2's (plan section 10.1).
    Inspector,
    Curves,
    Sphere,
}

impl Pane {
    /// The side panes, in their tabs' order: [`Pane::UPPER`] of them in
    /// the upper tabs, the rest under them, so that neither row of tabs
    /// is wider than the side of a 960-point window.
    pub const SIDE: [Pane; 6] = [
        Pane::Info,
        Pane::Inspector,
        Pane::Doctor,
        Pane::Files,
        Pane::Curves,
        Pane::Sphere,
    ];
    pub const UPPER: usize = 4;

    pub fn title(self) -> &'static str {
        match self {
            Pane::Viewer => "Viewer",
            Pane::Info => "Info",
            Pane::Doctor => "Doctor",
            Pane::Files => "Files",
            Pane::Inspector => "Inspector",
            Pane::Curves => "Curves",
            Pane::Sphere => "Sphere",
        }
    }
}

/// The layout of section 7.2 with M2's tabs: the viewer, and beside it
/// the Info, Inspector, Curves, Sphere, Doctor and Files tabs, split
/// 3 : 1.
/// Whether a drag that stopped this frame was let go with Escape, which
/// egui ends a drag on: it restores the start rather than keeping the
/// move (M2 plan section 4.3).
pub(crate) fn escaped(ui: &egui::Ui) -> bool {
    ui.input(|i| i.key_pressed(egui::Key::Escape))
}

pub fn default_tree() -> Tree<Pane> {
    let mut tiles = Tiles::default();
    let viewer = tiles.insert_pane(Pane::Viewer);
    let (upper, lower) = Pane::SIDE.split_at(Pane::UPPER);
    let mut tabs = |panes: &[Pane]| {
        let panes = panes.iter().map(|p| tiles.insert_pane(*p)).collect();
        tiles.insert_tab_tile(panes)
    };
    let (upper, lower) = (tabs(upper), tabs(lower));
    let side = tiles.insert_vertical_tile(vec![upper, lower]);
    let root = tiles.insert_horizontal_tile(vec![viewer, side]);
    if let Some(Tile::Container(Container::Linear(l))) = tiles.get_mut(root) {
        l.shares.set_share(viewer, 3.0);
        l.shares.set_share(side, 1.0);
    }
    Tree::new(egui::Id::unique("spherecut-tiles"), root, tiles)
}

/// A saved layout with every pane: one from before a pane was added gets
/// it as a tab beside Info's (or the first tabs), else beside the root.
pub fn with_every_pane(mut tree: Tree<Pane>) -> Tree<Pane> {
    for pane in std::iter::once(Pane::Viewer).chain(Pane::SIDE) {
        if tree.tiles.find_pane(&pane).is_some() {
            continue;
        }
        let id = tree.tiles.insert_pane(pane);
        let tabs = tree
            .tiles
            .find_pane(&Pane::Info)
            .and_then(|info| tree.tiles.parent_of(info))
            .filter(|p| {
                matches!(
                    tree.tiles.get(*p),
                    Some(Tile::Container(Container::Tabs(_)))
                )
            })
            .or_else(|| {
                tree.tiles.iter().find_map(|(id, t)| {
                    matches!(t, Tile::Container(Container::Tabs(_))).then_some(*id)
                })
            });
        match tabs.or(tree.root) {
            Some(parent) => {
                if let Some(Tile::Container(c)) = tree.tiles.get_mut(parent) {
                    c.add_child(id);
                }
            }
            None => tree.root = Some(id),
        }
    }
    tree
}

/// How the startup doctor check runs.
#[derive(Debug)]
pub enum DoctorRun {
    /// `media::doctor::run` on this render node, on a worker.
    Node(PathBuf),
    /// A report made already (tests).
    Report(Box<DoctorReport>),
    /// A report that never comes (tests of the pending state).
    Never,
}

/// What an app is made of. eframe's creation callback makes these from
/// its device (`crate::run`); the kittest suite makes them from lavapipe
/// and a test backend.
pub struct AppParts {
    pub gpu: Gpu,
    pub backend: Arc<dyn MediaBackend>,
    pub clock: Arc<dyn Clock>,
    pub audio: Option<Arc<dyn AudioOutputFactory>>,
    pub repaint: Option<Repaint>,
    pub config: PlayerConfig,
    pub doctor: DoctorRun,
    /// Lists the Files pane's directories; false lists none (tests that
    /// don't want the file system).
    pub list_dirs: bool,
    pub settings: Settings,
    /// What set the cache's budget over the settings (`--cache-mb`, the
    /// environment), if anything did.
    pub cache_override: Option<String>,
    pub tree: Option<Tree<Pane>>,
    /// Files to open, from the command line: the first opens.
    pub open: Vec<PathBuf>,
}

impl std::fmt::Debug for AppParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppParts")
            .field("backend", &self.backend.name())
            .field("config", &self.config)
            .field("doctor", &self.doctor)
            .field("settings", &self.settings)
            .field("cache_override", &self.cache_override)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

/// The startup doctor check.
#[derive(Debug)]
pub enum DoctorState {
    Pending(Job<DoctorReport>),
    Done(Arc<DoctorReport>),
    /// The worker ended without a report.
    Lost,
}

/// Everything but the tiles, which [`Behavior`] borrows while the tree
/// draws.
#[derive(Debug)]
pub struct AppState {
    pub gpu: Gpu,
    pub player: Option<Player>,
    pub view: ViewState,
    pub drag: Option<GrabStart>,
    /// The view and what the viewer showed when the drag began, which
    /// Escape goes back to.
    pub drag_from: Option<(glam::DQuat, Look)>,
    /// The last level drag's fit, for the overlay and tests.
    pub last_fit: Option<GrabFit>,
    pub scrub: Scrub,
    /// The cached spans the scrub bar last shaded.
    pub scrub_spans: Vec<(Time, Time)>,
    pub doctor: DoctorState,
    /// The app's own issues (section 7.6), shown in the Doctor pane.
    pub app_issues: Vec<Issue>,
    pub notices: Notices,
    pub files: FilesPane,
    pub info: InfoPane,
    pub settings: Settings,
    /// The Settings window is open.
    pub settings_open: bool,
    pub cache_override: Option<String>,
    pub snapshot: PlayerSnapshot,
    /// Each stream's info, once it opened.
    pub streams: Vec<Option<VideoStreamInfo>>,
    /// The viewer's rect at the last draw, in points: what input maps
    /// through.
    pub viewer_rect: Option<egui::Rect>,
    /// What the last UI frame gave the viewer to draw.
    pub shown: Option<DrawKey>,
    /// The frames [`SpherecutApp::logic`] picked, for the viewer to draw.
    pub presented: Option<Presented>,
    /// `logic` ran for the coming `ui`, as eframe runs it before each.
    pub logic_ran: bool,
    pub overlay: OverlayState,
    /// The open project (M2 plan section 10).
    pub reframe: Reframe,
    /// The first stream's frame on screen this pass, for the sphere
    /// overview's thumbnail: held, as the viewer's is, until egui has
    /// submitted the pass.
    pub overview_frame: Option<(Arc<spherecut_render::GpuFrame>, RawKey)>,
    /// The export window and the export running (section 10.6).
    pub export: crate::export::ExportUi,
}

/// The app.
#[derive(Debug)]
pub struct SpherecutApp {
    pub tree: Tree<Pane>,
    pub state: AppState,
    /// `--smoke`: the run that drives the app, then closes it.
    pub smoke: Option<SmokeDriver>,
}

impl SpherecutApp {
    /// The app, its player started and its doctor check running.
    pub fn new(parts: AppParts) -> SpherecutApp {
        let AppParts {
            gpu,
            backend,
            clock,
            audio,
            repaint,
            config,
            doctor,
            list_dirs,
            settings,
            cache_override,
            tree,
            open,
        } = parts;
        let mut notices = Notices::default();
        let mut app_issues = Vec::new();
        if gpu.caps.is_cpu {
            let issue = Issue {
                severity: Severity::Warning,
                what: format!(
                    "Rendering on a CPU adapter ({})",
                    spherecut_render::gpu::describe(&gpu.caps.info)
                ),
                impact: "playback will be slow".into(),
                fix: "Install your GPU's Vulkan driver: sudo dnf install mesa-vulkan-drivers   \
                      (Fedora; Debian and Ubuntu: sudo apt install mesa-vulkan-drivers)"
                    .into(),
            };
            notices.push(Notice::from_issue(&issue).with_kind(NoticeKind::CpuAdapter));
            app_issues.push(issue);
        }
        if !gpu.caps.anisotropy {
            app_issues.push(Issue {
                severity: Severity::Info,
                what: "This GPU has no anisotropic filtering".into(),
                impact: "wide views are a little softer".into(),
                fix: "Nothing to do: the isotropic filter is used".into(),
            });
        }
        let mut parts = PlayerParts::new(backend, gpu.clone(), clock);
        parts.audio = audio;
        parts.repaint = repaint;
        // The player's opens decide by the startup check's decode checks
        // rather than run their own (two runs of them, at once, on the
        // same devices).
        let mut keeper = None;
        match &doctor {
            DoctorRun::Node(node) => {
                let (k, later) = HwPromise::new();
                keeper = Some(k);
                parts.hw_later = Some(later);
                parts.render_node = Some(node.clone());
            }
            DoctorRun::Report(r) => parts.hw = Some(HwSupport::from_report(r)),
            DoctorRun::Never => {}
        }
        let player = match Player::new(parts, config) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!(error = %e, "the player can't start");
                notices.push(
                    Notice::new(
                        NoticeKind::GpuFailed,
                        Severity::Error,
                        "Playback can't start",
                    )
                    .impact(e.message.clone()),
                );
                None
            }
        };
        let doctor = match doctor {
            DoctorRun::Node(node) => DoctorState::Pending(Job::spawn("doctor", move || {
                let report = spherecut_media::doctor::run(&node);
                if let Some(k) = keeper {
                    k.keep(HwSupport::from_report(&report));
                }
                report
            })),
            // As though the check had just ended: the first tick makes its
            // banners.
            DoctorRun::Report(r) => DoctorState::Pending(Job::done(*r)),
            DoctorRun::Never => {
                let (tx, rx) = std::sync::mpsc::channel::<DoctorReport>();
                // The sender is leaked on purpose: the job stays pending.
                std::mem::forget(tx);
                DoctorState::Pending(Job::from_receiver(rx))
            }
        };
        let mut view = ViewState::default();
        view.filter = settings::filter(&settings, gpu.caps.anisotropy);
        let files = FilesPane::new(settings.last_dir.clone(), list_dirs);
        let mut state = AppState {
            gpu,
            player,
            view,
            drag: None,
            drag_from: None,
            last_fit: None,
            scrub: Scrub::default(),
            scrub_spans: Vec::new(),
            doctor,
            app_issues,
            notices,
            files,
            info: InfoPane::default(),
            settings,
            settings_open: false,
            cache_override,
            snapshot: PlayerSnapshot::default(),
            streams: Vec::new(),
            viewer_rect: None,
            shown: None,
            presented: None,
            logic_ran: false,
            overlay: OverlayState::default(),
            reframe: Reframe::default(),
            overview_frame: None,
            export: crate::export::ExportUi::default(),
        };
        if let Some(path) = open.into_iter().next() {
            state.open(path);
        }
        SpherecutApp {
            tree: tree.map_or_else(default_tree, with_every_pane),
            state,
            smoke: None,
        }
    }

    /// What runs every pass, the window shown or not: the player's events
    /// and the doctor's result taken, the jobs polled, and the frames to
    /// show picked. eframe calls only this while the window is minimized
    /// or occluded: the presenter moves the cache's playhead and ends the
    /// clip, so playback goes on hidden. It asks for the next pass while
    /// anything moves. Never waits.
    pub fn logic(&mut self, ctx: &egui::Context) {
        let s = &mut self.state;
        // The last pass has presented, painted or not (D41).
        s.gpu.submit_gate().open();
        s.tick();
        s.presented = s.player.as_ref().and_then(|p| {
            // As eframe decides whether to paint: frames passed over
            // while nothing is painted aren't dropped.
            p.set_painted(ctx.input(|i| i.viewport().visible()).unwrap_or(true));
            p.present(Instant::now())
        });
        s.export.pace.pass(s.export_held(), s.viewer_lead());
        s.request_repaint(ctx);
        s.logic_ran = true;
        if let Some(smoke) = &mut self.smoke {
            smoke.logic(s);
        }
    }

    /// The whole UI, for one frame: the menu bar, the notices and the
    /// tiles. eframe runs [`logic`](Self::logic) before it; when nothing
    /// did (kittest), it runs `logic` itself.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if !self.state.logic_ran {
            self.logic(ui.ctx());
        }
        self.state.logic_ran = false;
        let now = Instant::now();
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.state.close_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        egui::Panel::top("menu").show(ui, |ui| self.state.menu_ui(ui));
        if !self.state.notices.shown().is_empty() {
            egui::Panel::top("notices").show(ui, |ui| self.state.notices.ui(ui));
        }
        self.state.keys(ui);
        // A text field's focus holds the keys, not a scrub's settle.
        self.state.scrub.settle(self.state.player.as_ref(), now);
        if std::mem::take(&mut self.state.files.focus) {
            self.tree
                .make_active(|_, tile| matches!(tile, Tile::Pane(Pane::Files)));
        }
        let filter = self.state.view.filter;
        self.state.overview_frame = self.state.presented.as_ref().and_then(|p| {
            let f = p.frames.first()?;
            Some((
                f.gpu.clone(),
                RawKey {
                    slot: f.slot,
                    frame: f.frame,
                    filter,
                },
            ))
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let mut behavior = Behavior {
                state: &mut self.state,
                now,
            };
            self.tree.ui(&mut behavior, ui);
        });
        settings::window(&mut self.state, ui.ctx());
        crate::reframe::windows(&mut self.state, ui.ctx());
        crate::export::window(&mut self.state, ui.ctx());
        self.state.let_go_of_lost_drags(&ctx);
        if std::mem::take(&mut self.state.reframe.quit) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.state.request_repaint(ui.ctx());
        if let Some(smoke) = &mut self.smoke {
            smoke.ui(&mut self.state, ui.ctx());
        }
        // Dropped with the pass's callbacks, which hold it until egui
        // has submitted them.
        self.state.overview_frame = None;
    }

    /// The player's threads are told to stop; [`crate::run`] joins them
    /// after the event loop, never here.
    pub fn begin_shutdown(&self) {
        if let Some(p) = &self.state.player {
            p.begin_shutdown();
        }
    }

    /// The threads of the jobs still running (the doctor's checks, a
    /// listing, a probe), given up, for [`crate::run`] to join after the
    /// event loop.
    pub fn take_threads(&mut self) -> Vec<JoinHandle<()>> {
        let s = &mut self.state;
        let mut threads = s.files.take_threads();
        threads.extend(s.info.take_threads());
        threads.extend(s.reframe.take_threads());
        threads.extend(s.export.take_threads());
        if matches!(s.doctor, DoctorState::Pending(_))
            && let DoctorState::Pending(job) = std::mem::replace(&mut s.doctor, DoctorState::Lost)
        {
            threads.extend(job.into_thread());
        }
        threads.extend(self.smoke.as_mut().and_then(SmokeDriver::take_thread));
        threads
    }
}

impl AppState {
    /// A drag whose widget went from under it (its key deleted or undone
    /// mid-drag) gets no release: once egui drags nothing it is let go,
    /// unkept, so it moves nothing later and no longer holds the export
    /// to the UI's pace.
    fn let_go_of_lost_drags(&mut self, ctx: &egui::Context) {
        if ctx.dragged_id().is_some() {
            return;
        }
        let r = &mut self.reframe;
        let lost = r.strip.dragging() || r.curves.dragging() || r.front_drag.is_some();
        r.strip.cancel();
        r.curves.cancel();
        r.front_drag = None;
        if lost && let Some(doc) = &mut r.doc {
            doc.end_preview();
        }
        self.drag = None;
        self.drag_from = None;
    }

    /// Open these streams of the video at `path` in the player (`None`
    /// for the best), and forget the last file's notices.
    pub(crate) fn open_media(&mut self, path: PathBuf, streams: Vec<Option<usize>>) {
        if self.player.is_none() {
            return;
        }
        self.forget_file();
        self.info.open(&path);
        self.view.reset();
        if let Some(player) = &self.player {
            player.open_streams(path, streams);
        }
        self.reframe.span_reset();
    }

    /// The last file's notices and streams forgotten. The machine's
    /// notices stay, dismissed or not: the doctor's, the adapter's, a
    /// failed GPU, a missing audio device, a slot the GPU dropped; and
    /// the project's.
    pub(crate) fn forget_file(&mut self) {
        // The last file's events, sent since this frame's tick, go with
        // its notices rather than onto the next file's. Once the player
        // has the new file, it sends none of the last one's (but for one
        // already past its check: D36).
        if let Some(player) = &self.player {
            for e in player.events() {
                self.event(e);
            }
        }
        self.notices.retain(|k| {
            matches!(
                k,
                NoticeKind::CpuAdapter
                    | NoticeKind::Doctor(_)
                    | NoticeKind::GpuFailed
                    | NoticeKind::NoAudioDevice
                    | NoticeKind::SlotDropped
                    | NoticeKind::Project
                    | NoticeKind::MediaOffline
                    | NoticeKind::Export
            )
        });
        self.streams.clear();
    }

    /// The player's events into notices, the doctor's result into
    /// banners, and the jobs polled. Never waits.
    fn tick(&mut self) {
        if let DoctorState::Pending(job) = &self.doctor {
            match job.poll() {
                Poll::Pending => {}
                Poll::Done(report) => {
                    for issue in &report.issues {
                        if issue.severity != Severity::Info {
                            self.notices.push(Notice::from_issue(issue));
                        }
                    }
                    self.doctor = DoctorState::Done(Arc::new(report));
                }
                Poll::Lost => {
                    tracing::error!("the doctor's checks ended without a report");
                    self.doctor = DoctorState::Lost;
                }
            }
        }
        self.files.poll();
        self.info.poll();
        self.export_tick();
        let Some(player) = &self.player else {
            return;
        };
        let events = player.events();
        self.snapshot = player.snapshot();
        self.overlay.tick(Instant::now(), &self.snapshot.perf);
        for e in events {
            self.event(e);
        }
        self.reframe_tick();
        if let Some((path, lenses)) = self.info.take_lenses()
            && lenses.len() >= 2
            && self.snapshot.path.as_ref() == Some(&path)
        {
            self.open_lenses(path, lenses);
        }
    }

    /// The open file again, with each of its lenses' streams: a camera's
    /// raw file, which the probe found after the file opened on its best
    /// stream alone. It goes on where it was, playing or not, and its
    /// notices stay.
    fn open_lenses(&mut self, path: PathBuf, lenses: Vec<usize>) {
        let Some(player) = &self.player else {
            return;
        };
        let (at, play) = (self.snapshot.clock, self.snapshot.want_play);
        tracing::info!(?lenses, "opening the lenses of {}", path.display());
        // A camera's raw file has no view to key until M3 stitches it.
        self.reframe.let_go();
        self.notices.push(
            Notice::new(
                NoticeKind::Lenses,
                Severity::Info,
                format!(
                    "A camera's raw file: its {} lenses side by side",
                    lenses.len()
                ),
            )
            .impact("stitching arrives in M3"),
        );
        self.streams.clear();
        player.open_streams(path, lenses.into_iter().map(Some).collect());
        self.reframe.span_reset();
        if at > Time::ZERO {
            player.seek(at, SeekMode::Exact);
        }
        if play {
            player.play();
        }
    }

    /// The viewer's columns: one a video stream.
    pub fn columns(&self) -> usize {
        self.snapshot.streams.len().max(self.streams.len()).max(1)
    }

    /// Whether the viewer shows the raw view (section 7.8): each stream's
    /// picture flat, for a source that isn't equirect or a camera's raw
    /// file's lenses, until M3 stitches them.
    pub fn raw_view(&self) -> bool {
        use spherecut_media::spherical::Projection;
        self.columns() > 1
            || self.streams.iter().flatten().any(|i| {
                !matches!(
                    i.mapping.projection,
                    None | Some(Projection::Equirect { .. })
                )
            })
    }

    fn event(&mut self, e: PlayerEvent) {
        use NoticeKind as K;
        match e {
            PlayerEvent::Opened { stream, info } => {
                if self.streams.len() <= stream {
                    self.streams.resize(stream + 1, None);
                }
                for n in crate::panes::stream_notices(&info) {
                    self.notices.push(n);
                }
                self.streams[stream] = Some(*info);
            }
            PlayerEvent::OpenFailed { stream, error } => self.notices.push(
                Notice::new(
                    K::OpenFailed(stream),
                    Severity::Error,
                    "This file can't be opened",
                )
                .impact(error.message),
            ),
            PlayerEvent::FellBack {
                stream,
                from,
                reason,
            } => self.notices.push(
                Notice::new(
                    K::Fallback(stream),
                    Severity::Warning,
                    format!("{from} decoding went on in software"),
                )
                .impact(format!("{reason}; playback may be slow")),
            ),
            PlayerEvent::DecodeFailed { stream, error } => self.notices.push(
                Notice::new(K::DecodeFailed(stream), Severity::Error, "Decoding stopped")
                    .impact(error.message),
            ),
            PlayerEvent::AudioFailed { error } => self.notices.push(
                Notice::new(
                    K::AudioFailed,
                    Severity::Warning,
                    "The audio can't be played",
                )
                .impact(format!(
                    "{}; the video plays on its own clock",
                    error.message
                )),
            ),
            PlayerEvent::AudioOpened { output } => {
                let null = output
                    .as_ref()
                    .is_none_or(|o| o.kind == spherecut_audio::OutputKind::Null);
                if null {
                    let issue = Issue {
                        severity: Severity::Info,
                        what: "No audio output device: playing silently".into(),
                        impact: "the video plays on the audio's clock, unheard".into(),
                        fix: "Check that PipeWire or PulseAudio runs: systemctl --user status pipewire"
                            .into(),
                    };
                    self.notices
                        .push(Notice::from_issue(&issue).with_kind(K::NoAudioDevice));
                    if !self.app_issues.iter().any(|i| i.what == issue.what) {
                        self.app_issues.push(issue);
                    }
                }
            }
            PlayerEvent::NoAudio => self.notices.push(Notice::new(
                K::NoAudio,
                Severity::Info,
                "This file has no audio",
            )),
            PlayerEvent::SlotDropped { error, live, .. } => self.notices.push(
                Notice::slot_dropped(&error.message, live, self.streams.len().max(1)),
            ),
            PlayerEvent::UploadFailed { error, .. } => self.notices.push(
                Notice::new(K::UploadFailed, Severity::Warning, "A frame can't be shown")
                    .impact(error.message),
            ),
            PlayerEvent::GpuFailed { error } => self.notices.push(
                Notice::new(K::GpuFailed, Severity::Error, "The GPU device failed")
                    .impact(format!("{}; restart Spherecut", error.message)),
            ),
            PlayerEvent::CacheTooSmall {
                budget,
                frame_bytes,
            } => self.notices.push(
                Notice::new(
                    K::CacheTooSmall,
                    Severity::Warning,
                    "The frame cache is too small for this file",
                )
                .impact(format!(
                    "{} MiB holds {} of its frames; stepping back may decode",
                    budget >> 20,
                    budget.checked_div(frame_bytes).unwrap_or(0)
                ))
                .fix("Set a larger cache: SPHERECUT_FRAME_CACHE_MIB=4096 spherecut"),
            ),
            PlayerEvent::AudioLate
            | PlayerEvent::Decoder { .. }
            | PlayerEvent::StreamEnded { .. }
            | PlayerEvent::Ended => {}
        }
    }

    /// Repaint while anything moves: playback, a seek, a morph, a job.
    fn request_repaint(&self, ctx: &egui::Context) {
        let s = &self.snapshot;
        let busy = matches!(
            s.state,
            PlayState::Opening | PlayState::Seeking | PlayState::Playing
        ) || s.want_play
            || self.view.animating()
            || self.scrub.pending();
        if busy {
            ctx.request_repaint();
        } else if matches!(self.doctor, DoctorState::Pending(_))
            || self.files.pending()
            || self.info.pending()
            || self.reframe.pending()
            || matches!(self.reframe.look, Look::Editing { .. })
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if self.export.running() {
            // The progress bar, and the end's notice.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn menu_ui(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            let doc = self.reframe.doc.is_some();
            ui.menu_button("File", |ui| {
                if ui
                    .add(egui::Button::new("Open…").shortcut_text("Ctrl+O"))
                    .clicked()
                {
                    self.files.focus = true;
                    ui.close();
                }
                let saving = self.reframe.saving();
                if ui
                    .add_enabled(
                        doc && !saving,
                        egui::Button::new("Save").shortcut_text("Ctrl+S"),
                    )
                    .clicked()
                {
                    self.save();
                    ui.close();
                }
                if ui
                    .add_enabled(
                        doc && !saving,
                        egui::Button::new("Save as…").shortcut_text("Ctrl+Shift+S"),
                    )
                    .clicked()
                {
                    self.open_save_as();
                    ui.close();
                }
                // Without a project, a running export can still be
                // followed.
                let export = doc || self.export.running();
                if ui
                    .add_enabled(export, egui::Button::new("Export…").shortcut_text("Ctrl+E"))
                    .clicked()
                {
                    self.open_export();
                    ui.close();
                }
                if ui.add_enabled(doc, egui::Button::new("Close")).clicked() {
                    self.close_project();
                    ui.close();
                }
                ui.separator();
                if ui.button("Settings…").clicked() {
                    self.settings_open = true;
                    ui.close();
                }
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| self.edit_menu(ui));
            ui.menu_button("View", |ui| {
                let now = ui.input(|i| i.time);
                for p in Preset::ALL {
                    if ui
                        .selectable_label(self.view.preset == p, p.label())
                        .clicked()
                    {
                        self.view.switch_to(p, now);
                    }
                }
                ui.separator();
                let mut morph = self.view.lens.morph;
                if ui
                    .add(egui::Slider::new(&mut morph, 0.0..=1.0).text("Morph"))
                    .changed()
                {
                    self.view.set_morph(morph);
                }
                egui::ComboBox::from_label("Morph to")
                    .selected_text(crate::view::projection_label(self.view.lens.to))
                    .show_ui(ui, |ui| {
                        for p in Preset::ALL {
                            let to = p.projection();
                            if ui
                                .selectable_label(self.view.lens.to == to, p.label())
                                .clicked()
                            {
                                self.view.set_morph_target(to);
                            }
                        }
                    });
                ui.separator();
                let mut aniso = self.view.filter == Filter::Anisotropic;
                let can = self.gpu.caps.anisotropy;
                if ui
                    .add_enabled(
                        can,
                        egui::Checkbox::new(&mut aniso, "Anisotropic filtering"),
                    )
                    .changed()
                {
                    self.view.filter = if aniso {
                        Filter::Anisotropic
                    } else {
                        Filter::Isotropic
                    };
                    self.settings.anisotropic = aniso;
                }
                ui.checkbox(&mut self.settings.overlay, "Overlay (F3)");
                let sphere = self.reframe.doc.as_ref().is_some_and(|d| {
                    matches!(
                        d.project.the_sequence().output,
                        spherecut_core::project::Output::Equirect { .. }
                    )
                });
                ui.add_enabled(
                    sphere,
                    egui::Checkbox::new(
                        &mut self.reframe.equirect_preview,
                        "Preview the 360 output",
                    ),
                )
                .on_disabled_hover_text(
                    "For a 360 output: set it in the Inspector or the export window",
                );
                ui.add_enabled(
                    self.reframe.doc.is_some(),
                    egui::Checkbox::new(&mut self.reframe.looping, "Loop in to out (Ctrl+L)"),
                );
            });
            ui.menu_button("Help", |ui| {
                ui.label(format!(
                    "{} {}",
                    spherecut_core::brand::APP_NAME,
                    spherecut_core::brand::VERSION
                ));
                ui.separator();
                ui.label(transport::KEYS_HELP);
            });
        });
    }

    /// Edit ▸: undo and redo by their steps' labels, and the reframer's
    /// edits by their keys.
    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        let doc = self.reframe.doc.is_some();
        let undo = self
            .reframe
            .doc
            .as_ref()
            .and_then(|d| d.undo_label())
            .map(str::to_owned);
        let redo = self
            .reframe
            .doc
            .as_ref()
            .and_then(|d| d.redo_label())
            .map(str::to_owned);
        let label = |verb: &str, step: &Option<String>| match step {
            Some(l) => format!("{verb} {l}"),
            None => verb.to_owned(),
        };
        if ui
            .add_enabled(
                undo.is_some(),
                egui::Button::new(label("Undo", &undo)).shortcut_text("Ctrl+Z"),
            )
            .clicked()
        {
            self.undo();
            ui.close();
        }
        if ui
            .add_enabled(
                redo.is_some(),
                egui::Button::new(label("Redo", &redo)).shortcut_text("Ctrl+Shift+Z"),
            )
            .clicked()
        {
            self.redo();
            ui.close();
        }
        ui.separator();
        let selected = self
            .reframe
            .doc
            .as_ref()
            .is_some_and(|d| !d.selection.is_empty());
        let keyed = self.reframe.flat_output().is_some();
        type Item = (&'static str, &'static str, bool, fn(&mut AppState));
        let items: [Item; 5] = [
            ("Key the view", "Alt+K", keyed, AppState::key_view),
            ("Set in", "I", doc, AppState::set_in),
            ("Set out", "O", doc, AppState::set_out),
            ("Add marker", "M", doc, AppState::add_marker),
            ("Delete keys", "Delete", selected, AppState::remove_selected),
        ];
        for (text, key, enabled, run) in items {
            if ui
                .add_enabled(enabled, egui::Button::new(text).shortcut_text(key))
                .clicked()
            {
                run(self);
                ui.close();
            }
        }
    }

    /// The transport, view and project keys, while no text field has
    /// focus. egui matches a key without Shift or Alt whether they are
    /// held or not, so the chords go first.
    fn keys(&mut self, ui: &mut egui::Ui) {
        if ui.ctx().text_edit_focused() {
            return;
        }
        use egui::{Key, Modifiers};
        let pressed =
            |ui: &mut egui::Ui, m: Modifiers, k: Key| ui.input_mut(|i| i.consume_key(m, k));
        let t = ui.input(|i| i.time);
        let shift = Modifiers::COMMAND | Modifiers::SHIFT;
        if pressed(ui, shift, Key::Z) || pressed(ui, Modifiers::COMMAND, Key::Y) {
            self.redo();
        }
        if pressed(ui, Modifiers::COMMAND, Key::Z) {
            self.undo();
        }
        if pressed(ui, shift, Key::S) {
            self.open_save_as();
        }
        if pressed(ui, Modifiers::COMMAND, Key::S) {
            self.save();
        }
        if pressed(ui, Modifiers::COMMAND, Key::O) {
            self.files.focus = true;
        }
        if pressed(ui, Modifiers::COMMAND, Key::E) {
            self.open_export();
        }
        if pressed(ui, Modifiers::COMMAND, Key::L) {
            self.toggle_loop();
        }
        if pressed(ui, Modifiers::ALT, Key::K) {
            self.key_view();
        }
        if pressed(ui, Modifiers::ALT, Key::OpenBracket) {
            self.seek_key(false);
        }
        if pressed(ui, Modifiers::ALT, Key::CloseBracket) {
            self.seek_key(true);
        }
        if pressed(ui, Modifiers::NONE, Key::I) {
            self.set_in();
        }
        if pressed(ui, Modifiers::NONE, Key::O) {
            self.set_out();
        }
        if pressed(ui, Modifiers::NONE, Key::M) {
            self.add_marker();
        }
        if pressed(ui, Modifiers::NONE, Key::Delete) || pressed(ui, Modifiers::NONE, Key::Backspace)
        {
            self.remove_selected();
        }
        if pressed(ui, Modifiers::NONE, Key::Space) || pressed(ui, Modifiers::NONE, Key::K) {
            self.toggle_play();
        }
        if pressed(ui, Modifiers::SHIFT, Key::ArrowRight) {
            self.jump(1);
        }
        if pressed(ui, Modifiers::SHIFT, Key::ArrowLeft) {
            self.jump(-1);
        }
        if pressed(ui, Modifiers::NONE, Key::ArrowRight)
            || pressed(ui, Modifiers::NONE, Key::Period)
        {
            self.step(1);
        }
        if pressed(ui, Modifiers::NONE, Key::ArrowLeft) || pressed(ui, Modifiers::NONE, Key::Comma)
        {
            self.step(-1);
        }
        if pressed(ui, Modifiers::NONE, Key::Home) {
            self.seek(Time::ZERO, SeekMode::Exact);
        }
        if pressed(ui, Modifiers::NONE, Key::End) {
            self.seek_last();
        }
        if pressed(ui, Modifiers::NONE, Key::R) {
            self.view.reset();
        }
        if pressed(ui, Modifiers::NONE, Key::Plus) || pressed(ui, Modifiers::NONE, Key::Equals) {
            self.view.zoom(ZOOM_STEP, glam::DVec2::ZERO);
        }
        if pressed(ui, Modifiers::NONE, Key::Minus) {
            self.view.zoom(1.0 / ZOOM_STEP, glam::DVec2::ZERO);
        }
        if pressed(ui, Modifiers::NONE, Key::F3) {
            self.settings.overlay = !self.settings.overlay;
        }
        for (key, preset) in [
            (Key::Num1, Preset::Rectilinear),
            (Key::Num2, Preset::Fisheye),
            (Key::Num3, Preset::TinyPlanet),
        ] {
            if pressed(ui, Modifiers::NONE, key) {
                self.view.switch_to(preset, t);
            }
        }
    }

    /// The settings into the view and the player, after the window
    /// changed them.
    pub fn apply_settings(&mut self) {
        self.view.filter = settings::filter(&self.settings, self.gpu.caps.anisotropy);
        if let Some(p) = &self.player {
            let mut config = p.config();
            self.settings
                .apply(&mut config, self.cache_override.is_some());
            p.set_config(config);
        }
    }

    pub fn toggle_play(&mut self) {
        if self.player.is_none() {
            return;
        }
        if !self.snapshot.want_play {
            self.end_look();
        }
        let Some(p) = &self.player else { return };
        if self.snapshot.want_play {
            p.pause();
        } else {
            p.play();
        }
    }

    pub fn step(&mut self, frames: i32) {
        self.end_look();
        if let Some(p) = &self.player {
            p.step(frames);
        }
    }

    pub fn seek(&mut self, at: Time, mode: SeekMode) {
        self.end_look();
        if let Some(p) = &self.player {
            p.seek(at, mode);
        }
    }

    fn jump(&mut self, secs: i64) {
        let at = self.snapshot.clock;
        let to = if secs >= 0 {
            at.checked_add(Time::from_secs(secs))
        } else {
            at.checked_sub(Time::from_secs(-secs))
        };
        let to = to.unwrap_or(Time::ZERO).max(Time::ZERO);
        let to = match self.duration() {
            Some(d) => to.min(transport::last_frame_time(d, self.stream0())),
            None => to,
        };
        self.seek(to, SeekMode::Exact);
    }

    fn seek_last(&mut self) {
        if let Some(d) = self.duration() {
            let at = transport::last_frame_time(d, self.stream0());
            self.seek(at, SeekMode::Exact);
        }
    }

    pub fn stream0(&self) -> Option<&VideoStreamInfo> {
        self.streams.first().and_then(Option::as_ref)
    }

    /// The clip's duration: the first stream's, else its end once decoded.
    pub fn duration(&self) -> Option<Time> {
        self.stream0()
            .and_then(|i| i.duration)
            .or_else(|| self.snapshot.streams.first().and_then(|s| s.end))
    }

    /// What the viewer says in place of a picture, while it has none.
    pub fn placeholder(&self) -> Option<&'static str> {
        if self.shown.is_some() {
            return None;
        }
        Some(match self.snapshot.state {
            PlayState::Empty if self.reframe.loading() => "Opening the project…",
            PlayState::Empty if self.reframe.doc.is_some() => "The project's video is missing",
            PlayState::Empty => "Open a video or a project from the Files pane",
            PlayState::Failed => "This file can't be shown",
            _ => "Decoding…",
        })
    }

    /// The viewer pane: the picture over the transport bar.
    fn viewer_ui(&mut self, ui: &mut egui::Ui, now: Instant) {
        egui::Panel::bottom("transport").show(ui, |ui| {
            transport::bar_ui(self, ui, now);
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(ui, |ui| self.picture_ui(ui));
    }

    /// The projected picture, and the drag and zoom on it. With a flat
    /// output it is the keyed view (or a look-around, or a change being
    /// made) through the output frame, dimmed past it (M2 plan 10.2).
    fn picture_ui(&mut self, ui: &mut egui::Ui) {
        let t = ui.input(|i| i.time);
        self.view.settle(t);
        self.watch_view(Instant::now());
        self.apply_keys();
        let mut area = ui.available_rect_before_wrap();
        let front = self.reframe.preview_front();
        if front.is_some() {
            // The 360 output as it exports: 2:1, as large as fits.
            let w = area.width().min(area.height() * 2.0);
            area = egui::Rect::from_center_size(area.center(), egui::vec2(w, w / 2.0));
        }
        let response = ui.allocate_rect(area, egui::Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let rect = viewer::pixel_rect(area, ppp);
        self.viewer_rect = Some(rect);
        let vp = viewer::viewport(rect, ui.ctx());
        let framing = self.framing(viewer::physical(&vp));
        let free = ui.input(|i| i.modifiers.ctrl);
        // The raw view's pictures are flat: nothing to look around in.
        let raw = self.raw_view();
        if raw {
            self.drag = None;
        } else if let Some(front) = front {
            self.drag = None;
            self.front_ui(ui, &response, rect, front);
        } else {
            self.look_ui(ui, &response, &vp, ppp, free, framing);
        }
        if response.double_clicked() {
            self.toggle_play();
        }

        // What to draw: the first stream's frame on screen, projected, or
        // in the raw view every stream's, flat.
        let presented = self.presented.take();
        let frames = presented.as_ref().map_or(&[][..], |p| &p.frames[..]);
        let mut view = ViewParams::new(self.view.q, self.view.lens);
        if let Some(f) = framing {
            view = view.framed(f);
        }
        if let Some(front) = front {
            view = ViewParams::equirect(front.quat());
        }
        let filter = self.view.filter;
        let shown = frames.first().map(|s| {
            (
                s.gpu.clone(),
                DrawKey {
                    slot: s.slot,
                    frame: s.frame,
                    view,
                    filter,
                },
            )
        });
        self.shown = shown.as_ref().map(|(_, k)| k.clone());
        let raw = if raw {
            frames
                .iter()
                .map(|s| RawDraw {
                    column: s.stream,
                    frame: s.gpu.clone(),
                    key: RawKey {
                        slot: s.slot,
                        frame: s.frame,
                        filter,
                    },
                })
                .collect()
        } else {
            Vec::new()
        };
        let flat = raw.is_empty();
        let shown = shown.filter(|_| flat);
        let perf = self.player.as_ref().map(|p| p.perf().clone());
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewerCallback {
                rect,
                shown,
                raw,
                columns: self.columns(),
                perf,
            },
        ));
        drop(presented);
        self.picture_notes(ui, &response, rect, framing.filter(|_| flat));
    }

    /// Drag to look around, and zoom with the wheel or a pinch, the
    /// pointer mapped through the output frame when there is one.
    fn look_ui(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        vp: &egui::epaint::ViewportInPixels,
        ppp: f32,
        free: bool,
        framing: Option<Framing>,
    ) {
        let mut through = ViewParams::new(self.view.q, self.view.lens);
        if let Some(f) = framing {
            through = through.framed(f);
        }
        let point = |pos| viewer::frame_point(pos, vp, ppp, &through);
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.view.cancel_morph();
            self.drag = Some(self.view.grab(point(origin)));
            self.drag_from = Some((self.view.q, self.reframe.look));
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && let (Some(mut start), Some(pos)) = (self.drag, response.interact_pointer_pos())
        {
            let fit = self.view.drag_to(&mut start, point(pos), free);
            self.drag = Some(start);
            self.last_fit = Some(fit);
        }
        if response.drag_stopped() {
            self.drag = None;
            match self.drag_from.take() {
                Some((q, look)) if escaped(ui) => self.cancel_look(q, look),
                _ => self.reframe.released = true,
            }
        }
        if response.hovered()
            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
        {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let p = point(pos);
            if scroll != 0.0 {
                self.view
                    .zoom((ZOOM_PER_POINT * f64::from(scroll)).exp(), p);
            }
            if zoom != 1.0 {
                self.view.zoom(f64::from(zoom), p);
            }
        }
    }

    /// A drag of the equirect preview turns the 360 output's front: the
    /// picture follows the pointer, and the release is one step.
    fn front_ui(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: egui::Rect,
        front: Orientation,
    ) {
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
        {
            self.reframe.front_drag = Some((front, p));
        }
        if let Some((start, from)) = self.reframe.front_drag
            && let Some(p) = response.interact_pointer_pos()
            && response.dragged()
        {
            let d = p - from;
            let y = start.ypr();
            let yaw = y.yaw - f64::from(d.x / rect.width().max(1.0)) * std::f64::consts::TAU;
            let pitch = (y.pitch + f64::from(d.y / rect.height().max(1.0)) * std::f64::consts::PI)
                .clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
            let o = Orientation::from_ypr(spherecut_sphere::orient::Ypr::new(yaw, pitch, y.roll));
            if let (Ok(o), Some(doc)) = (o, &mut self.reframe.doc) {
                doc.front_preview = Some(o);
            }
        }
        if response.drag_stopped() && self.reframe.front_drag.take().is_some() {
            let edits = self
                .reframe
                .doc
                .as_ref()
                .filter(|_| !escaped(ui))
                .and_then(|d| d.front_preview.map(|o| d.set_front(o)))
                .unwrap_or_default();
            if let Some(doc) = &mut self.reframe.doc {
                doc.front_preview = None;
            }
            self.edit("Turn the front", &edits);
        }
    }

    /// The picture's label, its placeholder, the look-around's badge, and
    /// the overlay over it.
    fn picture_notes(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        rect: egui::Rect,
        framing: Option<Framing>,
    ) {
        let placeholder = self.placeholder();
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Image,
                true,
                placeholder.unwrap_or("The picture"),
            )
        });
        if let Some(text) = placeholder {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(16.0),
                egui::Color32::GRAY,
            );
        }
        if self.reframe.look == Look::Around && placeholder.is_none() {
            // In the frame's corner: its rect is in the viewer's pixels.
            let ppp = ui.ctx().pixels_per_point();
            let corner = framing.map_or(rect.left_top(), |f| {
                rect.left_top() + egui::vec2(f.rect[0] as f32, f.rect[1] as f32) / ppp
            });
            let text = egui::RichText::new(NOT_KEYED)
                .color(egui::Color32::WHITE)
                .background_color(egui::Color32::from_black_alpha(160));
            let at =
                egui::Rect::from_min_size(corner + egui::vec2(8.0, 8.0), egui::vec2(220.0, 20.0));
            ui.put(at, egui::Label::new(text).selectable(false));
        }
        if self.settings.overlay {
            crate::overlay::overlay_ui(self, ui, rect);
        } else {
            self.overlay.last = None;
        }
    }
}

/// The tiles' view of the app.
struct Behavior<'a> {
    state: &'a mut AppState,
    now: Instant,
}

impl egui_tiles::Behavior<Pane> for Behavior<'_> {
    fn pane_ui(
        &mut self,
        ui: &mut egui::Ui,
        _tile: TileId,
        pane: &mut Pane,
    ) -> egui_tiles::UiResponse {
        match pane {
            Pane::Viewer => self.state.viewer_ui(ui, self.now),
            Pane::Info => crate::panes::info_ui(self.state, ui),
            Pane::Doctor => crate::panes::doctor_ui(self.state, ui),
            Pane::Inspector => crate::inspector::inspector_ui(self.state, ui),
            Pane::Curves => crate::curves::curves_ui(self.state, ui),
            Pane::Sphere => crate::overview::overview_ui(self.state, ui),
            Pane::Files => {
                if let Some(path) = self.state.files.ui(ui) {
                    self.state.settings.last_dir = self.state.files.dir().map(PathBuf::from);
                    self.state.open(path);
                }
            }
        }
        egui_tiles::UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title().into()
    }

    fn simplification_options(&self) -> egui_tiles::SimplificationOptions {
        egui_tiles::SimplificationOptions {
            all_panes_must_have_tabs: false,
            ..Default::default()
        }
    }
}
