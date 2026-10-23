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

/// A small spherical H.264 file with stereo audio, and the six broken
/// files derived from it.
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

    // JSON carries the damage list.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    let out = cmd
        .args(["probe", "--json"])
        .arg(&corrupt)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let damage = v["damage"].as_array().unwrap();
    assert!(
        damage[0].as_str().unwrap().starts_with("track #0: chunk 1"),
        "{v}"
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    let out = cmd.args(["probe", "--json"]).arg(&good).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["damage"], serde_json::json!([]));

    // A directory is not a media file.
    let out = probe(&[&dir]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("is not a regular file"));
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
