//! Parity (M2 plan section 11, amendment 2 item 31, D52): an export,
//! decoded again, against the parity reference, `StillRenderer` of the
//! same views of the same source frame in the encoder's own planes. The
//! views take in the moving marker's path and the frame counter, so a
//! frame shifted by one fails, as a view turned by 1° does.
//!
//! On lavapipe with the software encoders: flat through libx264 from an
//! 8-bit source, every frame of 2 s; 360 through libx265 from a 10-bit
//! one, every frame of half a second; and libsvtav1 at the smallest
//! preset, 1:1 at 1080, from each source, every frame of half a second
//! (amendment 2 item 4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{
    Scratch, best, export, frames, grid, held, measure, media_path, project, references, secs,
    worst,
};
use spherecut_core::project::{Aspect, Codec, Orientation, Output, Project, Range};
use spherecut_core::time::Time;
use spherecut_export::{FramePlan, Report};
use spherecut_render::YuvLayout;
use spherecut_sphere::orient::Ypr;
use spherecut_synth::media as synth;

/// The export of `project` holds to the reference, and the two controls
/// fall below the floors: `turned` (the same keys 1° further round) and
/// the frames shifted by one.
fn parity(
    name: &str,
    project: &Project,
    turned: &Project,
    yuv: YuvLayout,
    floors: (f64, f64),
) -> Report {
    let scratch = Scratch::new(name);
    let out = scratch.0.join(format!("{name}.mp4"));
    let report = export(project, &out);
    let got = frames(&out);
    assert_eq!(got.len() as u64, report.frames, "frames decoded");
    let src = frames(&media_path(project));
    // The source is at the sequence's rate from the trim's start: frame
    // n is shown at its own time.
    let clip = project.the_clip();
    let plan = FramePlan::new(clip.source, project.the_sequence().rate).unwrap();
    for n in 0..report.frames {
        let t = plan.source_time(n).unwrap();
        assert_eq!(held(&src, t).time, t, "{name}: frame {n}");
    }
    let decoded = || got.iter().map(|g| g.as_ref());
    let all: Vec<u64> = (0..report.frames).collect();
    let want = references(project, &src, yuv, &all);
    let m = measure(decoded().zip(want.iter().map(Vec::as_slice)));
    let (psnr, ssim) = worst(&m);
    eprintln!(
        "{name}: worst luma PSNR {psnr:.2} dB, SSIM {ssim:.5} over {} frames",
        m.len()
    );
    // The turned view on about ten frames: one is enough to fail.
    let step = (report.frames as usize / 10).max(1);
    let sampled: Vec<u64> = all.iter().copied().step_by(step).collect();
    let turned_want = references(turned, &src, yuv, &sampled);
    let at = |n: &u64| got[*n as usize].as_ref();
    let off = measure(
        sampled
            .iter()
            .map(at)
            .zip(turned_want.iter().map(Vec::as_slice)),
    );
    let (off_psnr, off_ssim) = best(&off);
    eprintln!("{name}: turned 1°, best PSNR {off_psnr:.2} dB, SSIM {off_ssim:.5}");
    let shifted = measure(decoded().zip(want.iter().skip(1).map(Vec::as_slice)));
    let (sh_psnr, sh_ssim) = best(&shifted);
    eprintln!("{name}: shifted a frame, best PSNR {sh_psnr:.2} dB, SSIM {sh_ssim:.5}");
    let (floor_psnr, floor_ssim) = floors;
    assert!(
        psnr >= floor_psnr && ssim >= floor_ssim,
        "{name}: worst {psnr:.2} dB, SSIM {ssim:.5}, below the floors {floor_psnr} dB, {floor_ssim}"
    );
    assert!(
        off_psnr < floor_psnr,
        "{name}: a view 1° off still reaches {off_psnr:.2} dB"
    );
    assert!(
        sh_psnr < floor_psnr,
        "{name}: frames shifted by one still reach {sh_psnr:.2} dB"
    );
    report
}

/// The floors (D52): the worst frame measured on lavapipe with Ubuntu
/// 24.04's FFmpeg 6.1, libx264 0.164.3108, libx265 3.5 and SVT-AV1
/// 1.7.0, less 3 dB of PSNR and 0.003 of SSIM, so above PLAN's 40 dB and
/// 0.98. Measured: flat 52.27 dB and 0.99811; 360 58.83 dB and 0.99961;
/// AV1 50.06 dB and 0.99754 in 8 bits, 51.49 dB and 0.99888 in 10.
const FLAT_FLOORS: (f64, f64) = (49.0, 0.995);
const SPHERE_FLOORS: (f64, f64) = (55.5, 0.996);
const AV1_FLOORS: (f64, f64) = (47.0, 0.9945);
const AV1_TEN_BIT_FLOORS: (f64, f64) = (48.5, 0.9958);

#[test]
fn a_flat_export_through_libx264_matches_the_reference() {
    let src = grid("parity-grid-h264-8", synth::Codec::H264, 8);
    let output = Output::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };
    let trim = Range {
        start: Time::ZERO,
        end: Time::from_secs(2),
    };
    let p = project(&src, trim, output, Codec::H264, 0.0);
    let turned = project(&src, trim, output, Codec::H264, 1.0);
    let r = parity("flat-x264", &p, &turned, YuvLayout::I420, FLAT_FLOORS);
    assert_eq!((r.frames, r.size, r.encoder), (60, (1920, 1080), "libx264"));
    assert!(!r.ten_bit);
    assert_eq!(r.duration, Time::from_secs(2));
}

#[test]
fn a_360_export_through_libx265_matches_the_reference_in_10_bits() {
    let src = grid("parity-grid-hevc-10", synth::Codec::Hevc, 10);
    let at = |yaw: f64| Output::Equirect {
        width: 3840,
        orientation: Orientation::from_ypr(Ypr::from_degrees(yaw, 0.0, 0.0)).unwrap(),
    };
    let trim = Range {
        start: Time::ZERO,
        end: secs(1, 2),
    };
    let p = project(&src, trim, at(30.0), Codec::Hevc, 0.0);
    let turned = project(&src, trim, at(31.0), Codec::Hevc, 0.0);
    let r = parity("360-x265", &p, &turned, YuvLayout::I010, SPHERE_FLOORS);
    assert_eq!((r.frames, r.size, r.encoder), (15, (3840, 1920), "libx265"));
    assert!(r.ten_bit);
    assert_eq!(p.the_clip().source, trim);
}

/// Half a second of `src` through libsvtav1 at 1:1 1080, held to
/// `floors`.
fn av1(name: &str, src: &std::path::Path, yuv: YuvLayout, floors: (f64, f64)) -> Report {
    let output = Output::Flat {
        aspect: Aspect::Square,
        short_side: 1080,
    };
    let trim = Range {
        start: Time::ZERO,
        end: secs(1, 2),
    };
    let p = project(src, trim, output, Codec::Av1, 0.0);
    let turned = project(src, trim, output, Codec::Av1, 1.0);
    let r = parity(name, &p, &turned, yuv, floors);
    assert_eq!(
        (r.frames, r.size, r.encoder),
        (15, (1080, 1080), "libsvtav1")
    );
    r
}

#[test]
fn a_flat_export_through_libsvtav1_matches_the_reference() {
    let src = grid("parity-grid-h264-8", synth::Codec::H264, 8);
    let r = av1("flat-svtav1-8", &src, YuvLayout::I420, AV1_FLOORS);
    assert!(!r.ten_bit);
}

#[test]
fn a_flat_export_through_libsvtav1_matches_the_reference_in_10_bits() {
    let src = grid("parity-grid-hevc-10", synth::Codec::Hevc, 10);
    let r = av1("flat-svtav1-10", &src, YuvLayout::I010, AV1_TEN_BIT_FLOORS);
    assert!(r.ten_bit);
}
