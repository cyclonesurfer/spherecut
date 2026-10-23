//! Everything that writes media: the synthetic clip writer, the MP4
//! encoder and the metadata injector, on real files.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::inject::{self, EntryEdit, InjectError, TrackKind};
use spherecut_media::probe;
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
fn write_leaves_only_complete_files() {
    let dir = tmp("atomic");
    let tagged = spec("tagged", 64, 2, Codec::H264, 8, Audio::Stereo);
    let untagged = Spec {
        spherical: false,
        ..spec("untagged", 64, 2, Codec::H264, 8, Audio::None)
    };
    for s in [&tagged, &untagged] {
        let p = media::write(s, &dir).unwrap();
        assert_eq!(p, dir.join(s.file_name()));
    }
    assert_eq!(listing(&dir), ["tagged.mp4", "untagged.mp4"]);

    // When the final rename fails (a directory is in the way), the error
    // comes back and no part file is left.
    let blocked = tmp("atomic-blocked");
    for s in [&tagged, &untagged] {
        std::fs::create_dir(blocked.join(s.file_name())).unwrap();
        let r = media::write(s, &blocked);
        assert!(r.is_err(), "{}: {r:?}", s.name);
    }
    assert_eq!(listing(&blocked), ["tagged.mp4", "untagged.mp4"]);
    assert!(blocked.join("tagged.mp4").is_dir());
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&blocked).ok();
}

#[test]
fn concurrent_writes_of_one_clip_all_succeed() {
    // Tier 2 tests share cached clips, and may generate one from several
    // threads at once.
    let dir = tmp("concurrent");
    let s = spec("shared", 64, 2, Codec::H264, 8, Audio::Stereo);
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| media::write(&s, &dir)))
            .collect();
        for w in writers {
            w.join().unwrap().unwrap();
        }
    });
    assert_eq!(listing(&dir), ["shared.mp4"]);
    let r = probe::probe(&dir.join("shared.mp4")).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    std::fs::remove_dir_all(&dir).ok();
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
