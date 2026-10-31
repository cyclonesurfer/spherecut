//! The skill's four recipes (a1-plan section 11.3, a2-plan 11), replayed
//! as the calls an agent following `SKILL.md` makes, on synthetic media
//! with a planted answer, each within its budget. A call costs what its result
//! puts before a model: the text of `tools/call`'s content, a character
//! for every 4, plus ceil(w / 28) × ceil(h / 28) for each image. Where
//! the test reads the pixels an agent would look at (recipe 1), its looks
//! ask for PNG, since the image crate here decodes no JPEG; the cost
//! counts pixels, the same in either format. Recipe 2 runs on the
//! defaults: JPEG, within the default image budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::limits::{IMAGE_DEFAULT_EDGE, INLINE_IMAGE_BYTES, MAX_IMAGE_BUDGET};
use spherecut_agent::rpc::{self, CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_agent::{Roots, SKILL};
use spherecut_media::inject::{self, EntryEdit, TrackKind};
use spherecut_media::spherical::{StereoMode, write};
use spherecut_synth::media::{self, Codec, Spec};
use spherecut_synth::moving_marker_lon;

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
            .join(format!("recipes-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

/// An agent's session: the tools, and what their results cost so far.
struct Agent {
    tools: Spherecut,
    cost: usize,
    calls: Vec<String>,
}

impl Agent {
    /// A session with images as large as PNG needs.
    fn new(root: &Path) -> Agent {
        Agent::with_budget(root, MAX_IMAGE_BUDGET)
    }

    /// A session with `spherecut mcp`'s image budget, `budget` bytes.
    fn with_budget(root: &Path, budget: usize) -> Agent {
        let roots = Roots::new(&[root.to_path_buf()], None).unwrap();
        Agent {
            tools: Spherecut::new(roots).image_budget(budget),
            cost: 0,
            calls: Vec::new(),
        }
    }

    /// A session that writes beneath `root` too, as `--allow-write
    /// --write-root` makes one.
    fn writing(root: &Path) -> Agent {
        let roots = Roots::new(&[root.to_path_buf()], Some(root)).unwrap();
        Agent {
            tools: Spherecut::new(roots).image_budget(MAX_IMAGE_BUDGET),
            cost: 0,
            calls: Vec::new(),
        }
    }

    /// `job` with `wait` until the render that answered `r` ends: the
    /// finished job.
    fn follow(&mut self, r: &Value) -> Value {
        let end = Instant::now() + Duration::from_secs(300);
        loop {
            let (j, _) = self.call("job", json!({"job": r["job"], "wait": 30}));
            match j["state"].as_str() {
                Some("done") => return j,
                Some("failed" | "cancelled") => panic!("the render ended: {j}"),
                _ => assert!(Instant::now() < end, "timed out: {j}"),
            }
        }
    }

    /// Call `tool` as `tools/call` does, and count what the model is
    /// shown. A PNG comes back decoded; a JPEG is counted by the size the
    /// result gives.
    fn call(&mut self, tool: &str, args: Value) -> (Value, Vec<image::RgbImage>) {
        let cx = CallCx::standalone(Instant::now() + Duration::from_secs(50));
        let out = self
            .tools
            .call(tool, args.clone(), &cx)
            .unwrap_or_else(|e| panic!("{tool} {args}: {}", e.to_json()));
        let shown = rpc::tool_result(Ok(out.clone()));
        let mut cost = 0;
        for c in shown["content"].as_array().unwrap() {
            if let Some(text) = c["text"].as_str() {
                cost += text.chars().count().div_ceil(4);
            }
        }
        let tokens = |w: u32, h: u32| w.div_ceil(28) as usize * h.div_ceil(28) as usize;
        let mut images = Vec::new();
        for i in &out.images {
            if i.mime == "image/png" {
                let png = image::load_from_memory(&i.data).unwrap().to_rgb8();
                cost += tokens(png.width(), png.height());
                images.push(png);
            } else {
                assert_eq!(i.mime, "image/jpeg");
                assert_eq!(out.images.len(), 1, "one image a look");
                let size = &out.result["image"];
                let side = |k: &str| u32::try_from(size[k].as_u64().unwrap()).unwrap();
                cost += tokens(side("width"), side("height"));
            }
        }
        self.cost += cost;
        self.calls.push(format!("{tool} {args}: {cost}"));
        (out.result, images)
    }

    fn within(&self, budget: usize) {
        assert!(
            self.cost <= budget,
            "{} over {budget}:\n{}",
            self.cost,
            self.calls.join("\n")
        );
        eprintln!("{} of {budget}:\n{}", self.cost, self.calls.join("\n"));
    }
}

/// Tile `t`'s picture: `[x, y, w, h]`.
fn rect(t: &Value) -> [u32; 4] {
    let r: Vec<u32> = t["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    [r[0], r[1], r[2], r[3]]
}

/// Where the grid's moving marker (magenta) is in tile `t`, as a
/// fraction of its width from the left, if it is there: the most magenta
/// pixel within a few rows of the middle, where the equator is.
fn marker_x(img: &image::RgbImage, t: &Value) -> Option<f64> {
    let [x, y, w, h] = rect(t);
    let band = (h / 16).max(2);
    let mut best = (0.0, 0);
    for yy in y + h / 2 - band..=y + h / 2 + band {
        for xx in x..x + w {
            let p = img.get_pixel(xx, yy).0.map(|c| f64::from(c) / 255.0);
            let magenta = p[0].min(p[2]) - p[1];
            if magenta > best.0 {
                best = (magenta, xx - x);
            }
        }
    }
    (best.0 > 0.5).then(|| (f64::from(best.1) + 0.5) / f64::from(w))
}

/// The marker's planted yaw at tile `t`'s frame.
fn truth(t: &Value) -> f64 {
    moving_marker_lon(t["time"]["frame"].as_u64().unwrap() as u32)
}

/// `a - b` in degrees, wrapped to [-180, 180).
fn off(a: f64, b: f64) -> f64 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

fn tiles(r: &Value) -> &Vec<Value> {
    r["tiles"].as_array().unwrap()
}

/// Recipe 1: the grid's marker runs round the equator, 3° a frame; the
/// recipe finds its yaw at three times, within 5°, for 12,000.
#[test]
fn recipe_finds_a_moving_subject() {
    assert!(SKILL.contains("## Find where a moving subject is over time"));
    let d = TempDir::new("subject");
    let clip = media::write(
        &Spec {
            name: "ride".into(),
            width: 1024,
            height: 512,
            frames: 90,
            codec: Codec::H264Lossless,
            spherical: true,
            ..Spec::default()
        },
        &d.0,
    )
    .unwrap();
    let file = clip.file_name().unwrap().to_str().unwrap();
    let mut agent = Agent::new(&d.0);

    // 1. probe.
    let (r, _) = agent.call("probe", json!({"files": [file]}));
    assert_eq!(r["files"][0]["projection"], "equirect", "{r}");

    // 2. A sheet over the clip: where the marker is in each tile.
    let (r, images) = agent.call(
        "look",
        json!({"file": file, "mode": "sheet", "count": 12, "format": "png"}),
    );
    let sightings: Vec<(&Value, f64)> = tiles(&r)
        .iter()
        .filter_map(|t| marker_x(&images[0], t).map(|x| (t, x * 360.0 - 180.0)))
        .collect();
    // The axis discs hide it as it passes them, a tile or two of 12.
    assert!(sightings.len() >= 7, "{} sightings in {r}", sightings.len());
    for (t, yaw) in &sightings {
        // A sheet's tiles are small: 10° is a few pixels.
        assert!(off(*yaw, truth(t)).abs() < 10.0, "{yaw} at {t}");
    }

    // 3. The sphere at three of those times, on its labelled grid.
    let picked: Vec<&Value> = [0, sightings.len() / 2, sightings.len() - 1]
        .iter()
        .map(|&i| &sightings[i].0["time"]["exact"])
        .collect();
    let (r, images) = agent.call(
        "look",
        json!({"file": file, "mode": "sphere", "times": picked, "format": "png"}),
    );
    let seen: Vec<(Value, f64)> = tiles(&r)
        .iter()
        .map(|t| {
            let x = marker_x(&images[0], t).unwrap_or_else(|| panic!("no marker in {t}"));
            (t.clone(), x * 360.0 - 180.0)
        })
        .collect();

    // 4. A view centred on each, to confirm and refine.
    for (t, yaw) in seen {
        let (r, images) = agent.call(
            "look",
            json!({
                "file": file,
                "times": [t["time"]["exact"]],
                "view": {"yaw": yaw, "pitch": 0, "fov": 60},
                "size": 384,
                "format": "png",
            }),
        );
        let tile = &tiles(&r)[0];
        let x = marker_x(&images[0], tile).unwrap_or_else(|| panic!("no marker in {tile}"));
        let refined = yaw
            + ((2.0 * x - 1.0) * 30f64.to_radians().tan())
                .atan()
                .to_degrees();
        // 5. The report: time and yaw, against the planted yaw.
        let error = off(refined, truth(tile));
        assert!(error.abs() < 5.0, "{refined} at {tile}: {error}° off");
    }
    agent.within(12_000);
}

/// Recipe 2: a 2:1 picture tagged top-bottom stereo fails
/// `projection_fits_picture`, which the full probe and a sphere look
/// show, for 4,000.
#[test]
fn recipe_validates_a_360_export() {
    assert!(SKILL.contains("## Validate a 360 export"));
    let d = TempDir::new("export");
    let flat = media::write(
        &Spec {
            name: "plain".into(),
            width: 512,
            height: 256,
            frames: 10,
            ..Spec::default()
        },
        &d.0,
    )
    .unwrap();
    let file = "export.mp4";
    inject::inject(
        &flat,
        &d.0.join(file),
        &[EntryEdit {
            track: TrackKind::Video,
            boxes: [
                write::st3d(StereoMode::TopBottom),
                write::sv3d_equirect("t"),
            ]
            .concat(),
        }],
    )
    .unwrap();
    let mut agent = Agent::with_budget(&d.0, INLINE_IMAGE_BYTES);

    // 1. check.
    let (r, _) = agent.call("check", json!({"file": file, "target": "youtube_360"}));
    assert_eq!(r["pass"], false, "{r}");
    let failed: Vec<&str> = r["rules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["ok"] == false)
        .map(|x| x["id"].as_str().unwrap())
        .collect();
    assert_eq!(failed, ["projection_fits_picture"], "{r}");

    // 2. The full probe says what the file claims.
    let (r, _) = agent.call("probe", json!({"files": [file], "detail": "full"}));
    assert!(r.to_string().contains("\"stereo\":\"top_bottom\""), "{r}");

    // 3. A sphere look on the defaults: the picture as the tags have it,
    // one eye, as a JPEG 768 pixels wide.
    let (r, _) = agent.call("look", json!({"file": file, "mode": "sphere"}));
    assert_eq!(r["image"]["format"], "jpeg", "{r}");
    assert_eq!(r["image"]["width"], IMAGE_DEFAULT_EDGE, "{r}");
    let warnings = r["warnings"].to_string();
    assert!(warnings.contains("stereo"), "{r}");
    agent.within(4_000);
}

/// Recipe 3: `jitter-dups` stutters for its jitter at 2 s and its gaps at
/// 5 and 7 s, which timing finds and motion around the worst bucket
/// confirms, for 8,000.
#[test]
fn recipe_finds_why_a_file_stutters() {
    assert!(SKILL.contains("## Find why a file stutters"));
    let d = TempDir::new("stutter");
    let clip = media::write_jitter_dups(&d.0).unwrap();
    let file = clip.file_name().unwrap().to_str().unwrap();
    let mut agent = Agent::new(&d.0);

    // 1. timing.
    let (r, _) = agent.call("analyze", json!({"file": file, "kind": "timing"}));
    let s = &r["summary"];
    assert_eq!(s["vfr"], true, "{s}");
    assert_eq!(s["gaps"]["count"], 2, "{s}");
    assert_eq!(s["gaps"]["missing"], 6, "{s}");
    let at: Vec<f64> = s["gaps"]["at"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["seconds"].as_f64().unwrap())
        .collect();
    assert!(
        (at[0] - 5.0).abs() < 0.1 && (at[1] - 7.0).abs() < 0.1,
        "{at:?}"
    );
    let worst = r["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .max_by_key(|b| b["missing"].as_u64().unwrap())
        .unwrap();
    let from = worst["from"].as_f64().unwrap();

    // 2. doctor: the path and its speed here.
    let (r, _) = agent.call("doctor", json!({"file": file, "speed": true}));
    assert!(r["file"]["decode"]["path"].is_string(), "{r}");
    assert!(
        r["file"]["speed"]["realtime"].as_f64().unwrap() > 0.0,
        "{r}"
    );

    // 3. motion around the worst bucket.
    let (r, _) = agent.call(
        "analyze",
        json!({
            "file": file,
            "kind": "motion",
            "from": format!("{}", (from - 0.5).max(0.0)),
            "to": format!("{}", from + 1.0),
            "buckets": 6,
        }),
    );
    assert_eq!(r["complete"], true, "{r}");
    // Motion measured every frame there is: the window's 45 slots less
    // the gap's.
    assert_eq!(r["summary"]["frames"], 45 - media::JITTER_GAP_SLOTS, "{r}");
    agent.within(8_000);
}

/// A PSNR in a result: `"inf"` is infinite.
fn db(v: &Value) -> f64 {
    match v {
        Value::String(s) if s == "inf" => f64::INFINITY,
        v => v.as_f64().unwrap_or_else(|| panic!("a PSNR: {v}")),
    }
}

/// Recipe 4: the grid's marker runs round the equator, 3° a frame. The
/// recipe keys a view path on it from a sheet and the sphere, and the
/// project's frames keep it within 5° of the middle; a draft of half a
/// second is above PLAN's least against the project (40 dB, 0.98) at
/// every time, and the whole render passes its check, for 12,000.
#[test]
fn recipe_reframes_a_clip_into_a_flat_video() {
    assert!(SKILL.contains("## Reframe a clip into a flat video"));
    let d = TempDir::new("reframe");
    let clip = media::write(
        &Spec {
            name: "ride".into(),
            width: 1024,
            height: 512,
            frames: 60,
            codec: Codec::H264Lossless,
            spherical: true,
            ..Spec::default()
        },
        &d.0,
    )
    .unwrap();
    let file = clip.file_name().unwrap().to_str().unwrap();
    let mut agent = Agent::writing(&d.0);

    // 1. probe, a sheet, and the sphere at the times the marker shows.
    let (r, _) = agent.call("probe", json!({"files": [file]}));
    assert_eq!(r["files"][0]["projection"], "equirect", "{r}");
    let (r, images) = agent.call(
        "look",
        json!({"file": file, "mode": "sheet", "count": 12, "format": "png"}),
    );
    let moments: Vec<&Value> = tiles(&r)
        .iter()
        .filter(|t| marker_x(&images[0], t).is_some())
        .map(|t| &t["time"]["exact"])
        .collect();
    assert!(moments.len() >= 7, "{} sightings in {r}", moments.len());
    let (r, images) = agent.call(
        "look",
        json!({"file": file, "mode": "sphere", "times": moments, "format": "png"}),
    );
    let seen: Vec<(Value, f64)> = tiles(&r)
        .iter()
        .filter_map(|t| {
            let x = marker_x(&images[0], t)?;
            Some((t["time"]["exact"].clone(), x * 360.0 - 180.0))
        })
        .collect();
    assert!(seen.len() >= 7, "{} sightings in {r}", seen.len());

    // 2. A project of the clip: flat 16:9 at 1080.
    let (s, _) = agent.call(
        "project",
        json!({"file": "ride.spherecut", "create": {"media": file}}),
    );
    let project = s["file"].clone();
    let id = s["clips"][0]["id"].clone();

    // 3. The ops, once.
    let (r, _) = agent.call("project", json!({"file": project, "part": "edits"}));
    assert!(r.to_string().contains("set_view_path"), "{r}");

    // 4. One edit: the view through each sighting, and the trim from the
    // first to the last.
    let points: Vec<Value> = seen
        .iter()
        .map(|(at, yaw)| json!({"at": at, "orientation": {"ypr_deg": [yaw, 0, 0]}, "fov": 60}))
        .collect();
    let (r, _) = agent.call(
        "edit",
        json!({"file": project, "edits": [
            {"op": "set_view_path", "clip": id, "points": points, "replace": true},
            {"op": "set_trim", "clip": id, "start": seen[0].0, "end": seen[seen.len() - 1].0},
        ]}),
    );
    assert_eq!(r["undo"]["agent_may"], true, "{r}");

    // 5. The project's frames: the marker in the middle, wherever it is in
    // the clip.
    let (r, images) = agent.call(
        "look",
        json!({"project": project, "mode": "sheet", "format": "png"}),
    );
    let mut found = 0;
    let mut worst_off = 0f64;
    for t in tiles(&r) {
        let Some(x) = marker_x(&images[0], t) else {
            continue;
        };
        found += 1;
        let off_centre = ((2.0 * x - 1.0) * 30f64.to_radians().tan())
            .atan()
            .to_degrees();
        assert!(off_centre.abs() < 5.0, "{off_centre}° off in {t}");
        worst_off = worst_off.max(off_centre.abs());
    }
    assert!(found >= 8, "{found} of 12 tiles show the marker: {r}");

    // 6. What a render would make.
    let (r, _) = agent.call("check", json!({"project": project, "target": "flat_16_9"}));
    assert_eq!(r["pass"], true, "{r}");

    // 7. A draft of half a second, against the project.
    let (r, _) = agent.call(
        "render",
        json!({"project": project, "output": "draft.mp4", "draft": true,
               "range": {"from": "0.5", "to": "1"}}),
    );
    let job = agent.follow(&r);
    assert_eq!(job["check"]["pass"], true, "{job}");
    let (c, _) = agent.call(
        "compare",
        json!({"file": r["output"], "against_project": project, "from": "0.5"}),
    );
    let worst = &c["worst"];
    assert!(
        db(&worst["psnr"]["psnr"]) > 40.0 && worst["ssim"]["ssim"].as_f64().unwrap() > 0.98,
        "{c}"
    );
    eprintln!(
        "{found} of 12 tiles show the marker, at most {worst_off:.1}° off centre; the draft's \
         worst {worst}"
    );

    // 8. The whole project.
    let (r, _) = agent.call(
        "render",
        json!({"project": project, "output": "ride-flat.mp4"}),
    );
    let job = agent.follow(&r);
    assert_eq!(job["check"]["pass"], true, "{job}");
    assert_eq!(job["report"]["size"], json!([1920, 1080]), "{job}");
    agent.within(12_000);
}
