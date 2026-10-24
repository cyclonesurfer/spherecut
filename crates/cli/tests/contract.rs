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
use spherecut_media::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings};
use spherecut_media::ff;
use spherecut_synth::media::{self, Audio, Codec, Spec};
use spherecut_synth::{Pattern, render_equirect, yuv};

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
    for schema in [contract::PROBE, contract::DOCTOR, contract::CLI] {
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
    assert_eq!(color(0), ["tv", "bt709", "bt709", "bt709", "left"]);
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
