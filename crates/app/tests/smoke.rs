//! The smoke driver (M1 plan section 12, item 5) under kittest, with no
//! compositor: it waits for the clip's first frame, plays it, takes the
//! screenshot, writes the JSON hw-test reads and asks the window to
//! close; and a clip that can't open ends the run with its error's code.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use egui_kittest::Harness;
use spherecut_app::smoke::{SCHEMA, SCREENSHOT_WIDTH, SmokeArgs, SmokeDriver};
use spherecut_app::{Settings, SpherecutApp};
use spherecut_media::backend::FfmpegBackend;
use spherecut_synth::media::{self, Codec, Spec};

/// The grid at 30 fps for 3 s: the run plays half of it.
fn clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        media::write(
            &Spec {
                name: "grid-smoke".into(),
                width: 1024,
                height: 512,
                frames: 90,
                codec: Codec::H264Lossless,
                spherical: true,
                ..Spec::default()
            },
            common::tmp(),
        )
        .unwrap()
    })
}

fn smoke_app(
    gpu: &spherecut_render::Gpu,
    rs: &egui_wgpu::RenderState,
    args: SmokeArgs,
) -> SpherecutApp {
    let mut parts = common::parts(gpu, Arc::new(FfmpegBackend::new()));
    parts.settings = Settings {
        overlay: true,
        ..Settings::default()
    };
    parts.open = vec![args.clip.clone()];
    let mut app = SpherecutApp::new(parts);
    app.smoke = Some(SmokeDriver::new(args, format!("{:?}", rs.target_format)));
    app
}

/// Step as a display would until the run has written its document.
fn run_to_end(h: &mut Harness<'_, SpherecutApp>) {
    common::step_until(h, Duration::from_secs(60), "the smoke run's end", |a| {
        a.smoke.as_ref().unwrap().done()
    });
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test reads the document between steps"
)]
fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn the_smoke_run_plays_measures_shoots_and_writes() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let dir = common::tmp().join("smoke-ok");
    std::fs::create_dir_all(&dir).unwrap();
    let args = SmokeArgs {
        clip: clip().to_path_buf(),
        seconds: 1.5,
        out: dir.join("smoke.json"),
        screenshot: Some(dir.join("smoke.png")),
    };
    let app = smoke_app(&gpu, &rs, args.clone());
    // 1600 × 1000 pixels: the screenshot comes out 960 × 600.
    let mut h = common::harness(app, &rs, [1280.0, 800.0], 1.25);
    run_to_end(&mut h);
    assert_eq!(h.state().smoke.as_ref().unwrap().outcome(), Some(&Ok(())));
    // The window was asked to close, once.
    h.step();

    let doc = read_json(&args.out);
    eprintln!("{}", serde_json::to_string_pretty(&doc).unwrap());
    assert_eq!(doc["schema"], SCHEMA);
    assert_eq!(doc["ok"], true);
    assert!(doc.get("error").is_none());
    let r = &doc["result"];
    assert_eq!(r["clip"], clip().display().to_string());
    assert_eq!(r["seconds"], 1.5);
    let played = r["played_s"].as_f64().unwrap();
    assert!((1.5..4.0).contains(&played), "played {played} s");
    assert_eq!(r["adapter"]["cpu"], true);
    assert!(
        r["adapter"]["name"].as_str().unwrap().contains("llvmpipe"),
        "{}",
        r["adapter"]
    );
    assert_eq!(r["surface_format"], format!("{:?}", rs.target_format));
    assert_eq!(r["scale"], 1.25);
    assert_eq!(r["window"], serde_json::json!([1600, 1000]));
    let rect = h.state().state.viewer_rect.unwrap();
    let px = |v: f32| (v * 1.25).round() as u64;
    assert_eq!(
        r["viewer"],
        serde_json::json!([px(rect.width()), px(rect.height())])
    );
    // No winit here: no refresh rate, and every pass ran the UI.
    assert!(r["refresh_hz"].is_null());
    let ui = &r["ui"];
    assert!(ui["frames"].as_u64().unwrap() >= 5, "{ui}");
    assert_eq!(ui["frames"], ui["passes"]);
    assert_eq!(ui["presented"], 1.0);
    for k in ["p50", "p95", "p99", "max"] {
        assert!(ui[k].as_f64().is_some_and(|v| v > 0.0), "ui {k}: {ui}");
    }
    let frames = &r["frames"];
    assert!(frames["shown"].as_u64().unwrap() >= 5, "{frames}");
    assert!(frames["expected"].as_f64().unwrap() > 40.0, "{frames}");
    assert_eq!(r["streams"][0]["decoder"], "software");
    assert!(
        r["streams"][0]["format"]
            .as_str()
            .unwrap()
            .starts_with("h264 1024×512")
    );
    assert_eq!(r["raw_view"], false);
    // Every stage's numbers over the run, none of them missed.
    let stages = r["stages"].as_object().unwrap();
    for name in [
        "decode",
        "upload_cpu",
        "upload_gpu",
        "render_cpu",
        "render_gpu",
        "ui_frame",
        "shown_interval",
    ] {
        let st = &stages[name];
        assert!(st["samples"].as_u64().unwrap() > 0, "{name}: {st}");
        assert_eq!(st["missed"], 0, "{name}: {st}");
        assert!(st["p95"].as_f64().unwrap() >= st["p50"].as_f64().unwrap());
    }
    assert!(!stages.contains_key("av_offset"));
    // One interval a pass, the first pass's before the run.
    assert_eq!(stages["ui_frame"]["samples"], ui["frames"]);
    assert!(r["av"]["samples"].as_u64().unwrap() > 0, "{}", r["av"]);
    assert!(r["av"]["p95_abs_ms"].as_f64().unwrap() >= 0.0);
    assert!(r["audio"].is_null(), "no audio device here");
    assert!(r["cache"]["bytes"].as_u64().unwrap() > 0);
    assert!(r["slots"]["active"].as_u64().unwrap() > 0);
    assert_eq!(r["blocked_on_ui_thread"], 0);

    // The screenshot: downscaled, the aspect kept, a picture.
    assert_eq!(
        r["screenshot"],
        args.screenshot.as_ref().unwrap().display().to_string()
    );
    assert_eq!(r["screenshot_size"], serde_json::json!([960, 600]));
    let png = image::open(args.screenshot.as_ref().unwrap())
        .unwrap()
        .to_rgb8();
    assert_eq!(png.dimensions(), (SCREENSHOT_WIDTH, 600));
    let colours: std::collections::HashSet<[u8; 3]> = png.pixels().map(|p| p.0).collect();
    assert!(colours.len() > 500, "{} colours", colours.len());
    assert!(
        doc["warnings"].as_array().unwrap().is_empty(),
        "{}",
        doc["warnings"]
    );
}

#[test]
fn a_clip_that_cant_open_ends_the_run_with_its_code() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let dir = common::tmp().join("smoke-missing");
    std::fs::create_dir_all(&dir).unwrap();
    let args = SmokeArgs {
        clip: dir.join("not-there.mp4"),
        seconds: 1.0,
        out: dir.join("smoke.json"),
        screenshot: Some(dir.join("smoke.png")),
    };
    let mut h = common::harness(smoke_app(&gpu, &rs, args.clone()), &rs, [800.0, 600.0], 1.0);
    run_to_end(&mut h);
    let Some(Err((code, message))) = h.state().smoke.as_ref().unwrap().outcome().cloned() else {
        panic!("the run didn't fail");
    };
    let doc = read_json(&args.out);
    assert_eq!(doc["schema"], SCHEMA);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["error"]["code"], code);
    assert_eq!(doc["error"]["message"], message);
    assert!(doc.get("result").is_none());
    assert!(!args.screenshot.unwrap().exists());
    eprintln!("{code}: {message}");
}

/// The watchdog's exit, as a test sees it: the status, on a channel.
fn exit_hook() -> (
    spherecut_app::smoke::ExitHook,
    std::sync::mpsc::Receiver<i32>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    (
        Box::new(move |code| {
            let _ = tx.send(code);
        }),
        rx,
    )
}

/// A window that has no UI pass, as on a locked GNOME session, where
/// the UI thread waits in presenting: the watchdog writes why and exits.
#[test]
fn the_watchdog_ends_a_run_whose_window_has_no_pass() {
    let dir = common::tmp().join("smoke-stuck");
    std::fs::create_dir_all(&dir).unwrap();
    let args = SmokeArgs {
        clip: clip().to_path_buf(),
        seconds: 1.0,
        out: dir.join("smoke.json"),
        screenshot: Some(dir.join("smoke.png")),
    };
    let mut d = SmokeDriver::new(args.clone(), "Bgra8UnormSrgb".into());
    let (hook, exited) = exit_hook();
    d.watch(Duration::from_millis(300), hook);
    assert_eq!(exited.recv_timeout(Duration::from_secs(20)), Ok(1));
    let doc = read_json(&args.out);
    assert_eq!(doc["schema"], SCHEMA);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["error"]["code"], "timed_out");
    let message = doc["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(
            "the window had no UI pass in the 0.3 s after it opened, nor did eframe's logic: \
             the UI thread is stuck"
        ),
        "{message}"
    );
    assert!(!args.screenshot.unwrap().exists());
    drop(d);
}

/// eframe's logic alone, as for a window the compositor hides: the
/// watchdog says that it ran.
#[test]
fn the_watchdog_tells_a_hidden_window_from_a_stuck_one() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let dir = common::tmp().join("smoke-hidden");
    std::fs::create_dir_all(&dir).unwrap();
    let args = SmokeArgs {
        clip: clip().to_path_buf(),
        seconds: 1.0,
        out: dir.join("smoke.json"),
        screenshot: None,
    };
    let mut h = common::harness(smoke_app(&gpu, &rs, args.clone()), &rs, [800.0, 600.0], 1.0);
    // One UI pass, then logic alone.
    let (hook, exited) = exit_hook();
    h.state_mut()
        .smoke
        .as_mut()
        .unwrap()
        .watch(Duration::from_secs(5), hook);
    h.step();
    let app = h.state_mut();
    let code = loop {
        app.logic(&egui::Context::default());
        match exited.recv_timeout(Duration::from_millis(20)) {
            Ok(code) => break code,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("{e}"),
        }
    };
    assert_eq!(code, 1);
    let doc = read_json(&args.out);
    assert_eq!(doc["error"]["code"], "timed_out");
    let message = doc["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("the window's UI passes stopped ")
            && message.contains(" s after it opened, while ")
            && message.contains(", and none came for 5 s, though eframe's logic ran ")
            && message.contains("the compositor shows nothing of the window"),
        "{message}"
    );
}

/// A run longer than the stall goes on while its passes come, writes
/// its own document, which the watchdog leaves alone, and past the
/// stall with the window still open is only exited.
#[test]
fn the_watchdog_leaves_a_finished_runs_document() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let dir = common::tmp().join("smoke-watched");
    std::fs::create_dir_all(&dir).unwrap();
    let args = SmokeArgs {
        clip: clip().to_path_buf(),
        // To the clip's end, 3 s in.
        seconds: 10.0,
        out: dir.join("smoke.json"),
        screenshot: None,
    };
    let mut h = common::harness(smoke_app(&gpu, &rs, args.clone()), &rs, [800.0, 600.0], 1.0);
    let (hook, exited) = exit_hook();
    // Shorter than the run, longer than lavapipe's slowest pass here.
    let stall = Duration::from_secs(2);
    h.state_mut().smoke.as_mut().unwrap().watch(stall, hook);
    run_to_end(&mut h);
    assert_eq!(h.state().smoke.as_ref().unwrap().outcome(), Some(&Ok(())));
    assert_eq!(
        exited.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "the watchdog fired while the passes came"
    );
    assert_eq!(read_json(&args.out)["ok"], true);
    // The window "didn't close": nothing here ends the process.
    assert_eq!(exited.recv_timeout(stall * 3), Ok(1));
    assert_eq!(read_json(&args.out)["ok"], true);
}
