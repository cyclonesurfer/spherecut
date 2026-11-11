//! The UI keeps its passes while decode falls behind (D40): a smoke run
//! of a 30 fps clip whose decode is held to a frame in 200 ms
//! (`smoke::with_decode_floor`) shows at most half the clip's frames,
//! while the UI runs at least two passes a decoded frame and never
//! blocks. Its control is the same run with the UI waiting for each
//! decoded frame (`--smoke-control-ui-waits`), which must fall short of
//! two passes a decoded frame, and blocks. kittest has no refresh rate,
//! so passes against decodes stand in for the frames a display presents;
//! `gui_decode_behind` holds the real gates on a desktop.
//!
//! `blocked_on_ui_thread` counts since the process started, so the two
//! runs go in sequence in one test, each judged on the count's change
//! across its own run. In its own test binary, so that no other test
//! marks a thread or shares its steps' time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use spherecut_app::smoke::{SmokeArgs, SmokeDriver, with_decode_floor};
use spherecut_app::{Settings, SpherecutApp};
use spherecut_core::blocking::{
    OnBlock, blocked_on_ui_thread, mark_ui_thread_with, unmark_ui_thread,
};
use spherecut_media::backend::FfmpegBackend;
use spherecut_synth::media::{self, Codec, Spec};
use spherecut_testkit::control::{Comparison, at_least, must_fail};

/// Decode's floor: 5 frames a second of a 30 fps clip.
const FLOOR: Duration = Duration::from_millis(200);

/// How long each run plays.
const SECONDS: f64 = 3.0;

/// The test's wall-time budget (M3 plan 4.1 rule 5).
const BUDGET: Duration = Duration::from_secs(15);

/// The grid at 30 fps for 3.3 s: each run plays 3 s of it.
fn clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        media::write(
            &Spec {
                name: "grid-behind".into(),
                width: 512,
                height: 256,
                frames: 100,
                codec: Codec::H264Lossless,
                spherical: true,
                ..Spec::default()
            },
            common::tmp(),
        )
        .unwrap()
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test reads the document after the run"
)]
fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A smoke run with decode held to [`FLOOR`], the UI waiting for each
/// decoded frame if `waits`, on a marked UI thread: its document, and
/// the blocking calls the UI thread made during it.
fn run(gpu: &spherecut_render::Gpu, waits: bool) -> (Value, u64) {
    let rs = common::render_state(gpu);
    let dir = common::tmp().join(if waits { "behind-control" } else { "behind" });
    std::fs::create_dir_all(&dir).unwrap();
    let args = SmokeArgs {
        clip: clip().to_path_buf(),
        seconds: SECONDS,
        out: dir.join("smoke.json"),
        screenshot: None,
        export: None,
        decoder: None,
        decode_floor: Some(FLOOR),
        control_ui_waits: waits,
    };
    let backend = with_decode_floor(Arc::new(FfmpegBackend::new()), args.decode_floor);
    let mut parts = common::parts(gpu, backend);
    parts.settings = Settings {
        overlay: true,
        ..Settings::default()
    };
    parts.open = vec![args.clip.clone()];
    let before = blocked_on_ui_thread();
    let mut app = SpherecutApp::new(parts);
    app.smoke = Some(SmokeDriver::new(
        args.clone(),
        format!("{:?}", rs.target_format),
    ));
    let mut h = common::harness(app, &rs, [640.0, 400.0], 1.0);
    common::step_until(
        &mut h,
        Duration::from_secs(60),
        "the smoke run's end",
        |a| a.smoke.as_ref().unwrap().done(),
    );
    assert_eq!(h.state().smoke.as_ref().unwrap().outcome(), Some(&Ok(())));
    let blocked = blocked_on_ui_thread() - before;
    (read_json(&args.out), blocked)
}

/// The UI's passes against two a decoded frame, over the whole run.
fn two_passes_a_decode(r: &Value) -> Comparison<u64> {
    at_least(
        r["ui"]["frames"].as_u64().unwrap(),
        2 * r["stages"]["decode"]["samples"].as_u64().unwrap(),
    )
}

#[test]
fn the_ui_keeps_its_passes_while_decode_falls_behind() {
    let start = Instant::now();
    let gpu = common::gpu();
    clip();
    // From here on this is the UI thread, its blocking calls counted.
    mark_ui_thread_with(OnBlock::Log);

    let (doc, blocked) = run(&gpu, false);
    let r = &doc["result"];
    eprintln!(
        "held to a frame in {FLOOR:?}: {} UI passes, {} frames decoded, settled {} of {} \
         frames shown, {blocked} blocking calls",
        r["ui"]["frames"],
        r["stages"]["decode"]["samples"],
        r["settled"]["frames"]["shown"],
        r["settled"]["frames"]["expected"]
    );
    assert_eq!(r["asked"]["decode_floor_ms"], 200.0, "{}", r["asked"]);
    assert_eq!(r["asked"]["control_ui_waits"], false);
    // Decode fell behind: at most half the clip's frames shown.
    let settled = &r["settled"]["frames"];
    let shown = settled["shown"].as_f64().unwrap();
    let expected = settled["expected"].as_f64().unwrap();
    assert!(expected > 0.0 && shown <= 0.5 * expected, "{settled}");
    // It decoded, and the UI ran on.
    assert!(
        r["stages"]["decode"]["samples"].as_u64().unwrap() >= 5,
        "{}",
        r["stages"]["decode"]
    );
    let passes = two_passes_a_decode(r);
    assert_eq!(passes.passes(), Some(true), "{passes}");
    assert_eq!(blocked, 0, "the UI thread blocked while decode fell behind");

    // The control: a UI that waits for each decoded frame.
    let (doc, blocked) = run(&gpu, true);
    let r = &doc["result"];
    eprintln!(
        "the control: {} UI passes, {} frames decoded, {blocked} blocking calls",
        r["ui"]["frames"], r["stages"]["decode"]["samples"]
    );
    assert_eq!(r["asked"]["control_ui_waits"], true);
    must_fail(
        "a UI that waits for each decoded frame",
        two_passes_a_decode(r),
    );
    assert!(blocked > 0, "the control's UI never waited");
    unmark_ui_thread();

    let took = start.elapsed();
    eprintln!("the_ui_keeps_its_passes_while_decode_falls_behind took {took:?}");
    assert!(took < BUDGET, "took {took:?}, over its {BUDGET:?}");
}
