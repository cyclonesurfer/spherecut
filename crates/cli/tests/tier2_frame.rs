//! Tier 2: `spherecut frame` decoding on the hardware renders what it
//! renders decoding in software (M1 plan section 12.2, DECISIONS D30 and
//! D38): the same RGB within 1 LSB, on the machine's GPU.
//!
//! H.264 and HEVC decoding is exactly specified, so the planes are the
//! same bit for bit; what the check adds over `synth`'s decode tests is
//! the rest of the pipeline: the download (NV12 or P010 where software
//! gives yuv420p or yuv420p10le), the upload of those layouts, and the
//! render. `--decoder vaapi` and `--decoder vulkan` never fall back, so a
//! frame that wasn't decoded on the device fails the run.
//!
//! VA-API is the decoder Spherecut relies on, so its test fails wherever
//! it is set up and doesn't work. Vulkan Video is optional (D38): its test
//! skips with frame's reason when the driver can't decode the stream, or
//! FFmpeg's Vulkan device is a CPU.
//!
//! `the_check_runs_on_lavapipe_with_software_standing_in` is the Tier 1
//! run of the same check, with software in the hardware's place, and a
//! different frame, which must fail it.
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as
//! ignored.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

mod document;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use spherecut_core::FrameRate;
use spherecut_render::{Image, Pixels};
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Codec, Spec};
use spherecut_testkit::tier2;

use document::document;

/// The bound: one 8-bit step.
const MAX_LSB: f64 = 1.0;

/// A directory of its own, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("tier2-frame-{}-{name}", std::process::id()));
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

/// The clips: 8-bit H.264, and 8- and 10-bit HEVC, coded as a camera
/// codes (not losslessly, which hardware decoders needn't take), 30
/// frames with a keyframe at the start only.
fn clips(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    [
        ("h264", Codec::H264, 8),
        ("hevc-main", Codec::Hevc, 8),
        ("hevc-main10", Codec::Hevc, 10),
    ]
    .into_iter()
    .map(|(name, codec, bits)| {
        let spec = Spec {
            name: name.into(),
            pattern: Pattern::Grid,
            width: 1920,
            height: 960,
            rate: FrameRate::FPS_30,
            frames: 30,
            codec,
            bits,
            spherical: true,
            ..Spec::default()
        };
        (name, media::write(&spec, dir).unwrap())
    })
    .collect()
}

/// What a `frame --json` run gave: its status, its document, its stderr.
struct Ran {
    status: Option<i32>,
    doc: Value,
    stderr: String,
}

/// `spherecut frame CLIP -o OUT --json` with `args`, nothing else from the
/// environment steering it.
fn frame(clip: &Path, out: &Path, args: &[&str]) -> Ran {
    let o = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .arg("frame")
        .arg(clip)
        .arg("-o")
        .arg(out)
        .args(["--json", "--overwrite"])
        .args(args)
        .env_remove("SPHERECUT_LOG")
        .env_remove("SPHERECUT_ADAPTER")
        .output()
        .unwrap();
    Ran {
        status: o.status.code(),
        doc: document(&o.stdout),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn unit(img: &Image) -> Vec<f64> {
    match &img.pixels {
        Pixels::Rgb8(p) => p.iter().map(|&v| f64::from(v) / 255.0).collect(),
        Pixels::Rgb16(p) => p.iter().map(|&v| f64::from(v) / 65535.0).collect(),
        Pixels::LinearF32(p) => p.iter().map(|&v| f64::from(v)).collect(),
    }
}

/// The largest difference between two PNGs, in 8-bit steps.
fn max_lsb(a: &Path, b: &Path) -> f64 {
    let load = |p: &Path| Image::from_png(&std::fs::read(p).unwrap()).unwrap();
    let (a, b) = (load(a), load(b));
    assert_eq!((a.width, a.height), (b.width, b.height));
    unit(&a)
        .iter()
        .zip(unit(&b))
        .map(|(x, y)| (x - y).abs() * 255.0)
        .fold(0.0, f64::max)
}

/// The views each clip is rendered through: a magnified rectilinear view
/// and a 360° fisheye, which minifies, both at a frame past the first,
/// so that the decoder's references matter.
const VIEWS: &[&[&str]] = &[
    &[
        "--fov", "60", "--yaw", "30", "--pitch", "10", "--size", "1280x720",
    ],
    &[
        "--projection",
        "fisheye",
        "--fov",
        "360",
        "--size",
        "720x720",
    ],
];

/// Why the decoder under test didn't decode the frame: frame's code and
/// message.
struct Refused {
    code: String,
    message: String,
}

/// Renders `clip` at `at` through every view, decoding with `decoder`
/// and with `reference_args` (software, at frame `reference_at`), on the
/// same adapter, and returns the largest difference over every view. A
/// run of `decoder` that fails with exit 3 is refused; any other failure
/// panics.
fn compare(
    dir: &Path,
    clip: &Path,
    decoder: &[&str],
    at: &str,
    reference_at: &str,
    common: &[&str],
) -> Result<f64, Refused> {
    let mut worst: f64 = 0.0;
    for (i, view) in VIEWS.iter().enumerate() {
        let got = dir.join(format!("got-{i}.png"));
        let want = dir.join(format!("want-{i}.png"));
        let run = |out: &Path, dec: &[&str], frame_no: &str| {
            let mut args: Vec<&str> = view.to_vec();
            args.extend(common);
            args.extend(dec);
            args.extend(["--frame", frame_no]);
            frame(clip, out, &args)
        };
        let r = run(&got, decoder, at);
        if r.status == Some(3) {
            return Err(Refused {
                code: r.doc["error"]["code"].as_str().unwrap_or("").into(),
                message: r.doc["error"]["message"].as_str().unwrap_or("").into(),
            });
        }
        assert_eq!(r.status, Some(0), "{decoder:?}: {} {}", r.doc, r.stderr);
        let path = decoder.last().copied().unwrap_or("software");
        assert_eq!(r.doc["result"]["decode"]["path"], path, "{}", r.doc);
        assert!(
            r.doc["result"]["decode"].get("fallback").is_none(),
            "{}",
            r.doc
        );
        let s = run(&want, &["--decoder", "software"], reference_at);
        assert_eq!(s.status, Some(0), "software: {} {}", s.doc, s.stderr);
        assert_eq!(s.doc["result"]["decode"]["path"], "software");
        assert_eq!(
            r.doc["result"]["gpu"], s.doc["result"]["gpu"],
            "rendered on two adapters"
        );
        worst = worst.max(max_lsb(&got, &want));
    }
    Ok(worst)
}

/// The adapter and render node a Tier 2 run uses: the machine's GPU
/// (`auto`, never lavapipe), and VA-API's node when there is one.
fn hardware_args(node: &Option<String>) -> Vec<&str> {
    let mut args = vec!["--adapter", "auto"];
    if let Some(n) = node {
        args.extend(["--render-node", n.as_str()]);
    }
    args
}

/// The GPU `auto` picks must not be a CPU: these tests are about a
/// GPU's decode and render. Asked before the clips are made, so that a
/// machine without one skips at once.
fn require_gpu(dir: &Path, common: &[&str]) {
    let spec = Spec {
        name: "tiny".into(),
        width: 64,
        height: 32,
        frames: 1,
        spherical: true,
        ..Spec::default()
    };
    let clip = media::write(&spec, dir).unwrap();
    let mut args = common.to_vec();
    args.extend(["--decoder", "software", "--size", "64x32"]);
    let r = frame(&clip, &dir.join("tiny.png"), &args);
    assert_eq!(r.status, Some(0), "{} {}", r.doc, r.stderr);
    if r.doc["result"]["gpu"]["device_type"] == "cpu" {
        tier2::skip(format!(
            "the only adapter is a CPU: {}",
            r.doc["result"]["gpu"]["name"]
        ));
    }
}

#[test]
#[ignore = "tier2: needs a VA-API GPU"]
fn vaapi_render_matches_software() {
    let node = tier2::require_render_node();
    let node = Some(node.to_string_lossy().into_owned());
    let dir = TempDir::new("vaapi");
    let common = hardware_args(&node);
    require_gpu(&dir, &common);
    let clips = clips(&dir);
    for (name, clip) in &clips {
        match compare(&dir, clip, &["--decoder", "vaapi"], "17", "17", &common) {
            Ok(d) => {
                eprintln!("{name}: VA-API against software, at most {d} LSB");
                assert!(d <= MAX_LSB, "{name}: {d} LSB apart");
            }
            Err(r) => panic!(
                "{name}: VA-API refused it ({}): {} (run `spherecut doctor` for the fix)",
                r.code, r.message
            ),
        }
    }
}

#[test]
#[ignore = "tier2: needs Vulkan Video decode (optional path)"]
fn vulkan_render_matches_software() {
    tier2::require_hw_run();
    let node = tier2::render_node().map(|n| n.to_string_lossy().into_owned());
    let dir = TempDir::new("vulkan");
    let common = hardware_args(&node);
    require_gpu(&dir, &common);
    let clips = clips(&dir);
    let mut refused = Vec::new();
    let mut compared = 0;
    for (name, clip) in &clips {
        match compare(&dir, clip, &["--decoder", "vulkan"], "17", "17", &common) {
            Ok(d) => {
                eprintln!("{name}: Vulkan Video against software, at most {d} LSB");
                assert!(d <= MAX_LSB, "{name}: {d} LSB apart");
                compared += 1;
            }
            // The driver can't decode it, or there is no Vulkan GPU for
            // FFmpeg: the optional path isn't there.
            Err(r) if r.code == "hw_unavailable" || r.code == "hw_unsupported" => {
                refused.push(format!("{name}: {} ({})", r.message, r.code));
            }
            Err(r) => panic!("{name}: {} ({})", r.message, r.code),
        }
    }
    if compared == 0 {
        tier2::skip(format!(
            "Vulkan Video decoded none of the clips: {}",
            refused.join("; ")
        ));
    }
    for r in &refused {
        eprintln!("not compared: {r}");
    }
}

/// The same check in Tier 1, on lavapipe, with software standing in for
/// the hardware: the same frame through the same pipeline is 0 LSB
/// apart, and the next frame, which a decoder that lost its place would
/// give, is far outside the bound.
#[test]
fn the_check_runs_on_lavapipe_with_software_standing_in() {
    let dir = TempDir::new("stand-in");
    let spec = Spec {
        name: "stand-in".into(),
        pattern: Pattern::Grid,
        width: 512,
        height: 256,
        rate: FrameRate::FPS_30,
        frames: 30,
        codec: Codec::Hevc,
        bits: 10,
        spherical: true,
        ..Spec::default()
    };
    let clip = media::write(&spec, &dir).unwrap();
    let common = ["--adapter", "cpu"];
    let same = compare(&dir, &clip, &["--decoder", "software"], "17", "17", &common)
        .unwrap_or_else(|r| panic!("{} ({})", r.message, r.code));
    assert!(same <= MAX_LSB, "{same}");
    assert_eq!(same, 0.0);
    let off = compare(&dir, &clip, &["--decoder", "software"], "18", "17", &common)
        .unwrap_or_else(|r| panic!("{} ({})", r.message, r.code));
    assert!(
        off > 10.0 * MAX_LSB,
        "the next frame is only {off} LSB away"
    );
}
