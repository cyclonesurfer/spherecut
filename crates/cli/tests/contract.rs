//! The CLI's JSON contract (DECISIONS D27, `docs/cli-json.md`) for probe
//! and doctor: with `--json`, exactly one compact document on stdout per
//! run, on success and on every kind of failure; the same shape for one
//! file or many; the exit statuses; codes that don't change; and times
//! that read back exactly.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use spherecut_cli::contract::{self, Exit};
use spherecut_core::{FrameRate, FrameTime, Rational, Time};
use spherecut_media::doctor::{self, DoctorReport, Issue, Severity};
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, Codec, Spec};

mod document;

use document::document;

/// Every code the JSON can carry, as `docs/cli-json.md` lists them.
/// Renaming one is a new schema version (D27), so a rename must fail here.
const CODES: &[&str] = &[
    "cannot_decode",
    "damaged",
    "decode_failed",
    "doctor_errors",
    "files_failed",
    "hw_unavailable",
    "hw_unsupported",
    "internal",
    "io_error",
    "nested_open_refused",
    "no_decoder",
    "no_video_or_audio",
    "not_a_file",
    "not_found",
    "not_media",
    "open_failed",
    "too_large",
    "track_without_stream",
    "truncated",
    "unexpected_first_box",
    "unsupported_container",
    "usage",
];

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("contract-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn spherecut(args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(args)
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap()
}

/// `spherecut probe --json` on `files`: its exit status and its document.
fn probe_json(files: &[&Path]) -> (Option<i32>, Value) {
    let mut args = vec![OsStr::new("probe"), OsStr::new("--json")];
    args.extend(files.iter().map(|f| f.as_os_str()));
    let out = spherecut(&args);
    (out.status.code(), document(&out.stdout))
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A small spherical H.264 clip with stereo audio at `rate`, and the
/// broken files made from it.
fn clip(dir: &Path, rate: FrameRate) -> (PathBuf, Vec<PathBuf>) {
    let spec = Spec {
        name: "good".into(),
        pattern: Pattern::Grid,
        width: 256,
        height: 128,
        rate,
        frames: 7,
        codec: Codec::H264,
        bits: 8,
        audio: Audio::Stereo,
        spherical: true,
    };
    let good = media::write(&spec, dir).unwrap();
    let bad = media::write_bad_files(&good, dir).unwrap();
    (good, bad)
}

/// A Matroska element with an 8-byte size.
fn el(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let size = (payload.len() as u64 | 1 << 56).to_be_bytes();
    [id, &size, payload].concat()
}

/// A Matroska file whose only track is a line of subtitles.
fn subtitles_only() -> Vec<u8> {
    let header = el(
        b"\x1a\x45\xdf\xa3",
        &[el(b"\x42\x82", b"matroska"), el(b"\x42\x87", &[4])].concat(),
    );
    let info = el(
        b"\x15\x49\xa9\x66",
        &el(b"\x2a\xd7\xb1", &[0x0f, 0x42, 0x40]),
    );
    let track = [
        el(b"\xd7", &[1]),
        el(b"\x73\xc5", &[1]),
        // Track type 0x11: subtitles.
        el(b"\x83", &[0x11]),
        el(b"\x86", b"S_TEXT/UTF8"),
    ]
    .concat();
    let tracks = el(b"\x16\x54\xae\x6b", &el(b"\xae", &track));
    let block = [&b"\x81\x00\x00\x80"[..], b"not a video"].concat();
    let cluster = el(
        b"\x1f\x43\xb6\x75",
        &[el(b"\xe7", &[0]), el(b"\xa3", &block)].concat(),
    );
    let segment = [info, tracks, cluster].concat();
    [header, el(b"\x18\x53\x80\x67", &segment)].concat()
}

/// Whether JSON object `v` has exactly the keys `want`, in any order.
fn has_keys(v: &Value, want: &[&str]) -> bool {
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = want.to_vec();
    keys.sort_unstable();
    want.sort_unstable();
    keys == want
}

#[test]
fn every_code_is_listed_and_stable() {
    assert_eq!(contract::all_codes(), CODES);
    for c in CODES {
        assert!(
            !c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{c} is not snake_case"
        );
    }
    // The statuses are part of the contract too; 3 and 4 are reserved for
    // `frame` and the GUI.
    let statuses = [
        (Exit::Ok, 0),
        (Exit::Failed, 1),
        (Exit::Usage, 2),
        (Exit::Missing, 3),
        (Exit::Cancelled, 4),
    ];
    for (exit, status) in statuses {
        assert_eq!(exit as u8, status, "{exit:?}");
    }
}

#[test]
fn one_file_or_many_is_one_document_of_the_same_shape() {
    let dir = tmp("shape");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let copy = dir.join("copy.mp4");
    std::fs::copy(&good, &copy).unwrap();

    let (status, one) = probe_json(&[&good]);
    assert_eq!(status, Some(0), "{one}");
    let (status, two) = probe_json(&[&good, &copy]);
    assert_eq!(status, Some(0), "{two}");
    for v in [&one, &two] {
        assert!(has_keys(v, &["schema", "ok", "result", "warnings"]), "{v}");
        assert_eq!(v["schema"], "spherecut.probe/1");
        assert_eq!(v["ok"], true);
        assert_eq!(v["warnings"], serde_json::json!([]));
        assert!(has_keys(&v["result"], &["files"]));
        for f in v["result"]["files"].as_array().unwrap() {
            assert!(has_keys(f, &["path", "ok", "report"]), "{f}");
            assert_eq!(f["ok"], true);
            assert_eq!(f["report"]["path"], f["path"]);
        }
    }
    let files = &two["result"]["files"];
    assert_eq!(files.as_array().unwrap().len(), 2);
    assert_eq!(files[0]["path"], good.to_str().unwrap());
    assert_eq!(files[1]["path"], copy.to_str().unwrap());
    // The one file's entry is the first of the two.
    assert_eq!(one["result"]["files"][0], files[0]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_kind_of_failure_is_one_document_with_its_code() {
    let dir = tmp("failures");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let write = |name: &str, data: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    };
    let subtitles = write("subtitles.mkv", &subtitles_only());
    // FFmpeg reads this as LRC lyrics, which Spherecut doesn't open.
    let lyrics = write("lyrics.txt", b"[00:01.00]not a video\n");
    // (file, its code, whether it still has a report)
    let cases: Vec<(PathBuf, &str, bool)> = vec![
        (dir.join("missing.mp4"), "not_found", false),
        // A path through a regular file: ENOTDIR.
        (good.join("x.mp4"), "io_error", false),
        (dir.clone(), "not_a_file", false),
        (dir.join("bad-empty.mp4"), "not_media", false),
        (dir.join("bad-text.mp4"), "not_media", false),
        (lyrics, "unsupported_container", false),
        (dir.join("bad-trunc-half.mp4"), "truncated", false),
        (dir.join("bad-trunc-end.mp4"), "truncated", true),
        (dir.join("bad-corrupt-moov.mp4"), "damaged", true),
        (dir.join("bad-timescale.mp4"), "damaged", true),
        (subtitles, "no_video_or_audio", true),
    ];
    for (path, code, has_report) in &cases {
        let (status, v) = probe_json(&[path]);
        assert_eq!(status, Some(1), "{}: {v}", path.display());
        assert!(has_keys(
            &v,
            &["schema", "ok", "result", "error", "warnings"]
        ));
        assert_eq!(v["schema"], "spherecut.probe/1");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "files_failed");
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(msg.starts_with("1 of 1 file failed: "), "{msg}");
        let file = &v["result"]["files"][0];
        assert_eq!(file["ok"], false);
        assert_eq!(file["path"], path.to_str().unwrap());
        assert_eq!(file["error"]["code"], *code, "{}: {v}", path.display());
        assert_eq!(file["error"]["path"], file["path"]);
        assert!(CODES.contains(code));
        assert_eq!(file.get("report").is_some(), *has_report, "{v}");
        let message = file["error"]["message"].as_str().unwrap();
        assert!(msg.ends_with(message), "{msg} / {message}");
        if *has_report {
            // FFmpeg's log is the report's, not repeated in the error.
            assert!(file["error"].get("ffmpeg_log").is_none(), "{v}");
        }
    }
    // FFmpeg's word on a file it could not open is in its error.
    let (_, v) = probe_json(&[&dir.join("bad-trunc-half.mp4")]);
    let log = v["result"]["files"][0]["error"]["ffmpeg_log"].clone();
    assert!(
        log.as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().ends_with("moov atom not found")),
        "{v}"
    );

    // All of them in one run, with a good file first: the same entries, in
    // order, and one error for the run.
    let mut all: Vec<&Path> = vec![&good];
    all.extend(cases.iter().map(|(p, ..)| p.as_path()));
    let (status, v) = probe_json(&all);
    assert_eq!(status, Some(1));
    let files = v["result"]["files"].as_array().unwrap();
    assert_eq!(files.len(), cases.len() + 1);
    assert_eq!(files[0]["ok"], true);
    for (f, (_, code, _)) in files[1..].iter().zip(&cases) {
        assert_eq!(f["error"]["code"], *code, "{f}");
    }
    let msg = v["error"]["message"].as_str().unwrap();
    let want = format!(
        "{n} of {} files failed: {}",
        cases.len() + 1,
        files[1]["error"]["message"].as_str().unwrap(),
        n = cases.len()
    );
    assert!(msg.starts_with(&want), "{msg}");
    assert!(
        msg.ends_with(&format!(" (and {} more)", cases.len() - 1)),
        "{msg}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn problems_carry_codes() {
    let dir = tmp("problems");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let mut f = std::fs::read(&good).unwrap();
    // A first box FFmpeg reads past: a warning, not damage.
    f[4..8].copy_from_slice(b"ftyq");
    // A sample entry of a codec FFmpeg doesn't know.
    let at = f.windows(4).position(|w| w == b"stsd").unwrap() + 16;
    assert_eq!(&f[at..at + 4], b"avc1");
    f[at..at + 4].copy_from_slice(b"zzzz");
    let odd = dir.join("odd.mp4");
    std::fs::write(&odd, &f).unwrap();
    let large = media::write_mjpeg_declaring(&dir, "large.mp4", 16400, 8200).unwrap();

    let (status, v) = probe_json(&[&odd, &large]);
    assert_eq!(status, Some(0), "{v}");
    let codes = |i: usize| -> Vec<String> {
        v["result"]["files"][i]["report"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                assert!(has_keys(w, &["code", "message"]));
                w["code"].as_str().unwrap().to_string()
            })
            .collect()
    };
    assert_eq!(codes(0), ["unexpected_first_box", "cannot_decode"], "{v}");
    assert_eq!(codes(1), ["too_large"], "{v}");
    for c in codes(0).iter().chain(&codes(1)) {
        assert!(CODES.contains(&c.as_str()), "{c}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn usage_errors_under_json_are_a_document_and_exit_2() {
    // (arguments, the schema of the document)
    let cases: &[(&[&str], &str)] = &[
        (&["probe", "--json"], "spherecut.probe/1"),
        (
            &["probe", "--json", "--frobnicate", "x.mp4"],
            "spherecut.probe/1",
        ),
        (&["doctor", "--json", "--render-node"], "spherecut.doctor/1"),
        (&["doctor", "--json", "extra"], "spherecut.doctor/1"),
        (&["--json"], "spherecut/1"),
        (&["frobnicate", "--json"], "spherecut/1"),
    ];
    for (args, schema) in cases {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let out = spherecut(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let v = document(&out.stdout);
        assert!(has_keys(&v, &["schema", "ok", "error", "warnings"]), "{v}");
        assert_eq!(v["schema"], *schema, "{args:?}");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "usage");
        let message = v["error"]["message"].as_str().unwrap();
        // clap's words, which end by pointing to --help.
        assert!(message.ends_with("try '--help'."), "{message}");
        assert!(!message.starts_with("error:"), "{message}");
        // And clap's own words for people, on stderr.
        assert!(text(&out.stderr).starts_with("error: "), "{args:?}");
    }

    // Without --json, it is clap's text alone.
    let out = spherecut(&[OsStr::new("probe")]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(
        text(&out.stderr).contains("the following required arguments were not provided"),
        "{}",
        text(&out.stderr)
    );
    // Help is text, with --json too, and is no failure.
    let out = spherecut(&["probe", "--json", "--help"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).contains("Usage: spherecut probe"));
    // After `--`, `--json` is a file name, and it isn't there.
    let out = spherecut(&["probe", "--", "--json"].map(OsStr::new));
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert!(text(&out.stderr).contains("--json: No such file"));
}

#[test]
fn stdout_carries_only_the_document_whatever_the_log_says() {
    let dir = tmp("log");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .arg(&good)
        .env("SPHERECUT_LOG", "trace")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(document(&out.stdout)["ok"], true);
    // The log went to stderr.
    assert!(
        text(&out.stderr).contains(" ffmpeg: "),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_closed_stdout_keeps_the_exit_status() {
    let dir = tmp("closed");
    let (good, bad) = clip(&dir, FrameRate::FPS_25);
    for (files, status) in [(vec![&good], 0), (vec![&good, &bad[0]], 1)] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_spherecut"))
            .args(["probe", "--json"])
            .args(&files)
            .env_remove("SPHERECUT_LOG")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Closed before probe has finished, so its one write fails.
        drop(child.stdout.take());
        let out = child.wait_with_output().unwrap();
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(status), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert!(!stderr.contains("stdout"), "{stderr}");
    }
    // A stdout that can't be written is a failure, said on stderr.
    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .arg(&good)
        .env_remove("SPHERECUT_LOG")
        .stdout(std::fs::File::create("/dev/full").unwrap())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("error: cannot write to stdout"),
        "{}",
        text(&out.stderr)
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn times_and_rationals_read_back_exactly() {
    let dir = tmp("times");
    let (good, _) = clip(&dir, FrameRate::FPS_29_97);
    let (status, v) = probe_json(&[&good]);
    assert_eq!(status, Some(0), "{v}");
    let json = &v["result"]["files"][0]["report"];
    let report = spherecut_media::probe::probe(&good).unwrap();

    let exact = |v: &Value| -> Rational { v.as_str().unwrap().parse().unwrap() };
    // A time is {exact, seconds}; exact reads back to the very value, and
    // seconds is that value as a float.
    let time = |v: &Value| -> Time {
        assert!(has_keys(v, &["exact", "seconds"]), "{v}");
        let t = Time(exact(&v["exact"]));
        assert_eq!(v["seconds"].as_f64().unwrap(), t.to_secs_f64());
        t
    };
    assert_eq!(
        Some(time(&json["container"]["duration"])),
        report.container.duration
    );
    assert_eq!(
        Some(time(&json["container"]["start_time"])),
        report.container.start_time
    );
    let video = &json["streams"][0];
    assert_eq!(video["kind"], "video");
    assert_eq!(Some(time(&video["duration"])), report.streams[0].duration);
    // Rationals are "n/d", or "n" for an integer.
    assert_eq!(video["avg_frame_rate"], "30000/1001");
    assert_eq!(
        Some(exact(&video["avg_frame_rate"])),
        report.streams[0].avg_frame_rate
    );
    assert_eq!(exact(&video["time_base"]), report.streams[0].time_base);
    // Seven frames at 29.97: 7007/30000 s, not 0.2335666... rounded.
    assert_eq!(
        report.streams[0].duration,
        Some(FrameRate::FPS_29_97.frame_to_time(7).unwrap())
    );

    // The shapes themselves, through serde_json, which `--json` uses.
    let t = Time(Rational::new(1001, 30_000).unwrap());
    let s = serde_json::to_string(&t).unwrap();
    assert_eq!(s, r#"{"exact":"1001/30000","seconds":0.03336666666666667}"#);
    assert_eq!(serde_json::from_str::<Time>(&s).unwrap(), t);
    let f = FrameTime::at(FrameRate::FPS_29_97, 3).unwrap();
    let s = serde_json::to_string(&f).unwrap();
    assert_eq!(s, r#"{"exact":"1001/10000","seconds":0.1001,"frame":3}"#);
    assert_eq!(serde_json::from_str::<FrameTime>(&s).unwrap(), f);
    assert_eq!(
        serde_json::to_string(&FrameRate::FPS_25).unwrap(),
        r#""25""#
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A doctor report with `issues` and nothing else.
fn doctor_report(issues: Vec<Issue>) -> DoctorReport {
    DoctorReport {
        distro: doctor::Distro::Other,
        ffmpeg_version: "6.1.1".into(),
        libraries: BTreeMap::new(),
        hw_device_types: Vec::new(),
        decoders: Vec::new(),
        encoders: Vec::new(),
        gpu: doctor::Gpu {
            render_node: "/dev/dri/renderD128".into(),
            vendor: doctor::Vendor::Unknown,
            vendor_id: None,
        },
        devices: Vec::new(),
        vulkan_devices: Vec::new(),
        decode_checks: Vec::new(),
        encode_checks: Vec::new(),
        issues,
    }
}

fn issue(severity: Severity, what: &str) -> Issue {
    Issue {
        severity,
        what: what.into(),
        impact: "i".into(),
        fix: "f".into(),
    }
}

#[test]
fn doctors_errors_fail_the_run_and_its_warnings_do_not() {
    // This FFmpeg has every decoder doctor requires, so its errors are
    // shown on reports made for the purpose, through what the binary
    // runs.
    let warned = doctor_report(vec![
        issue(Severity::Warning, "no GPU"),
        issue(Severity::Info, "a note"),
    ]);
    let (env, exit) = contract::doctor(&warned);
    assert_eq!(exit, Exit::Ok);
    let v: Value = serde_json::from_str(&env.to_line().unwrap()).unwrap();
    assert!(has_keys(&v, &["schema", "ok", "result", "warnings"]));
    assert_eq!(v["ok"], true);

    let failed = doctor_report(vec![
        issue(Severity::Warning, "no GPU"),
        issue(Severity::Error, "FFmpeg has no HEVC decoder"),
        issue(Severity::Error, "FFmpeg has no AAC decoder"),
    ]);
    let (env, exit) = contract::doctor(&failed);
    assert_eq!(exit, Exit::Failed);
    let line = env.to_line().unwrap();
    assert_eq!(line.matches('\n').count(), 1);
    let v: Value = serde_json::from_str(&line).unwrap();
    assert!(has_keys(
        &v,
        &["schema", "ok", "result", "error", "warnings"]
    ));
    assert_eq!(v["schema"], "spherecut.doctor/1");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "doctor_errors");
    assert_eq!(
        v["error"]["message"],
        "doctor found 2 errors: FFmpeg has no HEVC decoder; FFmpeg has no AAC decoder"
    );
    // The report comes with the error.
    assert_eq!(v["result"]["issues"][1]["severity"], "error");

    // The binary, on whatever this machine has: it exits 1 exactly when a
    // problem is an error, in JSON and in text.
    let node = tmp("doctor").join("no-such-renderD128");
    let args = [
        OsStr::new("doctor"),
        OsStr::new("--render-node"),
        node.as_os_str(),
    ];
    let json = spherecut(&[&args[..], &[OsStr::new("--json")]].concat());
    let v = document(&json.stdout);
    let errors = v["result"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["severity"] == "error");
    assert_eq!(json.status.code(), Some(if errors { 1 } else { 0 }), "{v}");
    assert_eq!(v["ok"], !errors);
    assert_eq!(v.get("error").is_some(), errors);
    let text_run = spherecut(&args);
    assert_eq!(text_run.status.code(), json.status.code());
}
