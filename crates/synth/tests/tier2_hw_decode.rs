//! Tier 2: hardware decode on real GPUs.
//!
//! Each test decodes a generated clip on the device and requires the result
//! to be bit-identical to FFmpeg's software decoder (H.264 and HEVC decoding
//! is exactly specified, so any difference is a driver or plumbing bug).
//! The decoder is not allowed to fall back to software: a frame that wasn't
//! decoded on the device fails the test.
//!
//! VA-API is the hardware decoder Spherecut relies on. Vulkan Video is
//! optional, and not every driver has it, so its tests skip with FFmpeg's
//! reason when the driver can't decode the format, and fail on any other
//! error. They run on the GPU behind VA-API's render node, or the first
//! GPU, never on llvmpipe (`open_vulkan_device`, DECISIONS D27).
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use spherecut_core::FrameRate;
use spherecut_media::decode::{VideoDecoder, open_hw_device, open_vulkan_device};
use spherecut_media::encode::EncodeError;
use spherecut_media::probe::{self, StreamKind};
use spherecut_media::{HwDevice, HwError, MediaError, capture_hw_decode_log};
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, Codec, Spec, SynthError};
use spherecut_testkit::{bench, tier2};

/// Generated clips are cached in Cargo's per-target temp dir.
///
/// Caching by existence is safe because `media::write` renames a clip into
/// place only once it is complete: an interrupted run leaves no file at
/// `path`, at most a hidden temporary file next to it. The directory name
/// has a version because writers before that fix could leave a partial
/// clip.
fn clip(name: &str, codec: Codec, bits: u32, width: u32, frames: u32) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tier2-media-v2");
    let spec = Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width,
        height: width / 2,
        rate: FrameRate::FPS_30,
        frames,
        codec,
        bits,
        audio: Audio::None,
        spherical: true,
    };
    let path = dir.join(spec.file_name());
    if !path.exists() {
        match media::write(&spec, &dir) {
            Ok(_) => {}
            // A prerequisite, not a hardware result; hw-test lists the fix.
            Err(SynthError::Encode(EncodeError::NoEncoder(e))) => {
                tier2::skip(format!("FFmpeg has no {e} encoder to generate test clips"))
            }
            Err(e) => panic!("generating {}: {e}", path.display()),
        }
    }
    path
}

/// A decoded frame's pts and planes.
type Frame = (Option<i64>, [Vec<u16>; 3]);

fn decode_all(path: &Path, hw: Option<Arc<HwDevice>>) -> Result<Vec<Frame>, MediaError> {
    let mut d = VideoDecoder::open(path, None, hw.clone())?;
    let mut out = Vec::new();
    while let Some(f) = d.next_frame()? {
        assert_eq!(f.hw_decoded, hw.is_some());
        let planes = f
            .planes_u16()
            .unwrap_or_else(|| panic!("unexpected pixel format: {f:?}"));
        out.push((f.pts, planes));
    }
    Ok(out)
}

fn vaapi() -> Arc<HwDevice> {
    let node = tier2::require_render_node();
    open_hw_device("vaapi", Some(&node.to_string_lossy()))
        .unwrap_or_else(|e| panic!("{e} (run `spherecut doctor` for the fix)"))
}

/// FFmpeg's Vulkan device on a GPU.
struct Vulkan {
    dev: Arc<HwDevice>,
    /// Which device it is, and why that one.
    about: String,
}

/// Why there is no Vulkan GPU to decode on.
enum NoVulkan {
    /// There is none: a test skips.
    Missing(String),
    /// Choosing it went wrong: a test fails.
    Broken(String),
}

impl NoVulkan {
    fn reason(&self) -> &str {
        match self {
            NoVulkan::Missing(r) | NoVulkan::Broken(r) => r,
        }
    }
}

/// FFmpeg's Vulkan device on the GPU behind VA-API's render node, else the
/// first GPU.
fn vulkan_gpu() -> Result<Vulkan, NoVulkan> {
    match open_vulkan_device(tier2::render_node().as_deref()) {
        Ok(c) if c.used.is_cpu() => Err(NoVulkan::Missing(format!(
            "no GPU for Vulkan Video: FFmpeg's Vulkan device is {c}"
        ))),
        Ok(c) => {
            eprintln!("Vulkan device: {c}");
            Ok(Vulkan {
                about: c.to_string(),
                dev: c.device,
            })
        }
        Err(
            e @ MediaError::Hardware {
                reason: HwError::Vulkan(_),
                ..
            },
        ) => Err(NoVulkan::Broken(e.to_string())),
        Err(e) => Err(NoVulkan::Missing(format!(
            "no Vulkan device for FFmpeg: {e}"
        ))),
    }
}

/// [`vulkan_gpu`] in a test, which skips without one.
fn vulkan() -> Vulkan {
    tier2::require_hw_run();
    match vulkan_gpu() {
        Ok(v) => v,
        Err(NoVulkan::Missing(why)) => tier2::skip(why),
        Err(NoVulkan::Broken(why)) => panic!("{why}"),
    }
}

/// Why `dev` can't decode the first frame of `path`, if it can't: the
/// error, and after it the stream's size and level and what FFmpeg logged
/// meanwhile. Only the log says why for a driver without the codec
/// ("Device does not support decoding hevc!") or the profile ("Unable to
/// initialize video session: hevc profile "Main 10" not supported!"). For
/// a stream bigger or of a higher level than the driver decodes, FFmpeg
/// says nothing but the limits it checked ("Maximum level: 153 (stream
/// 186)"), so the stream's own size goes with them.
fn first_frame(path: &Path, dev: &Arc<HwDevice>) -> Result<(), (MediaError, String)> {
    let (r, log) = capture_hw_decode_log(|| {
        VideoDecoder::open(path, None, Some(dev.clone())).and_then(|mut d| d.next_frame())
    });
    r.map(drop).map_err(|e| {
        let mut about = vec![video_stream(path)];
        if !log.is_empty() {
            about.push(format!("FFmpeg: {}", log.join(" ")));
        }
        (e, format!(" ({})", about.join("; ")))
    })
}

/// The size and level of `path`'s video stream, e.g. "stream 1024x512,
/// level 93", in FFmpeg's numbering of levels as in its own lines.
fn video_stream(path: &Path) -> String {
    let r = match probe::probe(path) {
        Ok(r) => r,
        Err(e) => return format!("stream not probed: {e}"),
    };
    let Some(s) = r.streams.iter().find(|s| s.kind == StreamKind::Video) else {
        return "no video stream".into();
    };
    let mut out = match &s.video {
        Some(v) => format!("stream {}x{}", v.width, v.height),
        None => "stream of unknown size".into(),
    };
    if let Some(level) = s.level {
        out += &format!(", level {level}");
    }
    out
}

/// Decode `path` in software and on `dev`, called `on` in messages, and
/// require the same frames.
fn check_matches_software(on: &str, dev: Arc<HwDevice>, path: &Path) {
    let sw = decode_all(path, None).expect("software decode");
    let hw = match decode_all(path, Some(dev)) {
        Ok(f) => f,
        Err(e) => panic!("decode of {} on {on} failed: {e}", path.display()),
    };
    assert_eq!(hw.len(), sw.len(), "frame count");
    for (i, (a, b)) in sw.iter().zip(&hw).enumerate() {
        assert_eq!(a.0, b.0, "pts of frame {i}");
        for p in 0..3 {
            assert!(
                a.1[p] == b.1[p],
                "frame {i} plane {p} differs between software and {on}"
            );
        }
    }
}

/// Opens the device before generating the clip, so a machine without one
/// skips straight away.
fn check_vaapi_matches_software(clip: impl FnOnce() -> PathBuf) {
    let dev = vaapi();
    check_matches_software("VA-API", dev, &clip());
}

/// The same on Vulkan Video, which skips when the driver can't decode
/// `format`.
fn check_vulkan_matches_software(format: &str, clip: impl FnOnce() -> PathBuf) {
    let vk = vulkan();
    let path = clip();
    match first_frame(&path, &vk.dev) {
        Ok(()) => {}
        Err((e @ MediaError::HwUnsupported { .. }, why)) => tier2::skip(format!(
            "Vulkan Video can't decode {format} on {}: {e}{why}",
            vk.about
        )),
        Err((e, why)) => panic!(
            "Vulkan decode of {} on {} failed: {e}{why}",
            path.display(),
            vk.about
        ),
    }
    check_matches_software(&format!("Vulkan on {}", vk.about), vk.dev, &path);
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with HEVC Main"]
fn vaapi_hevc_main_matches_software() {
    check_vaapi_matches_software(|| clip("t2-hevc8", Codec::Hevc, 8, 1024, 12));
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with HEVC Main10"]
fn vaapi_hevc_main10_matches_software() {
    check_vaapi_matches_software(|| clip("t2-hevc10", Codec::Hevc, 10, 1024, 12));
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with H.264"]
fn vaapi_h264_matches_software() {
    check_vaapi_matches_software(|| clip("t2-h264", Codec::H264, 8, 1024, 12));
}

#[test]
#[ignore = "tier2: needs Vulkan Video HEVC decode (optional path)"]
fn vulkan_hevc_main_matches_software() {
    check_vulkan_matches_software("HEVC Main", || clip("t2-hevc8", Codec::Hevc, 8, 1024, 12));
}

#[test]
#[ignore = "tier2: needs Vulkan Video HEVC Main10 decode (optional path)"]
fn vulkan_hevc_main10_matches_software() {
    check_vulkan_matches_software("HEVC Main 10", || {
        clip("t2-hevc10", Codec::Hevc, 10, 1024, 12)
    });
}

#[test]
#[ignore = "tier2: needs Vulkan Video H.264 decode (optional path)"]
fn vulkan_h264_matches_software() {
    check_vulkan_matches_software("H.264", || clip("t2-h264", Codec::H264, 8, 1024, 12));
}

#[test]
#[ignore = "tier2: benchmark, needs a VA-API GPU"]
fn bench_decode_hevc_4k_and_5k7() {
    let dev = vaapi();
    // Vulkan Video is optional here: without it, its rows are left out
    // and the report says why.
    let vulkan = vulkan_gpu();
    if let Ok(vk) = &vulkan {
        bench::note(format!("Vulkan Video rows ran on {}", vk.about));
    }
    let fps = |path: &Path, hw: Option<Arc<HwDevice>>| -> Result<f64, MediaError> {
        let mut d = VideoDecoder::open(path, None, hw)?;
        let t = Instant::now();
        let mut n = 0;
        while d.next_frame()?.is_some() {
            n += 1;
        }
        Ok(n as f64 / t.elapsed().as_secs_f64())
    };
    // 5760x2880 is what 5.7K cameras record; the target is 30 fps.
    for (w, name) in [(3840, "t2-hevc8-4k"), (5760, "t2-hevc8-5k7")] {
        let path = clip(name, Codec::Hevc, 8, w, 60);
        let size = format!("{w}x{}", w / 2);
        bench::measure(
            &format!("decode {size} HEVC 8-bit, software"),
            "fps",
            || fps(&path, None).expect("software decode"),
        );
        bench::measure(
            &format!("decode {size} HEVC 8-bit, VA-API (incl. copy to RAM)"),
            "fps",
            || fps(&path, Some(dev.clone())).unwrap_or_else(|e| panic!("{e}")),
        );
        let row = format!("decode {size} HEVC 8-bit, Vulkan Video (incl. copy to RAM)");
        let measured = match &vulkan {
            Err(no) => Err(no.reason().to_string()),
            Ok(vk) => first_frame(&path, &vk.dev)
                .map_err(|(e, why)| format!("{e}{why}"))
                .and_then(|()| {
                    bench::try_measure(&row, "fps", || fps(&path, Some(vk.dev.clone())))
                        .map_err(|e| e.to_string())
                }),
        };
        if let Err(why) = measured {
            bench::not_measured(&row, why);
        }
    }
}
