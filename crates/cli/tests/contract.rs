//! The CLI's JSON contract (DECISIONS D27, `docs/cli-json.md`) for probe,
//! doctor and frame: with `--json`, exactly one compact document on stdout
//! per run, on success and on every kind of failure; the same shape for
//! one file or many; the exit statuses, frame's 3 and 4 included; codes
//! that don't change; times that read back exactly; and SIGINT, which
//! cancels frame and ends probe and doctor at once.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

use serde_json::Value;
use spherecut_cli::contract::{self, Exit};
use spherecut_core::{FrameRate, FrameTime, Rational, Time};
use spherecut_media::doctor::{self, DoctorReport, Issue, Severity};
use spherecut_media::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings};
use spherecut_media::ff;
use spherecut_synth::media::{self, Audio, Codec, Spec};
use spherecut_synth::{Pattern, render_equirect, yuv};

// Only `strace` is used here; confinement.rs uses the rest.
#[allow(dead_code)]
mod common;
mod document;

use document::document;

/// Every code the JSON can carry, as `docs/cli-json.md` lists them.
/// Renaming one is a new schema version (D27), so a rename must fail here.
const CODES: &[&str] = &[
    "adapter_not_found",
    "anisotropy_unavailable",
    "cancelled",
    "cannot_decode",
    "crop_applied",
    "damaged",
    "decode_failed",
    "doctor_errors",
    "files_failed",
    "gpu_failed",
    "hw_failed",
    "hw_unavailable",
    "hw_unsupported",
    "internal",
    "invalid_input",
    "io_error",
    "nested_open_refused",
    "no_decoder",
    "no_gpu",
    "no_video_or_audio",
    "not_a_file",
    "not_equirect",
    "not_found",
    "not_implemented",
    "not_media",
    "open_failed",
    "out_of_gpu_memory",
    "out_of_range",
    "output_exists",
    "pose_not_applied",
    "stereo_left_eye",
    "stereo_unknown",
    "too_large",
    "track_without_stream",
    "truncated",
    "unexpected_first_box",
    "unsupported_container",
    "unsupported_pixel_format",
    "usage",
];

/// The codes `frame` exits 3 with: a device or a codec the run needs and
/// this machine doesn't have (D27).
const MISSING: &[&str] = &[
    "adapter_not_found",
    "hw_failed",
    "hw_unavailable",
    "hw_unsupported",
    "no_decoder",
    "no_gpu",
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
        ..Spec::default()
    };
    let good = media::write(&spec, dir).unwrap();
    let bad = media::write_bad_files(&good, dir).unwrap();
    (good, bad)
}

/// A short H.264 clip whose stream says nothing of its colour: no range,
/// matrix, primaries or transfer.
fn untagged_clip(dir: &Path) -> PathBuf {
    let settings = VideoSettings {
        encoder: "libx264".into(),
        width: 256,
        height: 128,
        rate: FrameRate::FPS_25,
        layout: PixelLayout::Yuv420p,
        options: [("preset", "veryfast"), ("threads", "1")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .to_vec(),
        color: ColorTags::NONE,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let mut w = Mp4Writer::create(&dir.join("untagged.mp4"), &settings, None).unwrap();
    for i in 0..3 {
        let img = render_equirect(Pattern::Grid, settings.width, settings.height, i, 1);
        w.push_video(&yuv::to_yuv420(&img, 8)).unwrap();
    }
    w.finish().unwrap()
}

/// `src` remuxed into `dst` as a fragmented MP4, as live recorders write
/// one: an empty `moov`, then a `moof` and an `mdat` for each frame.
// Reads a file this test wrote and writes another with ffmpeg-next alone,
// apart from Spherecut's confined open (D24), as synth's decode_errors
// test does.
#[allow(clippy::disallowed_methods)]
fn fragmented(src: &Path, dst: &Path) {
    let mut ictx = ff::format::input(src).unwrap();
    let mut octx = ff::format::output_as(dst, "mp4").unwrap();
    for st in ictx.streams() {
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    let mut opts = ff::Dictionary::new();
    opts.set("movflags", "empty_moov+frag_every_frame");
    octx.write_header_with(opts).unwrap();
    for (st, mut pkt) in ictx.packets() {
        pkt.rescale_ts(st.time_base(), octx.stream(st.index()).unwrap().time_base());
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
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
    // `frame`'s status for each code: 2 for usage, 3 for what is missing,
    // 4 for a cancel, 1 for everything else.
    for c in CODES {
        let want = match *c {
            "usage" => Exit::Usage,
            "cancelled" => Exit::Cancelled,
            c if MISSING.contains(&c) => Exit::Missing,
            _ => Exit::Failed,
        };
        assert_eq!(contract::frame_exit(c), want, "{c}");
    }
    // The statuses are part of the contract too; 3 and 4 are `frame`'s
    // (and the GUI's, later).
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

/// `docs/cli-json.md` is the contract people read, so it must document
/// every code, schema and exit status the binary can give.
#[test]
fn the_document_describes_every_code_schema_and_status() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli-json.md");
    let docs = std::fs::read_to_string(&docs).unwrap();
    for c in CODES {
        assert!(
            docs.contains(&format!("| `{c}` |")),
            "docs/cli-json.md has no table row for the code {c}"
        );
    }
    for schema in [
        contract::PROBE,
        contract::DOCTOR,
        contract::FRAME,
        contract::CLI,
    ] {
        assert!(
            docs.contains(&format!("`{schema}`")),
            "docs/cli-json.md doesn't name the schema {schema}"
        );
    }
    for status in 0..=4 {
        assert!(
            docs.contains(&format!("| {status} |")),
            "docs/cli-json.md has no row for exit status {status}"
        );
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
    let mkv = subtitles_only();
    let subtitles = write("subtitles.mkv", &mkv);
    // Matroska files FFmpeg can't open: cut short in the EBML header, and
    // in the Segment before its tracks.
    let segment = mkv
        .windows(4)
        .position(|w| w == b"\x18\x53\x80\x67")
        .unwrap();
    let mkv_header = write("mkv-cut-header.mkv", &mkv[..segment - 10]);
    let mkv_segment = write("mkv-cut-segment.mkv", &mkv[..segment + 20]);
    // As long as its Segment says, which probe checks, but the elements
    // inside run past it, which FFmpeg finds.
    let mut inside = mkv[..segment + 20].to_vec();
    inside[segment + 4..segment + 12].copy_from_slice(&(8u64 | 1 << 56).to_be_bytes());
    let mkv_inside = write("mkv-damaged-inside.mkv", &inside);
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
        (mkv_header, "truncated", false),
        (mkv_segment, "truncated", false),
        (mkv_inside, "open_failed", false),
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
    // A Matroska file cut short says so, as an MP4 file does.
    let (_, v) = probe_json(&[&dir.join("mkv-cut-header.mkv")]);
    let message = v["result"]["files"][0]["error"]["message"]
        .as_str()
        .unwrap();
    assert!(
        message.ends_with(&format!(
            "the file ends after {} bytes, before its Matroska Segment starts: it was cut short",
            segment - 10
        )),
        "{message}"
    );
    let (_, v) = probe_json(&[&dir.join("mkv-cut-segment.mkv")]);
    let message = v["result"]["files"][0]["error"]["message"]
        .as_str()
        .unwrap();
    assert!(
        message.ends_with(&format!(
            "the file holds 8 of the {} bytes its Matroska Segment says it has: it was cut short",
            mkv.len() - segment - 12
        )),
        "{message}"
    );
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
fn an_unreadable_file_is_an_io_error_not_damage() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp("unreadable");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let mode = |p: &Path, mode: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    // Anyone may reach the directory and read `good`, whatever the umask,
    // so that only its own mode keeps the run from reading the file.
    mode(&dir, 0o755);
    mode(&good, 0o644);
    let file = dir.join("noread.mp4");
    std::fs::copy(&good, &file).unwrap();
    mode(&file, 0o000);
    let bin = env!("CARGO_BIN_EXE_spherecut");
    let as_nobody = std::fs::File::open(&file).is_ok();
    let mut cmd = if !as_nobody {
        Command::new(bin)
    } else {
        // Root reads any file, so the run drops to nobody, when setpriv
        // can run the binary so.
        let nobody = ["--reuid=65534", "--regid=65534", "--clear-groups", bin];
        let runs = Command::new("setpriv")
            .args(nobody)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !runs {
            eprintln!(
                "not run through the binary: this user reads any file, and setpriv \
                 can't run it as nobody; probe's unit test covers the code"
            );
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        let mut cmd = Command::new("setpriv");
        cmd.args(nobody);
        cmd
    };
    let out = cmd
        .args(["probe", "--json"])
        .args([&good, &file])
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap();
    let v = document(&out.stdout);
    // The readable copy beside it is read, so the run reaches the
    // directory and the error below comes from reading the file: a
    // directory it can't search gives the same io_error before probe
    // reads a byte.
    let reached = &v["result"]["files"][0];
    if as_nobody && reached["ok"] != true {
        assert_eq!(reached["error"]["code"], "io_error", "{v}");
        eprintln!(
            "not run through the binary: nobody can't reach {}; probe's unit \
             test covers the code",
            dir.display()
        );
        std::fs::remove_dir_all(&dir).ok();
        return;
    }
    assert_eq!(reached["ok"], true, "{v}");
    assert_eq!(out.status.code(), Some(1), "{v}");
    let f = &v["result"]["files"][1];
    // The fix is chmod, not another copy.
    assert_eq!(f["error"]["code"], "io_error", "{v}");
    let message = f["error"]["message"].as_str().unwrap();
    assert!(message.contains("Permission denied"), "{message}");
    assert!(!message.contains("MP4 structure"), "{message}");
    assert!(f.get("report").is_none(), "{v}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn long_box_lists_are_cut_and_stereo_modes_are_names() {
    let dir = tmp("shapes-mp4");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let mut f = std::fs::read(&good).unwrap();
    // A hundred empty boxes after the rest, as a fragmented recording has
    // a `moof` and an `mdat` per fragment.
    for _ in 0..100 {
        f.extend_from_slice(b"\0\0\0\x08free");
    }
    // A stereo mode the specification doesn't define.
    let at = f.windows(4).position(|w| w == b"st3d").unwrap() + 8;
    assert_eq!(f[at], 0);
    f[at] = 7;
    let odd = dir.join("odd.mp4");
    std::fs::write(&odd, &f).unwrap();

    let (status, v) = probe_json(&[&good, &odd]);
    assert_eq!(status, Some(0), "{v}");
    let report = |i: usize| &v["result"]["files"][i]["report"];
    let (mp4, odd_mp4) = (&report(0)["mp4"], &report(1)["mp4"]);
    assert_eq!(mp4["top_level"].as_array().unwrap().len(), 4, "{mp4}");
    assert_eq!(mp4["top_level_omitted"], 0);
    let listed = odd_mp4["top_level"].as_array().unwrap();
    assert_eq!(listed.len(), 64);
    assert_eq!(listed[..4], mp4["top_level"].as_array().unwrap()[..]);
    assert_eq!(odd_mp4["top_level_omitted"], 104 - 64);
    // Text lists every box, as before.
    let out = spherecut(&[OsStr::new("probe"), odd.as_os_str()]);
    assert_eq!(text(&out.stdout).matches(" free").count(), 101);

    let v2 = |i: usize| &report(i)["streams"][0]["spherical"]["v2"];
    assert_eq!(v2(0)["stereo"], "mono");
    assert_eq!(v2(0)["stereo_value"], 0);
    assert_eq!(v2(1)["stereo"], "unknown");
    assert_eq!(v2(1)["stereo_value"], 7);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_full_check_of_a_fragmented_file_leaves_out_its_fragments() {
    let dir = tmp("fragmented");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let frag = dir.join("fragmented.mp4");
    fragmented(&good, &frag);
    let mut f = std::fs::read(&frag).unwrap();
    // The later half of the fragments' samples, moved far past the end of
    // the file, which loses frames: each `trun`'s data offset (flag 0x1,
    // after the version, flags and sample count).
    let truns: Vec<usize> = (0..f.len() - 4)
        .filter(|&i| &f[i..i + 4] == b"trun")
        .collect();
    assert!(truns.len() > 4, "{} truns", truns.len());
    for &trun in &truns[truns.len() / 2..] {
        assert_eq!(f[trun + 7] & 1, 1, "no data offset");
        f[trun + 12..trun + 16].copy_from_slice(&0x7000_0000u32.to_be_bytes());
    }
    let moved = dir.join("moved.mp4");
    std::fs::write(&moved, &f).unwrap();

    let (status, v) = probe_json(&[&frag, &moved]);
    assert_eq!(status, Some(0), "{v}");
    for file in v["result"]["files"].as_array().unwrap() {
        let report = &file["report"];
        // "full" is every top-level box and `moov`'s sample tables, which
        // a fragmented file leaves empty; its fragments' `trun`s are not
        // checked, as docs/cli-json.md says. Checking them would add
        // damage, which the contract allows within its version.
        assert_eq!(report["structure"], serde_json::json!({"check": "full"}));
        assert_eq!(report["damage"], serde_json::json!([]), "{file}");
        let boxes = &report["mp4"]["top_level"];
        let moofs = boxes.as_array().unwrap().iter().filter(|b| b[0] == "moof");
        assert!(moofs.count() > 1, "{boxes}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn colour_and_field_order_are_ffmpegs_names_not_null() {
    let dir = tmp("colour");
    let (tagged, _) = clip(&dir, FrameRate::FPS_25);
    let untagged = untagged_clip(&dir);
    // Its codec setup is damaged, so FFmpeg knows no chroma location or
    // field order.
    let no_setup = dir.join("bad-codec-config.mp4");
    let (_, v) = probe_json(&[&tagged, &untagged, &no_setup]);
    let video = |i: usize| &v["result"]["files"][i]["report"]["streams"][0]["video"];
    let color = |i: usize| {
        let c = &video(i)["color"];
        [
            "range",
            "matrix",
            "primaries",
            "transfer",
            "chroma_location",
        ]
        .map(|k| c[k].as_str().unwrap_or_else(|| panic!("{k}: {v}")))
    };
    assert_eq!(color(0), ["tv", "bt709", "bt709", "bt709", "center"]);
    assert_eq!(video(0)["field_order"], "progressive");
    // What the file doesn't say is FFmpeg's `unknown`, as ffprobe prints
    // it, not null.
    assert_eq!(color(1)[..4], ["unknown"; 4], "{v}");
    assert_eq!(color(2)[4], "unspecified", "{v}");
    assert_eq!(video(2)["field_order"], "unknown", "{v}");
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
fn a_panic_is_one_internal_document_and_exit_1() {
    // Debug builds panic on SPHERECUT_DEBUG_PANIC, on the main thread or
    // on another; release builds have no such switch.
    let dir = tmp("panic");
    let (good, _) = clip(&dir, FrameRate::FPS_25);
    let run = |how: &str, args: &[&OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_spherecut"))
            .args(args)
            .env_remove("SPHERECUT_LOG")
            .env("SPHERECUT_DEBUG_PANIC", how)
            .output()
            .unwrap()
    };
    let probe = [OsStr::new("probe"), OsStr::new("--json"), good.as_os_str()];
    // As probe starts to run, on its thread or another, and while a
    // command line that doesn't parse (no file) is reported, which is
    // before any command runs.
    let cases: [(&str, &[&OsStr]); 3] =
        [("main", &probe), ("thread", &probe), ("usage", &probe[..2])];
    for (how, args) in cases {
        let out = run(how, args);
        let v = document(&out.stdout);
        if !cfg!(debug_assertions) {
            let usage = how == "usage";
            assert_eq!(out.status.code(), Some(if usage { 2 } else { 0 }), "{v}");
            assert_eq!(v["ok"], !usage);
            continue;
        }
        assert_eq!(out.status.code(), Some(1), "{how}: {v}");
        assert!(
            has_keys(&v, &["schema", "ok", "error", "warnings"]),
            "{how}: {v}"
        );
        assert_eq!(v["schema"], "spherecut.probe/1");
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "internal");
        let message = v["error"]["message"].as_str().unwrap();
        assert!(message.starts_with("spherecut panicked at "), "{message}");
        assert!(
            message.contains(&format!("SPHERECUT_DEBUG_PANIC={how}")),
            "{message}"
        );
        // Rust's own report, for people.
        let stderr = text(&out.stderr);
        assert!(stderr.contains("panicked at"), "{stderr}");
    }
    // Without --json, a panic is Rust's.
    if cfg!(debug_assertions) {
        let out = run("main", &[probe[0], probe[2]]);
        assert_eq!(out.status.code(), Some(101));
        assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    }
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
    // Without a known index (a variable-rate stream), `frame` is left out.
    let vfr = FrameTime {
        time: f.time,
        frame: None,
    };
    let s = serde_json::to_string(&vfr).unwrap();
    assert_eq!(s, r#"{"exact":"1001/10000","seconds":0.1001}"#);
    assert_eq!(serde_json::from_str::<FrameTime>(&s).unwrap(), vfr);
    // A null index, as many JSON writers send for one they don't know, is
    // no index; one that isn't a whole number of 0 or more is refused.
    let read = |frame: &str| {
        serde_json::from_str::<FrameTime>(&format!(r#"{{"exact":"1001/10000","frame":{frame}}}"#))
    };
    assert_eq!(read("null").unwrap(), vfr);
    assert_eq!(read("3").unwrap(), f);
    for bad in ["-1", "3.5", r#""3""#] {
        assert!(read(bad).is_err(), "{bad}");
    }
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
    // A node that isn't there, in no directory of its own to clean up.
    let node = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "contract-{}-no-such-renderD128",
        std::process::id()
    ));
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

// ---------------------------------------------------------------------
// frame

/// A small spherical clip, for `frame`.
fn sphere_clip(dir: &Path) -> PathBuf {
    let spec = Spec {
        name: "sphere".into(),
        pattern: Pattern::Grid,
        width: 256,
        height: 128,
        rate: FrameRate::FPS_25,
        frames: 3,
        codec: Codec::H264,
        spherical: true,
        ..Spec::default()
    };
    media::write(&spec, dir).unwrap()
}

/// `spherecut frame CLIP -o OUT --json ARGS` with `env` added and nothing
/// else from the environment steering it: its status, document and
/// stderr.
fn frame_json(
    clip: &Path,
    out: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (Option<i32>, Value, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    cmd.arg("frame")
        .arg(clip)
        .arg("-o")
        .arg(out)
        .arg("--json")
        .args(args)
        .env_remove("SPHERECUT_LOG")
        .env_remove("SPHERECUT_ADAPTER");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let o = cmd.output().unwrap();
    (o.status.code(), document(&o.stdout), text(&o.stderr))
}

#[test]
fn frame_documents_have_the_envelope_too() {
    let dir = tmp("frame-envelope");
    let clip = sphere_clip(&dir);
    let out = dir.join("out.png");

    let (status, v, stderr) = frame_json(
        &clip,
        &out,
        &[
            "--adapter",
            "cpu",
            "--decoder",
            "software",
            "--size",
            "64x32",
        ],
        &[],
    );
    assert_eq!(status, Some(0), "{v} {stderr}");
    assert!(has_keys(&v, &["schema", "ok", "result", "warnings"]), "{v}");
    assert_eq!(v["schema"], "spherecut.frame/1");
    assert_eq!(v["ok"], true);
    assert_eq!(v["warnings"], serde_json::json!([]));
    let r = &v["result"];
    let fields = [
        "output",
        "frame",
        "requested",
        "exactness",
        "size",
        "depth",
        "view",
        "filter",
        "renderer",
        "decode",
        "gpu",
        "color",
        "source",
    ];
    assert!(has_keys(r, &fields), "{r}");
    assert_eq!(r["output"], out.to_str().unwrap());
    assert!(has_keys(&r["frame"], &["pts", "time"]), "{r}");
    assert!(
        has_keys(&r["frame"]["time"], &["exact", "seconds", "frame"]),
        "{r}"
    );
    assert!(has_keys(&r["requested"], &["exact", "seconds"]), "{r}");
    assert_eq!(r["exactness"], "exact");
    assert_eq!(r["size"], serde_json::json!({"width": 64, "height": 32}));
    assert_eq!(r["depth"], 8);
    assert!(
        has_keys(
            &r["view"],
            &[
                "yaw",
                "pitch",
                "roll",
                "fov",
                "projection",
                "morph_to",
                "morph"
            ]
        ),
        "{r}"
    );
    assert_eq!(r["view"]["projection"], "rectilinear");
    assert_eq!(r["view"]["morph_to"], Value::Null);
    assert_eq!(r["filter"], "iso");
    assert_eq!(r["renderer"], "gpu");
    // Software as asked: no fallback to report.
    assert_eq!(r["decode"], serde_json::json!({"path": "software"}));
    assert!(
        has_keys(&r["gpu"], &["name", "driver", "driver_info", "device_type"]),
        "{r}"
    );
    assert_eq!(r["gpu"]["device_type"], "cpu");
    assert!(
        has_keys(
            &r["color"],
            &[
                "codes",
                "range",
                "matrix",
                "transfer",
                "primaries",
                "siting",
                "assumed"
            ]
        ),
        "{r}"
    );
    assert!(
        has_keys(&r["source"], &["stream", "codec", "size", "pix_fmt"]),
        "{r}"
    );
    assert_eq!(r["source"]["codec"], "h264");

    // `auto` with no render node goes on in software and says why; the
    // CPU reference renders with no adapter.
    let (status, v, _) = frame_json(
        &clip,
        &out,
        &[
            "--reference",
            "--size",
            "64x32",
            "--overwrite",
            "--render-node",
            "/nonexistent",
        ],
        &[],
    );
    assert_eq!(status, Some(0), "{v}");
    let r = &v["result"];
    assert_eq!(r["renderer"], "reference");
    assert_eq!(r["gpu"], Value::Null);
    assert_eq!(
        r["decode"],
        serde_json::json!({
            "path": "software",
            "fallback": {
                "from": "vaapi",
                "code": "hw_unavailable",
                "reason": "no VA-API device: no render node at /nonexistent",
            },
        })
    );

    // A failure: no result, and an error naming the file; a usage error
    // names none.
    // (The output is checked first: it mustn't be there.)
    std::fs::remove_file(&out).unwrap();
    let (status, v, _) = frame_json(&dir.join("missing.mp4"), &out, &["--adapter", "cpu"], &[]);
    assert_eq!(status, Some(1), "{v}");
    assert!(has_keys(&v, &["schema", "ok", "error", "warnings"]), "{v}");
    assert_eq!(v["ok"], false);
    assert!(has_keys(&v["error"], &["code", "message", "path"]), "{v}");
    assert_eq!(v["error"]["code"], "not_found");
    let (status, v, _) = frame_json(&clip, &out, &["--size", "1x1"], &[]);
    assert_eq!(status, Some(2), "{v}");
    assert!(has_keys(&v["error"], &["code", "message"]), "{v}");
    std::fs::remove_dir_all(&dir).ok();
}

/// frame's exit statuses 0 to 3 (4 is SIGINT's, below), each with its
/// document: no adapter at all, by running the child with
/// `VK_DRIVER_FILES` pointing at no ICD, and no VA-API device.
#[test]
fn frame_exits_0_to_3_with_one_document_each() {
    let dir = tmp("frame-exits");
    let clip = sphere_clip(&dir);
    let out = dir.join("out.png");
    let no_icd = dir.join("no-such-icd.json");
    let no_icd = no_icd.to_str().unwrap();
    // (arguments, environment, status, code)
    type Row<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], i32, &'a str);
    let rows: &[Row] = &[
        (&["--adapter", "cpu", "--size", "64x32"], &[], 0, ""),
        (
            &["--adapter", "cpu", "--frame", "3"],
            &[],
            1,
            "out_of_range",
        ),
        (&["--size", "1x1"], &[], 2, "usage"),
        (
            &["--size", "64x32"],
            &[("VK_DRIVER_FILES", no_icd), ("VK_ICD_FILENAMES", no_icd)],
            3,
            "no_gpu",
        ),
        (
            &[
                "--adapter",
                "cpu",
                "--decoder",
                "vaapi",
                "--render-node",
                "/nonexistent",
            ],
            &[],
            3,
            "hw_unavailable",
        ),
    ];
    for (args, env, status, code) in rows {
        std::fs::remove_file(&out).ok();
        let (got, v, stderr) = frame_json(&clip, &out, args, env);
        assert_eq!(got, Some(*status), "{args:?}: {v} {stderr}");
        assert_eq!(v["schema"], "spherecut.frame/1");
        assert_eq!(v["ok"], *status == 0, "{v}");
        assert_eq!(out.exists(), *status == 0, "{args:?}");
        if *status != 0 {
            assert_eq!(v["error"]["code"], *code, "{args:?}: {v}");
        }
    }
    let (_, v, _) = frame_json(
        &clip,
        &out,
        &[],
        &[("VK_DRIVER_FILES", no_icd), ("VK_ICD_FILENAMES", no_icd)],
    );
    let m = v["error"]["message"].as_str().unwrap();
    assert!(m.contains("spherecut doctor"), "{m}");
    std::fs::remove_dir_all(&dir).ok();
}

/// `long-gop` (M1 plan section 14): 1920x960 lossless HEVC in one GOP of
/// `frames` frames, so that a seek to the last decodes every one. The
/// grid moves 2 pixels a frame, so that no two frames are alike.
fn long_gop(dir: &Path, frames: u32) -> PathBuf {
    let (w, h) = (1920usize, 960usize);
    let img = render_equirect(Pattern::Grid, w as u32, h as u32, 0, 1);
    let yuv::YuvPlanes::U8(base) = yuv::to_yuv420(&img, 8) else {
        unreachable!()
    };
    let params = format!(
        "lossless=1:keyint={frames}:min-keyint={frames}:scenecut=0:bframes=0:log-level=error"
    );
    let settings = VideoSettings {
        encoder: "libx265".into(),
        width: w as u32,
        height: h as u32,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: vec![
            ("preset".into(), "ultrafast".into()),
            ("x265-params".into(), params),
        ],
        color: ColorTags::BT709_LIMITED,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let path = dir.join(format!("long-gop-{frames}.mp4"));
    let mut wr = Mp4Writer::create(&path, &settings, None).unwrap();
    // Rolled left by `by` columns.
    let roll = |p: &[u8], pw: usize, by: usize| -> Vec<u8> {
        let mut p = p.to_vec();
        for row in p.chunks_mut(pw) {
            row.rotate_left(by % pw);
        }
        p
    };
    for i in 0..frames as usize {
        let planes = [
            roll(&base[0], w, 2 * i),
            roll(&base[1], w / 2, i),
            roll(&base[2], w / 2, i),
        ];
        wr.push_video(&yuv::YuvPlanes::U8(planes)).unwrap();
    }
    wr.finish().unwrap()
}

/// `frame` on `clip`'s last frame with the log on stderr, running: the
/// child, when it logged that it is seeking, and its stderr once it ends.
fn seek_to_last(
    clip: &Path,
    out: &Path,
    frames: u32,
) -> (
    std::process::Child,
    std::sync::mpsc::Receiver<Instant>,
    std::thread::JoinHandle<String>,
) {
    use std::io::BufRead;
    let mut child = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .arg("frame")
        .arg(clip)
        .arg("-o")
        .arg(out)
        .args([
            "--json",
            "--equirect",
            "--adapter",
            "cpu",
            "--size",
            "64x32",
        ])
        .args(["--frame", &(frames - 1).to_string()])
        .env("SPHERECUT_LOG", "info")
        .env_remove("SPHERECUT_ADAPTER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if line.contains("seeking to") {
                let _ = tx.send(Instant::now());
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    (child, rx, reader)
}

/// Ctrl+C 300 ms into a seek on `long-gop` (M1 plan section 14): exit 4,
/// one `cancelled` document, no output and no temporary file, well
/// before the seek would have ended. The clip is written longer where
/// the whole seek takes under twice the 300 ms, so that the signal always
/// comes in the middle of it.
#[test]
fn sigint_during_a_seek_cancels_with_exit_4() {
    use std::time::Duration;
    let dir = tmp("sigint");
    let whole = dir.join("whole.png");
    let mut frames = 600;
    let (clip, full) = loop {
        let clip = long_gop(&dir, frames);
        let (child, seeking, log) = seek_to_last(&clip, &whole, frames);
        let at = seeking.recv_timeout(Duration::from_secs(60));
        let o = child.wait_with_output().unwrap();
        let full = at.map(|at| at.elapsed());
        let log = log.join().unwrap();
        assert_eq!(o.status.code(), Some(0), "{log}");
        let full = full.unwrap_or_else(|e| panic!("no seek logged ({e}): {log}"));
        // Out of the way of the next, longer clip's run.
        std::fs::remove_file(&whole).unwrap();
        if full >= Duration::from_millis(600) || frames >= 4800 {
            break (clip, full);
        }
        std::fs::remove_file(&clip).ok();
        frames *= 2;
    };
    assert!(
        full >= Duration::from_millis(600),
        "the whole seek took {full:?}"
    );

    let out = dir.join("out.png");
    let (child, seeking, log) = seek_to_last(&clip, &out, frames);
    let Ok(at) = seeking.recv_timeout(Duration::from_secs(60)) else {
        let o = child.wait_with_output().unwrap();
        panic!(
            "no seek logged ({:?}): {}",
            o.status.code(),
            log.join().unwrap()
        );
    };
    std::thread::sleep(Duration::from_millis(300).saturating_sub(at.elapsed()));
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let sent = Instant::now();
    let o = child.wait_with_output().unwrap();
    let after = sent.elapsed();
    eprintln!("{frames} frames: the whole seek took {full:?}; stopped {after:?} after SIGINT");
    let log = log.join().unwrap();
    assert_eq!(o.status.code(), Some(4), "{log}");
    let v = document(&o.stdout);
    assert!(has_keys(&v, &["schema", "ok", "error", "warnings"]), "{v}");
    assert_eq!(v["schema"], "spherecut.frame/1");
    assert_eq!(v["error"]["code"], "cancelled", "{v}");
    assert_eq!(v["error"]["message"], "interrupted: no frame written");
    assert!(log.contains("interrupted: stopping"), "{log}");
    assert!(!out.exists());
    let temps: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| spherecut_core::fs::is_temp_name(&e.file_name().to_string_lossy()))
        .collect();
    assert!(temps.is_empty(), "{temps:?}");
    // It stopped well before the seek's end, which was 300 ms on.
    assert!(
        after + Duration::from_millis(300) < full,
        "stopped {after:?} after the signal; the whole seek takes {full:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A SIGINT that frame starts with ignored stays ignored, as a shell
/// starts a background job (`spherecut frame … &`) or a program that
/// protects its child: no handler replaces it (D27), and the run goes on
/// through a Ctrl+C meant for the foreground and writes its frame.
#[test]
fn an_ignored_sigint_stays_ignored() {
    use std::io::BufRead;
    use std::time::Duration;
    let dir = tmp("sigint-ignored");
    let clip = sphere_clip(&dir);
    let out = dir.join("out.png");
    // A render that lavapipe takes a second or more over, so that the run
    // is still going once it has logged its seek.
    let mut child = Command::new("sh")
        .args(["-c", "trap '' INT; exec \"$0\" \"$@\""])
        .arg(env!("CARGO_BIN_EXE_spherecut"))
        .arg("frame")
        .arg(&clip)
        .arg("-o")
        .arg(&out)
        .args(["--json", "--adapter", "cpu", "--size", "8192x4096"])
        .env("SPHERECUT_LOG", "info")
        .env_remove("SPHERECUT_ADAPTER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let log = std::thread::spawn(move || {
        let mut all = String::new();
        for line in std::io::BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if line.contains("seeking to") {
                let _ = tx.send(());
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    if rx.recv_timeout(Duration::from_secs(60)).is_err() {
        let o = child.wait_with_output().unwrap();
        panic!(
            "no seek logged ({:?}): {}",
            o.status.code(),
            log.join().unwrap()
        );
    }
    // SIGINT is signal 2: bit 1 of the masks.
    let status = std::fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap();
    let mask = |key: &str| {
        let line = status.lines().find(|l| l.starts_with(key)).unwrap();
        u64::from_str_radix(line[key.len()..].trim(), 16).unwrap()
    };
    let state = status.lines().find(|l| l.starts_with("State:")).unwrap();
    assert!(
        !state.contains('Z'),
        "the run ended too soon to look at: {state}"
    );
    assert_eq!(mask("SigIgn:") & 2, 2, "{status}");
    assert_eq!(mask("SigCgt:") & 2, 0, "{status}");
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let o = child.wait_with_output().unwrap();
    let log = log.join().unwrap();
    assert_eq!(o.status.code(), Some(0), "{log}");
    let v = document(&o.stdout);
    assert_eq!(v["ok"], true, "{v}");
    assert!(out.exists());
    assert!(!log.contains("interrupted"), "{log}");
    std::fs::remove_dir_all(&dir).ok();
}

/// The `rt_sigaction` calls for SIGINT that `args` makes, under strace.
fn sigint_actions(dir: &Path, name: &str, args: &[&OsStr]) -> Vec<String> {
    let log = dir.join(format!("{name}.strace"));
    let out = common::strace(&log, "rt_sigaction")
        .arg(env!("CARGO_BIN_EXE_spherecut"))
        .args(args)
        .env_remove("SPHERECUT_ADAPTER")
        .output()
        .unwrap();
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "{name}: {}",
        text(&out.stderr)
    );
    std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|l| l.contains("rt_sigaction(SIGINT,"))
        .map(str::to_string)
        .collect()
}

/// probe and doctor keep SIGINT's default action, which ends them at once
/// (and they exit only 0, 1 or 2 otherwise); frame alone installs its
/// handler, once, with `SA_RESETHAND` so that a second Ctrl+C ends it
/// (D27, D28), after asking for the action it started with.
#[test]
fn only_frame_handles_sigint() {
    use std::os::unix::process::ExitStatusExt;
    let dir = tmp("sigint-default");
    let clip = sphere_clip(&dir);
    let out = dir.join("out.png");
    let probe = sigint_actions(&dir, "probe", &["probe".as_ref(), clip.as_os_str()]);
    assert_eq!(probe, Vec::<String>::new());
    let doctor = sigint_actions(&dir, "doctor", &["doctor".as_ref()]);
    assert_eq!(doctor, Vec::<String>::new());
    let frame = sigint_actions(
        &dir,
        "frame",
        &[
            "frame".as_ref(),
            clip.as_os_str(),
            "--adapter".as_ref(),
            "cpu".as_ref(),
            "-o".as_ref(),
            out.as_os_str(),
        ],
    );
    // It asks first, to leave an ignored SIGINT alone
    // (`an_ignored_sigint_stays_ignored`), then sets its handler.
    assert_eq!(frame.len(), 2, "{frame:#?}");
    assert!(
        frame[0].contains("rt_sigaction(SIGINT, NULL, "),
        "{frame:#?}"
    );
    assert!(frame[1].contains("SA_RESETHAND"), "{frame:#?}");
    assert!(frame[1].contains("SA_RESTART"), "{frame:#?}");
    assert!(!frame[1].contains("SIG_DFL") && !frame[1].contains("SIG_IGN"));

    // A probe blocked writing its document to a pipe no one reads ends at
    // the first SIGINT, killed by it.
    // Twice what fills a pipe (64 KiB).
    let probe_json_len = |n: usize| {
        let mut args = vec![OsStr::new("probe"), OsStr::new("--json")];
        args.extend(std::iter::repeat_n(clip.as_os_str(), n));
        spherecut(&args).stdout.len()
    };
    let per_file = probe_json_len(2) - probe_json_len(1);
    let copies = 2 * 65_536 / per_file;
    let mut child = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .args(std::iter::repeat_n(&clip, copies))
        .env_remove("SPHERECUT_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let wchan = format!("/proc/{}/wchan", child.id());
    let started = Instant::now();
    while !std::fs::read_to_string(&wchan).is_ok_and(|w| w.contains("pipe_write")) {
        assert!(child.try_wait().unwrap().is_none(), "probe ended unblocked");
        assert!(
            started.elapsed().as_secs() < 30,
            "probe never blocked on its stdout"
        );
        std::thread::yield_now();
    }
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let status = child.wait().unwrap();
    assert_eq!(status.signal(), Some(2), "{status:?}");
    std::fs::remove_dir_all(&dir).ok();
}
