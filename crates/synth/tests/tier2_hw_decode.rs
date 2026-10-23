//! Tier 2: hardware decode on real GPUs.
//!
//! Each test decodes a generated clip on the device and requires the result
//! to be bit-identical to FFmpeg's software decoder (H.264 and HEVC decoding
//! is exactly specified, so any difference is a driver or plumbing bug).
//! The decoder is not allowed to fall back to software: a frame that wasn't
//! decoded on the device fails the test.
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use spherecut_core::FrameRate;
use spherecut_media::decode::{VideoDecoder, open_hw_device};
use spherecut_media::encode::EncodeError;
use spherecut_media::{HwDevice, MediaError};
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

fn device(kind: &str) -> Arc<HwDevice> {
    let node = tier2::require_render_node();
    let dev = if kind == "vaapi" {
        Some(node.to_string_lossy().into_owned())
    } else {
        None
    };
    match open_hw_device(kind, dev.as_deref()) {
        Ok(d) => d,
        Err(e) => panic!("{e} (run `spherecut doctor` for the fix)"),
    }
}

/// Opens the device before generating the clip, so a machine without one
/// skips straight away.
fn check_matches_software(kind: &str, clip: impl FnOnce() -> PathBuf) {
    let dev = device(kind);
    let path = &clip();
    let sw = decode_all(path, None).expect("software decode");
    let hw = match decode_all(path, Some(dev)) {
        Ok(f) => f,
        Err(e) => panic!("{kind} decode of {} failed: {e}", path.display()),
    };
    assert_eq!(hw.len(), sw.len(), "frame count");
    for (i, (a, b)) in sw.iter().zip(&hw).enumerate() {
        assert_eq!(a.0, b.0, "pts of frame {i}");
        for p in 0..3 {
            assert!(
                a.1[p] == b.1[p],
                "frame {i} plane {p} differs between software and {kind}"
            );
        }
    }
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with HEVC Main"]
fn vaapi_hevc_main_matches_software() {
    check_matches_software("vaapi", || clip("t2-hevc8", Codec::Hevc, 8, 1024, 12));
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with HEVC Main10"]
fn vaapi_hevc_main10_matches_software() {
    check_matches_software("vaapi", || clip("t2-hevc10", Codec::Hevc, 10, 1024, 12));
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with H.264"]
fn vaapi_h264_matches_software() {
    check_matches_software("vaapi", || clip("t2-h264", Codec::H264, 8, 1024, 12));
}

#[test]
#[ignore = "tier2: needs Vulkan Video HEVC decode (optional path)"]
fn vulkan_hevc_main_matches_software() {
    tier2::require_render_node();
    let dev = match open_hw_device("vulkan", None) {
        Ok(d) => d,
        Err(e) => tier2::skip(format!("no Vulkan device for FFmpeg: {e}")),
    };
    let path = clip("t2-hevc8", Codec::Hevc, 8, 1024, 12);
    match VideoDecoder::open(&path, None, Some(dev.clone())).and_then(|mut d| d.next_frame()) {
        Ok(_) => check_matches_software("vulkan", || path),
        // Vulkan Video is optional, and not every driver has it. Any other
        // failure is a bug.
        Err(e @ MediaError::HwUnsupported { .. }) => {
            tier2::skip(format!("Vulkan Video HEVC decode unavailable: {e}"))
        }
        Err(e) => panic!("Vulkan decode of {} failed: {e}", path.display()),
    }
}

#[test]
#[ignore = "tier2: benchmark, needs a VA-API GPU"]
fn bench_decode_hevc_4k_and_5k7() {
    let node = tier2::require_render_node();
    let dev =
        open_hw_device("vaapi", Some(&node.to_string_lossy())).unwrap_or_else(|e| panic!("{e}"));
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
    }
}
