//! The export's MP4 (M2 plan section 8.4, amendment 2 items 8, 28 and
//! 53): written in one last pass with the moov first and no video edit
//! list, and for 360 the V1 `uuid` under the video `trak` beside V2's
//! boxes, so that A1's `check` passes it for YouTube.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Scratch, clip, gpu, project, secs, with_audio};
use serde_json::{Value, json};
use spherecut_agent::Roots;
use spherecut_agent::rpc::{CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_core::brand::APP_NAME;
use spherecut_core::project::{Aspect, AudioChoice, Codec, Orientation, Output, Project, Range};
use spherecut_core::time::FrameRate;
use spherecut_export::{Export, ExportJob, Report};
use spherecut_media::mp4::{self, Mp4Tree};
use spherecut_media::spherical;
use spherecut_synth::Pattern;
use spherecut_synth::media::{Audio, Spec};

/// 2 s of the grid at 30 fps, 1024×512, with `audio`.
fn source(name: &str, audio: Audio) -> std::path::PathBuf {
    clip(&Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width: 1024,
        height: 512,
        rate: FrameRate::FPS_30,
        frames: 60,
        audio,
        spherical: true,
        ..Spec::default()
    })
}

/// The export of `project` to `out`, and the last pass's progress as it
/// ended.
fn export(project: &Project, out: &Path) -> (Report, Option<(u64, u64)>) {
    let job = ExportJob::new(project.clone(), out, gpu().clone());
    let mut e = Export::start(job).unwrap();
    let report = e.wait(Duration::from_secs(1800)).expect("the export ends");
    (report.unwrap(), e.progress().finishing)
}

/// The top-level boxes' types, in order.
fn top(tree: &Mp4Tree) -> Vec<String> {
    tree.top.iter().map(|b| b.fourcc.to_string()).collect()
}

/// A1's `check` of `file` for `target`, which must pass.
fn passes(dir: &Path, file: &Path, target: &str) {
    let tools = Spherecut::new(Roots::new(&[dir.to_path_buf()], None).unwrap());
    let cx = CallCx::standalone(Instant::now() + Duration::from_secs(50));
    let args = json!({"file": file, "target": target});
    let r: Value = tools.call("check", args, &cx).unwrap().result;
    assert_eq!(r["pass"], true, "{target}: {r:#}");
}

#[test]
fn a_360_export_has_the_moov_first_v1_and_v2_and_no_video_edit_list() {
    let src = source("mp4-ambix", Audio::AmbiXTones);
    let scratch = Scratch::new("mp4-360");
    let trim = Range {
        start: secs(1, 4),
        end: secs(3, 4),
    };
    let sphere = Output::Equirect {
        width: 3840,
        orientation: Orientation::IDENTITY,
    };
    let mut p = project(&src, trim, sphere, Codec::Hevc, 0.0);
    with_audio(&mut p, AudioChoice::Copy);
    let out = scratch.0.join("sphere.mp4");
    let (report, finishing) = export(&p, &out);
    assert_eq!(finishing, Some((report.bytes, report.bytes)));

    let tree = mp4::read_file(&out).unwrap();
    assert!(tree.damage().is_empty(), "{:?}", tree.damage());
    assert_eq!(top(&tree), ["ftyp", "free", "moov", "mdat"]);
    let tracks: Vec<_> = tree.tracks().collect();
    let kind = |t: &mp4::Mp4Box| mp4::handler_type(t).map(|h| h.0);
    let video = tracks.iter().find(|t| kind(t) == Some(*b"vide")).unwrap();
    let audio = tracks.iter().find(|t| kind(t) == Some(*b"soun")).unwrap();
    // The video starts at 0 with no edit list; the audio keeps the one
    // that trims it.
    assert!(video.child(b"edts").is_none());
    assert_eq!(mp4::edit_list(audio).map(|e| e.len()), Some(1));

    let v = spherical::read_track(video);
    let v1 = v.v1.expect("V1");
    assert!(v1.spherical && v1.stitched, "{v1:?}");
    assert_eq!(v1.projection_type.as_deref(), Some("equirectangular"));
    assert_eq!(v1.stitching_software.as_deref(), Some(APP_NAME));
    let v2 = v.v2.expect("V2");
    assert_eq!(v2.stereo, Some(spherical::StereoMode::Mono));
    assert!(spherical::read_track(audio).spatial_audio.is_some());

    passes(&scratch.0, &out, "youtube_360");
}

#[test]
fn a_flat_export_has_the_moov_first_and_no_spherical_metadata() {
    let src = source("mp4-stereo", Audio::Stereo);
    let scratch = Scratch::new("mp4-flat");
    let trim = Range {
        start: secs(1, 4),
        end: secs(3, 4),
    };
    let flat = Output::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };
    let mut p = project(&src, trim, flat, Codec::H264, 0.0);
    with_audio(&mut p, AudioChoice::Aac);
    let out = scratch.0.join("flat.mp4");
    export(&p, &out);
    let tree = mp4::read_file(&out).unwrap();
    assert!(tree.damage().is_empty(), "{:?}", tree.damage());
    assert_eq!(top(&tree), ["ftyp", "free", "moov", "mdat"]);
    for t in tree.tracks() {
        let s = spherical::read_track(t);
        assert!(s.v1.is_none() && s.v2.is_none() && s.spatial_audio.is_none());
        if mp4::handler_type(t).is_some_and(|h| h.0 == *b"vide") {
            assert!(t.child(b"edts").is_none());
        }
    }
    passes(&scratch.0, &out, "flat_16_9");
}
