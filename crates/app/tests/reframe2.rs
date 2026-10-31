//! The reframer's second part with the whole app (M2 plan sections 10.4
//! to 10.6 and 11): the curve editor's yaw drag changes only that key's
//! yaw, the sphere overview's click turns the view there, the Inspector
//! adds a target, and an export from the window reaches 100 % with the
//! UI drawing on, its "Replace the file?" window, and Cancel.
//!
//! Each test runs on a thread marked as the UI thread, so any wait
//! through `may_block` panics: the export runs while the UI steps, so
//! the export test is also the never_blocks test during an export
//! (amendment 2 item 21).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use egui_tiles::Tile;
use glam::DQuat;
use spherecut_app::SpherecutApp;
use spherecut_app::app::Pane;
use spherecut_app::doc::{Doc, Lane};
use spherecut_app::notices::NoticeKind;
use spherecut_app::transport::format_time;
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::project::{Orient, Orientation};
use spherecut_core::{FrameRate, Rounding, Time};
use spherecut_media::backend::{FfmpegBackend, SeekMode};
use spherecut_media::doctor::Severity;
use spherecut_sphere::orient::Ypr;
use spherecut_synth::media::{self, Codec, Gop, Spec};

const RATE: FrameRate = FrameRate::FPS_30;
const FRAMES: i64 = 60;

/// The most a step may take while an export runs. lavapipe runs one
/// queue in order, so a UI frame's readback waits behind the export
/// frame submitted before it: 80 to 140 ms a step at 1080p here, and
/// about 330 ms for the export's first frame, its shaders compiled. A
/// wait on the export itself would be its whole length, seconds. The
/// refresh-interval gate is gui_smoke's (Tier 2, M2 plan amendment 8).
const STEP: Duration = Duration::from_secs(1);

type H = Harness<'static, SpherecutApp>;

/// 60 frames at 30 fps, a keyframe every 10.
fn clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        media::write(
            &Spec {
                name: "reframe2-60".into(),
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
fn start(gpu: &spherecut_render::Gpu, size: [f32; 2]) -> (egui_wgpu::RenderState, H) {
    let rs = common::render_state(gpu);
    let mut parts = common::parts(gpu, Arc::new(FfmpegBackend::new()));
    parts.open = vec![clip().to_path_buf()];
    mark_ui_thread_with(OnBlock::Panic);
    let mut h = common::harness(SpherecutApp::new(parts), &rs, size, 1.0);
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

fn step(h: &mut H) {
    h.step();
    common::pace(Duration::from_millis(5));
}

fn until(h: &mut H, what: &str, done: impl FnMut(&SpherecutApp) -> bool) {
    common::step_until(h, Duration::from_secs(10), what, done);
}

fn seek(h: &mut H, frame: i64) {
    h.state_mut().state.seek(t(frame), SeekMode::Exact);
    until(h, &format!("frame {frame}"), |a| {
        let f = a.state.snapshot.shown.first().copied().flatten();
        f.and_then(|f| RATE.time_to_frame(f.time, Rounding::Floor).ok()) == Some(frame)
    });
}

fn turn(yaw_deg: f64, pitch_deg: f64) -> DQuat {
    Ypr::new(yaw_deg.to_radians(), pitch_deg.to_radians(), 0.0).to_quat()
}

fn apart_deg(a: DQuat, b: DQuat) -> f64 {
    spherecut_sphere::rotation_angle(a, b).to_degrees()
}

/// The side pane `pane` brought to the front of its tabs.
fn show(h: &mut H, pane: Pane) {
    h.state_mut()
        .tree
        .make_active(|_, tile| matches!(tile, Tile::Pane(p) if *p == pane));
    step(h);
    step(h);
}

/// The orientation key `lane`'s key at `frame` holds, in degrees.
fn ypr_at(h: &H, frame: i64) -> (f64, f64, f64) {
    let k = doc(h).key_at(Lane::Orientation, t(frame)).unwrap();
    let key = doc(h).orientation_key(k.id).unwrap();
    let Orient::View(o) = key.value else {
        panic!("an aim key")
    };
    let [y, p, r] = o.ypr_deg();
    (y, p, r)
}

/// A click at `p`: pressed and let go there.
fn click_at(h: &mut H, p: egui::Pos2) {
    h.hover_at(p);
    step(h);
    h.drag_at(p);
    step(h);
    h.drop_at(p);
    step(h);
    step(h);
}

/// Frame 20 keyed looking 60° round, 10° up: a second key on each lane.
fn key_at_20(h: &mut H) {
    seek(h, 20);
    h.state_mut().state.view.q = turn(60.0, 10.0);
    h.key_press_modifiers(Modifiers::ALT, Key::K);
    step(h);
    assert_eq!(doc(h).undo_label(), Some("Key the view"));
    assert_eq!(doc(h).lane_keys(Lane::Orientation).len(), 2);
}

/// Step until the button `label` stays where it is: a window shows a
/// frame after it opens, and moves again when its content widens it.
fn until_still(h: &mut H, label: &str) {
    let mut last = None;
    for _ in 0..20 {
        step(h);
        let at = h.query_by_label(label).map(|n| n.rect());
        if at.is_some() && at == last {
            return;
        }
        last = at;
    }
    panic!("{label} never settled: {last:?}");
}

#[test]
fn a_curve_drag_moves_only_that_keys_yaw() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu, [1100.0, 700.0]);
    key_at_20(&mut h);
    show(&mut h, Pane::Curves);
    let before0 = ypr_at(&h, 0);
    let (yaw, pitch, roll) = ypr_at(&h, 20);
    let fov = doc(&h).lane_keys(Lane::Fov).len();

    let dot = h
        .get_by_label(&format!("Yaw key at {}", format_time(t(20))))
        .rect()
        .center();
    let to = dot - egui::vec2(0.0, 30.0);
    h.hover_at(dot);
    h.drag_at(dot);
    step(&mut h);
    for i in 1..=4 {
        h.hover_at(dot + (to - dot) * (i as f32 / 4.0));
        step(&mut h);
        // A drag previews: nothing is recorded until it lets go.
        assert_eq!(doc(&h).undo_label(), Some("Key the view"));
    }
    assert!(doc(&h).preview.is_some(), "the drag previews the curve");
    h.drop_at(to);
    step(&mut h);
    step(&mut h);
    assert_eq!(doc(&h).undo_label(), Some("Move yaw"));
    assert!(doc(&h).preview.is_none());
    let (yaw2, pitch2, roll2) = ypr_at(&h, 20);
    assert!(yaw2 > yaw + 1.0, "up raises yaw: {yaw} to {yaw2}");
    assert!((pitch2 - pitch).abs() < 1e-6, "pitch {pitch} to {pitch2}");
    assert!((roll2 - roll).abs() < 1e-6, "roll {roll} to {roll2}");
    assert_eq!(ypr_at(&h, 0), before0, "the other key is as it was");
    assert_eq!(doc(&h).lane_keys(Lane::Fov).len(), fov);
    // One step back.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    step(&mut h);
    let (yaw3, _, _) = ypr_at(&h, 20);
    assert!((yaw3 - yaw).abs() < 1e-6);
    finish(h);
}

#[test]
fn a_click_on_the_overview_turns_the_view_there() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu, [1100.0, 700.0]);
    show(&mut h, Pane::Sphere);
    let _ = h.get_by_label("The sphere");
    let rect = h.state().state.reframe.overview.rect.expect("drawn");
    // 90° round to the right, 20° up, as the thumbnail maps it.
    let want = turn(90.0, 20.0);
    let p = spherecut_app::overview::to_thumb(rect, want * glam::DVec3::NEG_Z);
    click_at(&mut h, p);
    let q = h.state().state.view.q;
    assert!(apart_deg(q, want) < 1.0, "{}° off", apart_deg(q, want));
    // The only key took it, one step.
    assert_eq!(doc(&h).undo_label(), Some("Turn the view"));
    let v = doc(&h).view_at(t(0));
    assert!(apart_deg(v.orientation, want) < 1.0);
    finish(h);
}

#[test]
fn the_inspector_adds_a_target_where_the_view_looks() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu, [1100.0, 700.0]);
    show(&mut h, Pane::Inspector);
    h.state_mut().state.view.q = turn(-40.0, 5.0);
    step(&mut h);
    // At the Inspector's foot, below the fold: as a screen reader
    // presses it.
    h.get_by_label("Add a target where the view looks")
        .click_accesskit();
    step(&mut h);
    step(&mut h);
    let targets = doc(&h).project.targets();
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(targets[0].name, "Target 1");
    assert!(doc(&h).undo_label().is_some_and(|l| l.contains("target")));
    finish(h);
}

/// The export window open, named `name`, its Start clicked.
fn start_export(h: &mut H, name: &str) {
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    step(h);
    assert!(h.state().state.export.open);
    h.state_mut().state.export.name = name.into();
    until_still(h, "Start");
    h.get_by_label("Start").click();
    step(h);
}

/// Step while the export runs, each step within [`STEP`]: the UI
/// thread never waits on it. The slowest step, and the steps.
fn while_exporting(h: &mut H, limit: Duration) -> (Duration, usize) {
    let start = Instant::now();
    let (mut slowest, mut steps) = (Duration::ZERO, 0);
    loop {
        let s = Instant::now();
        h.step();
        let took = s.elapsed();
        slowest = slowest.max(took);
        steps += 1;
        assert!(took < STEP, "step {steps} took {took:?} while exporting");
        if !h.state().state.export.running() {
            return (slowest, steps);
        }
        assert!(
            start.elapsed() < limit,
            "the export didn't end in {limit:?}"
        );
        common::pace(Duration::from_micros(16_667));
    }
}

#[test]
fn an_export_from_the_window_reaches_the_end_while_the_ui_draws_on() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu, [800.0, 560.0]);
    // Frames 0 to 9: a short export at the smallest preset.
    seek(&mut h, 9);
    h.key_press(Key::O);
    step(&mut h);
    assert_eq!(doc(&h).clip().source.end, t(10));
    seek(&mut h, 0);
    let dir = common::tmp().join(format!("export-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    h.state_mut().state.files.list(dir.clone());
    let out = dir.join("ten.mp4");
    let _ = std::fs::remove_file(&out);

    // Played, looping in the trim, while it exports: the export is
    // paced a frame a UI pass.
    h.key_press_modifiers(Modifiers::COMMAND, Key::L);
    h.key_press(Key::Space);
    step(&mut h);
    start_export(&mut h, "ten");
    assert!(h.state().state.export.running());
    let mut last = 0;
    let mut seen_progress = false;
    let mut times = Vec::new();
    let start = Instant::now();
    loop {
        let s = Instant::now();
        h.step();
        let took = s.elapsed();
        times.push(took);
        assert!(
            took < STEP,
            "step {} took {took:?} while exporting",
            times.len()
        );
        assert!(h.state().state.export.pace.held() || !h.state().state.snapshot.want_play);
        let ui = &h.state().state.export;
        if let Some(p) = ui.progress() {
            assert!(p.frames_done >= last, "progress went back");
            last = p.frames_done;
            seen_progress |= p.frames_total == 10;
        }
        if !ui.running() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "no end");
        common::pace(Duration::from_micros(16_667));
    }
    let report = match h.state().state.export.last.clone() {
        Some(Ok(r)) => r,
        other => panic!("the export didn't end well: {other:?}"),
    };
    assert!(seen_progress, "the progress said 10 frames");
    assert_eq!(report.frames, 10);
    assert_eq!(report.output, out);
    assert!(out.is_file());
    let n = h.state().state.notices.get(&NoticeKind::Export).unwrap();
    assert_eq!(n.severity, Severity::Info);
    assert_eq!(n.what, format!("Exported {}", out.display()));
    assert!(n.impact.contains("10 frames"), "{}", n.impact);
    assert!(n.fix.contains("Checked: "), "{}", n.fix);
    let (held, _) = h.state().state.export.pace.waits();
    times.sort();
    eprintln!(
        "exported 10 frames while playing in {} steps, the median {:?}, the slowest {:?}; {held} \
         frames held for a UI pass",
        times.len(),
        times[times.len() / 2],
        times.last().unwrap()
    );

    // Again: the file is there, so the window asks first.
    h.key_press(Key::Space);
    step(&mut h);
    h.get_by_label("Start").click();
    step(&mut h);
    until(&mut h, "the replace window", |a| {
        a.state.export.replace.is_some()
    });
    until_still(&mut h, "Replace");
    h.get_by_label("Replace").click();
    step(&mut h);
    let (slowest, steps) = while_exporting(&mut h, Duration::from_secs(120));
    eprintln!("replaced: {steps} steps, the slowest {slowest:?}");
    assert!(matches!(h.state().state.export.last, Some(Ok(_))));

    // And Cancel leaves the file that was there.
    let size = std::fs::metadata(&out).unwrap().len();
    h.get_by_label("Start").click();
    step(&mut h);
    until(&mut h, "the replace window", |a| {
        a.state.export.replace.is_some()
    });
    until_still(&mut h, "Replace");
    h.get_by_label("Replace").click();
    step(&mut h);
    until_still(&mut h, "Cancel");
    h.get_by_label("Cancel").click();
    step(&mut h);
    while_exporting(&mut h, Duration::from_secs(60));
    assert!(matches!(
        h.state().state.export.last,
        Some(Err(spherecut_export::ExportError::Cancelled))
    ));
    assert_eq!(std::fs::metadata(&out).unwrap().len(), size);
    let n = h.state().state.notices.get(&NoticeKind::Export).unwrap();
    assert!(n.what.contains("was cancelled"), "{}", n.what);
    finish(h);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_front_of_a_360_output_turns_from_the_overview() {
    let gpu = common::gpu();
    let (_rs, mut h) = start(&gpu, [1100.0, 700.0]);
    common::set_output(
        &mut h,
        spherecut_core::project::Output::Equirect {
            width: 3840,
            orientation: Orientation::IDENTITY,
        },
    );
    show(&mut h, Pane::Sphere);
    let rect = h.state().state.reframe.overview.rect.expect("drawn");
    let want = turn(-120.0, 0.0);
    let p = spherecut_app::overview::to_thumb(rect, want * glam::DVec3::NEG_Z);
    click_at(&mut h, p);
    assert_eq!(doc(&h).undo_label(), Some("Turn the front"));
    let spherecut_core::project::Output::Equirect { orientation, .. } =
        doc(&h).project.the_sequence().output
    else {
        panic!("not 360")
    };
    assert!(apart_deg(orientation.quat(), want) < 1.0);
    // The viewer's equirect preview shows the output.
    h.state_mut().state.reframe.equirect_preview = true;
    step(&mut h);
    assert!(h.state().state.reframe.preview_front().is_some());
    finish(h);
}
