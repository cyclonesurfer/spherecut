//! The scrub bar, the frame step keys and the overlay with the whole
//! app, on a thread marked as the UI thread, so that any wait through
//! `may_block` panics (plan criterion 8; the M1 review's defect 8).
//!
//! A drag pauses playback and seeks to keyframes while the pointer
//! moves, never showing a frame past it; resting (`EXACT_AFTER`) seeks
//! exactly; letting go plays on.
//! Each step returns within 250 ms, a bound set by lavapipe's rendering
//! that a wait on the decoder would pass.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use spherecut_app::SpherecutApp;
use spherecut_core::FrameRate;
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_media::backend::FfmpegBackend;
use spherecut_synth::media::{self, Codec, Gop, Spec};

/// The most a step may take.
const STEP: Duration = Duration::from_millis(250);

const RATE: FrameRate = FrameRate::FPS_30;
const FRAMES: i64 = 60;
const KEYINT: i64 = 10;

/// 60 frames at 30 fps, a keyframe every 10.
fn gop_clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        media::write(
            &Spec {
                name: "scrub-gop".into(),
                width: 512,
                height: 256,
                rate: RATE,
                frames: FRAMES as u32,
                codec: Codec::H264Lossless,
                spherical: true,
                gop: Some(Gop {
                    keyint: KEYINT as u32,
                    bframes: 0,
                    open: false,
                    radl: 0,
                }),
                ..Spec::default()
            },
            common::tmp(),
        )
        .unwrap()
    })
}

/// The frame shown, by its index.
fn shown(h: &Harness<'_, SpherecutApp>) -> Option<i64> {
    let f = h.state().state.snapshot.shown.first().copied().flatten()?;
    RATE.time_to_frame(f.time, spherecut_core::Rounding::Floor)
        .ok()
}

/// Whether the player wants to play, as it says now (the app's snapshot
/// is from the start of the step).
fn want_play(h: &Harness<'_, SpherecutApp>) -> bool {
    h.state()
        .state
        .player
        .as_ref()
        .unwrap()
        .snapshot()
        .want_play
}

/// One step, timed.
fn step(h: &mut Harness<'_, SpherecutApp>, what: &str) {
    let start = Instant::now();
    h.step();
    let took = start.elapsed();
    assert!(took < STEP, "{what}: a step took {took:?}");
}

/// Step until frame `want` shows, or fail after 5 s.
fn until_frame(h: &mut Harness<'_, SpherecutApp>, want: i64, what: &str) {
    let start = Instant::now();
    while shown(h) != Some(want) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{what}: frame {want} never showed (frame {:?})",
            shown(h)
        );
        step(h, what);
        common::pace(Duration::from_millis(5));
    }
}

#[test]
fn a_scrub_seeks_keyframes_while_moving_and_exactly_at_rest() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut parts = common::parts(&gpu, Arc::new(FfmpegBackend::new()));
    parts.open = vec![gop_clip().to_path_buf()];
    mark_ui_thread_with(OnBlock::Panic);
    let mut h = common::harness(SpherecutApp::new(parts), &rs, [960.0, 600.0], 1.0);
    common::until_shown(&mut h, &rs);

    // The overlay on, and playing.
    h.key_press(egui::Key::F3);
    h.key_press(egui::Key::Space);
    step(&mut h, "play");
    assert!(h.state().state.settings.overlay);
    assert!(want_play(&h));

    let bar = h.get_by_label("Scrub bar").rect();
    let at = |frame: i64| {
        egui::pos2(
            bar.left() + (frame as f32 + 0.5) / FRAMES as f32 * bar.width(),
            bar.center().y,
        )
    };

    // Grab at frame 5 and move on a frame a step: playback pauses, and
    // what shows is never past the pointer (a keyframe seek lands at or
    // before it, or on the frame itself where the cache has it).
    h.hover_at(at(5));
    h.drag_at(at(5));
    step(&mut h, "grab");
    let mut seen = Vec::new();
    for f in 33..=38 {
        h.hover_at(at(f));
        step(&mut h, "moving");
        assert!(!want_play(&h), "playback goes on during the drag");
        if let Some(k) = shown(&h) {
            assert!(k <= f, "frame {k} showed with the pointer at {f}");
            seen.push(k);
        }
    }
    eprintln!("shown while moving: {seen:?}");

    // At rest on frame 38: the exact seek.
    until_frame(&mut h, 38, "at rest");
    assert!(!want_play(&h));

    // Let go: playback goes on from there.
    h.drop_at(at(38));
    step(&mut h, "let go");
    assert!(want_play(&h), "playback didn't resume");
    let start = Instant::now();
    while shown(&h).is_none_or(|k| k <= 38) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "playback didn't move on from frame 38"
        );
        step(&mut h, "playing on");
        common::pace(Duration::from_millis(5));
    }

    // Paused, the frame step keys land on the next and previous frames.
    h.key_press(egui::Key::Space);
    step(&mut h, "pause");
    assert!(!want_play(&h));
    // Paused where the clock stopped: the app's frame is the player's.
    let start = Instant::now();
    let mut k = loop {
        step(&mut h, "paused");
        let now = h.state().state.player.as_ref().unwrap().snapshot();
        let f = now.shown.first().copied().flatten().map(|f| f.time);
        let k = f.and_then(|t| RATE.time_to_frame(t, spherecut_core::Rounding::Floor).ok());
        if let Some(k) = k
            && Some(k) == shown(&h)
        {
            break k;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the pause didn't settle"
        );
    };
    for (key, by) in [
        (egui::Key::ArrowRight, 1),
        (egui::Key::ArrowRight, 1),
        (egui::Key::ArrowLeft, -1),
    ] {
        h.key_press(key);
        k += by;
        until_frame(&mut h, k, "a frame step");
    }

    // eframe drops the app on the UI thread, still marked.
    drop(h);
    unmark_ui_thread();
}
