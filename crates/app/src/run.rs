//! eframe's side (M1 plan section 7.1): the device, the window, the
//! persisted state and the shutdown.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_core::brand;
use spherecut_core::clock::SystemClock;
use spherecut_media::backend::FfmpegBackend;
use spherecut_media::hw::GpuIds;
use spherecut_playback::{AudioDevice, CacheBudget, DeviceAudio, Player, PlayerConfig, Repaint};
use spherecut_render::{AdapterChoice, Gpu, RenderError};

use crate::app::{AppParts, DoctorRun, Pane, Settings, SpherecutApp};
use crate::viewer::ViewerResources;

/// eframe storage keys.
const SETTINGS_KEY: &str = "spherecut-settings";
const TREE_KEY: &str = "spherecut-tiles";

/// The longest the player's threads are waited for once the window has
/// gone.
const PLAYER_BUDGET: Duration = Duration::from_secs(2);

/// The longest the jobs' threads are, from the same start: the doctor's
/// checks take up to 3 s.
const JOBS_BUDGET: Duration = Duration::from_secs(3);

/// What the CLI passes when it has no subcommand.
#[derive(Debug, Clone, Default)]
pub struct GuiArgs {
    /// Files to open; the first opens now.
    pub files: Vec<PathBuf>,
    /// `--cache-mb`: the frame cache's budget, over the environment's and
    /// the settings'.
    pub cache: Option<CacheBudget>,
    /// `--adapter`, else `SPHERECUT_ADAPTER`, else the first GPU.
    pub adapter: Option<AdapterChoice>,
}

/// Why the GUI can't run.
#[derive(Debug, thiserror::Error)]
pub enum GuiError {
    /// No Wayland or X11 display to open a window on (exit 3).
    #[error(
        "no display: neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set; \
         run Spherecut in a desktop session, or use `spherecut probe`, `doctor` or `frame`"
    )]
    NoDisplay,
    #[error(transparent)]
    Gpu(#[from] RenderError),
    #[error("the window failed: {0}")]
    Window(String),
}

impl GuiError {
    /// The code of the CLI's contract (D27).
    pub fn code(&self) -> &'static str {
        match self {
            GuiError::NoDisplay => "no_display",
            GuiError::Gpu(e) => e.code(),
            GuiError::Window(_) => "internal",
        }
    }
}

/// Whether winit 0.30 finds a display, by its own rule: a non-empty
/// `WAYLAND_DISPLAY` or `WAYLAND_SOCKET`, or a non-empty `DISPLAY`.
pub fn has_display(var: impl Fn(&str) -> Option<String>) -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"]
        .iter()
        .any(|k| var(k).is_some_and(|v| !v.is_empty()))
}

/// Put `ViewerResources` into egui-wgpu's callback resources, once, before
/// the first frame, on the UI thread: the lock is free then.
#[expect(
    clippy::disallowed_methods,
    reason = "the one write of egui-wgpu's renderer lock, at startup before any frame, uncontended"
)]
pub fn install(render_state: &egui_wgpu::RenderState, gpu: &Gpu) {
    let resources = ViewerResources::new(gpu, render_state.target_format);
    render_state
        .renderer
        .write()
        .callback_resources
        .insert(resources);
}

/// The GUI: open the device, then the window, and run until it closes.
/// The player's threads are joined after the event loop, with a 2 s
/// budget, and the jobs' (the doctor's checks, a listing, a probe) with
/// 3 s; one still in a driver call is left behind, and logged.
pub fn run(args: GuiArgs) -> Result<(), GuiError> {
    if !has_display(|k| std::env::var(k).ok()) {
        return Err(GuiError::NoDisplay);
    }
    spherecut_media::sync_ffmpeg_log_level();
    let choice = args
        .adapter
        .clone()
        .or_else(AdapterChoice::from_env)
        .unwrap_or(AdapterChoice::Auto);
    let gpu = Gpu::for_window(&choice)?;
    // Decoding runs on the rendering GPU's node, where doctor's checks run.
    let info = &gpu.caps.info;
    let ids = GpuIds {
        pci_bus_id: info.device_pci_bus_id.clone(),
        vendor: info.vendor,
        device: info.device,
    };
    let node = spherecut_media::hw::render_node(Some(&ids), None);
    let backend: Arc<FfmpegBackend> = Arc::new(FfmpegBackend::on_node(node.clone()));
    let left = Rc::new(RefCell::new(Leftover::default()));
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        // egui's pass then has one sample and no depth, as the viewer's
        // pipelines do.
        multisampling: 0,
        depth_buffer: 0,
        viewport: egui::ViewportBuilder::default()
            .with_app_id(brand::APP_ID)
            .with_title(brand::APP_NAME)
            .with_decorations(true)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([480.0, 320.0]),
        wgpu_options: egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
                instance: gpu.instance.clone(),
                adapter: gpu.adapter.clone(),
                device: gpu.device.clone(),
                queue: gpu.queue.clone(),
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let leftover = left.clone();
    let result = eframe::run_native(
        brand::APP_NAME,
        options,
        Box::new(move |cc| {
            spherecut_core::blocking::mark_ui_thread();
            let rs = cc
                .wgpu_render_state
                .as_ref()
                .ok_or("eframe made no wgpu render state")?;
            install(rs, &gpu);
            let settings: Settings = cc
                .storage
                .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
                .unwrap_or_default();
            let tree = cc
                .storage
                .and_then(|s| eframe::get_value::<egui_tiles::Tree<Pane>>(s, TREE_KEY));
            let ctx = cc.egui_ctx.clone();
            let repaint: Repaint = Arc::new(move || ctx.request_repaint());
            let chosen = CacheBudget::choose(
                args.cache,
                CacheBudget::env_value().as_deref(),
                CacheBudget::Auto,
            );
            if let Some(e) = &chosen.env_error {
                tracing::warn!(error = %e, "ignoring {}", spherecut_playback::budget::ENV);
            }
            let config = PlayerConfig {
                cache: chosen.budget,
                ..PlayerConfig::default()
            };
            let audio = DeviceAudio::new(backend.clone(), AudioDevice::Default);
            let app = SpherecutApp::new(AppParts {
                gpu: gpu.clone(),
                backend,
                clock: Arc::new(SystemClock::new()),
                audio: Some(Arc::new(audio)),
                repaint: Some(repaint),
                config,
                doctor: DoctorRun::Node(node.path.clone()),
                list_dirs: true,
                settings,
                tree,
                open: args.files,
            });
            Ok(Box::new(EframeApp { app, leftover }))
        }),
    );
    spherecut_core::blocking::unmark_ui_thread();
    join(std::mem::take(&mut *left.borrow_mut()));
    result.map_err(|e| GuiError::Window(e.to_string()))
}

/// What `on_exit` leaves for [`run`] to join.
#[derive(Default)]
struct Leftover {
    player: Option<Player>,
    threads: Vec<JoinHandle<()>>,
}

#[expect(
    clippy::disallowed_methods,
    reason = "after the event loop, on no UI thread: the player's join, with its own budget"
)]
fn join(left: Leftover) {
    let start = Instant::now();
    if let Some(player) = left.player {
        let report = player.join(PLAYER_BUDGET);
        if !report.is_clean() {
            tracing::warn!(
                panicked = ?report.panicked,
                still_running = ?report.still_running,
                "the player's threads didn't all end"
            );
        }
    }
    let still_running = join_threads(left.threads, start + JOBS_BUDGET);
    if !still_running.is_empty() {
        tracing::warn!(?still_running, "the jobs' threads didn't all end");
    }
}

/// Join `threads` as they end, until `deadline`. Those still running
/// are left detached; their names come back.
#[expect(
    clippy::disallowed_methods,
    reason = "after the event loop, on no UI thread: the jobs' joins, with a budget"
)]
fn join_threads(mut threads: Vec<JoinHandle<()>>, deadline: Instant) -> Vec<String> {
    spherecut_core::may_block("joining the jobs' threads");
    let name = |h: &JoinHandle<()>| h.thread().name().unwrap_or("a job").to_owned();
    loop {
        let mut i = 0;
        while i < threads.len() {
            if threads[i].is_finished() {
                let h = threads.swap_remove(i);
                let n = name(&h);
                if h.join().is_err() {
                    tracing::warn!(thread = n, "a job's thread panicked");
                }
            } else {
                i += 1;
            }
        }
        if threads.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    threads.iter().map(name).collect()
}

/// The app, as eframe runs it.
struct EframeApp {
    app: SpherecutApp,
    /// Where `on_exit` leaves the player and the jobs' threads, for
    /// [`run`] to join.
    leftover: Rc<RefCell<Leftover>>,
}

impl eframe::App for EframeApp {
    /// eframe calls this before each `ui`, and alone while the window is
    /// hidden: playback goes on.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.app.state.settings);
        eframe::set_value(storage, TREE_KEY, &self.app.tree);
    }

    fn on_exit(&mut self) {
        // Tells the threads to stop, and never waits.
        self.app.begin_shutdown();
        if let Ok(mut left) = self.leftover.try_borrow_mut() {
            left.player = self.app.state.player.take();
            left.threads = self.app.take_threads();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// eframe's hidden passes reach the app's `logic`: without the
    /// forward, eframe's default does nothing and playback stops while
    /// the window is hidden.
    #[test]
    fn eframes_logic_reaches_the_apps() {
        let gpu = Gpu::headless(&AdapterChoice::Cpu).expect("lavapipe");
        let app = SpherecutApp::new(AppParts {
            gpu: gpu.clone(),
            backend: Arc::new(FfmpegBackend::new()),
            clock: Arc::new(SystemClock::new()),
            audio: None,
            repaint: None,
            config: PlayerConfig::default(),
            doctor: DoctorRun::Never,
            list_dirs: false,
            settings: Settings::default(),
            tree: None,
            open: Vec::new(),
        });
        let mut app = EframeApp {
            app,
            leftover: Rc::default(),
        };
        assert!(!app.app.state.logic_ran);
        let ctx = egui::Context::default();
        eframe::App::logic(&mut app, &ctx, &mut eframe::Frame::_new_kittest());
        assert!(app.app.state.logic_ran);
        app.app.begin_shutdown();
    }

    #[test]
    fn a_display_is_found_by_winits_rule() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert!(!has_display(env(&[])));
        assert!(!has_display(env(&[
            ("WAYLAND_DISPLAY", ""),
            ("DISPLAY", "")
        ])));
        assert!(has_display(env(&[("WAYLAND_DISPLAY", "wayland-0")])));
        assert!(has_display(env(&[("WAYLAND_SOCKET", "3")])));
        assert!(has_display(env(&[("DISPLAY", ":0")])));
        assert_eq!(GuiError::NoDisplay.code(), "no_display");
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's worker waits to be let go"
    )]
    fn the_jobs_threads_are_joined_until_the_deadline() {
        let ends = std::thread::Builder::new()
            .name("ends".into())
            .spawn(|| {})
            .unwrap();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let waits = std::thread::Builder::new()
            .name("waits".into())
            .spawn(move || {
                let _ = wait.recv();
            })
            .unwrap();
        let start = Instant::now();
        let left = join_threads(vec![ends, waits], start + Duration::from_millis(100));
        assert_eq!(left, ["waits"]);
        assert!(start.elapsed() >= Duration::from_millis(100));
        go.send(()).unwrap();
        // A thread that ends in time is joined, and the wait ends with it.
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let waits = std::thread::spawn(move || {
            let _ = wait.recv();
        });
        go.send(()).unwrap();
        let left = join_threads(vec![waits], Instant::now() + Duration::from_secs(10));
        assert!(left.is_empty());
    }
}
