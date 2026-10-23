//! Files whose names are not UTF-8, which Linux allows: they probe and
//! decode like any other. ffmpeg-next's own open functions panic on them.

#![allow(clippy::unwrap_used)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::FrameRate;
use spherecut_media::decode::VideoDecoder;
use spherecut_media::encode::{Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
use spherecut_media::probe;

/// Ten 256x128 frames of H.264, in a file whose name has a Latin-1 "é".
fn clip() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("non-utf8-names-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let settings = VideoSettings {
        encoder: "libx264".into(),
        width: 256,
        height: 128,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: Vec::new(),
        bt709_limited: true,
    };
    let mut w = Mp4Writer::create(&dir.join("clip.mp4"), &settings, None).unwrap();
    for i in 0..10 {
        let luma = vec![16 + 20 * i; 256 * 128];
        let chroma = vec![128; 128 * 64];
        w.push_video(&YuvPlanes::U8([luma, chroma.clone(), chroma]))
            .unwrap();
    }
    let path = dir.join(OsStr::from_bytes(b"caf\xe9.mp4"));
    std::fs::rename(w.finish().unwrap(), &path).unwrap();
    assert!(path.to_str().is_none());
    path
}

fn frames(mut d: VideoDecoder) -> usize {
    let mut n = 0;
    while d.next_frame().unwrap().is_some() {
        n += 1;
    }
    n
}

#[test]
fn a_file_whose_name_is_not_utf8_probes_and_decodes() {
    let path = clip();
    let r = probe::probe(&path).unwrap();
    assert!(r.damage.is_empty() && r.warnings.is_empty(), "{r:#?}");
    assert_eq!(r.streams.len(), 1);
    assert_eq!(r.streams[0].codec.as_deref(), Some("h264"));

    assert_eq!(frames(VideoDecoder::open(&path, None, None).unwrap()), 10);
    let cancel = Arc::new(AtomicBool::new(false));
    let d = VideoDecoder::open_cancellable(&path, None, None, cancel).unwrap();
    assert_eq!(frames(d), 10);
}
