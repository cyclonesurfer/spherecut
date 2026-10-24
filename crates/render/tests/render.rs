//! Tier 1, on lavapipe: `render_still` against the CPU reference (M1 plan
//! section 4.5, DECISIONS D31), for every projection and a morph, and for
//! every plane layout, depth, range, matrix and chroma siting the
//! renderer samples.
//!
//! Each tolerance is a PSNR floor calibrated on lavapipe: the PSNR
//! measured there (in the comment beside it) less 3 dB, rounded down.
//! What is left between the two is the GPU's f32 trigonometry, its
//! filtering weights and its LOD fraction, about 50 dB at 8 bits and 58
//! at 10. No pixel may be off by more than 0.1 either, which a seam column
//! or a black rim pixel would be. Negative controls check that the
//! comparison tells trilinear from level 0, and one siting from another,
//! by far more than the margin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Mutex, OnceLock};

use common::{Frame, Kind, color, frame, gpu};
use spherecut_core::video::{ChromaSiting, Matrix, Range, Transfer};
use spherecut_render::reference::{self, ImageF64};
use spherecut_render::{
    DebugMode, Depth, Image, Placement, Stereo, StillOptions, StillRenderer, ViewParams,
};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_synth::{Pattern, RgbImage, render_equirect};
use spherecut_testkit::compare::{View, diff_stats, psnr};

const OUT: (u32, u32) = (192, 128);

/// One renderer for the binary, so that its pipelines are built once.
fn renderer() -> &'static Mutex<StillRenderer> {
    static R: OnceLock<Mutex<StillRenderer>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(StillRenderer::new(gpu())))
}

/// synth's general scene, 512×256: 4 luma levels and 3 chroma ones.
fn grid() -> &'static RgbImage {
    static GRID: OnceLock<RgbImage> = OnceLock::new();
    GRID.get_or_init(|| render_equirect(Pattern::Grid, 512, 256, 0, 2))
}

fn view(yaw: f64, pitch: f64, roll: f64, lens: Lens) -> ViewParams {
    ViewParams::new(Ypr::from_degrees(yaw, pitch, roll).to_quat(), lens)
}

fn lens(p: Projection, hfov_deg: f64) -> Lens {
    Lens::new(p, hfov_deg.to_radians())
}

/// Each projection where it is hard (the seam, a pole, minification) and
/// a morph half way, with the trilinear, level-0 and LOD floors.
fn views() -> Vec<(&'static str, ViewParams, [f64; 3])> {
    let morph = Lens {
        from: Projection::Rectilinear,
        to: Projection::Stereographic,
        morph: 0.5,
        hfov: 150f64.to_radians(),
    };
    vec![
        // Measured 51.56, 51.63 and 77.62 dB.
        (
            "rectilinear across the seam",
            view(180.0, 20.0, 0.0, lens(Projection::Rectilinear, 100.0)),
            [48.0, 48.0, 74.0],
        ),
        // 50.22, 51.87, 67.29.
        (
            "fisheye over the pole",
            view(30.0, 60.0, 10.0, lens(Projection::Fisheye, 240.0)),
            [47.0, 48.0, 64.0],
        ),
        // 50.98, 52.08, 68.40.
        (
            "tiny planet",
            view(0.0, -90.0, 0.0, lens(Projection::Stereographic, 300.0)),
            [47.0, 49.0, 65.0],
        ),
        // 51.19, 51.97, 72.30.
        ("morph", view(-45.0, 5.0, 0.0, morph), [48.0, 48.0, 69.0]),
        // 49.60, 52.41, 63.44.
        (
            "fisheye 360",
            view(90.0, 0.0, 0.0, lens(Projection::Fisheye, 360.0)),
            [46.0, 49.0, 60.0],
        ),
    ]
}

/// The PSNR of `gpu` against `cpu`, and how many channels are off by
/// more than 0.1.
fn measure(gpu: &Image, cpu: &ImageF64) -> (f64, usize) {
    let (w, h) = (gpu.width as usize, gpu.height as usize);
    assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));
    let g = gpu.unit_rgb();
    let a = View::new(w, h, 3, &g);
    let b = View::new(w, h, 3, &cpu.data);
    let d = diff_stats(a, b, 0.1).unwrap();
    (psnr(a, b).unwrap(), d.count_over)
}

/// Fails unless `gpu` is within `floor` dB of `cpu`, with no channel off
/// by more than 0.1.
fn check(what: &str, gpu: &Image, cpu: &ImageF64, floor: f64) -> f64 {
    let (p, over) = measure(gpu, cpu);
    eprintln!("{what}: {p:.2} dB (floor {floor}), {over} channels off by more than 0.1");
    assert!(p >= floor, "{what}: {p:.2} dB, below {floor}");
    assert_eq!(over, 0, "{what}: {over} channels off by more than 0.1");
    p
}

fn render(frame: &Frame, view: &ViewParams, opts: StillOptions) -> Image {
    let mut r = renderer()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    r.render(gpu(), frame, view, OUT, &opts).unwrap()
}

fn nv12() -> Frame {
    frame(
        grid(),
        Kind::Nv12,
        color(Range::Limited, Matrix::Bt709, ChromaSiting::Left),
        40,
    )
}

#[test]
fn every_projection_matches_the_reference() {
    let f = nv12();
    for (name, v, [tri, l0, _]) in views() {
        let g = render(&f, &v, StillOptions::default());
        let trilinear = reference::render_trilinear(&f, &v, OUT).unwrap();
        check(&format!("trilinear, {name}"), &g, &trilinear, tri);
        let g = render(
            &f,
            &v,
            StillOptions {
                debug: DebugMode::Level0,
                ..StillOptions::default()
            },
        );
        check(
            &format!("level 0, {name}"),
            &g,
            &reference::render_level0(&f, &v, OUT).unwrap(),
            l0,
        );
        // Negative control: level 0 isn't trilinear (measured 22.6 to
        // 43.8 dB, the least where the seam view barely minifies).
        let (p, _) = measure(&g, &trilinear);
        assert!(p < tri - 3.0, "{name}: level 0 is {p:.2} dB from trilinear");
    }
}

#[test]
fn every_layout_matches_the_reference() {
    let v = views()[1].1;
    // Through the fisheye over the pole, which both magnifies and
    // minifies, crosses the seam and meets the pole.
    let cases = [
        // Measured 50.22 dB.
        (
            Kind::Nv12,
            Range::Limited,
            Matrix::Bt709,
            ChromaSiting::Left,
            47.0,
        ),
        // 58.21.
        (
            Kind::P010,
            Range::Limited,
            Matrix::Bt2020Ncl,
            ChromaSiting::TopLeft,
            55.0,
        ),
        // 50.97.
        (
            Kind::I420,
            Range::Full,
            Matrix::Bt709,
            ChromaSiting::Center,
            47.0,
        ),
        // 58.51.
        (
            Kind::I420p10,
            Range::Full,
            Matrix::Bt601,
            ChromaSiting::Center,
            55.0,
        ),
        // 49.98.
        (
            Kind::I422,
            Range::Limited,
            Matrix::Bt601,
            ChromaSiting::Left,
            46.0,
        ),
        // 58.09.
        (
            Kind::I422p10,
            Range::Limited,
            Matrix::Bt709,
            ChromaSiting::Left,
            55.0,
        ),
        // 50.57.
        (
            Kind::I444,
            Range::Full,
            Matrix::Bt2020Ncl,
            ChromaSiting::Center,
            47.0,
        ),
        // 57.35.
        (
            Kind::I444p12,
            Range::Limited,
            Matrix::Bt709,
            ChromaSiting::Center,
            54.0,
        ),
        // 52.65.
        (
            Kind::Gray,
            Range::Limited,
            Matrix::Bt709,
            ChromaSiting::Left,
            49.0,
        ),
        // 54.91.
        (
            Kind::Gbrp,
            Range::Full,
            Matrix::Identity,
            ChromaSiting::Left,
            51.0,
        ),
    ];
    for (kind, range, matrix, siting, floor) in cases {
        let f = frame(grid(), kind, color(range, matrix, siting), 24);
        let g = render(&f, &v, StillOptions::default());
        let c = reference::render_trilinear(&f, &v, OUT).unwrap();
        check(
            &format!("{kind:?} {range:?} {matrix:?} {siting:?}"),
            &g,
            &c,
            floor,
        );
    }
}

/// Red and blue stripes three pixels wide: chroma that changes every one
/// and a half chroma samples, so that where they sit shows.
fn stripes() -> RgbImage {
    let (w, h) = (512u32, 256u32);
    RgbImage {
        width: w,
        height: h,
        pixels: (0..w * h)
            .map(|i| {
                if (i % w) / 3 % 2 == 0 {
                    [0.9, 0.1, 0.2]
                } else {
                    [0.1, 0.3, 0.9]
                }
            })
            .collect(),
    }
}

#[test]
fn chroma_siting_is_honoured() {
    // The stripes magnified about seven times, so that half a luma texel
    // is three output pixels.
    let v = view(10.0, 0.0, 0.0, lens(Projection::Rectilinear, 20.0));
    let img = stripes();
    let mut got = Vec::new();
    for siting in [ChromaSiting::Left, ChromaSiting::Center] {
        let f = frame(
            &img,
            Kind::I420,
            color(Range::Limited, Matrix::Bt709, siting),
            0,
        );
        let g = render(&f, &v, StillOptions::default());
        got.push((g, reference::render_trilinear(&f, &v, OUT).unwrap()));
    }
    // Measured 52.54 and 52.29 dB against their own siting, 19.60 and
    // 19.62 against the other's.
    check("left siting", &got[0].0, &got[0].1, 49.0);
    check("centre siting", &got[1].0, &got[1].1, 49.0);
    for (a, b) in [(0, 1), (1, 0)] {
        let (p, _) = measure(&got[a].0, &got[b].1);
        assert!(p < 30.0, "the sitings are {p:.2} dB apart");
    }
}

#[test]
fn sixteen_bits_and_linear_light() {
    let f = frame(
        grid(),
        Kind::P010,
        color(Range::Limited, Matrix::Bt709, ChromaSiting::Left),
        0,
    );
    let v = views()[0].1;
    let c = reference::render_trilinear(&f, &v, OUT).unwrap();
    let opts = StillOptions {
        depth: Depth::Sixteen,
        ..StillOptions::default()
    };
    // Measured 68.98 dB: the OETF on the CPU, without 8-bit steps.
    check("16-bit", &render(&f, &v, opts), &c, 65.0);
    let mut lin = f.clone();
    lin.color.transfer = Transfer::Linear;
    let c = reference::render_trilinear(&lin, &v, OUT).unwrap();
    // 58.52: no EOTF, the encode pass's OETF all the same.
    check(
        "linear transfer",
        &render(&lin, &v, StillOptions::default()),
        &c,
        55.0,
    );
}

#[test]
fn lod_heatmap_matches_the_reference() {
    let f = nv12();
    for (name, v, [_, _, floor]) in views() {
        let opts = StillOptions {
            debug: DebugMode::LodHeatmap,
            depth: Depth::Linear,
            ..StillOptions::default()
        };
        let c = reference::lod_map(&f, &v, OUT, &Placement::FULL).unwrap();
        check(&format!("LOD, {name}"), &render(&f, &v, opts), &c, floor);
    }
}

#[test]
fn placement_crops_and_eyes() {
    let f = nv12();
    let v = views()[4].1;
    let crop = Placement {
        left: 0.25,
        right: 0.25,
        top: 0.1,
        bottom: 0.0,
        stereo: Stereo::Mono,
    };
    let eye = |stereo| Placement {
        stereo,
        ..Placement::FULL
    };
    // Measured 51.78, 49.90 and 50.67 dB.
    for (p, floor) in [
        (crop, 48.0),
        (eye(Stereo::TopBottom), 46.0),
        (eye(Stereo::LeftRight), 47.0),
    ] {
        let opts = StillOptions {
            placement: p,
            ..StillOptions::default()
        };
        let c = reference::render(
            &f,
            &v,
            OUT,
            &reference::Options {
                level0: false,
                placement: p,
            },
        )
        .unwrap();
        check(&format!("{p:?}"), &render(&f, &v, opts), &c, floor);
    }
}
