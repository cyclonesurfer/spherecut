//! What the kittest suite shares: lavapipe with the viewer's resources
//! installed, a grid clip, an app on them, and a harness that draws it.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::WgpuTestRenderer;
use spherecut_app::viewer::{DrawKey, ViewerResources};
use spherecut_app::{AppParts, DoctorRun, Settings, SpherecutApp};
use spherecut_core::SystemClock;
use spherecut_media::backend::{FfmpegBackend, MediaBackend};
use spherecut_media::doctor::{self, DoctorReport, Issue, Severity};
use spherecut_playback::{CacheBudget, PlayerConfig};
use spherecut_render::{AdapterChoice, Gpu};
use spherecut_synth::media::{self, Codec, Spec};

/// lavapipe.
pub fn gpu() -> Gpu {
    Gpu::headless(&AdapterChoice::Cpu).expect("lavapipe")
}

/// A fresh render state on `gpu`'s device (a harness takes one never
/// used before) with the viewer's resources installed, as eframe's
/// creation callback leaves it.
pub fn render_state(gpu: &Gpu) -> egui_wgpu::RenderState {
    let setup = egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
        instance: gpu.instance.clone(),
        adapter: gpu.adapter.clone(),
        device: gpu.device.clone(),
        queue: gpu.queue.clone(),
    });
    let rs =
        egui_kittest::wgpu::create_render_state(setup, egui_wgpu::RendererOptions::PREDICTABLE);
    spherecut_app::install(&rs, gpu);
    rs
}

/// A directory of this test binary's, kept for the run.
pub fn tmp() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("app-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

/// A lossless 1024x512 grid equirect of 12 frames at 30 fps, with
/// spherical metadata: its F marker (red, at −Z) is where the view looks
/// at first.
pub fn grid_clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        media::write(
            &Spec {
                name: "grid-gui".into(),
                width: 1024,
                height: 512,
                frames: 12,
                codec: Codec::H264Lossless,
                spherical: true,
                ..Spec::default()
            },
            tmp(),
        )
        .unwrap()
    })
}

/// A doctor report with `issues` and nothing else.
pub fn doctor_report(issues: Vec<Issue>) -> DoctorReport {
    DoctorReport {
        distro: doctor::Distro::Other,
        ffmpeg_version: "6.1.1".into(),
        libraries: BTreeMap::new(),
        hw_device_types: Vec::new(),
        decoders: Vec::new(),
        encoders: Vec::new(),
        gpu: doctor::Gpu {
            render_node: "/dev/dri/renderD128".into(),
            vendor: doctor::Vendor::Unknown,
            vendor_id: None,
        },
        devices: Vec::new(),
        vulkan_devices: Vec::new(),
        decode_checks: Vec::new(),
        encode_checks: Vec::new(),
        issues,
    }
}

pub fn issue(severity: Severity, what: &str) -> Issue {
    Issue {
        severity,
        what: what.into(),
        impact: "an impact".into(),
        fix: "sudo dnf install a-fix".into(),
    }
}

/// An app's parts on `gpu` and `backend`: software decoding, a 256 MiB
/// cache, no audio, no doctor report yet, no file system.
pub fn parts(gpu: &Gpu, backend: Arc<dyn MediaBackend>) -> AppParts {
    AppParts {
        gpu: gpu.clone(),
        backend,
        clock: Arc::new(SystemClock::new()),
        audio: None,
        repaint: None,
        config: PlayerConfig {
            cache: CacheBudget::Bytes(256 << 20),
            decoder: spherecut_playback::DecoderPreference::Software,
            ..PlayerConfig::default()
        },
        doctor: DoctorRun::Never,
        list_dirs: false,
        settings: Settings::default(),
        cache_override: None,
        tree: None,
        open: Vec::new(),
    }
}

/// The app on the grid clip, opened.
pub fn grid_app(gpu: &Gpu) -> SpherecutApp {
    let mut p = parts(gpu, Arc::new(FfmpegBackend::new()));
    p.open = vec![grid_clip().to_path_buf()];
    SpherecutApp::new(p)
}

/// A harness drawing `app` at `size` points and `ppp` pixels a point,
/// on `rs`.
pub fn harness<'a>(
    app: SpherecutApp,
    rs: &egui_wgpu::RenderState,
    size: [f32; 2],
    ppp: f32,
) -> Harness<'a, SpherecutApp> {
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(ppp)
        // As eframe paints every pass: the callbacks' prepare runs only
        // when a pass is rendered.
        .with_render_every_step(true)
        // A 60 Hz display's frames, so that a morph takes 15 of them.
        .with_step_dt(1.0 / 60.0)
        .renderer(WgpuTestRenderer::from_render_state(rs.clone()))
        .build_ui_state(|ui, app: &mut SpherecutApp| app.ui(ui), app)
}

/// A wait between steps, as a display's frames would leave.
#[expect(
    clippy::disallowed_methods,
    reason = "the tests pace their steps; the app's code never sleeps"
)]
pub fn pace(d: Duration) {
    std::thread::sleep(d);
}

/// Step until `done` holds, or fail after `limit`.
pub fn step_until(
    h: &mut Harness<'_, SpherecutApp>,
    limit: Duration,
    what: &str,
    mut done: impl FnMut(&SpherecutApp) -> bool,
) {
    let start = Instant::now();
    loop {
        h.step();
        if done(h.state()) {
            return;
        }
        assert!(start.elapsed() < limit, "{what}: not within {limit:?}");
        pace(Duration::from_millis(5));
    }
}

/// The viewer's draws so far: projected, and encoded only.
#[expect(
    clippy::disallowed_methods,
    reason = "a test's read of the renderer lock, between frames"
)]
pub fn draws(rs: &egui_wgpu::RenderState) -> (u64, u64) {
    let r = rs.renderer.read();
    let v = r
        .callback_resources
        .get::<ViewerResources>()
        .expect("installed");
    (v.projected, v.encoded_only)
}

/// The size of the viewer's target, as `prepare` made it.
#[expect(
    clippy::disallowed_methods,
    reason = "a test's read of the renderer lock, between frames"
)]
pub fn target_extent(rs: &egui_wgpu::RenderState) -> Option<(u32, u32)> {
    let r = rs.renderer.read();
    r.callback_resources
        .get::<ViewerResources>()
        .and_then(ViewerResources::target_extent)
}

/// What the viewer's target last projected.
#[expect(
    clippy::disallowed_methods,
    reason = "a test's read of the renderer lock, between frames"
)]
pub fn last_drawn(rs: &egui_wgpu::RenderState) -> Option<DrawKey> {
    let r = rs.renderer.read();
    r.callback_resources
        .get::<ViewerResources>()
        .and_then(|v| v.last().cloned())
}

/// Step until the viewer has projected the frame the app shows.
pub fn until_shown(h: &mut Harness<'_, SpherecutApp>, rs: &egui_wgpu::RenderState) {
    let start = Instant::now();
    loop {
        h.step();
        let shown = h.state().state.shown.clone();
        if shown.is_some() && last_drawn(rs) == shown {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "no frame shown within 20 s"
        );
        pace(Duration::from_millis(5));
    }
}
