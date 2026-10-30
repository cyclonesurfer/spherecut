//! `check` (a1-plan sections 6.4 and 11.2) on clips synth writes, with
//! boxes injected where a clip needs metadata that doesn't fit it: a 2:1
//! picture tagged top-bottom, SA3D on stereo audio, a cubemap, cropping
//! bounds, a Matroska remux, flat pictures for the flat targets, a cut
//! file, an `.insv` without its trailer, and the worst of these under
//! `CHECK_BYTES`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::limits::CHECK_BYTES;
use spherecut_agent::rpc::{CallCx, Tools};
use spherecut_agent::tools::Spherecut;
use spherecut_agent::{Roots, ToolError};
use spherecut_media::inject::{self, EntryEdit, TrackKind};
use spherecut_media::spherical::{Projection, StereoMode, write};
use spherecut_synth::media::{self, Audio, Codec, Spec};

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("check-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), None).unwrap())
    }

    /// A clip of `spec`, short and small unless it says otherwise.
    fn clip(&self, spec: Spec) -> PathBuf {
        media::write(&spec, &self.0).unwrap()
    }

    /// `src` with `video` and `audio` boxes added to its first such
    /// track's sample entry, as `name`.
    fn inject(&self, src: &Path, name: &str, video: Vec<u8>, audio: Vec<u8>) -> PathBuf {
        let out = self.0.join(name);
        let mut edits = Vec::new();
        if !video.is_empty() {
            edits.push(EntryEdit {
                track: TrackKind::Video,
                boxes: video,
            });
        }
        if !audio.is_empty() {
            edits.push(EntryEdit {
                track: TrackKind::Audio,
                boxes: audio,
            });
        }
        inject::inject(src, &out, &edits).unwrap();
        out
    }
}

fn spec(name: &str, width: u32, height: u32) -> Spec {
    Spec {
        name: name.into(),
        width,
        height,
        frames: 10,
        ..Spec::default()
    }
}

fn call(tools: &Spherecut, args: Value) -> Result<Value, ToolError> {
    let cx = CallCx::standalone(Instant::now() + Duration::from_secs(50));
    tools.call("check", args, &cx).map(|o| o.result)
}

fn check(tools: &Spherecut, file: &Path, target: Option<&str>) -> Value {
    let mut args = json!({"file": file});
    if let Some(t) = target {
        args["target"] = json!(t);
    }
    let r = call(tools, args).unwrap();
    assert!(r.to_string().len() <= CHECK_BYTES, "{r}");
    // `pass` is every rule's `ok`, and a failed rule has its fix.
    let rules = r["rules"].as_array().unwrap();
    assert_eq!(r["pass"], rules.iter().all(|x| x["ok"] == true), "{r}");
    for x in rules {
        assert_eq!(x["ok"] == false, x.get("fix").is_some(), "{x}");
        assert!(
            !x["message"].as_str().unwrap().contains(".mp4"),
            "no file names: {x}"
        );
    }
    r
}

fn rule<'a>(r: &'a Value, id: &str) -> &'a Value {
    r["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap_or_else(|| panic!("no rule {id}: {r}"))
}

fn ids(r: &Value) -> Vec<&str> {
    r["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap())
        .collect()
}

#[test]
fn a_tagged_ambix_clip_passes_youtube_360_and_archive() {
    let d = TempDir::new("ambix");
    let file = d.clip(Spec {
        audio: Audio::AmbiX {
            azimuth: 90,
            elevation: 0,
        },
        spherical: true,
        ..spec("t-ambix", 256, 128)
    });
    let tools = d.tools();
    let r = check(&tools, &file, Some("youtube_360"));
    assert_eq!(r["pass"], true, "{r}");
    assert_eq!(r["target"], "youtube_360");
    assert_eq!(
        ids(&r),
        [
            "readable",
            "projection_fits_picture",
            "spatial_audio_fits",
            "spherical_metadata",
            "equirect",
            "container",
            "video_codec",
            "progressive",
            "spatial_audio_youtube",
            "one_audio_track"
        ]
    );
    assert_eq!(
        rule(&r, "spatial_audio_fits")["message"],
        "1st order ambiX in 4 channels"
    );
    assert_eq!(
        r["advice"],
        json!([]),
        "H.264, 4:2:0, AAC-LC, 48 kHz, 30 fps"
    );
    assert_eq!(r["warnings"], json!([]));

    let r = check(&tools, &file, Some("archive"));
    assert_eq!(r["pass"], true, "{r}");
    let r = check(&tools, &file, None);
    assert_eq!(r["target"], Value::Null);
    assert_eq!(
        ids(&r),
        ["readable", "projection_fits_picture", "spatial_audio_fits"]
    );
}

#[test]
fn a_2_1_picture_tagged_top_bottom_fails_and_mono_fits() {
    let d = TempDir::new("tb");
    let flat = d.clip(spec("t-flat", 256, 128));
    let tb = d.inject(
        &flat,
        "t-tb.mp4",
        [
            write::st3d(StereoMode::TopBottom),
            write::sv3d_equirect("t"),
        ]
        .concat(),
        Vec::new(),
    );
    let r = check(&d.tools(), &tb, None);
    let fits = rule(&r, "projection_fits_picture");
    assert_eq!(fits["ok"], false, "{r}");
    assert_eq!(
        fits["message"],
        "the picture is 256x128 (2:1), but top-bottom stereo equirect is 2:1 per eye"
    );
    assert!(fits["fix"].as_str().unwrap().contains("mono"), "{fits}");
    // Tagged left-right, a 4:1 picture fits.
    let wide = d.clip(spec("t-wide", 512, 128));
    let lr = d.inject(
        &wide,
        "t-lr.mp4",
        [
            write::st3d(StereoMode::LeftRight),
            write::sv3d_equirect("t"),
        ]
        .concat(),
        Vec::new(),
    );
    let r = check(&d.tools(), &lr, Some("youtube_360"));
    assert_eq!(rule(&r, "projection_fits_picture")["ok"], true, "{r}");
}

#[test]
fn sa3d_on_stereo_audio_fails_its_channel_count() {
    let d = TempDir::new("sa3d");
    let stereo = d.clip(Spec {
        audio: Audio::Stereo,
        spherical: true,
        ..spec("t-stereo", 256, 128)
    });
    let bad = d.inject(
        &stereo,
        "t-sa3d.mp4",
        Vec::new(),
        write::sa3d_first_order_ambix(),
    );
    let r = check(&d.tools(), &bad, Some("youtube_360"));
    let fits = rule(&r, "spatial_audio_fits");
    assert_eq!(fits["ok"], false, "{r}");
    assert_eq!(
        fits["message"],
        "SA3D says 1st order ambiX, 4 channels, but the track has 2"
    );
    assert_eq!(rule(&r, "spatial_audio_youtube")["ok"], false);
    assert_eq!(r["pass"], false);

    // Head-locked stereo wants six channels, so four fail.
    let four = d.clip(Spec {
        audio: Audio::AmbiX {
            azimuth: 0,
            elevation: 0,
        },
        ..spec("t-four", 256, 128)
    });
    let head = d.inject(&four, "t-head.mp4", Vec::new(), write::sa3d_ambix(1, true));
    let r = check(&d.tools(), &head, None);
    assert_eq!(
        rule(&r, "spatial_audio_fits")["message"],
        "SA3D says 1st order ambiX with head-locked stereo, 6 channels, but the track has 4"
    );
}

#[test]
fn an_untagged_2_1_clip_gets_the_metadata_fix() {
    let d = TempDir::new("untagged");
    let file = d.clip(spec("t-untagged", 256, 128));
    let tools = d.tools();
    let r = check(&tools, &file, Some("youtube_360"));
    let sph = rule(&r, "spherical_metadata");
    assert_eq!(sph["ok"], false);
    assert!(sph["fix"].as_str().unwrap().contains("sv3d"), "{sph}");
    // Not equirect without metadata: the rule isn't there.
    assert!(!ids(&r).contains(&"equirect"));
    let r = check(&tools, &file, Some("archive"));
    assert_eq!(r["pass"], true);
    assert_eq!(r["advice"][0]["id"], "spherical_metadata", "{r}");
}

#[test]
fn a_cubemap_has_its_shape_checked_and_fails_youtube() {
    let d = TempDir::new("cubemap");
    let cube = Projection::Cubemap {
        layout: 0,
        padding: 0,
    };
    let tools = d.tools();
    let three_two = d.clip(spec("t-32", 384, 256));
    let file = d.inject(
        &three_two,
        "t-cube.mp4",
        write::sv3d("t", &cube),
        Vec::new(),
    );
    let r = check(&tools, &file, Some("youtube_360"));
    assert_eq!(rule(&r, "projection_fits_picture")["ok"], true, "{r}");
    let eq = rule(&r, "equirect");
    assert_eq!(eq["ok"], false);
    assert!(eq["message"].as_str().unwrap().contains("cubemap"));
    let two_one = d.clip(spec("t-21", 256, 128));
    let file = d.inject(
        &two_one,
        "t-cube21.mp4",
        write::sv3d("t", &cube),
        Vec::new(),
    );
    let r = check(&tools, &file, None);
    assert_eq!(rule(&r, "projection_fits_picture")["ok"], false, "{r}");
}

#[test]
fn bounds_change_the_shape_an_equirect_must_have() {
    let d = TempDir::new("bounds");
    // Half the sphere's height cropped away: 4:1.
    let half = Projection::Equirect {
        top: 1 << 30,
        bottom: 1 << 30,
        left: 0,
        right: 0,
    };
    let tools = d.tools();
    for (w, h, ok) in [(512, 128, true), (256, 128, false)] {
        let src = d.clip(spec(&format!("t-{w}"), w, h));
        let file = d.inject(
            &src,
            &format!("t-b{w}.mp4"),
            write::sv3d("t", &half),
            Vec::new(),
        );
        let r = check(&tools, &file, None);
        assert_eq!(
            rule(&r, "projection_fits_picture")["ok"],
            ok,
            "{w}x{h}: {r}"
        );
    }
}

#[test]
fn matroska_fails_youtube_s_container_rule_but_keeps_its_projection() {
    let d = TempDir::new("mkv");
    let mp4 = d.clip(Spec {
        spherical: true,
        ..spec("t-mkv", 256, 128)
    });
    let mkv = media::remux_to_mkv(&mp4, &d.0).unwrap();
    let r = check(&d.tools(), &mkv, Some("youtube_360"));
    assert_eq!(rule(&r, "spherical_metadata")["ok"], true, "{r}");
    assert_eq!(rule(&r, "projection_fits_picture")["ok"], true);
    let c = rule(&r, "container");
    assert_eq!(c["ok"], false);
    assert!(c["fix"].as_str().unwrap().contains("MP4"), "{c}");
    assert_eq!(
        rule(&r, "readable")["message"],
        "nothing is cut short (in Matroska, only its length is checked)"
    );
}

#[test]
fn flat_targets_check_the_shape_and_the_metadata() {
    let d = TempDir::new("flat");
    let tools = d.tools();
    let wide = d.clip(spec("t-169", 320, 180));
    let r = check(&tools, &wide, Some("flat_16_9"));
    assert_eq!(r["pass"], true, "{r}");
    assert_eq!(
        ids(&r),
        ["readable", "not_spherical", "aspect", "square_pixels"]
    );
    let r = check(&tools, &wide, Some("vertical"));
    assert_eq!(rule(&r, "aspect")["ok"], false);
    assert_eq!(
        rule(&r, "aspect")["message"],
        "it is shown at 16:9, not 9:16"
    );
    let tall = d.clip(spec("t-916", 180, 320));
    assert_eq!(check(&tools, &tall, Some("vertical"))["pass"], true);
    let sphere = d.clip(Spec {
        spherical: true,
        ..spec("t-sph", 256, 128)
    });
    let r = check(&tools, &sphere, Some("flat_16_9"));
    assert_eq!(rule(&r, "not_spherical")["ok"], false, "{r}");
    assert_eq!(rule(&r, "aspect")["ok"], false);
}

#[test]
fn a_cut_file_fails_readable_rather_than_the_call() {
    let d = TempDir::new("cut");
    // The bad files break its audio and its projection too.
    let good = d.clip(Spec {
        audio: Audio::Stereo,
        spherical: true,
        ..spec("t-good", 256, 128)
    });
    let tools = d.tools();
    let bad = media::write_bad_files(&good, &d.0).unwrap();
    let mut failed = 0;
    for file in &bad {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        match call(&tools, json!({"file": file, "target": "archive"})) {
            Ok(r) => {
                assert!(r.to_string().len() <= CHECK_BYTES, "{name}: {r}");
                if name.contains("trunc") {
                    assert_eq!(r["pass"], false, "{name}: {r}");
                    let read = rule(&r, "readable");
                    assert_eq!(read["ok"], false, "{name}: {r}");
                    assert!(
                        read["message"].as_str().unwrap().contains("cut short"),
                        "{name}: {read}"
                    );
                    failed += 1;
                }
            }
            // A file that isn't media at all is the call's error.
            Err(e) => assert_eq!(e.code, codes::UNSUPPORTED_CONTAINER, "{name}: {e:?}"),
        }
    }
    assert!(failed >= 3, "the truncated files: {failed}");
}

#[test]
fn an_insv_without_its_trailer_fails_archive() {
    let d = TempDir::new("insv");
    let clip = d.clip(spec("t-insv", 256, 128));
    let insv = d.0.join("VID_20261030_000000_00_001.insv");
    std::fs::copy(&clip, &insv).unwrap();
    let r = check(&d.tools(), &insv, Some("archive"));
    let cam = rule(&r, "camera_metadata");
    assert_eq!(cam["ok"], false, "{r}");
    assert!(cam["message"].as_str().unwrap().contains("trailer"));
    assert_eq!(r["pass"], false);
}

/// The most rules a synthetic file fails at once stay under
/// `CHECK_BYTES` (a1-plan section 6.4): HEVC 4:2:2 tagged top-bottom on
/// a 2:1 picture at 15 fps, head-locked SA3D on 44.1 kHz stereo.
#[test]
fn the_worst_file_fits() {
    let d = TempDir::new("worst");
    let src = d.clip(Spec {
        codec: Codec::Hevc,
        chroma: spherecut_synth::yuv::Chroma::Yuv422,
        rate: spherecut_core::FrameRate::new(spherecut_core::Rational::from_integer(15)).unwrap(),
        audio: Audio::Stereo,
        sample_rate: 44_100,
        ..spec("t-worst", 256, 128)
    });
    let file = d.inject(
        &src,
        "t-worst-tagged.mp4",
        [
            write::st3d(StereoMode::TopBottom),
            write::sv3d_equirect("t"),
        ]
        .concat(),
        write::sa3d_ambix(1, true),
    );
    let r = check(&d.tools(), &file, Some("youtube_360"));
    assert_eq!(r["pass"], false);
    let failed = r["rules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["ok"] == false)
        .count();
    assert!(failed >= 3, "{r}");
    assert!(r["advice"].as_array().unwrap().len() >= 3, "{r}");
    let bytes = r.to_string().len();
    assert!(bytes <= CHECK_BYTES, "{bytes}: {r}");
}

#[test]
fn errors_name_their_argument() {
    let d = TempDir::new("errors");
    let tools = d.tools();
    let e = call(&tools, json!({"file": d.0.join("none.mp4")})).unwrap_err();
    assert_eq!(e.code, codes::NOT_FOUND);
    assert_eq!(e.argument.as_deref(), Some("file"));
    let e = call(&tools, json!({"file": "a.mp4", "target": "tiktok"})).unwrap_err();
    assert_eq!(e.code, codes::BAD_ARGUMENT);
    assert_eq!(e.argument.as_deref(), Some("target"));
}
