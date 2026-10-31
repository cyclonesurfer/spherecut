//! Render jobs (a2-plan sections 5 and 9.3), on lavapipe: `render`
//! answers within a second and `job` follows it to a checked file;
//! cancelling and the server's end leave nothing; ids of another
//! server; the queue's limit and its reserved names; ranges and drafts;
//! the media a render reads; and a link planted at a queued render's
//! name.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Session, request};
use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::limits::{CANCEL_WAIT, EXIT_GRACE, QUEUED_RENDERS};
use spherecut_agent::rpc::{CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_agent::{Roots, ToolError};
use spherecut_synth::media::{self, Spec};

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

impl TempDir {
    /// A directory holding `clip.mp4`, a 2-second equirect at 30 fps, and
    /// `trip.spherecut`, a project of it.
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("render-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let spec = Spec {
            name: "clip".into(),
            width: 512,
            height: 256,
            frames: 60,
            spherical: true,
            ..Spec::default()
        };
        media::write(&spec, &dir).unwrap();
        let d = TempDir(dir);
        ok(
            &d.tools(),
            "project",
            json!({"file": d.project(), "create": {"media": d.path("clip.mp4")}}),
        );
        d
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    fn project(&self) -> PathBuf {
        self.path("trip.spherecut")
    }

    /// Spherecut reading and writing beneath the directory.
    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), Some(&self.0)).unwrap())
    }

    /// Every name in the directory, hidden ones included.
    fn names(&self) -> BTreeSet<OsString> {
        std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect()
    }

    fn render(&self, output: &str) -> Value {
        json!({"project": self.project(), "output": self.path(output)})
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

fn job(tools: &Spherecut, id: &Value) -> Value {
    ok(tools, "job", json!({"job": id}))
}

/// Poll job `id` until `done` says so, or panic after `within`.
fn until(tools: &Spherecut, id: &Value, within: Duration, done: impl Fn(&Value) -> bool) -> Value {
    let end = Instant::now() + within;
    loop {
        let j = job(tools, id);
        if done(&j) {
            return j;
        }
        assert!(Instant::now() < end, "timed out: {j}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn running(j: &Value) -> bool {
    j["state"] == "running" && j["frames"]["done"].as_u64().unwrap_or(0) > 0
}

/// PLAN's A2 render: the id within a second, frames that only grow, and
/// a file whose report and check pass.
#[test]
fn render_answers_within_a_second_and_job_follows_it_to_a_checked_file() {
    let dir = TempDir::new("follow");
    let tools = dir.tools();
    let asked = Instant::now();
    let r = ok(&tools, "render", {
        let mut a = dir.render("out.mp4");
        a["draft"] = json!(true);
        a
    });
    let took = asked.elapsed();
    assert!(took < Duration::from_secs(1), "render took {took:?}");
    assert_eq!(r["frames"], 60, "{r}");
    assert_eq!(r["ahead"], 0, "{r}");
    assert_eq!(r["draft"], true);
    assert!(r["estimate_bytes"].as_u64().unwrap() > 0, "{r}");
    let id = r["job"].clone();
    assert!(
        id.as_str().unwrap().starts_with('j'),
        "{id} isn't an id of ours"
    );

    let mut last = 0;
    let mut polls = 0;
    let done = loop {
        let j = job(&tools, &id);
        let n = j["frames"]["done"].as_u64().unwrap();
        assert!(n >= last, "frames went back from {last} to {n}: {j}");
        assert_eq!(j["frames"]["total"], 60, "{j}");
        last = n;
        polls += 1;
        match j["state"].as_str().unwrap() {
            "done" => break j,
            "queued" | "running" | "finishing" => {}
            s => panic!("{s}: {j}"),
        }
        assert!(polls < 6000, "{j}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(polls >= 2, "{polls} polls");
    let report = &done["report"];
    assert_eq!(report["frames"], 60, "{done}");
    assert_eq!(report["draft"], true);
    assert_eq!(
        report["file"].as_str().map(PathBuf::from),
        Some(dir.path("out.mp4"))
    );
    let bytes = std::fs::metadata(dir.path("out.mp4")).unwrap().len();
    assert_eq!(report["bytes"], bytes);
    assert_eq!(done["check"]["pass"], true, "{}", done["check"]);
    // The check is made once; later calls give it again.
    assert_eq!(job(&tools, &id)["check"], done["check"]);
    // Nothing else is left: no temporary file.
    let names: Vec<String> = dir
        .names()
        .into_iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.starts_with(".spherecut-")),
        "{names:?}"
    );
}

#[test]
fn a_cancelled_render_stops_within_two_seconds_and_leaves_nothing() {
    let dir = TempDir::new("cancel");
    let tools = dir.tools();
    let before = dir.names();
    let id = ok(&tools, "render", dir.render("out.mp4"))["job"].clone();
    until(&tools, &id, Duration::from_secs(30), running);
    let asked = Instant::now();
    let j = ok(&tools, "job", json!({"job": id, "cancel": true}));
    let took = asked.elapsed();
    assert_eq!(j["state"], "cancelled", "{j}");
    assert!(took <= CANCEL_WAIT, "cancel took {took:?}");
    assert_eq!(dir.names(), before);
    // A cancelled job stays cancelled.
    assert_eq!(job(&tools, &id)["state"], "cancelled");
    let again = ok(&tools, "job", json!({"job": id, "cancel": true}));
    assert_eq!(again["state"], "cancelled");
}

/// The server's end cancels its renders, within the grace, leaving no
/// file.
#[test]
fn the_input_closing_mid_render_ends_the_server_in_time_and_leaves_nothing() {
    let dir = TempDir::new("eof");
    let before = dir.names();
    let tools = Arc::new(dir.tools());
    let mut s = Session::start(tools.clone());
    s.initialize("2025-11-25");
    let r = s.call_tool(1, "render", dir.render("out.mp4"));
    let id = r["result"]["structuredContent"]["job"].clone();
    assert!(id.is_string(), "{r}");
    until(&tools, &id, Duration::from_secs(30), running);
    let closed = Instant::now();
    let ended = s.close();
    let took = closed.elapsed();
    assert!(
        took < EXIT_GRACE + Duration::from_millis(400),
        "serve took {took:?}"
    );
    assert_eq!(ended.stuck, 0);
    assert_eq!(dir.names(), before);
    assert_eq!(job(&tools, &id)["state"], "cancelled");
    // Nothing more is queued once the server has ended.
    let e = refused(&tools, "render", dir.render("later.mp4"));
    assert_eq!(e.code, codes::CANCELLED, "{e:?}");
}

/// A wait sends progress while it waits; a second call waiting at the
/// same time answers at once, so a worker is always free.
#[test]
fn one_call_waits_at_a_time_with_progress() {
    let dir = TempDir::new("wait");
    let tools = Arc::new(dir.tools());
    let mut s = Session::start(tools.clone());
    s.initialize("2025-11-25");
    let r = s.call_tool(1, "render", dir.render("out.mp4"));
    let id = r["result"]["structuredContent"]["job"].clone();
    s.send(&request(
        2,
        "tools/call",
        json!({"name": "job", "arguments": {"job": id, "wait": 40},
               "_meta": {"progressToken": "p"}}),
    ));
    let mut progress = Vec::new();
    let first = loop {
        let m = s.recv_for(Duration::from_secs(5)).expect("a message");
        if m["method"] == "notifications/progress" {
            progress.push(m["params"]["progress"].as_f64().unwrap());
            if progress.len() == 2 {
                break None;
            }
            continue;
        }
        break Some(m);
    };
    assert!(first.is_none(), "{first:?}");
    let other = s.call_method(
        3,
        "tools/call",
        json!({"name": "job", "arguments": {"job": id, "wait": 40}}),
    );
    let other = &other["result"]["structuredContent"];
    assert!(other["note"].as_str().unwrap().contains("another call"), "{other}");
    let done = loop {
        let m = s.recv_for(Duration::from_secs(45)).expect("the wait's answer");
        if m["method"] == "notifications/progress" {
            progress.push(m["params"]["progress"].as_f64().unwrap());
            continue;
        }
        break m;
    };
    assert_eq!(done["id"], 2, "{done}");
    let done = &done["result"]["structuredContent"];
    assert_eq!(done["state"], "done", "{done}");
    assert!(progress.windows(2).all(|w| w[0] < w[1]), "{progress:?}");
    s.close();
}

#[test]
fn an_id_of_another_server_is_lost_and_an_unknown_one_not_found() {
    let dir = TempDir::new("ids");
    let first = dir.tools();
    let id = ok(&first, "render", {
        let mut a = dir.render("out.mp4");
        a["draft"] = json!(true);
        a["range"] = json!({"to": "3f"});
        a
    })["job"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = dir.tools();
    let e = refused(&second, "job", json!({"job": id}));
    assert_eq!(e.code, codes::JOB_LOST, "{e:?}");
    assert!(e.next.unwrap().contains("render again"));
    let (tag, _) = id.split_once('-').unwrap();
    let e = refused(&first, "job", json!({"job": format!("{tag}-99")}));
    assert_eq!(e.code, codes::NOT_FOUND, "{e:?}");
    for bad in ["x", "j-1", "jzzzzzzzz-1", "j0123abcd-x"] {
        let e = refused(&first, "job", json!({"job": bad}));
        assert_eq!(e.code, codes::BAD_ARGUMENT, "{bad}: {e:?}");
        assert_eq!(e.argument.as_deref(), Some("job"));
    }
    let e = refused(&first, "job", json!({"job": id, "wait": 46}));
    assert_eq!(e.code, codes::OUT_OF_RANGE, "{e:?}");
    let e = refused(&first, "job", json!({"job": id, "wait": 1, "cancel": true}));
    assert_eq!(e.argument.as_deref(), Some("wait"), "{e:?}");
}

/// One runs and four wait: a fifth waiting is `busy`, and a waiting
/// render's file is refused to another render.
#[test]
fn a_fifth_queued_render_is_busy_and_a_queued_name_is_reserved() {
    let dir = TempDir::new("busy");
    let before = dir.names();
    let tools = dir.tools();
    let first = ok(&tools, "render", dir.render("r0.mp4"));
    until(&tools, &first["job"], Duration::from_secs(30), running);
    for i in 1..=QUEUED_RENDERS {
        let r = ok(&tools, "render", dir.render(&format!("r{i}.mp4")));
        assert_eq!(r["ahead"], i, "{r}");
        assert_eq!(job(&tools, &r["job"])["state"], "queued");
    }
    let e = refused(&tools, "render", dir.render("r9.mp4"));
    assert_eq!(e.code, codes::BUSY, "{e:?}");
    let e = refused(&tools, "render", dir.render("r2.mp4"));
    assert_eq!(e.code, codes::OUTPUT_EXISTS, "{e:?}");
    assert!(e.message.contains("not yet finished"), "{e:?}");
    // The server's end takes them all, leaving nothing.
    tools.shutdown(Instant::now() + Duration::from_secs(5));
    assert_eq!(dir.names(), before);
}

#[test]
fn a_range_and_a_draft_give_what_was_asked_and_a_range_outside_the_trim_is_refused() {
    let dir = TempDir::new("range");
    let tools = dir.tools();
    let mut a = dir.render("part.mp4");
    a["draft"] = json!(true);
    a["range"] = json!({"from": "10f", "to": "20f"});
    let r = ok(&tools, "render", a);
    assert_eq!(r["frames"], 10, "{r}");
    let full = ok(
        &tools,
        "render",
        json!({"project": dir.project(), "output": dir.path("whole.mp4"), "range": {"to": "2f"}}),
    );
    // A draft is half the preset's size, each side to a multiple of 8.
    let (w, h) = (full["size"][0].as_u64().unwrap(), full["size"][1].as_u64().unwrap());
    assert_eq!(r["size"], json!([w.div_ceil(16) * 8, h.div_ceil(16) * 8]));
    let done = ok(&tools, "job", json!({"job": r["job"], "wait": 40}));
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["report"]["frames"], 10);
    assert_eq!(done["report"]["size"], r["size"]);

    for (range, arg) in [
        (json!({"to": "3"}), "range.to"),
        (json!({"from": "1", "to": "1"}), "range.to"),
        (json!({"from": "61f"}), "range.from"),
    ] {
        let mut a = dir.render("no.mp4");
        a["range"] = range.clone();
        let e = refused(&tools, "render", a);
        assert_eq!(e.code, codes::OUT_OF_RANGE, "{range}: {e:?}");
        assert_eq!(e.argument.as_deref(), Some(arg), "{range}");
    }
    let e = refused(
        &tools,
        "render",
        json!({"project": dir.project(), "output": dir.path("a.mov")}),
    );
    assert_eq!(e.argument.as_deref(), Some("output"), "{e:?}");
    let e = refused(
        &tools,
        "render",
        json!({"project": dir.project(), "output": dir.path("clip.mp4")}),
    );
    assert_eq!(e.code, codes::OUTPUT_EXISTS, "{e:?}");
}

#[test]
fn a_render_reads_the_media_the_project_was_made_with() {
    let dir = TempDir::new("media");
    let tools = dir.tools();
    let clip = dir.path("clip.mp4");
    let held = dir.path("held.mp4");
    std::fs::rename(&clip, &held).unwrap();
    let e = refused(&tools, "render", dir.render("out.mp4"));
    assert_eq!(e.code, codes::MEDIA_MISSING, "{e:?}");
    assert_eq!(e.argument.as_deref(), Some("project"));

    // The same size, another first MiB.
    let mut bytes = std::fs::read(&held).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&clip, &bytes).unwrap();
    let e = refused(&tools, "render", dir.render("out.mp4"));
    assert_eq!(e.code, codes::MEDIA_CHANGED, "{e:?}");
    assert!(e.message.contains("first MiB"), "{e:?}");

    std::fs::write(&clip, &bytes[..mid]).unwrap();
    let e = refused(&tools, "render", dir.render("out.mp4"));
    assert_eq!(e.code, codes::MEDIA_CHANGED, "{e:?}");
    assert!(e.message.contains("size"), "{e:?}");
    assert!(!dir.path("out.mp4").exists());
}

/// A link put at a waiting render's name isn't followed: the render
/// fails `output_exists`, and nothing reaches the link's target.
#[test]
fn a_link_planted_at_a_queued_renders_name_is_not_followed() {
    let dir = TempDir::new("link");
    let tools = dir.tools();
    let first = ok(&tools, "render", dir.render("first.mp4"));
    until(&tools, &first["job"], Duration::from_secs(30), running);
    let mut a = dir.render("second.mp4");
    a["draft"] = json!(true);
    a["range"] = json!({"to": "3f"});
    let second = ok(&tools, "render", a);
    let elsewhere = std::env::temp_dir().join(format!("render-elsewhere-{}.mp4", std::process::id()));
    std::os::unix::fs::symlink(&elsewhere, dir.path("second.mp4")).unwrap();
    ok(&tools, "job", json!({"job": first["job"], "cancel": true}));
    let j = ok(&tools, "job", json!({"job": second["job"], "wait": 40}));
    assert_eq!(j["state"], "failed", "{j}");
    assert_eq!(j["error"]["code"], codes::OUTPUT_EXISTS, "{j}");
    assert!(!elsewhere.exists());
    assert!(
        std::fs::symlink_metadata(dir.path("second.mp4"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn spherecut_call_refuses_render_and_job() {
    let dir = TempDir::new("call");
    for tool in ["render", "job"] {
        let roots = Roots::new(std::slice::from_ref(&dir.0), Some(&dir.0)).unwrap();
        let cx = CallCx::standalone(Instant::now() + Duration::from_secs(5));
        let e = spherecut_agent::call(
            tool,
            dir.render("out.mp4"),
            spherecut_agent::Options::new(roots),
            &cx,
        )
        .unwrap_err();
        assert_eq!(e.code, codes::BAD_ARGUMENT, "{e:?}");
        assert_eq!(e.argument.as_deref(), Some("tool"));
        assert!(e.next.unwrap().contains("spherecut render"));
    }
}
