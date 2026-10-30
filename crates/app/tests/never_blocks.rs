//! The UI thread never waits, with the whole app (M1 plan section 8.6):
//! its backend holds every open and decode until released, and the
//! harness thread is marked, so any wait on it through `may_block` would
//! panic. Each of 60 steps returns within 250 ms, a bound set by
//! lavapipe's rendering that a hang or a wait on the decoder would pass;
//! the viewer says it is decoding meanwhile; released, the first frame
//! shows within 30 steps.
//!
//! In its own test binary, so that the other kittests' rendering doesn't
//! share its steps' time.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use spherecut_app::SpherecutApp;
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_media::backend::FfmpegBackend;
use spherecut_playback::PlayState;
use spherecut_synth::fake::GatedBackend;

/// The most a step may take.
const STEP: Duration = Duration::from_millis(250);

/// A 60 Hz display's frame, between steps.
const FRAME: Duration = Duration::from_micros(16_667);

#[test]
fn the_app_never_waits_on_a_backend_that_never_answers() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let backend = GatedBackend::new(FfmpegBackend::new());
    let gate = backend.gate.clone();
    let mut parts = common::parts(&gpu, Arc::new(backend));
    parts.open = vec![common::grid_clip().to_path_buf()];

    // From here on this is the UI thread, as eframe's creation callback
    // marks it before the app is made.
    mark_ui_thread_with(OnBlock::Panic);
    // The guard is live: a wait here would have panicked.
    assert!(std::panic::catch_unwind(|| spherecut_core::may_block("a test's wait")).is_err());
    let mut h = common::harness(SpherecutApp::new(parts), &rs, [960.0, 600.0], 1.0);
    let mut slowest = Duration::ZERO;
    for i in 0..60 {
        let start = Instant::now();
        h.step();
        let took = start.elapsed();
        slowest = slowest.max(took);
        assert!(took < STEP, "step {i} took {took:?}");
        let s = &h.state().state;
        assert!(
            s.shown.is_none(),
            "step {i}: a frame showed with the gate closed"
        );
        assert!(
            matches!(s.snapshot.state, PlayState::Opening | PlayState::Seeking),
            "step {i}: the player is {:?}",
            s.snapshot.state
        );
        common::pace(FRAME);
    }
    assert_eq!(h.state().state.placeholder(), Some("Decoding…"));
    let _ = h.get_by_label("Decoding…");
    eprintln!("gated: the slowest of 60 steps took {slowest:?}");

    gate.release();
    let mut steps = 0;
    while !(h.state().state.shown.is_some() && common::last_drawn(&rs) == h.state().state.shown) {
        steps += 1;
        assert!(
            steps <= 30,
            "no frame showed within 30 steps of the release"
        );
        let start = Instant::now();
        h.step();
        let took = start.elapsed();
        assert!(took < STEP, "step {steps} after the release took {took:?}");
        common::pace(FRAME);
    }
    eprintln!("released: the first frame showed at step {steps}");
    assert_eq!(h.state().state.placeholder(), None);
    let _ = h.get_by_label("The picture");

    // eframe drops the app on the UI thread, still marked.
    drop(h);
    unmark_ui_thread();
}
