//! The timeline prototype (M2 plan section 12, amendment 3) under
//! kittest on lavapipe: its script zooms, drags, trims and pans the
//! seeded timeline for a cycle and a bit, then the run writes its
//! document and screenshot; the timeline stays whole throughout, and
//! without a run the prototype only draws.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::wgpu::WgpuTestRenderer;
use spherecut_app::proto::{
    CLIPS, CYCLE_S, MIN_SPAN_S, ProtoArgs, SCHEMA, TRACKS, Timeline, TimelineProto,
};
use spherecut_app::smoke::{Adapter, SCREENSHOT_WIDTH};

/// 20 passes a second: a cycle is 240 of them.
const STEP_DT: f32 = 1.0 / 20.0;

fn harness<'a>(
    gpu: &spherecut_render::Gpu,
    rs: &egui_wgpu::RenderState,
    args: &ProtoArgs,
    ppp: f32,
) -> Harness<'a, TimelineProto> {
    let proto = TimelineProto::new(Adapter::of(gpu), format!("{:?}", rs.target_format), args);
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(ppp)
        .with_render_every_step(true)
        .with_step_dt(STEP_DT)
        .renderer(WgpuTestRenderer::from_render_state(rs.clone()))
        .build_ui_state(|ui, p: &mut TimelineProto| p.ui(ui, None), proto)
}

/// One pass, with the script's input for it as eframe's hook would add.
fn step(h: &mut Harness<'_, TimelineProto>) {
    let time = h.ctx.input(|i| i.time) + f64::from(STEP_DT);
    let events = h.state_mut().script(time);
    h.input_mut().events.extend(events);
    h.step();
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test reads the document after the run"
)]
fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn the_script_zooms_drags_trims_and_pans_then_the_run_writes() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let dir = common::tmp().join("proto-run");
    std::fs::create_dir_all(&dir).unwrap();
    let seconds = CYCLE_S + 1.5;
    let args = ProtoArgs {
        seconds,
        out: Some(dir.join("proto.json")),
        screenshot: Some(dir.join("proto.png")),
        zoom: 1.0,
    };
    // 1600 × 1000 pixels: the screenshot comes out 960 × 600.
    let mut h = harness(&gpu, &rs, &args, 1.25);
    let seed = Timeline::generate(spherecut_app::proto::SEED);
    let start = Instant::now();
    while !h.state().done() {
        step(&mut h);
        // Whatever the script did, every track is in order and every
        // clip long enough.
        assert!(h.state().timeline.is_valid());
        assert!(
            start.elapsed() < Duration::from_secs(240),
            "the run didn't end"
        );
    }
    assert_eq!(h.state().outcome(), Some(&Ok(())));
    // The window was asked to close.
    h.step();

    let p = h.state();
    assert_eq!(p.textures(), CLIPS);
    assert_eq!(p.timeline.clips(), CLIPS);
    // The drag moved a clip of the second track and the trim rippled the
    // fourth's; the others are as the seed made them.
    for (i, (now, was)) in p.timeline.tracks.iter().zip(&seed.tracks).enumerate() {
        assert_eq!(now != was, i == 1 || i == 3, "track {i}");
    }

    let doc = read_json(args.out.as_ref().unwrap());
    eprintln!("{}", serde_json::to_string_pretty(&doc).unwrap());
    assert_eq!(doc["schema"], SCHEMA);
    assert_eq!(doc["ok"], true);
    assert!(
        doc["warnings"].as_array().unwrap().is_empty(),
        "{}",
        doc["warnings"]
    );
    let r = &doc["result"];
    assert_eq!(r["seconds"], seconds);
    let played = r["played_s"].as_f64().unwrap();
    assert!(
        (seconds..seconds + 0.2).contains(&played),
        "played {played} s"
    );
    assert_eq!(r["adapter"]["cpu"], true);
    assert_eq!(r["surface_format"], format!("{:?}", rs.target_format));
    assert_eq!(r["scale"], 1.25);
    assert_eq!(r["zoom"], 1.0);
    assert_eq!(r["window"], serde_json::json!([1600, 1000]));
    assert!(r["refresh_hz"].is_null());
    assert_eq!(
        (&r["tracks"], &r["clips"], &r["textures"]),
        (
            &serde_json::json!(TRACKS),
            &serde_json::json!(CLIPS),
            &serde_json::json!(CLIPS)
        )
    );

    // Measured from the first second on, every pass a UI pass; without
    // a refresh rate the passes are what was expected.
    let ui = &r["ui"];
    let from = ui["from_s"].as_f64().unwrap();
    assert!((1.0..1.2).contains(&from), "{ui}");
    let frames = ui["frames"].as_u64().unwrap();
    let want = ((played - from) * f64::from(STEP_DT).recip()).round() as u64;
    assert!(
        frames.abs_diff(want) <= 2,
        "{frames} frames, {want} wanted: {ui}"
    );
    assert_eq!(ui["passes"], ui["frames"]);
    assert_eq!(ui["expected"].as_f64().unwrap(), frames as f64);
    assert_eq!(ui["presented"], 1.0);
    assert_eq!(ui["late"], 0);
    for k in ["interval_ms", "build_ms"] {
        assert_eq!(ui[k]["samples"], ui["frames"], "{k}: {ui}");
        assert!(ui[k]["p99"].as_f64().unwrap() > 0.0, "{k}: {ui}");
    }
    // kittest has no eframe to count each pass's work.
    assert_eq!(ui["work_ms"]["samples"], 0);

    // Everything the script does landed.
    let a = &r["actions"];
    assert_eq!(a["moves"], 1, "{a}");
    assert_eq!(a["trims"], 1, "{a}");
    assert_eq!(a["scrubs"], 0, "{a}");
    assert!(a["zoom_passes"].as_u64().unwrap() >= 70, "{a}");
    assert!(a["pan_passes"].as_u64().unwrap() >= 50, "{a}");
    // From the whole timeline, and a margin, to the closest zoom.
    let end = seed.end();
    let [lo, hi] = [
        r["span_s"][0].as_f64().unwrap(),
        r["span_s"][1].as_f64().unwrap(),
    ];
    assert!((lo - MIN_SPAN_S).abs() < 0.05, "{lo} s");
    assert!((hi - (end * 1.02 + 2.0)).abs() < 1e-6, "{hi} s, end {end}");
    let d = &r["drawn"];
    for k in ["clips", "thumbnails", "names"] {
        assert!(d[k].as_u64().unwrap() > 0, "{k}: {d}");
    }
    // Zoomed out, every clip is drawn, though too narrow for its name.
    assert_eq!(d["clips"], CLIPS, "{d}");
    assert!(d["names"].as_u64().unwrap() < CLIPS as u64, "{d}");
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
}

#[test]
fn without_a_run_the_prototype_only_draws() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let args = ProtoArgs {
        seconds: 1.0,
        out: None,
        screenshot: None,
        zoom: 1.0,
    };
    let mut h = harness(&gpu, &rs, &args, 1.0);
    for _ in 0..40 {
        step(&mut h);
    }
    let p = h.state();
    assert!(!p.done());
    assert!(p.outcome().is_none());
    assert_eq!(p.textures(), CLIPS);
    assert_eq!(p.timeline, Timeline::generate(spherecut_app::proto::SEED));
    let a = p.actions;
    assert_eq!(
        (a.moves, a.trims, a.scrubs, a.zoom_passes, a.pan_passes),
        (0, 0, 0, 0, 0)
    );
    // The whole timeline fits.
    let view = p.view.unwrap();
    assert_eq!(view.left_s, 0.0);
}
