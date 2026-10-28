//! The golden images (M1 plan section 6, DECISIONS D32), on lavapipe.
//!
//! Every row of the plan's table is a function below, run in order by
//! one test, [`goldens`], which gathers every failure before it reports,
//! so that blessing can refuse as a whole:
//!
//! - **Stored images** (`goldens/*.png`, with `goldens/manifest.json`)
//!   must match the render pixel for pixel at the driver build and source
//!   hash they were blessed at. At another build they must keep PSNR ≥ 48
//!   dB and SSIM ≥ 0.995, with at most 4 pixels more than 4 LSB off and
//!   none more than 8: the averages miss a fault a few dozen pixels wide,
//!   which [`the_other_driver_rule_catches_local_faults`] makes from every
//!   stored view, and a pixel 5 off is the drift made here (D32, amended
//!   2026-10-28). A source whose planes no longer hash as blessed is
//!   reported as the synthetic source having changed, never compared as
//!   a render.
//! - **The CPU reference** (`reference::render_trilinear`, f64) holds each
//!   image to a PSNR floor calibrated when blessing: the PSNR measured
//!   then, less 3 dB, and never below 36 dB.
//! - **Minified views** are compared with `synth::render_view_ideal`, the
//!   supersampled pattern itself, with a floor and a margin of at least 3
//!   dB over the level-0 control, on the pixels the analytic LOD puts 2.5
//!   levels down or more ([`IDEAL_MIN_LOD`]).
//! - **Seams and poles** count bad pixels against a roll twin (the
//!   texture rolled by half its width and the view turned 180°), with the
//!   implicit-LOD control, which must show the line the analytic LOD
//!   removes.
//! - **Negative controls** are measured and asserted every run: a
//!   control that stops failing fails the test.
//!
//! `SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test
//! goldens` rewrites the PNGs and the manifest, and only when every check
//! passes and every control fails as it should; otherwise it lists what
//! failed and writes nothing. A failing run writes the images it compared
//! to `golden-diffs/` in the cargo target directory. Every run is on the
//! CPU adapter (lavapipe), asserted, and every failure names its driver
//! build. Filtering is `Filter::Isotropic` throughout.
//!
//! The plan's `mips_area` row is `mips_are_area_means_at_odd_sizes` in
//! `tests/upload.rs` (45 to 22 and 23 to 11, R8, Rg8, R16Float and
//! Rg16Float, against an independent area mean), which covers it whole.
//!
//! The Tier 2 tests at the end run the same rows on a GPU, against the
//! CPU and ideal references only, at the floors blessed here, and on the
//! machine's own llvmpipe by the other-driver rule, recording the drift;
//! on a GPU each CPU check's error is broken down by LOD band too. The
//! mip pass rounds its 8-bit means to codes and the encode pass its
//! output, so a driver's conversion has nothing left to round: when the
//! mip pass left its means to the driver, RADV's export, which packs
//! them to f16 toward zero first, cost the RX 7600's wide, minified views
//! up to 15 dB, as [`the_wide_views_lose_to_the_mip_stores_not_the_filter`]
//! shows by emulating it, and predicts what they come to now.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use common::{
    Frame, Kind, color, frame, gpu, hardware_gpu, hardware_gpus, lavapipe_unorm8, radv_unorm8,
};
use glam::{DQuat, DVec2, DVec3};
use serde_json::{Map, Value, json};
use spherecut_core::video::{
    ChromaSiting, ColorInfo, Matrix, Range, SampleBytes, Transfer, VideoPlanes,
};
use spherecut_render::readback::round_f16;
use spherecut_render::reference::{self, ImageF64};
use spherecut_render::{
    AdapterChoice, DebugMode, Depth, Gpu, Image, LayoutKind, Pixels, Placement, PlaneFormat,
    SlotShape, Stereo, StillOptions, StillRenderer, ViewParams,
};
use spherecut_sphere::LonLat;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_sphere::pixel;
use spherecut_synth::{Pattern, RgbImage, render_equirect, render_view_ideal};
use spherecut_testkit::compare::{DiffStats, View, diff_stats, psnr, ssim};
use spherecut_testkit::{Fnv64, bench, tier2};

/// Where the stored images and the manifest live.
const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens");
const MANIFEST: &str = "manifest.json";
/// Set to bless; set to [`LOWER`] to let the blessing lower a floor by
/// more than [`FLOOR_DROP_DB`].
const UPDATE: &str = "SPHERECUT_UPDATE_GOLDENS";
const LOWER: &str = "lower";
/// What rewrites the goldens.
const REGENERATE: &str = "SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test goldens";

/// The other-driver rule: 48.13 dB is an RMS error of one 8-bit step.
const OTHER_PSNR_DB: f64 = 48.0;
const OTHER_SSIM: f64 = 0.995;
/// And what the averages miss: at most [`OTHER_PIXELS_PAST`] pixels with
/// a channel more than [`OTHER_PAST_LSB`] off, and none more than
/// [`OTHER_MAX_LSB`]. LLVM 22.1.8's llvmpipe put `fisheye_180` at most 5
/// LSB off at 62.79 dB; within PSNR and SSIM, implicit LOD puts 11 to 72
/// pixels past 4 LSB by up to 9, and a black pole quad is 95 LSB off
/// (D32, `the_other_driver_rule_catches_local_faults`).
const OTHER_PAST_LSB: f64 = 4.0;
const OTHER_PIXELS_PAST: f64 = 4.0;
const OTHER_MAX_LSB: f64 = 8.0;
/// The CPU tolerance: measured − 3 dB when blessing, never below 36.
const CPU_MARGIN_DB: f64 = 3.0;
const CPU_FLOOR_DB: f64 = 36.0;
/// No channel may be further than this from the CPU reference, which a
/// seam column or a misplaced rim would be (as in `tests/render.rs`).
const CPU_MAX_CHANNEL: f64 = 0.1;
/// How far a blessing may lower a calibrated floor unasked. At one
/// driver build a floor comes out the same at every blessing, so a drop
/// past this is a render that got worse, or another driver, and needs
/// `SPHERECUT_UPDATE_GOLDENS=lower`.
const FLOOR_DROP_DB: f64 = 0.5;
/// The ideal floor: measured − 1 dB when blessing. The comparison is with
/// the pattern itself, so it moves with filtering, not with the GPU's
/// rounding, and a 1 dB drop is a real loss of detail or a new alias.
const IDEAL_MARGIN_DB: f64 = 1.0;
/// Above the level-0 control, against the ideal.
const IDEAL_OVER_LEVEL0_DB: f64 = 3.0;
/// Samples per pixel side of the ideal: 8 for the grid, 16 for the
/// checker, whose cells a minified pixel covers dozens of.
const IDEAL_N_GRID: u32 = 8;
const IDEAL_N_CHECKER: u32 = 16;
/// The ideal comparisons cover the pixels the analytic LOD puts at
/// least this many levels down (a footprint of 5.7 texels or more),
/// where level 0's bilinear tap reads 4 of the 32 or more texels under
/// the pixel and must alias. Where minification is milder, isotropic
/// trilinear is softer than the box-filtered ideal and level 0 is close
/// to it, so neither margin can hold there (D32 has the measurements).
const IDEAL_MIN_LOD: f64 = 2.5;
/// The fewest pixels an ideal comparison may rest on.
const IDEAL_MIN_PIXELS: f64 = 1000.0;
/// The BT.601 and BT.709 codings of the grid, rendered, against each
/// other: 46.3 dB measured, 24.7 dB with the matrices swapped in the
/// colour fold (D32).
const MATRIX_AGREE_DB: f64 = 40.0;
/// `siting_left_center`'s smooth chroma against its 4:4:4 truth: at its
/// own siting 51.4 dB measured, and 17.5 dB nearer than tagged as the
/// other siting, which a Left/Center swap in `chroma_transform` turns
/// into 17.5 dB further (D32).
const SITING_SWING_DB: f64 = 45.0;
const SITING_OVER_DB: f64 = 10.0;
/// Seams and poles: a pixel is bad past 2/255, and the implicit-LOD
/// control must give more than 100 of them.
const TWIN_THRESHOLD: f64 = 2.0 / 255.0;
const CONTROL_BAD_OVER: usize = 100;

// ---------------------------------------------------------------------
// On a GPU (D32)

/// Whether pixel `(i, j)` of a `size` view shows the picture: its centre
/// within θ = π and inside the cover, as the shader and the reference
/// have it. Elsewhere both draw black.
fn shows(v: &ViewParams, size: (u32, u32), placement: &Placement, (i, j): (u32, u32)) -> bool {
    let c = pixel::centre(i, j);
    let theta = v.lens.theta(pixel::to_lens(size, c.x, c.y).length());
    let suv = reference::sphere_uv(reference::sphere_dir(v, size, c.x, c.y));
    let [u0, v0, u1, v1] = placement.cover();
    theta <= PI + 1e-4 && suv.x >= u0 && suv.y >= v0 && suv.x <= u1 && suv.y <= v1
}

/// The bands of luma's analytic LOD before clamping that a CPU check's
/// error is broken down by, each up to its bound: magnified or at most a
/// level down, between, and from [`IDEAL_MIN_LOD`] down.
const LOD_BANDS: [(&str, f64); 3] = [
    ("lod_below_1", 1.0),
    ("lod_1_to_2.5", IDEAL_MIN_LOD),
    ("lod_from_2.5", f64::INFINITY),
];

/// Each pixel's band in [`LOD_BANDS`], by `reference::lod_map`, or `None`
/// where the view doesn't show the picture ([`shows`]).
fn lod_bands(
    f: &Frame,
    v: &ViewParams,
    size: (u32, u32),
    placement: &Placement,
) -> Vec<Option<usize>> {
    let lod = reference::lod_map(f, v, size, placement).unwrap();
    lod.data
        .chunks(3)
        .enumerate()
        .map(|(k, l)| {
            let at = ((k % size.0 as usize) as u32, (k / size.0 as usize) as u32);
            shows(v, size, placement, at)
                .then(|| LOD_BANDS.iter().position(|b| l[2] < b.1).unwrap())
        })
        .collect()
}

/// A CPU check's error over one band of [`LOD_BANDS`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct BandError {
    pixels: f64,
    /// Over the band's pixels; `None` when it has none.
    psnr: Option<f64>,
    /// Of the squared error over the whole picture.
    share: f64,
}

/// `got` against `want` per band, as `bands` deals the pixels.
fn band_errors(bands: &[Option<usize>], got: &[f64], want: &[f64]) -> Vec<BandError> {
    let total: f64 = got.iter().zip(want).map(|(a, b)| (a - b).powi(2)).sum();
    (0..LOD_BANDS.len())
        .map(|n| {
            let mask: Vec<bool> = bands.iter().map(|b| *b == Some(n)).collect();
            let pixels = mask.iter().filter(|m| **m).count();
            if pixels == 0 {
                return BandError {
                    pixels: 0.0,
                    psnr: None,
                    share: 0.0,
                };
            }
            let (a, b) = (pick(&mask, got), pick(&mask, want));
            let sq: f64 = a.iter().zip(&b).map(|(x, y)| (x - y).powi(2)).sum();
            BandError {
                pixels: pixels as f64,
                psnr: Some(psnr_in(&mask, got, want)),
                share: if total > 0.0 { sq / total } else { 0.0 },
            }
        })
        .collect()
}

// ---------------------------------------------------------------------
// Sources

/// synth's general scene, 1024×512, made once per binary.
fn grid() -> &'static RgbImage {
    static G: OnceLock<RgbImage> = OnceLock::new();
    G.get_or_init(|| render_equirect(Pattern::Grid, 1024, 512, 0, 2))
}

/// The 1° checker, 2048×1024.
fn checker() -> &'static RgbImage {
    static C: OnceLock<RgbImage> = OnceLock::new();
    C.get_or_init(|| render_equirect(Pattern::HighFrequency, 2048, 1024, 0, 2))
}

/// A frame and what it is, for the manifest.
struct Source {
    frame: Frame,
    about: Value,
}

impl Source {
    fn new(pattern: &str, frame: Frame) -> Source {
        let c = &frame.color;
        let about = json!({
            "pattern": pattern,
            "size": [frame.size.0, frame.size.1],
            "layout": frame.layout.name,
            "range": format!("{:?}", c.range).to_lowercase(),
            "matrix": format!("{:?}", c.matrix).to_lowercase(),
            "siting": format!("{:?}", c.siting).to_lowercase(),
        });
        Source { frame, about }
    }

    /// FNV-64 of the planes' samples, row by row without the padding.
    fn hash(&self) -> u64 {
        let mut h = Fnv64::new();
        for (desc, (bytes, stride, (w, rows))) in
            self.frame.layout.planes.iter().zip(&self.frame.planes)
        {
            let row = desc.row_bytes(*w);
            for y in 0..*rows as usize {
                h.write(&bytes[y * stride..y * stride + row]);
            }
        }
        h.finish()
    }
}

fn limited709() -> ColorInfo {
    color(Range::Limited, Matrix::Bt709, ChromaSiting::Left)
}

/// The grid as 8-bit limited-range NV12, BT.709, Left-sited, with padded
/// rows: the goldens' everyday source.
fn grid_nv12() -> Source {
    Source::new("grid", frame(grid(), Kind::Nv12, limited709(), 32))
}

/// The checker as full-range 8-bit gray: one R8 plane.
fn checker_gray() -> Source {
    Source::new(
        "hifreq",
        frame(
            checker(),
            Kind::Gray,
            color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
            0,
        ),
    )
}

/// `f` with each plane's rows turned by half the picture's width: the
/// texture of the roll twin, whose seam is where `f`'s middle column is.
fn rolled(f: &Frame) -> Frame {
    let mut out = f.clone();
    for (desc, (bytes, stride, (w, h))) in out.layout.planes.iter().zip(out.planes.iter_mut()) {
        let shift = (f.size.0 / 2) >> desc.log2_sub.0;
        assert_eq!(
            shift << desc.log2_sub.0,
            f.size.0 / 2,
            "the roll must be whole texels"
        );
        let texel = desc.row_bytes(1);
        let row = desc.row_bytes(*w);
        for y in 0..*h as usize {
            let r = &mut bytes[y * *stride..y * *stride + row];
            r.rotate_right(shift as usize * texel);
        }
    }
    out
}

/// `f` with every plane's rows `stride(row bytes)` apart.
fn restride(f: &Frame, stride: impl Fn(usize) -> usize) -> Frame {
    let mut out = f.clone();
    for (desc, plane) in out.layout.planes.iter().zip(out.planes.iter_mut()) {
        let (bytes, old, (w, h)) = plane;
        let row = desc.row_bytes(*w);
        let new = stride(row);
        assert!(new >= row);
        let mut b = vec![0xa5u8; new * (*h as usize - 1) + row];
        for y in 0..*h as usize {
            b[y * new..y * new + row].copy_from_slice(&bytes[y * *old..y * *old + row]);
        }
        *bytes = b;
        *old = new;
    }
    out
}

/// A frame of `kind` from per-plane sample functions `(plane, x, y) →
/// code`, rows tight, placed where the format puts its bits.
fn coded(
    kind: Kind,
    size: (u32, u32),
    color: ColorInfo,
    code: impl Fn(usize, u32, u32) -> u16,
) -> Frame {
    let layout = kind.layout();
    let shift = if layout.msb_aligned {
        16 - layout.bit_depth
    } else {
        0
    };
    let planes = layout
        .planes
        .iter()
        .enumerate()
        .map(|(i, desc)| {
            let (w, h) = desc.size(size.0, size.1);
            let comps = u32::from(desc.components);
            let mut bytes = Vec::with_capacity(desc.row_bytes(w) * h as usize);
            for y in 0..h {
                for x in 0..w * comps {
                    // Interleaved chroma: U then V, as planes 1 and 2.
                    let plane = if comps == 2 { 1 + (x % 2) as usize } else { i };
                    let c = code(plane, x / comps, y);
                    match desc.bytes {
                        SampleBytes::One => bytes.push(c as u8),
                        SampleBytes::Two => bytes.extend_from_slice(&(c << shift).to_le_bytes()),
                    }
                }
            }
            (bytes, desc.row_bytes(w), (w, h))
        })
        .collect();
    Frame {
        layout,
        size,
        planes,
        color,
    }
}

// ---------------------------------------------------------------------
// Views

fn ypr(yaw: f64, pitch: f64, roll: f64) -> DQuat {
    Ypr::from_degrees(yaw, pitch, roll).to_quat()
}

fn lens(p: Projection, hfov_deg: f64) -> Lens {
    Lens::new(p, hfov_deg.to_radians())
}

fn proj_name(p: Projection) -> &'static str {
    match p {
        Projection::Rectilinear => "rectilinear",
        Projection::Fisheye => "fisheye",
        Projection::Stereographic => "stereographic",
    }
}

/// A view as the manifest records it.
fn view_about(v: &ViewParams, size: (u32, u32)) -> Value {
    let e = Ypr::from_quat(v.orientation);
    let q = v.orientation.normalize();
    let mut m = json!({
        "size": [size.0, size.1],
        "projection": proj_name(v.lens.from),
        "hfov_deg": num(v.lens.hfov.to_degrees()),
        "yaw_deg": num(e.yaw.to_degrees()),
        "pitch_deg": num(e.pitch.to_degrees()),
        "roll_deg": num(e.roll.to_degrees()),
        "quaternion_xyzw": [q.x, q.y, q.z, q.w],
        "filter": "isotropic",
    });
    if v.lens.from != v.lens.to {
        m["morph_to"] = json!(proj_name(v.lens.to));
        m["morph"] = num(v.lens.morph);
    }
    m
}

/// The view's direction through point `(x, y)`, or `None` past θ = π,
/// where the shader draws black: what the ideal samples.
fn ideal_dir(v: &ViewParams, size: (u32, u32)) -> impl Fn(f64, f64) -> Option<DVec3> + Sync + '_ {
    move |x, y| {
        let theta = v.lens.theta(pixel::to_lens(size, x, y).length());
        (theta <= PI + 1e-4).then(|| reference::sphere_dir(v, size, x, y))
    }
}

/// The pixels of a `size` view of `f` that every view in `views`
/// minifies by [`IDEAL_MIN_LOD`] levels or more, by the CPU's analytic
/// LOD of luma before clamping, and shows whole: a pixel with a corner
/// past θ = π is black in the render and partly black in the ideal, and
/// would only dilute the comparison.
fn minified(f: &Frame, views: &[ViewParams], size: (u32, u32)) -> Vec<bool> {
    let mut mask = vec![true; size.0 as usize * size.1 as usize];
    for v in views {
        let lod = reference::lod_map(f, v, size, &Placement::default()).unwrap();
        let dir = ideal_dir(v, size);
        for (k, (m, l)) in mask.iter_mut().zip(lod.data.chunks(3)).enumerate() {
            let (i, j) = ((k % size.0 as usize) as f64, (k / size.0 as usize) as f64);
            let whole = [(i, j), (i + 1.0, j), (i, j + 1.0), (i + 1.0, j + 1.0)]
                .iter()
                .all(|&(x, y)| dir(x, y).is_some());
            *m &= whole && l[2] >= IDEAL_MIN_LOD;
        }
    }
    mask
}

/// [`ideal_dir`] on the pixels in `mask` only: the rest come out black,
/// and are not compared.
fn ideal_dir_in<'a>(
    v: &'a ViewParams,
    size: (u32, u32),
    mask: &'a [bool],
) -> impl Fn(f64, f64) -> Option<DVec3> + Sync + 'a {
    let dir = ideal_dir(v, size);
    move |x, y| {
        let (i, j) = ((x as u32).min(size.0 - 1), (y as u32).min(size.1 - 1));
        if mask[j as usize * size.0 as usize + i as usize] {
            dir(x, y)
        } else {
            None
        }
    }
}

/// The orientation that puts `world` (a pole) at point `at` of a `size`
/// view through `lens`, with the view's up as near the world's as the
/// pitch allows: `base` turns −Z to `world`, then a small turn takes
/// the ray through `at` to −Z.
fn put_at(base: DQuat, lens: &Lens, size: (u32, u32), at: (f64, f64)) -> DQuat {
    let ray = lens.unproject(pixel::to_lens(size, at.0, at.1));
    base * DQuat::from_rotation_arc(ray.normalize(), DVec3::NEG_Z)
}

// ---------------------------------------------------------------------
// Pictures and metrics

/// RGB in [0, 1].
fn unit(img: &Image) -> Vec<f64> {
    img.unit_rgb()
}

fn rgb_unit(img: &RgbImage) -> Vec<f64> {
    img.pixels.iter().flatten().copied().collect()
}

fn psnr_of(size: (u32, u32), a: &[f64], b: &[f64]) -> f64 {
    let (w, h) = (size.0 as usize, size.1 as usize);
    psnr(View::new(w, h, 3, a), View::new(w, h, 3, b)).unwrap()
}

fn ssim_of(size: (u32, u32), a: &[f64], b: &[f64]) -> f64 {
    let (w, h) = (size.0 as usize, size.1 as usize);
    ssim(View::new(w, h, 3, a), View::new(w, h, 3, b)).unwrap()
}

fn bad(size: (u32, u32), a: &[f64], b: &[f64], threshold: f64) -> DiffStats {
    let (w, h) = (size.0 as usize, size.1 as usize);
    diff_stats(View::new(w, h, 3, a), View::new(w, h, 3, b), threshold).unwrap()
}

/// The RGB of the pixels in `mask`, in order.
fn pick(mask: &[bool], rgb: &[f64]) -> Vec<f64> {
    mask.iter()
        .zip(rgb.chunks(3))
        .filter(|(m, _)| **m)
        .flat_map(|(_, c)| c.iter().copied())
        .collect()
}

/// PSNR over the pixels in `mask`.
fn psnr_in(mask: &[bool], a: &[f64], b: &[f64]) -> f64 {
    let (a, b) = (pick(mask, a), pick(mask, b));
    psnr(
        View::new(a.len() / 3, 1, 3, &a),
        View::new(b.len() / 3, 1, 3, &b),
    )
    .unwrap()
}

/// The RMS of `a − b` over every channel.
fn rms(a: &[f64], b: &[f64]) -> f64 {
    (a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>() / a.len().max(1) as f64).sqrt()
}

fn max_diff(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f64::max)
}

/// A JSON number, to 4 decimals; ±∞ as a string.
fn num(v: f64) -> Value {
    if v.is_finite() {
        json!((v * 1e4).round() / 1e4)
    } else if v > 0.0 {
        json!("inf")
    } else {
        json!("-inf")
    }
}

/// How a render compares with its stored golden.
#[derive(Debug, Clone, PartialEq)]
struct Drift {
    identical: bool,
    psnr: f64,
    ssim: f64,
    /// Pixels with a channel more than [`OTHER_PAST_LSB`] off.
    past: f64,
    max_lsb: f64,
}

fn drift(got: &Image, want: &Image) -> Result<Drift, String> {
    if (got.width, got.height) != (want.width, want.height) {
        return Err(format!(
            "{}x{} against a {}x{} golden",
            got.width, got.height, want.width, want.height
        ));
    }
    let (a, b) = (unit(got), unit(want));
    let size = (got.width, got.height);
    // Each pixel's largest channel difference, in whole 8-bit steps when
    // both are 8-bit, as the rule counts them.
    let lsb: Vec<f64> = match (&got.pixels, &want.pixels) {
        (Pixels::Rgb8(x), Pixels::Rgb8(y)) => x
            .chunks(3)
            .zip(y.chunks(3))
            .map(|(p, q)| f64::from((0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0)))
            .collect(),
        _ => a
            .chunks(3)
            .zip(b.chunks(3))
            .map(|(p, q)| max_diff(p, q) * 255.0)
            .collect(),
    };
    Ok(Drift {
        identical: got.pixels == want.pixels,
        psnr: psnr_of(size, &a, &b),
        ssim: ssim_of(size, &a, &b),
        past: lsb.iter().filter(|d| **d > OTHER_PAST_LSB).count() as f64,
        max_lsb: lsb.iter().copied().fold(0.0, f64::max),
    })
}

/// Section 6's rule for a stored image: identical at the driver build it
/// was blessed on, else within the other-driver tolerance.
fn judge(d: &Drift, same_driver: bool) -> Result<(), String> {
    let numbers = format!(
        "PSNR {:.2} dB, SSIM {:.5}, {:.0} {} more than {OTHER_PAST_LSB} LSB off, max {:.0} LSB",
        d.psnr,
        d.ssim,
        d.past,
        if d.past == 1.0 { "pixel" } else { "pixels" },
        d.max_lsb
    );
    if same_driver {
        return if d.identical {
            Ok(())
        } else {
            Err(format!(
                "not identical to the golden at the driver build it was blessed on: {numbers}"
            ))
        };
    }
    if d.psnr >= OTHER_PSNR_DB
        && d.ssim >= OTHER_SSIM
        && d.past <= OTHER_PIXELS_PAST
        && d.max_lsb <= OTHER_MAX_LSB
    {
        Ok(())
    } else {
        Err(format!(
            "outside the other-driver tolerance (PSNR ≥ {OTHER_PSNR_DB} dB, SSIM ≥ {OTHER_SSIM}, \
             at most {OTHER_PIXELS_PAST} pixels more than {OTHER_PAST_LSB} LSB off, none more \
             than {OTHER_MAX_LSB}): {numbers}"
        ))
    }
}

/// `target/golden-diffs/`, under `CARGO_TARGET_DIR` when it is set.
fn diff_dir() -> PathBuf {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    tmp.parent().unwrap_or(tmp).join("golden-diffs")
}

/// An 8-bit picture of `rgb` (unit scale).
fn rgb8(size: (u32, u32), rgb: &[f64]) -> Image {
    Image {
        width: size.0,
        height: size.1,
        pixels: Pixels::Rgb8(
            rgb.iter()
                .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect(),
        ),
    }
}

// ---------------------------------------------------------------------
// The adapter

/// The driver build a run renders on.
#[derive(Debug, Clone)]
struct Driver {
    adapter: String,
    driver: String,
    driver_info: String,
}

impl Driver {
    fn of(gpu: &Gpu) -> Driver {
        let i = &gpu.caps.info;
        Driver {
            adapter: i.name.clone(),
            driver: i.driver.clone(),
            driver_info: i.driver_info.clone(),
        }
    }

    /// LLVM's version, as llvmpipe names itself ("llvmpipe (LLVM 20.1.2,
    /// 256 bits)") or Mesa's driver info has it.
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

/// The wgpu version in the workspace's `Cargo.lock`.
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

/// HEAD's commit, which the goldens are blessed at (the working tree may
/// hold changes on top of it).
fn head_commit() -> String {
    std::process::Command::new("git")
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

// ---------------------------------------------------------------------
// A run

/// How a run treats the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Tier 1, on lavapipe: everything. Stored images by the exact rule
    /// at the build they were blessed on, by the other-driver rule
    /// elsewhere; `bless` rewrites them if everything passes.
    Golden { bless: bool },
    /// Tier 2, on a GPU: the CPU and ideal references and the controls,
    /// never the PNGs, which are lavapipe's.
    References,
    /// Tier 2, on the machine's llvmpipe: everything, the stored images
    /// by the other-driver rule whatever the build, with their drift
    /// noted for the report.
    OtherDriver,
}

struct Run<'g> {
    gpu: &'g Gpu,
    mode: Mode,
    driver: Driver,
    /// The manifest as read, or why there is none.
    manifest: Result<Value, String>,
    renderer: StillRenderer,
    row: String,
    started: Instant,
    thresholds: Map<String, Value>,
    measured: Map<String, Value>,
    failures: Vec<String>,
    images: Map<String, Value>,
    checks: Map<String, Value>,
    pngs: BTreeMap<String, Vec<u8>>,
    notes: Vec<String>,
    /// A blessing's floors, old and new, and whether it may lower one by
    /// more than [`FLOOR_DROP_DB`].
    floors: Vec<String>,
    lower_floors: bool,
    /// Whether a blessing says what it wrote on the terminal, past the
    /// test harness's capture: `goldens` does, the harness's own tests
    /// don't.
    tell: bool,
    /// Where the goldens are read and blessed, and where failures' diffs
    /// go: [`GOLDENS`] and [`diff_dir`], or scratch directories for the
    /// harness's own tests.
    dir: PathBuf,
    diffs: PathBuf,
}

impl<'g> Run<'g> {
    fn new(gpu: &'g Gpu, mode: Mode) -> Run<'g> {
        Run::at(gpu, mode, Path::new(GOLDENS), &diff_dir())
    }

    fn at(gpu: &'g Gpu, mode: Mode, dir: &Path, diffs: &Path) -> Run<'g> {
        let path = dir.join(MANIFEST);
        let manifest = std::fs::read_to_string(&path)
            .map_err(|e| format!("no manifest at {} ({e})", path.display()))
            .and_then(|t| {
                serde_json::from_str(&t)
                    .map_err(|e| format!("{} doesn't parse: {e}", path.display()))
            });
        Run {
            gpu,
            mode,
            driver: Driver::of(gpu),
            manifest,
            renderer: StillRenderer::new(gpu),
            row: String::new(),
            started: Instant::now(),
            thresholds: Map::new(),
            measured: Map::new(),
            failures: Vec::new(),
            images: Map::new(),
            checks: Map::new(),
            pngs: BTreeMap::new(),
            notes: Vec::new(),
            floors: Vec::new(),
            lower_floors: false,
            tell: false,
            dir: dir.to_path_buf(),
            diffs: diffs.to_path_buf(),
        }
    }

    fn blessing(&self) -> bool {
        self.mode == Mode::Golden { bless: true }
    }

    fn begin(&mut self, row: &str) {
        self.row = row.to_string();
        self.started = Instant::now();
        self.thresholds.clear();
        self.measured.clear();
    }

    /// Records a failure of the current row, with the driver build.
    fn fail(&mut self, msg: impl std::fmt::Display) {
        let line = format!("{}: {msg} [on {}]", self.row, self.driver.describe());
        eprintln!("FAIL {line}");
        self.failures.push(line);
    }

    fn measure(&mut self, key: &str, v: f64) {
        eprintln!("{}: {key} = {v:.4}", self.row);
        self.measured.insert(key.into(), num(v));
    }

    fn threshold(&mut self, key: &str, v: Value) {
        self.thresholds.insert(key.into(), v);
    }

    fn at_least(&mut self, key: &str, v: f64, min: f64) -> bool {
        self.measure(key, v);
        self.threshold(&format!("{key}_min"), num(min));
        let ok = v >= min;
        if !ok {
            self.fail(format!("{key} is {v:.4}, below {min}"));
        }
        ok
    }

    /// `v` strictly above `bound`: the plan's "over 100" and "over 10
    /// LSB" bounds on controls.
    fn over(&mut self, key: &str, v: f64, bound: f64) -> bool {
        self.measure(key, v);
        self.threshold(&format!("{key}_over"), num(bound));
        let ok = v > bound;
        if !ok {
            self.fail(format!("{key} is {v:.4}, not over {bound}"));
        }
        ok
    }

    fn at_most(&mut self, key: &str, v: f64, max: f64) -> bool {
        self.measure(key, v);
        self.threshold(&format!("{key}_max"), num(max));
        let ok = v <= max;
        if !ok {
            self.fail(format!("{key} is {v:.4}, above {max}"));
        }
        ok
    }

    /// A blessed threshold of the current row, from the manifest.
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

    /// A floor calibrated when blessing, as `measured − margin` (to 0.01
    /// dB) and never below `floor`, and read from the manifest otherwise.
    /// A blessing that would lower a blessed floor by more than
    /// [`FLOOR_DROP_DB`] fails, unless it was told it may.
    fn calibrated(&mut self, key: &str, v: f64, margin: f64, floor: f64) -> bool {
        let tkey = format!("{key}_min");
        let min = if self.blessing() {
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
                     than {FLOOR_DROP_DB} dB: a render that got worse, or another driver build; \
                     to accept it, bless with {UPDATE}={LOWER}"
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

    /// Renders, or records why it couldn't.
    fn render(
        &mut self,
        f: &Frame,
        v: &ViewParams,
        size: (u32, u32),
        opts: StillOptions,
    ) -> Option<Image> {
        match self.renderer.render(self.gpu, f, v, size, &opts) {
            Ok(img) => Some(img),
            Err(e) => {
                self.fail(format!("render_still: {e} ({})", e.code()));
                None
            }
        }
    }

    fn render_with(
        &mut self,
        f: &Frame,
        v: &ViewParams,
        size: (u32, u32),
        debug: DebugMode,
    ) -> Option<Image> {
        self.render(
            f,
            v,
            size,
            StillOptions {
                debug,
                ..StillOptions::default()
            },
        )
    }

    /// On a GPU, where a CPU check's error sits: per band of
    /// [`LOD_BANDS`], its pixels, the PSNR over them and its share of the
    /// squared error, as measurements for the hw-test report.
    fn by_lod(&mut self, key: &str, bands: &[Option<usize>], got: &[f64], cpu: &[f64]) {
        for (b, (band, _)) in band_errors(bands, got, cpu).iter().zip(LOD_BANDS) {
            self.measure(&format!("{key}_{band}_pixels"), b.pixels);
            if let Some(p) = b.psnr {
                self.measure(&format!("{key}_{band}_psnr_db"), p);
            }
            self.measure(&format!("{key}_{band}_error_share"), b.share);
        }
    }

    /// `got` against the CPU's `cpu`, which renders `f` through `v` with
    /// `placement`: the calibrated PSNR floor (on a GPU with the error by
    /// LOD band), and no channel off by more than [`CPU_MAX_CHANNEL`].
    fn cpu_check(
        &mut self,
        key: &str,
        got: &Image,
        cpu: &ImageF64,
        (f, v, placement): (&Frame, &ViewParams, &Placement),
    ) {
        let size = (got.width, got.height);
        let g = unit(got);
        let p = psnr_of(size, &g, &cpu.data);
        if self.mode == Mode::References {
            let bands = lod_bands(f, v, size, placement);
            self.by_lod(key, &bands, &g, &cpu.data);
        }
        let ok = self.calibrated(&format!("{key}_psnr_db"), p, CPU_MARGIN_DB, CPU_FLOOR_DB);
        let m = max_diff(&g, &cpu.data);
        let ok = self.at_most(&format!("{key}_max_channel"), m, CPU_MAX_CHANNEL) && ok;
        if !ok {
            self.write_diffs(key, size, &g, &cpu.data);
        }
    }

    /// Writes `<row>-<what>-{got,want,diff}.png` for a failure.
    fn write_diffs(&mut self, what: &str, size: (u32, u32), got: &[f64], want: &[f64]) {
        let dir = self.diffs.clone();
        let name = format!("{}-{what}", self.row);
        let diff: Vec<f64> = got
            .iter()
            .zip(want)
            .map(|(a, b)| ((a - b).abs() * 16.0).min(1.0))
            .collect();
        let written = std::fs::create_dir_all(&dir).and_then(|()| {
            for (suffix, data) in [("got", got), ("want", want), ("diff", &diff[..])] {
                let png = rgb8(size, data).to_png().map_err(std::io::Error::other)?;
                std::fs::write(dir.join(format!("{name}-{suffix}.png")), png)?;
            }
            Ok(())
        });
        match written {
            Ok(()) => eprintln!("{}: wrote {name}-*.png to {}", self.row, dir.display()),
            Err(e) => eprintln!(
                "{}: couldn't write diffs to {}: {e}",
                self.row,
                dir.display()
            ),
        }
    }

    /// The fields every manifest entry has.
    fn stamp(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("adapter".into(), json!(self.driver.adapter));
        m.insert("driver".into(), json!(self.driver.driver));
        m.insert("driver_info".into(), json!(self.driver.driver_info));
        m.insert("llvm".into(), json!(self.driver.llvm()));
        m.insert("wgpu".into(), json!(wgpu_version()));
        m.insert("commit".into(), json!(head_commit()));
        m.insert("date".into(), json!(today()));
        m.insert("regenerate".into(), json!(REGENERATE));
        m.insert("thresholds".into(), Value::Object(self.thresholds.clone()));
        m.insert("measured".into(), Value::Object(self.measured.clone()));
        m
    }

    /// Ends a row that stores no image, with every source it renders and
    /// every view it renders them through: one of each is recorded as
    /// itself, more as a list, in the row's order.
    fn end_check(&mut self, what: &str, sources: &[&Source], views: Vec<Value>) {
        let one_or_list = |mut v: Vec<Value>| {
            if v.len() == 1 {
                v.remove(0)
            } else {
                Value::Array(v)
            }
        };
        let mut m = self.stamp();
        m.insert("what".into(), json!(what));
        if !sources.is_empty() {
            let about = sources.iter().map(|s| s.about.clone()).collect();
            let hashes = sources
                .iter()
                .map(|s| json!(format!("{:016x}", s.hash())))
                .collect();
            m.insert("source".into(), one_or_list(about));
            m.insert("source_fnv64".into(), one_or_list(hashes));
        }
        if !views.is_empty() {
            m.insert("view".into(), one_or_list(views));
        }
        self.end(m, false);
    }

    /// Ends the row with its manifest entry, which must say what it
    /// rendered, from what and how (D32).
    fn end(&mut self, entry: Map<String, Value>, image: bool) {
        for field in ["source", "source_fnv64", "view"] {
            if !entry.contains_key(field) {
                self.fail(format!("its manifest entry has no {field}"));
            }
        }
        if matches!(self.mode, Mode::References | Mode::OtherDriver) {
            let numbers: Vec<String> = self
                .measured
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect();
            self.notes.push(format!(
                "golden {} on {}: {}",
                self.row,
                self.driver.describe(),
                numbers.join(", ")
            ));
        }
        let into = if image {
            &mut self.images
        } else {
            &mut self.checks
        };
        into.insert(self.row.clone(), Value::Object(entry));
        eprintln!(
            "{}: {:.2} s",
            self.row,
            self.started.elapsed().as_secs_f64()
        );
    }

    /// The row's image against its stored golden, by the rules of section
    /// 6, or kept for blessing; then the row ends.
    fn stored(&mut self, what: &str, got: &Image, source: &Source, view: Value) {
        let png = format!("{}.png", self.row);
        let hash = format!("{:016x}", source.hash());
        let mut entry = Map::new();
        match self.mode {
            Mode::References => {}
            Mode::Golden { bless: true } => match got.to_png() {
                Ok(bytes) => {
                    let old = std::fs::read(self.dir.join(&png)).ok();
                    let same = old
                        .and_then(|b| Image::from_png(&b).ok())
                        .is_some_and(|o| o.pixels == got.pixels);
                    eprintln!(
                        "{}: blessing {png} ({})",
                        self.row,
                        if same { "unchanged" } else { "new or changed" }
                    );
                    entry.insert(
                        "png_fnv64".into(),
                        json!(format!("{:016x}", spherecut_testkit::fnv64(&bytes))),
                    );
                    self.pngs.insert(png.clone(), bytes);
                }
                Err(e) => self.fail(format!("encoding {png}: {e}")),
            },
            Mode::Golden { bless: false } | Mode::OtherDriver => {
                self.compare_stored(&png, &hash, got)
            }
        }
        self.threshold("same_driver", json!("identical pixels"));
        self.threshold(
            "other_driver",
            json!({
                "psnr_db_min": OTHER_PSNR_DB,
                "ssim_min": OTHER_SSIM,
                "pixels_past_4_lsb_max": OTHER_PIXELS_PAST,
                "max_lsb": OTHER_MAX_LSB,
            }),
        );
        let mut m = self.stamp();
        m.append(&mut entry);
        m.insert("what".into(), json!(what));
        m.insert("png".into(), json!(png));
        m.insert("source".into(), source.about.clone());
        m.insert("source_fnv64".into(), json!(hash));
        m.insert("view".into(), view);
        self.end(m, true);
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
                "the synthetic source changed (its planes hash {hash}, blessed as {blessed_hash}): \
                 synth or libm moved, not the renderer, so the render isn't compared; check synth \
                 and bless again with `{REGENERATE}`"
            ));
            return;
        }
        let want = match std::fs::read(self.dir.join(png))
            .map_err(|e| e.to_string())
            .and_then(|b| Image::from_png(&b).map_err(|e| e.to_string()))
        {
            Ok(w) => w,
            Err(e) => {
                self.fail(format!("reading {png}: {e}"));
                return;
            }
        };
        let same_driver = self.mode != Mode::OtherDriver
            && entry["adapter"].as_str() == Some(&self.driver.adapter)
            && entry["driver_info"].as_str() == Some(&self.driver.driver_info);
        match drift(got, &want) {
            Err(e) => self.fail(format!("{png}: {e}")),
            Ok(d) => {
                self.measure("golden_psnr_db", d.psnr);
                self.measure("golden_ssim", d.ssim);
                self.measure("golden_pixels_past_4_lsb", d.past);
                self.measure("golden_max_lsb", d.max_lsb);
                self.measure("golden_identical", f64::from(u8::from(d.identical)));
                if let Err(e) = judge(&d, same_driver) {
                    self.fail(format!(
                        "{png}, blessed on {} {}: {e}",
                        entry["adapter"].as_str().unwrap_or("?"),
                        entry["driver_info"].as_str().unwrap_or("?")
                    ));
                    let size = (got.width, got.height);
                    self.write_diffs("golden", size, &unit(got), &unit(&want));
                }
            }
        }
    }

    /// Ends the run: blessing writes everything or nothing; any failure
    /// fails the test, listed; Tier 2 runs leave their notes for the
    /// report.
    fn finish(self) {
        for n in &self.notes {
            bench::note(n);
        }
        let list = self.failures.join("\n  ");
        if self.blessing() {
            assert!(
                self.failures.is_empty(),
                "refusing to bless: {} checks failed, and nothing was written:\n  {list}",
                self.failures.len()
            );
            self.write_goldens();
            return;
        }
        assert!(
            self.failures.is_empty(),
            "{} golden checks failed on {} (diffs in {}):\n  {list}",
            self.failures.len(),
            self.driver.describe(),
            self.diffs.display()
        );
    }

    fn write_goldens(&self) {
        let dir = &self.dir;
        std::fs::create_dir_all(dir).unwrap();
        // Stale images go, so the directory holds what the manifest says.
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
            "about": "The render goldens (M1 plan section 6, DECISIONS D32): images rendered on \
                      lavapipe from synth's in-memory planes, with each check's thresholds and \
                      what it measured when blessed.",
            "regenerate": REGENERATE,
            "images": self.images,
            "checks": self.checks,
        });
        let text = serde_json::to_string_pretty(&manifest).unwrap() + "\n";
        std::fs::write(dir.join(MANIFEST), text).unwrap();
        let summary = format!(
            "blessed {} images and {} checks into {}, with these floors (old → new):\n  {}\n",
            self.pngs.len(),
            self.checks.len(),
            dir.display(),
            self.floors.join("\n  ")
        );
        if self.tell {
            // Straight to the terminal: a passing test's eprintln! is
            // captured, and whoever blesses should see the floors move.
            use std::io::Write;
            let _ = std::io::stderr().lock().write_all(summary.as_bytes());
        } else {
            eprint!("{summary}");
        }
    }
}

// ---------------------------------------------------------------------
// The rows (M1 plan section 6's table, in its order)

type Row = fn(&mut Run<'_>);

const ROWS: &[(&str, Row)] = &[
    ("rect_forward", rect_forward),
    ("rect_ypr", rect_ypr),
    ("fisheye_180", fisheye_180),
    ("fisheye_360", fisheye_360),
    ("stereo_tinyplanet", stereo_tinyplanet),
    ("morph_half", morph_half),
    ("seam_quad", seam_quad),
    ("zenith_quad", zenith_quad),
    ("nadir_quad", nadir_quad),
    ("tinyplanet_pole", tinyplanet_pole),
    ("shimmer_tinyplanet", shimmer_tinyplanet),
    ("shimmer_fisheye360", shimmer_fisheye360),
    ("tenbit_ramp", tenbit_ramp),
    ("layout_invariance", layout_invariance),
    ("passthrough_exact", passthrough_exact),
    ("range_full_vs_limited", range_full_vs_limited),
    ("matrix_601_709", matrix_601_709),
    ("siting_left_center", siting_left_center),
    ("crop_half", crop_half),
    ("lod_heatmap", lod_heatmap),
];

fn run_rows(run: &mut Run<'_>, only: Option<&[&str]>) {
    for (name, row) in ROWS {
        if only.is_some_and(|o| !o.contains(name)) {
            continue;
        }
        run.begin(name);
        row(run);
    }
}

const RECT: (u32, u32) = (256, 144);
const SQUARE: (u32, u32) = (192, 192);
const PLANET: (u32, u32) = (256, 256);

/// A stored view of the grid, checked against the CPU and then by `more`,
/// the row's own checks, before the image is stored, so that the manifest
/// and a Tier 2 report have every number the row measured: the first rows.
fn stored_grid_view(
    run: &mut Run<'_>,
    what: &str,
    v: ViewParams,
    size: (u32, u32),
    more: impl FnOnce(&mut Run<'_>, &Source, &Image),
) {
    let s = grid_nv12();
    let Some(got) = run.render_with(&s.frame, &v, size, DebugMode::Off) else {
        return;
    };
    match reference::render_trilinear(&s.frame, &v, size) {
        Ok(cpu) => run.cpu_check("cpu", &got, &cpu, (&s.frame, &v, &Placement::default())),
        Err(e) => run.fail(format!("the CPU reference: {e}")),
    }
    more(run, &s, &got);
    run.stored(what, &got, &s, view_about(&v, size));
}

fn rect_forward_view() -> ViewParams {
    ViewParams::new(DQuat::IDENTITY, lens(Projection::Rectilinear, 90.0))
}

fn rect_forward(run: &mut Run<'_>) {
    stored_grid_view(
        run,
        "the grid, rectilinear 90°, looking forward",
        rect_forward_view(),
        RECT,
        |_, _, _| {},
    );
}

/// The camera's right, up and forward for a yaw, pitch and roll in
/// degrees, built from the convention's words rather than from
/// `sphere::orient`: yaw > 0 turns right about the world's up; pitch > 0
/// looks up, about the camera's right; roll > 0 turns the camera
/// clockwise as seen from behind, its up towards its right. World +X is
/// right, +Y up, −Z forward.
fn camera_axes(yaw: f64, pitch: f64, roll: f64) -> (DVec3, DVec3, DVec3) {
    let (y, p, r) = (yaw.to_radians(), pitch.to_radians(), roll.to_radians());
    let forward = DVec3::new(y.sin(), 0.0, -y.cos());
    let right = DVec3::new(y.cos(), 0.0, y.sin());
    let up = DVec3::Y;
    let (forward, up) = (
        forward * p.cos() + up * p.sin(),
        up * p.cos() - forward * p.sin(),
    );
    let (right, up) = (
        right * r.cos() - up * r.sin(),
        up * r.cos() + right * r.sin(),
    );
    (right, up, forward)
}

/// Where a rectilinear camera with `axes` and half-angle `half` shows
/// direction `d`, in pixels of a `size` view: the lens point is the
/// gnomonic projection over tan(half), x right and y up, spread over the
/// width (`p.x` from −1 to 1, `p.y` by the same scale).
fn rect_pixel(
    axes: (DVec3, DVec3, DVec3),
    half: f64,
    size: (u32, u32),
    d: DVec3,
) -> Option<(f64, f64)> {
    let (right, up, forward) = axes;
    let z = d.dot(forward);
    if z <= 0.0 {
        return None;
    }
    let (px, py) = (d.dot(right) / z / half.tan(), d.dot(up) / z / half.tan());
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let (x, y) = ((px + 1.0) * w / 2.0, (h - py * w) / 2.0);
    (x >= 2.0 && y >= 2.0 && x <= w - 2.0 && y <= h - 2.0).then_some((x, y))
}

/// `rect_ypr`'s yaw, pitch and roll, in degrees.
const RECT_YPR: (f64, f64, f64) = (30.0, 20.0, 10.0);

fn rect_ypr_view() -> ViewParams {
    let (yaw, pitch, roll) = RECT_YPR;
    ViewParams::new(ypr(yaw, pitch, roll), lens(Projection::Rectilinear, 90.0))
}

fn rect_ypr(run: &mut Run<'_>) {
    stored_grid_view(
        run,
        "the grid, rectilinear 90°, yaw 30, pitch 20, roll 10",
        rect_ypr_view(),
        RECT,
        |run, _, got| ypr_markers(run, got, RECT_YPR),
    );
}

/// The markers of `rect_ypr`, where the convention's words put them in
/// `got`, a 90° view at `angles` (yaw, pitch and roll in degrees).
fn ypr_markers(run: &mut Run<'_>, got: &Image, angles: (f64, f64, f64)) {
    let (yaw, pitch, roll) = angles;
    let half = 45f64.to_radians();
    let at = |deg: (f64, f64)| {
        LonLat {
            lon: deg.0.to_radians(),
            lat: deg.1.to_radians(),
        }
        .to_dir()
    };
    // The front marker's centre is red; the prime meridian at 40° up and
    // the equator at 50° east are white lines, 1.2° wide.
    let red = |c: [f64; 3]| c[0] >= 0.8 && c[1] <= 0.2 && c[2] <= 0.2;
    let white = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2] >= 0.9;
    type Mark<'a> = (&'a str, DVec3, &'a dyn Fn([f64; 3]) -> bool);
    let marks: [Mark; 3] = [
        ("the red F marker's centre", DVec3::NEG_Z, &red),
        ("the prime meridian at 40° N", at((0.0, 40.0)), &white),
        ("the equator at 50° E", at((50.0, 0.0)), &white),
    ];
    let axes = camera_axes(yaw, pitch, roll);
    let mut hits = 0.0;
    for (what, d, ok) in &marks {
        let Some((x, y)) = rect_pixel(axes, half, RECT, *d) else {
            run.fail(format!("{what} should be in view"));
            continue;
        };
        let c = got.unit_at(x as u32, y as u32).unwrap();
        if ok(c) {
            hits += 1.0;
        } else {
            run.fail(format!("{what}, at pixel ({x:.1}, {y:.1}), is {c:?}"));
        }
    }
    run.at_least("markers_found", hits, 3.0);
    // Control: with any one angle's sign turned, the red marker's centre
    // lands off its disc, so the check can tell the convention's signs.
    let mut misplaced = 0.0;
    for (name, flipped) in [
        ("yaw", (-yaw, pitch, roll)),
        ("pitch", (yaw, -pitch, roll)),
        ("roll", (yaw, pitch, -roll)),
    ] {
        let p = rect_pixel(
            camera_axes(flipped.0, flipped.1, flipped.2),
            half,
            RECT,
            DVec3::NEG_Z,
        );
        match p.map(|(x, y)| got.unit_at(x as u32, y as u32).unwrap()) {
            Some(c) if red(c) => {
                run.fail(format!("with the {name} turned, F's centre is still red"))
            }
            _ => misplaced += 1.0,
        }
    }
    run.at_least("control_signs_misplaced", misplaced, 3.0);
}

/// Pixels whose centre is past θ = π (by more than `margin`) must be
/// black, which the stored image and the CPU can't say on their own.
fn black_past_pi(run: &mut Run<'_>, got: &Image, v: &ViewParams) {
    let size = (got.width, got.height);
    let (mut past, mut lit) = (0.0, 0.0);
    for j in 0..size.1 {
        for i in 0..size.0 {
            let c = pixel::centre(i, j);
            let theta = v.lens.theta(pixel::to_lens(size, c.x, c.y).length());
            if theta > PI + 1e-3 {
                past += 1.0;
                if got.unit_at(i, j).unwrap() != [0.0; 3] {
                    lit += 1.0;
                }
            }
        }
    }
    run.measure("pixels_past_pi", past);
    run.at_most("pixels_past_pi_not_black", lit, 0.0);
}

fn fisheye_180_view() -> ViewParams {
    // Looking back, so the seam runs through the middle.
    ViewParams::new(ypr(180.0, 0.0, 0.0), lens(Projection::Fisheye, 180.0))
}

fn fisheye_180(run: &mut Run<'_>) {
    let v = fisheye_180_view();
    stored_grid_view(
        run,
        "the grid, fisheye 180°, looking back across the seam",
        v,
        SQUARE,
        |run, _, got| black_past_pi(run, got, &v),
    );
}

fn fisheye_360_view() -> ViewParams {
    ViewParams::new(ypr(90.0, 30.0, 0.0), lens(Projection::Fisheye, 360.0))
}

fn fisheye_360(run: &mut Run<'_>) {
    let v = fisheye_360_view();
    stored_grid_view(
        run,
        "the grid, fisheye 360°, yaw 90, pitch 30",
        v,
        SQUARE,
        |run, _, got| {
            black_past_pi(run, got, &v);
            let past = run.measured["pixels_past_pi"].as_f64().unwrap_or(0.0);
            run.at_least("pixels_past_pi_checked", past, 1.0);
        },
    );
}

/// `got` and its level-0 control against the ideal: a calibrated floor,
/// and the margin over the control.
fn ideal_check(run: &mut Run<'_>, mask: &[bool], got: &Image, level0: &Image, ideal: &RgbImage) {
    let size = (got.width, got.height);
    let i = rgb_unit(ideal);
    run.threshold("ideal_min_lod", num(IDEAL_MIN_LOD));
    run.at_least(
        "ideal_pixels",
        mask.iter().filter(|m| **m).count() as f64,
        IDEAL_MIN_PIXELS,
    );
    let p = psnr_in(mask, &unit(got), &i);
    let c = psnr_in(mask, &unit(level0), &i);
    run.calibrated("ideal_psnr_db", p, IDEAL_MARGIN_DB, 0.0);
    run.measure("control_level0_ideal_psnr_db", c);
    if !run.at_least("ideal_over_level0_db", p - c, IDEAL_OVER_LEVEL0_DB) {
        run.write_diffs("ideal", size, &unit(got), &i);
    }
}

fn stereo_tinyplanet_view() -> ViewParams {
    ViewParams::new(ypr(0.0, -90.0, 0.0), lens(Projection::Stereographic, 300.0))
}

fn stereo_tinyplanet(run: &mut Run<'_>) {
    let v = stereo_tinyplanet_view();
    stored_grid_view(
        run,
        "the grid, tiny planet (stereographic 300°, pitch −90)",
        v,
        PLANET,
        |run, s, got| {
            let Some(level0) = run.render_with(&s.frame, &v, PLANET, DebugMode::Level0) else {
                return;
            };
            let mask = minified(&s.frame, &[v], PLANET);
            let ideal = render_view_ideal(
                Pattern::Grid,
                ideal_dir_in(&v, PLANET, &mask),
                PLANET,
                IDEAL_N_GRID,
            );
            ideal_check(run, &mask, got, &level0, &ideal);
        },
    );
}

fn morph_half_view() -> ViewParams {
    let l = Lens {
        from: Projection::Rectilinear,
        to: Projection::Stereographic,
        morph: 0.5,
        hfov: 150f64.to_radians(),
    };
    ViewParams::new(ypr(0.0, -10.0, 0.0), l)
}

fn morph_half(run: &mut Run<'_>) {
    stored_grid_view(
        run,
        "the grid, rectilinear to stereographic at 0.5, 150°",
        morph_half_view(),
        RECT,
        |_, _, _| {},
    );
}

/// The roll twin of `view` on `s`: the texture rolled by half its width
/// and the world turned 180° about its up axis, so that every pixel
/// samples the same texels, but the twin's seam (u = 0) runs where `s`
/// has longitude 0. The analytic LOD must give the two alike, to 2/255
/// at every pixel; the implicit-LOD control must not.
fn twin_check(run: &mut Run<'_>, s: &Source, v: &ViewParams, size: (u32, u32)) {
    let twin = rolled(&s.frame);
    let tv = ViewParams::new(DQuat::from_rotation_y(PI) * v.orientation, v.lens);
    for (debug, key) in [
        (DebugMode::Off, "bad_pixels"),
        (DebugMode::ImplicitLod, "control_implicit_bad_pixels"),
    ] {
        let (Some(a), Some(b)) = (
            run.render_with(&s.frame, v, size, debug),
            run.render_with(&twin, &tv, size, debug),
        ) else {
            return;
        };
        let (ua, ub) = (unit(&a), unit(&b));
        let d = bad(size, &ua, &ub, TWIN_THRESHOLD);
        run.measure(&format!("{key}_max_lsb"), d.max * 255.0);
        let ok = if debug == DebugMode::Off {
            run.at_most(key, d.count_over as f64, 0.0)
        } else {
            run.over(key, d.count_over as f64, CONTROL_BAD_OVER as f64)
        };
        if !ok {
            run.write_diffs(key, size, &ua, &ub);
        }
    }
    run.threshold("bad_pixel_over", num(TWIN_THRESHOLD));
    run.end_check(
        "against its roll twin, with the implicit-LOD control",
        &[s],
        vec![view_about(v, size)],
    );
}

fn seam_quad(run: &mut Run<'_>) {
    // The seam (longitude ±180°) at x = 129, between the centres of
    // columns 128 and 129, which share a 2×2 quad: an implicit derivative
    // across that quad sees u jump by 1.
    let s = checker_gray();
    let l = lens(Projection::Rectilinear, 90.0);
    let x = 129.0;
    let phi = (pixel::to_lens(RECT, x, 0.0).x * l.half(Projection::Rectilinear).tan()).atan();
    let v = ViewParams::new(Ypr::new(PI - phi, 0.0, 0.0).to_quat(), l);
    // The CPU checks the placement first: u wraps between columns 128 and
    // 129 on every row, and nowhere else.
    let mut wraps = Vec::new();
    for j in 0..RECT.1 {
        for i in 0..RECT.0 - 1 {
            let u = |i: u32| {
                let c = pixel::centre(i, j);
                reference::sphere_uv(reference::sphere_dir(&v, RECT, c.x, c.y)).x
            };
            if (u(i) - u(i + 1)).abs() > 0.5 {
                wraps.push((i, j));
            }
        }
    }
    let placed = wraps.len() == RECT.1 as usize && wraps.iter().all(|&(i, _)| i == 128);
    assert!(
        placed,
        "the seam should cross between columns 128 and 129 on every row, not at {:?}",
        &wraps[..wraps.len().min(4)]
    );
    run.measure("yaw_deg", (PI - phi).to_degrees());
    twin_check(run, &s, &v, RECT);
}

/// A pole view: `base` turns −Z to the pole, which then lies at the
/// centre of the quad of pixels (128, 72) to (129, 73), or (128, 128) to
/// (129, 129) for a square view; asserted on the CPU.
fn pole(run: &mut Run<'_>, base: DQuat, pole: DVec3, l: Lens, size: (u32, u32)) {
    let s = checker_gray();
    let at = (129.0, f64::from(size.1 / 2 + 1));
    let v = ViewParams::new(put_at(base, &l, size, at), l);
    let d = reference::sphere_dir(&v, size, at.0, at.1);
    assert!(d.dot(pole) > 1.0 - 1e-12, "the pole isn't at {at:?}: {d:?}");
    assert!(
        at.0 % 2.0 == 1.0 && at.1 % 2.0 == 1.0,
        "{at:?} isn't a quad's centre"
    );
    twin_check(run, &s, &v, size);
}

fn zenith_quad(run: &mut Run<'_>) {
    pole(
        run,
        ypr(0.0, 90.0, 0.0),
        DVec3::Y,
        lens(Projection::Rectilinear, 120.0),
        RECT,
    );
}

fn nadir_quad(run: &mut Run<'_>) {
    pole(
        run,
        ypr(0.0, -90.0, 0.0),
        DVec3::NEG_Y,
        lens(Projection::Rectilinear, 120.0),
        RECT,
    );
}

fn tinyplanet_pole(run: &mut Run<'_>) {
    pole(
        run,
        ypr(0.0, -90.0, 0.0),
        DVec3::NEG_Y,
        lens(Projection::Stereographic, 300.0),
        PLANET,
    );
}

/// Eight frames yawing 0.37° each: the render's frame-to-frame change no
/// more than the ideal's plus 2 LSB, the level-0 control's at least three
/// times the ideal's, and the render at least 3 dB nearer the ideal than
/// the control, frame by frame.
fn shimmer(run: &mut Run<'_>, pitch: f64, l: Lens, size: (u32, u32)) {
    let s = checker_gray();
    let views: Vec<ViewParams> = (0..8)
        .map(|t| ViewParams::new(ypr(0.37 * f64::from(t), pitch, 0.0), l))
        .collect();
    // Shimmer is the flicker of detail too fine to show, so it is measured
    // where every frame minifies; elsewhere the checker is resolved, and
    // its motion is the change the frames ought to show.
    let mask = minified(&s.frame, &views, size);
    run.threshold("ideal_min_lod", num(IDEAL_MIN_LOD));
    run.at_least(
        "ideal_pixels",
        mask.iter().filter(|m| **m).count() as f64,
        IDEAL_MIN_PIXELS,
    );
    let mut frames = Vec::new();
    for v in &views {
        let (Some(g), Some(c)) = (
            run.render_with(&s.frame, v, size, DebugMode::Off),
            run.render_with(&s.frame, v, size, DebugMode::Level0),
        ) else {
            return;
        };
        let ideal = render_view_ideal(
            Pattern::HighFrequency,
            ideal_dir_in(v, size, &mask),
            size,
            IDEAL_N_CHECKER,
        );
        frames.push((
            pick(&mask, &unit(&g)),
            pick(&mask, &unit(&c)),
            pick(&mask, &rgb_unit(&ideal)),
        ));
    }
    let all = vec![true; frames[0].0.len() / 3];
    let (mut worst_excess, mut sum_g, mut sum_c, mut sum_i) = (f64::MIN, 0.0, 0.0, 0.0);
    for w in frames.windows(2) {
        let (g, c, i) = (
            rms(&w[1].0, &w[0].0),
            rms(&w[1].1, &w[0].1),
            rms(&w[1].2, &w[0].2),
        );
        worst_excess = worst_excess.max(g - i);
        sum_g += g;
        sum_c += c;
        sum_i += i;
    }
    let pairs = (frames.len() - 1) as f64;
    run.measure("rms_change_lsb", sum_g / pairs * 255.0);
    run.measure("ideal_rms_change_lsb", sum_i / pairs * 255.0);
    run.at_most("rms_change_over_ideal_lsb_worst", worst_excess * 255.0, 2.0);
    run.at_least("control_level0_rms_change_over_ideal", sum_c / sum_i, 3.0);
    let mut worst_margin = f64::MAX;
    let mut psnr_sum = 0.0;
    for (g, c, i) in &frames {
        let p = psnr_in(&all, g, i);
        psnr_sum += p;
        worst_margin = worst_margin.min(p - psnr_in(&all, c, i));
    }
    let mean = psnr_sum / frames.len() as f64;
    run.calibrated("ideal_psnr_db", mean, IDEAL_MARGIN_DB, 0.0);
    run.at_least(
        "ideal_over_level0_db_worst",
        worst_margin,
        IDEAL_OVER_LEVEL0_DB,
    );
    let v = ViewParams::new(ypr(0.0, pitch, 0.0), l);
    run.end_check(
        "8 frames yawing 0.37° each, against the ideal, with the level-0 control",
        &[&s],
        vec![view_about(&v, size)],
    );
}

fn shimmer_tinyplanet(run: &mut Run<'_>) {
    shimmer(run, -90.0, lens(Projection::Stereographic, 300.0), PLANET);
}

fn shimmer_fisheye360(run: &mut Run<'_>) {
    shimmer(run, 0.0, lens(Projection::Fisheye, 360.0), SQUARE);
}

fn tenbit_ramp(run: &mut Run<'_>) {
    // Luma rising one code per 1.17 texels across the whole panorama,
    // chroma neutral; seen magnified about 2.2 times.
    let size = (1024, 512);
    let code = |p: usize, x: u32, _y: u32| -> u16 {
        if p == 0 {
            (64 + x * 876 / 1023) as u16
        } else {
            512
        }
    };
    let p010 = Source::new("ramp", coded(Kind::P010, size, limited709(), code));
    let i010 = Source::new("ramp", coded(Kind::I420p10, size, limited709(), code));
    let v = ViewParams::new(DQuat::IDENTITY, lens(Projection::Rectilinear, 20.0));
    let opts = StillOptions {
        depth: Depth::Sixteen,
        ..StillOptions::default()
    };
    let (Some(a), Some(b)) = (
        run.render(&p010.frame, &v, RECT, opts),
        run.render(&i010.frame, &v, RECT, opts),
    ) else {
        return;
    };
    run.at_least(
        "layouts_identical",
        f64::from(u8::from(a.pixels == b.pixels)),
        1.0,
    );
    // Non-decreasing along every row, in every channel.
    let mut drops = 0.0;
    let mut rise = 0.0;
    if let Pixels::Rgb16(p) = &a.pixels {
        for j in 0..RECT.1 as usize {
            for i in 1..RECT.0 as usize {
                for c in 0..3 {
                    let (l, r) = (p[3 * (j * 256 + i - 1) + c], p[3 * (j * 256 + i) + c]);
                    if r < l {
                        drops += 1.0;
                    }
                    rise += f64::from(r.saturating_sub(l));
                }
            }
        }
    } else {
        run.fail("a 16-bit still came back at another depth");
    }
    run.at_most("decreasing_steps", drops, 0.0);
    // Codes 64 to 940 cover 0 to 1: the view's 20° see about 49 codes.
    run.at_least(
        "rise_per_row_codes",
        rise / f64::from(RECT.1) / 3.0 / 65535.0 * 876.0,
        30.0,
    );
    match reference::render_trilinear(&p010.frame, &v, RECT) {
        Ok(cpu) => {
            let m = max_diff(&unit(&a), &cpu.data);
            run.at_most("cpu_max_diff_10bit_codes", m * 1023.0, 2.0);
        }
        Err(e) => run.fail(format!("the CPU reference: {e}")),
    }
    run.end_check(
        "a 10-bit ramp as P010 and yuv420p10, magnified, read back at 16 bits",
        &[&p010, &i010],
        vec![view_about(&v, RECT)],
    );
}

fn layout_invariance(run: &mut Run<'_>) {
    // 1100 wide, so that no row is already a multiple of 64 or 256 bytes.
    static G: OnceLock<RgbImage> = OnceLock::new();
    let img = G.get_or_init(|| render_equirect(Pattern::Grid, 1100, 550, 0, 2));
    let v = ViewParams::new(ypr(-20.0, 10.0, 0.0), lens(Projection::Rectilinear, 90.0));
    let mut first: Option<(String, Image)> = None;
    let mut differ = 0.0;
    for kind in [Kind::Nv12, Kind::I420] {
        let base = frame(img, kind, limited709(), 0);
        for (name, stride) in [
            (
                "64",
                &(|r: usize| r.next_multiple_of(64)) as &dyn Fn(usize) -> usize,
            ),
            ("256", &|r: usize| r.next_multiple_of(256)),
            ("4109", &|_| 4109),
        ] {
            let f = restride(&base, stride);
            // Every plane's rows really are padded.
            assert!(
                f.layout
                    .planes
                    .iter()
                    .zip(&f.planes)
                    .all(|(d, p)| p.1 > d.row_bytes(p.2.0))
            );
            let Some(got) = run.render_with(&f, &v, RECT, DebugMode::Off) else {
                return;
            };
            let what = format!("{} with rows {name}", kind.name());
            match &first {
                None => first = Some((what, got)),
                Some((w0, g0)) => {
                    if g0.pixels != got.pixels {
                        differ += 1.0;
                        let d = max_diff(&unit(g0), &unit(&got)) * 255.0;
                        run.fail(format!("{what} differs from {w0} by up to {d:.0} LSB"));
                    }
                }
            }
        }
    }
    run.at_most("renders_not_identical", differ, 0.0);
    // The hash leaves the padding out, so each layout's three strides
    // hash alike.
    let nv12 = Source::new("grid", frame(img, Kind::Nv12, limited709(), 0));
    let i420 = Source::new("grid", frame(img, Kind::I420, limited709(), 0));
    run.end_check(
        "one picture as NV12 and I420, rows padded to 64 and 256 bytes and 4109 bytes apart",
        &[&nv12, &i420],
        vec![view_about(&v, RECT)],
    );
}

/// The nearest IEEE half to `x ≥ 0`, ties to even, from the bits of `x`:
/// the step is 2^(e − 10) for 2^e ≤ x < 2^(e+1), and 2^−24 below 2^−14.
/// Written apart from `readback::round_f16`, for an independent model.
fn nearest_half(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let e = ((x.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    let step = 2f64.powi(if e < -14 { -24 } else { e - 10 });
    (x / step).round_ties_even() * step
}

fn eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn oetf(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// What a flat field of luma `k` (8 bits, chroma neutral) comes out as,
/// by an independent model of the chain: Y' by H.273 (limited: (k − 16)
/// / 219, full: k / 255) clamped to [0, 1]; the sRGB EOTF; the nearest
/// half, as the linear target stores it; the OETF of the encode pass;
/// and the encode pass's rounding to the nearest code, whose k / 255 a
/// unorm8 store to nearest keeps, even packed to f16 toward zero first
/// (DECISIONS D31). The GPU does each step in f32, so where a value lies
/// within its error of a tie (an f16 tie, or a half code) either
/// neighbour may come out: the second value is the other candidate,
/// when there is one.
fn passthrough_model(k: u8, limited: bool) -> (u8, Option<u8>) {
    let y = if limited {
        (f64::from(k) - 16.0) / 219.0
    } else {
        f64::from(k) / 255.0
    }
    .clamp(0.0, 1.0);
    let lin = eotf(y);
    let out = |h: f64| (oetf(h) * 255.0).round() as u8;
    let h = nearest_half(lin);
    let mut other = None;
    // f32's dot product and pow: a few parts in a million at most.
    for l in [lin * (1.0 - 4e-6), lin * (1.0 + 4e-6)] {
        let alt = out(nearest_half(l));
        if alt != out(h) {
            other = Some(alt);
        }
    }
    let code = oetf(h) * 255.0;
    if (code - code.floor() - 0.5).abs() < 1e-4 {
        other = Some(if out(h) as f64 > code {
            out(h) - 1
        } else {
            out(h) + 1
        });
    }
    (out(h), other)
}

fn passthrough_exact(run: &mut Run<'_>) {
    // 256 flat fields side by side, Y = k across columns 16k to 16k + 15
    // of a 4096-wide NV12 picture, chroma neutral; seen through eight
    // rectilinear 45° views round the equator, a texel per pixel at the
    // centre and magnified towards the edges, so only level 0 and 1 are
    // sampled. A pixel is checked where every texel its taps can touch,
    // at both levels, lies in one field.
    let size = (4096u32, 16u32);
    let out = (512u32, 8u32);
    let views: Vec<ViewParams> = (0..8)
        .map(|m| {
            ViewParams::new(
                ypr(22.5 + 45.0 * f64::from(m), 0.0, 0.0),
                lens(Projection::Rectilinear, 45.0),
            )
        })
        .collect();
    let mut found: BTreeMap<(bool, u8), (u8, usize)> = BTreeMap::new();
    let mut wrong = Vec::new();
    let mut sources = Vec::new();
    for limited in [false, true] {
        let range = if limited { Range::Limited } else { Range::Full };
        let f = coded(
            Kind::Nv12,
            size,
            color(range, Matrix::Bt709, ChromaSiting::Left),
            |p, x, _| {
                if p == 0 { (x / 16) as u16 } else { 128 }
            },
        );
        for v in &views {
            let Some(got) = run.render_with(&f, v, out, DebugMode::Off) else {
                return;
            };
            for j in 0..out.1 {
                for i in 0..out.0 {
                    let c = pixel::centre(i, j);
                    let u = reference::sphere_uv(reference::sphere_dir(v, out, c.x, c.y)).x;
                    let t = (u * f64::from(size.0)).rem_euclid(f64::from(size.0));
                    let k = (t / 16.0).floor() as u8;
                    if !(4.0..=12.0).contains(&(t % 16.0)) {
                        continue;
                    }
                    let Pixels::Rgb8(p) = &got.pixels else {
                        unreachable!()
                    };
                    let px = &p[3 * (j * out.0 + i) as usize..][..3];
                    if px[0] != px[1] || px[1] != px[2] {
                        wrong.push(format!(
                            "{range:?} k = {k}: a grey field came out as {px:?}"
                        ));
                    }
                    let e = found.entry((limited, k)).or_insert((px[0], 0));
                    if e.0 != px[0] {
                        wrong.push(format!("{range:?} k = {k}: both {} and {}", e.0, px[0]));
                    }
                    e.1 += 1;
                }
            }
        }
        sources.push(Source::new("bands", f));
    }
    let (mut ambiguous, mut off_model, mut off_k, mut seen) = (0.0, 0.0, 0.0, 0.0);
    let mut flips = Vec::new();
    for limited in [false, true] {
        for k in 0..=255u8 {
            let Some(&(got, _)) = found.get(&(limited, k)) else {
                wrong.push(format!(
                    "k = {k} ({}) was never sampled",
                    if limited { "limited" } else { "full" }
                ));
                continue;
            };
            seen += 1.0;
            let (want, other) = passthrough_model(k, limited);
            if other.is_some() {
                ambiguous += 1.0;
            }
            if got != want && Some(got) != other {
                off_model += 1.0;
                wrong.push(format!(
                    "{} k = {k}: {got}, where the model of the chain gives {want}",
                    if limited { "limited" } else { "full" }
                ));
            }
            if !limited && got != k {
                off_k += 1.0;
                wrong.push(format!("full k = {k} came out {got}"));
            }
            if limited && (16..=235).contains(&k) {
                // The bound: the linear target's f16 store moves the
                // linear value by at most 2^−11 of itself, which the OETF
                // turns into at most (1.055 / 2.4) · 2^−11 of full scale,
                // 0.0547 codes. The encode pass rounds to codes itself,
                // so the unorm8 store adds nothing (left to RADV, whose
                // export packs to f16 toward zero, the OETF's value lost
                // up to 0.1245 codes more, D31). So the output is
                // round((k − 16) · 255 / 219) unless that lies within
                // 0.0547 codes of a tie, and one code off at most where
                // it does.
                let ideal = (f64::from(k) - 16.0) * 255.0 / 219.0;
                let bound = 1.055 / 2.4 * 2f64.powi(-11) * 255.0 + 1e-4;
                if f64::from(got) != ideal.round() {
                    flips.push(k);
                    let near = (ideal - ideal.floor() - 0.5).abs() < bound;
                    if !near || (f64::from(got) - ideal).abs() > 0.5 + bound {
                        wrong.push(format!(
                            "limited k = {k}: {got}, {ideal:.4} exactly, outside the f16 bound of {bound:.4}"
                        ));
                    }
                }
            }
        }
    }
    for w in wrong.iter().take(12) {
        run.fail(w);
    }
    run.measure("fields_seen", seen);
    run.at_most("off_the_model", off_model, 0.0);
    run.at_most("full_range_not_k", off_k, 0.0);
    run.measure("model_ambiguous", ambiguous);
    run.at_most("problems", wrong.len() as f64, 0.0);
    eprintln!("passthrough_exact: limited k one code from round((k − 16) · 255 / 219): {flips:?}");
    run.threshold(
        "limited",
        json!(
            "equal to the model; round((k − 16) · 255 / 219) except within 0.0547 codes of a tie"
        ),
    );
    run.measured
        .insert("limited_one_code_off".into(), json!(flips));
    run.end_check(
        "flat fields of every 8-bit luma code, full and limited range, through the f16 target",
        &sources.iter().collect::<Vec<_>>(),
        views.iter().map(|v| view_about(v, out)).collect(),
    );
}

fn range_full_vs_limited(run: &mut Run<'_>) {
    // At 10 bits, whose planes are f16 textures that the GPU filters in
    // floating point. lavapipe filters 8-bit planes in 8-bit fixed point,
    // to whole codes (D32), which on top of the two codings' own rounding
    // puts an NV12 pair up to 4 LSB apart: that is recorded, and its
    // control asserted too.
    let v = ViewParams::new(DQuat::IDENTITY, lens(Projection::Rectilinear, 90.0));
    let mut sources = Vec::new();
    for (kind, key) in [(Kind::P010, ""), (Kind::Nv12, "nv12_")] {
        let full = frame(
            grid(),
            kind,
            color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
            0,
        );
        let limited = frame(grid(), kind, limited709(), 0);
        let mut swapped = full.clone();
        swapped.color.range = Range::Limited;
        let (Some(a), Some(b), Some(c)) = (
            run.render_with(&full, &v, RECT, DebugMode::Off),
            run.render_with(&limited, &v, RECT, DebugMode::Off),
            run.render_with(&swapped, &v, RECT, DebugMode::Off),
        ) else {
            return;
        };
        let (ua, ub, uc) = (unit(&a), unit(&b), unit(&c));
        let apart = max_diff(&ua, &ub) * 255.0;
        if kind == Kind::Nv12 {
            run.measure("nv12_max_lsb", apart);
        } else if !run.at_most("max_lsb", apart, 2.0) {
            run.write_diffs("ranges", RECT, &ua, &ub);
        }
        run.over(
            &format!("{key}control_full_as_limited_max_lsb"),
            max_diff(&ua, &uc) * 255.0,
            10.0,
        );
        sources.push(Source::new("grid", full));
        sources.push(Source::new("grid", limited));
    }
    run.end_check(
        "the grid coded at full and at limited range, 10-bit (8-bit recorded); control: full decoded as limited",
        &sources.iter().collect::<Vec<_>>(),
        vec![view_about(&v, RECT)],
    );
}

fn matrix_view() -> ViewParams {
    ViewParams::new(ypr(-30.0, 15.0, 0.0), lens(Projection::Rectilinear, 90.0))
}

/// The grid as limited-range NV12 coded with matrix `m`.
fn grid_coded(m: Matrix) -> Frame {
    frame(
        grid(),
        Kind::Nv12,
        color(Range::Limited, m, ChromaSiting::Left),
        0,
    )
}

fn matrix_601_709(run: &mut Run<'_>) {
    let v = matrix_view();
    let mut psnrs = BTreeMap::new();
    for (name, m) in [("601", Matrix::Bt601), ("709", Matrix::Bt709)] {
        let f = grid_coded(m);
        let (Some(got), Ok(cpu)) = (
            run.render_with(&f, &v, RECT, DebugMode::Off),
            reference::render_trilinear(&f, &v, RECT),
        ) else {
            run.fail("render or the CPU reference failed");
            return;
        };
        let at = (&f, &v, &Placement::default());
        run.cpu_check(&format!("cpu_{name}"), &got, &cpu, at);
        psnrs.insert(name, (f, cpu, unit(&got)));
    }
    // The two codings carry the same picture, so their renders agree,
    // whatever render::color does with either. The CPU reference can't
    // say that: it builds its colour from the same `color::fold` as the
    // GPU, so a matrix swapped inside the fold passes both CPU checks.
    // The codings are synth's, from core's Kr and Kb.
    let (a, b) = (&psnrs["601"].2, &psnrs["709"].2);
    run.measure("601_vs_709_max_lsb", max_diff(a, b) * 255.0);
    if !run.at_least("601_vs_709_psnr_db", psnr_of(RECT, a, b), MATRIX_AGREE_DB) {
        run.write_diffs("agree", RECT, a, b);
    }
    // Control: the BT.601 frame decoded as BT.709 misses its own CPU
    // reference by far more than the tolerance's 3 dB.
    let (f601, cpu601, _) = &psnrs["601"];
    let mut swapped = f601.clone();
    swapped.color.matrix = Matrix::Bt709;
    if let Some(got) = run.render_with(&swapped, &v, RECT, DebugMode::Off) {
        let p = psnr_of(RECT, &unit(&got), &cpu601.data);
        run.measure("control_601_as_709_psnr_db", p);
        match run
            .thresholds
            .get("cpu_601_psnr_db_min")
            .and_then(Value::as_f64)
        {
            Some(tol) => {
                run.at_least("control_601_as_709_below_tolerance_db", tol - p, 10.0);
            }
            None => run.fail("no BT.601 tolerance to hold the control against"),
        }
    }
    let sources: Vec<Source> = ["601", "709"]
        .iter()
        .map(|m| Source::new("grid", psnrs[m].0.clone()))
        .collect();
    run.end_check(
        "the grid coded with BT.601 and with BT.709, each against the CPU; control: 601 decoded as 709",
        &sources.iter().collect::<Vec<_>>(),
        vec![view_about(&v, RECT)],
    );
}

/// R'G'B' in [0, 1] as BT.709 limited-range Y'CbCr codes at `bits`, by
/// H.273's equations, written out here rather than taken from synth or
/// the renderer.
fn ycbcr709(c: [f64; 3], bits: u32) -> [u16; 3] {
    let (kr, kb) = (0.2126, 0.0722);
    let y = kr * c[0] + (1.0 - kr - kb) * c[1] + kb * c[2];
    let cb = (c[2] - y) / (2.0 * (1.0 - kb));
    let cr = (c[0] - y) / (2.0 * (1.0 - kr));
    let k = f64::from(1u32 << (bits - 8));
    [16.0 + 219.0 * y, 128.0 + 224.0 * cb, 128.0 + 224.0 * cr].map(|v| (v * k).round() as u16)
}

fn siting_left_center(run: &mut Run<'_>) {
    // Red and blue stripes five texels wide: a sharp chroma edge every
    // 2.5 chroma texels, so the edges fall at different phases of the
    // chroma grid; seen magnified about seven times. 10-bit, as in
    // `range_full_vs_limited`: NV12's 8-bit planes come back from
    // lavapipe's filter as whole codes, up to 2 LSB off the f64 reference
    // on these edges, which is recorded, with its control.
    let (w, h) = (1024u32, 512u32);
    let img = RgbImage {
        width: w,
        height: h,
        pixels: (0..w * h)
            .map(|i| {
                if (i % w) / 5 % 2 == 0 {
                    [0.9, 0.1, 0.2]
                } else {
                    [0.1, 0.3, 0.9]
                }
            })
            .collect(),
    };
    let v = ViewParams::new(ypr(10.0, 0.0, 0.0), lens(Projection::Rectilinear, 20.0));
    let mut sources = Vec::new();
    for (kind, key) in [(Kind::P010, ""), (Kind::Nv12, "nv12_")] {
        let mut cpus = BTreeMap::new();
        let mut gpus = BTreeMap::new();
        for (name, siting) in [
            ("left", ChromaSiting::Left),
            ("center", ChromaSiting::Center),
        ] {
            let f = frame(&img, kind, color(Range::Limited, Matrix::Bt709, siting), 0);
            let (Some(got), Ok(cpu)) = (
                run.render_with(&f, &v, RECT, DebugMode::Off),
                reference::render_trilinear(&f, &v, RECT),
            ) else {
                run.fail("render or the CPU reference failed");
                return;
            };
            let g = unit(&got);
            let off = max_diff(&g, &cpu.data) * 255.0;
            let what = format!("{key}{name}_cpu_max_lsb");
            if kind == Kind::Nv12 {
                run.measure(&what, off);
            } else if !run.at_most(&what, off, 1.0) {
                run.write_diffs(name, RECT, &g, &cpu.data);
            }
            cpus.insert(name, cpu);
            gpus.insert(name, g);
            sources.push(Source::new("stripes", f));
        }
        // Control: the Left-sited frame decoded as Center (the planes are
        // the same; only the tag differs) misses Left's CPU reference by
        // far.
        run.over(
            &format!("{key}control_left_as_center_max_lsb"),
            max_diff(&gpus["center"], &cpus["left"].data) * 255.0,
            10.0,
        );
    }
    // Where each siting puts its chroma, against a truth that doesn't go
    // through `color::chroma_transform`, as the CPU reference does (it
    // shares the GPU's): red and blue swinging against each other, 32
    // texels a period, coded at 4:2:0 with each chroma sample taken from
    // the pattern where the siting says that sample sits, and at 4:4:4,
    // where no siting applies. Tagged as the other siting, the chroma
    // lands half a luma texel off.
    let swing = |x: f64| {
        let s = (std::f64::consts::TAU * x / 32.0).sin();
        [0.5 + 0.35 * s, 0.45, 0.5 - 0.35 * s]
    };
    let limited = |siting| color(Range::Limited, Matrix::Bt709, siting);
    let full = coded(
        Kind::I444p12,
        (w, h),
        limited(ChromaSiting::Left),
        |p, x, _| ycbcr709(swing(f64::from(x) + 0.5), 12)[p],
    );
    let Some(truth) = run.render_with(&full, &v, RECT, DebugMode::Off) else {
        return;
    };
    let truth = unit(&truth);
    // Chroma sample j sits at luma sample 2j + t: t = 0 for Left, ½ for
    // Center (H.273), at texel coordinate 2j + ½ + t.
    for (name, siting, t, other) in [
        ("left", ChromaSiting::Left, 0.0, ChromaSiting::Center),
        ("center", ChromaSiting::Center, 0.5, ChromaSiting::Left),
    ] {
        let f = coded(Kind::P010, (w, h), limited(siting), |p, x, _| {
            let at = if p == 0 {
                f64::from(x) + 0.5
            } else {
                2.0 * f64::from(x) + 0.5 + t
            };
            ycbcr709(swing(at), 10)[p]
        });
        let mut wrong = f.clone();
        wrong.color.siting = other;
        let (Some(g), Some(c)) = (
            run.render_with(&f, &v, RECT, DebugMode::Off),
            run.render_with(&wrong, &v, RECT, DebugMode::Off),
        ) else {
            return;
        };
        let (g, c) = (unit(&g), unit(&c));
        let right = psnr_of(RECT, &g, &truth);
        let control = psnr_of(RECT, &c, &truth);
        run.at_least(&format!("swing_{name}_psnr_db"), right, SITING_SWING_DB);
        run.measure(
            &format!("swing_{name}_max_lsb"),
            max_diff(&g, &truth) * 255.0,
        );
        run.measure(&format!("control_swing_{name}_as_other_psnr_db"), control);
        if !run.at_least(
            &format!("swing_{name}_over_control_db"),
            right - control,
            SITING_OVER_DB,
        ) {
            run.write_diffs(&format!("swing-{name}"), RECT, &g, &truth);
        }
        sources.push(Source::new("swing", f));
    }
    sources.push(Source::new("swing", full));
    run.end_check(
        "red and blue stripes 5 texels wide, Left and Center sited, magnified, 10-bit (8-bit recorded); \
         control: Left as Center",
        &sources.iter().collect::<Vec<_>>(),
        vec![view_about(&v, RECT)],
    );
}

/// A 180° panorama: the grid's middle half, with sv3d bounds cropping a
/// quarter of the sphere's width off each side.
fn crop_half_source() -> (Frame, Placement) {
    let g = grid();
    let half = RgbImage {
        width: 512,
        height: 512,
        pixels: (0..512 * 512)
            .map(|i| g.get(256 + i % 512, i / 512))
            .collect(),
    };
    let placement = Placement {
        left: 0.25,
        right: 0.25,
        top: 0.0,
        bottom: 0.0,
        stereo: Stereo::Mono,
    };
    (frame(&half, Kind::Nv12, limited709(), 0), placement)
}

fn crop_half_view() -> ViewParams {
    ViewParams::new(ypr(60.0, 0.0, 0.0), lens(Projection::Rectilinear, 120.0))
}

fn crop_half(run: &mut Run<'_>) {
    let (f, placement) = crop_half_source();
    let v = crop_half_view();
    let opts = StillOptions {
        placement,
        ..StillOptions::default()
    };
    let Some(got) = run.render(&f, &v, RECT, opts) else {
        return;
    };
    // Outside the cover by more than two of its texels: black.
    let margin = 2.0 / 1024.0;
    let (mut outside, mut lit) = (0.0, 0.0);
    for j in 0..RECT.1 {
        for i in 0..RECT.0 {
            let c = pixel::centre(i, j);
            let u = reference::sphere_uv(reference::sphere_dir(&v, RECT, c.x, c.y)).x;
            if !(0.25 - margin..=0.75 + margin).contains(&u) {
                outside += 1.0;
                if got.unit_at(i, j).unwrap() != [0.0; 3] {
                    lit += 1.0;
                }
            }
        }
    }
    run.at_least("pixels_outside", outside, 1000.0);
    run.at_most("pixels_outside_not_black", lit, 0.0);
    let cpu = reference::render(
        &f,
        &v,
        RECT,
        &reference::Options {
            level0: false,
            placement,
        },
    );
    match cpu {
        Ok(cpu) => run.cpu_check("cpu", &got, &cpu, (&f, &v, &placement)),
        Err(e) => run.fail(format!("the CPU reference: {e}")),
    }
    let s = Source::new("grid, the middle half", f);
    run.end_check(
        "a 180° panorama by sv3d bounds, seen past its edge",
        &[&s],
        vec![view_about(&v, RECT)],
    );
}

fn lod_heatmap(run: &mut Run<'_>) {
    let s = grid_nv12();
    let v = ViewParams::new(ypr(150.0, 30.0, 0.0), lens(Projection::Fisheye, 270.0));
    let opts = StillOptions {
        debug: DebugMode::LodHeatmap,
        depth: Depth::Linear,
        ..StillOptions::default()
    };
    let Some(got) = run.render(&s.frame, &v, SQUARE, opts) else {
        return;
    };
    // λ from the CPU's footprint: log2 of its longer side in texels of
    // luma (1024×512, 5 levels) and of chroma (512×256, 4 levels).
    let (mut worst, mut over) = (0.0f64, 0.0);
    for j in 0..SQUARE.1 {
        for i in 0..SQUARE.0 {
            let (gx, gy) = reference::uv_footprint(&v, SQUARE, i, j);
            let lod = |w: f64, h: f64| {
                let s = glam::DVec2::new(w, h);
                (gx * s).length().max((gy * s).length()).max(1e-30).log2()
            };
            let (l, c) = (lod(1024.0, 512.0), lod(512.0, 256.0));
            let want = [l.clamp(0.0, 4.0), c.clamp(0.0, 3.0), l];
            let have = got.unit_at(i, j).unwrap();
            for k in 0..3 {
                let d = (have[k] - want[k]).abs();
                worst = worst.max(d);
                if d > 0.5 {
                    over += 1.0;
                }
            }
        }
    }
    run.measure("max_level_error", worst);
    run.at_most("levels_off_by_more_than_half", over, 0.0);
    run.end_check(
        "DEBUG_MODE 3's levels against the CPU's uv_footprint, fisheye 270°",
        &[&s],
        vec![view_about(&v, SQUARE)],
    );
}

// ---------------------------------------------------------------------
// A GPU's filter and mip stores, emulated (D32)
//
// The CPU reference with its filter's fractions and its 8-bit mip stores
// held as a GPU might hold them, to see what each costs the CPU checks:
// `the_wide_views_lose_to_the_mip_stores_not_the_filter`.

/// How a GPU holds the numbers it filters with, for [`Emulation`]: each
/// bilinear fraction, the LOD and the blend weight between two levels in
/// fixed point with this many fractional bits, or in f64 (`None`),
/// rounded to nearest or toward zero; whether each filtered 8-bit value
/// is held to whole codes; and how the 8-bit mips are stored.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Precision {
    texel_bits: Option<i32>,
    lod_bits: Option<i32>,
    blend_bits: Option<i32>,
    truncate: bool,
    codes: bool,
    mips: MipStore,
}

impl Precision {
    /// The CPU reference's own: its filter in f64, and its 8-bit mips as
    /// the mip pass makes them.
    const F64: Precision = Precision {
        texel_bits: None,
        lod_bits: None,
        blend_bits: None,
        truncate: false,
        codes: false,
        mips: MipStore::Rounded,
    };

    fn fixed(&self, v: f64, bits: Option<i32>) -> f64 {
        let Some(b) = bits else {
            return v;
        };
        let s = 2f64.powi(b);
        if self.truncate {
            (v * s).floor() / s
        } else {
            (v * s).round() / s
        }
    }

    fn describe(&self) -> String {
        let bits = |b: Option<i32>| b.map_or_else(|| "f64".into(), |b| b.to_string());
        format!(
            "texel {}, LOD {}, blend {}{}{}, mips {:?}",
            bits(self.texel_bits),
            bits(self.lod_bits),
            bits(self.blend_bits),
            if self.truncate { " toward zero" } else { "" },
            if self.codes { ", 8-bit results" } else { "" },
            self.mips
        )
    }
}

/// How an 8-bit mip level is stored. As the mip pass makes it now: the
/// exact area mean of the codes to the nearest code, ties to even
/// (`reference::halve_codes`), returned as k / 255, which `Rounded`
/// stores as k and `RoundedThroughRadv` as RADV's export stores it, packed
/// to f16 toward zero first (D31): k again, which is the fix's point. As
/// the reference stored it before the pass rounded: `TiesUp`, the f64
/// mean to the nearest code with ties up (f64's `round`), which the RX
/// 7600's CPU checks at be38fa4 were measured against. Or as the pass made
/// it then (D31), its f32 area mean left to the export:
/// `UnroundedLavapipe` that mean times 255 to the nearest even code, as
/// lavapipe's export does, and `UnroundedRadv` that mean through RADV's
/// export, which puts a mean on a half code, or up to 0.1245 codes above
/// one, a code low.
#[derive(Debug, Clone, Copy, PartialEq)]
enum MipStore {
    Rounded,
    RoundedThroughRadv,
    TiesUp,
    UnroundedLavapipe,
    UnroundedRadv,
}

impl MipStore {
    /// A texel stored, from `k`, the code the pass rounds the mean to,
    /// `exact`, the mean in f64, and `shader`, the mean as the pass summed
    /// it in f32 before it rounded: a code / 255.
    fn store(self, k: u8, exact: f64, shader: f32) -> f64 {
        let code = match self {
            MipStore::Rounded => k,
            MipStore::RoundedThroughRadv => radv_unorm8(f32::from(k) / 255.0),
            MipStore::TiesUp => (exact.clamp(0.0, 1.0) * 255.0).round() as u8,
            MipStore::UnroundedLavapipe => lavapipe_unorm8(shader),
            MipStore::UnroundedRadv => radv_unorm8(shader),
        };
        f64::from(code) / 255.0
    }
}

/// One plane's mip chain as the GPU stores it: each level's size and
/// texels, components interleaved.
struct Chain {
    format: PlaneFormat,
    comps: usize,
    levels: Vec<(u32, u32, Vec<f64>)>,
}

impl Chain {
    /// Plane `i` of `f` and its levels, each 8-bit one stored by `mips`.
    fn new(f: &Frame, shape: &SlotShape, i: usize, mips: MipStore) -> Chain {
        let p = shape.planes[i];
        let comps = p.format.components() as usize;
        let data = f.plane(i).unwrap();
        let row_bytes = p.size.0 as usize * p.format.bytes_per_texel() as usize;
        let shift = if shape.msb_aligned {
            16 - u32::from(shape.bit_depth)
        } else {
            0
        };
        let max = (1u32 << shape.bit_depth) - 1;
        let mut texels = Vec::new();
        for y in 0..p.size.1 {
            let row = data.row(y, row_bytes).unwrap();
            if p.format.is_deep() {
                texels.extend(row.chunks_exact(2).map(|b| {
                    let code = (u32::from(u16::from_le_bytes([b[0], b[1]])) >> shift).min(max);
                    round_f16(f64::from(code) / f64::from(max))
                }));
            } else {
                texels.extend(row.iter().map(|&b| f64::from(b) / 255.0));
            }
        }
        let mut chain = Chain {
            format: p.format,
            comps,
            levels: vec![(p.size.0, p.size.1, texels)],
        };
        while chain.levels.len() < p.levels as usize {
            let next = chain.halve(mips);
            chain.levels.push(next);
        }
        chain
    }

    /// The next level down: each texel the area-weighted mean of those
    /// it covers, stored in the plane's format: a deep one's f64 mean as
    /// its nearest half, an 8-bit one by `mips`, from the code the pass
    /// rounds it to, the f64 mean and the f32 mean the pass returned
    /// before it rounded.
    fn halve(&self, mips: MipStore) -> (u32, u32, Vec<f64>) {
        let (sw, sh, src) = self.levels.last().unwrap();
        let (sw, sh) = (*sw, *sh);
        let (dw, dh) = ((sw / 2).max(1), (sh / 2).max(1));
        let rounded = (!self.format.is_deep()).then(|| {
            let codes: Vec<u8> = src.iter().map(|t| (t * 255.0).round() as u8).collect();
            reference::halve_codes(&codes, (sw, sh), self.comps).2
        });
        let weights = |j: u32, ss: u32, ds: u32| -> Vec<(u32, f64)> {
            let (lo, hi) = (j * ss, j * ss + ss);
            (lo / ds..=(hi - 1) / ds)
                .map(|x| {
                    let overlap = hi.min((x + 1) * ds) - lo.max(x * ds);
                    (x, f64::from(overlap) / f64::from(ss))
                })
                .collect()
        };
        let mut data = Vec::with_capacity(dw as usize * dh as usize * self.comps);
        for j in 0..dh {
            let wy = weights(j, sh, dh);
            for i in 0..dw {
                let wx = weights(i, sw, dw);
                for c in 0..self.comps {
                    let (mut acc, mut shader) = (0.0, 0f32);
                    for &(y, a) in &wy {
                        for &(x, b) in &wx {
                            let t = src[(y as usize * sw as usize + x as usize) * self.comps + c];
                            acc += a * b * t;
                            shader += a as f32 * b as f32 * t as f32;
                        }
                    }
                    data.push(match &rounded {
                        None => round_f16(acc),
                        Some(k) => mips.store(k[data.len()], acc, shader),
                    });
                }
            }
        }
        (dw, dh, data)
    }

    /// A filtered value as `p` holds it.
    fn held(&self, p: &Precision, v: [f64; 2]) -> [f64; 2] {
        if p.codes && !self.format.is_deep() {
            v.map(|x| (x * 255.0).round() / 255.0)
        } else {
            v
        }
    }

    /// Level `l`'s bilinear blend at `uv`, its fractions held by `p`.
    fn bilinear(&self, l: usize, uv: DVec2, wrap_u: bool, p: &Precision) -> [f64; 2] {
        let (w, h, data) = &self.levels[l];
        let (w, h) = (i64::from(*w), i64::from(*h));
        let x = uv.x * w as f64 - 0.5;
        let y = uv.y * h as f64 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (a, b) = (p.fixed(x - x0, p.texel_bits), p.fixed(y - y0, p.texel_bits));
        let col = |i: i64| {
            if wrap_u {
                i.rem_euclid(w)
            } else {
                i.clamp(0, w - 1)
            }
        };
        let row = |j: i64| j.clamp(0, h - 1);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let mut out = [0.0; 2];
        for (c, o) in out.iter_mut().enumerate().take(self.comps) {
            let t = |i: i64, j: i64| data[(row(j) * w + col(i)) as usize * self.comps + c];
            let top = t(x0, y0) * (1.0 - a) + t(x0 + 1, y0) * a;
            let bottom = t(x0, y0 + 1) * (1.0 - a) + t(x0 + 1, y0 + 1) * a;
            *o = top * (1.0 - b) + bottom * b;
        }
        self.held(p, out)
    }

    /// Trilinear at `lod`, already clamped to the chain.
    fn sample(&self, uv: DVec2, lod: f64, wrap_u: bool, p: &Precision) -> [f64; 2] {
        let last = self.levels.len() - 1;
        let lod = p.fixed(lod, p.lod_bits);
        let l0 = (lod.floor().max(0.0) as usize).min(last);
        let l1 = (l0 + 1).min(last);
        let t = p.fixed((lod - l0 as f64).clamp(0.0, 1.0), p.blend_bits);
        let a = self.bilinear(l0, uv, wrap_u, p);
        if t == 0.0 || l1 == l0 {
            return a;
        }
        let b = self.bilinear(l1, uv, wrap_u, p);
        self.held(p, [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t])
    }
}

/// Where a shown pixel samples: luma's and chroma's texture uv, and
/// their LODs before clamping.
struct Tap {
    uv: DVec2,
    cuv: DVec2,
    lod: [f64; 2],
}

/// `reference::render` with its filter at any [`Precision`]: the
/// source's chains and colour, and where each pixel samples (`None`
/// where the view shows nothing), worked out once for every precision.
/// At [`Precision::F64`] it is the reference itself, which
/// `the_wide_views_lose_to_the_mip_stores_not_the_filter` asserts.
struct Emulation {
    frame: Frame,
    shape: SlotShape,
    semi_planar: bool,
    matrix: [[f64; 4]; 3],
    linear: bool,
    wraps: bool,
    levels: [f64; 2],
    taps: Vec<Option<Tap>>,
}

impl Emulation {
    fn new(f: &Frame, v: &ViewParams, size: (u32, u32), placement: &Placement) -> Emulation {
        let shape = SlotShape::of(&f.layout, placement.shown(f.size)).unwrap();
        let u = spherecut_render::color::source_uniform(&shape, &f.color, placement);
        let (scale, offset) = placement.uv_transform();
        let (scale, offset) = (DVec2::from(scale), DVec2::from(offset));
        let (cs, co) = if matches!(shape.kind, LayoutKind::SemiPlanar | LayoutKind::Planar) {
            spherecut_render::color::chroma_transform(&shape, f.color.siting)
        } else {
            ([1.0, 1.0], [0.0, 0.0])
        };
        let (cs, co) = (DVec2::from(cs), DVec2::from(co));
        let texels = |i: usize| {
            let p = shape.planes.get(i).copied().unwrap_or(shape.planes[0]);
            DVec2::new(f64::from(p.size.0), f64::from(p.size.1))
        };
        let lod = |g: [DVec2; 2], s: DVec2| {
            (g[0] * s)
                .length()
                .max((g[1] * s).length())
                .max(1e-30)
                .log2()
        };
        let mut taps = Vec::with_capacity(size.0 as usize * size.1 as usize);
        for j in 0..size.1 {
            for i in 0..size.0 {
                taps.push(shows(v, size, placement, (i, j)).then(|| {
                    let c = pixel::centre(i, j);
                    let suv = reference::sphere_uv(reference::sphere_dir(v, size, c.x, c.y));
                    let (gx, gy) = reference::uv_footprint(v, size, i, j);
                    let uv = suv * scale + offset;
                    let g = [gx * scale, gy * scale];
                    Tap {
                        uv,
                        cuv: uv * cs + co,
                        lod: [lod(g, texels(0)), lod(g.map(|x| x * cs), texels(1))],
                    }
                }));
            }
        }
        Emulation {
            frame: f.clone(),
            semi_planar: shape.kind == LayoutKind::SemiPlanar,
            matrix: spherecut_render::color::fold(&f.color, shape.kind, shape.bit_depth),
            linear: f.color.transfer == Transfer::Linear,
            wraps: placement.wraps(),
            levels: u.levels.map(f64::from),
            taps,
            shape,
        }
    }

    /// Every plane's mip chain, each 8-bit level stored by `mips`.
    fn chains(&self, mips: MipStore) -> Vec<Chain> {
        (0..self.shape.planes.len())
            .map(|i| Chain::new(&self.frame, &self.shape, i, mips))
            .collect()
    }

    /// RGB in [0, 1], as the encode pass computes it before it rounds it
    /// to codes.
    fn render(&self, p: &Precision) -> Vec<f64> {
        let chains = self.chains(p.mips);
        let mut out = Vec::with_capacity(3 * self.taps.len());
        for t in &self.taps {
            let Some(t) = t else {
                out.extend([0.0; 3]);
                continue;
            };
            let plane = |i: usize, uv: DVec2, lod: f64| {
                chains
                    .get(i)
                    .map_or([0.0; 2], |c| c.sample(uv, lod, self.wraps, p))
            };
            let y = plane(0, t.uv, t.lod[0].clamp(0.0, self.levels[0] - 1.0));
            let chroma = t.lod[1].clamp(0.0, self.levels[1] - 1.0);
            let (c1, c2) = (plane(1, t.cuv, chroma), plane(2, t.cuv, chroma));
            let s = [
                y[0],
                c1[0],
                if self.semi_planar { c1[1] } else { c2[0] },
                1.0,
            ];
            let rgb = self
                .matrix
                .map(|row| (0..4).map(|i| row[i] * s[i]).sum::<f64>().clamp(0.0, 1.0));
            out.extend(rgb.map(|v| {
                let lin = round_f16(if self.linear { v } else { eotf(v) });
                oetf(lin.clamp(0.0, 1.0))
            }));
        }
        out
    }
}

/// One CPU check of the rows: its row and PSNR key, and what it renders.
struct CpuCheck {
    row: &'static str,
    key: &'static str,
    frame: Frame,
    view: ViewParams,
    size: (u32, u32),
    placement: Placement,
}

/// Every CPU check of the rows, in their order.
fn cpu_checks() -> Vec<CpuCheck> {
    let check = |row, key, frame, view, size, placement| CpuCheck {
        row,
        key,
        frame,
        view,
        size,
        placement,
    };
    let grid = || grid_nv12().frame;
    let mono = Placement::default();
    let (crop, cropped) = crop_half_source();
    let key = "cpu_psnr_db";
    vec![
        check("rect_forward", key, grid(), rect_forward_view(), RECT, mono),
        check("rect_ypr", key, grid(), rect_ypr_view(), RECT, mono),
        check("fisheye_180", key, grid(), fisheye_180_view(), SQUARE, mono),
        check("fisheye_360", key, grid(), fisheye_360_view(), SQUARE, mono),
        check(
            "stereo_tinyplanet",
            key,
            grid(),
            stereo_tinyplanet_view(),
            PLANET,
            mono,
        ),
        check("morph_half", key, grid(), morph_half_view(), RECT, mono),
        check("crop_half", key, crop, crop_half_view(), RECT, cropped),
        check(
            "matrix_601_709",
            "cpu_601_psnr_db",
            grid_coded(Matrix::Bt601),
            matrix_view(),
            RECT,
            mono,
        ),
        check(
            "matrix_601_709",
            "cpu_709_psnr_db",
            grid_coded(Matrix::Bt709),
            matrix_view(),
            RECT,
            mono,
        ),
    ]
}

/// Pixels of `display` (RGB in [0, 1]) whose red lies more than 0.1 of
/// a code from a whole 8-bit one.
fn between_codes(display: &[f64]) -> usize {
    display
        .chunks(3)
        .filter(|p| {
            let c = p[0] * 255.0;
            (c - c.round()).abs() > 0.1
        })
        .count()
}

/// Whether `gpu` holds what it filters from an 8-bit plane to whole
/// codes, as the emulation's model of lavapipe's filter has it, and how
/// many pixels say otherwise: the checker's greys of 0.1 and 0.9
/// magnified 2.25 times, so that its edges are filtered at level 0, read
/// back in linear light, whose f16 moves a code by up to 0.055 (D32
/// found every pixel within 0.035 of a code at 16 bits). The f64
/// reference puts thousands of pixels between codes, which is asserted.
fn filters_to_codes(gpu: &Gpu, renderer: &mut StillRenderer) -> (bool, usize) {
    let s = checker_gray();
    let v = ViewParams::new(ypr(10.0, 0.0, 0.0), lens(Projection::Rectilinear, 20.0));
    let opts = StillOptions {
        depth: Depth::Linear,
        ..StillOptions::default()
    };
    let got = renderer.render(gpu, &s.frame, &v, RECT, &opts).unwrap();
    let display: Vec<f64> = unit(&got).iter().map(|l| oetf(*l)).collect();
    let mut seen = [false; 256];
    for p in display.chunks(3) {
        seen[(p[0].clamp(0.0, 1.0) * 255.0).round() as usize] = true;
    }
    let shades = seen.iter().filter(|s| **s).count();
    assert!(shades > 100, "the probe shows {shades} shades");
    let cpu = reference::render_trilinear(&s.frame, &v, RECT).unwrap();
    let reference = between_codes(&cpu.data);
    assert!(
        reference > 2000,
        "a filter in f64 puts {reference} pixels between codes"
    );
    let between = between_codes(&display);
    eprintln!(
        "the 8-bit filter probe: {shades} shades, {between} pixels between codes ({reference} \
         in f64)"
    );
    (between == 0, between)
}

// ---------------------------------------------------------------------
// Local faults, for the other-driver rule (D32)
//
// What a localized fault does to a stored golden, against what another
// llvmpipe build does: `the_other_driver_rule_catches_local_faults`.

/// The rows that store an image, with their views and sizes.
fn stored_views() -> [(&'static str, ViewParams, (u32, u32)); 6] {
    [
        ("rect_forward", rect_forward_view(), RECT),
        ("rect_ypr", rect_ypr_view(), RECT),
        ("fisheye_180", fisheye_180_view(), SQUARE),
        ("fisheye_360", fisheye_360_view(), SQUARE),
        ("stereo_tinyplanet", stereo_tinyplanet_view(), PLANET),
        ("morph_half", morph_half_view(), RECT),
    ]
}

fn rgb8_of(img: &Image) -> &[u8] {
    match &img.pixels {
        Pixels::Rgb8(p) => p,
        _ => panic!("an 8-bit image was expected"),
    }
}

/// `img` moved `(dx, dy)` pixels right and down, its first columns or
/// rows repeated: a render a pixel off, or cropped a pixel off.
fn moved(img: &Image, (dx, dy): (u32, u32)) -> Image {
    let p = rgb8_of(img);
    let w = img.width as usize;
    let mut out = p.to_vec();
    for y in 0..img.height as usize {
        for x in 0..w {
            let (sx, sy) = (x.saturating_sub(dx as usize), y.saturating_sub(dy as usize));
            out[3 * (y * w + x)..][..3].copy_from_slice(&p[3 * (sy * w + sx)..][..3]);
        }
    }
    Image {
        pixels: Pixels::Rgb8(out),
        ..img.clone()
    }
}

/// `img` with the pixels in `mask` black.
fn blanked(img: &Image, mask: &[bool]) -> Image {
    let mut out = rgb8_of(img).to_vec();
    for (px, m) in out.chunks_mut(3).zip(mask) {
        if *m {
            px.fill(0);
        }
    }
    Image {
        pixels: Pixels::Rgb8(out),
        ..img.clone()
    }
}

/// The pixels of a `size` view along the seam: shown, with u wrapping
/// between them and the pixel to their right or below.
fn seam_pixels(v: &ViewParams, size: (u32, u32)) -> Vec<bool> {
    let p = Placement::default();
    let u = |i: u32, j: u32| {
        let c = pixel::centre(i, j);
        reference::sphere_uv(reference::sphere_dir(v, size, c.x, c.y)).x
    };
    let mut mask = vec![false; size.0 as usize * size.1 as usize];
    for j in 0..size.1 {
        for i in 0..size.0 {
            if !shows(v, size, &p, (i, j)) {
                continue;
            }
            let wraps = |n: (u32, u32)| {
                n.0 < size.0
                    && n.1 < size.1
                    && shows(v, size, &p, n)
                    && (u(i, j) - u(n.0, n.1)).abs() > 0.5
            };
            mask[(j * size.0 + i) as usize] = wraps((i + 1, j)) || wraps((i, j + 1));
        }
    }
    mask
}

/// The four pixels nearest each pole a `size` view shows within a pixel.
fn pole_pixels(v: &ViewParams, size: (u32, u32)) -> Vec<bool> {
    let p = Placement::default();
    let n = size.0 as usize * size.1 as usize;
    let mut mask = vec![false; n];
    let dirs: Vec<Option<DVec3>> = (0..n)
        .map(|k| {
            let (i, j) = ((k % size.0 as usize) as u32, (k / size.0 as usize) as u32);
            let c = pixel::centre(i, j);
            shows(v, size, &p, (i, j)).then(|| reference::sphere_dir(v, size, c.x, c.y))
        })
        .collect();
    for pole in [DVec3::Y, DVec3::NEG_Y] {
        let mut near: Vec<(f64, usize)> = dirs
            .iter()
            .enumerate()
            .filter_map(|(k, d)| d.map(|d| (d.angle_between(pole), k)))
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let Some(&(a, k)) = near.first() else {
            continue;
        };
        // Within a pixel: nearer than the pixel's own step to its
        // neighbour.
        let (i, j) = ((k % size.0 as usize) as u32, (k / size.0 as usize) as u32);
        let c = pixel::centre(i, j);
        let step = reference::sphere_dir(v, size, c.x, c.y).angle_between(reference::sphere_dir(
            v,
            size,
            c.x + 1.0,
            c.y,
        ));
        if a <= step {
            for &(_, k) in near.iter().take(4) {
                mask[k] = true;
            }
        }
    }
    mask
}

/// The pixels of a `size` view within a pixel inside θ = π.
fn rim_pixels(v: &ViewParams, size: (u32, u32)) -> Vec<bool> {
    let p = Placement::default();
    let mut mask = vec![false; size.0 as usize * size.1 as usize];
    for j in 0..size.1 {
        for i in 0..size.0 {
            let c = pixel::centre(i, j);
            let rho = pixel::to_lens(size, c.x, c.y).length();
            let further = rho + 2.0 / f64::from(size.0);
            mask[(j * size.0 + i) as usize] =
                shows(v, size, &p, (i, j)) && v.lens.theta(further) > PI + 1e-4;
        }
    }
    mask
}

/// `v` with its field of view changed so that what its left and right
/// edges show lands `px` pixels further in (`px` < 0: further out), or
/// `None` where the projection can't widen that far.
fn widened(v: &ViewParams, size: (u32, u32), px: f64) -> Option<ViewParams> {
    let edge = 1.0 - 2.0 * px / f64::from(size.0);
    let want = v.lens.theta(1.0);
    let lens = |hfov: f64| Lens { hfov, ..v.lens };
    let (mut lo, mut hi) = (v.lens.hfov / 2.0, v.lens.hfov * 2.0);
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        if lens(mid).theta(edge) < want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let l = lens((lo + hi) / 2.0);
    ((l.theta(edge) - want).abs() < 1e-9).then(|| ViewParams::new(v.orientation, l))
}

// ---------------------------------------------------------------------
// The tests

#[test]
fn goldens() {
    let gpu = gpu();
    assert_eq!(gpu.caps.info.device_type, wgpu::DeviceType::Cpu);
    let update = std::env::var_os(UPDATE);
    let bless = update.as_ref().is_some_and(|v| !v.is_empty() && v != "0");
    let _ = std::fs::remove_dir_all(diff_dir());
    let mut run = Run::new(gpu, Mode::Golden { bless });
    run.lower_floors = update.is_some_and(|v| v == LOWER);
    run.tell = true;
    let start = Instant::now();
    run_rows(&mut run, None);
    eprintln!("goldens: {:.1} s", start.elapsed().as_secs_f64());
    run.finish();
}

#[test]
fn stored_images_follow_the_rules_of_section_6() {
    let img = |f: &dyn Fn(u32, u32, usize) -> u8| Image {
        width: 64,
        height: 32,
        pixels: Pixels::Rgb8(
            (0..64 * 32 * 3u32)
                .map(|n| f(n / 3 % 64, n / 3 / 64, (n % 3) as usize))
                .collect(),
        ),
    };
    let want = img(&|x, y, c| ((x * 7 + y * 3 + c as u32 * 40) % 200) as u8 + 20);
    // One LSB everywhere: 48.13 dB, inside the other-driver rule, and not
    // identical.
    let one = img(&|x, y, c| ((x * 7 + y * 3 + c as u32 * 40) % 200) as u8 + 21);
    // Half a pixel's shift, as a bilinear tap would make it: outside.
    let shifted = img(&|x, y, c| {
        let a = (x * 7 + y * 3 + c as u32 * 40) % 200;
        let b = ((x + 1) * 7 + y * 3 + c as u32 * 40) % 200;
        ((a + b) / 2) as u8 + 20
    });
    let same = drift(&want, &want).unwrap();
    assert!(same.identical && judge(&same, true).is_ok() && judge(&same, false).is_ok());
    let d = drift(&one, &want).unwrap();
    assert!((d.psnr - 48.13).abs() < 0.01 && d.max_lsb == 1.0, "{d:?}");
    assert!(judge(&d, true).is_err() && judge(&d, false).is_ok());
    let d = drift(&shifted, &want).unwrap();
    assert!(judge(&d, false).is_err(), "{d:?}");
    // Past 4 LSB: four pixels may be, by up to 8 LSB, in any channel; a
    // fifth pixel, or a ninth LSB, fails, and the message counts them.
    let off = |pixels: &[(usize, usize, u8)]| {
        let mut img = want.clone();
        if let Pixels::Rgb8(p) = &mut img.pixels {
            for &(n, c, by) in pixels {
                p[3 * n + c] += by;
            }
        }
        drift(&img, &want).unwrap()
    };
    let d = off(&[(0, 0, 5), (100, 1, 5), (700, 2, 6), (2047, 0, 8)]);
    assert!(d.past == 4.0 && d.max_lsb == 8.0, "{d:?}");
    assert!(judge(&d, false).is_ok(), "{d:?}");
    // Two channels of one pixel are one pixel.
    let d = off(&[(0, 0, 5), (0, 1, 5), (100, 1, 5), (700, 2, 6), (2047, 0, 8)]);
    assert!(d.past == 4.0 && judge(&d, false).is_ok(), "{d:?}");
    let d = off(&[
        (0, 0, 5),
        (100, 1, 5),
        (700, 2, 6),
        (1000, 2, 5),
        (2047, 0, 8),
    ]);
    let e = judge(&d, false).unwrap_err();
    assert!(
        d.past == 5.0 && e.contains("5 pixels more than 4 LSB off, max 8 LSB"),
        "{e}"
    );
    let d = off(&[(0, 0, 9)]);
    let e = judge(&d, false).unwrap_err();
    assert!(
        d.past == 1.0 && e.contains("1 pixel more than 4 LSB off, max 9 LSB"),
        "{e}"
    );
    let small = Image {
        width: 32,
        ..want.clone()
    };
    assert!(drift(&small, &want).is_err());
}

/// A scratch goldens directory and its diffs, for the harness's own
/// tests: emptied first, and removed when the test passes (a failing one
/// leaves it, with its diffs, to look at).
struct Scratch {
    dir: PathBuf,
    diffs: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("goldens-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let diffs = dir.join("diffs");
    Scratch { dir, diffs }
}

/// Blesses `rows` into `dir`.
fn bless_into(dir: &Path, diffs: &Path, rows: &[&str]) {
    let mut run = Run::at(gpu(), Mode::Golden { bless: true }, dir, diffs);
    run_rows(&mut run, Some(rows));
    run.finish();
}

#[test]
fn blessing_writes_everything_or_nothing() {
    let s = scratch("bless");
    let (dir, diffs) = (s.dir.clone(), s.diffs.clone());
    std::fs::write(dir.join(MANIFEST), "{}").unwrap();
    // One failure anywhere and nothing is written, however many rows
    // passed.
    let mut run = Run::at(gpu(), Mode::Golden { bless: true }, &dir, &diffs);
    run_rows(&mut run, Some(&["rect_forward"]));
    run.begin("a_row");
    run.fail("a check that failed");
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run.finish()));
    let msg = *refused
        .expect_err("blessing went ahead")
        .downcast::<String>()
        .unwrap();
    assert!(
        msg.contains("refusing to bless") && msg.contains("a check that failed"),
        "{msg}"
    );
    assert_eq!(std::fs::read_to_string(dir.join(MANIFEST)).unwrap(), "{}");
    assert!(!dir.join("rect_forward.png").exists());
    // Everything passing writes the images and the manifest, which a
    // plain run then matches pixel for pixel.
    bless_into(&dir, &diffs, &["rect_forward", "lod_heatmap"]);
    let mut run = Run::at(gpu(), Mode::Golden { bless: false }, &dir, &diffs);
    run_rows(&mut run, Some(&["rect_forward", "lod_heatmap"]));
    assert!(run.failures.is_empty(), "{:?}", run.failures);
    assert_eq!(
        run.images["rect_forward"]["measured"]["golden_identical"],
        json!(1.0)
    );
    assert!(
        run.checks["lod_heatmap"]["thresholds"]["levels_off_by_more_than_half_max"].is_number()
    );
}

#[test]
fn a_bound_over_is_strict() {
    // "Over 100 bad pixels" fails at 100.
    let s = scratch("over");
    let mut run = Run::at(gpu(), Mode::Golden { bless: false }, &s.dir, &s.diffs);
    run.begin("a_row");
    assert!(!run.over("control", 100.0, 100.0));
    assert!(run.over("control", 101.0, 100.0));
    assert_eq!(run.failures.len(), 1, "{:?}", run.failures);
    assert_eq!(run.thresholds["control_over"], json!(100.0));
}

#[test]
fn blessing_wont_lower_a_floor_unasked() {
    let s = scratch("floors");
    let (dir, diffs) = (s.dir.clone(), s.diffs.clone());
    bless_into(&dir, &diffs, &["rect_forward"]);
    let path = dir.join(MANIFEST);
    let manifest = |floor: Option<f64>| {
        let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let t = &mut m["images"]["rect_forward"]["thresholds"]["cpu_psnr_db_min"];
        let was = t.as_f64().unwrap();
        if let Some(f) = floor {
            *t = json!(f);
            std::fs::write(&path, m.to_string()).unwrap();
        }
        was
    };
    let floor = manifest(None);
    let bless = |lower: bool| {
        let mut run = Run::at(gpu(), Mode::Golden { bless: true }, &dir, &diffs);
        run.lower_floors = lower;
        run_rows(&mut run, Some(&["rect_forward"]));
        let failures = run.failures.clone();
        let blessed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run.finish()));
        (failures, blessed.is_ok())
    };
    // Blessed before at a floor 0.4 dB higher: within the margin, so the
    // blessing goes ahead and writes this build's floor.
    manifest(Some(floor + 0.4));
    let (failures, blessed) = bless(false);
    assert!(blessed && failures.is_empty(), "{failures:?}");
    assert_eq!(manifest(None), floor);
    // 2 dB higher: the blessing refuses, says which floor and how to
    // accept it, and writes nothing.
    manifest(Some(floor + 2.0));
    let (failures, blessed) = bless(false);
    assert!(
        !blessed
            && failures.len() == 1
            && failures[0].contains("would lower cpu_psnr_db_min")
            && failures[0].contains("SPHERECUT_UPDATE_GOLDENS=lower"),
        "{failures:?}"
    );
    assert_eq!(manifest(None), floor + 2.0);
    // Told it may, it lowers it.
    let (failures, blessed) = bless(true);
    assert!(blessed && failures.is_empty(), "{failures:?}");
    assert_eq!(manifest(None), floor);
}

#[test]
fn a_changed_source_is_not_compared_as_a_render() {
    let s = scratch("source");
    let (dir, diffs) = (s.dir.clone(), s.diffs.clone());
    bless_into(&dir, &diffs, &["rect_forward"]);
    let path = dir.join(MANIFEST);
    let blessed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let rerun = || {
        let mut run = Run::at(gpu(), Mode::Golden { bless: false }, &dir, &diffs);
        run_rows(&mut run, Some(&["rect_forward"]));
        run.failures
    };
    // The planes hash otherwise than blessed: the source changed.
    let mut m = blessed.clone();
    m["images"]["rect_forward"]["source_fnv64"] = json!("0123456789abcdef");
    std::fs::write(&path, m.to_string()).unwrap();
    let failures = rerun();
    assert!(
        failures.len() == 1 && failures[0].contains("the synthetic source changed"),
        "{failures:?}"
    );
    assert!(
        failures[0].contains("llvmpipe"),
        "no driver build in {failures:?}"
    );
    // The same source and one pixel a step off: not identical, at the
    // build it was blessed on, and the diffs are written.
    std::fs::write(&path, blessed.to_string()).unwrap();
    let png = dir.join("rect_forward.png");
    let mut img = Image::from_png(&std::fs::read(&png).unwrap()).unwrap();
    if let Pixels::Rgb8(p) = &mut img.pixels {
        p[1000] = p[1000].wrapping_add(1);
    }
    std::fs::write(&png, img.to_png().unwrap()).unwrap();
    let failures = rerun();
    assert!(
        failures.len() == 1 && failures[0].contains("not identical"),
        "{failures:?}"
    );
    assert!(diffs.join("rect_forward-golden-diff.png").exists());
}

#[test]
fn the_passthrough_model_and_its_bound() {
    // Full range is every code, exactly.
    for k in 0..=255u8 {
        assert_eq!(passthrough_model(k, false).0, k, "{k}");
    }
    // Limited range clamps outside 16 to 235, and inside is within one
    // code of round((k − 16) · 255 / 219), off by one only near a tie.
    assert_eq!(passthrough_model(10, true).0, 0);
    assert_eq!(passthrough_model(240, true).0, 255);
    let mut off = Vec::new();
    for k in 16..=235u8 {
        let ideal = (f64::from(k) - 16.0) * 255.0 / 219.0;
        let (got, _) = passthrough_model(k, true);
        if f64::from(got) != ideal.round() {
            assert!((ideal - ideal.floor() - 0.5).abs() < 0.0548, "{k}");
            off.push(k);
        }
    }
    assert_eq!(off, [86, 159, 165], "the codes the f16 store moves");
    // The half of 1 is 1, of 0.1 is 1638/16384, of a subnormal a multiple
    // of 2^−24.
    assert_eq!(nearest_half(1.0), 1.0);
    assert_eq!(nearest_half(0.1), 1638.0 / 16384.0);
    assert_eq!(nearest_half(3.0 * 2f64.powi(-26)), 2f64.powi(-24));
}

// No Tier 1 test but `goldens` reads `tests/goldens` (the Tier 2 tests,
// run on their own, do): `goldens` rewrites it while blessing, in
// parallel with the others, which would read the images it replaces, or
// one halfway through its rewrite, and fail the blessing command although
// it had written everything. The two Tier 2 paths' tests run on goldens
// blessed into a scratch directory, and
// `the_other_driver_rule_catches_local_faults` renders the stored views.

#[test]
fn the_reference_only_path_runs_on_lavapipe() {
    // goldens_on_gpu_match_reference's path, on two rows, with the floors
    // their blessing calibrated, which hold a GPU as they hold lavapipe.
    let s = scratch("references");
    let (dir, diffs) = (s.dir.clone(), s.diffs.clone());
    let rows = ["rect_forward", "tenbit_ramp"];
    bless_into(&dir, &diffs, &rows);
    let mut run = Run::at(gpu(), Mode::References, &dir, &diffs);
    run_rows(&mut run, Some(&rows));
    assert_eq!(run.notes.len(), 2, "{:?}", run.notes);
    // The error by LOD band: a 90° view of 1024 texels round, under a
    // level down everywhere.
    let m = &run.images["rect_forward"]["measured"];
    assert_eq!(m["cpu_lod_below_1_pixels"], json!(36864.0), "{m}");
    assert_eq!(m["cpu_lod_below_1_error_share"], json!(1.0), "{m}");
    assert_eq!(m["cpu_lod_from_2.5_pixels"], json!(0.0), "{m}");
    assert!(m["cpu_lod_below_1_psnr_db"].is_number(), "{m}");
    assert!(
        run.notes[0].contains("cpu_lod_1_to_2.5_pixels 0.0"),
        "{:?}",
        run.notes
    );
    assert!(run.pngs.is_empty());
    run.finish();
    // Blessed at 99 dB, the floor is 99 dB, which lavapipe's 53.2 dB
    // fails: no other floor stands in for the blessed one.
    let path = dir.join(MANIFEST);
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["images"]["rect_forward"]["thresholds"]["cpu_psnr_db_min"] = json!(99.0);
    std::fs::write(&path, m.to_string()).unwrap();
    let mut run = Run::at(gpu(), Mode::References, &dir, &diffs);
    run_rows(&mut run, Some(&["rect_forward"]));
    assert!(
        run.failures.len() == 1
            && run.failures[0].contains("cpu_psnr_db is ")
            && run.failures[0].contains(", below 99 [on "),
        "{:?}",
        run.failures
    );
}

#[test]
fn the_other_driver_path_runs_on_lavapipe() {
    // goldens_on_host_llvmpipe's path, on one stored image: the
    // other-driver rule whatever the build, with the drift noted. Blessed
    // at this build, the golden is identical; one pixel a step off then
    // passes, where the exact rule fails it at this build, and five
    // pixels 5 LSB off fail by the count, which the message gives.
    let s = scratch("other-driver");
    let (dir, diffs) = (s.dir.clone(), s.diffs.clone());
    bless_into(&dir, &diffs, &["rect_forward"]);
    let rerun = |mode: Mode| {
        let mut run = Run::at(gpu(), mode, &dir, &diffs);
        run_rows(&mut run, Some(&["rect_forward"]));
        run
    };
    let run = rerun(Mode::OtherDriver);
    assert!(
        run.notes[0].contains("golden_psnr_db")
            && run.notes[0].contains("golden_pixels_past_4_lsb 0.0"),
        "{:?}",
        run.notes
    );
    run.finish();
    let png = dir.join("rect_forward.png");
    let blessed = Image::from_png(&std::fs::read(&png).unwrap()).unwrap();
    let write_off = |pixels: &[usize], by: u8| {
        let mut img = blessed.clone();
        if let Pixels::Rgb8(p) = &mut img.pixels {
            for &n in pixels {
                let c = &mut p[3 * n];
                *c = if *c < 128 { *c + by } else { *c - by };
            }
        }
        std::fs::write(&png, img.to_png().unwrap()).unwrap();
    };
    write_off(&[1000], 1);
    let run = rerun(Mode::OtherDriver);
    assert!(run.failures.is_empty(), "{:?}", run.failures);
    let m = &run.images["rect_forward"]["measured"];
    assert!(
        m["golden_identical"] == json!(0.0) && m["golden_max_lsb"] == json!(1.0),
        "{m}"
    );
    let exact = rerun(Mode::Golden { bless: false });
    assert!(
        exact.failures.len() == 1 && exact.failures[0].contains("not identical"),
        "{:?}",
        exact.failures
    );
    write_off(&[1000, 5000, 9000, 20000, 30000], 5);
    let run = rerun(Mode::OtherDriver);
    assert!(
        run.failures.len() == 1
            && run.failures[0].contains("outside the other-driver tolerance")
            && run.failures[0].contains("5 pixels more than 4 LSB off, max 5 LSB"),
        "{:?}",
        run.failures
    );
    let m = &run.images["rect_forward"]["measured"];
    assert_eq!(m["golden_pixels_past_4_lsb"], json!(5.0), "{m}");
}

#[test]
fn driver_builds_are_told_apart() {
    let d = Driver {
        adapter: "llvmpipe (LLVM 20.1.2, 256 bits)".into(),
        driver: "llvmpipe".into(),
        driver_info: "Mesa 25.2.8-0ubuntu0.24.04.4 (LLVM 20.1.2)".into(),
    };
    assert_eq!(d.llvm().as_deref(), Some("20.1.2"));
    let radv = Driver {
        adapter: "AMD Radeon RX 7600 (RADV NAVI33)".into(),
        driver: "radv".into(),
        driver_info: "Mesa 26.2.5".into(),
    };
    assert_eq!(radv.llvm(), None);
    assert!(today().len() == 10 && today().starts_with("20"));
    assert!(wgpu_version().starts_with("30."));
}

#[test]
#[ignore = "tier2: needs a GPU (RADV or ANV)"]
fn goldens_on_gpu_match_reference() {
    // Every GPU, or the one SPHERECUT_ADAPTER names.
    tier2::require_hw_run();
    let gpus = if AdapterChoice::from_env().is_some() {
        vec![hardware_gpu()]
    } else {
        let (gpus, seen) = hardware_gpus();
        if gpus.is_empty() {
            tier2::skip(format!("no GPU: Vulkan lists only {}", seen.join(", ")));
        }
        gpus
    };
    let mut failed = Vec::new();
    for g in &gpus {
        let mut run = Run::new(g, Mode::References);
        run_rows(&mut run, None);
        for n in &run.notes {
            bench::note(n);
        }
        failed.extend(run.failures);
    }
    assert!(
        failed.is_empty(),
        "{} checks failed:\n  {}",
        failed.len(),
        failed.join("\n  ")
    );
}

#[test]
#[ignore = "tier2: runs the stored goldens on this machine's llvmpipe"]
fn goldens_on_host_llvmpipe() {
    tier2::require_hw_run();
    let gpu = match Gpu::headless(&AdapterChoice::Cpu) {
        Ok(g) => g,
        Err(e) => tier2::skip(format!(
            "no llvmpipe adapter ({e}): sudo dnf install mesa-vulkan-drivers (Fedora) or sudo pacman -S vulkan-swrast (Arch)"
        )),
    };
    assert_eq!(gpu.caps.info.device_type, wgpu::DeviceType::Cpu);
    let mut run = Run::new(&gpu, Mode::OtherDriver);
    run_rows(&mut run, None);
    run.finish();
}

/// Every CPU check's PSNR on the RX 7600 (RADV NAVI33, Mesa 26.2.5) at
/// be38fa4, when the mip pass left its 8-bit means to the driver's
/// export: `goldens_on_gpu_match_reference`'s notes in
/// `hw-reports/odin-be38fa409e.md`, by row and key.
const RX_7600_BE38FA4: &[(&str, &str, f64)] = &[
    ("rect_forward", "cpu_psnr_db", 58.5452),
    ("rect_ypr", "cpu_psnr_db", 58.0408),
    ("fisheye_180", "cpu_psnr_db", 47.7761),
    ("fisheye_360", "cpu_psnr_db", 44.6747),
    ("stereo_tinyplanet", "cpu_psnr_db", 51.4508),
    ("morph_half", "cpu_psnr_db", 52.7078),
    ("crop_half", "cpu_psnr_db", 56.5701),
    ("matrix_601_709", "cpu_601_psnr_db", 58.2078),
    ("matrix_601_709", "cpu_709_psnr_db", 58.0879),
];

#[test]
fn the_wide_views_lose_to_the_mip_stores_not_the_filter() {
    // Every CPU check, emulated with a GPU's precision ([`Emulation`])
    // and rounded to codes as the encode pass rounds it, against the
    // reference, as a GPU's CPU check measures it; and the filter's own
    // error, unrounded.
    //
    // At be38fa4 the mip pass left its 8-bit means to the driver's export
    // and the reference took their ties up, and the RX 7600 lost up to 15
    // dB on the wide, minified views (D32). Against that reference,
    // fixed-point filter weights, even a 4-bit blend toward zero, come
    // nowhere near its numbers, and RADV's export of those means comes
    // within 1 dB of every row.
    //
    // Now the pass rounds the means itself, ties to even, as the
    // reference does, and RADV's export of what it returns stores the
    // reference's codes in every level of every check. RADV's filter as
    // emulated, on those mips, with the error the emulation missed at
    // be38fa4 added, predicts each check after the fix: each clears
    // lavapipe's floor (its PSNR here less 3 dB, as a blessing calibrates
    // it), where the old means through RADV's export, against the old
    // reference, miss `fisheye_360`'s, as the RX 7600 did. lavapipe's own
    // filter, emulated the same way, comes within 1 dB of its render on
    // every row, which is what the method can be checked against here.
    // Both are asserted only where lavapipe filters 8-bit planes to whole
    // codes, as the model has it and as the lavapipe the goldens are
    // blessed on does: one that filters them in floating point renders
    // the wide views nearer the reference (59.97 dB for `fisheye_360`
    // with GALLIVM_PERF=no_aos_sampling), so its floors aren't the
    // blessed ones and say nothing of a GPU.
    let fixed = |blend: i32, truncate: bool| Precision {
        texel_bits: Some(8),
        lod_bits: Some(8),
        blend_bits: Some(blend),
        truncate,
        ..Precision::F64
    };
    let filters = [
        fixed(8, false),
        fixed(6, false),
        fixed(4, false),
        fixed(4, true),
    ];
    // be38fa4's reference, and each filter on its mips.
    let then = |p: Precision| Precision {
        mips: MipStore::TiesUp,
        ..p
    };
    // RADV's filter as the emulation finds it, on the old means through
    // its export and on what the pass returns now.
    let radv_then = Precision {
        mips: MipStore::UnroundedRadv,
        ..fixed(8, false)
    };
    let radv_now = Precision {
        mips: MipStore::RoundedThroughRadv,
        ..fixed(8, false)
    };
    let lavapipe = Precision {
        codes: true,
        ..fixed(8, false)
    };
    let coded = |v: &[f64]| -> Vec<f64> { v.iter().map(|x| (x * 255.0).round() / 255.0).collect() };
    // A floor as a blessing calibrates it from a PSNR.
    let floor = |db: f64| (((db - CPU_MARGIN_DB) * 100.0).floor() / 100.0).max(CPU_FLOOR_DB);
    // Squared error from PSNR and back.
    let mse = |db: f64| 10f64.powf(-db / 10.0);
    let db = |e: f64| -10.0 * e.log10();
    let gpu = gpu();
    let mut renderer = StillRenderer::new(gpu);
    let (mut filter_miss, mut radv_miss, mut lavapipe_miss) = ([0.0f64; 4], 0.0f64, 0.0f64);
    let (mut under, mut predictions) = (Vec::new(), Vec::new());
    for c in cpu_checks() {
        let what = format!("{} {}", c.row, c.key);
        let cpu = reference::render(
            &c.frame,
            &c.view,
            c.size,
            &reference::Options {
                level0: false,
                placement: c.placement,
            },
        )
        .unwrap();
        let e = Emulation::new(&c.frame, &c.view, c.size, &c.placement);
        // At f64 the emulation is the reference.
        let exact = e.render(&Precision::F64);
        let off = max_diff(&exact, &cpu.data);
        assert!(
            off < 1e-12,
            "{what}: the emulation is {off:e} off the reference"
        );
        // RADV's export keeps every code the pass rounds to.
        let through = e.chains(MipStore::RoundedThroughRadv);
        for (n, (a, b)) in through.iter().zip(e.chains(MipStore::Rounded)).enumerate() {
            assert!(
                a.levels == b.levels,
                "{what}: RADV's export moves plane {n}'s rounded mips"
            );
        }
        let old = e.render(&then(Precision::F64));
        let rx = RX_7600_BE38FA4
            .iter()
            .find(|(r, k, _)| *r == c.row && *k == c.key)
            .unwrap()
            .2;
        let opts = StillOptions {
            placement: c.placement,
            ..StillOptions::default()
        };
        let here = renderer
            .render(gpu, &c.frame, &c.view, c.size, &opts)
            .unwrap();
        let here = psnr_of(c.size, &unit(&here), &cpu.data);
        eprintln!(
            "{what}: RX 7600 at be38fa4 {rx:.2} dB, lavapipe {here:.2} dB (its floor {}), a \
             filter in f64 {:.2} dB",
            floor(here),
            psnr_of(c.size, &coded(&exact), &cpu.data)
        );
        // be38fa4: each filter against the reference of the time.
        let mut unrounded = Vec::new();
        for (n, p) in filters.iter().enumerate() {
            let got = e.render(&then(*p));
            let psnr = psnr_of(c.size, &coded(&got), &old);
            let raw = psnr_of(c.size, &got, &old);
            eprintln!(
                "{what}: at be38fa4, {}: {psnr:.2} dB ({raw:.2} unrounded)",
                then(*p).describe()
            );
            filter_miss[n] = filter_miss[n].max((psnr - rx).abs());
            unrounded.push(raw);
        }
        // Fewer bits in the blend, or truncated, never come nearer.
        assert!(
            unrounded.windows(2).all(|w| w[0] >= w[1]),
            "{what}: {unrounded:?}"
        );
        // Each export alone, the filter in f64: the old means against the
        // old reference, and the pass's now against the reference.
        for (mips, want, at) in [
            (MipStore::UnroundedRadv, &old, "at be38fa4"),
            (MipStore::UnroundedLavapipe, &old, "at be38fa4"),
            (MipStore::RoundedThroughRadv, &cpu.data, "now"),
        ] {
            let p = Precision {
                mips,
                ..Precision::F64
            };
            let psnr = psnr_of(c.size, &coded(&e.render(&p)), want);
            eprintln!("{what}: {at}, {}: {psnr:.2} dB", p.describe());
        }
        let bands = lod_bands(&c.frame, &c.view, c.size, &c.placement);
        let mut psnrs = Vec::new();
        for (name, p, want) in [
            ("at be38fa4, RADV's export", radv_then, &old),
            ("now, RADV's export", radv_now, &cpu.data),
            ("now, lavapipe's filter", lavapipe, &cpu.data),
            (
                "RADV's export of the old means against today's reference",
                radv_then,
                &cpu.data,
            ),
        ] {
            let got = coded(&e.render(&p));
            let psnr = psnr_of(c.size, &got, want);
            let by: Vec<String> = band_errors(&bands, &got, want)
                .iter()
                .zip(LOD_BANDS)
                .map(|(b, (band, _))| {
                    let db = b.psnr.map_or_else(|| "-".into(), |d| format!("{d:.2} dB"));
                    format!("{band} {} pixels {db} {:.1}%", b.pixels, b.share * 100.0)
                })
                .collect();
            eprintln!(
                "{what}: {name} ({}): {psnr:.2} dB; {}",
                p.describe(),
                by.join(", ")
            );
            psnrs.push(psnr);
        }
        let (radv, now, model) = (psnrs[0], psnrs[1], psnrs[2]);
        radv_miss = radv_miss.max((radv - rx).abs());
        lavapipe_miss = lavapipe_miss.max((model - here).abs());
        // The prediction: the emulation now, with the squared error it
        // missed of the RX 7600 at be38fa4 (RADV's own filter, as far as
        // the model isn't it) added.
        let missed = (mse(rx) - mse(radv)).max(0.0);
        let predicted = db(mse(now) + missed);
        eprintln!(
            "{what}: the RX 7600 after the fix: {now:.2} dB emulated, {predicted:.2} dB with the \
             error the emulation missed at be38fa4 ({:.2} dB alone) added",
            db(missed)
        );
        predictions.push((what.clone(), predicted, floor(here)));
        if radv < floor(here) {
            under.push(what);
        }
    }
    eprintln!(
        "worst miss: of the RX 7600, each filter {filter_miss:.2?} dB and RADV's export of the \
         old means {radv_miss:.2} dB; of lavapipe, its filter {lavapipe_miss:.2} dB; the old \
         means through RADV's export under lavapipe's floor: {under:?}"
    );
    for (p, miss) in filters.iter().zip(filter_miss) {
        assert!(
            miss > 5.0,
            "{} explains the RX 7600 to {miss:.2} dB",
            p.describe()
        );
    }
    assert!(
        radv_miss < 1.0,
        "RADV's export misses the RX 7600 by {radv_miss:.2} dB"
    );
    assert!(
        under.iter().any(|w| w.starts_with("fisheye_360 ")),
        "the old means through RADV's export clear lavapipe's floors but {under:?}"
    );
    // The model of lavapipe's filter is a fact about the lavapipe it was
    // made on, which holds what it filters from 8-bit planes to codes, and
    // so are the floors a blessing calibrates; a lavapipe that filters
    // them in floating point (GALLIVM_PERF=no_aos_sampling, or another
    // build) is reported, not held to either.
    let (to_codes, between) = filters_to_codes(gpu, &mut renderer);
    let short: Vec<String> = predictions
        .iter()
        .filter(|(_, predicted, floor)| predicted < floor)
        .map(|(what, predicted, floor)| format!("{what} at {predicted:.2} dB, under {floor}"))
        .collect();
    if to_codes {
        assert!(
            lavapipe_miss < 1.0,
            "lavapipe's filter misses lavapipe by {lavapipe_miss:.2} dB"
        );
        assert!(
            short.is_empty(),
            "the RX 7600 is predicted under lavapipe's floors after the fix: {short:?}"
        );
    } else {
        eprintln!(
            "lavapipe here filters 8-bit planes between codes ({between} pixels of the probe), \
             so neither its filter's model, which misses it by {lavapipe_miss:.2} dB, nor the \
             RX 7600's prediction against the floors it calibrates ({short:?} under them) is \
             asserted"
        );
    }
}

#[test]
fn the_other_driver_rule_catches_local_faults() {
    // Each stored view's render against faults made from it, or rendered
    // beside it: moved a pixel right or down, the view a pixel wider or
    // narrower at its edges, implicit LOD (the seam and pole rows'
    // control), and the seam, the poles' quads or the pixels within a
    // pixel of the rim blanked. Every fault fails the rule, but implicit
    // LOD in the rect views, which show no seam or pole and nothing far
    // minified, and differ there by no more than 4 LSB. Four pass PSNR
    // and SSIM, and only the count or the maximum catches them. As drift,
    // the views widened or narrowed by a hundredth of a pixel at their
    // edges pass. The render is the stored image wherever `goldens`
    // passes at the build it was blessed on; the PNG isn't read, since a
    // blessing rewrites it beside this test.
    let gpu = gpu();
    let mut renderer = StillRenderer::new(gpu);
    let s = grid_nv12();
    // The faults the averages miss, and whether the count and the
    // maximum catch them.
    let averages_miss = [
        ("fisheye_180", "implicit_lod", true, true),
        ("stereo_tinyplanet", "implicit_lod", true, false),
        ("stereo_tinyplanet", "poles_black", false, true),
        ("morph_half", "implicit_lod", true, false),
    ];
    let mut missed = Vec::new();
    for (row, v, size) in stored_views() {
        let mut render = |v: &ViewParams, debug: DebugMode| {
            let opts = StillOptions {
                debug,
                ..StillOptions::default()
            };
            renderer.render(gpu, &s.frame, v, size, &opts).unwrap()
        };
        let want = render(&v, DebugMode::Off);
        for px in [0.01, -0.01] {
            // A 360° fisheye can't widen.
            let Some(w) = widened(&v, size, px) else {
                continue;
            };
            let d = drift(&render(&w, DebugMode::Off), &want).unwrap();
            eprintln!(
                "{row}: drift, {px} pixel at the edges: {} pixels past {OTHER_PAST_LSB} LSB, max \
                 {} LSB, {:.2} dB, SSIM {:.5}",
                d.past, d.max_lsb, d.psnr, d.ssim
            );
            assert!(judge(&d, false).is_ok(), "{row}: {px} pixel: {d:?}");
        }
        let mut faults = vec![
            ("moved_right", moved(&want, (1, 0))),
            ("moved_down", moved(&want, (0, 1))),
            ("implicit_lod", render(&v, DebugMode::ImplicitLod)),
        ];
        for (name, px) in [("wider", 1.0), ("narrower", -1.0)] {
            // A 360° fisheye can't widen.
            if let Some(w) = widened(&v, size, px) {
                faults.push((name, render(&w, DebugMode::Off)));
            }
        }
        for (name, mask) in [
            ("seam_black", seam_pixels(&v, size)),
            ("poles_black", pole_pixels(&v, size)),
            ("rim_black", rim_pixels(&v, size)),
        ] {
            if mask.contains(&true) {
                faults.push((name, blanked(&want, &mask)));
            }
        }
        for (name, got) in &faults {
            let d = drift(got, &want).unwrap();
            let averages = d.psnr >= OTHER_PSNR_DB && d.ssim >= OTHER_SSIM;
            eprintln!(
                "{row}: {name}: {} pixels past {OTHER_PAST_LSB} LSB, max {} LSB, {:.2} dB, SSIM \
                 {:.5}{}",
                d.past,
                d.max_lsb,
                d.psnr,
                d.ssim,
                if averages {
                    ", within the averages"
                } else {
                    ""
                }
            );
            if *name == "implicit_lod" && row.starts_with("rect_") {
                assert!(d.past == 0.0 && d.max_lsb <= OTHER_PAST_LSB, "{row}: {d:?}");
                continue;
            }
            assert!(judge(&d, false).is_err(), "{row}: {name} passes: {d:?}");
            if averages {
                missed.push((
                    row,
                    *name,
                    d.past > OTHER_PIXELS_PAST,
                    d.max_lsb > OTHER_MAX_LSB,
                ));
            }
        }
    }
    assert_eq!(
        missed, averages_miss,
        "the faults within PSNR and SSIM, and whether the count and the maximum catch them"
    );
}
