//! FFmpeg reads only the file `spherecut probe` was given (DECISIONS
//! D24): playlists, concat scripts and other containers are refused by
//! name, a file whose name looks like a URL or a protocol is read as that
//! file, and under strace nothing opens a network socket or the files a
//! playlist lists.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use spherecut_core::FrameRate;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, Codec, Spec};

mod common;

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("confinement-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `spherecut probe` on `args`, run in `dir`, so that relative names are
/// taken from there.
fn probe_in(dir: &Path, args: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .current_dir(dir)
        .arg("probe")
        .args(args)
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A two-frame H.264 clip of `width` x `height`, at `dir/name`.
fn clip(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
    let spec = Spec {
        name: format!("clip-{width}x{height}"),
        pattern: Pattern::Grid,
        width,
        height,
        rate: FrameRate::FPS_25,
        frames: 2,
        codec: Codec::H264,
        bits: 8,
        audio: Audio::None,
        spherical: false,
        ..Spec::default()
    };
    let made = media::write(&spec, dir).unwrap();
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::rename(made, &path).unwrap();
    path
}

/// One second of 8 kHz mono silence as a WAV file.
fn wav() -> Vec<u8> {
    let data = [0u8; 16_000];
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    // PCM, one channel, 8000 Hz, 16000 bytes/s, 2-byte frames, 16 bits.
    for (n, len) in [(1u32, 2), (1, 2), (8000, 4), (16_000, 4), (2, 2), (16, 2)] {
        v.extend_from_slice(&n.to_le_bytes()[..len]);
    }
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&data);
    v
}

/// The files FFmpeg would follow out of the file it was given, in `dir`,
/// with what `spherecut probe` must say about each: an HLS playlist whose
/// segment lies outside its folder, one with a segment on the network, a
/// concat script, and a WAV file, which FFmpeg reads but Spherecut
/// doesn't open. The files they list are in `outside/`, and are real.
fn hostile(dir: &Path) -> Vec<(&'static str, &'static str)> {
    clip(dir, "outside/private.mp4", 64, 32);
    std::fs::copy(
        dir.join("outside/private.mp4"),
        dir.join("outside/private.ts"),
    )
    .unwrap();
    let write = |name: &str, data: &[u8]| {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    };
    let playlist = |segment: &str| {
        format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:1.0,\n{segment}\n#EXT-X-ENDLIST\n"
        )
    };
    write(
        "inside/outside.m3u8",
        playlist("../outside/private.ts").as_bytes(),
    );
    write(
        "inside/remote.m3u8",
        playlist("http://127.0.0.1:1/segment.ts").as_bytes(),
    );
    write(
        "inside/list.ffconcat",
        b"ffconcat version 1.0\nfile '../outside/private.mp4'\n",
    );
    write("tone.wav", &wav());
    let hls = "Apple HTTP Live Streaming (`hls`)";
    vec![
        ("inside/outside.m3u8", hls),
        ("inside/remote.m3u8", hls),
        (
            "inside/list.ffconcat",
            "Virtual concatenation script (`concat`)",
        ),
        ("tone.wav", "WAV / WAVE (Waveform Audio) (`wav`)"),
    ]
}

/// Names FFmpeg would read as protocol URLs, each a real 320x160 clip, and
/// a 512x256 `x.mp4` that the `concat:` and `subfile` ones would read
/// instead. `http://127.0.0.1:1/x.mp4` is the path `http:/127.0.0.1:1/x.mp4`.
fn named_like_urls(dir: &Path) -> Vec<&'static str> {
    clip(dir, "x.mp4", 512, 256);
    let names = vec![
        "concat:x.mp4",
        "concat:x.mp4|x.mp4",
        "subfile,,start,0,end,0,,:x.mp4",
        "file:x.mp4",
        "a:b.mp4",
        "http://127.0.0.1:1/x.mp4",
    ];
    let first = clip(dir, names[0], 320, 160);
    for name in &names[1..] {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::copy(&first, p).unwrap();
    }
    names
}

#[test]
fn playlists_concat_scripts_and_other_containers_are_refused_by_name() {
    let dir = tmp("refused");
    for (name, container) in hostile(&dir) {
        let out = probe_in(&dir, &[name.as_ref()]);
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(out.status.code(), Some(1), "{name}: {stdout}{stderr}");
        assert!(stdout.is_empty(), "{name}: {stdout}");
        let want = format!(
            "error: FFmpeg reads {name} as {container}, and Spherecut opens only MP4/MOV and Matroska/WebM files"
        );
        assert!(stderr.contains(&want), "{name}: no {want:?} in {stderr}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_path_that_looks_like_a_url_is_a_path() {
    let dir = tmp("url");
    let url = "http://127.0.0.1:1/x.mp4";
    let out = probe_in(&dir, &[url.as_ref()]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("error: {url}: No such file or directory")),
        "{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn files_named_like_protocols_are_read_as_those_files() {
    let dir = tmp("colons");
    for name in named_like_urls(&dir) {
        let out = probe_in(&dir, &[name.as_ref()]);
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert!(out.status.success(), "{name}: {stdout}{stderr}");
        assert!(stdout.starts_with(&format!("{name}\n")), "{stdout}");
        assert!(
            stdout.contains("h264 [avc1] High, 320x160,"),
            "{name} was not read as itself: {stdout}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn probe_opens_no_network_socket_and_no_listed_file() {
    let dir = tmp("strace");
    let mut args: Vec<&str> = hostile(&dir).into_iter().map(|(name, _)| name).collect();
    args.extend(named_like_urls(&dir));
    args.push("http://127.0.0.1:1/missing.mp4");
    let log = dir.join("strace.log");
    let out = common::strace(&log, "socket,openat,?open")
        .current_dir(&dir)
        .arg(env!("CARGO_BIN_EXE_spherecut"))
        .arg("probe")
        .args(&args)
        .output()
        .unwrap();
    let log = std::fs::read_to_string(&log).unwrap();
    // Some files fail, the rest are probed.
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let opened = |name: &str| {
        log.lines()
            .any(|l| l.contains("open") && l.contains(&format!("\"{name}\"")))
    };
    // The trace saw probe open its inputs ...
    for name in [
        "inside/outside.m3u8",
        "inside/list.ffconcat",
        "concat:x.mp4",
    ] {
        assert!(opened(name), "strace saw no open of {name}:\n{log}");
    }
    // ... and nothing they list, and no network socket.
    let private: Vec<&str> = log.lines().filter(|l| l.contains("private")).collect();
    assert!(private.is_empty(), "{private:#?}");
    assert!(!opened("x.mp4"), "{log}");
    let sockets = common::inet_sockets(&log);
    assert!(sockets.is_empty(), "{sockets:#?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// The confinement table's last row (M1 plan section 12.1), for frame:
/// rendering a clip opens no network socket either.
#[test]
fn frame_opens_no_network_socket() {
    let dir = tmp("strace-frame");
    let spec = Spec {
        name: "clip".into(),
        pattern: Pattern::Grid,
        width: 256,
        height: 128,
        rate: FrameRate::FPS_25,
        frames: 2,
        codec: Codec::H264,
        bits: 8,
        audio: Audio::None,
        spherical: true,
        ..Spec::default()
    };
    media::write(&spec, &dir).unwrap();
    let log = dir.join("strace.log");
    let out = common::strace(&log, "socket,connect,openat")
        .current_dir(&dir)
        .arg(env!("CARGO_BIN_EXE_spherecut"))
        .args(["frame", "clip.mp4", "--adapter", "cpu", "-o", "out.png"])
        .env_remove("SPHERECUT_ADAPTER")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(dir.join("out.png").is_file());
    let log = std::fs::read_to_string(&log).unwrap();
    // The trace saw frame open its input ...
    assert!(log.contains("\"clip.mp4\""), "{log}");
    // ... and no network socket, nor a connection anywhere.
    let sockets = common::inet_sockets(&log);
    assert!(sockets.is_empty(), "{sockets:#?}");
    let connects: Vec<&str> = log.lines().filter(|l| l.contains("connect(")).collect();
    assert!(
        connects.iter().all(|l| l.contains("AF_UNIX")),
        "{connects:#?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
