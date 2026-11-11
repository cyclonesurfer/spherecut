//! The export's MP4 (M2 plan section 8.4, amendment 2 items 8, 28 and
//! 53): written in one last pass with the moov first and no video edit
//! list, and for 360 the V1 `uuid` under the video `trak` beside V2's
//! boxes, so that A1's `check` passes it for YouTube.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use common::{Scratch, clip, gpu, project, secs, with_audio};
use serde_json::{Value, json};
use spherecut_agent::Roots;
use spherecut_agent::rpc::{CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_core::brand::APP_NAME;
use spherecut_core::project::{Aspect, AudioChoice, Codec, Orientation, Output, Project, Range};
use spherecut_core::time::FrameRate;
use spherecut_export::check::{Expected, ExpectedAudio, check, check_demuxer_view};
use spherecut_export::{Export, ExportJob, Report, Stage};
use spherecut_media::Source;
use spherecut_media::mp4::{self, Mp4Tree};
use spherecut_media::probe::{self, DemuxerView, StreamKind};
use spherecut_media::spherical;
use spherecut_synth::Pattern;
use spherecut_synth::media::{Audio, Spec};
use spherecut_testkit::control::must_fail;

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

    // FFmpeg's own demuxer reads what was written (F123): the whole
    // sphere, mono, at no pose, ambiX audio, and no complaint.
    let want = Expected {
        codec: Codec::Hevc,
        size: (3840, 1920),
        frames: 15,
        duration: secs(1, 2),
        rate: FrameRate::FPS_30,
        sphere: true,
        audio: Some(ExpectedAudio {
            codec: "aac".into(),
            rate: 48_000,
            channels: 4,
            ambix: true,
        }),
    };
    let flag = || Arc::new(AtomicBool::new(false));
    let read = |path: &Path| probe::demuxer_view(Source::path(path), None).unwrap();
    let layout = |view: &DemuxerView| {
        let a = view.streams.iter().find(|s| s.kind == StreamKind::Audio);
        a.and_then(|a| a.channel_layout.clone())
    };
    let view = read(&out);
    let video_view = view.streams.iter().find(|s| s.kind == StreamKind::Video);
    let sphere = video_view
        .and_then(|v| v.spherical.clone())
        .expect("FFmpeg's sphere");
    let whole = spherical::Projection::Equirect {
        top: 0,
        bottom: 0,
        left: 0,
        right: 0,
    };
    assert_eq!(sphere.projection, Some(whole));
    assert_eq!(sphere.stereo, Some(spherical::StereoMode::Mono));
    assert_eq!(
        (sphere.pose_yaw, sphere.pose_pitch, sphere.pose_roll),
        (0.0, 0.0, 0.0)
    );
    assert_eq!(layout(&view).as_deref(), Some("ambisonic 1"));
    assert!(
        view.spherical_complaints().is_empty(),
        "{:?}",
        view.ffmpeg_log
    );
    check_demuxer_view(&view, &want).unwrap();
    // Control: the same reading, wanted flat, fails.
    let flat = Expected {
        sphere: false,
        ..want.clone()
    };
    let e = must_fail(
        "a 360 file's reading, wanted flat",
        check_demuxer_view(&view, &flat),
    );
    assert!(e.to_string().starts_with("check: ffmpeg_sphere: "), "{e}");

    // Copies FFmpeg reads otherwise, each with one box renamed `free` or
    // one field changed, and the check of FFmpeg's reading that names it
    // (`None`: FFmpeg reads it as written).
    let bytes = std::fs::read(&out).unwrap();
    let entry = |t: &mp4::Mp4Box| {
        t.path(&[b"mdia", b"minf", b"stbl", b"stsd"])
            .unwrap()
            .children[0]
            .clone()
    };
    let (ventry, aentry) = (entry(video), entry(audio));
    let st3d = ventry.child(b"st3d").unwrap();
    let sv3d = ventry.child(b"sv3d").unwrap();
    let svhd = sv3d.child(b"svhd").unwrap();
    let sa3d = aentry.child(b"SA3D").unwrap();
    let v1 = video
        .children
        .iter()
        .find(|b| b.uuid == Some(spherical::SPHERICAL_V1_UUID))
        .unwrap();
    let free = |b: &mp4::Mp4Box| (usize::try_from(b.offset).unwrap() + 4, &b"free"[..]);
    let at = |b: &mp4::Mp4Box| usize::try_from(b.offset + b.header_len).unwrap();
    let copy = |name: &str, edits: &[(usize, &[u8])]| {
        let mut f = bytes.clone();
        for &(o, new) in edits {
            f[o..o + new.len()].copy_from_slice(new);
        }
        let path = scratch.0.join(format!("{name}.mp4"));
        std::fs::write(&path, f).unwrap();
        path
    };
    let version_1 = &[1u8][..];
    for (name, edits, fails_at) in [
        ("v1-free", vec![free(v1)], None),
        ("sv3d-free", vec![free(sv3d)], None),
        (
            "sv3d-and-v1-free",
            vec![free(sv3d), free(v1)],
            Some("ffmpeg_sphere"),
        ),
        ("st3d-free", vec![free(st3d)], Some("ffmpeg_sphere")),
        ("sa3d-free", vec![free(sa3d)], Some("ffmpeg_ambix")),
        ("sa3d-v1", vec![(at(sa3d), version_1)], Some("ffmpeg_ambix")),
        (
            "sa3d-map",
            vec![
                (at(sa3d) + 12, &[0, 0, 0, 1][..]),
                (at(sa3d) + 16, &[0, 0, 0, 0][..]),
            ],
            Some("ffmpeg_ambix"),
        ),
        ("svhd-v1", vec![(at(svhd), version_1)], Some("ffmpeg_log")),
        (
            "svhd-v1-v1-free",
            vec![(at(svhd), version_1), free(v1)],
            Some("ffmpeg_sphere"),
        ),
    ] {
        let doctored = copy(name, &edits);
        let view = read(&doctored);
        let r = check_demuxer_view(&view, &want);
        match fails_at {
            None => {
                r.unwrap_or_else(|e| panic!("{name}: {e}"));
            }
            Some(check) => {
                let e = must_fail(name, r).to_string();
                assert!(e.starts_with(&format!("check: {check}: ")), "{name}: {e}");
            }
        }
        // Our own parser's checks name what FFmpeg ignores in `SA3D` too
        // (F124).
        if name.starts_with("sa3d-v") || name == "sa3d-map" {
            let e = must_fail(name, check(&doctored, &want, flag())).to_string();
            assert!(e.starts_with("check: sa3d: "), "{name}: {e}");
        }
        let log = view.ffmpeg_log.join("\n");
        match name {
            "sa3d-free" | "sa3d-map" => {
                assert_ne!(layout(&view).as_deref(), Some("ambisonic 1"), "{name}");
            }
            "sa3d-v1" => assert!(log.contains("Unsupported SA3D box version 1"), "{log}"),
            "svhd-v1" => assert!(log.contains("Unknown spherical version 1"), "{log}"),
            _ => {}
        }
    }

    let names: Vec<_> = report.checks.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "probe",
            "layout",
            "codec",
            "size",
            "frames",
            "duration",
            "rate",
            "st3d",
            "sv3d",
            "v1",
            "audio",
            "sa3d",
            "ffmpeg_sphere",
            "ffmpeg_ambix",
            "ffmpeg_log"
        ]
    );
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
    let (report, _) = export(&p, &out);
    let names: Vec<_> = report.checks.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "probe",
            "layout",
            "codec",
            "size",
            "frames",
            "duration",
            "rate",
            "spherical",
            "audio",
            "ffmpeg_sphere",
            "ffmpeg_log"
        ]
    );
    let found = |name| {
        let c = report.checks.iter().find(|c| c.name == name).unwrap();
        c.found.clone()
    };
    assert_eq!(found("frames"), "15 frames");
    assert_eq!(found("size"), "1920x1080");
    assert_eq!(found("audio"), "aac, 48000 Hz, 2 channels");
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

    // The same file against what it isn't fails the check that says so.
    let want = Expected {
        codec: Codec::H264,
        size: (1920, 1080),
        frames: 15,
        duration: secs(1, 2),
        rate: FrameRate::FPS_30,
        sphere: false,
        audio: Some(ExpectedAudio {
            codec: "aac".into(),
            rate: 48_000,
            channels: 2,
            ambix: false,
        }),
    };
    let flag = || Arc::new(AtomicBool::new(false));
    assert_eq!(check(&out, &want, flag()).unwrap(), report.checks);
    let fails = |want: Expected, name: &str| {
        let e = check(&out, &want, flag()).unwrap_err();
        assert_eq!(e.stage(), Some(Stage::Check), "{e}");
        assert!(
            e.to_string().starts_with(&format!("check: {name}: ")),
            "{e}"
        );
    };
    fails(
        Expected {
            frames: 16,
            ..want.clone()
        },
        "frames",
    );
    fails(
        Expected {
            codec: Codec::Hevc,
            ..want.clone()
        },
        "codec",
    );
    fails(
        Expected {
            duration: secs(2, 3),
            ..want.clone()
        },
        "duration",
    );
    fails(
        Expected {
            sphere: true,
            ..want.clone()
        },
        "st3d",
    );
    fails(
        Expected {
            audio: None,
            ..want.clone()
        },
        "audio",
    );

    // FFmpeg's demuxer reads no sphere in it (F123); the control: the
    // same reading, wanted 360, fails.
    let view = probe::demuxer_view(Source::path(&out), None).unwrap();
    check_demuxer_view(&view, &want).unwrap();
    let sphere = Expected {
        sphere: true,
        ..want.clone()
    };
    let e = must_fail(
        "a flat file's reading, wanted 360",
        check_demuxer_view(&view, &sphere),
    );
    assert!(e.to_string().starts_with("check: ffmpeg_sphere: "), "{e}");
}
