//! A file opened by one path and named by another (`Source`, a1-plan
//! amendment 10): probe reads the one and reports the other, in its
//! report and in its errors.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::Source;
use spherecut_media::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
use spherecut_media::probe;

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn temp_dir(name: &str) -> TempDir {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Three 128x64 frames of H.264 at `path`.
fn clip(path: &Path) {
    let settings = VideoSettings {
        encoder: "libx264".into(),
        width: 128,
        height: 64,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: Vec::new(),
        color: ColorTags::BT709_LIMITED,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let mut w = Mp4Writer::create(&path.with_extension("tmp.mp4"), &settings, None).unwrap();
    for _ in 0..3 {
        let chroma = vec![128; 64 * 32];
        w.push_video(&YuvPlanes::U8([vec![80; 128 * 64], chroma.clone(), chroma]))
            .unwrap();
    }
    std::fs::rename(w.finish().unwrap(), path).unwrap();
}

#[test]
fn probe_reads_the_open_path_and_reports_the_name() {
    let dir = temp_dir("source-name");
    let open = dir.0.join("opened-without-an-extension");
    clip(&open);
    let name = Path::new("trips/day one/VID_0001.mp4");
    let r = probe::probe_source(Source { open: &open, name }).unwrap();
    assert_eq!(r.path, name);
    assert_eq!(r.streams.len(), 1);
    assert!(r.damage.is_empty(), "{r:#?}");
}

#[test]
fn probe_s_errors_carry_the_name() {
    let dir = temp_dir("source-errors");
    let name = Path::new("named.mp4");
    let empty = dir.0.join("empty");
    std::fs::write(&empty, b"").unwrap();
    let missing = dir.0.join("missing");
    for open in [&empty, &missing, &dir.0] {
        let e = probe::probe_source(Source { open, name }).unwrap_err();
        let text = e.to_string();
        assert!(text.contains("named.mp4"), "{text}");
        assert!(!text.contains(&*dir.0.to_string_lossy()), "{text}");
    }
}
