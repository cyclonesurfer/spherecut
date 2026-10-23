//! `VideoDecoder` on a Vulkan device that decodes nothing: Mesa's lavapipe,
//! which has no Vulkan Video. Every stream must be turned down with
//! `MediaError::HwUnsupported`, never a generic decode error, so a caller
//! knows that software may still decode it. Tier 1 needs lavapipe, as the
//! doctor tests do.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use spherecut_core::FrameRate;
use spherecut_media::decode::{VideoDecoder, open_hw_device};
use spherecut_media::encode::{Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
use spherecut_media::{HwDevice, HwRefusal, MediaError};

fn tmp() -> PathBuf {
    let d =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("hw-refusal-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// FFmpeg's Vulkan device on lavapipe: FFmpeg picks the device whose name
/// contains the string, so a GPU in the machine doesn't get in the way.
fn lavapipe() -> Arc<HwDevice> {
    open_hw_device("vulkan", Some("llvmpipe")).unwrap_or_else(|e| {
        panic!(
            "{e}; Tier 1 needs FFmpeg's Vulkan device on Mesa's lavapipe \
             (mesa-vulkan-drivers on Fedora and Ubuntu, vulkan-swrast on Arch)"
        )
    })
}

/// Ten 256x128 frames of a moving ramp, encoded with FFmpeg's `encoder`.
fn clip(name: &str, encoder: &str, options: &[(&str, &str)]) -> PathBuf {
    let (w, h) = (256, 128);
    let settings = VideoSettings {
        encoder: encoder.into(),
        width: w,
        height: h,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: options
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        bt709_limited: true,
    };
    let (w, h) = (w as usize, h as usize);
    let mut out = Mp4Writer::create(&tmp().join(name), &settings, None).unwrap();
    for i in 0..10 {
        let luma = (0..w * h).map(|p| ((p % w + 8 * i) % 256) as u8).collect();
        let chroma = vec![128; w * h / 4];
        out.push_video(&YuvPlanes::U8([luma, chroma.clone(), chroma]))
            .unwrap();
    }
    out.finish().unwrap()
}

fn refusal(r: Result<impl std::fmt::Debug, MediaError>) -> HwRefusal {
    match r {
        Err(MediaError::HwUnsupported { kind, refusal, .. }) => {
            assert_eq!(kind, "Vulkan");
            refusal
        }
        r => panic!("expected HwUnsupported, got {r:?}"),
    }
}

#[test]
fn a_refused_stream_stays_refused() {
    let path = clip("hevc.mp4", "libx265", &[("x265-params", "log-level=error")]);
    let mut d = VideoDecoder::open(&path, None, Some(lavapipe())).unwrap();
    let first = refusal(d.next_frame());
    // FFmpeg's HEVC decoder would go on in software after turning the
    // device down; every later call gives the same answer instead.
    for _ in 0..3 {
        assert_eq!(refusal(d.next_frame()), first);
    }
}

#[test]
fn av1_is_refused_rather_than_decoded_by_a_software_only_decoder() {
    // FFmpeg lists libdav1d before its own AV1 decoder, and libdav1d never
    // decodes on a device. The clip is doctor's one AV1 frame.
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/av1-main.obu");
    match VideoDecoder::open(&path, None, Some(lavapipe())) {
        // This FFmpeg has no AV1 decoder for Vulkan Video at all.
        Err(e) => assert_eq!(refusal(Err::<(), _>(e)), HwRefusal::NotOffered),
        // FFmpeg's own decoder offered the device's format, and lavapipe
        // turned it down. libdav1d would have decoded in software without
        // asking for a format, which reads as NotOffered.
        Ok(mut d) => assert_eq!(refusal(d.next_frame()), HwRefusal::Withdrawn),
    }
    // Software still decodes it.
    let mut d = VideoDecoder::open(&path, None, None).unwrap();
    assert!(d.next_frame().unwrap().is_some());
}

#[test]
fn a_codec_no_decoder_can_decode_on_the_device_is_refused_at_open() {
    // No FFmpeg has Vulkan Video for MPEG-4 Part 2.
    let path = clip("mpeg4.mp4", "mpeg4", &[]);
    assert_eq!(
        refusal(VideoDecoder::open(&path, None, Some(lavapipe()))),
        HwRefusal::NotOffered
    );
    // Software still decodes it.
    let mut d = VideoDecoder::open(&path, None, None).unwrap();
    assert!(d.next_frame().unwrap().is_some());
}
