//! The project tools (a2-plan sections 4 and 9.2): every op of the edit
//! reference through `edit` and back through `undo`, revisions, dry runs,
//! all or none, the user's steps, paths, paging, and times as agents
//! write them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::reference;
use spherecut_agent::rpc::{CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_agent::{Roots, ToolError};
use spherecut_core::project::folder::ProjectFolder;
use spherecut_core::project::{Project, file};
use spherecut_synth::media::{self, Spec};

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("project-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    /// Spherecut reading and writing beneath the directory.
    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), Some(&self.0)).unwrap())
    }

    /// `name`, the reference's project ([`reference::project`]) of a
    /// media file `a.mp4` that isn't there, as core writes it: the user
    /// made its one step.
    fn fixture(&self, name: &str) -> PathBuf {
        self.project(name, &self.path("a.mp4"))
    }

    /// The same, of the media file `media`.
    fn project(&self, name: &str, media: &Path) -> PathBuf {
        let (project, history) = reference::project(reference::media(media)).unwrap();
        let folder = ProjectFolder::open(&self.0).unwrap();
        folder.create(OsStr::new(name), &project, &history).unwrap();
        self.path(name)
    }

    /// A 2:1 equirect clip of ten frames, `name`.mp4 in `sub`.
    fn clip(&self, sub: &str, name: &str) -> PathBuf {
        let spec = Spec {
            name: name.into(),
            width: 256,
            height: 128,
            frames: 10,
            spherical: true,
            ..Spec::default()
        };
        media::write(&spec, &self.path(sub)).unwrap()
    }
}

fn call(tools: &Spherecut, tool: &str, args: Value) -> Result<Value, ToolError> {
    let cx = CallCx::standalone(Instant::now() + Duration::from_secs(50));
    tools.call(tool, args, &cx).map(|o| o.result)
}

fn ok(tools: &Spherecut, tool: &str, args: Value) -> Value {
    call(tools, tool, args.clone()).unwrap_or_else(|e| panic!("{tool} {args}: {e:?}"))
}

fn refused(tools: &Spherecut, tool: &str, args: Value) -> ToolError {
    match call(tools, tool, args.clone()) {
        Ok(v) => panic!("{tool} {args} wasn't refused: {v}"),
        Err(e) => e,
    }
}

fn load(path: &Path) -> Project {
    file::load(path).unwrap()
}

fn edit(file: &Path, edits: Value) -> Value {
    json!({"file": file, "edits": edits})
}

#[test]
fn every_op_of_the_reference_applies_and_undoes_to_what_was_there() {
    let dir = TempDir::new("every-op");
    dir.clip("clips", "b");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    for op in reference::OPS {
        let before = load(&p);
        let bytes = std::fs::read(&p).unwrap();
        let args = edit(&p, json!([reference::example(op)]));
        let refusal = match op.op {
            "add_clip" | "remove_clip" => Some("limit"),
            "remove_media" => Some("in_use"),
            _ => None,
        };
        if let Some(code) = refusal {
            let e = refused(&tools, "edit", args);
            assert_eq!(e.code, codes::BAD_EDIT, "{}: {e:?}", op.op);
            assert_eq!(e.argument.as_deref(), Some("edits[0]"), "{}", op.op);
            assert!(e.message.contains(code), "{}: {}", op.op, e.message);
            assert_eq!(std::fs::read(&p).unwrap(), bytes, "{} left it", op.op);
            continue;
        }
        let r = ok(&tools, "edit", args);
        let after = load(&p);
        assert_eq!(r["revision"], after.revision(), "{}: {r}", op.op);
        assert!(after.revision() > before.revision(), "{}", op.op);
        assert!(!after.same_content(&before), "{} changed nothing", op.op);
        assert_eq!(r["undo"]["agent_may"], true, "{}: {r}", op.op);
        let u = ok(&tools, "undo", json!({"file": p}));
        assert_eq!(u["undone"], r["label"], "{}: {u}", op.op);
        let undone = load(&p);
        assert!(
            undone.same_content(&before),
            "{}: undo didn't give back what was there",
            op.op
        );
        assert!(undone.revision() > after.revision(), "{}", op.op);
    }
}

#[test]
fn an_edit_names_the_ids_it_made_and_labels_its_step() {
    let dir = TempDir::new("ids");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    let r = ok(
        &tools,
        "edit",
        json!({"file": p, "label": "  Mark\nthe landing ", "edits": [
            {"op": "add_marker", "clip": 4, "marker": {"at": "4", "name": "landing"}},
            {"op": "set_trim", "clip": 4, "end": "9"},
        ]}),
    );
    assert_eq!(r["label"], "Mark the landing");
    // Only the edits that add something are listed.
    assert_eq!(r["ids"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["ids"][0]["edit"], 0, "{r}");
    let id = r["ids"][0]["ids"][0]
        .as_u64()
        .unwrap_or_else(|| panic!("{r}"));
    assert!(
        load(&p).the_clip().markers.iter().any(|m| m.id.0 == id),
        "{r}"
    );
    let r = ok(
        &tools,
        "edit",
        edit(&p, json!([{"op": "set_trim", "clip": 4, "start": "1"}])),
    );
    assert_eq!(r["label"], "Set trim");
}

#[test]
fn if_revision_holds_the_edit_to_the_project_it_was_decided_on() {
    let dir = TempDir::new("revision");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    let at = load(&p).revision();
    let trim = json!([{"op": "set_trim", "clip": 4, "end": "9"}]);
    let e = refused(
        &tools,
        "edit",
        json!({"file": p, "edits": trim, "if_revision": at + 1}),
    );
    assert_eq!(e.code, codes::STALE_REVISION);
    assert_eq!(e.argument.as_deref(), Some("if_revision"));
    let r = ok(
        &tools,
        "edit",
        json!({"file": p, "edits": trim, "if_revision": at}),
    );
    let e = refused(&tools, "undo", json!({"file": p, "if_revision": at}));
    assert_eq!(e.code, codes::STALE_REVISION);
    ok(
        &tools,
        "undo",
        json!({"file": p, "if_revision": r["revision"]}),
    );
}

#[test]
fn a_dry_run_saves_nothing() {
    let dir = TempDir::new("dry-run");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    let bytes = std::fs::read(&p).unwrap();
    let r = ok(
        &tools,
        "edit",
        json!({"file": p, "dry_run": true,
               "edits": [{"op": "set_trim", "clip": 4, "end": "9"}]}),
    );
    assert_eq!(r["dry_run"], true);
    assert!(r.get("undo").is_none(), "{r}");
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    assert!(
        !dir.path(".a.spherecut.lock").exists(),
        "a dry run took the lock"
    );
}

#[test]
fn edits_are_all_or_none() {
    let dir = TempDir::new("all-or-none");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    let bytes = std::fs::read(&p).unwrap();
    let e = refused(
        &tools,
        "edit",
        edit(
            &p,
            json!([
                {"op": "set_trim", "clip": 4, "end": "9"},
                {"op": "rename", "id": 2, "name": "Jump"},
                {"op": "remove_key", "key": 99},
            ]),
        ),
    );
    assert_eq!(e.code, codes::BAD_EDIT);
    assert_eq!(e.argument.as_deref(), Some("edits[2]"));
    assert!(e.message.starts_with("remove_key refused"), "{}", e.message);
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
}

#[test]
fn times_in_edits_are_a1s_with_frames_at_the_clips_rate() {
    let dir = TempDir::new("times");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    for (start, secs) in [
        (json!("1.5"), 1.5),
        (json!("0:02"), 2.0),
        (json!([5, 2]), 2.5),
        (json!("90f"), 3.0),
        (json!({"exact": "7/2"}), 3.5),
    ] {
        ok(
            &tools,
            "edit",
            edit(&p, json!([{"op": "set_trim", "clip": 4, "start": start}])),
        );
        let got = load(&p).the_clip().source.start.to_secs_f64();
        assert_eq!(got, secs, "{start}");
    }
    let e = refused(
        &tools,
        "edit",
        edit(&p, json!([{"op": "set_trim", "clip": 4, "start": 1.5}])),
    );
    assert_eq!(e.code, codes::BAD_ARGUMENT);
    assert_eq!(e.argument.as_deref(), Some("edits[0].start"));
}

#[test]
fn undo_takes_back_the_agents_steps_and_not_the_users() {
    let dir = TempDir::new("undo");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    // The fixture's one step is the user's.
    let e = refused(&tools, "undo", json!({"file": p}));
    assert_eq!(e.code, codes::NOT_TOP, "{e:?}");
    let summary = ok(&tools, "project", json!({"file": p}));
    assert_eq!(summary["history"]["undo"]["agent_may"], false, "{summary}");

    let trim = |end: &str| edit(&p, json!([{"op": "set_trim", "clip": 4, "end": end}]));
    ok(&tools, "edit", trim("9"));
    ok(&tools, "edit", trim("8"));
    ok(&tools, "undo", json!({"file": p}));
    assert_eq!(load(&p).the_clip().source.end.to_secs_f64(), 9.0);
    ok(&tools, "undo", json!({"file": p}));
    assert_eq!(load(&p).the_clip().source.end.to_secs_f64(), 10.0);
    let r = ok(&tools, "undo", json!({"file": p, "redo": true}));
    assert_eq!(r["redone"], "Set trim", "{r}");
    assert_eq!(load(&p).the_clip().source.end.to_secs_f64(), 9.0);
    assert_eq!(r["redo"]["agent_may"], true, "{r}");
    ok(&tools, "undo", json!({"file": p, "redo": true}));
    let e = refused(&tools, "undo", json!({"file": p, "redo": true}));
    assert_eq!(e.code, codes::NOTHING_TO_REDO);

    // A hand edit: the project rewritten without its history.
    let mut project = load(&p);
    let edits: Vec<_> =
        serde_json::from_value(json!([{"op": "rename", "id": 2, "name": "x"}])).unwrap();
    project.apply(&edits).unwrap();
    file::save(&project, &p).unwrap();
    let e = refused(&tools, "undo", json!({"file": p}));
    assert_eq!(e.code, codes::STALE_REVISION, "{e:?}");
    // An edit starts the history again, and says so.
    let r = ok(&tools, "edit", trim("7"));
    assert_eq!(r["history_reset"], true, "{r}");
    ok(&tools, "undo", json!({"file": p}));
    let e = refused(&tools, "undo", json!({"file": p}));
    assert_eq!(e.code, codes::NOTHING_TO_UNDO, "{e:?}");
}

#[test]
fn a_new_project_is_made_of_a_video_once() {
    let dir = TempDir::new("create");
    let clip = dir.clip("clips", "a");
    let tools = dir.tools();
    let r = ok(
        &tools,
        "project",
        json!({"file": "cut.spherecut", "create": {"media": clip}}),
    );
    assert_eq!(r["created"], true, "{r}");
    assert_eq!(r["revision"], 0);
    assert_eq!(r["media"][0]["status"], "present", "{r}");
    assert_eq!(r["history"]["steps"], 0, "{r}");
    let p = dir.path("cut.spherecut");
    assert_eq!(r["file"], p.to_string_lossy().as_ref());
    let project = load(&p);
    assert_eq!(project.media()[0].path, clip);
    assert_eq!(project.media()[0].video.width, 256);
    // The same call again replaces nothing.
    let bytes = std::fs::read(&p).unwrap();
    let e = refused(
        &tools,
        "project",
        json!({"file": "cut.spherecut", "create": {"media": clip}}),
    );
    assert_eq!(e.code, codes::OUTPUT_EXISTS);
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    // A new project has nothing to undo.
    let e = refused(&tools, "undo", json!({"file": p}));
    assert_eq!(e.code, codes::NOTHING_TO_UNDO);
    let e = refused(
        &tools,
        "project",
        json!({"file": "cut.json", "create": {"media": clip}}),
    );
    assert_eq!(e.code, codes::BAD_ARGUMENT);
    assert_eq!(e.argument.as_deref(), Some("file"));
    let e = refused(
        &tools,
        "project",
        json!({"file": "b.spherecut", "create": {"media": "clips/nothing.mp4"}}),
    );
    assert_eq!(e.argument.as_deref(), Some("create.media"), "{e:?}");
    assert!(!dir.path("b.spherecut").exists());
}

#[test]
fn the_summary_and_parts_name_what_the_edits_name() {
    let dir = TempDir::new("parts");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    let s = ok(&tools, "project", json!({"file": p}));
    assert_eq!(s["sequence"], 2, "{s}");
    assert_eq!(s["media"][0]["id"], 1, "{s}");
    assert_eq!(s["media"][0]["status"], "missing", "{s}");
    assert_eq!(s["clips"][0]["id"], 4, "{s}");
    assert_eq!(s["targets"][0]["id"], 10, "{s}");
    assert_eq!(s["history"]["steps"], 1, "{s}");

    let v = ok(
        &tools,
        "project",
        json!({"file": p, "part": "view", "clip": 4}),
    );
    let ids = |track: &str| -> Vec<u64> {
        v[track]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|k| k["id"].as_u64())
            .collect()
    };
    assert_eq!(ids("orientation"), [5, 8], "{v}");
    assert_eq!(ids("fov"), [6], "{v}");
    assert_eq!(ids("projection"), [7], "{v}");
    assert_eq!(v["more"], false);

    let m = ok(
        &tools,
        "project",
        json!({"file": p, "part": "markers", "clip": 4}),
    );
    assert_eq!(m["markers"][0]["id"], 9, "{m}");
    let t = ok(&tools, "project", json!({"file": p, "part": "targets"}));
    assert_eq!(t["targets"][0]["name"], "rider", "{t}");
    let h = ok(&tools, "project", json!({"file": p, "part": "history"}));
    assert_eq!(h["steps"][0]["label"], "Set up", "{h}");
    assert_eq!(h["steps"][0]["by"], "user", "{h}");
    let e = ok(&tools, "project", json!({"file": p, "part": "edits"}));
    assert_eq!(e["ops"].as_array().unwrap().len(), reference::OPS.len());
    ok(&tools, "project", json!({"file": p, "part": "sequence"}));

    // Arguments a part doesn't take.
    let e = refused(
        &tools,
        "project",
        json!({"file": p, "part": "history", "from": "1"}),
    );
    assert_eq!(e.code, codes::BAD_ARGUMENT);
    let e = refused(
        &tools,
        "project",
        json!({"file": p, "create": {"media": "a.mp4"}, "part": "view"}),
    );
    assert_eq!(e.code, codes::BAD_ARGUMENT);
}

/// A view of many keys comes in pages that miss nothing and repeat
/// nothing.
#[test]
fn a_long_view_pages_from_next_from() {
    let dir = TempDir::new("paging");
    let tools = dir.tools();
    let p = dir.fixture("a.spherecut");
    let points: Vec<Value> = (0..290)
        .map(|i| json!({"at": format!("{i}f"), "orientation": {"ypr_deg": [i, 0, 0]}}))
        .collect();
    ok(
        &tools,
        "edit",
        edit(
            &p,
            json!([{"op": "set_view_path", "clip": 4, "points": points, "replace": true}]),
        ),
    );
    let mut seen = Vec::new();
    let mut from: Option<Value> = None;
    for page in 0.. {
        assert!(page < 5, "too many pages");
        let mut args = json!({"file": p, "part": "view", "clip": 4});
        if let Some(f) = &from {
            args["from"] = f.clone();
        }
        let v = ok(&tools, "project", args);
        for track in ["orientation", "fov", "projection"] {
            let keys = v[track].as_array().unwrap();
            assert!(keys.len() <= 200, "{track}: {}", keys.len());
            seen.extend(keys.iter().map(|k| k["id"].as_u64().unwrap()));
        }
        if v["more"] != true {
            break;
        }
        from = Some(v["next_from"].clone());
        assert!(from.as_ref().unwrap().is_string(), "{}", v["next_from"]);
    }
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "a key listed twice");
    let clip = load(&p);
    let view = &clip.the_clip().view;
    let all = view.orientation.keys.len() + view.fov.keys.len() + view.projection.keys.len();
    assert_eq!(seen.len(), all);
    assert_eq!(view.orientation.keys.len(), 290);
}

#[test]
fn projects_are_written_only_beneath_the_write_root() {
    let dir = TempDir::new("roots");
    std::fs::create_dir_all(dir.path("read")).unwrap();
    std::fs::create_dir_all(dir.path("write")).unwrap();
    let inside = dir.path("read");
    let (project, history) = reference::project(reference::media(&inside.join("a.mp4"))).unwrap();
    ProjectFolder::open(&inside)
        .unwrap()
        .create(OsStr::new("a.spherecut"), &project, &history)
        .unwrap();
    let p = inside.join("a.spherecut");
    let tools = Spherecut::new(
        Roots::new(std::slice::from_ref(&inside), Some(&dir.path("write"))).unwrap(),
    );
    ok(&tools, "project", json!({"file": p}));
    let e = refused(
        &tools,
        "edit",
        edit(&p, json!([{"op": "rename", "id": 2, "name": "x"}])),
    );
    assert_eq!(e.code, codes::OUTSIDE_ROOTS, "{e:?}");
    assert_eq!(e.argument.as_deref(), Some("file"));

    // Without a write root, edit and undo aren't there to call.
    let reading = Spherecut::new(Roots::new(&[inside], None).unwrap());
    assert!(!reading.has("edit") && !reading.has("undo"));
    ok(&reading, "project", json!({"file": p}));
    let e = refused(
        &reading,
        "project",
        json!({"file": "b.spherecut", "create": {"media": "a.mp4"}}),
    );
    assert_eq!(e.code, codes::WRITE_REFUSED, "{e:?}");
}

#[test]
fn a_file_that_isnt_a_project_is_left_as_it_was() {
    let dir = TempDir::new("not-project");
    let tools = dir.tools();
    let p = dir.path("notes.spherecut");
    std::fs::write(&p, b"some notes, not a project").unwrap();
    let e = refused(
        &tools,
        "edit",
        edit(&p, json!([{"op": "rename", "id": 2, "name": "x"}])),
    );
    assert_eq!(e.code, codes::NOT_PROJECT, "{e:?}");
    assert_eq!(std::fs::read(&p).unwrap(), b"some notes, not a project");
    assert!(
        !dir.path(".notes.spherecut.lock").exists(),
        "a lock beside it"
    );
    let e = refused(&tools, "project", json!({"file": p}));
    assert_eq!(e.code, codes::NOT_PROJECT);
}

#[test]
fn media_outside_the_roots_is_named_without_being_read() {
    let dir = TempDir::new("media-outside");
    let tools = dir.tools();
    let p = dir.project("a.spherecut", Path::new("/nonexistent-spherecut/a.mp4"));
    let s = ok(&tools, "project", json!({"file": p}));
    assert_eq!(s["media"][0]["status"], "outside_roots", "{s}");
}

/// Through our own client over pipes: the list a session gets, and an
/// edit's result.
#[test]
fn a_session_lists_the_project_tools_and_edits() {
    let dir = TempDir::new("session");
    let p = dir.fixture("a.spherecut");
    let mut s = common::Session::start(Arc::new(dir.tools()));
    s.initialize("2025-06-18");
    let list = s.call_method(1, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "probe", "look", "analyze", "check", "doctor", "project", "edit", "undo", "render",
            "job"
        ]
    );
    let r = s.call_tool(
        2,
        "edit",
        json!({"file": p, "edits": [{"op": "set_trim", "clip": 4, "end": "9"}]}),
    );
    assert_ne!(r["result"]["isError"], true, "{r}");
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    let out: Value = serde_json::from_str(text).unwrap();
    assert_eq!(out["label"], "Set trim");
    let r = s.call_tool(3, "undo", json!({"file": p, "redo": 1}));
    assert_eq!(r["result"]["isError"], true, "{r}");
    s.close();
}
