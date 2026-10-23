//! End-to-end checks on generated media: what we write, FFmpeg and our own
//! parsers read back exactly.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use spherecut_core::{FrameRate, Rational};
use spherecut_media::detect::Layout;
use spherecut_media::encode::YuvPlanes;
use spherecut_media::ff;
use spherecut_media::probe::{self, StreamKind};
use spherecut_synth::media::{self, Audio, Codec, Spec};
use spherecut_synth::{Pattern, decode_frame_code, yuv};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("spherecut-synth-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn small(name: &str, codec: Codec, bits: u32, audio: Audio, spherical: bool) -> Spec {
    Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width: 512,
        height: 256,
        rate: FrameRate::FPS_29_97,
        frames: 6,
        codec,
        bits,
        audio,
        spherical,
    }
}

/// Decode every video frame in software and return them as planes.
fn decode_all(path: &Path) -> Vec<(i64, YuvPlanes, u32, u32, u32)> {
    spherecut_media::init();
    let mut ictx = ff::format::input(path).unwrap();
    let st = ictx.streams().best(ff::media::Type::Video).unwrap();
    let idx = st.index();
    let mut dec = ff::codec::context::Context::from_parameters(st.parameters())
        .unwrap()
        .decoder()
        .video()
        .unwrap();
    let mut out = Vec::new();
    let mut frame = ff::frame::Video::empty();
    let mut grab = |dec: &mut ff::decoder::Video, out: &mut Vec<_>| {
        while dec.receive_frame(&mut frame).is_ok() {
            let (w, h) = (frame.width(), frame.height());
            let ten = frame.format() == ff::format::Pixel::YUV420P10LE;
            let planes = (0..3)
                .map(|i| {
                    let (pw, ph) = if i == 0 { (w, h) } else { (w / 2, h / 2) };
                    let stride = frame.stride(i);
                    let data = frame.data(i);
                    (0..ph as usize)
                        .flat_map(|y| {
                            let row = &data[y * stride..];
                            (0..pw as usize).map(move |x| {
                                if ten {
                                    u16::from_le_bytes([row[2 * x], row[2 * x + 1]])
                                } else {
                                    row[x] as u16
                                }
                            })
                        })
                        .collect::<Vec<u16>>()
                })
                .collect::<Vec<_>>();
            let [y, u, v]: [Vec<u16>; 3] = planes.try_into().unwrap();
            let p = if ten {
                YuvPlanes::U16([y, u, v])
            } else {
                YuvPlanes::U8([y, u, v].map(|p| p.into_iter().map(|x| x as u8).collect()))
            };
            out.push((frame.pts().unwrap_or(-1), p, w, h, if ten { 10 } else { 8 }));
        }
    };
    for (s, pkt) in ictx.packets() {
        if s.index() == idx {
            dec.send_packet(&pkt).unwrap();
            grab(&mut dec, &mut out);
        }
    }
    dec.send_eof().unwrap();
    grab(&mut dec, &mut out);
    out
}

#[test]
fn lossless_hevc10_round_trips_frames_timing_and_metadata() {
    let dir = tmp("lossless");
    let spec = small(
        "t-lossless",
        Codec::HevcLossless,
        10,
        Audio::AmbiX {
            azimuth: 45,
            elevation: 30,
        },
        true,
    );
    let path = media::write(&spec, &dir).unwrap();

    let r = probe::probe(&path).unwrap();
    let v = r
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .unwrap();
    assert_eq!(v.codec.as_deref(), Some("hevc"));
    let vi = v.video.as_ref().unwrap();
    assert_eq!((vi.width, vi.height, vi.bit_depth), (512, 256, Some(10)));
    assert_eq!(v.avg_frame_rate, Some(Rational::new(30000, 1001).unwrap()));
    assert_eq!(v.frames, Some(6));
    assert_eq!(vi.color.matrix.as_deref(), Some("bt709"));
    assert_eq!(vi.color.range.as_deref(), Some("tv"));
    assert!(v.spherical.v2.is_some(), "sv3d/st3d missing");
    let a = r
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Audio)
        .unwrap();
    assert_eq!(a.audio.as_ref().unwrap().channels, 4);
    assert!(a.spherical.spatial_audio.as_ref().unwrap().is_ambix());
    assert_eq!(r.camera.as_ref().unwrap().layout, Layout::EquirectTagged);

    // Lossless: every decoded frame matches what we encoded, and carries
    // its own frame number.
    let frames = decode_all(&path);
    assert_eq!(frames.len(), 6);
    for (i, (_, planes, w, h, bits)) in frames.iter().enumerate() {
        assert_eq!(*bits, 10);
        let src = yuv::to_yuv420(
            &spherecut_synth::render_equirect(Pattern::Grid, 512, 256, i as u32, 2),
            10,
        );
        match (planes, &src) {
            (YuvPlanes::U16(a), YuvPlanes::U16(b)) => assert!(a == b, "frame {i} differs"),
            _ => panic!("bit depth mismatch"),
        }
        let img = yuv::from_yuv420(planes, *w, *h, 10);
        assert_eq!(decode_frame_code(&img), Some(i as u32));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lossy_h264_untagged_is_flagged_as_likely_equirect() {
    let dir = tmp("untagged");
    let path = media::write(
        &small("t-untagged", Codec::H264, 8, Audio::Stereo, false),
        &dir,
    )
    .unwrap();
    let r = probe::probe(&path).unwrap();
    assert_eq!(r.camera.as_ref().unwrap().layout, Layout::EquirectLikely);
    assert!(r.streams.iter().all(|s| s.spherical.is_empty()));
    // Frame codes survive lossy coding.
    for (i, (_, planes, w, h, bits)) in decode_all(&path).iter().enumerate() {
        let img = yuv::from_yuv420(planes, *w, *h, *bits);
        assert_eq!(decode_frame_code(&img), Some(i as u32));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn broken_files_give_errors_not_panics() {
    let dir = tmp("bad");
    let good = media::write(&small("t-good", Codec::H264, 8, Audio::Stereo, true), &dir).unwrap();
    let bad = media::write_bad_files(&good, &dir).unwrap();
    assert_eq!(bad.len(), 6);
    for b in &bad {
        let name = b.file_name().unwrap().to_string_lossy().into_owned();
        match probe::probe(b) {
            Err(e) => assert!(!e.to_string().is_empty()),
            // Some damage (late truncation, corruption) leaves a readable
            // file; then the damage must show up as warnings.
            Ok(r) => assert!(
                !r.warnings.is_empty() || name.contains("corrupt") || name.contains("trunc-end"),
                "{name} probed cleanly"
            ),
        }
    }
    // A directory is not a media file.
    assert!(probe::probe(&dir).is_err());
    std::fs::remove_dir_all(&dir).ok();
}
