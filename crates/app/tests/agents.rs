//! A project shared with agents (a2-plan sections 6 and 9.5): an agent's
//! step is in the window's Edit menu by the agent's name and the window
//! saves its own on top, which the agent can't undo past; the window's
//! lock refuses an agent's edit until the project closes; a stale history
//! opens fresh, saying so; a project changed on disk isn't saved over,
//! while Save as still works; and an agent's project of a camera's raw
//! file stays open once its lenses do (m3-plan 13.1), while the file
//! opened alone makes no project.
//!
//! The agent's calls are `spherecut call`'s ([`spherecut_agent::call`]),
//! made from the test's thread between the window's frames. Each test
//! runs on a thread marked as the UI thread, so any wait through
//! `may_block` panics: the window's loads and saves run on workers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::rpc::CallCx;
use spherecut_agent::{Options, Roots, ToolError};
use spherecut_app::SpherecutApp;
use spherecut_app::doc::Doc;
use spherecut_app::notices::NoticeKind;
use spherecut_app::reframe::SETTLE;
use spherecut_app::viewer::{RawShown, ViewerResources};
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::project::file;
use spherecut_media::backend::FfmpegBackend;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Codec, Spec};

type H = Harness<'static, SpherecutApp>;

/// A folder of a test's own: a clip, and `trip.spherecut`, the project an
/// agent made of it.
struct Folder {
    dir: PathBuf,
    clip: PathBuf,
    project: PathBuf,
}

impl Folder {
    /// An equirect clip's.
    fn new(name: &str) -> Folder {
        Folder::of(name, |dir| {
            let spec = Spec {
                name: "clip".into(),
                width: 512,
                height: 256,
                frames: 30,
                codec: Codec::H264Lossless,
                spherical: true,
                ..Spec::default()
            };
            media::write(&spec, dir).unwrap()
        })
    }

    /// A camera's raw file's ([`write_lenses`]).
    fn lenses(name: &str) -> Folder {
        Folder::of(name, |dir| write_lenses(dir, "lenses.insv", false))
    }

    /// The clip `write` leaves in the folder, and the agent's project of
    /// it.
    fn of(name: &str, write: impl FnOnce(&Path) -> PathBuf) -> Folder {
        let dir = common::tmp().join(name);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let clip = write(&dir);
        let project = dir.join("trip.spherecut");
        let f = Folder { dir, clip, project };
        f.agent(
            "project",
            json!({"file": f.project, "create": {"media": f.clip}}),
        )
        .unwrap();
        f
    }

    /// One call of an agent's, as `spherecut call` makes it, reading and
    /// writing beneath the folder.
    fn agent(&self, tool: &str, args: Value) -> Result<Value, ToolError> {
        let roots = Roots::new(std::slice::from_ref(&self.dir), Some(&self.dir)).unwrap();
        let cx = CallCx::standalone(Instant::now() + Duration::from_secs(50));
        elsewhere(|| spherecut_agent::call(tool, args, Options::new(roots), &cx).map(|o| o.result))
    }

    /// The agent trims the clip to end at `end`, "20f" for frame 20.
    fn trim(&self, end: &str) -> Result<Value, ToolError> {
        self.trim_with(end, false)
    }

    /// [`Folder::trim`], or its dry run.
    fn trim_with(&self, end: &str, dry_run: bool) -> Result<Value, ToolError> {
        let summary = self
            .agent("project", json!({"file": self.project}))
            .unwrap();
        let clip = summary["clips"][0]["id"].clone();
        self.agent(
            "edit",
            json!({"file": self.project, "dry_run": dry_run,
                   "edits": [{"op": "set_trim", "clip": clip, "end": end}]}),
        )
    }

    fn undo(&self) -> Result<Value, ToolError> {
        self.agent("undo", json!({"file": self.project}))
    }

    /// The project as it is on disk.
    fn on_disk(&self) -> spherecut_core::project::Project {
        elsewhere(|| file::load(&self.project).unwrap())
    }
}

/// `f` run off the window's thread, as another program would: waits on
/// the marked thread panic.
fn elsewhere<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| s.spawn(f).join().unwrap())
}

/// A camera's raw file in `dir`, named `name`, as small as the probe
/// still takes for one: two video streams of one size, a lens each
/// (`panes::lenses`), the grid's and then the seam pattern's, or the other
/// way round when `swapped`. It is Matroska inside, which FFmpeg tells by
/// its bytes, not its name.
fn write_lenses(dir: &Path, name: &str, swapped: bool) -> PathBuf {
    let lens = |name: &str, pattern| {
        let spec = Spec {
            name: name.into(),
            pattern,
            width: 64,
            height: 64,
            frames: 30,
            codec: Codec::H264Lossless,
            ..Spec::default()
        };
        media::write(&spec, dir).unwrap()
    };
    let a = lens("lens-a", Pattern::Grid);
    let b = lens("lens-b", Pattern::Seam);
    let pair = if swapped { [&b, &a] } else { [&a, &b] };
    spherecut_media::encode::mux_video_streams(&pair.map(PathBuf::as_path), &dir.join(name))
        .unwrap()
}

/// What each raw view column last drew.
#[expect(
    clippy::disallowed_methods,
    reason = "a test's read of the renderer lock, between frames"
)]
fn raw_last(rs: &egui_wgpu::RenderState) -> Vec<Option<RawShown>> {
    let r = rs.renderer.read();
    r.callback_resources
        .get::<ViewerResources>()
        .map_or_else(Vec::new, ViewerResources::raw_last)
}

/// The file's bytes, read off the window's thread.
#[expect(
    clippy::disallowed_methods,
    reason = "the test's read of a file, on a thread of its own"
)]
fn bytes(path: &Path) -> Vec<u8> {
    elsewhere(|| std::fs::read(path).unwrap())
}

/// Another writer (an older build, a text editor) renames the sequence
/// and saves the project, leaving its history as it was.
fn rename(f: &Folder, name: &str) {
    let mut p = f.on_disk();
    let id = p.the_sequence().id.0;
    p.apply(&[spherecut_core::project::Edit::Rename {
        id,
        name: name.into(),
    }])
    .unwrap();
    elsewhere(|| file::save(&p, &f.project).unwrap());
}

/// The app with `path` open, a project or a video, on a marked thread;
/// `before` runs on it first, before its first pass.
fn start(
    gpu: &spherecut_render::Gpu,
    path: &Path,
    before: impl FnOnce(&mut SpherecutApp),
) -> (egui_wgpu::RenderState, H) {
    let rs = common::render_state(gpu);
    let mut parts = common::parts(gpu, Arc::new(FfmpegBackend::new()));
    parts.open = vec![path.to_path_buf()];
    mark_ui_thread_with(OnBlock::Panic);
    let mut app = SpherecutApp::new(parts);
    before(&mut app);
    let h = common::harness(app, &rs, [960.0, 600.0], 1.0);
    (rs, h)
}

/// The app with `project` open, on a marked thread.
fn open(gpu: &spherecut_render::Gpu, project: &Path) -> (egui_wgpu::RenderState, H) {
    let (rs, mut h) = start(gpu, project, |_| {});
    until(&mut h, "the project to open", |a| {
        let r = &a.state.reframe;
        !r.pending() && r.doc.as_ref().is_some_and(|d| d.path.is_some()) && a.state.shown.is_some()
    });
    (rs, h)
}

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

/// Save, then wait for it to end.
fn save(h: &mut H) {
    h.state_mut().state.save();
    until(h, "the save", |a| !a.state.reframe.saving());
}

/// File ▸ Close, then wait for the project to go.
fn close(h: &mut H) {
    h.state_mut().state.close_project();
    until(h, "the close", |a| {
        a.state.reframe.doc.is_none() && !a.state.reframe.saving()
    });
}

/// The notice of `kind` up, if one is.
fn notice(h: &H, kind: NoticeKind) -> Option<String> {
    h.state()
        .state
        .notices
        .shown()
        .into_iter()
        .find(|n| n.kind == kind)
        .map(|n| format!("{}: {}", n.what, n.impact))
}

/// PLAN's A2 test: an agent's edit, the window opens the project with
/// the agent's step in its Edit menu, makes an edit of its own, saves and
/// closes; the agent can't undo past the window's step, and the file has
/// the window's edit.
#[test]
fn the_window_undoes_an_agents_step_and_the_agent_cant_undo_the_windows() {
    let f = Folder::new("agents-plan");
    f.trim("20f").unwrap();
    let gpu = common::gpu();
    let (_rs, mut h) = open(&gpu, &f.project);
    assert_eq!(
        doc(&h).undo_label().as_deref(),
        Some("Set trim (spherecut call)")
    );
    h.get_by_label("Edit").click();
    step(&mut h);
    step(&mut h);
    h.get_by_label("Undo Set trim (spherecut call) Ctrl+Z");
    h.key_press(Key::Escape);
    step(&mut h);

    h.key_press(Key::M);
    step(&mut h);
    assert_eq!(doc(&h).clip().markers.len(), 1);
    assert!(doc(&h).dirty());
    save(&mut h);
    assert!(!doc(&h).dirty(), "{:?}", notice(&h, NoticeKind::Project));
    close(&mut h);

    let e = f.undo().unwrap_err();
    assert_eq!(e.code, codes::NOT_TOP, "{e:?}");
    let p = f.on_disk();
    assert_eq!(p.the_clip().markers.len(), 1, "the window's marker");
    let twenty = spherecut_core::FrameRate::FPS_30.frame_to_time(20).unwrap();
    assert_eq!(p.the_clip().source.end, twenty, "the agent's trim");

    finish(h);

    // Opened again, the window undoes both, its own and then the agent's.
    let (_rs, mut h) = open(&gpu, &f.project);
    assert_eq!(doc(&h).undo_label().as_deref(), Some("Add marker"));
    h.state_mut().state.undo();
    step(&mut h);
    assert_eq!(
        doc(&h).undo_label().as_deref(),
        Some("Set trim (spherecut call)")
    );
    h.state_mut().state.undo();
    step(&mut h);
    assert_eq!(doc(&h).undo_label(), None);
    assert_eq!(
        doc(&h).clip().source.end,
        f.on_disk().media()[0].video.duration
    );
    finish(h);
}

/// While the window has the project open, an agent's edit is refused,
/// naming the window; once it closes, the edit goes through.
#[test]
fn the_windows_lock_holds_off_an_agents_edit_until_it_closes() {
    let f = Folder::new("agents-lock");
    let gpu = common::gpu();
    let (_rs, mut h) = open(&gpu, &f.project);
    let before = bytes(&f.project);
    let e = f.trim("20f").unwrap_err();
    assert_eq!(e.code, codes::PROJECT_LOCKED, "{e:?}");
    assert!(e.message.contains("Spherecut's window"), "{e:?}");
    // A dry run says what the edit would meet.
    let e = f.trim_with("20f", true).unwrap_err();
    assert_eq!(e.code, codes::PROJECT_LOCKED, "{e:?}");
    assert_eq!(bytes(&f.project), before);
    close(&mut h);
    f.trim("20f").unwrap();
    finish(h);
}

/// A history some other writer left behind opens fresh with a notice; a
/// project changed on disk while open isn't saved over, and Save as
/// still saves the window's changes.
#[test]
fn a_stale_history_opens_fresh_and_a_changed_file_isnt_saved_over() {
    let f = Folder::new("agents-stale");
    f.trim("20f").unwrap();
    // Another writer (an older build, a text editor) saves the project
    // without its history.
    rename(&f, "elsewhere");

    let gpu = common::gpu();
    let (_rs, mut h) = open(&gpu, &f.project);
    let stale = notice(&h, NoticeKind::History).expect("the history's notice");
    assert!(stale.contains("left behind"), "{stale}");
    assert_eq!(doc(&h).undo_label(), None, "a fresh history");

    // Changed on disk while open: the window's save is refused.
    h.key_press(Key::M);
    step(&mut h);
    rename(&f, "theirs");
    let written = bytes(&f.project);
    save(&mut h);
    assert!(doc(&h).dirty(), "not saved");
    let why = notice(&h, NoticeKind::Project).expect("the save's notice");
    assert!(why.contains("changed on disk"), "{why}");
    assert_eq!(bytes(&f.project), written);

    // Save as still works.
    let other = f.dir.join("mine.spherecut");
    h.state_mut().state.save_to(other.clone(), false);
    until(&mut h, "the save as", |a| !a.state.reframe.saving());
    assert_eq!(doc(&h).path.as_deref(), Some(other.as_path()));
    assert!(!doc(&h).dirty());
    assert_eq!(
        elsewhere(|| file::load(&other).unwrap())
            .the_clip()
            .markers
            .len(),
        1
    );
    assert_eq!(bytes(&f.project), written);
    // The old file's lock went with it: an agent edits it now.
    f.trim("10f").unwrap();
    finish(h);
}

/// Step until the player has both lenses open and the viewer has drawn
/// each in its column.
fn until_lenses(h: &mut H, rs: &egui_wgpu::RenderState) {
    until(h, "both lenses open", |a| {
        a.state.snapshot.streams.len() == 2
    });
    until(h, "both columns drawn", |_| {
        let last = raw_last(rs);
        last.len() == 2 && last.iter().all(Option::is_some)
    });
}

/// An agent's project of a camera's raw file stays open (m3-plan 13.1):
/// the window opens it on the clip's one stream, the probe finds the
/// file's two lenses, and they open side by side in the project's raw
/// view, with the project, its history, its lock and its notices kept.
/// It is a project like any other there but for the view, which the raw
/// view has none of: a zoom makes no edit, the export waits for the
/// stitch (m3-plan 13.2), a marker is an edit, and it saves.
#[test]
fn an_agent_made_insv_project_stays_open() {
    let f = Folder::lenses("agents-insv");
    f.trim("20f").unwrap();
    // The file is written again since the agent added it, so the project
    // opens with a notice of its own.
    write_lenses(&f.dir, "lenses.insv", true);
    let other = write_lenses(&f.dir, "other.insv", false);
    let gpu = common::gpu();
    let (rs, mut h) = start(&gpu, &f.project, |_| {});
    until(&mut h, "both lenses open", |a| {
        a.state.snapshot.streams.len() == 2
    });
    assert!(
        h.state().state.reframe.doc.is_some(),
        "the project was let go when its lenses opened"
    );
    until_lenses(&mut h, &rs);
    let s = &h.state().state;
    assert!(s.raw_view());
    assert_eq!(s.columns(), 2);
    let d = doc(&h);
    assert_eq!(d.path.as_deref(), Some(f.project.as_path()));
    assert_eq!(d.name(), "trip.spherecut");
    assert_eq!(
        s.snapshot.path.as_deref(),
        d.media().map(|m| m.path.as_path())
    );
    let twenty = spherecut_core::FrameRate::FPS_30.frame_to_time(20).unwrap();
    assert_eq!(d.clip().source.end, twenty, "the agent's trim");
    assert_eq!(d.undo_label().as_deref(), Some("Set trim (spherecut call)"));
    let changed = notice(&h, NoticeKind::MediaChanged).expect("the project's notice");
    assert!(changed.contains("changed since it was added"), "{changed}");
    let lenses = notice(&h, NoticeKind::Lenses).expect("the lenses' notice");
    assert!(lenses.contains("2 lenses side by side"), "{lenses}");
    // The window holds the project still: an agent's edit waits for it.
    let e = f.trim("10f").unwrap_err();
    assert_eq!(e.code, codes::PROJECT_LOCKED, "{e:?}");

    // Another camera file's probe, landing now, opens nothing.
    let media = s.snapshot.path.clone();
    h.state_mut().state.info.open(&other);
    until(&mut h, "the other file's probe", |a| {
        !a.state.info.pending()
    });
    for _ in 0..3 {
        step(&mut h);
    }
    let s = &h.state().state;
    assert_eq!(s.snapshot.path, media);
    assert_eq!(s.snapshot.streams.len(), 2);
    assert_eq!(doc(&h).path.as_deref(), Some(f.project.as_path()));

    // The raw view has no view to change: a zoom there is no edit.
    h.key_press(Key::Plus);
    let zoomed = Instant::now();
    while zoomed.elapsed() < SETTLE * 2 {
        step(&mut h);
    }
    assert_eq!(
        doc(&h).undo_label().as_deref(),
        Some("Set trim (spherecut call)")
    );
    assert!(!doc(&h).dirty());
    // The export would render one lens as an equirect: Start starts none.
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    step(&mut h);
    assert!(h.state().state.export.open);
    h.get_by_label("Stitched export arrives with M3.11");
    h.get_by_label("Start").click();
    step(&mut h);
    assert!(!h.state().state.export.running(), "a lens exported as 360");
    h.get_by_label("Close").click();
    step(&mut h);
    assert!(!h.state().state.export.open);
    // A marker is an edit, and the project saves with it.
    h.key_press(Key::M);
    step(&mut h);
    assert_eq!(doc(&h).undo_label().as_deref(), Some("Add marker"));
    save(&mut h);
    assert!(!doc(&h).dirty(), "{:?}", notice(&h, NoticeKind::Project));
    close(&mut h);
    assert_eq!(f.on_disk().the_clip().markers.len(), 1);
    finish(h);
}

/// The control: the same file opened alone opens as its two lenses with
/// no project (a raw view has no view to key until M3 stitches it), even
/// when the probe lands after the untitled project was made, which goes.
/// A fix that never let go of the open project fails here.
#[test]
fn an_insv_opened_alone_opens_its_lenses_without_a_project() {
    let f = Folder::lenses("agents-insv-alone");
    let gpu = common::gpu();
    // The probe is put off until the untitled project is made, as a slow
    // disk's would be.
    let (rs, mut h) = start(&gpu, &f.clip, |app| app.state.info.close());
    until(&mut h, "the untitled project", |a| {
        a.state.reframe.doc.is_some()
    });
    assert_eq!(doc(&h).path, None);
    assert!(!h.state().state.raw_view(), "one picture, projected");
    h.state_mut().state.info.open(&f.clip);
    until_lenses(&mut h, &rs);
    let s = &h.state().state;
    assert!(
        s.reframe.doc.is_none(),
        "the untitled project of a camera's raw file was kept"
    );
    assert!(!s.reframe.pending(), "and none is being made");
    assert!(s.raw_view());
    assert_eq!(s.columns(), 2);
    assert!(notice(&h, NoticeKind::Lenses).is_some());
    finish(h);
}
