//! `spherecut frame` end to end (M1 plan sections 5, 6 and 14, DECISIONS
//! D16 and D27): the Euler table, the frame code read back through a 360°
//! fisheye, every failure with no output and no temporary file left
//! behind, `--overwrite` and the race it guards against, and the pipeline
//! goldens with the swscale cross-check.
//!
//! Every run renders on lavapipe (`--adapter cpu`) or with `--reference`,
//! and decodes in software: this machine has no render node, so `auto`
//! falls back, which the JSON says.
//!
//! **The pipeline goldens** follow the render goldens' rules
//! (`crates/render/tests/goldens.rs`, D32). Each row renders a file
//! through the whole of `frame`, decoding included, and stores the PNG in
//! `tests/goldens/` with `tests/goldens/manifest.json`:
//!
//! - at the driver build and source hash a PNG was blessed at, the render
//!   must match it pixel for pixel; elsewhere by PSNR ≥ 48 dB, SSIM ≥
//!   0.995 and at most 4 LSB. A source whose decoded planes no longer hash
//!   as blessed is reported as the source having changed;
//! - `--reference`, the CPU renderer, run with no Vulkan driver at all,
//!   holds each to a PSNR floor calibrated when blessing (the PSNR then,
//!   less 3 dB, never below 36 dB), with no channel further than 0.1;
//! - `SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-cli --test frame
//!   pipeline_goldens` rewrites the PNGs and the manifest, only when every
//!   check passes, and refuses to lower a floor by more than 0.5 dB unless
//!   it is `lower`. Failures write their images to `golden-diffs/` in the
//!   target directory.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

mod document;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use glam::DVec2;
use serde_json::{Map, Value, json};
use spherecut_core::video::{Matrix, Range, VideoPlanes};
use spherecut_core::{FrameRate, StopToken};
use spherecut_media::backend::{FfmpegBackend, MediaBackend, NextFrame, VideoRequest};
use spherecut_media::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
use spherecut_media::ff;
use spherecut_render::reference;
use spherecut_render::{Image, Pixels, ViewParams};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_sphere::{LonLat, equirect, pixel};
use spherecut_synth::media::{self, CLIP_COLOR, Codec, Spec};
use spherecut_synth::yuv::{self, Chroma, YuvFormat};
use spherecut_synth::{AXIS_MARKERS, Pattern, RgbImage, decode_frame_code, render_equirect};
use spherecut_testkit::compare::{View, psnr, ssim};
use spherecut_testkit::{Fnv64, fnv64};

use document::document;

// ---------------------------------------------------------------------
// Running frame

/// A test's own directory, removed with what it holds when dropped, so
/// also when the test fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("frame-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The binary, with nothing from the environment steering it.
fn spherecut() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    cmd.env_remove("SPHERECUT_LOG")
        .env_remove("SPHERECUT_ADAPTER")
        .env_remove("SPHERECUT_DEBUG_FRAME_RACE")
        .env_remove("SPHERECUT_DEBUG_PANIC");
    cmd
}

/// What a `frame --json` run gave.
#[derive(Debug)]
struct Ran {
    status: Option<i32>,
    doc: Value,
    stderr: String,
}

impl Ran {
    fn ok(&self) -> &Value {
        assert_eq!(self.status, Some(0), "{} / {}", self.doc, self.stderr);
        assert_eq!(self.doc["ok"], true, "{}", self.doc);
        &self.doc["result"]
    }

    fn code(&self) -> &str {
        self.doc["error"]["code"].as_str().unwrap_or("")
    }

    /// The warnings' codes, in order.
    fn warnings(&self) -> Vec<&str> {
        self.doc["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap())
            .collect()
    }
}

/// `spherecut frame FILE -o OUT --json ARGS`, on lavapipe unless `args`
/// pick an adapter or `--reference`.
fn frame(file: &Path, out: &Path, args: &[&str]) -> Ran {
    frame_in(file, out, args, &[])
}

/// [`frame`] with `env` set.
fn frame_in(file: &Path, out: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Ran {
    let mut all: Vec<OsString> = vec!["frame".into(), file.into(), "-o".into(), out.into()];
    all.push("--json".into());
    if !args
        .iter()
        .any(|a| *a == "--adapter" || *a == "--reference")
    {
        all.extend(["--adapter".into(), "cpu".into()]);
    }
    all.extend(args.iter().map(OsString::from));
    let mut cmd = spherecut();
    cmd.args(&all);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    Ran {
        status: out.status.code(),
        doc: document(&out.stdout),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The temporary files in `dir`: there must never be any after a run.
fn temp_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| spherecut_core::fs::is_temp_name(n))
        .collect()
}

fn load(path: &Path) -> Image {
    Image::from_png(&std::fs::read(path).unwrap()).unwrap()
}

/// An image's samples in unit scale, R, G, B per pixel.
fn unit(img: &Image) -> Vec<f64> {
    match &img.pixels {
        Pixels::Rgb8(p) => p.iter().map(|&v| f64::from(v) / 255.0).collect(),
        Pixels::Rgb16(p) => p.iter().map(|&v| f64::from(v) / 65535.0).collect(),
        Pixels::LinearF32(p) => p.iter().map(|&v| f64::from(v)).collect(),
    }
}

/// Pixel `(x, y)` in unit scale.
fn rgb(img: &Image, x: u32, y: u32) -> [f64; 3] {
    let u = unit(img);
    let i = 3 * (y * img.width + x) as usize;
    [u[i], u[i + 1], u[i + 2]]
}

fn far(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f64::max)
}

/// A spherical clip of synth's grid, `frames` long at 30 fps.
fn grid_clip(dir: &Path, name: &str, size: (u32, u32), frames: u32, codec: Codec) -> PathBuf {
    let spec = Spec {
        name: name.into(),
        width: size.0,
        height: size.1,
        frames,
        codec,
        spherical: true,
        ..Spec::default()
    };
    media::write(&spec, dir).unwrap()
}

// ---------------------------------------------------------------------
// The Euler table and roll

/// The marker colours: F, R, B, L, U, D.
fn marker(label: char) -> [f64; 3] {
    let i = "FRBLUD".find(label).unwrap();
    AXIS_MARKERS[i].1
}

/// The plan's Euler table (section 12.1), on the grid clip: a 10° view, 33
/// pixels wide, whose centre pixel looks along the camera's axis.
///
/// At the poles that pixel is the pole itself, where the equirect's
/// columns meet: there isotropic trilinear filtering at the analytic LOD
/// (D32) reads a level whose texels span 90° of longitude, so the U and D
/// discs come out a neutral grey (0.72 and 0.28 measured, the same on
/// the CPU reference), not white and black. Those two rows check that the
/// centre is the pole, neutral and brighter for U than for D, and
/// `the_markers_round_a_fisheye_turn_with_the_camera` checks the rest of
/// the sphere round it.
#[test]
fn the_euler_table() {
    let dir = TempDir::new("euler");
    let clip = grid_clip(&dir, "grid", (1024, 512), 1, Codec::H264Lossless);
    let out = dir.join("out.png");
    let rows: &[(&[&str], char)] = &[
        (&[], 'F'),
        (&["--yaw", "90"], 'R'),
        (&["--yaw", "180"], 'B'),
        (&["--yaw", "-90"], 'L'),
        (&["--pitch", "90"], 'U'),
        (&["--pitch", "-90"], 'D'),
        (&["--roll", "90"], 'F'),
    ];
    for (args, label) in rows {
        let mut a = vec!["--fov", "10", "--size", "33x33", "--overwrite"];
        a.extend_from_slice(args);
        let ran = frame(&clip, &out, &a);
        let view = &ran.ok()["view"];
        let img = load(&out);
        let c = rgb(&img, 16, 16);
        match label {
            'U' | 'D' => {
                let spread = far(c, [c[0]; 3]).max(far(c, [c[1]; 3]));
                assert!(spread <= 2.0 / 255.0, "{args:?}: the pole is {c:?}");
                let ok = if *label == 'U' {
                    c[0] > 0.6
                } else {
                    c[0] < 0.4
                };
                assert!(ok, "{args:?}: the pole is {c:?}");
            }
            _ => assert!(
                far(c, marker(*label)) <= 0.1,
                "{args:?}: the centre is {c:?}, not {label} {:?}",
                marker(*label)
            ),
        }
        // The JSON gives the view as asked.
        for (k, v) in args.chunks(2).map(|kv| (kv[0], kv[1])) {
            let v: f64 = v.parse().unwrap();
            assert_eq!(view[&k[2..]], v, "{}", ran.doc);
        }
    }
}

/// Amendment 10: `--roll 90` turns the picture about the view's axis so
/// that the U marker is on its left (the camera rolled clockwise, as a
/// head tilts to the right). In a 200° fisheye the markers 90° off axis
/// sit at radius 0.9: F stays at the centre and the four round it move a
/// quarter turn. U and D are the poles, which filter to neutral greys
/// (see [`the_euler_table`]), the others are their own colours.
#[test]
fn the_markers_round_a_fisheye_turn_with_the_camera() {
    let dir = TempDir::new("roll");
    let clip = grid_clip(&dir, "grid", (1024, 512), 1, Codec::H264Lossless);
    let out = dir.join("out.png");
    let size = (401u32, 401u32);
    // Lens points 90° off axis: top, left, right, bottom.
    let at = |p: DVec2| {
        let q = pixel::from_lens(size, p);
        (q.x as u32, q.y as u32)
    };
    let spots = [
        at(DVec2::new(0.0, 0.9)),
        at(DVec2::new(-0.9, 0.0)),
        at(DVec2::new(0.9, 0.0)),
        at(DVec2::new(0.0, -0.9)),
    ];
    // (arguments, centre, then top, left, right, bottom)
    let rows: &[(&[&str], [char; 5])] = &[
        (&[], ['F', 'U', 'L', 'R', 'D']),
        (&["--roll", "90"], ['F', 'R', 'U', 'D', 'L']),
        (&["--roll", "-90"], ['F', 'L', 'D', 'U', 'R']),
        (&["--pitch", "90"], ['U', 'B', 'L', 'R', 'F']),
        (&["--pitch", "-90"], ['D', 'F', 'L', 'R', 'B']),
    ];
    for (args, want) in rows {
        let mut a = vec![
            "--projection",
            "fisheye",
            "--fov",
            "200",
            "--size",
            "401x401",
            "--overwrite",
        ];
        a.extend_from_slice(args);
        frame(&clip, &out, &a).ok();
        let img = load(&out);
        let centre = at(DVec2::ZERO);
        for (&(x, y), label) in [centre].iter().chain(&spots).zip(want) {
            let c = rgb(&img, x, y);
            let ok = match label {
                'U' => far(c, [0.72; 3]) <= 0.05,
                'D' => far(c, [0.28; 3]) <= 0.05,
                _ => far(c, marker(*label)) <= 0.1,
            };
            assert!(ok, "{args:?}: ({x}, {y}) is {c:?}, not {label}");
        }
    }
}

// ---------------------------------------------------------------------
// The frame code

/// What `img`, a 360° fisheye looking along −Z, shows of the sphere, as
/// a 360×180 equirect, nearest pixel.
fn unwrap_fisheye(img: &Image) -> RgbImage {
    let lens = Lens::new(Projection::Fisheye, 360f64.to_radians());
    let size = (img.width, img.height);
    let u = unit(img);
    let (w, h) = (360u32, 180u32);
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for j in 0..h {
        for i in 0..w {
            let uv = DVec2::new(
                (f64::from(i) + 0.5) / f64::from(w),
                (f64::from(j) + 0.5) / f64::from(h),
            );
            let c = lens.project(equirect::to_dir(uv)).map_or([0.0; 3], |p| {
                let q = pixel::from_lens(size, p);
                let x = (q.x.max(0.0) as u32).min(size.0 - 1);
                let y = (q.y.max(0.0) as u32).min(size.1 - 1);
                let k = 3 * (y * size.0 + x) as usize;
                [u[k], u[k + 1], u[k + 2]]
            });
            pixels.push(c);
        }
    }
    RgbImage {
        width: w,
        height: h,
        pixels,
    }
}

/// The frame number `frame` rendered, read from its frame code.
fn shown(clip: &Path, out: &Path, args: &[&str]) -> (u32, Value) {
    let mut a = vec![
        "--projection",
        "fisheye",
        "--fov",
        "360",
        "--size",
        "512x512",
        "--overwrite",
    ];
    a.extend_from_slice(args);
    let ran = frame(clip, out, &a);
    let result = ran.ok().clone();
    let code = decode_frame_code(&unwrap_fisheye(&load(out)));
    (
        code.unwrap_or_else(|| panic!("{args:?}: no frame code")),
        result,
    )
}

/// `--time` and `--frame` on a constant-rate clip, and on its Matroska
/// remux, whose millisecond timestamps round each frame's start: the
/// frame shown is the frame at that time, read back from its code.
#[test]
fn the_frame_shown_is_the_one_asked_for() {
    let dir = TempDir::new("code");
    let mp4 = grid_clip(&dir, "cfr", (512, 256), 30, Codec::H264Lossless);
    let mkv = media::remux_to_mkv(&mp4, &dir).unwrap();
    let out = dir.join("out.png");
    // (arguments, the frame, the requested time as text, whether a frame
    // was asked for)
    let rows: &[(&[&str], u32, &str, bool)] = &[
        (&[], 0, "0", false),
        (&["--time", "0"], 0, "0", false),
        (&["--time", "0.51"], 15, "51/100", false),
        (&["--time", "0.99"], 29, "99/100", false),
        // Exactly frame 10's start.
        (&["--time", "1/3"], 10, "1/3", false),
        (&["--frame", "17"], 17, "17/30", true),
        (&["--time", "17f"], 17, "17/30", true),
        (&["--frame", "29"], 29, "29/30", true),
    ];
    // The remux has no spherical metadata.
    for (clip, tag) in [(&mp4, None), (&mkv, Some("--equirect"))] {
        for (args, want, requested, by_frame) in rows {
            let mut args = args.to_vec();
            args.extend(tag);
            let (got, r) = shown(clip, &out, &args);
            let name = clip.file_name().unwrap().to_string_lossy();
            assert_eq!(got, *want, "{name} {args:?}: {r}");
            assert_eq!(r["frame"]["time"]["frame"], *want, "{name} {args:?}: {r}");
            assert_eq!(r["requested"]["exact"], *requested, "{name} {args:?}: {r}");
            assert_eq!(
                r["requested"].get("frame").is_some(),
                *by_frame,
                "{name} {args:?}: {r}"
            );
            if *by_frame {
                assert_eq!(r["requested"]["frame"], *want, "{r}");
            }
            assert_eq!(r["exactness"], "exact");
        }
    }
}

/// On a variable-rate clip a frame number counts frames (section 5), and
/// a time shows the frame whose interval holds it, with no frame number
/// in the JSON, which only counting would know.
#[test]
fn a_frame_number_on_a_variable_rate_clip_counts_frames() {
    let dir = TempDir::new("vfr");
    let spec = Spec {
        name: "vfr".into(),
        width: 512,
        height: 256,
        frames: 30,
        codec: Codec::H264Lossless,
        spherical: true,
        vfr: true,
        ..Spec::default()
    };
    let clip = media::write(&spec, &dir).unwrap();
    let out = dir.join("out.png");
    let start = |n: u32| (0..n).map(media::vfr_ticks).sum::<i64>();

    let (got, r) = shown(&clip, &out, &["--frame", "7"]);
    assert_eq!(got, 7, "{r}");
    assert_eq!(r["frame"]["time"]["frame"], 7, "{r}");
    assert_eq!(r["frame"]["pts"], start(7), "{r}");
    assert_eq!(r["requested"]["frame"], 7, "{r}");

    // A tick into frame 7's interval.
    let t = format!("{}/{}", start(7) + 1, media::VFR_TIMESCALE);
    let (got, r) = shown(&clip, &out, &["--time", &t]);
    assert_eq!(got, 7, "{r}");
    assert!(r["frame"]["time"].get("frame").is_none(), "{r}");
    assert_eq!(r["frame"]["pts"], start(7), "{r}");

    // Seven frames at the rate the encoder was told is another frame:
    // the frame number isn't a time.
    let (got, _) = shown(&clip, &out, &["--time", "7/30"]);
    assert_ne!(got, 7);
    let (got, _) = shown(&clip, &out, &["--frame", "29"]);
    assert_eq!(got, 29);

    for n in ["30", "99"] {
        let ran = frame(&clip, &dir.join("none.png"), &["--frame", n]);
        assert_eq!(ran.status, Some(1), "{}", ran.doc);
        assert_eq!(ran.code(), "out_of_range");
        let message = ran.doc["error"]["message"].as_str().unwrap();
        assert!(message.contains("has 30 frames, 0 to 29"), "{message}");
    }
    assert!(!dir.join("none.png").exists());
}

// ---------------------------------------------------------------------
// Failures

/// `src` remuxed into `dst` with `moov` before `mdat`, so that the index
/// lists samples past a cut.
// Reads a file this test wrote and writes another with ffmpeg-next alone,
// apart from Spherecut's confined open (D24), as synth's seek test does.
#[allow(clippy::disallowed_methods)]
fn faststart(src: &Path, dst: &Path) {
    let mut ictx = ff::format::input(src).unwrap();
    let mut octx = ff::format::output(dst).unwrap();
    for st in ictx.streams() {
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    let mut opts = ff::Dictionary::new();
    opts.set("movflags", "faststart");
    octx.write_header_with(opts).unwrap();
    for (st, mut pkt) in ictx.packets() {
        pkt.rescale_ts(st.time_base(), octx.stream(st.index()).unwrap().time_base());
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
}

/// Every row of the exit table that a file or an argument can bring
/// about (the adapter and the SIGINT rows are in `contract.rs`): the exit
/// status, the code, and no output and no temporary file afterwards.
#[test]
fn every_failure_leaves_nothing_behind() {
    let dir = TempDir::new("errors");
    let good = grid_clip(&dir, "good", (512, 256), 30, Codec::H264);
    let untagged = media::write(
        &Spec {
            name: "untagged".into(),
            frames: 3,
            ..Spec::default()
        },
        &dir,
    )
    .unwrap();
    let text = dir.join("notes.mp4");
    std::fs::write(&text, b"not a video, only words\n").unwrap();
    // Cut at two thirds, with the index first: frames past the cut are
    // listed but gone. With a keyframe every 8 frames the cut falls after
    // a dozen good ones; `good`'s one keyframe, a third of the file,
    // takes the cut itself, and nothing decodes.
    let gop8 = media::write(
        &Spec {
            name: "gop8".into(),
            width: 512,
            height: 256,
            gop: Some(media::Gop {
                keyint: 8,
                bframes: 0,
                open: false,
                radl: 0,
            }),
            ..Spec::default()
        },
        &dir,
    )
    .unwrap();
    let cut_at = |src: &Path, name: &str| {
        let fast = dir.join(format!("{name}-faststart.mp4"));
        faststart(src, &fast);
        let bytes = std::fs::read(&fast).unwrap();
        let cut = dir.join(format!("{name}.mp4"));
        std::fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).unwrap();
        cut
    };
    let cut = cut_at(&gop8, "cut");
    let cut_first = cut_at(&good, "cut-first");
    // The same cut in Matroska, which says how long the file was in its
    // Segment; and in a variable-rate clip, whose frames are counted.
    let mkv = media::remux_to_mkv(&gop8, &dir).unwrap();
    let bytes = std::fs::read(&mkv).unwrap();
    let mkv_cut = dir.join("cut.mkv");
    std::fs::write(&mkv_cut, &bytes[..bytes.len() * 2 / 3]).unwrap();
    let vfr = media::write(
        &Spec {
            name: "vfr".into(),
            width: 512,
            height: 256,
            vfr: true,
            gop: Some(media::Gop {
                keyint: 8,
                bframes: 0,
                open: false,
                radl: 0,
            }),
            ..Spec::default()
        },
        &dir,
    )
    .unwrap();
    let vfr_cut = cut_at(&vfr, "vfr-cut");
    // A sample entry of a codec FFmpeg doesn't know.
    let mut f = std::fs::read(&good).unwrap();
    let at = f.windows(4).position(|w| w == b"stsd").unwrap() + 16;
    assert_eq!(&f[at..at + 4], b"avc1");
    f[at..at + 4].copy_from_slice(b"zzzz");
    let zzzz = dir.join("zzzz.mp4");
    std::fs::write(&zzzz, &f).unwrap();
    let missing = dir.join("missing.mp4");
    let out_dir = dir.join("outputs");
    std::fs::create_dir_all(&out_dir).unwrap();
    let out = out_dir.join("out.png");

    // (file, arguments, exit status, code)
    let rows: Vec<(&Path, Vec<&str>, i32, &str)> = vec![
        (&good, vec!["--size", "10x10"], 2, "usage"),
        (&good, vec!["--size", "1920"], 2, "usage"),
        (&good, vec!["--size", "16385x16"], 2, "usage"),
        (&good, vec!["--projection", "cube"], 2, "usage"),
        (&good, vec!["--fov", "200"], 2, "usage"),
        (&good, vec!["--fov", "0"], 2, "usage"),
        (
            &good,
            vec!["--projection", "fisheye", "--fov", "361"],
            2,
            "usage",
        ),
        (&good, vec!["--time", "1:2"], 2, "usage"),
        (&good, vec!["--time", "-1"], 2, "usage"),
        (&good, vec!["--time", "-1:02.5"], 2, "usage"),
        (&good, vec!["--frame", "-1"], 2, "usage"),
        (&good, vec!["--fov", "-5"], 2, "usage"),
        (&good, vec!["--time", "1", "--frame", "2"], 2, "usage"),
        (&good, vec!["--bogus"], 2, "usage"),
        (&good, vec!["--morph", "0.5"], 2, "usage"),
        (&good, vec!["--morph-to", "fisheye"], 2, "usage"),
        (
            &good,
            vec!["--morph-to", "fisheye", "--morph", "2"],
            2,
            "usage",
        ),
        (&good, vec!["--reference", "--adapter", "cpu"], 2, "usage"),
        (&good, vec!["--reference", "--filter", "aniso"], 2, "usage"),
        (&good, vec!["--stream", "5"], 2, "usage"),
        (&missing, vec![], 1, "not_found"),
        (&text, vec![], 1, "not_media"),
        (&untagged, vec![], 1, "not_equirect"),
        (&mkv, vec![], 1, "not_equirect"),
        (&good, vec!["--time", "99"], 1, "out_of_range"),
        (&good, vec!["--time", "1"], 1, "out_of_range"),
        (&good, vec!["--frame", "30"], 1, "out_of_range"),
        (&cut, vec!["--equirect", "--time", "0.9"], 1, "truncated"),
        (
            &mkv_cut,
            vec!["--equirect", "--time", "0.9"],
            1,
            "truncated",
        ),
        (
            &mkv_cut,
            vec!["--equirect", "--frame", "29"],
            1,
            "truncated",
        ),
        (
            &vfr_cut,
            vec!["--equirect", "--frame", "29"],
            1,
            "truncated",
        ),
        // Too far on for any timestamp of the clip.
        (
            &good,
            vec!["--time", "5000000000000000/1"],
            1,
            "out_of_range",
        ),
        (
            &good,
            vec!["--frame", "1000000000000000000"],
            1,
            "out_of_range",
        ),
        (
            &good,
            vec!["--frame", "18446744073709551615"],
            1,
            "out_of_range",
        ),
        // The decoder's setup fails, before any frame: the probe's damage
        // is in the warnings (below).
        (&cut_first, vec!["--equirect"], 1, "decode_failed"),
        (
            &good,
            vec!["--adapter", "no-such-gpu"],
            3,
            "adapter_not_found",
        ),
        (
            &good,
            vec!["--decoder", "vaapi", "--render-node", "/nonexistent"],
            3,
            "hw_unavailable",
        ),
        // lavapipe's Vulkan has no video decode.
        (&good, vec!["--decoder", "vulkan"], 3, "hw_unavailable"),
        (&zzzz, vec!["--equirect"], 3, "no_decoder"),
    ];
    for (file, args, status, code) in &rows {
        let ran = frame(file, &out, args);
        let what = format!("{} {args:?}: {} / {}", file.display(), ran.doc, ran.stderr);
        assert_eq!(ran.status, Some(*status), "{what}");
        assert_eq!(ran.doc["schema"], "spherecut.frame/1", "{what}");
        assert_eq!(ran.doc["ok"], false, "{what}");
        assert!(ran.doc.get("result").is_none(), "{what}");
        assert_eq!(ran.code(), *code, "{what}");
        assert!(!out.exists(), "{what}: an output was left");
        assert_eq!(temp_files(&out_dir), Vec::<String>::new(), "{what}");
    }
    // Without --json the message is for people, on stderr.
    let o = spherecut()
        .args(["frame", "--adapter", "cpu", "-o"])
        .arg(&out)
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.starts_with("error: "), "{stderr}");

    // The messages say what is wrong, and what to do.
    let message = |file: &Path, args: &[&str]| {
        let ran = frame(file, &out, args);
        ran.doc["error"]["message"].as_str().unwrap().to_string()
    };
    let m = message(&good, &["--time", "99"]);
    assert!(
        m.starts_with("no frame at 99 s: the clip runs from 0 to 1 s"),
        "{m}"
    );
    let m = message(&untagged, &[]);
    assert!(
        m.ends_with("--equirect renders it as equirect anyway"),
        "{m}"
    );
    let m = message(&cut, &["--equirect", "--time", "0.9"]);
    assert!(m.contains("is damaged before 0.9 s"), "{m}");
    // probe reads spherical metadata from MP4 boxes only, which the
    // message says rather than that the file has none.
    let m = message(&mkv, &[]);
    assert!(
        m.contains(" is a 512x256 Matroska/WebM file, and Spherecut doesn't read Matroska's spherical metadata yet"),
        "{m}"
    );
    let m = message(&mkv_cut, &["--equirect", "--frame", "29"]);
    assert!(m.contains("is damaged before frame 29"), "{m}");
    assert!(m.contains("bytes its Matroska Segment says it has"), "{m}");
    assert!(m.contains("; the last good frame is at "), "{m}");
    let m = message(&vfr_cut, &["--equirect", "--frame", "29"]);
    assert!(m.contains("is damaged before frame 29"), "{m}");
    assert!(m.contains("the file is truncated"), "{m}");
    assert!(m.contains("; the last good frame is at "), "{m}");
    let m = message(&good, &["--time", "5000000000000000/1"]);
    assert_eq!(
        m,
        "no frame at 5000000000000000 s: the clip runs from 0 to 1 s"
    );
    let m = message(&good, &["--frame", "18446744073709551615"]);
    assert!(
        m.starts_with("no frame 18446744073709551615: ") && m.ends_with(" has 30 frames, 0 to 29"),
        "{m}"
    );
    let m = message(&zzzz, &["--equirect"]);
    assert!(m.contains("\"zzzz\""), "{m}");
    // A negative value gets frame's own message, not clap's tip to pass
    // it after `--`.
    for (args, says) in [
        (
            ["--time", "-1"],
            "a time can't be negative: the clip starts at 0",
        ),
        (["--time", "-1:02.5"], "a time can't be negative"),
        (
            ["--frame", "-1"],
            "a frame number can't be negative: the first frame is 0",
        ),
        (
            ["--fov", "-5"],
            "--fov -5: rectilinear takes more than 0 and at most 170 degrees",
        ),
    ] {
        let m = message(&good, &args);
        assert!(m.contains(says), "{args:?}: {m}");
    }
    let m = message(&good, &["--fov", "200"]);
    assert!(
        m.contains("--fov 200: rectilinear takes more than 0 and at most 170 degrees"),
        "{m}"
    );

    // The cut file still gives the frames before its cut, with the damage
    // as a warning; and the untagged one renders as an equirect when told.
    let ran = frame(&cut, &out, &["--equirect", "--time", "0.1"]);
    ran.ok();
    assert!(ran.warnings().contains(&"truncated"), "{}", ran.doc);
    std::fs::remove_file(&out).unwrap();
    let ran = frame(&cut_first, &out, &["--equirect"]);
    assert_eq!(ran.warnings().first(), Some(&"truncated"), "{}", ran.doc);
    assert!(!out.exists());
    frame(&untagged, &out, &["--equirect"]).ok();
    assert_eq!(temp_files(&out_dir), Vec::<String>::new());
}

/// What a cut in the middle of a B-frame's packet leaves of a clip. The
/// B-frame, shown before the P-frame that came before it in the file, is
/// lost with everything after it in the file, while that P-frame still
/// comes out as the decoder drains.
struct Lost {
    /// The lost B-frame's number and the P-frame's, from 0.
    b: u64,
    p: u64,
    /// The B-frame's start, as `--time` takes it.
    b_time: String,
    /// The start of the last frame before it that the cut file holds, in
    /// seconds as frame's messages say them.
    before: String,
}

/// Cut `clip` into `cut` in the middle of the first packet past its
/// middle that is a B-frame shown before the packet before it.
// Reads a file this test wrote with ffmpeg-next alone, as `faststart`
// does.
#[allow(clippy::disallowed_methods)]
fn cut_in_a_b_frame(clip: &Path, cut: &Path) -> Lost {
    let mut ictx = ff::format::input(clip).unwrap();
    let st = ictx.streams().best(ff::media::Type::Video).unwrap();
    let (index, tb) = (st.index(), st.time_base());
    // (pts, position, size), in the file's order.
    let packets: Vec<(i64, usize, usize)> = ictx
        .packets()
        .filter(|(s, _)| s.index() == index)
        .map(|(_, p)| {
            let at = usize::try_from(p.position()).unwrap();
            (p.pts().unwrap(), at, p.size())
        })
        .collect();
    let i = (packets.len() / 2..packets.len() - 1)
        .find(|&i| packets[i + 1].0 < packets[i].0)
        .unwrap_or_else(|| panic!("{}: no B-frame: {packets:?}", clip.display()));
    let (p, b) = (packets[i], packets[i + 1]);
    let mut shown: Vec<i64> = packets.iter().map(|q| q.0).collect();
    shown.sort_unstable();
    let number = |pts: i64| shown.iter().position(|&q| q == pts).unwrap() as u64;
    let ticks = |pts: i64| (pts - shown[0]) * i64::from(tb.numerator());
    let before = packets[..=i]
        .iter()
        .map(|q| q.0)
        .filter(|&q| q < b.0)
        .max()
        .unwrap();
    let secs = ticks(before) as f64 / f64::from(tb.denominator());
    let before = format!("{secs:.3}");
    let before = before
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string();
    let bytes = std::fs::read(clip).unwrap();
    std::fs::write(cut, &bytes[..b.1 + b.2 / 2]).unwrap();
    Lost {
        b: number(b.0),
        p: number(p.0),
        b_time: format!("{}/{}", ticks(b.0), tb.denominator()),
        before,
    }
}

/// `mkv` as a live recording leaves it, its Segment's size unknown, so
/// that nothing can tell a cut from the end.
fn unknown_size(mkv: &Path, dst: &Path) {
    let mut f = std::fs::read(mkv).unwrap();
    let at = f
        .windows(4)
        .position(|w| w == [0x18, 0x53, 0x80, 0x67])
        .unwrap()
        + 4;
    // FFmpeg writes the size in 8 bytes; all ones is unknown.
    assert_eq!(f[at], 0x01);
    f[at + 1..at + 8].fill(0xFF);
    std::fs::write(dst, &f).unwrap();
}

/// A frame the cut lost is never shown in its place, as exact, though a
/// later frame still came out (D13, D27, the M1 plan's amendment 8): in
/// Matroska, whose demuxer reads a cut as the end of the file, it is
/// `truncated`, by a time or a frame number; on a variable-rate MP4,
/// counting stops at the cut, since a frame given up after it may come
/// after a lost one. Where nothing can tell a cut from the end (Matroska
/// that doesn't say its size), a frame number at a constant rate still
/// never shows another frame: frames are missing there (`damaged`), or
/// it is past the last (`out_of_range`).
#[test]
fn a_frame_lost_to_a_cut_is_never_shown_in_its_place() {
    let dir = TempDir::new("lost");
    let spec = |name: &str, vfr| Spec {
        name: name.into(),
        width: 512,
        height: 256,
        // Long enough for the cut to come after the blocks a Matroska rate
        // is checked on (D29): in them it would be a gap in the rate.
        frames: 150,
        vfr,
        // As synth's seek oracle has them: the encoder puts B-frames before
        // each keyframe at least.
        gop: Some(media::Gop {
            keyint: 8,
            bframes: 3,
            open: true,
            radl: 0,
        }),
        ..Spec::default()
    };
    let cfr = media::write(&spec("bframes", false), &dir).unwrap();
    let mkv = media::remux_to_mkv(&cfr, &dir).unwrap();
    let vfr = media::write(&spec("vfr-bframes", true), &dir).unwrap();
    let vfr_fast = dir.join("vfr-bframes-faststart.mp4");
    faststart(&vfr, &vfr_fast);
    let out = dir.join("out.png");
    let none = dir.join("none.png");
    let refused = |clip: &Path, args: &[&str], code: &str| {
        let mut all = vec!["--equirect"];
        all.extend_from_slice(args);
        let ran = frame(clip, &none, &all);
        assert_eq!(ran.status, Some(1), "{args:?}: {}", ran.doc);
        assert_eq!(ran.code(), code, "{args:?}: {}", ran.doc);
        assert!(!none.exists());
        ran.doc["error"]["message"].as_str().unwrap().to_string()
    };

    let mkv_cut = dir.join("cut.mkv");
    let lost = cut_in_a_b_frame(&mkv, &mkv_cut);
    let (b, p) = (lost.b.to_string(), lost.p.to_string());
    let held = (lost.p + 1).to_string();
    let last_good = format!("; the last good frame is at {} s", lost.before);
    for args in [["--frame", b.as_str()], ["--time", lost.b_time.as_str()]] {
        let m = refused(&mkv_cut, &args, "truncated");
        assert!(m.contains("bytes its Matroska Segment says it has"), "{m}");
        assert!(m.ends_with(&last_good), "{args:?}: {m}");
    }
    refused(&mkv_cut, &["--frame", "149"], "truncated");
    // Well before the cut, and the frames the decoder gives up after it.
    let early = lost.b / 2;
    let (got, _) = shown(
        &mkv_cut,
        &out,
        &["--equirect", "--frame", &early.to_string()],
    );
    assert_eq!(u64::from(got), early);

    let vfr_cut = dir.join("vfr-cut.mp4");
    let lost = cut_in_a_b_frame(&vfr_fast, &vfr_cut);
    for n in [lost.b, lost.p] {
        let m = refused(&vfr_cut, &["--frame", &n.to_string()], "truncated");
        assert!(m.contains("the file is truncated"), "{m}");
        assert!(m.contains("; the last good frame is at "), "{m}");
    }
    let early = lost.b / 2;
    let (got, _) = shown(
        &vfr_cut,
        &out,
        &["--equirect", "--frame", &early.to_string()],
    );
    assert_eq!(u64::from(got), early);

    let live = dir.join("live-cut.mkv");
    unknown_size(&mkv_cut, &live);
    let m = refused(&live, &["--frame", &b], "damaged");
    assert!(m.contains(&format!("has no frame {b}: ")), "{m}");
    let m = refused(&live, &["--frame", "149"], "out_of_range");
    assert!(m.ends_with(&format!(" has {held} frames, 0 to {p}")), "{m}");
    let (got, r) = shown(&live, &out, &["--equirect", "--frame", &p]);
    assert_eq!(got.to_string(), p, "{r}");
    assert_eq!(temp_files(&dir), Vec::<String>::new());
}

/// Where the output can't go: a directory, a device, a directory that
/// isn't there.
#[test]
fn an_output_that_is_not_a_file_is_refused() {
    let dir = TempDir::new("not-a-file");
    let good = grid_clip(&dir, "good", (256, 128), 1, Codec::H264);
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let rows: &[(&Path, &[&str], &str, &str)] = &[
        (&sub, &[], "not_a_file", "it is a directory"),
        (&sub, &["--overwrite"], "not_a_file", "it is a directory"),
        (
            Path::new("/dev/null"),
            &[],
            "not_a_file",
            "not a regular file",
        ),
        (
            Path::new("/dev/null"),
            &["--overwrite"],
            "not_a_file",
            "not a regular file",
        ),
    ];
    for (out, args, code, says) in rows {
        let ran = frame(&good, out, args);
        assert_eq!(ran.status, Some(1), "{}", ran.doc);
        assert_eq!(ran.code(), *code, "{}", ran.doc);
        let m = ran.doc["error"]["message"].as_str().unwrap();
        assert!(m.contains(says), "{m}");
        assert_eq!(ran.doc["error"]["path"], out.to_str().unwrap());
    }
    assert!(sub.is_dir());
    assert_eq!(std::fs::read_dir(&sub).unwrap().count(), 0);
    let ran = frame(&good, &dir.join("absent/out.png"), &[]);
    assert_eq!(ran.status, Some(1), "{}", ran.doc);
    assert_eq!(ran.code(), "io_error", "{}", ran.doc);
    assert!(!dir.join("absent").exists());
    // A name that ends in a slash is a directory's, there or not, and is
    // refused before any work, as one that is there is.
    for out in [dir.join("absent"), sub.clone()] {
        let mut name = out.into_os_string();
        name.push("/");
        let out = PathBuf::from(name);
        let ran = frame(&good, &out, &[]);
        assert_eq!(ran.status, Some(1), "{}", ran.doc);
        assert_eq!(ran.code(), "not_a_file", "{}", ran.doc);
        let m = ran.doc["error"]["message"].as_str().unwrap();
        assert!(
            m.ends_with(": the name is a directory's, not a file's"),
            "{m}"
        );
    }
    assert!(!dir.join("absent").exists());
    assert_eq!(std::fs::read_dir(&sub).unwrap().count(), 0);
    assert_eq!(temp_files(&dir), Vec::<String>::new());
}

/// D16: without `--overwrite` an existing output is never replaced, a
/// link included, even one that points nowhere; with it, the file is
/// replaced whole, and a link is written through and stays a link.
#[test]
fn an_existing_output_is_kept_unless_overwrite() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new("overwrite");
    let good = grid_clip(&dir, "good", (256, 128), 1, Codec::H264);
    let out = dir.join("out.png");
    let old = b"an older picture";
    std::fs::write(&out, old).unwrap();

    let ran = frame(&good, &out, &["--size", "64x32"]);
    assert_eq!(ran.status, Some(1), "{}", ran.doc);
    assert_eq!(ran.code(), "output_exists");
    assert_eq!(
        ran.doc["error"]["message"],
        format!("{} exists; pass --overwrite to replace it", out.display())
    );
    assert_eq!(std::fs::read(&out).unwrap(), old);

    let r = frame(&good, &out, &["--size", "64x32", "--overwrite"]);
    assert_eq!(r.ok()["output"], out.to_str().unwrap());
    assert_eq!(load(&out).width, 64);

    // A link to a file, and a dangling one.
    let target = dir.join("target.png");
    std::fs::write(&target, old).unwrap();
    let link = dir.join("link.png");
    symlink(&target, &link).unwrap();
    let dangling = dir.join("dangling.png");
    symlink(dir.join("nowhere.png"), &dangling).unwrap();
    for l in [&link, &dangling] {
        let ran = frame(&good, l, &["--size", "64x32"]);
        assert_eq!(ran.code(), "output_exists", "{}", ran.doc);
    }
    assert_eq!(std::fs::read(&target).unwrap(), old);
    assert!(!dir.join("nowhere.png").exists());
    for (l, file) in [(&link, &target), (&dangling, &dir.join("nowhere.png"))] {
        frame(&good, l, &["--size", "64x32", "--overwrite"]).ok();
        assert!(std::fs::symlink_metadata(l).unwrap().is_symlink());
        assert_eq!(load(file).width, 64);
    }
    assert_eq!(temp_files(&dir), Vec::<String>::new());
}

/// A file that appears at the output while the frame renders is left
/// alone: `frame` loses the race with `output_exists`, and what appeared
/// is what stays. Debug builds stage it with `SPHERECUT_DEBUG_FRAME_RACE`,
/// which writes the output just before `frame` publishes its own.
#[test]
fn a_race_for_the_output_is_lost_without_replacing() {
    if !cfg!(debug_assertions) {
        return;
    }
    let dir = TempDir::new("race");
    let good = grid_clip(&dir, "good", (256, 128), 1, Codec::H264);
    let out = dir.join("out.png");
    let run = |args: &[&str]| {
        let mut cmd = spherecut();
        cmd.args(["frame", "--json", "--adapter", "cpu", "-o"])
            .arg(&out)
            .arg(&good)
            .args(args)
            .env("SPHERECUT_DEBUG_FRAME_RACE", "1");
        let o = cmd.output().unwrap();
        (o.status.code(), document(&o.stdout))
    };
    let (status, v) = run(&[]);
    assert_eq!(status, Some(1), "{v}");
    assert_eq!(v["error"]["code"], "output_exists", "{v}");
    let m = v["error"]["message"].as_str().unwrap();
    assert!(m.contains("appeared while the frame was rendered"), "{m}");
    assert_eq!(std::fs::read(&out).unwrap(), b"written by someone else");
    assert_eq!(temp_files(&dir), Vec::<String>::new());
    // With --overwrite there is no race to lose: the hook isn't reached,
    // and the frame replaces the file.
    let (status, v) = run(&["--overwrite"]);
    assert_eq!(status, Some(0), "{v}");
    assert!(load(&out).width > 0);
}

// ---------------------------------------------------------------------
// What frame warns about

fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len() + 8).unwrap();
    [&size.to_be_bytes()[..], kind, payload].concat()
}

/// A full box: version 0, no flags.
fn full_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    mp4_box(kind, &[&[0, 0, 0, 0][..], payload].concat())
}

/// `sv3d` for an equirect with `pose` (yaw, pitch, roll in degrees) and
/// `bounds` (top, bottom, left, right, in 0.32 fixed point), as the
/// Spherical Video V2 spec lays it out.
fn sv3d(pose: [f64; 3], bounds: [u32; 4]) -> Vec<u8> {
    let svhd = full_box(b"svhd", b"spherecut tests\0");
    let prhd: Vec<u8> = pose
        .iter()
        .flat_map(|d| ((d * 65536.0).round() as i32).to_be_bytes())
        .collect();
    let equi: Vec<u8> = bounds.iter().flat_map(|b| b.to_be_bytes()).collect();
    let proj = mp4_box(
        b"proj",
        &[full_box(b"prhd", &prhd), full_box(b"equi", &equi)].concat(),
    );
    mp4_box(b"sv3d", &[svhd, proj].concat())
}

/// A one-frame lossless H.264 clip of `img` with `boxes` in its sample
/// entry.
fn tagged_clip(dir: &Path, name: &str, img: &RgbImage, boxes: Vec<u8>) -> PathBuf {
    use spherecut_media::inject::{EntryEdit, TrackKind};
    let settings = VideoSettings {
        encoder: "libx264".into(),
        width: img.width,
        height: img.height,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: [("qp", "0"), ("preset", "veryfast"), ("threads", "1")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .to_vec(),
        color: CLIP_COLOR,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let mut w = Mp4Writer::create(&dir.join(format!("{name}.mp4")), &settings, None).unwrap();
    w.push_video(&yuv::to_yuv(
        img,
        8,
        &YuvFormat::tagged(&CLIP_COLOR, Chroma::Yuv420),
    ))
    .unwrap();
    w.finish_with_edits(&[EntryEdit {
        track: TrackKind::Video,
        boxes,
    }])
    .unwrap()
}

/// `a` above `b`, or beside it.
fn stack(a: &RgbImage, b: &RgbImage, beside: bool) -> RgbImage {
    let (w, h) = (a.width as usize, a.height as usize);
    let pixels = if beside {
        (0..h)
            .flat_map(|y| {
                a.pixels[y * w..(y + 1) * w]
                    .iter()
                    .chain(&b.pixels[y * w..(y + 1) * w])
                    .copied()
            })
            .collect()
    } else {
        a.pixels.iter().chain(&b.pixels).copied().collect()
    };
    RgbImage {
        width: if beside { a.width * 2 } else { a.width },
        height: if beside { a.height } else { a.height * 2 },
        pixels,
    }
}

/// The warnings of section 5's table, each with its code and message, and
/// the picture they describe: the left eye of a stereo pair, the whole
/// picture of an unknown layout, black outside a cropped panorama, and a
/// pose left unapplied. (`anisotropy_unavailable` needs a device without
/// anisotropic filtering, which lavapipe isn't.)
#[test]
fn what_frame_renders_differently_it_warns_about() {
    use spherecut_media::spherical::{StereoMode, write};
    let dir = TempDir::new("warnings");
    let grid = render_equirect(Pattern::Grid, 1024, 512, 0, 2);
    let other = render_equirect(Pattern::HighFrequency, 1024, 512, 0, 2);
    let full = [0u32; 4];
    let mono = |pose, bounds| [write::st3d(StereoMode::Mono), sv3d(pose, bounds)].concat();
    let quarter = 1u32 << 30;
    // The front half of the sphere, lon −90° to 90°: the grid's middle
    // half.
    let front = RgbImage {
        width: 512,
        height: 512,
        pixels: (0..512 * 512)
            .map(|i| grid.pixels[(i / 512) * 1024 + 256 + i % 512])
            .collect(),
    };
    // (name, picture, the boxes in its sample entry, warning, message)
    type Case = (
        &'static str,
        RgbImage,
        Vec<u8>,
        Option<&'static str>,
        String,
    );
    let cases: Vec<Case> =
        vec![
        (
            "top-bottom",
            stack(&grid, &other, false),
            [write::st3d(StereoMode::TopBottom), sv3d([0.0; 3], full)].concat(),
            Some("stereo_left_eye"),
            "stereo, top-bottom: rendering the left eye (the top half)".into(),
        ),
        (
            "left-right",
            stack(&grid, &other, true),
            [write::st3d(StereoMode::LeftRight), sv3d([0.0; 3], full)].concat(),
            Some("stereo_left_eye"),
            "stereo, left-right: rendering the left eye (the left half)".into(),
        ),
        (
            "custom",
            grid.clone(),
            [write::st3d(StereoMode::StereoCustom), sv3d([0.0; 3], full)].concat(),
            Some("stereo_unknown"),
            "stereo layout StereoCustom is unknown: rendering the whole picture, as mono".into(),
        ),
        (
            "cropped",
            front,
            mono([0.0; 3], [0, 0, quarter, quarter]),
            Some("crop_applied"),
            "the picture covers part of the sphere (cropped by 0.25 left, 0 top, 0.25 right and \
             0 bottom): the rest is black"
                .into(),
        ),
        (
            "posed",
            grid.clone(),
            mono([30.0, -10.0, 5.0], full),
            Some("pose_not_applied"),
            "the file's pose (yaw 30°, pitch -10°, roll 5°) is not applied (M2)".into(),
        ),
        ("plain", grid.clone(), mono([0.0; 3], full), None, String::new()),
    ];
    let out = dir.join("out.png");
    for (name, img, boxes, code, message) in &cases {
        let clip = tagged_clip(&dir, name, img, boxes.clone());
        // The F marker ahead, and what is behind.
        let ran = frame(
            &clip,
            &out,
            &["--fov", "10", "--size", "33x33", "--overwrite"],
        );
        ran.ok();
        let w = ran.doc["warnings"].as_array().unwrap();
        match code {
            Some(code) => {
                assert_eq!(w.len(), 1, "{name}: {}", ran.doc);
                assert!(has_two_keys(&w[0]), "{name}: {}", ran.doc);
                assert_eq!(w[0]["code"], *code, "{name}");
                assert_eq!(w[0]["message"], message.as_str(), "{name}");
                // People see it on stderr.
                assert!(
                    ran.stderr.contains(message.as_str()),
                    "{name}: {}",
                    ran.stderr
                );
            }
            None => assert!(w.is_empty(), "{name}: {}", ran.doc),
        }
        let centre = rgb(&load(&out), 16, 16);
        assert!(
            far(centre, marker('F')) <= 0.1,
            "{name}: the centre is {centre:?}"
        );
        let ran = frame(
            &clip,
            &out,
            &[
                "--fov",
                "10",
                "--size",
                "33x33",
                "--yaw",
                "180",
                "--overwrite",
            ],
        );
        ran.ok();
        let behind = rgb(&load(&out), 16, 16);
        let want = if *name == "cropped" {
            [0.0; 3]
        } else {
            marker('B')
        };
        assert!(far(behind, want) <= 0.1, "{name}: behind is {behind:?}");
    }

    // A crop on one side and the top only, lon −90° to 180° and lat 67.5°
    // down: each side lands where its bound says, left from right and top
    // from bottom, on the GPU and on the CPU reference alike.
    let (left, top) = (256usize, 64usize);
    let (w, h) = (1024 - left, 512 - top);
    let part = RgbImage {
        width: w as u32,
        height: h as u32,
        pixels: (0..w * h)
            .map(|i| grid.pixels[(i / w + top) * 1024 + left + i % w])
            .collect(),
    };
    let clip = tagged_clip(
        &dir,
        "one-sided",
        &part,
        mono([0.0; 3], [quarter / 2, 0, quarter, 0]),
    );
    let black = [0.0; 3];
    // (the view, the centre pixel's colour, or None for anything but black)
    let views: [(&[&str], Option<[f64; 3]>); 5] = [
        (&[], Some(marker('F'))),
        (&["--yaw", "90"], Some(marker('R'))),
        (&["--yaw", "-135"], Some(black)),
        (&["--pitch", "80"], Some(black)),
        (&["--pitch", "-80"], None),
    ];
    for renderer in [&["--adapter", "cpu"][..], &["--reference"]] {
        for (view, want) in &views {
            let mut args = vec!["--fov", "10", "--size", "33x33", "--overwrite"];
            args.extend_from_slice(renderer);
            args.extend_from_slice(view);
            let ran = frame(&clip, &out, &args);
            ran.ok();
            assert_eq!(
                ran.doc["warnings"][0]["message"],
                "the picture covers part of the sphere (cropped by 0.25 left, 0.125 top, 0 right \
                 and 0 bottom): the rest is black",
                "{args:?}"
            );
            let centre = rgb(&load(&out), 16, 16);
            match want {
                Some(want) => assert!(far(centre, *want) <= 0.1, "{args:?}: {centre:?}"),
                None => assert!(far(centre, black) > 0.2, "{args:?}: {centre:?}"),
            }
        }
    }
}

/// Whether a warning has exactly its code and its message.
fn has_two_keys(w: &Value) -> bool {
    let mut keys: Vec<&str> = w.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys == ["code", "message"]
}

// ---------------------------------------------------------------------
// The pipeline goldens

const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens");
const MANIFEST: &str = "manifest.json";
const UPDATE: &str = "SPHERECUT_UPDATE_GOLDENS";
const LOWER: &str = "lower";
const REGENERATE: &str =
    "SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-cli --test frame pipeline_goldens";
/// The render goldens' rules (D32).
const OTHER_PSNR_DB: f64 = 48.0;
const OTHER_SSIM: f64 = 0.995;
const OTHER_MAX_LSB: f64 = 4.0;
const CPU_MARGIN_DB: f64 = 3.0;
const CPU_FLOOR_DB: f64 = 36.0;
const CPU_MAX_CHANNEL: f64 = 0.1;
const FLOOR_DROP_DB: f64 = 0.5;
/// The swscale cross-check's bound, in 8-bit steps, and how far apart a
/// swapped matrix or range must put the two.
const SWSCALE_MAX_LSB: f64 = 1.0;
const SWSCALE_CONTROL_OVER_LSB: f64 = 1.0;
/// How far from the exact conversion of its codes the render may be, in
/// 8-bit steps: half a step of rounding, and f16's 0.12 near 1 (0.60
/// measured on lavapipe; swscale itself is up to 0.94 from it at limited
/// range, so the two are compared as 8-bit values).
const EXACT_MAX_LSB: f64 = 0.75;

/// The driver build a run rendered on, from the JSON's `gpu`.
#[derive(Debug, Clone)]
struct Driver {
    adapter: String,
    driver: String,
    driver_info: String,
}

impl Driver {
    fn of(result: &Value) -> Driver {
        let g = &result["gpu"];
        let s = |k: &str| g[k].as_str().unwrap_or("?").to_string();
        Driver {
            adapter: s("name"),
            driver: s("driver"),
            driver_info: s("driver_info"),
        }
    }

    fn llvm(&self) -> Option<String> {
        [&self.adapter, &self.driver_info].iter().find_map(|s| {
            let at = s.find("LLVM ")? + 5;
            let v: String = s[at..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            (!v.is_empty()).then_some(v)
        })
    }

    fn describe(&self) -> String {
        format!("{} ({} {})", self.adapter, self.driver, self.driver_info)
    }
}

/// The wgpu version in `Cargo.lock`.
fn wgpu_version() -> String {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(lock).unwrap_or_default();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "name = \"wgpu\""
            && let Some(v) = lines
                .next()
                .and_then(|v| v.trim().strip_prefix("version = "))
        {
            return v.trim_matches('"').to_string();
        }
    }
    "unknown".into()
}

fn head_commit() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Today in UTC, as YYYY-MM-DD (Hinnant's days-to-civil).
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A JSON number, to 4 decimals; +∞ as a string.
fn num(v: f64) -> Value {
    if v.is_finite() {
        json!((v * 1e4).round() / 1e4)
    } else {
        json!("inf")
    }
}

fn psnr_of(a: &Image, b: &Image) -> f64 {
    let (w, h) = (a.width as usize, a.height as usize);
    psnr(View::new(w, h, 3, &unit(a)), View::new(w, h, 3, &unit(b))).unwrap()
}

fn ssim_of(a: &Image, b: &Image) -> f64 {
    let (w, h) = (a.width as usize, a.height as usize);
    ssim(View::new(w, h, 3, &unit(a)), View::new(w, h, 3, &unit(b))).unwrap()
}

fn max_diff(a: &Image, b: &Image) -> f64 {
    unit(a)
        .iter()
        .zip(unit(b))
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f64::max)
}

/// FNV-1a over every row of every plane of the first frame `clip`
/// decodes in software, without padding: what the manifest knows a
/// source by.
fn source_hash(clip: &Path) -> String {
    let mut d = FfmpegBackend::new()
        .open_video(clip, &VideoRequest::default())
        .unwrap();
    let NextFrame::Frame(f) = d.next_frame(&StopToken::never()).unwrap() else {
        panic!("{}: no frame", clip.display());
    };
    let mut h = Fnv64::new();
    let layout = f.layout().clone();
    for (i, desc) in layout.planes.iter().enumerate() {
        let p = f.plane(i).unwrap();
        let row = desc.row_bytes(p.width);
        for y in 0..p.height {
            h.write(p.row(y, row).unwrap());
        }
    }
    format!("{:016x}", h.finish())
}

/// The pipeline goldens' run: every row's checks, gathered, then blessed
/// or reported together.
struct Run {
    bless: bool,
    lower_floors: bool,
    manifest: Result<Value, String>,
    driver: Option<Driver>,
    row: String,
    thresholds: Map<String, Value>,
    measured: Map<String, Value>,
    failures: Vec<String>,
    images: Map<String, Value>,
    /// The entry of the image row under way, which goes into `images`
    /// with every number it measured once the row ends.
    pending: Option<Map<String, Value>>,
    checks: Map<String, Value>,
    pngs: BTreeMap<String, Vec<u8>>,
    floors: Vec<String>,
    scratch: TempDir,
}

impl Run {
    fn new() -> Run {
        let update = std::env::var(UPDATE).ok();
        let path = Path::new(GOLDENS).join(MANIFEST);
        let manifest = std::fs::read_to_string(&path)
            .map_err(|e| format!("no manifest at {} ({e})", path.display()))
            .and_then(|t| {
                serde_json::from_str(&t)
                    .map_err(|e| format!("{} doesn't parse: {e}", path.display()))
            });
        Run {
            bless: update.is_some(),
            lower_floors: update.as_deref() == Some(LOWER),
            manifest,
            driver: None,
            row: String::new(),
            thresholds: Map::new(),
            measured: Map::new(),
            failures: Vec::new(),
            images: Map::new(),
            pending: None,
            checks: Map::new(),
            pngs: BTreeMap::new(),
            floors: Vec::new(),
            scratch: TempDir::new("goldens"),
        }
    }

    fn begin(&mut self, row: &str) {
        self.end_image();
        self.row = row.into();
        self.thresholds.clear();
        self.measured.clear();
    }

    /// Ends the image row under way, if any: its entry, with what it
    /// measured after its image was compared too (its twins and
    /// controls), goes into the manifest.
    fn end_image(&mut self) {
        if let Some(mut entry) = self.pending.take() {
            let mut m = self.stamp();
            m.append(&mut entry);
            self.images.insert(self.row.clone(), Value::Object(m));
        }
    }

    fn describe(&self) -> String {
        self.driver
            .as_ref()
            .map_or_else(|| "no adapter yet".into(), Driver::describe)
    }

    fn fail(&mut self, msg: impl std::fmt::Display) {
        let line = format!("{}: {msg} [on {}]", self.row, self.describe());
        eprintln!("FAIL {line}");
        self.failures.push(line);
    }

    fn check(&mut self, ok: bool, msg: impl std::fmt::Display) {
        if !ok {
            self.fail(msg);
        }
    }

    fn measure(&mut self, key: &str, v: f64) {
        eprintln!("{}: {key} = {v:.4}", self.row);
        self.measured.insert(key.into(), num(v));
    }

    fn at_least(&mut self, key: &str, v: f64, min: f64) -> bool {
        self.measure(key, v);
        self.thresholds.insert(format!("{key}_min"), num(min));
        let ok = v >= min;
        self.check(ok, format!("{key} is {v:.4}, below {min}"));
        ok
    }

    fn at_most(&mut self, key: &str, v: f64, max: f64) -> bool {
        self.measure(key, v);
        self.thresholds.insert(format!("{key}_max"), num(max));
        let ok = v <= max;
        self.check(ok, format!("{key} is {v:.4}, above {max}"));
        ok
    }

    fn over(&mut self, key: &str, v: f64, bound: f64) -> bool {
        self.measure(key, v);
        self.thresholds.insert(format!("{key}_over"), num(bound));
        let ok = v > bound;
        self.check(ok, format!("{key} is {v:.4}, not over {bound}"));
        ok
    }

    fn blessed(&self, key: &str) -> Result<f64, String> {
        let m = self.manifest.as_ref().map_err(Clone::clone)?;
        ["images", "checks"]
            .iter()
            .find_map(|k| m[k][&self.row]["thresholds"][key].as_f64())
            .ok_or_else(|| {
                format!(
                    "the manifest has no {key} for {}: bless with `{REGENERATE}`",
                    self.row
                )
            })
    }

    /// The render goldens' calibrated floor: `measured − margin` when
    /// blessing, never below `floor`, and never lowered by more than
    /// [`FLOOR_DROP_DB`] unasked; the manifest's otherwise.
    fn calibrated(&mut self, key: &str, v: f64, margin: f64, floor: f64) -> bool {
        let tkey = format!("{key}_min");
        let min = if self.bless {
            let new = (((v - margin) * 100.0).floor() / 100.0).max(floor);
            let old = self.blessed(&tkey).ok();
            self.floors.push(format!(
                "{} {tkey}: {} → {new}",
                self.row,
                old.map_or_else(|| "new".into(), |o| o.to_string())
            ));
            if let Some(old) = old
                && new < old - FLOOR_DROP_DB
                && !self.lower_floors
            {
                self.fail(format!(
                    "blessing would lower {tkey} from {old} to {new} (measured {v:.2}), by more \
                     than {FLOOR_DROP_DB} dB: to accept it, bless with {UPDATE}={LOWER}"
                ));
            }
            new
        } else {
            match self.blessed(&tkey) {
                Ok(m) => m,
                Err(e) => {
                    self.measure(key, v);
                    self.fail(e);
                    return false;
                }
            }
        };
        self.at_least(key, v, min)
    }

    /// `frame` on `clip` with `args` into the scratch directory, on
    /// lavapipe or with `--reference`: the result and the image, or a
    /// failure recorded.
    fn render(&mut self, clip: &Path, name: &str, args: &[&str]) -> Option<(Value, Image)> {
        self.render_in(clip, name, args, &[])
    }

    /// [`render`](Self::render) with `env` set.
    fn render_in(
        &mut self,
        clip: &Path,
        name: &str,
        args: &[&str],
        env: &[(&str, &OsStr)],
    ) -> Option<(Value, Image)> {
        let out = self.scratch.join(format!("{}-{name}.png", self.row));
        let mut a = args.to_vec();
        a.push("--overwrite");
        let ran = frame_in(clip, &out, &a, env);
        if ran.status != Some(0) {
            self.fail(format!("frame {args:?}: {} / {}", ran.doc, ran.stderr));
            return None;
        }
        let result = ran.doc["result"].clone();
        if self.driver.is_none() && !result["gpu"].is_null() {
            self.driver = Some(Driver::of(&result));
        }
        Some((result, load(&out)))
    }

    /// The row's render and its `--reference` twin: the calibrated CPU
    /// floor, and no channel further than [`CPU_MAX_CHANNEL`].
    fn cpu_check(&mut self, got: &Image, cpu: &Image) {
        let p = psnr_of(got, cpu);
        let ok = self.calibrated("cpu_psnr_db", p, CPU_MARGIN_DB, CPU_FLOOR_DB);
        let m = max_diff(got, cpu);
        let ok = self.at_most("cpu_max_channel", m, CPU_MAX_CHANNEL) && ok;
        if !ok {
            self.write_diffs("cpu", got, cpu);
        }
    }

    fn write_diffs(&mut self, what: &str, got: &Image, want: &Image) {
        let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let dir = tmp.parent().unwrap_or(tmp).join("golden-diffs");
        let name = format!("cli-{}-{what}", self.row);
        let (g, w) = (unit(got), unit(want));
        let diff: Vec<f64> = g
            .iter()
            .zip(&w)
            .map(|(a, b)| ((a - b).abs() * 16.0).min(1.0))
            .collect();
        let rgb8 = |data: &[f64]| Image {
            width: got.width,
            height: got.height,
            pixels: Pixels::Rgb8(
                data.iter()
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                    .collect(),
            ),
        };
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            for (suffix, data) in [("got", &g), ("want", &w), ("diff", &diff)] {
                let png = rgb8(data).to_png().map_err(std::io::Error::other)?;
                std::fs::write(dir.join(format!("{name}-{suffix}.png")), png)?;
            }
            Ok(())
        });
        match written {
            Ok(()) => eprintln!("{}: wrote {name}-*.png to {}", self.row, dir.display()),
            Err(e) => eprintln!("{}: couldn't write diffs: {e}", self.row),
        }
    }

    fn stamp(&self) -> Map<String, Value> {
        let mut m = Map::new();
        let d = self.driver.clone().unwrap_or(Driver {
            adapter: "?".into(),
            driver: "?".into(),
            driver_info: "?".into(),
        });
        m.insert("adapter".into(), json!(d.adapter));
        m.insert("driver".into(), json!(d.driver));
        m.insert("driver_info".into(), json!(d.driver_info));
        m.insert("llvm".into(), json!(d.llvm()));
        m.insert("wgpu".into(), json!(wgpu_version()));
        m.insert("commit".into(), json!(head_commit()));
        m.insert("date".into(), json!(today()));
        m.insert("regenerate".into(), json!(REGENERATE));
        m.insert("thresholds".into(), Value::Object(self.thresholds.clone()));
        m.insert("measured".into(), Value::Object(self.measured.clone()));
        m
    }

    /// Ends a row that stores no image.
    fn end_check(&mut self, what: &str, source: Value, hashes: Value, view: Value) {
        let mut m = self.stamp();
        m.insert("what".into(), json!(what));
        m.insert("source".into(), source);
        m.insert("source_fnv64".into(), hashes);
        m.insert("view".into(), view);
        self.checks.insert(self.row.clone(), Value::Object(m));
    }

    /// The row's image against its stored PNG, or kept for blessing. The
    /// row ends at the next [`Run::begin`] or at [`Run::finish`], so that
    /// the checks after this one are recorded with it.
    fn stored(&mut self, what: &str, got: &Image, source: Value, hash: &str, args: &[&str]) {
        let png = format!("{}.png", self.row);
        let mut entry = Map::new();
        if self.bless {
            match got.to_png() {
                Ok(bytes) => {
                    entry.insert("png_fnv64".into(), json!(format!("{:016x}", fnv64(&bytes))));
                    self.pngs.insert(png.clone(), bytes);
                }
                Err(e) => self.fail(format!("encoding {png}: {e}")),
            }
        } else {
            self.compare_stored(&png, hash, got);
        }
        self.thresholds
            .insert("same_driver".into(), json!("identical pixels"));
        self.thresholds.insert(
            "other_driver".into(),
            json!({"psnr_db_min": OTHER_PSNR_DB, "ssim_min": OTHER_SSIM, "max_lsb": OTHER_MAX_LSB}),
        );
        entry.insert("what".into(), json!(what));
        entry.insert("png".into(), json!(png));
        entry.insert("source".into(), source);
        entry.insert("source_fnv64".into(), json!(hash));
        entry.insert("view".into(), json!(args.join(" ")));
        self.pending = Some(entry);
    }

    fn compare_stored(&mut self, png: &str, hash: &str, got: &Image) {
        let entry = match &self.manifest {
            Ok(m) => m["images"][&self.row].clone(),
            Err(e) => {
                let e = e.clone();
                self.fail(format!("{e}: bless with `{REGENERATE}`"));
                return;
            }
        };
        if entry.is_null() {
            self.fail(format!(
                "{png} isn't in the manifest: bless with `{REGENERATE}`"
            ));
            return;
        }
        let blessed_hash = entry["source_fnv64"].as_str().unwrap_or("");
        if blessed_hash != hash {
            self.fail(format!(
                "the synthetic source changed (its decoded planes hash {hash}, blessed as \
                 {blessed_hash}): synth, the encoder or the decoder moved, not the renderer, so \
                 the render isn't compared; check them and bless again with `{REGENERATE}`"
            ));
            return;
        }
        let want = match std::fs::read(Path::new(GOLDENS).join(png))
            .map_err(|e| e.to_string())
            .and_then(|b| Image::from_png(&b).map_err(|e| e.to_string()))
        {
            Ok(w) => w,
            Err(e) => {
                self.fail(format!("reading {png}: {e}"));
                return;
            }
        };
        if (got.width, got.height) != (want.width, want.height) {
            self.fail(format!("{png}: the render's size isn't the golden's"));
            return;
        }
        let d = self.driver.clone();
        let same_driver = d.is_some_and(|d| {
            entry["adapter"].as_str() == Some(&d.adapter)
                && entry["driver_info"].as_str() == Some(&d.driver_info)
        });
        let identical = got.pixels == want.pixels;
        let (p, s, m) = (
            psnr_of(got, &want),
            ssim_of(got, &want),
            max_diff(got, &want) * 255.0,
        );
        self.measure("golden_psnr_db", p);
        self.measure("golden_ssim", s);
        self.measure("golden_max_lsb", m);
        let numbers = format!("PSNR {p:.2} dB, SSIM {s:.5}, max {m:.1} LSB");
        let fault = if same_driver {
            (!identical).then(|| {
                format!(
                    "not identical to the golden at the driver build it was blessed on: {numbers}"
                )
            })
        } else {
            (p < OTHER_PSNR_DB || s < OTHER_SSIM || m > OTHER_MAX_LSB).then(|| {
                format!(
                    "outside the other-driver tolerance (PSNR ≥ {OTHER_PSNR_DB} dB, SSIM ≥ \
                     {OTHER_SSIM}, max ≤ {OTHER_MAX_LSB} LSB): {numbers}"
                )
            })
        };
        if let Some(f) = fault {
            self.fail(format!("{png}: {f}"));
            self.write_diffs("golden", got, &want);
        }
    }

    fn finish(mut self) {
        self.end_image();
        let list = self.failures.join("\n  ");
        if self.bless {
            assert!(
                self.failures.is_empty(),
                "refusing to bless: {} checks failed, and nothing was written:\n  {list}",
                self.failures.len()
            );
            let dir = Path::new(GOLDENS);
            std::fs::create_dir_all(dir).unwrap();
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".png") && !self.pngs.contains_key(&name) {
                    std::fs::remove_file(e.path()).unwrap();
                }
            }
            for (name, bytes) in &self.pngs {
                std::fs::write(dir.join(name), bytes).unwrap();
            }
            let manifest = json!({
                "schema": "spherecut.goldens/1",
                "about": "The pipeline goldens (M1 plan section 6): files encoded by synth, \
                          decoded in software and rendered by `spherecut frame` on lavapipe, \
                          with each check's thresholds and what it measured when blessed.",
                "regenerate": REGENERATE,
                "images": self.images,
                "checks": self.checks,
            });
            let text = serde_json::to_string_pretty(&manifest).unwrap() + "\n";
            std::fs::write(dir.join(MANIFEST), text).unwrap();
            use std::io::Write;
            let _ = std::io::stderr().lock().write_all(
                format!(
                    "blessed {} images and {} checks into {}, with these floors (old → new):\n  \
                     {}\n",
                    self.pngs.len(),
                    self.checks.len(),
                    dir.display(),
                    self.floors.join("\n  ")
                )
                .as_bytes(),
            );
            return;
        }
        // Every number a row measures is in the manifest from its blessing
        // (the comparison with the stored image is the plain run's own).
        if let Ok(m) = &self.manifest {
            let mut missing = Vec::new();
            for (kind, rows) in [("images", &self.images), ("checks", &self.checks)] {
                for (row, entry) in rows {
                    let Some(measured) = entry["measured"].as_object() else {
                        continue;
                    };
                    for key in measured.keys().filter(|k| !k.starts_with("golden_")) {
                        if m[kind][row]["measured"][key].is_null() {
                            missing.push(format!("{row}: {key}"));
                        }
                    }
                }
            }
            if !missing.is_empty() {
                self.failures.push(format!(
                    "the manifest doesn't record {}: bless again with `{REGENERATE}`",
                    missing.join(", ")
                ));
            }
        }
        let list = self.failures.join("\n  ");
        assert!(
            self.failures.is_empty(),
            "{} pipeline golden checks failed on {}:\n  {list}",
            self.failures.len(),
            self.describe()
        );
    }
}

/// A clip from `planes` (one frame), tagged `color`, in `layout`, through
/// x265's lossless mode.
fn lossless_clip(
    path: &Path,
    layout: PixelLayout,
    size: (u32, u32),
    color: ColorTags,
    planes: &YuvPlanes,
) -> PathBuf {
    let settings = VideoSettings {
        encoder: "libx265".into(),
        width: size.0,
        height: size.1,
        rate: FrameRate::FPS_30,
        layout,
        options: [
            ("preset", "veryfast"),
            ("threads", "1"),
            ("x265-params", "lossless=1:log-level=error"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .to_vec(),
        color,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let mut w = Mp4Writer::create(path, &settings, None).unwrap();
    w.push_video(planes).unwrap();
    w.finish().unwrap()
}

/// One picture as rawvideo in Matroska, in `pix` (NV12 or I420), from
/// 8-bit 4:2:0 `planes`: the one way to have FFmpeg decode to NV12
/// without hardware.
// Writes a file with ffmpeg-next alone, apart from Spherecut's confined
// open (D24), as the faststart remux does.
#[allow(clippy::disallowed_methods)]
fn raw_mkv(path: &Path, pix: ff::format::Pixel, size: (u32, u32), planes: &[Vec<u8>; 3]) {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut frame = ff::frame::Video::new(pix, size.0, size.1);
    let stride0 = frame.stride(0);
    for y in 0..h {
        frame.data_mut(0)[y * stride0..][..w].copy_from_slice(&planes[0][y * w..][..w]);
    }
    let (cw, ch) = (w / 2, h / 2);
    if pix == ff::format::Pixel::NV12 {
        let s = frame.stride(1);
        let uv = frame.data_mut(1);
        for y in 0..ch {
            for x in 0..cw {
                uv[y * s + 2 * x] = planes[1][y * cw + x];
                uv[y * s + 2 * x + 1] = planes[2][y * cw + x];
            }
        }
    } else {
        for (p, plane) in planes.iter().enumerate().skip(1) {
            let s = frame.stride(p);
            for y in 0..ch {
                frame.data_mut(p)[y * s..][..cw].copy_from_slice(&plane[y * cw..][..cw]);
            }
        }
    }
    frame.set_pts(Some(0));
    frame.set_color_range(ff::color::Range::MPEG);
    frame.set_color_space(ff::color::Space::BT709);
    let codec = ff::encoder::find(ff::codec::Id::RAWVIDEO).unwrap();
    let mut octx = ff::format::output_as(path, "matroska").unwrap();
    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .unwrap();
    let tb = ff::Rational::new(1, 30);
    enc.set_width(size.0);
    enc.set_height(size.1);
    enc.set_format(pix);
    enc.set_time_base(tb);
    enc.set_frame_rate(Some(ff::Rational::new(30, 1)));
    enc.set_color_range(ff::color::Range::MPEG);
    enc.set_colorspace(ff::color::Space::BT709);
    let mut enc = enc.open_as(codec).unwrap();
    let mut st = octx.add_stream(codec).unwrap();
    st.set_parameters(&enc);
    st.set_time_base(tb);
    octx.write_header().unwrap();
    let st_tb = octx.stream(0).unwrap().time_base();
    enc.send_frame(&frame).unwrap();
    enc.send_eof().unwrap();
    let mut pkt = ff::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(0);
        pkt.rescale_ts(tb, st_tb);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
}

/// The rows, in order: each renders through `frame` on lavapipe and with
/// `--reference`.
#[test]
fn pipeline_goldens() {
    let mut run = Run::new();
    hevc10_lossless(&mut run);
    h264_full_range(&mut run);
    nv12_vs_yuv420p(&mut run);
    yuv422p10_full_chroma(&mut run);
    swscale_cross_check(&mut run);
    run.finish();
}

/// A stored row: `clip` through `args` on the GPU, stored, and against
/// `--reference`. Returns the GPU's result.
fn stored_row(
    run: &mut Run,
    row: &str,
    what: &str,
    clip: &Path,
    source: Value,
    args: &[&str],
) -> Option<(Value, Image)> {
    run.begin(row);
    let hash = source_hash(clip);
    let (result, got) = run.render(clip, "gpu", args)?;
    let mut cpu_args = args.to_vec();
    cpu_args.push("--reference");
    // With no Vulkan driver at all, so that the CPU renders it whatever
    // the JSON says: the floors alone would pass a GPU render too.
    let no_icd = run.scratch.join("no-such-icd.json");
    let no_gpu = [
        ("VK_DRIVER_FILES", no_icd.as_os_str()),
        ("VK_ICD_FILENAMES", no_icd.as_os_str()),
    ];
    let (cpu_result, cpu) = run.render_in(clip, "cpu", &cpu_args, &no_gpu)?;
    run.check(
        cpu_result["renderer"] == "reference" && cpu_result["gpu"].is_null(),
        format!("--reference didn't render on the CPU: {cpu_result}"),
    );
    run.check(
        cpu_result["source"] == result["source"] && cpu_result["color"] == result["color"],
        "--reference decoded another picture",
    );
    run.cpu_check(&got, &cpu);
    run.stored(what, &got, source, &hash, args);
    Some((result, got))
}

const VIEW_YPR: &[&str] = &[
    "--fov", "90", "--yaw", "30", "--pitch", "20", "--roll", "10", "--size", "256x144",
];

fn hevc10_lossless(run: &mut Run) {
    let dir = run.scratch.join("hevc10");
    let spec = Spec {
        name: "hevc10".into(),
        width: 1024,
        height: 512,
        frames: 1,
        codec: Codec::HevcLossless,
        bits: 10,
        spherical: true,
        ..Spec::default()
    };
    let clip = media::write(&spec, &dir).unwrap();
    let mut args = VIEW_YPR.to_vec();
    args.extend(["--depth", "16"]);
    let source = json!("synth grid 1024x512, x265 lossless 10-bit 4:2:0, BT.709 limited");
    if let Some((r, img)) = stored_row(
        run,
        "hevc10_lossless",
        "lossless 10-bit HEVC through the whole pipeline, decoded as yuv420p10le, 16-bit PNG",
        &clip,
        source,
        &args,
    ) {
        run.check(
            r["source"]["pix_fmt"] == "yuv420p10le" && r["source"]["codec"] == "hevc",
            format!("decoded as {}", r["source"]),
        );
        run.check(r["depth"] == 16, format!("depth {}", r["depth"]));
        run.check(
            matches!(img.pixels, Pixels::Rgb16(_)),
            "the PNG isn't 16-bit",
        );
    }
}

fn h264_full_range(run: &mut Run) {
    let dir = run.scratch.join("full");
    let spec = Spec {
        name: "full".into(),
        width: 1024,
        height: 512,
        frames: 1,
        codec: Codec::H264Lossless,
        spherical: true,
        color: ColorTags {
            range: 2,
            ..CLIP_COLOR
        },
        ..Spec::default()
    };
    let clip = media::write(&spec, &dir).unwrap();
    let args = [
        "--projection",
        "fisheye",
        "--fov",
        "180",
        "--size",
        "192x192",
    ];
    let source = json!("synth grid 1024x512, x264 lossless 8-bit 4:2:0, BT.709 full range");
    if let Some((r, _)) = stored_row(
        run,
        "h264_full_range",
        "full-range H.264 through the whole pipeline",
        &clip,
        source,
        &args,
    ) {
        run.check(
            r["color"]["range"] == "full" && r["color"]["assumed"]["range"] == false,
            format!("colour {}", r["color"]),
        );
        run.check(
            r["source"]["pix_fmt"] == "yuvj420p" || r["source"]["pix_fmt"] == "yuv420p",
            format!("decoded as {}", r["source"]),
        );
    }
}

fn nv12_vs_yuv420p(run: &mut Run) {
    let dir = run.scratch.join("nv12");
    std::fs::create_dir_all(&dir).unwrap();
    let size = (512u32, 256u32);
    let img = render_equirect(Pattern::Grid, size.0, size.1, 0, 2);
    let YuvPlanes::U8(planes) = yuv::to_yuv(&img, 8, &YuvFormat::BT709_420) else {
        unreachable!()
    };
    let nv12 = dir.join("nv12.mkv");
    let i420 = dir.join("i420.mkv");
    raw_mkv(&nv12, ff::format::Pixel::NV12, size, &planes);
    raw_mkv(&i420, ff::format::Pixel::YUV420P, size, &planes);
    let mut args = VIEW_YPR.to_vec();
    args.push("--equirect");
    let source = json!("synth grid 512x256, rawvideo NV12 in Matroska, BT.709 limited");
    let Some((r, got)) = stored_row(
        run,
        "nv12_vs_yuv420p",
        "an NV12 decode; the same picture as yuv420p must give the same bytes",
        &nv12,
        source,
        &args,
    ) else {
        return;
    };
    // The twin, against the same row.
    let Some((r2, twin)) = run.render(&i420, "i420", &args) else {
        return;
    };
    run.check(
        r["source"]["pix_fmt"] == "nv12" && r2["source"]["pix_fmt"] == "yuv420p",
        format!("decoded as {} and {}", r["source"], r2["source"]),
    );
    let identical = got.pixels == twin.pixels;
    run.measure("twin_identical", f64::from(u8::from(identical)));
    run.check(identical, "the NV12 and yuv420p renders differ");
    if let Some(e) = run.pending.as_mut() {
        e.insert("twin_fnv64".into(), json!(source_hash(&i420)));
    }
}

/// 4:2:2 has full vertical chroma resolution: chroma that changes from
/// every row to the next, flat across, renders as its 4:4:4 twin does,
/// while its 4:2:0 coding, which can't hold it, doesn't.
fn yuv422p10_full_chroma(run: &mut Run) {
    let dir = run.scratch.join("422");
    std::fs::create_dir_all(&dir).unwrap();
    let size = (1024u32, 512u32);
    let (w, h) = (size.0 as usize, size.1 as usize);
    // Luma flat at mid grey; Cb and Cr swing ±160 codes (10-bit) from
    // one row to the next, which 4:2:0's chroma rows, two picture rows
    // each, can't hold.
    let luma = vec![502u16; w * h];
    let chroma = |cw: usize, sign: i32| -> Vec<u16> {
        (0..h)
            .flat_map(|y| {
                let v = 512 + sign * if y % 2 == 0 { 160 } else { -160 };
                std::iter::repeat_n(v as u16, cw)
            })
            .collect()
    };
    let p422 = YuvPlanes::U16([luma.clone(), chroma(w / 2, 1), chroma(w / 2, -1)]);
    let p444 = YuvPlanes::U16([luma.clone(), chroma(w, 1), chroma(w, -1)]);
    // Each 4:2:0 chroma row covers two of the rows, which differ: the
    // nearest it can come is their mean, flat.
    let p420 = YuvPlanes::U16([luma, vec![512; w * h / 4], vec![512; w * h / 4]]);
    let tags = ColorTags::BT709_LIMITED;
    let c422 = lossless_clip(
        &dir.join("422.mp4"),
        PixelLayout::Yuv422p10,
        size,
        tags,
        &p422,
    );
    let c444 = lossless_clip(
        &dir.join("444.mp4"),
        PixelLayout::Yuv444p10,
        size,
        tags,
        &p444,
    );
    let c420 = lossless_clip(
        &dir.join("420.mp4"),
        PixelLayout::Yuv420p10,
        size,
        tags,
        &p420,
    );
    let args = ["--equirect", "--fov", "20", "--size", "192x192"];
    let source = json!(
        "1024x512 10-bit 4:2:2, x265 lossless: flat luma, Cb and Cr alternating row by row, \
         BT.709 limited"
    );
    let Some((r, got)) = stored_row(
        run,
        "yuv422p10_full_chroma",
        "4:2:2 10-bit keeps its full vertical chroma: the same as its 4:4:4 twin, unlike 4:2:0",
        &c422,
        source,
        &args,
    ) else {
        return;
    };
    run.check(
        r["source"]["pix_fmt"] == "yuv422p10le",
        format!("decoded as {}", r["source"]),
    );
    let Some((_, twin)) = run.render(&c444, "444", &args) else {
        return;
    };
    let Some((_, control)) = run.render(&c420, "420", &args) else {
        return;
    };
    let d = max_diff(&got, &twin) * 255.0;
    run.at_most("vs_444_max_lsb", d, 1.0);
    let c = max_diff(&got, &control) * 255.0;
    run.over("control_420_max_lsb", c, 10.0);
}

/// Texel `t` of 4:4:4 `planes` as R'G'B' in [0, 1], decoded exactly with
/// `matrix` and `range` (H.273's nominal ranges, in f64), independently
/// of swscale and of `render::color`.
fn exact_rgb(planes: &YuvPlanes, t: usize, matrix: Matrix, range: Range) -> [f64; 3] {
    let (codes, bits) = match planes {
        YuvPlanes::U8(p) => ([p[0][t], p[1][t], p[2][t]].map(f64::from), 8),
        YuvPlanes::U16(p) => ([p[0][t], p[1][t], p[2][t]].map(f64::from), 10),
    };
    let [y, u, v] = codes;
    let (yy, pb, pr) = if range == Range::Full {
        let max = f64::from((1u32 << bits) - 1);
        let mid = f64::from(1u32 << (bits - 1));
        (y / max, (u - mid) / max, (v - mid) / max)
    } else {
        let s = f64::from(1u32 << (bits - 8));
        (
            (y / s - 16.0) / 219.0,
            (u / s - 128.0) / 224.0,
            (v / s - 128.0) / 224.0,
        )
    };
    let (kr, kb) = if matrix == Matrix::Bt601 {
        (0.299, 0.114)
    } else {
        (0.2126, 0.0722)
    };
    let r = yy + 2.0 * (1.0 - kr) * pr;
    let b = yy + 2.0 * (1.0 - kb) * pb;
    let g = (yy - kr * r - kb * b) / (1.0 - kr - kb);
    [r, g, b]
}

/// The swscale cross-check (section 6): 4:4:4 pictures of flat blocks,
/// BT.601 and BT.709, limited and full, 8 and 10 bits, rendered by
/// `frame` and converted by swscale (to RGB48, rounded to 8 bits) with
/// the matrix and range passed explicitly: within 1 LSB wherever a
/// pixel's taps lie inside one block. 4:4:4 keeps chroma upsampling out
/// of it, and a 360° fisheye magnifies the source at every latitude
/// checked, so the view samples level 0. The render is also held to the
/// exact conversion, within [`EXACT_MAX_LSB`]. Controls: swscale told
/// the other matrix, or the other range, is more than 1 LSB off.
fn swscale_cross_check(run: &mut Run) {
    run.begin("swscale_cross_check");
    let dir = run.scratch.join("swscale");
    std::fs::create_dir_all(&dir).unwrap();
    let size = (256u32, 128u32);
    let (w, h) = (size.0 as usize, size.1 as usize);
    const BLOCK: usize = 16;
    const MARGIN: f64 = 3.0;
    // In-gamut colours, one per block, from a fixed sequence.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        0.05 + 0.9 * (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let colours: Vec<[f64; 3]> = (0..(w / BLOCK) * (h / BLOCK))
        .map(|_| [next(), next(), next()])
        .collect();
    let picture = RgbImage {
        width: size.0,
        height: size.1,
        pixels: (0..w * h)
            .map(|i| colours[(i / w / BLOCK) * (w / BLOCK) + (i % w) / BLOCK])
            .collect(),
    };
    let out = (512u32, 512u32);
    let args = [
        "--equirect",
        "--projection",
        "fisheye",
        "--fov",
        "360",
        "--size",
        "512x512",
    ];
    let view = ViewParams::new(
        Ypr::from_degrees(0.0, 0.0, 0.0).to_quat(),
        Lens::new(Projection::Fisheye, 360f64.to_radians()),
    );
    // Each output pixel's texel, where it is checked.
    let mut checked: Vec<(usize, usize)> = Vec::new();
    for j in 0..out.1 {
        for i in 0..out.0 {
            let c = pixel::centre(i, j);
            // Inside the disc, clear of its rim at θ = π.
            if pixel::to_lens(out, c.x, c.y).length() > 0.98 {
                continue;
            }
            let d = reference::sphere_dir(&view, out, c.x, c.y);
            if LonLat::from_dir(d).lat.abs() > 45f64.to_radians() {
                continue;
            }
            let uv = reference::sphere_uv(d);
            let (tx, ty) = (uv.x * w as f64, uv.y * h as f64);
            let inside = |t: f64, n: usize| {
                let r = t.rem_euclid(BLOCK as f64);
                r >= MARGIN && r <= BLOCK as f64 - MARGIN && t < n as f64
            };
            if inside(tx, w) && inside(ty, h) {
                checked.push(((j * out.0 + i) as usize, (ty as usize) * w + tx as usize));
            }
        }
    }
    run.check(
        checked.len() > 20_000,
        format!("only {} pixels checked", checked.len()),
    );
    run.measure("pixels_checked", checked.len() as f64);
    let mut hashes = Vec::new();
    let (mut worst, mut exact_worst): (f64, f64) = (0.0, 0.0);
    for bits in [8u32, 10] {
        for (matrix, mcode) in [(Matrix::Bt601, 6), (Matrix::Bt709, 1)] {
            for (range, rcode) in [(Range::Limited, 1), (Range::Full, 2)] {
                let name = format!("{bits}bit-{matrix:?}-{range:?}").to_lowercase();
                let tags = ColorTags {
                    range: rcode,
                    matrix: mcode,
                    transfer: 1,
                    primaries: 1,
                    chroma_location: 0,
                };
                let format = YuvFormat::tagged(&tags, Chroma::Yuv444);
                let planes = yuv::to_yuv(&picture, bits, &format);
                let layout = if bits == 8 {
                    PixelLayout::Yuv444p
                } else {
                    PixelLayout::Yuv444p10
                };
                let clip = lossless_clip(
                    &dir.join(format!("{name}.mp4")),
                    layout,
                    size,
                    tags,
                    &planes,
                );
                hashes.push(json!(source_hash(&clip)));
                let Some((r, got)) = run.render(&clip, &name, &args) else {
                    continue;
                };
                let want_matrix = if matrix == Matrix::Bt601 {
                    "bt601"
                } else {
                    "bt709"
                };
                let want_range = if range == Range::Full {
                    "full"
                } else {
                    "limited"
                };
                run.check(
                    r["color"]["matrix"] == want_matrix
                        && r["color"]["range"] == want_range
                        && r["color"]["assumed"]["matrix"] == false,
                    format!("{name}: decoded as {}", r["color"]),
                );
                let Pixels::Rgb8(px) = &got.pixels else {
                    run.fail(format!("{name}: not an 8-bit render"));
                    continue;
                };
                let sws = |m: Matrix, rg: Range| -> Vec<u16> {
                    spherecut_media::encode::swscale_rgb48(&planes, layout, size, m, rg).unwrap()
                };
                // In 8-bit steps, swscale's 16 bits rounded to 8.
                let off = |rgb48: &[u16]| -> f64 {
                    checked
                        .iter()
                        .flat_map(|&(o, t)| {
                            (0..3).map(move |c| {
                                let s = (f64::from(rgb48[3 * t + c]) / 257.0).round();
                                (f64::from(px[3 * o + c]) - s).abs()
                            })
                        })
                        .fold(0.0, f64::max)
                };
                let d = off(&sws(matrix, range));
                worst = worst.max(d);
                run.at_most(&format!("{name}_max_lsb"), d, SWSCALE_MAX_LSB);
                // And the exact conversion, which neither swscale nor the
                // shader computes: the GPU's rounding, and f16's error.
                let e = checked
                    .iter()
                    .flat_map(|&(o, t)| {
                        let x = exact_rgb(&planes, t, matrix, range);
                        (0..3).map(move |c| (f64::from(px[3 * o + c]) - 255.0 * x[c]).abs())
                    })
                    .fold(0.0, f64::max);
                exact_worst = exact_worst.max(e);
                run.at_most(&format!("{name}_exact_max_lsb"), e, EXACT_MAX_LSB);
                let other_matrix = if matrix == Matrix::Bt601 {
                    Matrix::Bt709
                } else {
                    Matrix::Bt601
                };
                let other_range = if range == Range::Full {
                    Range::Limited
                } else {
                    Range::Full
                };
                let c = off(&sws(other_matrix, range));
                run.over(
                    &format!("{name}_matrix_control_lsb"),
                    c,
                    SWSCALE_CONTROL_OVER_LSB,
                );
                let c = off(&sws(matrix, other_range));
                run.over(
                    &format!("{name}_range_control_lsb"),
                    c,
                    SWSCALE_CONTROL_OVER_LSB,
                );
            }
        }
    }
    run.measure("worst_max_lsb", worst);
    run.measure("worst_exact_max_lsb", exact_worst);
    run.end_check(
        "4:4:4 flat blocks at BT.601 and BT.709, limited and full, 8 and 10 bits, through \
         frame and through swscale with the matrix and range given: within 1 LSB inside the \
         blocks, at |latitude| ≤ 45°",
        json!("256x128 4:4:4, 16-texel blocks of 128 in-gamut colours, x265 lossless, 8 variants"),
        Value::Array(hashes),
        json!(args.join(" ")),
    );
}
