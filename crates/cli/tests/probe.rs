//! `spherecut probe` on generated good and broken files: exit codes and
//! what it says.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use spherecut_core::FrameRate;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, Codec, Spec};

mod document;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("spherecut-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn probe(args: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .arg("probe")
        .args(args)
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A small spherical H.264 file with stereo audio, and the broken files
/// derived from it.
fn media(dir: &Path) -> (PathBuf, Vec<PathBuf>) {
    let spec = Spec {
        name: "good".into(),
        pattern: Pattern::Grid,
        width: 512,
        height: 256,
        rate: FrameRate::FPS_25,
        frames: 5,
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

#[test]
fn good_files_pass_and_bad_files_fail_with_a_reason() {
    let dir = tmp("files");
    let (good, bad) = media(&dir);

    let out = probe(&[&good]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("#0  video   h264"), "{stdout}");
    assert!(stdout.contains("spherical v2: equirect"), "{stdout}");
    assert!(
        !stdout.contains("DAMAGE") && !stderr.contains("error"),
        "{stdout}{stderr}"
    );

    // (file, how it fails, why)
    let expect = [
        ("bad-empty.mp4", "cannot open", "the file is empty"),
        ("bad-text.mp4", "cannot open", "not a recognised media file"),
        ("bad-trunc-header.mp4", "cannot open", "it was cut short"),
        ("bad-trunc-half.mp4", "cannot open", "it was cut short"),
        ("bad-trunc-end.mp4", "is damaged", "it was cut short"),
        (
            "bad-corrupt-moov.mp4",
            "is damaged",
            "runs past the end of the file",
        ),
        (
            "bad-trak-type.mp4",
            "is damaged",
            "box `\\xd1rak` at offset",
        ),
        (
            "bad-trak-renamed.mp4",
            "is damaged",
            "in `moov` starts with a track header (`tkhd`), so it is a `trak` whose type is damaged and FFmpeg skips its stream",
        ),
        (
            "bad-timescale.mp4",
            "is damaged",
            "track #0: its `mdhd` gives timescale 0, so its times are wrong",
        ),
        (
            "bad-codec-config.mp4",
            "is damaged",
            "stream #0 (sample entry `avc1`) has no pixel format, so its codec setup is damaged",
        ),
        (
            "bad-audio-config.mp4",
            "is damaged",
            "stream #1 (sample entry `mp4a`) cannot be opened by FFmpeg's aac decoder (Invalid data found when processing input), so its codec setup is damaged",
        ),
        (
            "bad-projection.mp4",
            "is damaged",
            "in `avc1` has no `proj`, so FFmpeg ignores its projection",
        ),
    ];
    assert_eq!(bad.len(), expect.len());
    for (name, how, why) in expect {
        let out = probe(&[&dir.join(name)]);
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(out.status.code(), Some(1), "{name}: {stdout}{stderr}");
        let line = stderr
            .lines()
            .find(|l| l.starts_with("error: "))
            .unwrap_or("");
        assert!(line.contains(how) && line.contains(why), "{name}: {stderr}");
        if how == "is damaged" {
            // The report is still printed, with the damage in it.
            assert!(stdout.contains("#0  video"), "{name}: {stdout}");
            assert!(stdout.contains("  DAMAGE  "), "{name}: {stdout}");
        }
    }

    // One bad file fails the run; the others are still reported.
    let corrupt = dir.join("bad-corrupt-moov.mp4");
    let out = probe(&[&good, &corrupt, &dir.join("bad-empty.mp4")]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout.contains(&good.display().to_string()), "{stdout}");
    assert!(stdout.contains(&corrupt.display().to_string()), "{stdout}");

    // JSON carries the damage list, each with its code, and the error.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    let out = cmd
        .args(["probe", "--json"])
        .arg(&corrupt)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v = document::document(&out.stdout);
    let file = &v["result"]["files"][0];
    let damage = file["report"]["damage"].as_array().unwrap();
    assert!(
        damage[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("track #0: chunk 1"),
        "{v}"
    );
    assert_eq!(damage[0]["code"], "damaged", "{v}");
    assert_eq!(file["error"]["code"], "damaged", "{v}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    let out = cmd.args(["probe", "--json"]).arg(&good).output().unwrap();
    assert!(out.status.success());
    let v = document::document(&out.stdout);
    assert_eq!(
        v["result"]["files"][0]["report"]["damage"],
        serde_json::json!([])
    );

    // FFmpeg's own messages are shown in the report, though the log hides
    // them by default.
    let timescale = dir.join("bad-timescale.mp4");
    let out = probe(&[&timescale]);
    let stdout = text(&out.stdout);
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("  ffmpeg     [") && l.contains("] Invalid mdhd time scale 0")),
        "{stdout}"
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    let out = cmd
        .args(["probe", "--json"])
        .arg(&timescale)
        .output()
        .unwrap();
    let v = document::document(&out.stdout);
    let log = v["result"]["files"][0]["report"]["ffmpeg_log"]
        .as_array()
        .unwrap();
    assert!(
        log.iter()
            .any(|l| l.as_str().unwrap().contains("] Invalid mdhd time scale 0")),
        "{v}"
    );

    // A directory is not a media file.
    let out = probe(&[&dir]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("is not a regular file"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ffmpegs_log_is_kept_for_a_file_it_cannot_open() {
    // FFmpeg's own reason is printed under ours, and goes in the JSON.
    let dir = tmp("cannot-open");
    let (_, bad) = media(&dir);
    let half = bad
        .iter()
        .find(|b| b.ends_with("bad-trunc-half.mp4"))
        .unwrap();
    let moov = "  ffmpeg     [mov,mp4,m4a,3gp,3g2,mj2] moov atom not found";

    let out = probe(&[half]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("error: cannot open"));
    assert!(
        at.is_some_and(|i| lines[i + 1..].contains(&moov)),
        "{stderr}"
    );

    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .arg(half)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v = document::document(&out.stdout);
    let file = &v["result"]["files"][0];
    assert!(file.get("report").is_none(), "{v}");
    let error = &file["error"];
    assert!(
        error["message"].as_str().unwrap().contains("cannot open"),
        "{v}"
    );
    // Cut in half, it has no `moov` left, but probe's own checks say why.
    assert_eq!(error["code"], "truncated", "{v}");
    let log = error["ffmpeg_log"].as_array().unwrap();
    assert!(
        log.iter()
            .any(|l| l.as_str().unwrap().ends_with("moov atom not found")),
        "{v}"
    );
    // A file we refuse before FFmpeg reads it has no log.
    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .arg(dir.join("bad-empty.mp4"))
        .output()
        .unwrap();
    let v = document::document(&out.stdout);
    let error = &v["result"]["files"][0]["error"];
    assert_eq!(error["code"], "not_media", "{v}");
    assert!(error.get("ffmpeg_log").is_none(), "{v}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn file_names_that_are_not_utf8_are_reported() {
    // Linux file names are bytes. These two fail before FFmpeg opens them;
    // the error used to panic, and stop the files after it.
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = tmp("utf8");
    let (good, _) = media(&dir);
    let empty = dir.join(OsStr::from_bytes(b"empty-\xff.mp4"));
    std::fs::write(&empty, b"").unwrap();
    let missing = dir.join(OsStr::from_bytes(b"missing-\xfe.mp4"));

    let out = probe(&[&empty, &missing, &good]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("empty-\u{fffd}.mp4: the file is empty"),
        "{stderr}"
    );
    assert!(
        stderr.contains("missing-\u{fffd}.mp4: No such file"),
        "{stderr}"
    );
    assert!(stdout.contains(&good.display().to_string()), "{stdout}");
    assert!(stdout.contains("#0  video   h264"), "{stdout}");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    let out = cmd
        .args(["probe", "--json"])
        .args([&empty, &missing, &good])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let v = document::document(&out.stdout);
    let files = &v["result"]["files"];
    let path = |i: usize| files[i]["path"].as_str().unwrap().to_string();
    assert!(path(0).ends_with("empty-\u{fffd}.mp4"), "{v}");
    assert!(
        files[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("the file is empty")
    );
    assert_eq!(files[0]["error"]["path"], path(0).as_str());
    assert!(path(1).ends_with("missing-\u{fffd}.mp4"), "{v}");
    assert_eq!(files[1]["error"]["code"], "not_found", "{v}");
    assert_eq!(path(2), good.display().to_string());
    assert_eq!(files[2]["report"]["damage"], serde_json::json!([]));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn files_named_in_bytes_that_are_not_utf8_are_probed() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = tmp("utf8-report");
    let (good, _) = media(&dir);
    let named = dir.join(OsStr::from_bytes(b"clip-\xff.mp4"));
    std::fs::copy(&good, &named).unwrap();

    let out = probe(&[&named]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("clip-\u{fffd}.mp4"), "{stdout}");
    assert!(stdout.contains("#0  video   h264"), "{stdout}");

    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .arg(&named)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v = document::document(&out.stdout);
    let file = &v["result"]["files"][0];
    for path in [&file["path"], &file["report"]["path"]] {
        assert!(path.as_str().unwrap().ends_with("clip-\u{fffd}.mp4"), "{v}");
    }
    assert_eq!(file["report"]["damage"], serde_json::json!([]));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_reader_that_goes_away_is_not_an_error() {
    // `spherecut probe *.mp4 | head`: the files after the reader leaves
    // are still probed, so a broken one still fails the run.
    use std::process::Stdio;
    let dir = tmp("pipe");
    let (good, bad) = media(&dir);
    let mut args: Vec<&Path> = vec![&good; 50];
    args.push(&bad[0]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .arg("probe")
        .args(&args)
        .env_remove("SPHERECUT_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let stderr = text(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.contains("stdout"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let name = bad[0].file_name().unwrap().to_string_lossy();
    assert!(stderr.contains(&*name), "{stderr}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_closed_stderr_keeps_the_exit_code() {
    // `spherecut probe *.mp4 2>&1 | head`: the errors have nowhere to go,
    // but the run still fails, rather than panicking. With logging on, the
    // log has nowhere to go either.
    use std::process::Stdio;
    let dir = tmp("stderr-pipe");
    let (good, bad) = media(&dir);
    let mut args: Vec<&Path> = vec![&good; 40];
    args.extend(bad.iter().map(PathBuf::as_path));
    for log in ["", "debug"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_spherecut"))
            .arg("probe")
            .args(&args)
            .env("SPHERECUT_LOG", log)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stderr.take());
        assert_eq!(child.wait().unwrap().code(), Some(1), "SPHERECUT_LOG={log}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_codec_ffmpeg_does_not_know_is_a_warning() {
    // A sample entry type FFmpeg has no codec for may be a real codec it
    // lacks, so the file is not called damaged, but the report says the
    // stream cannot be decoded.
    let dir = tmp("unknown-codec");
    let (good, _) = media(&dir);
    let mut f = std::fs::read(&good).unwrap();
    // The first entry's type follows the version, flags, entry count and
    // entry size.
    let at = f.windows(4).position(|w| w == b"stsd").unwrap() + 16;
    assert_eq!(&f[at..at + 4], b"avc1");
    f[at..at + 4].copy_from_slice(b"zzzz");
    let path = dir.join("zzzz.mp4");
    std::fs::write(&path, &f).unwrap();

    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "  warning    stream #0 (sample entry `zzzz`) has no codec FFmpeg knows and cannot be decoded"
        ),
        "{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_audio_setup_ffmpeg_does_not_implement_is_a_warning() {
    // AAC object type 0: FFmpeg sets the stream up from the sample entry,
    // but its decoder says the object type is not implemented. That may be
    // a feature this FFmpeg lacks, so it is a warning, not damage.
    let dir = tmp("aot0");
    let (good, _) = media(&dir);
    let mut f = std::fs::read(&good).unwrap();
    // The AAC setup starts where bad-audio-config.mp4 differs.
    let bad = std::fs::read(dir.join("bad-audio-config.mp4")).unwrap();
    let at = f.iter().zip(&bad).position(|(a, b)| a != b).unwrap();
    f[at] &= 0x07;
    let path = dir.join("aot0.mp4");
    std::fs::write(&path, &f).unwrap();

    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "  warning    stream #1 (sample entry `mp4a`) uses a feature FFmpeg's aac decoder lacks (Function not implemented) and cannot be decoded"
        ),
        "{stdout}"
    );
    // FFmpeg logs that in two calls, and the report has it as one line.
    assert!(
        stdout
            .lines()
            .any(|l| l
                .starts_with("  ffmpeg     [aac] Audio object type 0 is not implemented. Update")),
        "{stdout}"
    );
    assert!(
        stdout
            .lines()
            .filter(|l| l.starts_with("  ffmpeg"))
            .all(|l| l.starts_with("  ffmpeg     [")),
        "{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_empty_track_ffmpeg_cannot_set_up_is_a_warning() {
    // bad-codec-config.mp4's video track, with its damaged `avcC`, emptied
    // of samples: with no samples FFmpeg may have had too little to set
    // the stream up from, so it is not called damaged.
    let dir = tmp("empty-track");
    media(&dir);
    let mut f = std::fs::read(dir.join("bad-codec-config.mp4")).unwrap();
    let tree = spherecut_media::mp4::read(&mut std::io::Cursor::new(&f)).unwrap();
    let stbl = tree
        .tracks()
        .next()
        .and_then(|t| t.path(&[b"mdia", b"minf", b"stbl"]))
        .unwrap();
    // Each table's count, after its version and flags (and in `stsz`, the
    // size all samples share).
    for (t, at) in [(b"stts", 4), (b"stsz", 8), (b"stsc", 4), (b"stco", 4)] {
        let b = stbl.child(t).unwrap();
        let at = (b.offset + b.header_len) as usize + at;
        f[at..at + 4].fill(0);
    }
    let path = dir.join("empty-track.mp4");
    std::fs::write(&path, &f).unwrap();

    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "  warning    stream #0 (sample entry `avc1`) has no pixel format and cannot be decoded"
        ),
        "{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_picture_over_max_pixels_is_a_warning_that_names_the_limit() {
    // Spherecut's decoders stop at 2^27 pixels (D24). FFmpeg's own
    // account of such a stream is a decoder that fails to open, or no
    // frame size at all, which would read as damage.
    let dir = tmp("over-max-pixels");
    let path = media::write_mjpeg_declaring(&dir, "over.mp4", 16400, 8200).unwrap();
    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stdout.contains("  #0  video   mjpeg [mp4v] Baseline, 16400x8200,"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "  warning    stream #0 (sample entry `mp4v`) is 16400x8200, more than the 2^27 pixels (16384x8192) per picture that Spherecut decodes"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("DAMAGE"), "{stdout}");

    // Fewer than 2^27 pixels, but FFmpeg's decoders pad the rows to
    // 16384 before they check.
    let path = media::write_mjpeg_declaring(&dir, "padded.mp4", 16380, 8194).unwrap();
    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "  warning    stream #0 (sample entry `mp4v`) is 16380x8194, which FFmpeg may pad to 16384x8194, more than the 2^27 pixels (16384x8192) per picture that Spherecut decodes"
        ),
        "{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn deeply_nested_boxes_fail_fast() {
    // A `moov` holding 200 000 nested `udta` boxes once took the parser
    // past 2.8 GB of memory and a stack overflow.
    let dir = tmp("nested");
    let n = 200_000usize;
    let mut f = Vec::with_capacity(24 + 8 * (n + 1));
    f.extend_from_slice(&24u32.to_be_bytes());
    f.extend_from_slice(b"ftypisom\0\0\x02\0isomiso2");
    for (i, t) in std::iter::once(b"moov")
        .chain(std::iter::repeat_n(b"udta", n))
        .enumerate()
    {
        f.extend_from_slice(&(8 * (n + 1 - i) as u32).to_be_bytes());
        f.extend_from_slice(t);
    }
    let path = dir.join("nested.mp4");
    std::fs::write(&path, &f).unwrap();
    let start = Instant::now();
    let out = probe(&[&path]);
    let elapsed = start.elapsed();
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("nested more than 32 deep"), "{stderr}");
    assert!(elapsed < Duration::from_secs(10), "took {elapsed:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ffmpeg_and_trak_disagreeing_on_the_stream_kind_is_damage() {
    // FFmpeg takes a track's kind from the last `hdlr` it sees in the
    // `trak`. Turn the video track's `dinf` into a QuickTime-style data
    // handler of type `soun`: FFmpeg then reads the video as audio, while
    // `mdia/hdlr` still says `vide`.
    let dir = tmp("kind");
    let (good, _) = media(&dir);
    let mut f = std::fs::read(&good).unwrap();
    let at = f.windows(4).position(|w| w == b"dinf").unwrap() - 4;
    let size = u32::from_be_bytes(f[at..at + 4].try_into().unwrap()) as usize;
    f[at + 4..at + 8].copy_from_slice(b"hdlr");
    let mut handler = vec![0u8; size - 8];
    handler[4..12].copy_from_slice(b"mhlrsoun");
    f[at + 8..at + size].copy_from_slice(&handler);
    let path = dir.join("kind.mp4");
    std::fs::write(&path, &f).unwrap();

    let out = probe(&[&path]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("track #0 is video (handler `vide`, sample entry `avc1`) but FFmpeg reads stream #0 as audio"),
        "{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn handler_and_sample_entry_disagreeing_is_damage() {
    // The video track's handler changed from `vide` to `soun`. FFmpeg then
    // reads the H.264 track as an audio stream it cannot decode and agrees
    // with the handler; only the sample entry shows the damage.
    let dir = tmp("handler");
    let (good, _) = media(&dir);
    let mut f = std::fs::read(&good).unwrap();
    let at = f
        .windows(16)
        .position(|w| w.starts_with(b"hdlr") && w.ends_with(b"vide"))
        .unwrap()
        + 12;
    f[at..at + 4].copy_from_slice(b"soun");
    let path = dir.join("handler.mp4");
    std::fs::write(&path, &f).unwrap();

    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.contains("#0  audio"), "{stdout}");
    assert!(
        stderr.contains(
            "track #0: its handler says audio (`soun`), but its sample entry `avc1` is video"
        ),
        "{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn file_ending_inside_a_box_header_fails() {
    // A camera file cut just after `moov` keeps the first bytes of the
    // vendor box that followed: too few for a header, but a box size.
    let dir = tmp("cut-header");
    let (good, _) = media(&dir);
    let mut f = std::fs::read(&good).unwrap();
    f.extend_from_slice(&11_308_611u32.to_be_bytes());
    let path = dir.join("cut-header.mp4");
    std::fs::write(&path, &f).unwrap();

    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stdout.contains("truncated box header (4 of 8 bytes)"),
        "{stdout}"
    );
    assert!(
        stderr.contains("ends 4 bytes into the header of a 11308611-byte box"),
        "{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn damaged_first_box_does_not_hide_other_damage() {
    // One changed byte in the type of the first box: FFmpeg still reads
    // the file as MP4, and so does probe, so the rest is still checked.
    let dir = tmp("ftyq");
    let (good, _) = media(&dir);
    let ftyq = |name: &str| {
        let mut f = std::fs::read(dir.join(name)).unwrap();
        f[4..8].copy_from_slice(b"ftyq");
        let path = dir.join(format!("ftyq-{name}"));
        std::fs::write(&path, &f).unwrap();
        path
    };
    let warning = "its first box `ftyq` is not of a type MP4/MOV files start with";

    let out = probe(&[&ftyq(&good.file_name().unwrap().to_string_lossy())]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains(warning), "{stdout}");
    // Spherical metadata is read again too.
    assert!(stdout.contains("spherical v2: equirect"), "{stdout}");

    for (name, why) in [
        ("bad-trunc-end.mp4", "it was cut short"),
        ("bad-corrupt-moov.mp4", "runs past the end of the file"),
    ] {
        let out = probe(&[&ftyq(name)]);
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(out.status.code(), Some(1), "{name}: {stdout}{stderr}");
        assert!(stdout.contains(warning), "{name}: {stdout}");
        assert!(
            stderr.contains("is damaged") && stderr.contains(why),
            "{name}: {stderr}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A Matroska element with an 8-byte size, or the "unknown" size.
fn el(id: &[u8], payload: &[u8], known: bool) -> Vec<u8> {
    let size = if known {
        (payload.len() as u64 | 1 << 56).to_be_bytes()
    } else {
        [0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
    };
    [id, &size, payload].concat()
}

/// A small Matroska file with one 8 kHz PCM track in three clusters.
fn matroska(size_known: bool) -> Vec<u8> {
    let e = |id: &[u8], payload: &[u8]| el(id, payload, true);
    let header = e(
        b"\x1a\x45\xdf\xa3",
        &[e(b"\x42\x82", b"matroska"), e(b"\x42\x87", &[4])].concat(),
    );
    let info = e(
        b"\x15\x49\xa9\x66",
        &[
            e(b"\x2a\xd7\xb1", &[0x0f, 0x42, 0x40]),
            e(b"\x4d\x80", b"t"),
        ]
        .concat(),
    );
    let audio = [
        e(b"\xb5", &8000f64.to_be_bytes()),
        e(b"\x9f", &[1]),
        e(b"\x62\x64", &[16]),
    ]
    .concat();
    let track = [
        e(b"\xd7", &[1]),
        e(b"\x73\xc5", &[1]),
        e(b"\x83", &[2]),
        e(b"\x86", b"A_PCM/INT/LIT"),
        e(b"\xe1", &audio),
    ]
    .concat();
    let tracks = e(b"\x16\x54\xae\x6b", &e(b"\xae", &track));
    let clusters: Vec<u8> = (0..3u8)
        .flat_map(|t| {
            let block = [&b"\x81\x00\x00\x80"[..], &[0; 1600]].concat();
            e(
                b"\x1f\x43\xb6\x75",
                &[e(b"\xe7", &[t * 100]), e(b"\xa3", &block)].concat(),
            )
        })
        .collect();
    let segment = [info, tracks, clusters].concat();
    [header, el(b"\x18\x53\x80\x67", &segment, size_known)].concat()
}

#[test]
fn matroska_cut_short_fails() {
    let dir = tmp("mkv");
    let write = |name: &str, data: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    };
    let good = matroska(true);
    let checked = "structure  only its length is checked";

    let out = probe(&[&write("good.mkv", &good)]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("#0  audio   pcm_s16le"), "{stdout}");
    assert!(stdout.contains(checked), "{stdout}");

    // FFmpeg opens it cut to 60% without a word.
    let out = probe(&[&write("cut.mkv", &good[..good.len() * 3 / 5])]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.contains("#0  audio   pcm_s16le"), "{stdout}");
    assert!(
        stderr.contains("bytes its Matroska Segment says it has: it was cut short"),
        "{stderr}"
    );

    // Without a recorded size there is nothing to compare with, and the
    // report says so.
    let out = probe(&[&write("live.mkv", &matroska(false))]);
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains("structure  not checked (its Matroska Segment does not record its size"),
        "{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A Matroska file whose only track is a line of subtitles.
fn matroska_subtitles() -> Vec<u8> {
    let e = |id: &[u8], payload: &[u8]| el(id, payload, true);
    let header = e(
        b"\x1a\x45\xdf\xa3",
        &[e(b"\x42\x82", b"matroska"), e(b"\x42\x87", &[4])].concat(),
    );
    let info = e(
        b"\x15\x49\xa9\x66",
        &e(b"\x2a\xd7\xb1", &[0x0f, 0x42, 0x40]),
    );
    let track = [
        e(b"\xd7", &[1]),
        e(b"\x73\xc5", &[1]),
        // Track type 0x11: subtitles.
        e(b"\x83", &[0x11]),
        e(b"\x86", b"S_TEXT/UTF8"),
    ]
    .concat();
    let tracks = e(b"\x16\x54\xae\x6b", &e(b"\xae", &track));
    let block = [&b"\x81\x00\x00\x80"[..], b"not a video"].concat();
    let cluster = e(
        b"\x1f\x43\xb6\x75",
        &[e(b"\xe7", &[0]), e(b"\xa3", &block)].concat(),
    );
    let segment = [info, tracks, cluster].concat();
    [header, el(b"\x18\x53\x80\x67", &segment, true)].concat()
}

#[test]
fn files_without_video_or_audio_fail() {
    // A container Spherecut opens, holding only subtitles: FFmpeg reads
    // it, but there is nothing to show.
    let dir = tmp("no-av");
    let path = dir.join("subtitles.mkv");
    std::fs::write(&path, matroska_subtitles()).unwrap();
    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.contains("container  matroska,webm"), "{stdout}");
    assert!(stdout.contains("#0  subtitle"), "{stdout}");
    assert!(
        stderr.contains(
            "subtitles.mkv has no video or audio stream; FFmpeg reads it as Matroska / WebM"
        ),
        "{stderr}"
    );

    // FFmpeg reads a text file that starts like song lyrics as LRC, which
    // Spherecut refuses by name, without a report.
    let path = dir.join("lyrics.txt");
    std::fs::write(&path, b"[00:01.00]not a video\n").unwrap();
    let out = probe(&[&path]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("lyrics.txt as LRC lyrics (`lrc`), and Spherecut opens only MP4/MOV and Matroska/WebM files"),
        "{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_missing_file_says_why_once() {
    // In a directory that is not there either, so there is nothing to
    // remove afterwards.
    let missing = std::env::temp_dir()
        .join(format!("spherecut-cli-{}-missing", std::process::id()))
        .join("nothing-here.mp4");
    let out = probe(&[&missing]);
    assert!(!missing.parent().unwrap().exists());
    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    assert_eq!(stderr.matches("No such file").count(), 1, "{stderr}");
    assert!(stderr.contains("nothing-here.mp4"), "{stderr}");
}
