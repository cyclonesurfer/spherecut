//! Everything that writes media: the synthetic clip writer, the MP4
//! encoder and the metadata injector, on real files.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::inject::{self, EntryEdit, InjectError, TrackKind};
use spherecut_media::spherical::write;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, Codec, Spec};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("spherecut-writers-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// File names in `dir`, sorted, hidden temp files included.
fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn spec(name: &str, width: u32, frames: u32, codec: Codec, bits: u32, audio: Audio) -> Spec {
    Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width,
        height: width / 2,
        rate: FrameRate::FPS_30,
        frames,
        codec,
        bits,
        audio,
        spherical: true,
    }
}

#[test]
fn inject_refuses_to_overwrite_its_own_input() {
    // This used to truncate the input and then copy it onto itself until
    // the disk was full. Every inject copy is now bounded by the size the
    // parser saw, so this test can't run away even if the check regresses.
    let dir = tmp("same");
    let clip = media::write(
        &Spec {
            spherical: false,
            ..spec("clip", 64, 2, Codec::H264, 8, Audio::Stereo)
        },
        &dir,
    )
    .unwrap();
    let before = std::fs::read(&clip).unwrap();
    let edits = [EntryEdit {
        track: TrackKind::Audio,
        boxes: write::sa3d_first_order_ambix(),
    }];
    match inject::inject(&clip, &clip, &edits) {
        Err(InjectError::SameFile { .. }) => {}
        r => panic!("{r:?}"),
    }
    assert!(std::fs::read(&clip).unwrap() == before, "input changed");
    assert_eq!(listing(&dir), ["clip.mp4"]);
    std::fs::remove_dir_all(&dir).ok();
}
