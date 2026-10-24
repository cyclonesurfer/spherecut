//! `VideoDecoder` and `probe` let FFmpeg read only the file they were
//! given (DECISIONS D24): a playlist is refused by name, and a path is
//! never a URL. Names FFmpeg would read as protocols are only a danger
//! without a directory (`concat:x.mp4`, not `/tmp/concat:x.mp4`), so
//! `crates/cli/tests/confinement.rs` tests those, from the directory that
//! holds them.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::MediaError;
use spherecut_media::decode::VideoDecoder;
use spherecut_media::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};

fn tmp(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("confinement-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Two frames of `width` x 64 H.264, at `path`.
fn clip(path: &Path, width: u32) {
    let settings = VideoSettings {
        encoder: "libx264".into(),
        width,
        height: 64,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: Vec::new(),
        color: ColorTags::BT709_LIMITED,
        edit_start: 0,
        time_base: None,
    };
    let w = width as usize;
    let mut out = Mp4Writer::create(path, &settings, None).unwrap();
    for _ in 0..2 {
        let chroma = vec![128; w * 16];
        out.push_video(&YuvPlanes::U8([vec![100; w * 64], chroma.clone(), chroma]))
            .unwrap();
    }
    out.finish().unwrap();
}

#[test]
fn a_playlist_is_refused_by_name() {
    let dir = tmp("playlist");
    clip(&dir.join("private.mp4"), 64);
    std::fs::create_dir_all(dir.join("inside")).unwrap();
    let playlist = dir.join("inside/list.m3u8");
    std::fs::write(
        &playlist,
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\n../private.mp4\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    let refused = |e: &MediaError| {
        matches!(e, MediaError::UnsupportedContainer { format, description, .. }
            if format == "hls" && description == "Apple HTTP Live Streaming")
    };
    let e = VideoDecoder::open(&playlist, None, None).unwrap_err();
    assert!(refused(&e), "{e:?}");
    let e = spherecut_media::probe::probe(&playlist).unwrap_err();
    assert!(refused(&e.error), "{e:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_path_is_never_a_url() {
    let dir = tmp("paths");
    // Was a connection to port 1 before FFmpeg was confined.
    let e = VideoDecoder::open(Path::new("http://127.0.0.1:1/x.mp4"), None, None).unwrap_err();
    assert!(
        matches!(&e, MediaError::Io { error, .. } if error.kind() == std::io::ErrorKind::NotFound),
        "{e:?}"
    );
    let e = VideoDecoder::open(&dir, None, None).unwrap_err();
    assert!(matches!(e, MediaError::NotAFile(_)), "{e:?}");
    std::fs::remove_dir_all(&dir).ok();
}
