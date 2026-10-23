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
use spherecut_media::{HwDevice, MediaError, ff};
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, Codec, Spec};
use spherecut_testkit::{bench, tier2};

/// Generated clips are cached in Cargo's per-target temp dir.
fn clip(name: &str, codec: Codec, bits: u32, width: u32, frames: u32) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("tier2-media");
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
        media::write(&spec, &dir).expect("generating clip");
    }
    path
}

/// Y, U, V planes as u16 samples at their native bit depth, regardless of
/// whether the frame is planar (software) or semi-planar (NV12/P010).
fn planes(f: &ff::frame::Video) -> [Vec<u16>; 3] {
    use ff::format::Pixel as P;
    let (w, h) = (f.width() as usize, f.height() as usize);
    let (cw, ch) = (w / 2, h / 2);
    let row = |plane: usize, y: usize| -> &[u8] { &f.data(plane)[y * f.stride(plane)..] };
    let rd16 = |b: &[u8], i: usize| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
    match f.format() {
        P::YUV420P => {
            let get = |p, pw, ph| -> Vec<u16> {
                (0..ph)
                    .flat_map(|y| {
                        row(p, y)[..pw]
                            .iter()
                            .map(|&v| v as u16)
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            [get(0, w, h), get(1, cw, ch), get(2, cw, ch)]
        }
        P::YUV420P10LE => {
            let get = |p, pw, ph| -> Vec<u16> {
                (0..ph)
                    .flat_map(|y| (0..pw).map(move |x| rd16(row(p, y), x)).collect::<Vec<_>>())
                    .collect()
            };
            [get(0, w, h), get(1, cw, ch), get(2, cw, ch)]
        }
        P::NV12 => {
            let y: Vec<u16> = (0..h)
                .flat_map(|r| row(0, r)[..w].iter().map(|&v| v as u16).collect::<Vec<_>>())
                .collect();
            let uv = |o: usize| -> Vec<u16> {
                (0..ch)
                    .flat_map(|r| {
                        (0..cw)
                            .map(move |x| row(1, r)[2 * x + o] as u16)
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            [y, uv(0), uv(1)]
        }
        P::P010LE => {
            // P010 keeps the 10 bits in the high end of each u16.
            let y: Vec<u16> = (0..h)
                .flat_map(|r| {
                    (0..w)
                        .map(move |x| rd16(row(0, r), x) >> 6)
                        .collect::<Vec<_>>()
                })
                .collect();
            let uv = |o: usize| -> Vec<u16> {
                (0..ch)
                    .flat_map(|r| {
                        (0..cw)
                            .map(move |x| rd16(row(1, r), 2 * x + o) >> 6)
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            [y, uv(0), uv(1)]
        }
        other => panic!("unexpected pixel format {other:?}"),
    }
}

/// A decoded frame's pts and planes.
type Frame = (Option<i64>, [Vec<u16>; 3]);

fn decode_all(path: &Path, hw: Option<Arc<HwDevice>>) -> Result<Vec<Frame>, MediaError> {
    let mut d = VideoDecoder::open(path, None, hw.clone())?;
    let mut out = Vec::new();
    while let Some(f) = d.next_frame()? {
        assert_eq!(f.hw_decoded, hw.is_some());
        out.push((f.pts, planes(&f.frame)));
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

fn check_matches_software(kind: &str, path: &Path) {
    let dev = device(kind);
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
    check_matches_software("vaapi", &clip("t2-hevc8", Codec::Hevc, 8, 1024, 12));
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with HEVC Main10"]
fn vaapi_hevc_main10_matches_software() {
    check_matches_software("vaapi", &clip("t2-hevc10", Codec::Hevc, 10, 1024, 12));
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with H.264"]
fn vaapi_h264_matches_software() {
    check_matches_software("vaapi", &clip("t2-h264", Codec::H264, 8, 1024, 12));
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
        Ok(_) => check_matches_software("vulkan", &path),
        // Vulkan Video is optional; Fedora's Mesa ships without H.265 video.
        Err(e) => tier2::skip(format!("Vulkan Video HEVC decode unavailable: {e}")),
    }
}

#[test]
#[ignore = "tier2: benchmark, needs a VA-API GPU"]
fn bench_decode_3840x1920_hevc() {
    let node = tier2::require_render_node();
    let path = clip("t2-hevc8-4k", Codec::Hevc, 8, 3840, 60);
    let run = |hw: Option<Arc<HwDevice>>| -> Result<f64, MediaError> {
        let mut d = VideoDecoder::open(&path, None, hw)?;
        let t = Instant::now();
        let mut n = 0;
        while d.next_frame()?.is_some() {
            n += 1;
        }
        Ok(n as f64 / t.elapsed().as_secs_f64())
    };
    let sw = run(None).expect("software decode");
    bench::record(
        "decode 3840x1920 HEVC 8-bit, software (incl. download)",
        sw,
        "fps",
    );
    let dev =
        open_hw_device("vaapi", Some(&node.to_string_lossy())).unwrap_or_else(|e| panic!("{e}"));
    let hw = run(Some(dev)).unwrap_or_else(|e| panic!("{e}"));
    bench::record(
        "decode 3840x1920 HEVC 8-bit, VA-API (incl. download)",
        hw,
        "fps",
    );
}
