//! `compare`, and projects in `look` and `check` (a2-plan sections 7 and
//! 9.4), on lavapipe: a file against itself; a render against its
//! project above the export's floors (D52) at every time, and one frame
//! off below them; the sheet within the inline budget; a partial answer
//! at a short deadline; look's times on a project, which are the
//! render's; and check on a project of each preset, a flat one failing
//! YouTube 360's metadata rule. `look` on a project against `frame
//! --project` is in the CLI's tests (`render.rs`), where both run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::limits::{COMPARE_COUNT, COMPARE_TIMES, INLINE_IMAGE_BYTES, SHEET_ROWS};
use spherecut_agent::rpc::{CallCx, Output, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_agent::{Roots, ToolError};
use spherecut_synth::media::{self, Spec};

/// The export's floors for a flat render through libx264 (D52, as
/// `crates/export/tests/parity.rs` holds them).
const FLAT_FLOORS: (f64, f64) = (49.0, 0.995);

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

impl TempDir {
    /// A directory holding `clip.mp4`, a 2-second equirect at 30 fps, and
    /// `trip.spherecut`, a project of it: flat 16:9 at 1080, its default.
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("compare-{name}-{}", std::process::id()));
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

    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), Some(&self.0)).unwrap())
    }

    /// The project's edits, applied.
    fn edit(&self, edits: Value) {
        ok(
            &self.tools(),
            "edit",
            json!({"file": self.project(), "edits": edits}),
        );
    }

    /// The project rendered to `output` over `range`, waited for: the
    /// finished job.
    fn rendered(&self, output: &str, range: Value) -> Value {
        let tools = self.tools();
        let r = ok(
            &tools,
            "render",
            json!({"project": self.project(), "output": self.path(output), "range": range}),
        );
        let end = Instant::now() + Duration::from_secs(300);
        loop {
            let j = ok(&tools, "job", json!({"job": r["job"], "wait": 30}));
            match j["state"].as_str() {
                Some("done") => return j,
                Some("failed" | "cancelled") => panic!("the render ended: {j}"),
                _ => assert!(Instant::now() < end, "timed out: {j}"),
            }
        }
    }
}

fn call_by(
    tools: &Spherecut,
    tool: &str,
    args: Value,
    within: Duration,
) -> Result<Output, ToolError> {
    let cx = CallCx::standalone(Instant::now() + within);
    tools.call(tool, args, &cx)
}

fn call(tools: &Spherecut, tool: &str, args: Value) -> Result<Value, ToolError> {
    call_by(tools, tool, args, Duration::from_secs(50)).map(|o| o.result)
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

/// A PSNR in a result: `"inf"` is infinite.
fn db(v: &Value) -> f64 {
    match v {
        Value::String(s) if s == "inf" => f64::INFINITY,
        v => v.as_f64().unwrap_or_else(|| panic!("a PSNR: {v}")),
    }
}

fn frames_of(times: &Value, key: &str) -> Vec<u64> {
    times
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t[key]["frame"].as_u64().unwrap())
        .collect()
}

#[test]
fn a_file_against_itself_is_identical_at_every_time() {
    let dir = TempDir::new("itself");
    let tools = dir.tools();
    let clip = dir.path("clip.mp4");
    let r = ok(
        &tools,
        "compare",
        json!({"file": clip, "against_file": clip}),
    );
    let times = &r["times"];
    assert_eq!(
        times.as_array().unwrap().len(),
        COMPARE_COUNT as usize,
        "{r}"
    );
    // The middles of eight equal parts of 2 s: (2i + 1) / 8 s, the frame
    // on screen then.
    assert_eq!(frames_of(times, "time"), [3, 11, 18, 26, 33, 41, 48, 56]);
    assert_eq!(frames_of(times, "time"), frames_of(times, "against"));
    for t in times.as_array().unwrap() {
        assert_eq!(
            (&t["psnr"], &t["ssim"]),
            (&json!("inf"), &json!(1.0)),
            "{t}"
        );
    }
    assert_eq!(r["mean"], json!({"psnr": "inf", "ssim": 1.0}), "{r}");
    assert_eq!(r["size"], json!([512, 256]));
    assert_eq!(r["against"]["file"], clip.to_string_lossy().as_ref());
    assert!(r.get("partial").is_none(), "{r}");

    // Times as given, in their order.
    let r = ok(
        &tools,
        "compare",
        json!({"file": clip, "against_file": clip, "times": ["40f", "0.5", [1, 10]]}),
    );
    assert_eq!(frames_of(&r["times"], "time"), [40, 15, 3]);

    // Refused before anything is opened.
    let refusals = [
        (json!({"file": clip}), "against_file"),
        (
            json!({"file": clip, "against_file": clip, "against_project": dir.project()}),
            "against_project",
        ),
        (
            json!({"file": clip, "against_file": clip, "from": "1"}),
            "from",
        ),
        (
            json!({"file": clip, "against_file": clip, "times": ["1"], "count": 3}),
            "count",
        ),
    ];
    for (args, at) in refusals {
        let e = refused(&tools, "compare", args);
        assert_eq!(
            (e.code, e.argument.as_deref()),
            (codes::BAD_ARGUMENT, Some(at)),
            "{e:?}"
        );
    }
    // Two sizes don't compare.
    let small = Spec {
        name: "small".into(),
        ..Spec::default()
    };
    media::write(&small, &dir.0).unwrap();
    let e = refused(
        &tools,
        "compare",
        json!({"file": clip, "against_file": dir.path("small.mp4")}),
    );
    assert_eq!(e.code, codes::BAD_ARGUMENT, "{e:?}");
    assert_eq!(e.argument.as_deref(), Some("against_file"));
    assert!(e.message.contains("256x128"), "{e:?}");
    // A time past the file's end.
    let e = refused(
        &tools,
        "compare",
        json!({"file": clip, "against_file": clip, "times": ["5"]}),
    );
    assert_eq!(e.argument.as_deref(), Some("times[0]"), "{e:?}");
}

/// PLAN's A2 check: a render compares frame for frame with its project,
/// above the export's floors at every time, and one frame off fails them
/// at every time. The sheet shows the worst within the inline budget, and
/// a short deadline gives the times measured.
#[test]
fn a_render_matches_its_project_frame_for_frame_and_one_frame_off_does_not() {
    let dir = TempDir::new("render");
    let tools = dir.tools();
    let job = dir.rendered("out.mp4", json!({"from": "0.5", "to": "1.5"}));
    assert_eq!(job["report"]["frames"], 30, "{job}");
    let out = dir.path("out.mp4");

    // `from` is where the render started in the media.
    let r = ok(
        &tools,
        "compare",
        json!({"file": out, "against_project": dir.project(), "from": "0.5"}),
    );
    assert_eq!(r["size"], json!([1920, 1080]), "{r}");
    assert_eq!(r["against"]["from"]["seconds"], 0.5, "{r}");
    let times = r["times"].as_array().unwrap();
    assert_eq!(times.len(), COMPARE_COUNT as usize);
    assert_eq!(
        frames_of(&r["times"], "time"),
        frames_of(&r["times"], "against")
    );
    let (floor_psnr, floor_ssim) = FLAT_FLOORS;
    for t in times {
        let (psnr, ssim) = (db(&t["psnr"]), t["ssim"].as_f64().unwrap());
        eprintln!(
            "render at {}: {psnr:.2} dB, SSIM {ssim:.5}",
            t["time"]["frame"]
        );
        assert!(psnr >= floor_psnr && ssim >= floor_ssim, "{t}");
    }
    assert!(db(&r["worst"]["psnr"]["psnr"]) >= floor_psnr, "{r}");

    // A frame on: below the floors at every time.
    let off = ok(
        &tools,
        "compare",
        json!({"file": out, "against_project": dir.project(), "from": "16f"}),
    );
    for t in off["times"].as_array().unwrap() {
        let psnr = db(&t["psnr"]);
        eprintln!("a frame off at {}: {psnr:.2} dB", t["time"]["frame"]);
        assert!(psnr < floor_psnr, "{t}");
    }
    // A `from` outside the trim.
    let e = refused(
        &tools,
        "compare",
        json!({"file": out, "against_project": dir.project(), "from": "3"}),
    );
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::OUT_OF_RANGE, Some("from")),
        "{e:?}"
    );

    // The sheet: the worst four, a row each of the file, the render and
    // their difference.
    let o = call_by(
        &tools,
        "compare",
        json!({"file": out, "against_project": dir.project(), "from": "0.5", "sheet": true}),
        Duration::from_secs(50),
    )
    .unwrap();
    assert_eq!(o.images.len(), 1, "{}", o.result);
    let image = &o.images[0];
    assert_eq!(image.mime, "image/jpeg");
    assert!(image.data.len().div_ceil(3) * 4 <= INLINE_IMAGE_BYTES);
    assert_eq!(o.result["image"]["bytes"], image.data.len());
    assert_eq!(image.data[..2], [0xff, 0xd8], "a JPEG");
    let size = |k: &str| o.result["image"][k].as_u64().unwrap();
    let (w, h) = (size("width"), size("height"));
    assert!(h > w / 2, "{w}x{h}: {SHEET_ROWS} rows of three");

    // Out of time: the times measured, and how to go on.
    let o = call_by(
        &tools,
        "compare",
        json!({"file": out, "against_project": dir.project(), "from": "0.5", "count": COMPARE_TIMES}),
        Duration::from_secs(4),
    )
    .unwrap();
    let r = o.result;
    let measured = r["times"].as_array().unwrap().len();
    eprintln!("out of time: {measured} of {COMPARE_TIMES} measured");
    assert_eq!(r["partial"], true, "{r}");
    assert_eq!(r["measured"], measured);
    assert!((1..COMPARE_TIMES).contains(&measured), "{r}");
    assert!(r["next"].is_string(), "{r}");
}

#[test]
fn look_on_a_project_shows_the_renders_frames_at_the_renders_times() {
    let dir = TempDir::new("look");
    let tools = dir.tools();
    let look = |args: Value| {
        let mut a = json!({"project": dir.project()});
        a.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        a
    };
    // A sheet spreads 12 over the render, first frame to last.
    let r = ok(&tools, "look", look(json!({"mode": "sheet", "size": 768})));
    let tiles = &r["tiles"];
    let shown = frames_of(tiles, "time");
    assert_eq!(shown.len(), 12, "{r}");
    assert_eq!((shown[0], shown[11]), (0, 59));
    assert!(shown.windows(2).all(|w| w[0] < w[1]), "{shown:?}");
    assert_eq!(frames_of(tiles, "source"), shown);
    assert_eq!(r["frames"], 60);
    assert_eq!(r["project"], dir.project().to_string_lossy().as_ref());

    // Trimmed, the render's 0 is the trim's start.
    dir.edit(json!([{"op": "set_trim", "clip": 4, "start": "0.5"}]));
    let r = ok(&tools, "look", look(json!({"times": ["0", "10f"]})));
    assert_eq!(frames_of(&r["tiles"], "time"), [0, 10], "{r}");
    assert_eq!(frames_of(&r["tiles"], "source"), [15, 25], "{r}");
    assert_eq!(r["frames"], 45);
    let e = refused(&tools, "look", look(json!({"times": ["1.5"]})));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::OUT_OF_RANGE, Some("times[0]")),
        "{e:?}"
    );
    assert!(e.valid.as_deref().unwrap_or("").contains("render"), "{e:?}");

    // The source's views and modes are its media file's.
    let refusals = [
        (look(json!({"mode": "sphere"})), "mode"),
        (look(json!({"mode": "raw"})), "mode"),
        (look(json!({"view": {"yaw": 90}})), "view"),
        (look(json!({"views": [{"yaw": 90}]})), "views"),
        (look(json!({"seek": "keyframe"})), "seek"),
        (look(json!({"assume_equirect": true})), "assume_equirect"),
    ];
    for (args, at) in refusals {
        let e = refused(&tools, "look", args);
        assert_eq!(
            (e.code, e.argument.as_deref()),
            (codes::BAD_ARGUMENT, Some(at)),
            "{e:?}"
        );
    }
    for (tool, args) in [
        (
            "look",
            json!({"file": dir.path("clip.mp4"), "project": dir.project()}),
        ),
        ("look", json!({})),
        (
            "check",
            json!({"file": dir.path("clip.mp4"), "project": dir.project()}),
        ),
        ("check", json!({})),
    ] {
        let e = refused(&tools, tool, args);
        assert_eq!(e.code, codes::BAD_ARGUMENT, "{tool}: {e:?}");
    }
}

/// Each preset with the target its output fits passes; 1:1 and 4:5 have
/// no target and pass the rules alone. A flat project for YouTube 360
/// fails for its missing spherical metadata.
#[test]
fn check_on_a_project_says_what_a_render_would_be() {
    let dir = TempDir::new("check");
    let tools = dir.tools();
    let check = |target: Option<&str>| {
        let mut a = json!({"project": dir.project()});
        if let Some(t) = target {
            a["target"] = json!(t);
        }
        ok(&tools, "check", a)
    };
    let mut outputs: Vec<(Value, Option<&str>)> = Vec::new();
    for (aspect, target) in [
        ("16:9", Some("flat_16_9")),
        ("9:16", Some("vertical")),
        ("1:1", None),
        ("4:5", None),
    ] {
        for short_side in [1080, 1440, 2160] {
            outputs.push((
                json!({"kind": "flat", "aspect": aspect, "short_side": short_side}),
                target,
            ));
        }
    }
    for width in [3840, 5760, 7680] {
        outputs.push((
            json!({"kind": "equirect", "width": width, "orientation": {"ypr_deg": [0, 0, 0]}}),
            Some("youtube_360"),
        ));
    }
    for (output, target) in outputs {
        dir.edit(json!([{"op": "set_output", "sequence": 2, "output": output}]));
        let r = check(target);
        assert_eq!(r["pass"], true, "{output} for {target:?}: {r}");
        assert_eq!(r["project"], dir.project().to_string_lossy().as_ref());
        assert!(r["checked"].as_str().unwrap().contains("render"), "{r}");
        let failed: Vec<&Value> = r["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|rule| rule["ok"] != true)
            .collect();
        assert!(failed.is_empty(), "{output}: {failed:?}");
    }

    dir.edit(json!([{"op": "set_output", "sequence": 2,
                     "output": {"kind": "flat", "aspect": "16:9", "short_side": 1080}}]));
    let r = check(Some("youtube_360"));
    assert_eq!(r["pass"], false, "{r}");
    let rule = r["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rule| rule["id"] == "spherical_metadata")
        .unwrap_or_else(|| panic!("{r}"));
    assert_eq!(rule["ok"], false, "{rule}");
    assert!(rule["fix"].is_string(), "{rule}");
}
