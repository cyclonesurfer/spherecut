//! `doctor` with a file (a1-plan section 6.5 and 11.2): the decode path
//! look takes for that file, named the same in both, and `speed`'s rate,
//! within its time. On a machine without a render node, as in CI, the
//! path is software and the fallback says why.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::error::codes;
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
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("doctor-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), None).unwrap())
    }
}

/// synth's grid, 30 fps, 256 × 128, `frames` long.
fn clip(dir: &Path, name: &str, frames: u32, spherical: bool) -> PathBuf {
    let spec = Spec {
        name: name.into(),
        width: 256,
        height: 128,
        frames,
        spherical,
        ..Spec::default()
    };
    media::write(&spec, dir).unwrap()
}

fn call(tools: &Spherecut, tool: &str, args: Value, time: Duration) -> Result<Value, ToolError> {
    let cx = CallCx::standalone(Instant::now() + time);
    tools.call(tool, args, &cx).map(|o| o.result)
}

fn doctor(tools: &Spherecut, args: Value) -> Value {
    call(tools, "doctor", args, Duration::from_secs(50)).unwrap()
}

/// Whether hardware decoding has a device to try here.
fn has_render_node() -> bool {
    Path::new("/dev/dri/renderD128").exists()
}

#[test]
fn doctor_names_the_decode_path_look_takes() {
    let dir = TempDir::new("path");
    let file = clip(&dir.0, "grid", 3, true);
    let tools = dir.tools();
    let r = doctor(&tools, json!({"file": file}));
    let f = &r["file"];
    assert_eq!(f["path"], file.to_str().unwrap(), "{f}");
    assert_eq!(f["codec"], "h264");
    assert_eq!(f["size"], json!([256, 128]));
    assert!(f.get("speed").is_none(), "{f}");
    // The report without a file is all there too.
    assert!(r["decode_checks"].is_array(), "{r}");
    if !has_render_node() {
        assert_eq!(f["decode"]["path"], "software", "{f}");
        assert_eq!(f["decode"]["transfer"], "system");
        let fallback = &f["decode"]["fallback"];
        assert_eq!(fallback["from"], "vaapi", "{f}");
        assert_eq!(fallback["code"], "hw_unavailable");
    }
    // look decodes it the same way.
    let looked = call(
        &tools,
        "look",
        json!({"file": file, "size": 256}),
        Duration::from_secs(50),
    )
    .unwrap();
    assert_eq!(looked["tiles"][0]["decode"], f["decode"], "{looked}");
}

#[test]
fn any_video_is_tried_not_only_equirects() {
    let dir = TempDir::new("plain");
    let file = clip(&dir.0, "plain", 2, false);
    let r = doctor(&dir.tools(), json!({"file": file}));
    assert!(r["file"]["decode"]["path"].is_string(), "{r}");
}

#[test]
fn speed_decodes_the_clip_and_says_how_fast() {
    let dir = TempDir::new("speed");
    let file = clip(&dir.0, "grid", 30, true);
    let r = doctor(&dir.tools(), json!({"file": file, "speed": true}));
    let speed = &r["file"]["speed"];
    // Thirty frames decode in far under 5 s: the first is the seek's, so
    // the test counts the 29 after it.
    assert_eq!(speed["ended"], true, "{speed}");
    assert_eq!(speed["frames"], 29, "{speed}");
    assert_eq!(speed["stream_fps"], 30.0);
    let fps = speed["fps"].as_f64().unwrap();
    assert!(fps > 0.0, "{speed}");
    let realtime = speed["realtime"].as_f64().unwrap();
    assert!((realtime - fps / 30.0).abs() < 0.01, "{speed}");
}

#[test]
fn speed_stops_within_the_calls_share_of_time() {
    let dir = TempDir::new("share");
    let file = clip(&dir.0, "long", 900, true);
    let started = Instant::now();
    let r = call(
        &dir.tools(),
        "doctor",
        json!({"file": file, "speed": true}),
        Duration::from_secs(2),
    )
    .unwrap();
    let speed = &r["file"]["speed"];
    // Half of what the call had left once the file was open.
    assert!(speed["seconds"].as_f64().unwrap() <= 1.05, "{speed}");
    assert!(started.elapsed() < Duration::from_secs(2), "{speed}");
}

#[test]
fn doctor_s_file_errors_name_their_argument() {
    let dir = TempDir::new("errors");
    std::fs::write(dir.0.join("bad.mp4"), b"not a video\n").unwrap();
    let tools = dir.tools();
    let e = call(
        &tools,
        "doctor",
        json!({"speed": true}),
        Duration::from_secs(50),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::BAD_ARGUMENT, Some("speed"))
    );
    let missing = dir.0.join("missing.mp4");
    let e = call(
        &tools,
        "doctor",
        json!({"file": missing}),
        Duration::from_secs(50),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::NOT_FOUND, Some("file"))
    );
    let bad = dir.0.join("bad.mp4");
    let e = call(
        &tools,
        "doctor",
        json!({"file": bad}),
        Duration::from_secs(50),
    )
    .unwrap_err();
    assert_eq!(e.argument.as_deref(), Some("file"), "{e:?}");
    // In doctor's own words, not FFmpeg's.
    assert!(!e.message.contains("Invalid data"), "{e:?}");
    let e = call(
        &tools,
        "doctor",
        json!({"file": dir.0.join("../x.mp4")}),
        Duration::from_secs(50),
    )
    .unwrap_err();
    assert_eq!(e.argument.as_deref(), Some("file"), "{e:?}");
}

#[test]
fn a_doctor_call_out_of_time_stops() {
    let dir = TempDir::new("time");
    let file = clip(&dir.0, "grid", 2, true);
    let cx = CallCx::standalone(Instant::now());
    let e = dir
        .tools()
        .call("doctor", json!({"file": file, "speed": true}), &cx)
        .err()
        .unwrap();
    assert_eq!(e.code, codes::TIMED_OUT, "{e:?}");
}
