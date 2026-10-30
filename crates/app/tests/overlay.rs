//! The performance overlay (M1 plan section 10), through the UI: F3 puts
//! it over the viewer, and after a clip has played every line has its
//! numbers (decode, upload and render times on the CPU and, through
//! lavapipe's timestamps, the GPU; the UI's frames; frames shown; the
//! cache and the memory); F3 again takes it away.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use spherecut_app::SpherecutApp;
use spherecut_app::overlay::Readings;
use spherecut_media::backend::FfmpegBackend;
use spherecut_synth::media::{self, Codec, Spec};

/// The grid at 30 fps for 3 s: long enough for the rates over a second
/// and two readings of the memory.
fn long_clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        media::write(
            &Spec {
                name: "grid-overlay".into(),
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

fn last(h: &egui_kittest::Harness<'_, SpherecutApp>) -> Option<Readings> {
    h.state().state.overlay.last.clone()
}

#[test]
fn the_overlay_has_every_number_after_playing() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut parts = common::parts(&gpu, Arc::new(FfmpegBackend::new()));
    parts.open = vec![long_clip().to_path_buf()];
    let mut h = common::harness(SpherecutApp::new(parts), &rs, [1000.0, 640.0], 1.25);
    common::until_shown(&mut h, &rs);
    assert!(last(&h).is_none(), "the overlay is off at first");
    assert!(h.query_by_label_contains("render    CPU").is_none());

    h.key_press(egui::Key::F3);
    h.step();
    assert!(h.state().state.settings.overlay);
    let r = last(&h).expect("the overlay is on");
    // Its lines are labels a screen reader reads.
    for line in r.lines().iter().skip(1) {
        let head = &line[..10];
        let _ = h.get_by_label_contains(head);
    }
    assert!(
        r.lines()[0].starts_with("software"),
        "the first line: {}",
        r.lines()[0]
    );

    // Played for 2 s of its 3, in real time; the decode rate is read on
    // the way, while the decoder still has frames to make.
    h.state_mut().state.toggle_play();
    let start = Instant::now();
    let mut decoded_fps: f64 = 0.0;
    while start.elapsed() < Duration::from_secs(2) {
        h.step();
        if let Some(r) = last(&h) {
            decoded_fps = decoded_fps.max(r.decoded_fps.unwrap_or(0.0));
        }
        common::pace(Duration::from_millis(8));
    }
    let r = last(&h).unwrap();
    let lines = r.lines();
    eprintln!("{}", lines.join("\n"));
    assert!(decoded_fps > 0.0, "no decode rate");
    let positive = |what: &str, v: Option<f32>| {
        assert!(
            v.is_some_and(|v| v > 0.0),
            "{what}: {v:?}\n{}",
            lines.join("\n")
        );
    };
    positive("decode", r.decode.p50);
    positive("upload CPU", r.upload_cpu.p50);
    positive("upload GPU", r.upload_gpu.p50);
    positive("render CPU", r.render_cpu.p50);
    // lavapipe writes timestamps (CPU ones), which the overlay says.
    assert!(r.timestamps && r.cpu_adapter);
    positive("render GPU", r.render_gpu.p50);
    assert!(lines[3].contains("(CPU timing)"), "{}", lines[3]);
    positive("UI frame", r.ui.p50);
    assert!(r.shown > 10, "frames shown: {}", r.shown);
    let (_, draws) = r.skipped.expect("draws over the last second");
    assert!(draws > 0);
    let rect = h.state().state.viewer_rect.unwrap();
    let px = |v: f32| (v * 1.25).round() as u32;
    assert_eq!(r.size, (px(rect.width()), px(rect.height())));
    assert_eq!(r.ppp, 1.25);
    assert!(r.cache.is_some_and(|c| c.bytes > 0), "{:?}", r.cache);
    assert!(r.slots.active > 0);
    let m = r.memory.expect("the memory read");
    assert!(m.rss.is_some_and(|b| b > 0), "{m:?}");
    assert!(m.minor_faults_per_s.is_some(), "{m:?}");
    assert_eq!(lines[7], "audio     none");

    // F3 again: gone.
    h.key_press(egui::Key::F3);
    h.step();
    assert!(!h.state().state.settings.overlay);
    assert!(last(&h).is_none());
    assert!(h.query_by_label_contains("render    CPU").is_none());
}
