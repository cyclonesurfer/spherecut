//! Tier 1, on lavapipe: `render_still` against the CPU reference (M1 plan
//! section 4.5, DECISIONS D31), for every projection and a morph, and for
//! every plane layout, depth, range, matrix and chroma siting the
//! renderer samples.
//!
//! Each tolerance is a PSNR floor calibrated on lavapipe: the PSNR
//! measured there (in the comment beside it) less 3 dB, rounded down.
//! What is left between the two is the GPU's f32 trigonometry, its
//! filtering weights and its LOD fraction, about 50 dB at 8 bits and 59
//! at 10. No pixel may be off by more than 0.1 either, which a seam column
//! or a black rim pixel would be. Negative controls check that the
//! comparison tells trilinear from level 0, and one siting from another,
//! by far more than the margin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Mutex, OnceLock};

use common::{Frame, Kind, color, frame, gpu};
use spherecut_core::video::{ChromaSiting, Matrix, Range, Transfer};
use spherecut_render::readback::{f16_to_f32, read_texture, round_f16};
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
        // Measured 51.56, 51.63 and 87.53 dB.
        (
            "rectilinear across the seam",
            view(180.0, 20.0, 0.0, lens(Projection::Rectilinear, 100.0)),
            [48.0, 48.0, 84.0],
        ),
        // 50.23, 51.88, 85.54.
        (
            "fisheye over the pole",
            view(30.0, 60.0, 10.0, lens(Projection::Fisheye, 240.0)),
            [47.0, 48.0, 82.0],
        ),
        // 50.99, 52.08, 85.27.
        (
            "tiny planet",
            view(0.0, -90.0, 0.0, lens(Projection::Stereographic, 300.0)),
            [47.0, 49.0, 82.0],
        ),
        // 51.21, 51.96, 87.89.
        ("morph", view(-45.0, 5.0, 0.0, morph), [48.0, 48.0, 84.0]),
        // 49.63, 52.41, 83.89.
        (
            "fisheye 360",
            view(90.0, 0.0, 0.0, lens(Projection::Fisheye, 360.0)),
            [46.0, 49.0, 80.0],
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
        // Measured 50.23 dB.
        (
            Kind::Nv12,
            Range::Limited,
            Matrix::Bt709,
            ChromaSiting::Left,
            47.0,
        ),
        // 58.96.
        (
            Kind::P010,
            Range::Limited,
            Matrix::Bt2020Ncl,
            ChromaSiting::TopLeft,
            55.0,
        ),
        // 51.00.
        (
            Kind::I420,
            Range::Full,
            Matrix::Bt709,
            ChromaSiting::Center,
            47.0,
        ),
        // 58.96.
        (
            Kind::I420p10,
            Range::Full,
            Matrix::Bt601,
            ChromaSiting::Center,
            55.0,
        ),
        // 49.99.
        (
            Kind::I422,
            Range::Limited,
            Matrix::Bt601,
            ChromaSiting::Left,
            46.0,
        ),
        // 58.92.
        (
            Kind::I422p10,
            Range::Limited,
            Matrix::Bt709,
            ChromaSiting::Left,
            55.0,
        ),
        // 50.60.
        (
            Kind::I444,
            Range::Full,
            Matrix::Bt2020Ncl,
            ChromaSiting::Center,
            47.0,
        ),
        // 58.96.
        (
            Kind::I444p12,
            Range::Limited,
            Matrix::Bt709,
            ChromaSiting::Center,
            55.0,
        ),
        // 52.71.
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
    // Measured 52.48 and 52.28 dB against their own siting, 19.60 and
    // 19.62 against the other's.
    check("left siting", &got[0].0, &got[0].1, 49.0);
    check("centre siting", &got[1].0, &got[1].1, 49.0);
    for (a, b) in [(0, 1), (1, 0)] {
        let (p, _) = measure(&got[a].0, &got[b].1);
        assert!(p < 30.0, "the sitings are {p:.2} dB apart");
    }
}

/// Where along the middle row (`across`) or column of `img` channel `c`
/// rises above its value at the image's edge: the centroid of the rise,
/// in output pixels.
fn peak(img: &Image, across: bool, c: usize) -> f64 {
    let (n, mid) = if across {
        (img.width, img.height / 2)
    } else {
        (img.height, img.width / 2)
    };
    let at = |k: u32| {
        let (x, y) = if across { (k, mid) } else { (mid, k) };
        img.unit_at(x, y).unwrap()[c]
    };
    let base = at(0);
    let (mut sum, mut weight) = (0.0, 0.0);
    for k in 0..n {
        let rise = at(k) - base;
        if rise > 1e-3 {
            sum += (f64::from(k) + 0.5) * rise;
            weight += rise;
        }
    }
    assert!(weight > 0.0, "nothing rises");
    sum / weight
}

#[test]
fn chroma_sits_where_its_siting_puts_it() {
    // One raised Cb column (or row) of a 4:2:0 frame, magnified about 27
    // times, must peak where the siting's definition puts that chroma
    // sample: chroma sample j at luma sample 2j + t, so at luma texel
    // coordinate 2j + ½ + t, for t = 0 (Left across, TopLeft down), ½
    // (Center across, Left down) or 1 (BottomLeft down). The geometry is
    // calibrated with two raised luma columns (rows), whose peaks are at
    // their texel centres, so nothing here comes from the renderer's own
    // chroma arithmetic, which the GPU and the reference share.
    let (w, h) = (256u32, 128u32);
    let size = (192, 192);
    let v = view(0.0, 0.0, 0.0, lens(Projection::Rectilinear, 10.0));
    let opts = StillOptions {
        depth: Depth::Sixteen,
        debug: DebugMode::Level0,
        ..StillOptions::default()
    };
    let grey = RgbImage {
        width: w,
        height: h,
        pixels: vec![[0.5; 3]; (w * h) as usize],
    };
    // Plane `p` of an I420 frame at `siting`, flat but for `raised` (luma
    // or chroma column or row `k`), 60 codes up.
    let raised = |siting, p: usize, across: bool, k: u32| {
        let mut f = frame(
            &grey,
            Kind::I420,
            color(Range::Full, Matrix::Bt709, siting),
            0,
        );
        for (i, (bytes, _, _)) in f.planes.iter_mut().enumerate() {
            bytes.fill(128);
            if i == p {
                let (pw, ph) = (
                    if p == 0 { w } else { w / 2 },
                    if p == 0 { h } else { h / 2 },
                );
                for y in 0..ph {
                    for x in 0..pw {
                        if (if across { x } else { y }) == k {
                            bytes[(y * pw + x) as usize] += 60;
                        }
                    }
                }
            }
        }
        f
    };
    let mut r = renderer()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut still = |f: &Frame| r.render(gpu(), f, &v, size, &opts).unwrap();
    // The view looks at luma texel (128, 64); chroma sample (64, 32).
    for (across, line, cases) in [
        (
            true,
            127,
            vec![(ChromaSiting::Left, 0.0), (ChromaSiting::Center, 0.5)],
        ),
        (
            false,
            63,
            vec![
                (ChromaSiting::TopLeft, 0.0),
                (ChromaSiting::Left, 0.5),
                (ChromaSiting::BottomLeft, 1.0),
            ],
        ),
    ] {
        // Luma lines `line` and `line + 2`, centred at `line + ½` and
        // `line + 2½`: output pixels to luma texels. Green rises with Y.
        let a = peak(
            &still(&raised(ChromaSiting::Center, 0, across, line)),
            across,
            1,
        );
        let b = peak(
            &still(&raised(ChromaSiting::Center, 0, across, line + 2)),
            across,
            1,
        );
        let texel = |px: f64| f64::from(line) + 0.5 + (px - a) * 2.0 / (b - a);
        let j = line.div_ceil(2);
        for (siting, t) in cases {
            // Blue rises with Cb.
            let got = texel(peak(&still(&raised(siting, 1, across, j)), across, 2));
            let want = f64::from(2 * j) + 0.5 + t;
            assert!(
                (got - want).abs() < 0.1,
                "{siting:?} chroma {} {j} peaks at luma {got:.3}, not {want}",
                if across { "column" } else { "row" }
            );
        }
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
    // Measured 89.60 dB: the OETF on the CPU, without 8-bit steps.
    check("16-bit", &render(&f, &v, opts), &c, 86.0);
    let mut lin = f.clone();
    lin.color.transfer = Transfer::Linear;
    let c = reference::render_trilinear(&lin, &v, OUT).unwrap();
    // 58.96: no EOTF, the encode pass's OETF all the same.
    check(
        "linear transfer",
        &render(&lin, &v, StillOptions::default()),
        &c,
        55.0,
    );
}

#[test]
fn the_linear_target_holds_the_nearest_half() {
    // 256 bands of grey, k = 0 to 255 at full range, 16 texels wide, all
    // the way round. Where both of a tap's columns lie in one band, the
    // projection writes the EOTF of k / 255, which the linear target must
    // hold rounded to the nearest half, ties to even: the shader rounds
    // it, since lavapipe's own conversion truncates (124 of the 256 a half
    // low without that, DECISIONS D31). The GPU's f32 `pow` may still put
    // a value that is within a millionth of a tie on its other side.
    let (w, h, band) = (4096u32, 64u32, 16u32);
    let grey = RgbImage {
        width: w,
        height: h,
        pixels: (0..w * h)
            .map(|i| [f64::from((i % w) / band) / 255.0; 3])
            .collect(),
    };
    let f = frame(
        &grey,
        Kind::Gray,
        color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
        0,
    );
    let size = (1024, 4);
    let opts = StillOptions {
        debug: DebugMode::Level0,
        depth: Depth::Linear,
        ..StillOptions::default()
    };
    let mut seen = [false; 256];
    let mut wrong = std::collections::BTreeMap::new();
    for yaw in [0.0, 90.0, 180.0, 270.0] {
        let v = view(yaw, 0.0, 0.0, lens(Projection::Rectilinear, 100.0));
        let g = renderer()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .render(gpu(), &f, &v, size, &opts)
            .unwrap();
        for j in 0..size.1 {
            for i in 0..size.0 {
                let d = reference::sphere_dir(&v, size, f64::from(i) + 0.5, f64::from(j) + 0.5);
                // The tap's two columns, with a tenth of a texel to spare.
                let x = reference::sphere_uv(d).x * f64::from(w) - 0.5;
                let col = |x: f64| (x.floor() as i64).rem_euclid(i64::from(w)) as u32 / band;
                let k = col(x - 0.1);
                if col(x + 1.1) != k {
                    continue;
                }
                seen[k as usize] = true;
                let want = round_f16(srgb_eotf(f64::from(k) / 255.0));
                let got = g.unit_at(i, j).unwrap()[0];
                if got != want && !near_a_tie(srgb_eotf(f64::from(k) / 255.0)) {
                    wrong.insert(k, (got, want));
                }
            }
        }
    }
    assert!(seen.iter().all(|&s| s), "some bands were never sampled");
    assert!(
        wrong.is_empty(),
        "{} of 256 bands aren't the nearest half of their value: {:?}",
        wrong.len(),
        wrong.iter().take(6).collect::<Vec<_>>()
    );
}

/// The sRGB EOTF in f64.
fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Whether `v` is within a millionth of a tie between two halves.
fn near_a_tie(v: f64) -> bool {
    round_f16(v * (1.0 - 1e-6)) != round_f16(v * (1.0 + 1e-6))
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
    // Measured 51.80, 49.93 and 50.72 dB.
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

#[test]
fn a_stereo_eye_shows_nothing_of_the_other() {
    // The left eye grey at 50, the right eye a colour at 200, seen across
    // the left eye's ±180° (left-right) and at its nadir (top-bottom),
    // where its edges meet the other eye in the frame. Every pixel must
    // be the left eye's grey, as a mono frame of it renders: 3a's first
    // slots held the whole frame and clamped at the texture's edges, so
    // the right eye bled in (448 pixels of 4096 looking back at a
    // left-right frame, 2652 looking down at a top-bottom one).
    let (w, h) = (512u32, 256u32);
    let size = (64, 64);
    let back = view(180.0, 0.0, 0.0, lens(Projection::Rectilinear, 6.0));
    let down = view(0.0, -90.0, 0.0, lens(Projection::Rectilinear, 6.0));
    let base = || {
        frame(
            &RgbImage {
                width: w,
                height: h,
                pixels: vec![[0.5; 3]; (w * h) as usize],
            },
            Kind::Nv12,
            color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
            8,
        )
    };
    // Y, then Cb and Cr: 50 and neutral in the left eye, 200 elsewhere.
    let coded = |left_eye: &dyn Fn(u32, u32) -> bool| {
        let mut f = base();
        for (p, (bytes, stride, (pw, ph))) in f.planes.iter_mut().enumerate() {
            let comps = if p == 0 { 1 } else { 2 };
            let (sx, sy) = (w / *pw, h / *ph);
            for y in 0..*ph {
                for x in 0..*pw {
                    let left = left_eye(x * sx, y * sy);
                    let v = match (p, left) {
                        (0, true) => 50,
                        (_, true) => 128,
                        (_, false) => 200,
                    };
                    for c in 0..comps {
                        bytes[y as usize * *stride + (x * comps + c) as usize] = v;
                    }
                }
            }
        }
        f
    };
    let mono = coded(&|_, _| true);
    for (stereo, v, left_eye) in [
        (
            Stereo::LeftRight,
            back,
            &(|x, _| x < w / 2) as &dyn Fn(u32, u32) -> bool,
        ),
        (Stereo::TopBottom, down, &|_, y| y < h / 2),
    ] {
        let f = coded(left_eye);
        for debug in [DebugMode::Off, DebugMode::Level0] {
            let opts = |placement| StillOptions {
                debug,
                placement,
                ..StillOptions::default()
            };
            let mut r = renderer()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let want = r
                .render(gpu(), &mono, &v, size, &opts(Placement::FULL))
                .unwrap();
            let placement = Placement {
                stereo,
                ..Placement::FULL
            };
            let got = r.render(gpu(), &f, &v, size, &opts(placement)).unwrap();
            let a = got.unit_rgb();
            let b = want.unit_rgb();
            let off = a
                .iter()
                .zip(&b)
                .filter(|(a, b)| (*a - *b).abs() > 1.5 / 255.0)
                .count();
            let worst = a
                .iter()
                .zip(&b)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            assert_eq!(
                off,
                0,
                "{stereo:?} {debug:?}: {off} channels show the other eye, by up to {:.0} codes",
                worst * 255.0
            );
        }
    }
}

/// The IEEE half nearest `v`, ties to even, worked out from its bits:
/// what an f16 store of `v` must hold. Written apart from the shader and
/// from `readback::round_f16`.
fn half_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let mant = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | if mant == 0 { 0x7c00 } else { 0x7e00 };
    }
    // f32's own subnormals are far below half the least half.
    if exp == 0 {
        return sign;
    }
    // |v| = m · 2^(e − 23): drop the bits of m below the half's last one,
    // 13 for a normal half (e ≥ −14), and below 2^−24 for a subnormal one.
    let (m, e) = (mant | 0x80_0000, exp - 127);
    let shift = if e >= -14 { 13 } else { (-1 - e) as u32 };
    if shift > 25 {
        return sign;
    }
    let (q, rest, half) = (m >> shift, m & ((1 << shift) - 1), 1 << (shift - 1));
    let q = if rest > half || (rest == half && q & 1 == 1) {
        q + 1
    } else {
        q
    };
    // A normal half's q is 1024 to 2048, and 2048 carries into the next
    // exponent; a subnormal's q of 1024 is the least normal half.
    let bits = if e >= -14 {
        (e + 15) as u32 * 1024 + q - 1024
    } else {
        q
    };
    sign | bits.min(0x7c00) as u16
}

#[test]
fn round_f16_is_the_nearest_half() {
    // `round_f16` (shaders/common.wgsl), which every f16 store goes
    // through, run on its own: for every finite half of both signs, each
    // midpoint between two of them (and between 65504 and 65536, past
    // which the nearest is infinite), each midpoint an f32 ulp either
    // side, and values far past the largest half, it must return the
    // nearest half, ties to even, as an f32 (an Rgba32Float target) and
    // as the half an Rgba16Float target stores. The subnormal halves,
    // which no 10- or 12-bit code reaches, are among them.
    let gpu = gpu();
    let mut values = Vec::new();
    for sign in [1.0f32, -1.0] {
        for h in 0u16..0x7c00 {
            let x = f16_to_f32(h);
            let next = if h + 1 == 0x7c00 {
                65536.0
            } else {
                f16_to_f32(h + 1)
            };
            // Exact: a half has 11 bits, an f32 24.
            let mid = (x + next) / 2.0;
            let around = [
                f32::from_bits(mid.to_bits() - 1),
                f32::from_bits(mid.to_bits() + 1),
            ];
            values.extend([x, mid, around[0], around[1]].map(|v| v * sign));
        }
        values.extend([65536.0, 7e4, 1e6, 1e30, f32::MAX, f32::INFINITY, 1e-30].map(|v| v * sign));
    }
    let width = 256u32;
    let height = (values.len() as u32).div_ceil(4 * width);
    values.resize((4 * width * height) as usize, 0.0);
    let device = &gpu.device;
    let texture = |label, format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let input = texture(
        "f32 bits",
        wgpu::TextureFormat::Rgba32Uint,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let bytes: Vec<u8> = values
        .iter()
        .flat_map(|v| v.to_bits().to_le_bytes())
        .collect();
    gpu.queue.write_texture(
        input.as_image_copy(),
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(16 * width),
            rows_per_image: None,
        },
        input.size(),
    );
    let target = |format| {
        texture(
            "round_f16",
            format,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        )
    };
    let (wide, narrow) = (
        target(wgpu::TextureFormat::Rgba32Float),
        target(wgpu::TextureFormat::Rgba16Float),
    );
    let source = format!(
        "{}{}",
        spherecut_render::layouts::COMMON_WGSL,
        r#"
@group(0) @binding(0) var bits: texture_2d<u32>;
struct Out { @location(0) wide: vec4<f32>, @location(1) narrow: vec4<f32> };
@fragment
fn fs_probe(@builtin(position) frag: vec4<f32>) -> Out {
    let r = round_f16(bitcast<vec4<f32>>(textureLoad(bits, vec2<u32>(frag.xy), 0)));
    return Out(r, r);
}
"#
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("round_f16 probe"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("round_f16 probe"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_full"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[wide.format(), narrow.format()].map(|format| {
                Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })
            }),
        }),
        multiview_mask: None,
        cache: None,
    });
    let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("round_f16 probe"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view(&input)),
        }],
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let (vw, vn) = (view(&wide), view(&narrow));
        let attach = |v| {
            Some(wgpu::RenderPassColorAttachment {
                view: v,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })
        };
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("round_f16 probe"),
            color_attachments: &[attach(&vw), attach(&vn)],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
    gpu.queue.submit([enc.finish()]);
    let wide = read_texture(gpu, &wide, 0).unwrap();
    let narrow = read_texture(gpu, &narrow, 0).unwrap();
    let mut wrong = Vec::new();
    for (i, &v) in values.iter().enumerate() {
        let want = half_bits(v);
        let got_wide = f32::from_le_bytes(wide.data[4 * i..4 * i + 4].try_into().unwrap());
        let got_narrow = u16::from_le_bytes([narrow.data[2 * i], narrow.data[2 * i + 1]]);
        if got_wide.to_bits() != f16_to_f32(want).to_bits() || got_narrow != want {
            wrong.push((
                v,
                got_wide,
                format!("{got_narrow:#06x}"),
                format!("{want:#06x}"),
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} values aren't rounded to their nearest half, such as {:?}",
        wrong.len(),
        values.len(),
        &wrong[..wrong.len().min(6)]
    );
}

#[test]
fn the_probes_half_is_ieees() {
    // half_bits against values whose halves are known.
    assert_eq!(half_bits(0.0), 0x0000);
    assert_eq!(half_bits(-0.0), 0x8000);
    assert_eq!(half_bits(1.0), 0x3c00);
    assert_eq!(half_bits(1.0 + 2f32.powi(-11)), 0x3c00, "a tie, to even");
    assert_eq!(
        half_bits(1.0 + 3.0 * 2f32.powi(-11)),
        0x3c02,
        "a tie, to even"
    );
    assert_eq!(half_bits(65504.0), 0x7bff);
    assert_eq!(half_bits(65519.996), 0x7bff);
    assert_eq!(half_bits(65520.0), 0x7c00, "a tie with 65536, infinite");
    assert_eq!(half_bits(-1e6), 0xfc00);
    assert_eq!(half_bits(2f32.powi(-24)), 0x0001);
    assert_eq!(half_bits(2f32.powi(-25)), 0x0000, "a tie, to even");
    assert_eq!(half_bits(3.0 * 2f32.powi(-25)), 0x0002, "a tie, to even");
    assert_eq!(half_bits(2f32.powi(-14) * (1.0 - 2f32.powi(-12))), 0x0400);
    assert_eq!(half_bits(f32::NAN) & 0x7c00, 0x7c00);
    // Every half is its own.
    for h in (0u16..0x7c00).chain(0x8000..0xfc00) {
        assert_eq!(half_bits(f16_to_f32(h)), h, "{h:#06x}");
    }
}
