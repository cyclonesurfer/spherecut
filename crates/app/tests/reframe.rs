//! The reframer with the whole app (M2 plan sections 10.1 to 10.3 and
//! 11): the trim plays and loops, `Alt+K` keys the view and the viewer
//! follows the keys, a change off a key is a look-around, a change on
//! one (or of a lane's only key) is one undo step, the strip's drags are
//! edits, and a project saves, reopens and asks before it is let go.
//!
//! Each test runs on a thread marked as the UI thread, so any wait
//! through `may_block` panics: the saves and loads run on workers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DQuat;
use spherecut_app::SpherecutApp;
use spherecut_app::doc::{Doc, Lane};
use spherecut_app::reframe::{Look, NOT_KEYED, Pending, SETTLE};
use spherecut_app::transport::format_time;
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::project::Ease;
use spherecut_core::{FrameRate, Rounding, Time};
use spherecut_media::backend::{FfmpegBackend, SeekMode};
use spherecut_sphere::orient::Ypr;
use spherecut_synth::media::{self, Codec, Gop, Spec};

const RATE: FrameRate = FrameRate::FPS_30;
const FRAMES: i64 = 60;

type H = Harness<'static, SpherecutApp>;

/// 60 frames at 30 fps, a keyframe every 10.
fn clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        media::write(
            &Spec {
                name: "reframe-60".into(),
                width: 512,
                height: 256,
                rate: RATE,
                frames: FRAMES as u32,
                codec: Codec::H264Lossless,
                spherical: true,
                gop: Some(Gop {
                    keyint: 10,
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

fn t(frame: i64) -> Time {
    RATE.frame_to_time(frame).unwrap()
}

/// The app on the clip, its untitled project made, on a marked thread.
fn start(gpu: &spherecut_render::Gpu) -> (egui_wgpu::RenderState, H) {
    let rs = common::render_state(gpu);
    let mut parts = common::parts(gpu, Arc::new(FfmpegBackend::new()));
    parts.open = vec![clip().to_path_buf()];
    mark_ui_thread_with(OnBlock::Panic);
    let mut h = common::harness(SpherecutApp::new(parts), &rs, [960.0, 600.0], 1.0);
    common::until_shown(&mut h, &rs);
    common::until_project(&mut h);
    (rs, h)
}

/// eframe drops the app on the UI thread, still marked.
fn finish(h: H) {
    drop(h);
    unmark_ui_thread();
}

fn doc(h: &H) -> &Doc {
    h.state().state.reframe.doc.as_ref().expect("a project")
}

fn look(h: &H) -> Look {
    h.state().state.reframe.look
}

fn lanes(h: &H) -> [usize; 3] {
    Lane::ALL.map(|l| doc(h).lane_keys(l).len())
}

/// The frame shown, by its index.
fn shown(h: &H) -> Option<i64> {
    let f = h.state().state.snapshot.shown.first().copied().flatten()?;
    RATE.time_to_frame(f.time, Rounding::Floor).ok()
}

/// Whether the player wants to play, as it says now.
fn want_play(h: &H) -> bool {
    h.state()
        .state
        .player
        .as_ref()
        .unwrap()
        .snapshot()
        .want_play
}

fn step(h: &mut H) {
    h.step();
    common::pace(Duration::from_millis(5));
}

fn until(h: &mut H, what: &str, done: impl FnMut(&SpherecutApp) -> bool) {
    common::step_until(h, Duration::from_secs(10), what, done);
}

fn until_frame(h: &mut H, want: i64) {
    until(h, &format!("frame {want}"), |a| {
        let f = a.state.snapshot.shown.first().copied().flatten();
        f.and_then(|f| RATE.time_to_frame(f.time, Rounding::Floor).ok()) == Some(want)
    });
}

fn seek(h: &mut H, frame: i64) {
    h.state_mut().state.seek(t(frame), SeekMode::Exact);
    until_frame(h, frame);
}

fn turn(yaw_deg: f64, pitch_deg: f64) -> DQuat {
    Ypr::new(yaw_deg.to_radians(), pitch_deg.to_radians(), 0.0).to_quat()
}

fn apart_deg(a: DQuat, b: DQuat) -> f64 {
    spherecut_sphere::rotation_angle(a, b).to_degrees()
}

/// What the viewer last drew, turned.
fn drawn(rs: &egui_wgpu::RenderState) -> DQuat {
    common::last_drawn(rs).expect("a draw").view.orientation
}

/// Frame 20 keyed looking 60° round: a second key on each lane.
fn key_at_20(h: &mut H) {
    seek(h, 20);
    h.state_mut().state.view.q = turn(60.0, 0.0);
    h.key_press_modifiers(Modifiers::ALT, Key::K);
    step(h);
    assert_eq!(lanes(h), [2, 2, 2]);
    assert_eq!(doc(h).undo_label(), Some("Key the view"));
    assert_eq!(look(h), Look::Keyed);
    let v = doc(h).view_at(t(20));
    assert!(apart_deg(v.orientation, turn(60.0, 0.0)) < 1e-6);
}

/// The strip's key on `lane` at `frame`.
fn key_label(lane: Lane, frame: i64) -> String {
    format!("{} key at {}", lane.label(), format_time(t(frame)))
}

#[test]
fn the_trim_plays_to_out_then_from_in_and_loops() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu);
    seek(&mut h, 10);
    h.key_press(Key::I);
    step(&mut h);
    seek(&mut h, 20);
    h.key_press(Key::O);
    step(&mut h);
    let trim = doc(&h).clip().source;
    assert_eq!((trim.start, trim.end), (t(10), t(21)));
    assert_eq!(doc(&h).undo_label(), Some("Set out"));

    // From 15, play stops on 20, the last frame in.
    seek(&mut h, 15);
    h.key_press(Key::Space);
    step(&mut h);
    let mut seen = Vec::new();
    let start = Instant::now();
    while want_play(&h) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "play never stopped"
        );
        step(&mut h);
        seen.extend(shown(&h));
    }
    until_frame(&mut h, 20);
    assert!(seen.iter().all(|f| (15..=20).contains(f)), "{seen:?}");

    // Play at out starts again from in.
    h.key_press(Key::Space);
    step(&mut h);
    let mut seen = Vec::new();
    let start = Instant::now();
    while want_play(&h) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "play never stopped"
        );
        step(&mut h);
        seen.extend(shown(&h));
    }
    until_frame(&mut h, 20);
    assert!(seen.iter().all(|f| (10..=20).contains(f)), "{seen:?}");
    assert!(
        seen.iter().any(|&f| f <= 13),
        "play didn't start at in: {seen:?}"
    );

    // Looping, play wraps from out to in and goes on.
    h.key_press_modifiers(Modifiers::COMMAND, Key::L);
    h.key_press(Key::Space);
    step(&mut h);
    assert!(h.state().state.reframe.looping);
    let mut seen = Vec::new();
    let start = Instant::now();
    let mut wraps = 0;
    while wraps < 2 {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no two wraps: {seen:?}"
        );
        assert!(want_play(&h), "looping play stopped: {seen:?}");
        step(&mut h);
        if let Some(f) = shown(&h) {
            if seen.last().is_some_and(|&l| f < l) {
                wraps += 1;
            }
            seen.push(f);
        }
    }
    assert!(seen.iter().all(|f| (10..=20).contains(f)), "{seen:?}");
    h.key_press(Key::Space);
    step(&mut h);
    assert!(!want_play(&h));
    finish(h);
}

#[test]
fn alt_k_keys_the_view_the_viewer_follows_and_undo_takes_it_back() {
    let gpu = common::gpu();
    let (rs, mut h) = start(&gpu);
    assert_eq!(lanes(&h), [1, 1, 1]);
    key_at_20(&mut h);

    // Between the keys the viewer draws the keys' view, part way round.
    seek(&mut h, 10);
    let want = doc(&h).view_at(t(10)).orientation;
    until(&mut h, "the keyed view at 10", |_| {
        common::last_drawn(&rs).is_some_and(|d| apart_deg(d.view.orientation, want) < 1e-6)
    });
    let yaw = Ypr::from_quat(want).yaw.to_degrees().abs();
    assert!(yaw > 5.0 && yaw < 55.0, "{yaw}");

    // Alt+] and Alt+[ go to the keys.
    h.key_press_modifiers(Modifiers::ALT, Key::CloseBracket);
    step(&mut h);
    until_frame(&mut h, 20);
    h.key_press_modifiers(Modifiers::ALT, Key::OpenBracket);
    step(&mut h);
    until_frame(&mut h, 0);
    seek(&mut h, 10);

    // Edit ▸ Undo, by its step's name.
    h.get_by_label("Edit").click();
    step(&mut h);
    step(&mut h);
    h.get_by_label("Undo Key the view Ctrl+Z").click();
    step(&mut h);
    step(&mut h);
    assert_eq!(lanes(&h), [1, 1, 1]);
    assert_eq!(doc(&h).redo_label(), Some("Key the view"));
    step(&mut h);
    until(&mut h, "the static view at 10", |_| {
        common::last_drawn(&rs)
            .is_some_and(|d| apart_deg(d.view.orientation, DQuat::IDENTITY) < 1e-6)
    });

    // Ctrl+Shift+Z redoes it.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    step(&mut h);
    assert_eq!(lanes(&h), [2, 2, 2]);
    assert_eq!(doc(&h).undo_label(), Some("Key the view"));
    until(&mut h, "the keyed view at 10 again", |_| {
        common::last_drawn(&rs).is_some_and(|d| apart_deg(d.view.orientation, want) < 1e-6)
    });
    finish(h);
}

#[test]
fn a_change_off_a_key_is_a_look_around_until_the_playhead_moves() {
    let gpu = common::gpu();
    let (rs, mut h) = start(&gpu);
    key_at_20(&mut h);
    seek(&mut h, 10);

    // Paused, off a key, two keys a lane: no key takes it.
    let since = Instant::now();
    h.state_mut().state.view.q = turn(0.0, 20.0);
    step(&mut h);
    assert!(matches!(look(&h), Look::Editing { .. }), "{:?}", look(&h));
    until(&mut h, "the change to settle", |a| {
        !matches!(a.state.reframe.look, Look::Editing { .. })
    });
    assert!(since.elapsed() >= SETTLE);
    assert_eq!(look(&h), Look::Around);
    assert_eq!(doc(&h).undo_label(), Some("Key the view"));
    let _ = h.get_by_label(NOT_KEYED);
    // The look stays where it was put.
    step(&mut h);
    assert!(apart_deg(drawn(&rs), turn(0.0, 20.0)) < 1e-6);

    // A frame step goes back to the keys.
    h.key_press(Key::ArrowRight);
    step(&mut h);
    until_frame(&mut h, 11);
    step(&mut h);
    assert_eq!(look(&h), Look::Keyed);
    assert!(h.query_by_label(NOT_KEYED).is_none());
    let want = doc(&h).view_at(t(11)).orientation;
    until(&mut h, "the keyed view at 11", |_| {
        common::last_drawn(&rs).is_some_and(|d| apart_deg(d.view.orientation, want) < 1e-6)
    });

    // Playing, any change is a look-around; pausing keeps it, Home
    // ends it.
    h.key_press(Key::Space);
    step(&mut h);
    assert!(want_play(&h));
    h.state_mut().state.view.q = turn(-30.0, 0.0);
    step(&mut h);
    assert_eq!(look(&h), Look::Around);
    h.key_press(Key::Space);
    step(&mut h);
    assert_eq!(look(&h), Look::Around);
    h.key_press(Key::Home);
    step(&mut h);
    assert_eq!(look(&h), Look::Keyed);
    assert_eq!(doc(&h).undo_label(), Some("Key the view"));
    finish(h);
}

#[test]
fn a_drag_on_a_key_edits_it_as_one_step() {
    let gpu = common::gpu();
    let (rs, mut h) = start(&gpu);
    key_at_20(&mut h);
    step(&mut h);

    let rect = h.state().state.viewer_rect.unwrap();
    let from = rect.center();
    let to = from + egui::vec2(80.0, 0.0);
    h.hover_at(from);
    h.drag_at(from);
    step(&mut h);
    for i in 1..=4 {
        h.hover_at(from + (to - from) * (i as f32 / 4.0));
        step(&mut h);
        // The frame after the drag starts sees the view move.
        if i >= 2 {
            assert!(matches!(look(&h), Look::Editing { .. }), "{:?}", look(&h));
        }
    }
    h.drop_at(to);
    step(&mut h);
    until(&mut h, "the drag's edit", |a| {
        a.state.reframe.look == Look::Keyed
    });
    assert_eq!(doc(&h).undo_label(), Some("Reframe"));
    assert_eq!(lanes(&h), [2, 2, 2]);
    let q = h.state().state.view.q;
    let keyed = doc(&h).view_at(t(20)).orientation;
    assert!(apart_deg(q, keyed) < 1e-6);
    assert!(
        apart_deg(keyed, turn(60.0, 0.0)) > 5.0,
        "the drag didn't turn it"
    );

    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    step(&mut h);
    assert_eq!(doc(&h).undo_label(), Some("Key the view"));
    until(&mut h, "the view before the drag", |_| {
        common::last_drawn(&rs)
            .is_some_and(|d| apart_deg(d.view.orientation, turn(60.0, 0.0)) < 1e-6)
    });
    finish(h);
}

#[test]
fn a_lanes_only_key_takes_a_static_reframe() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu);
    seek(&mut h, 30);
    let before = h.state().state.view.lens.hfov;
    h.key_press(Key::Plus);
    step(&mut h);
    until(&mut h, "the zoom's edit", |a| {
        a.state.reframe.look == Look::Keyed
            && a.state.reframe.doc.as_ref().unwrap().undo_label() == Some("Reframe")
    });
    assert_eq!(lanes(&h), [1, 1, 1]);
    let hfov = h.state().state.view.lens.hfov;
    assert!(hfov < before);
    for f in [0, 30, 59] {
        let v = doc(&h).view_at(t(f));
        assert!((v.lens.hfov - hfov).abs() < 1e-9, "frame {f}: {v:?}");
    }
    finish(h);
}

#[test]
fn the_strip_selects_moves_and_eases_keys_and_drags_the_trim() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu);
    key_at_20(&mut h);
    seek(&mut h, 0);

    // A click selects a key and goes to it.
    let at20 = key_label(Lane::Orientation, 20);
    h.get_by_label(&at20).click();
    step(&mut h);
    until_frame(&mut h, 20);
    let id = doc(&h).key_at(Lane::Orientation, t(20)).unwrap().id;
    assert_eq!(doc(&h).selection.iter().copied().collect::<Vec<_>>(), [id]);

    // A drag moves it, one step, snapped to a frame.
    let c0 = h
        .get_by_label(&key_label(Lane::Orientation, 0))
        .rect()
        .center();
    let c20 = h.get_by_label(&at20).rect().center();
    let per_frame = (c20.x - c0.x) / 20.0;
    let to = egui::pos2(c20.x + 5.0 * per_frame, c20.y);
    h.hover_at(c20);
    h.drag_at(c20);
    step(&mut h);
    for i in 1..=4 {
        h.hover_at(c20 + (to - c20) * (i as f32 / 4.0));
        step(&mut h);
    }
    h.drop_at(to);
    step(&mut h);
    step(&mut h);
    assert_eq!(doc(&h).undo_label(), Some("Move keys"));
    let at: Vec<Time> = doc(&h)
        .lane_keys(Lane::Orientation)
        .iter()
        .map(|k| k.at)
        .collect();
    assert_eq!(at, [t(0), t(25)]);
    assert_eq!(doc(&h).lane_keys(Lane::Fov)[1].at, t(20));
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    step(&mut h);
    assert_eq!(doc(&h).lane_keys(Lane::Orientation)[1].at, t(20));

    // Its right-click menu sets its ease.
    h.get_by_label(&at20).click_secondary();
    step(&mut h);
    step(&mut h);
    h.get_by_label("Linear").click();
    step(&mut h);
    step(&mut h);
    assert_eq!(doc(&h).undo_label(), Some("Set ease"));
    assert_eq!(doc(&h).lane_keys(Lane::Orientation)[1].ease, Ease::LINEAR);

    // Delete removes the selection, but a lane keeps its last key.
    h.get_by_label(&at20).click();
    step(&mut h);
    h.key_press(Key::Delete);
    step(&mut h);
    assert_eq!(doc(&h).undo_label(), Some("Delete keys"));
    assert_eq!(lanes(&h), [1, 2, 2]);
    h.get_by_label(&key_label(Lane::Orientation, 0)).click();
    step(&mut h);
    h.key_press(Key::Delete);
    step(&mut h);
    assert_eq!(lanes(&h), [1, 2, 2]);
    assert!(
        h.state()
            .state
            .notices
            .shown()
            .iter()
            .any(|n| n.what == "A lane keeps its last key")
    );

    // M marks the frame; a click on the marker goes to it.
    seek(&mut h, 30);
    h.key_press(Key::M);
    step(&mut h);
    assert_eq!(doc(&h).clip().markers.len(), 1);
    seek(&mut h, 0);
    h.get_by_label("Marker 1").click();
    step(&mut h);
    until_frame(&mut h, 30);

    // The in handle's drag moves in.
    let handle = h.get_by_label("In handle").rect().center();
    let to = egui::pos2(c0.x + 5.0 * per_frame, handle.y);
    h.hover_at(handle);
    h.drag_at(handle);
    step(&mut h);
    for i in 1..=4 {
        h.hover_at(handle + (to - handle) * (i as f32 / 4.0));
        step(&mut h);
    }
    h.drop_at(to);
    step(&mut h);
    step(&mut h);
    assert_eq!(doc(&h).undo_label(), Some("Move in"));
    assert_eq!(doc(&h).clip().source.start, t(5));
    finish(h);
}

#[test]
fn a_project_saves_reopens_and_asks_before_it_is_let_go() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu);
    let path = common::tmp().join("reframe-save.spherecut");
    let _ = std::fs::remove_file(&path);
    assert!(h.state_mut().state.close_requested(), "nothing to save yet");
    key_at_20(&mut h);
    seek(&mut h, 5);
    h.key_press(Key::I);
    step(&mut h);
    assert!(doc(&h).dirty());

    // Ctrl+Shift+S: the Save as window, then its Save.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::S);
    step(&mut h);
    let save_as = h
        .state_mut()
        .state
        .reframe
        .save_as
        .as_mut()
        .expect("Save as");
    save_as.dir = common::tmp().to_path_buf();
    save_as.name = "reframe-save.spherecut".into();
    step(&mut h);
    h.get_by_label("Save").click();
    step(&mut h);
    until(&mut h, "the save", |a| {
        !a.state.reframe.saving() && a.state.reframe.doc.as_ref().unwrap().path.is_some()
    });
    assert_eq!(doc(&h).path.as_deref(), Some(path.as_path()));
    assert!(!doc(&h).dirty());
    assert!(path.is_file());
    let saved = doc(&h).project.clone();

    // A change, then reopening the file: "Save changes?", Don't save.
    h.key_press(Key::M);
    step(&mut h);
    assert!(doc(&h).dirty());
    h.state_mut().state.open(path.clone());
    // A window's first frame only sizes it.
    step(&mut h);
    step(&mut h);
    assert_eq!(
        h.state().state.reframe.confirm,
        Some(Pending::Open(path.clone()))
    );
    h.get_by_label("Don't save").click();
    step(&mut h);
    until(&mut h, "the project to load", |a| {
        let r = &a.state.reframe;
        !r.pending() && r.doc.as_ref().is_some_and(|d| d.path.is_some()) && a.state.shown.is_some()
    });
    assert!(doc(&h).project.same_content(&saved));
    assert!(doc(&h).clip().markers.is_empty());
    assert!(!doc(&h).dirty());
    seek(&mut h, 20);
    step(&mut h);
    assert!(apart_deg(h.state().state.view.q, turn(60.0, 0.0)) < 1e-6);

    // A change, then reopening: "Save changes?", Save, then the open.
    h.key_press(Key::M);
    step(&mut h);
    h.state_mut().state.open(path.clone());
    // A window's first frame only sizes it.
    step(&mut h);
    step(&mut h);
    h.get_by_label("Save").click();
    step(&mut h);
    until(&mut h, "the save, then the load", |a| {
        let r = &a.state.reframe;
        !r.pending()
            && r.confirm.is_none()
            && r.doc
                .as_ref()
                .is_some_and(|d| d.clip().markers.len() == 1 && !d.dirty())
            && a.state.shown.is_some()
    });

    // Quitting with a change asks; the answer lets the window close.
    h.key_press(Key::M);
    step(&mut h);
    assert!(!h.state_mut().state.close_requested());
    assert_eq!(h.state().state.reframe.confirm, Some(Pending::Quit));
    step(&mut h);
    step(&mut h);
    h.get_by_label("Quit without saving").click();
    step(&mut h);
    assert!(h.state_mut().state.close_requested());
    finish(h);
}
