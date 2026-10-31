//! A project shared with agents (a2-plan sections 6 and 9.5): an agent's
//! step is in the window's Edit menu by the agent's name and the window
//! saves its own on top, which the agent can't undo past; the window's
//! lock refuses an agent's edit until the project closes; a stale history
//! opens fresh, saying so; and a project changed on disk isn't saved
//! over, while Save as still works.
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

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::rpc::CallCx;
use spherecut_agent::{Options, Roots, ToolError};
use spherecut_app::SpherecutApp;
use spherecut_app::doc::Doc;
use spherecut_app::notices::NoticeKind;
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::project::file;
use spherecut_media::backend::FfmpegBackend;
use spherecut_synth::media::{self, Codec, Spec};

type H = Harness<'static, SpherecutApp>;

/// A folder of a test's own: an equirect clip, and `trip.spherecut`, the
/// project an agent made of it.
struct Folder {
    dir: PathBuf,
    project: PathBuf,
}

impl Folder {
    fn new(name: &str) -> Folder {
        let dir = common::tmp().join(name);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let spec = Spec {
            name: "clip".into(),
            width: 512,
            height: 256,
            frames: 30,
            codec: Codec::H264Lossless,
            spherical: true,
            ..Spec::default()
        };
        let clip = media::write(&spec, &dir).unwrap();
        let project = dir.join("trip.spherecut");
        let f = Folder { dir, project };
        f.agent(
            "project",
            json!({"file": f.project, "create": {"media": clip}}),
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

/// The app with `project` open, on a marked thread.
fn open(gpu: &spherecut_render::Gpu, project: &Path) -> (egui_wgpu::RenderState, H) {
    let rs = common::render_state(gpu);
    let mut parts = common::parts(gpu, Arc::new(FfmpegBackend::new()));
    parts.open = vec![project.to_path_buf()];
    mark_ui_thread_with(OnBlock::Panic);
    let mut h = common::harness(SpherecutApp::new(parts), &rs, [960.0, 600.0], 1.0);
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
