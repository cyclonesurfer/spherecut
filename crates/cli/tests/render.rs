//! `spherecut render` end to end (M2 plan section 9, DECISIONS D51 and
//! D27): its lines under `--json` (`start`, `progress` at most once a
//! second, and the result or the error last), the exit statuses 0 to 4,
//! and every failure leaving nothing at `-o` and no temporary file: a
//! file in the way, a project that doesn't load or whose clip is
//! offline, a range with no frames, VA-API without a device, Ctrl+C, and
//! a disk that stops taking the file part way.
//!
//! **`frame --project`** (amendment 2 item 5) renders the sequence's
//! frame as the export does, so the parity sample runs through the CLI:
//! its PNG's luma against the exported file's decoded frame, and the
//! frame before it as the control.
//!
//! **The full disk** is a tmpfs of the test's own, in a user and mount
//! namespace, filled by another writer once the render has started (past
//! its free-space check), where the box lets a user mount one; the
//! export's own tests fill a fake disk everywhere.
//!
//! Every run renders on lavapipe (`--adapter cpu`) and decodes in
//! software (`--decoder software`), with libx264.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_cli::contract::{
    self, FallbackInfo, Finishing, RenderAudio, RenderCheck, RenderDecode, RenderEvent,
    RenderProgress, RenderResult, RenderStages, RenderStart, Size, TimeRange,
};
use spherecut_core::FrameRate;
use spherecut_core::StopToken;
use spherecut_core::project::file::{self, head_hash};
use spherecut_core::project::{
    AnyKeys, Aspect, AudioChoice, Codec, Ease, Edit, EncoderChoice, ExportSettings, Fingerprint,
    InitialView, Key, KeyId, Media, MediaId, MediaVideo, Orient, Orientation, Output, Project,
    Quality,
};
use spherecut_core::time::{Rational, Time};
use spherecut_core::video::VideoPlanes;
use spherecut_media::backend::{
    FfmpegBackend, MediaBackend, NextFrame, SeekMode, SeekOutcome, VideoRequest,
};
use spherecut_media::hw::DEFAULT_RENDER_NODE;
use spherecut_render::compare::{Plane, psnr_plane, ssim_plane};
use spherecut_render::{Image, Pixels};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::Projection;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self as synth, Spec};

// ---------------------------------------------------------------------
// Projects and runs

/// A test's own directory, removed with what it holds when dropped, so
/// also when the test fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("render-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// `frames` of the grid at 30 fps, 1024×512 equirect, written to `dir`.
fn clip(dir: &Path, name: &str, pattern: Pattern, frames: u32) -> PathBuf {
    synth::write(
        &Spec {
            name: name.into(),
            pattern,
            width: 1024,
            height: 512,
            frames,
            spherical: true,
            ..Spec::default()
        },
        dir,
    )
    .unwrap()
}

/// A project of `clip`, saved as `dir/p.spherecut`: the whole clip to a
/// 1920×1080 frame, the camera ahead, H.264 in software at the medium
/// preset, no audio.
fn project(dir: &Path, clip: &Path) -> PathBuf {
    let dec = FfmpegBackend::new()
        .open_video(clip, &VideoRequest::default())
        .unwrap();
    let info = dec.info();
    let media = Media {
        id: MediaId::NEW,
        path: clip.to_path_buf(),
        fingerprint: Fingerprint {
            bytes: std::fs::metadata(clip).unwrap().len(),
            head: head_hash(clip).unwrap(),
        },
        video: MediaVideo {
            stream: info.index as u32,
            width: info.size.0,
            height: info.size.1,
            rate: info.frame_rate().unwrap(),
            duration: info.duration.unwrap(),
        },
    };
    let view = InitialView {
        orientation: Orientation::IDENTITY,
        fov_deg: 90.0,
        projection: Projection::Rectilinear,
    };
    let mut p = Project::untitled(media, view).unwrap();
    let sequence = p.the_sequence().id;
    p.apply(&[
        Edit::SetOutput {
            sequence,
            output: Output::Flat {
                aspect: Aspect::Wide,
                short_side: 1080,
            },
        },
        Edit::SetExport {
            sequence,
            export: ExportSettings {
                codec: Codec::H264,
                quality: Quality::Medium,
                encoder: EncoderChoice::Software,
                audio: AudioChoice::None,
                motion_blur: false,
            },
        },
    ])
    .unwrap();
    let path = dir.join("p.spherecut");
    file::save(&p, &path).unwrap();
    path
}

/// The command line of `spherecut render PROJECT -o OUT ARGS`, on
/// lavapipe and decoding in software unless `args` say otherwise.
fn render_args(project: &Path, out: &Path, args: &[&str]) -> Vec<OsString> {
    let mut all: Vec<OsString> = vec!["render".into(), project.into(), "-o".into(), out.into()];
    if !args.contains(&"--adapter") {
        all.extend(["--adapter".into(), "cpu".into()]);
    }
    if !args.contains(&"--decoder") {
        all.extend(["--decoder".into(), "software".into()]);
    }
    all.extend(args.iter().map(OsString::from));
    all
}

fn spherecut() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    cmd.env_remove("SPHERECUT_LOG")
        .env_remove("SPHERECUT_ADAPTER")
        .env_remove("SPHERECUT_DEBUG_PANIC");
    cmd
}

/// What a `render --json` run gave.
#[derive(Debug)]
struct Ran {
    status: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Ran {
    /// The run's document: its last line.
    fn last(&self) -> &Value {
        self.lines
            .last()
            .unwrap_or_else(|| panic!("{:?} and no lines: {}", self.status, self.stderr))
    }

    fn ok(&self) -> &Value {
        assert_eq!(self.status, Some(0), "{:?} / {}", self.lines, self.stderr);
        assert_eq!(self.last()["ok"], true);
        &self.last()["result"]
    }

    fn code(&self) -> &str {
        self.last()["error"]["code"].as_str().unwrap_or("")
    }

    /// A failure's one line, of exit status `status` and code `code`.
    fn failed(&self, status: i32, code: &str) -> &Value {
        assert_eq!(
            (self.status, self.code()),
            (Some(status), code),
            "{:?} / {}",
            self.lines,
            self.stderr
        );
        let v = self.last();
        assert_eq!(v["schema"], contract::RENDER);
        assert_eq!(v["ok"], false);
        assert!(v.get("result").is_none(), "{v}");
        &v["error"]
    }
}

/// Every line on stdout, each a whole JSON envelope.
fn lines(stdout: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(stdout).unwrap();
    assert!(text.is_empty() || text.ends_with('\n'), "{text:?}");
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

fn run(project: &Path, out: &Path, args: &[&str]) -> Ran {
    let out = spherecut()
        .args(render_args(project, out, args))
        .output()
        .unwrap();
    Ran {
        status: out.status.code(),
        lines: lines(&out.stdout),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The temporary files in `dir`: there must never be any after a run.
fn temp_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| spherecut_core::fs::is_temp_name(n))
        .collect()
}

/// The event each line names.
fn events(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .map(|l| l["result"]["event"].as_str().unwrap_or("error"))
        .collect()
}

// ---------------------------------------------------------------------
// The lines

#[test]
fn a_render_is_a_start_line_progress_lines_and_its_result() {
    let dir = TempDir::new("lines");
    let src = clip(&dir, "grid", Pattern::Grid, 30);
    let p = project(&dir, &src);
    let out = dir.join("out.mp4");
    let ran = run(&p, &out, &["--json", "--draft", "--range", "..12f"]);
    let result = ran.ok().clone();

    for l in &ran.lines {
        assert_eq!(l["schema"], contract::RENDER, "{l}");
        assert_eq!(l["ok"], true, "{l}");
        assert_eq!(l["warnings"], json!([]), "{l}");
    }
    let events = events(&ran.lines);
    assert_eq!(events.first(), Some(&"start"), "{events:?}");
    assert_eq!(events.last(), Some(&"result"), "{events:?}");
    assert!(
        events[1..events.len() - 1].iter().all(|e| *e == "progress"),
        "{events:?}"
    );

    // What it will do: half of 1920×1080 each side, rounded up to a
    // multiple of 8, and 12 frames of the sequence's 30 fps.
    let start = &ran.lines[0]["result"];
    assert_eq!(start["output"], out.to_str().unwrap());
    assert_eq!(start["project"], p.to_str().unwrap());
    assert_eq!(start["preset"], "flat-16x9-1080");
    assert_eq!(start["size"], json!({"width": 960, "height": 544}));
    assert_eq!(start["draft"], true);
    assert_eq!(start["frames"], 12);
    assert_eq!(
        (&start["codec"], &start["quality"], &start["encoder"]),
        (&json!("h264"), &json!("medium"), &json!("libx264"))
    );
    assert_eq!(
        (&start["audio"], &start["motion_blur"]),
        (&json!("none"), &json!(false))
    );
    assert!(start.get("fallback").is_none(), "{start}");
    assert_eq!(
        start["duration"],
        serde_json::to_value(FrameRate::FPS_30.frame_to_time(12).unwrap()).unwrap()
    );
    assert_eq!(
        start["range"]["start"],
        serde_json::to_value(Time::ZERO).unwrap()
    );
    assert_eq!(start["range"]["end"], start["duration"]);

    let mut done = 0;
    for l in &ran.lines[1..ran.lines.len() - 1] {
        let p = &l["result"];
        let now = p["frames_done"].as_u64().unwrap();
        assert!(now >= done && now <= 12, "{l}");
        assert_eq!(p["frames_total"], 12);
        done = now;
    }

    // What it did.
    assert_eq!(result["output"], out.to_str().unwrap());
    assert_eq!(result["frames"], 12);
    assert_eq!(result["size"], json!({"width": 960, "height": 544}));
    assert_eq!(
        (&result["draft"], &result["ten_bit"]),
        (&json!(true), &json!(false))
    );
    assert_eq!(result["encoder"], "libx264");
    assert_eq!(result["decode"], json!({"path": "software"}));
    assert_eq!(result["audio"], Value::Null);
    assert_eq!(result["blur_capped"], 0);
    for stage in ["decode", "render", "readback", "encode", "finish"] {
        let busy = result["stages"][stage].as_f64().unwrap();
        assert!(busy > 0.0, "{stage}: {}", result["stages"]);
    }
    assert_eq!(result["bytes"], std::fs::metadata(&out).unwrap().len());
    let checks: Vec<&str> = result["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        checks,
        [
            "probe",
            "layout",
            "codec",
            "size",
            "frames",
            "duration",
            "rate",
            "spherical",
            "audio"
        ]
    );

    // And the file is what it says.
    let dec = FfmpegBackend::new()
        .open_video(&out, &VideoRequest::default())
        .unwrap();
    assert_eq!(dec.info().size, (960, 544));
    assert_eq!(temp_files(&dir), Vec::<String>::new());

    // Without --json: a line on stderr says what is rendering, and one on
    // stdout what was written.
    let text_out = dir.join("text.mp4");
    let o = spherecut()
        .args(render_args(&p, &text_out, &["--draft", "--range", "..3f"]))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("rendering 3 frames at 960×544 in libx264 to "),
        "{stderr}"
    );
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(stdout.starts_with("wrote "), "{stdout}");
    assert!(text_out.exists());
}

/// `docs/cli-json.md` lists every field each line can have, the ones a
/// run leaves out included.
#[test]
fn the_document_lists_every_field_of_every_line() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli-json.md");
    let docs = std::fs::read_to_string(docs).unwrap();
    let section = docs
        .split("## `spherecut render")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("docs/cli-json.md has a section for render");
    let tables: Vec<&str> = ["`start`:", "`progress`:", "`result`:"]
        .iter()
        .map(|t| {
            section
                .split(t)
                .nth(1)
                .and_then(|s| s.split("\n\n").nth(1))
                .unwrap_or_else(|| panic!("no table after {t}"))
        })
        .collect();

    let fallback = || {
        Some(FallbackInfo {
            from: "hevc_vaapi",
            code: "hw_failed",
            reason: "why".into(),
        })
    };
    let size = Size {
        width: 1,
        height: 1,
    };
    let rate = FrameRate::FPS_30;
    let lines = [
        RenderEvent::Start(RenderStart {
            output: "o".into(),
            project: "p".into(),
            preset: "flat-16x9-1080".into(),
            size,
            draft: false,
            rate,
            range: TimeRange {
                start: Time::ZERO,
                end: Time::ZERO,
            },
            frames: 0,
            duration: Time::ZERO,
            codec: "h264",
            quality: "high",
            encoder: "libx264",
            fallback: fallback(),
            audio: "none",
            motion_blur: false,
        }),
        RenderEvent::Progress(RenderProgress {
            frames_done: 0,
            frames_total: 0,
            fps: 0.0,
            elapsed: 0.0,
            eta: None,
            encoder: "libx264",
            fallback: fallback(),
            decode_fallback: fallback(),
            finishing: Some(Finishing {
                bytes_done: 0,
                bytes_total: 0,
            }),
        }),
        RenderEvent::Result(RenderResult {
            output: "o".into(),
            frames: 0,
            duration: Time::ZERO,
            size,
            draft: false,
            rate,
            codec: "h264",
            encoder: "libx264",
            fallback: fallback(),
            decode: RenderDecode {
                path: "software",
                fallback: fallback(),
            },
            ten_bit: false,
            audio: Some(RenderAudio {
                stream: 1,
                encoder: "aac",
                codec: "aac".into(),
                rate: 48_000,
                channels: 2,
                ambix: false,
                samples: 0,
            }),
            bytes: 0,
            blur_capped: 0,
            checks: vec![RenderCheck {
                name: "probe",
                found: "mp4".into(),
            }],
            stages: RenderStages {
                decode: 0.0,
                render: 0.0,
                readback: 0.0,
                encode: 0.0,
                finish: 0.0,
            },
            elapsed: 0.0,
        }),
    ];
    for (line, table) in lines.iter().zip(&tables) {
        let v = serde_json::to_value(line).unwrap();
        for key in v.as_object().unwrap().keys() {
            assert!(
                table.contains(&format!("| `{key}` |")),
                "docs/cli-json.md's table for {} has no row for {key}",
                v["event"]
            );
        }
        // And each row is a field.
        let rows = table
            .lines()
            .filter_map(|l| l.strip_prefix("| `"))
            .filter_map(|l| l.split('`').next());
        for row in rows {
            assert!(
                v.get(row).is_some(),
                "docs/cli-json.md lists {row} for {}, which has no such field",
                v["event"]
            );
        }
    }
}

// ---------------------------------------------------------------------
// Failures

#[test]
fn a_file_in_the_way_is_kept_unless_overwrite_is_given() {
    let dir = TempDir::new("overwrite");
    let src = clip(&dir, "grid", Pattern::Grid, 30);
    let p = project(&dir, &src);
    let out = dir.join("taken.mp4");
    std::fs::write(&out, b"mine").unwrap();
    let ran = run(&p, &out, &["--json", "--draft", "--range", "..3f"]);
    let e = ran.failed(1, "output_exists");
    assert_eq!(e["path"], out.to_str().unwrap());
    // Refused before any work: the only line.
    assert_eq!(ran.lines.len(), 1, "{:?}", ran.lines);
    assert_eq!(std::fs::read(&out).unwrap(), b"mine");

    let ran = run(
        &p,
        &out,
        &["--json", "--draft", "--range", "..3f", "--overwrite"],
    );
    assert_eq!(ran.ok()["frames"], 3);
    assert_eq!(&std::fs::read(&out).unwrap()[4..8], b"ftyp");
    assert_eq!(temp_files(&dir), Vec::<String>::new());
}

#[test]
fn a_project_that_does_not_load_or_whose_clip_is_offline_says_why() {
    let dir = TempDir::new("project");
    let out = dir.join("out.mp4");
    let missing = dir.join("none.spherecut");
    run(&missing, &out, &["--json"]).failed(1, "not_found");
    let garbage = dir.join("garbage.spherecut");
    std::fs::write(&garbage, b"\x00\x01 not a project").unwrap();
    run(&garbage, &out, &["--json"]).failed(1, "not_project");

    // The clip's file gone: offline, and the error names it.
    let src = clip(&dir, "gone", Pattern::Grid, 30);
    let p = project(&dir, &src);
    std::fs::remove_file(&src).unwrap();
    let ran = run(&p, &out, &["--json"]);
    let e = ran.failed(1, "media_offline");
    assert_eq!(e["path"], src.to_str().unwrap());

    // Another file in its place: exported anyway, with a warning.
    let other = clip(&dir, "other", Pattern::Seam, 30);
    std::fs::rename(&other, &src).unwrap();
    let ran = run(&p, &out, &["--json", "--draft", "--range", "..3f"]);
    ran.ok();
    let codes: Vec<&Value> = ran.last()["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["code"])
        .collect();
    assert_eq!(codes, [&json!("media_changed")]);
    assert!(out.exists());
    assert_eq!(temp_files(&dir), Vec::<String>::new());
}

#[test]
fn a_range_without_frames_is_out_of_range_and_one_without_dots_is_usage() {
    let dir = TempDir::new("range");
    let src = clip(&dir, "grid", Pattern::Grid, 30);
    let p = project(&dir, &src);
    let out = dir.join("out.mp4");
    for range in ["0.5..2", "20f..10f", "30f..", "..31f"] {
        let ran = run(&p, &out, &["--json", "--range", range]);
        let e = ran.failed(1, "out_of_range");
        assert!(
            e["message"].as_str().unwrap().contains("--range"),
            "{range}: {e}"
        );
    }
    let ran = run(&p, &out, &["--json", "--range", "5"]);
    ran.failed(2, "usage");
    assert!(!out.exists());
}

#[test]
fn va_api_alone_without_a_device_exits_3() {
    if Path::new(DEFAULT_RENDER_NODE).exists() {
        eprintln!("skipped: {DEFAULT_RENDER_NODE} is there, and Tier 2 exports through it");
        return;
    }
    let dir = TempDir::new("vaapi");
    let src = clip(&dir, "grid", Pattern::Grid, 30);
    let p = project(&dir, &src);
    let out = dir.join("out.mp4");
    let ran = run(&p, &out, &["--json", "--encoder", "vaapi"]);
    let e = ran.failed(3, "no_encoder");
    assert!(
        e["message"].as_str().unwrap().contains("no render node"),
        "{e}"
    );
    // And auto goes on in software, saying why in the lines' fields.
    let ran = run(
        &p,
        &out,
        &["--json", "--encoder", "auto", "--draft", "--range", "..3f"],
    );
    let result = ran.ok();
    assert_eq!(result["encoder"], "libx264");
    let f = &ran.lines[0]["result"]["fallback"];
    assert_eq!(
        (&f["from"], &f["code"]),
        (&json!("h264_vaapi"), &json!("hw_unavailable"))
    );
    assert_eq!(&result["fallback"], f);
    assert_eq!(ran.last()["warnings"], json!([]));
}

#[test]
fn usage_errors_and_panics_are_render_envelopes() {
    let dir = TempDir::new("usage");
    // No -o.
    let o = spherecut()
        .args(["render", "p.spherecut", "--json"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let usage = lines(&o.stdout);
    assert_eq!(usage.len(), 1, "{usage:?}");
    assert_eq!(usage[0]["schema"], contract::RENDER);
    assert_eq!(usage[0]["error"]["code"], "usage");

    if !cfg!(debug_assertions) {
        return;
    }
    let src = clip(&dir, "grid", Pattern::Grid, 30);
    let p = project(&dir, &src);
    let out = dir.join("out.mp4");
    for how in ["main", "thread"] {
        let o = spherecut()
            .args(render_args(&p, &out, &["--json", "--draft"]))
            .env("SPHERECUT_DEBUG_PANIC", how)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{how}");
        let lines = lines(&o.stdout);
        let v = lines.last().unwrap();
        assert_eq!(v["schema"], contract::RENDER, "{v}");
        assert_eq!(v["error"]["code"], "internal", "{v}");
        assert!(!out.exists());
    }
}

// ---------------------------------------------------------------------
// Ctrl+C and a full disk

#[test]
fn ctrl_c_cancels_the_render_and_leaves_nothing() {
    let dir = TempDir::new("sigint");
    let src = clip(&dir, "long", Pattern::Grid, 300);
    let p = project(&dir, &src);
    let out = dir.join("out.mp4");
    let mut child = spherecut()
        .args(render_args(&p, &out, &["--json"]))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    // Once a progress line says it is under way, and far from done.
    let mut seen = Vec::new();
    loop {
        let mut l = String::new();
        assert!(stdout.read_line(&mut l).unwrap() > 0, "ended: {seen:?}");
        let v: Value = serde_json::from_str(&l).unwrap();
        let event = v["result"]["event"].as_str().unwrap_or("").to_string();
        seen.push(v);
        if event == "progress" {
            break;
        }
        assert_eq!(event, "start", "{seen:?}");
    }
    let done = seen.last().unwrap()["result"]["frames_done"]
        .as_u64()
        .unwrap();
    assert!(done < 250, "{done} of 300 frames done in the first second");
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let sent = Instant::now();
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let o = child.wait_with_output().unwrap();
    let after = sent.elapsed();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(4), "{rest} / {stderr}");
    let rest = lines(rest.as_bytes());
    let v = rest.last().unwrap();
    assert_eq!(v["schema"], contract::RENDER);
    assert_eq!(v["error"]["code"], "cancelled", "{v}");
    assert_eq!(v["error"]["message"], "interrupted: no file written");
    assert!(
        events(&rest[..rest.len() - 1])
            .iter()
            .all(|e| *e == "progress"),
        "{rest:?}"
    );
    // Within the plan's 2 s.
    assert!(
        after < Duration::from_secs(2),
        "stopped {after:?} after SIGINT"
    );
    assert!(!out.exists());
    assert_eq!(temp_files(&dir), Vec::<String>::new());
}

/// Whether this box lets a user mount a tmpfs of their own, in a user
/// and mount namespace (`unshare`), as Fedora and Arch do.
fn own_tmpfs(at: &Path) -> bool {
    Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "sh", "-c"])
        .arg(r#"mount -t tmpfs -o size=1m tmpfs "$0""#)
        .arg(at)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn a_disk_that_fills_during_the_render_fails_it_and_leaves_nothing() {
    let dir = TempDir::new("full");
    let disk = dir.join("disk");
    std::fs::create_dir(&disk).unwrap();
    if !own_tmpfs(&disk) {
        eprintln!(
            "skipped: this box doesn't let a user mount a tmpfs of their own; the export's \
             tests fill a fake disk instead"
        );
        return;
    }
    let src = clip(&dir, "long", Pattern::Grid, 300);
    let p = project(&dir, &src);
    let out = disk.join("out.mp4");
    // The tmpfs is the shell's, which lists what is left on it beside it
    // before it goes.
    let script = r#"mount -t tmpfs -o size=128m tmpfs "$0" || exit 90
"$@"
status=$?
ls -A "$0" > "$0.left"
exit $status"#;
    let mut child = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "sh", "-c", script])
        .arg(&disk)
        .arg(env!("CARGO_BIN_EXE_spherecut"))
        .args(render_args(&p, &out, &["--json"]))
        .env_remove("SPHERECUT_LOG")
        .env_remove("SPHERECUT_ADAPTER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut seen = Vec::new();
    while !seen
        .last()
        .is_some_and(|l: &String| l.contains(r#""event":"progress""#))
    {
        let mut l = String::new();
        assert!(stdout.read_line(&mut l).unwrap() > 0, "ended: {seen:?}");
        assert!(!l.contains(r#""event":"result""#), "it finished: {seen:?}");
        seen.push(l);
    }

    // Part way, with frames in the file and past the free-space check,
    // something else fills the disk: through the shell's root, which
    // sees its tmpfs.
    let filler = PathBuf::from(format!("/proc/{}/root", child.id()))
        .join(disk.strip_prefix("/").unwrap())
        .join("filler");
    let mut f = std::fs::File::create(&filler).unwrap();
    let mib = vec![0u8; 1 << 20];
    let full = loop {
        if let Err(e) = f.write_all(&mib) {
            break e;
        }
    };
    assert_eq!(full.kind(), std::io::ErrorKind::StorageFull, "{full}");
    drop(f);

    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let o = child.wait_with_output().unwrap();
    let ran = Ran {
        status: o.status.code(),
        lines: lines(rest.as_bytes()),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    };
    let e = ran.failed(1, "io_error");
    let message = e["message"].as_str().unwrap();
    eprintln!("the full disk, after {} lines: {message}", seen.len());
    assert!(message.contains("No space left on device"), "{message}");
    assert_eq!(e["path"], out.to_str().unwrap());
    let left = std::fs::read_to_string(dir.join("disk.left")).unwrap();
    assert_eq!(left.lines().collect::<Vec<_>>(), ["filler"]);
}

// ---------------------------------------------------------------------
// frame --project

/// `project`'s sequence panning right at 100° a second from yaw 0, 10°
/// down, at the high preset: each frame's view is 3.3° on from the
/// last, so neighbours differ.
fn panning(project: &Path) {
    let mut p = file::load(project).unwrap();
    let (sequence, clip) = (p.the_sequence().id, p.the_clip().id);
    let export = p.the_sequence().export;
    let key = |ms: i64| Key {
        id: KeyId::NEW,
        at: Time::seconds(Rational::new(ms, 1000).unwrap()),
        value: Orient::View(
            Orientation::from_ypr(Ypr::from_degrees(ms as f64 / 10.0, -10.0, 0.0)).unwrap(),
        ),
        ease: Ease::LINEAR,
        spin: 0,
    };
    p.apply(&[
        Edit::SetKeys {
            clip,
            keys: AnyKeys::Orientation(vec![key(0), key(400)]),
        },
        Edit::SetExport {
            sequence,
            export: ExportSettings {
                quality: Quality::High,
                ..export
            },
        },
    ])
    .unwrap();
    file::save(&p, project).unwrap();
}

/// What a `frame --json` run gave: its status and its one document.
fn frame(args: &[&OsStr]) -> (Option<i32>, Value) {
    let mut all: Vec<&OsStr> = vec!["frame".as_ref()];
    all.extend_from_slice(args);
    for (flag, value) in [("--adapter", "cpu"), ("--decoder", "software")] {
        if !args.contains(&flag.as_ref()) {
            all.extend([OsStr::new(flag), OsStr::new(value)]);
        }
    }
    all.push("--json".as_ref());
    let out = spherecut().args(&all).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("{e}: {stdout:?} / {}", String::from_utf8_lossy(&out.stderr)));
    (out.status.code(), doc)
}

/// `frame --project P -o OUT ARGS`, which must write OUT: its result.
fn still_of(project: &Path, out: &Path, args: &[&str]) -> Value {
    let mut all: Vec<&OsStr> = vec![
        "--project".as_ref(),
        project.as_os_str(),
        "-o".as_ref(),
        out.as_os_str(),
    ];
    all.extend(args.iter().map(OsStr::new));
    let (status, doc) = frame(&all);
    assert_eq!(status, Some(0), "{doc}");
    assert_eq!(doc["schema"], contract::FRAME);
    doc["result"].clone()
}

/// The luma of an 8-bit PNG as the export's YUV pass makes it from the
/// same display values (BT.709, limited range), in unit scale.
fn png_luma(path: &Path) -> (usize, usize, Vec<f64>) {
    let image = Image::from_png(&std::fs::read(path).unwrap()).unwrap();
    let Pixels::Rgb8(rgb) = &image.pixels else {
        panic!("an 8-bit PNG");
    };
    let luma = rgb
        .chunks_exact(3)
        .map(|c| {
            let [r, g, b] = [c[0], c[1], c[2]].map(|v| f64::from(v) / 255.0);
            (16.0 + 219.0 * (0.2126 * r + 0.7152 * g + 0.0722 * b)) / 255.0
        })
        .collect();
    (image.width as usize, image.height as usize, luma)
}

/// The luma of every frame of the 8-bit `video`, in unit scale.
fn video_luma(video: &Path) -> Vec<(usize, usize, Vec<f64>)> {
    let mut dec = FfmpegBackend::new()
        .open_video(video, &VideoRequest::default())
        .unwrap();
    let stop = StopToken::never();
    let SeekOutcome::Frame { frame, .. } = dec.seek(Time::ZERO, SeekMode::Exact, &stop).unwrap()
    else {
        panic!("the seek stopped");
    };
    let mut frames = vec![frame];
    while let NextFrame::Frame(f) = dec.next_frame(&stop).unwrap() {
        frames.push(f);
    }
    frames
        .iter()
        .map(|f| {
            let p = f.plane(0).unwrap();
            let (w, h) = (p.width as usize, p.height as usize);
            let luma = (0..h)
                .flat_map(|y| p.bytes[y * p.stride..][..w].iter())
                .map(|&v| f64::from(v) / 255.0)
                .collect();
            (w, h, luma)
        })
        .collect()
}

/// PSNR and SSIM of `a` against `b`.
fn measure(a: &(usize, usize, Vec<f64>), b: &(usize, usize, Vec<f64>)) -> (f64, f64) {
    assert_eq!((a.0, a.1), (b.0, b.1));
    let (a, b) = (plane(a), plane(b));
    (psnr_plane(a, b).unwrap(), ssim_plane(a, b).unwrap())
}

fn plane(p: &(usize, usize, Vec<f64>)) -> Plane<'_> {
    Plane {
        width: p.0,
        height: p.1,
        data: &p.2,
    }
}

#[test]
fn frame_project_is_the_exports_frame_before_its_encode() {
    let dir = TempDir::new("still");
    let src = clip(&dir, "grid", Pattern::Grid, 12);
    let p = project(&dir, &src);
    panning(&p);
    let out = dir.join("out.mp4");
    run(&p, &out, &["--json"]).ok();
    let exported = video_luma(&out);
    assert_eq!(exported.len(), 12);

    let f7 = dir.join("f7.png");
    let r = still_of(&p, &f7, &["--frame", "7"]);
    assert_eq!(r["size"], json!({"width": 1920, "height": 1080}));
    let shown = serde_json::to_value(FrameRate::FPS_30.frame_to_time(7).unwrap()).unwrap();
    assert_eq!(
        r["sequence"],
        json!({
            "project": p.to_str().unwrap(),
            "frame": {"exact": shown["exact"], "seconds": shown["seconds"], "frame": 7},
            "output": "flat",
            "views": 1,
            "capped": false,
        })
    );
    assert_eq!(r["requested"]["frame"], 7);
    // The source frame at 7/30 s, the clip being untrimmed at 30 fps.
    assert_eq!(r["frame"]["time"]["frame"], 7);
    // The keyed view at 7/30 s: 23.33° right, its FOV the frame's.
    let view = &r["view"];
    assert!(
        (view["yaw"].as_f64().unwrap() - 70.0 / 3.0).abs() < 1e-5,
        "{view}"
    );
    assert!(
        (view["pitch"].as_f64().unwrap() + 10.0).abs() < 1e-5,
        "{view}"
    );
    assert_eq!(
        (&view["fov"], &view["projection"], &view["morph_to"]),
        (&json!(90.0), &json!("rectilinear"), &json!(null))
    );
    assert!(
        ["iso", "aniso"].contains(&r["filter"].as_str().unwrap()),
        "{r}"
    );

    // The parity sample: the still is the exported frame before its
    // encode, and the frame before isn't.
    let (psnr, ssim) = measure(&png_luma(&f7), &exported[7]);
    let f6 = dir.join("f6.png");
    still_of(&p, &f6, &["--frame", "6"]);
    let (control, _) = measure(&png_luma(&f6), &exported[7]);
    eprintln!("frame 7: {psnr:.2} dB, SSIM {ssim:.5}; frame 6 against it: {control:.2} dB");
    // Lavapipe and libx264 at the high preset measured 51.6 dB and
    // 0.9974; the frame before, 18.4 dB.
    assert!(
        psnr >= 48.5 && ssim >= 0.994,
        "{psnr:.2} dB, SSIM {ssim:.5}"
    );
    assert!(control < psnr - 12.0, "{control:.2} dB against {psnr:.2}");

    // --time is sequence time, and shows the frame on screen then.
    let t = dir.join("t.png");
    let r = still_of(&p, &t, &["--time", "0.25"]);
    assert_eq!(r["sequence"]["frame"]["frame"], 7);
    assert_eq!(r["requested"], json!({"exact": "1/4", "seconds": 0.25}));
    assert_eq!(std::fs::read(&t).unwrap(), std::fs::read(&f7).unwrap());

    // The line for people says which frame of what.
    let text = dir.join("text.png");
    let o = spherecut()
        .args([
            "frame",
            "--adapter",
            "cpu",
            "--decoder",
            "software",
            "--frame",
            "7",
        ])
        .arg("--project")
        .arg(&p)
        .arg("-o")
        .arg(&text)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    let line = String::from_utf8(o.stdout).unwrap();
    let want = format!(
        "wrote {}: 1920×1080 rectilinear 90° at 0.233 s of {} (its frame 7; the source's at \
         0.233 s, frame 7, pts ",
        text.display(),
        p.display()
    );
    assert!(line.starts_with(&want), "{line}");

    // With motion blur, the frame's views over the shutter.
    let mut project = file::load(&p).unwrap();
    let sequence = project.the_sequence();
    let export = ExportSettings {
        motion_blur: true,
        ..sequence.export
    };
    let id = sequence.id;
    project
        .apply(&[Edit::SetExport {
            sequence: id,
            export,
        }])
        .unwrap();
    file::save(&project, &p).unwrap();
    let blurred = dir.join("blurred.png");
    let r = still_of(&p, &blurred, &["--frame", "7"]);
    let views = r["sequence"]["views"].as_u64().unwrap();
    assert!(views > 1, "{r}");
    assert_ne!(
        std::fs::read(&blurred).unwrap(),
        std::fs::read(&f7).unwrap()
    );
}

#[test]
fn frame_project_takes_the_view_from_the_project_and_no_frame_past_its_end() {
    let dir = TempDir::new("still-flags");
    let src = clip(&dir, "grid", Pattern::Grid, 12);
    let p = project(&dir, &src);
    let png = dir.join("x.png");
    let with = |args: &[&str]| {
        let mut all: Vec<&OsStr> = vec!["-o".as_ref(), png.as_os_str()];
        all.extend(args.iter().map(OsStr::new));
        frame(&all)
    };
    let failed = |(status, doc): (Option<i32>, Value), want: (i32, &str)| {
        assert_eq!(
            (status, doc["error"]["code"].as_str()),
            (Some(want.0), Some(want.1)),
            "{doc}"
        );
        assert_eq!(doc["schema"], contract::FRAME, "{doc}");
        doc["error"]["message"].as_str().unwrap().to_string()
    };
    let project = p.to_str().unwrap();
    let file = src.to_str().unwrap();
    // The view is the project's, and so is the file.
    for flags in [
        &["--project", project, file][..],
        &["--project", project, "--yaw", "10"],
        &["--project", project, "--size", "64x64"],
        &["--project", project, "--filter", "aniso"],
        &["--project", project, "--reference"],
        &[],
    ] {
        failed(with(flags), (2, "usage"));
    }
    // Twelve frames: 0 to 11, and the last ends at 0.4 s.
    let m = failed(
        with(&["--project", project, "--frame", "12"]),
        (1, "out_of_range"),
    );
    assert_eq!(m, "no frame 12: the sequence's frames are 0 to 11");
    let m = failed(
        with(&["--project", project, "--time", "0.4"]),
        (1, "out_of_range"),
    );
    assert_eq!(
        m,
        "no frame at 0.4 s: the sequence runs from 0 to 0.4 s (its last frame starts at 0.367 s)"
    );
    // An offline clip, as render says.
    std::fs::remove_file(&src).unwrap();
    failed(with(&["--project", project]), (1, "media_offline"));
    assert!(!png.exists());
    assert_eq!(temp_files(&dir), Vec::<String>::new());
}
