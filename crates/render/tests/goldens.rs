//! The golden images (M1 plan section 6, DECISIONS D32), on lavapipe.
//!
//! Every row of the plan's table is a function below, run in order by
//! one test, [`goldens`], which gathers every failure before it reports,
//! so that blessing can refuse as a whole:
//!
//! - **Stored images** (`goldens/*.png`, with `goldens/manifest.json`)
//!   must match the render pixel for pixel at the driver build and source
//!   hash they were blessed at, and otherwise by PSNR ≥ 48 dB, SSIM ≥
//!   0.995 and at most 4 LSB. A source whose planes no longer hash as
//!   blessed is reported as the synthetic source having changed, never
//!   compared as a render.
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
//! CPU and ideal references only, and on the machine's own llvmpipe by
//! the other-driver rule, recording the drift.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use common::{Frame, Kind, color, frame, gpu};
use glam::{DQuat, DVec3};
use serde_json::{Map, Value, json};
use spherecut_core::video::{ChromaSiting, ColorInfo, Matrix, Range, SampleBytes};
use spherecut_render::reference::{self, ImageF64};
use spherecut_render::{
    AdapterChoice, DebugMode, Depth, Gpu, Image, Pixels, Placement, Stereo, StillOptions,
    StillRenderer, ViewParams,
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
/// Set to bless.
const UPDATE: &str = "SPHERECUT_UPDATE_GOLDENS";
/// What rewrites the goldens.
const REGENERATE: &str = "SPHERECUT_UPDATE_GOLDENS=1 cargo test -p spherecut-render --test goldens";

/// The other-driver rule: 48.13 dB is an RMS error of one 8-bit step.
const OTHER_PSNR_DB: f64 = 48.0;
const OTHER_SSIM: f64 = 0.995;
const OTHER_MAX_LSB: f64 = 4.0;
/// The CPU tolerance: measured − 3 dB when blessing, never below 36.
const CPU_MARGIN_DB: f64 = 3.0;
const CPU_FLOOR_DB: f64 = 36.0;
/// No channel may be further than this from the CPU reference, which a
/// seam column or a misplaced rim would be (as in `tests/render.rs`).
const CPU_MAX_CHANNEL: f64 = 0.1;
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
/// Seams and poles: a pixel is bad past 2/255, and the implicit-LOD
/// control must give more than 100 of them.
const TWIN_THRESHOLD: f64 = 2.0 / 255.0;
const CONTROL_MIN_BAD: usize = 100;

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
/// LOD of luma before clamping.
fn minified(f: &Frame, views: &[ViewParams], size: (u32, u32)) -> Vec<bool> {
    let mut mask = vec![true; size.0 as usize * size.1 as usize];
    for v in views {
        let lod = reference::lod_map(f, v, size, &Placement::default()).unwrap();
        for (m, l) in mask.iter_mut().zip(lod.data.chunks(3)) {
            *m &= l[2] >= IDEAL_MIN_LOD;
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
    // In whole 8-bit steps when both are 8-bit, as the rule counts them.
    let max_lsb = match (&got.pixels, &want.pixels) {
        (Pixels::Rgb8(x), Pixels::Rgb8(y)) => x
            .iter()
            .zip(y)
            .map(|(p, q)| p.abs_diff(*q))
            .max()
            .map_or(0.0, f64::from),
        _ => max_diff(&a, &b) * 255.0,
    };
    Ok(Drift {
        identical: got.pixels == want.pixels,
        psnr: psnr_of(size, &a, &b),
        ssim: ssim_of(size, &a, &b),
        max_lsb,
    })
}

/// Section 6's rule for a stored image: identical at the driver build it
/// was blessed on, else within the other-driver tolerance.
fn judge(d: &Drift, same_driver: bool) -> Result<(), String> {
    let numbers = format!(
        "PSNR {:.2} dB, SSIM {:.5}, max {:.0} LSB",
        d.psnr, d.ssim, d.max_lsb
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
    if d.psnr >= OTHER_PSNR_DB && d.ssim >= OTHER_SSIM && d.max_lsb <= OTHER_MAX_LSB {
        Ok(())
    } else {
        Err(format!(
            "outside the other-driver tolerance (PSNR ≥ {OTHER_PSNR_DB} dB, SSIM ≥ {OTHER_SSIM}, \
             max ≤ {OTHER_MAX_LSB} LSB): {numbers}"
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
    fn calibrated(&mut self, key: &str, v: f64, margin: f64, floor: f64) -> bool {
        let tkey = format!("{key}_min");
        let min = if self.blessing() {
            (((v - margin) * 100.0).floor() / 100.0).max(floor)
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

    /// `got` against the CPU's `cpu`: the calibrated PSNR floor, and no
    /// channel off by more than [`CPU_MAX_CHANNEL`].
    fn cpu_check(&mut self, key: &str, got: &Image, cpu: &ImageF64) {
        let size = (got.width, got.height);
        let g = unit(got);
        let p = psnr_of(size, &g, &cpu.data);
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

    /// Ends a row that stores no image.
    fn end_check(&mut self, what: &str, source: Option<&Source>, view: Option<Value>) {
        let mut m = self.stamp();
        m.insert("what".into(), json!(what));
        if let Some(s) = source {
            m.insert("source".into(), s.about.clone());
            m.insert("source_fnv64".into(), json!(format!("{:016x}", s.hash())));
        }
        if let Some(v) = view {
            m.insert("view".into(), v);
        }
        self.end(m, false);
    }

    fn end(&mut self, entry: Map<String, Value>, image: bool) {
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
            json!({"psnr_db_min": OTHER_PSNR_DB, "ssim_min": OTHER_SSIM, "max_lsb": OTHER_MAX_LSB}),
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
        eprintln!(
            "blessed {} images and {} checks into {}",
            self.pngs.len(),
            self.checks.len(),
            dir.display()
        );
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

/// A stored view of the grid, checked against the CPU: the first rows.
fn stored_grid_view(
    run: &mut Run<'_>,
    what: &str,
    v: ViewParams,
    size: (u32, u32),
) -> Option<Image> {
    let s = grid_nv12();
    let got = run.render_with(&s.frame, &v, size, DebugMode::Off)?;
    match reference::render_trilinear(&s.frame, &v, size) {
        Ok(cpu) => run.cpu_check("cpu", &got, &cpu),
        Err(e) => run.fail(format!("the CPU reference: {e}")),
    }
    run.stored(what, &got, &s, view_about(&v, size));
    Some(got)
}

fn rect_forward(run: &mut Run<'_>) {
    let v = ViewParams::new(DQuat::IDENTITY, lens(Projection::Rectilinear, 90.0));
    stored_grid_view(run, "the grid, rectilinear 90°, looking forward", v, RECT);
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

fn rect_ypr(run: &mut Run<'_>) {
    let (yaw, pitch, roll) = (30.0, 20.0, 10.0);
    let v = ViewParams::new(ypr(yaw, pitch, roll), lens(Projection::Rectilinear, 90.0));
    let Some(got) = stored_grid_view(
        run,
        "the grid, rectilinear 90°, yaw 30, pitch 20, roll 10",
        v,
        RECT,
    ) else {
        return;
    };
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

fn fisheye_180(run: &mut Run<'_>) {
    // Looking back, so the seam runs through the middle.
    let v = ViewParams::new(ypr(180.0, 0.0, 0.0), lens(Projection::Fisheye, 180.0));
    if let Some(got) = stored_grid_view(
        run,
        "the grid, fisheye 180°, looking back across the seam",
        v,
        SQUARE,
    ) {
        black_past_pi(run, &got, &v);
    }
}

fn fisheye_360(run: &mut Run<'_>) {
    let v = ViewParams::new(ypr(90.0, 30.0, 0.0), lens(Projection::Fisheye, 360.0));
    if let Some(got) = stored_grid_view(run, "the grid, fisheye 360°, yaw 90, pitch 30", v, SQUARE)
    {
        black_past_pi(run, &got, &v);
        let past = run.measured["pixels_past_pi"].as_f64().unwrap_or(0.0);
        run.at_least("pixels_past_pi_checked", past, 1.0);
    }
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

fn stereo_tinyplanet(run: &mut Run<'_>) {
    let v = ViewParams::new(ypr(0.0, -90.0, 0.0), lens(Projection::Stereographic, 300.0));
    let s = grid_nv12();
    let Some(got) = stored_grid_view(
        run,
        "the grid, tiny planet (stereographic 300°, pitch −90)",
        v,
        PLANET,
    ) else {
        return;
    };
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
    ideal_check(run, &mask, &got, &level0, &ideal);
    // The stored row ended with `stored`; record the ideal's numbers too.
    let entry = run
        .images
        .get_mut("stereo_tinyplanet")
        .and_then(Value::as_object_mut);
    if let Some(e) = entry {
        e.insert("thresholds".into(), Value::Object(run.thresholds.clone()));
        e.insert("measured".into(), Value::Object(run.measured.clone()));
    }
}

fn morph_half(run: &mut Run<'_>) {
    let l = Lens {
        from: Projection::Rectilinear,
        to: Projection::Stereographic,
        morph: 0.5,
        hfov: 150f64.to_radians(),
    };
    let v = ViewParams::new(ypr(0.0, -10.0, 0.0), l);
    stored_grid_view(
        run,
        "the grid, rectilinear to stereographic at 0.5, 150°",
        v,
        RECT,
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
            run.at_least(key, d.count_over as f64, CONTROL_MIN_BAD as f64)
        };
        if !ok {
            run.write_diffs(key, size, &ua, &ub);
        }
    }
    run.threshold("bad_pixel_over", num(TWIN_THRESHOLD));
    run.end_check(
        "against its roll twin, with the implicit-LOD control",
        Some(s),
        Some(view_about(v, size)),
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
        Some(&s),
        Some(view_about(&v, size)),
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
    let p010 = coded(Kind::P010, size, limited709(), code);
    let i010 = coded(Kind::I420p10, size, limited709(), code);
    let v = ViewParams::new(DQuat::IDENTITY, lens(Projection::Rectilinear, 20.0));
    let opts = StillOptions {
        depth: Depth::Sixteen,
        ..StillOptions::default()
    };
    let (Some(a), Some(b)) = (
        run.render(&p010, &v, RECT, opts),
        run.render(&i010, &v, RECT, opts),
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
    match reference::render_trilinear(&p010, &v, RECT) {
        Ok(cpu) => {
            let m = max_diff(&unit(&a), &cpu.data);
            run.at_most("cpu_max_diff_10bit_codes", m * 1023.0, 2.0);
        }
        Err(e) => run.fail(format!("the CPU reference: {e}")),
    }
    let s = Source::new("ramp", p010);
    run.end_check(
        "a 10-bit ramp as P010 and yuv420p10, magnified, read back at 16 bits",
        Some(&s),
        Some(view_about(&v, RECT)),
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
    let s = Source::new("grid", frame(img, Kind::Nv12, limited709(), 0));
    run.end_check(
        "one picture as NV12 and I420, rows padded to 64 and 256 bytes and 4109 bytes apart",
        Some(&s),
        Some(view_about(&v, RECT)),
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
/// and the unorm8 store, to nearest. The GPU does each step in f32, so
/// where a value lies within its error of a tie (an f16 tie, or a half
/// code) either neighbour may come out: the second value is the other
/// candidate, when there is one.
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
    let mut found: BTreeMap<(bool, u8), (u8, usize)> = BTreeMap::new();
    let mut wrong = Vec::new();
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
        let out = (512u32, 8u32);
        for m in 0..8 {
            let v = ViewParams::new(
                ypr(22.5 + 45.0 * f64::from(m), 0.0, 0.0),
                lens(Projection::Rectilinear, 45.0),
            );
            let Some(got) = run.render_with(&f, &v, out, DebugMode::Off) else {
                return;
            };
            for j in 0..out.1 {
                for i in 0..out.0 {
                    let c = pixel::centre(i, j);
                    let u = reference::sphere_uv(reference::sphere_dir(&v, out, c.x, c.y)).x;
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
                // The bound: the f16 store moves the linear value by at
                // most 2^−11 of itself, which the OETF turns into at most
                // (1.055 / 2.4) · 2^−11 of full scale, 0.0547 codes. So
                // the output is round((k − 16) · 255 / 219) unless that
                // lies within 0.0547 codes of a tie, and one code off at
                // most where it does.
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
        None,
        None,
    );
}

fn range_full_vs_limited(run: &mut Run<'_>) {
    // At 10 bits, whose planes are f16 textures that the GPU filters in
    // floating point. lavapipe filters 8-bit planes in 8-bit fixed point,
    // to whole codes (D32), which on top of the two codings' own rounding
    // puts an NV12 pair up to 4 LSB apart: that is recorded, and its
    // control asserted too.
    let v = ViewParams::new(DQuat::IDENTITY, lens(Projection::Rectilinear, 90.0));
    let mut source = None;
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
        run.at_least(
            &format!("{key}control_full_as_limited_max_lsb"),
            max_diff(&ua, &uc) * 255.0,
            10.0,
        );
        if kind == Kind::P010 {
            source = Some(Source::new("grid", full));
        }
    }
    run.end_check(
        "the grid coded at full and at limited range, 10-bit (8-bit recorded); control: full decoded as limited",
        source.as_ref(),
        Some(view_about(&v, RECT)),
    );
}

fn matrix_601_709(run: &mut Run<'_>) {
    let v = ViewParams::new(ypr(-30.0, 15.0, 0.0), lens(Projection::Rectilinear, 90.0));
    let mut psnrs = BTreeMap::new();
    for (name, m) in [("601", Matrix::Bt601), ("709", Matrix::Bt709)] {
        let f = frame(
            grid(),
            Kind::Nv12,
            color(Range::Limited, m, ChromaSiting::Left),
            0,
        );
        let (Some(got), Ok(cpu)) = (
            run.render_with(&f, &v, RECT, DebugMode::Off),
            reference::render_trilinear(&f, &v, RECT),
        ) else {
            run.fail("render or the CPU reference failed");
            return;
        };
        run.cpu_check(&format!("cpu_{name}"), &got, &cpu);
        psnrs.insert(name, (f, cpu));
    }
    // Control: the BT.601 frame decoded as BT.709 misses its own CPU
    // reference by far more than the tolerance's 3 dB.
    let (f601, cpu601) = &psnrs["601"];
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
    run.end_check(
        "the grid coded with BT.601 and with BT.709, each against the CPU; control: 601 decoded as 709",
        None,
        Some(view_about(&v, RECT)),
    );
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
        }
        // Control: the Left-sited frame decoded as Center (the planes are
        // the same; only the tag differs) misses Left's CPU reference by
        // far.
        run.at_least(
            &format!("{key}control_left_as_center_max_lsb"),
            max_diff(&gpus["center"], &cpus["left"].data) * 255.0,
            10.0,
        );
    }
    run.end_check(
        "red and blue stripes 5 texels wide, Left and Center sited, magnified, 10-bit (8-bit recorded); \
         control: Left as Center",
        None,
        Some(view_about(&v, RECT)),
    );
}

fn crop_half(run: &mut Run<'_>) {
    // A 180° panorama: the grid's middle half, with sv3d bounds cropping
    // a quarter of the sphere's width off each side.
    let g = grid();
    let half = RgbImage {
        width: 512,
        height: 512,
        pixels: (0..512 * 512)
            .map(|i| g.get(256 + i % 512, i / 512))
            .collect(),
    };
    let f = frame(&half, Kind::Nv12, limited709(), 0);
    let placement = Placement {
        left: 0.25,
        right: 0.25,
        top: 0.0,
        bottom: 0.0,
        stereo: Stereo::Mono,
    };
    let v = ViewParams::new(ypr(60.0, 0.0, 0.0), lens(Projection::Rectilinear, 120.0));
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
        Ok(cpu) => run.cpu_check("cpu", &got, &cpu),
        Err(e) => run.fail(format!("the CPU reference: {e}")),
    }
    let s = Source::new("grid, the middle half", f);
    run.end_check(
        "a 180° panorama by sv3d bounds, seen past its edge",
        Some(&s),
        Some(view_about(&v, RECT)),
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
        Some(&s),
        Some(view_about(&v, SQUARE)),
    );
}

// ---------------------------------------------------------------------
// The tests

#[test]
fn goldens() {
    let gpu = gpu();
    assert_eq!(gpu.caps.info.device_type, wgpu::DeviceType::Cpu);
    let bless = std::env::var_os(UPDATE).is_some_and(|v| !v.is_empty() && v != "0");
    let _ = std::fs::remove_dir_all(diff_dir());
    let mut run = Run::new(gpu, Mode::Golden { bless });
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
    let small = Image {
        width: 32,
        ..want.clone()
    };
    assert!(drift(&small, &want).is_err());
}

/// A scratch goldens directory and its diffs, for the harness's own
/// tests, emptied first.
fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("goldens-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let diffs = dir.join("diffs");
    (dir, diffs)
}

/// Blesses `rows` into `dir`.
fn bless_into(dir: &Path, diffs: &Path, rows: &[&str]) {
    let mut run = Run::at(gpu(), Mode::Golden { bless: true }, dir, diffs);
    run_rows(&mut run, Some(rows));
    run.finish();
}

#[test]
fn blessing_writes_everything_or_nothing() {
    let (dir, diffs) = scratch("bless");
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
fn a_changed_source_is_not_compared_as_a_render() {
    let (dir, diffs) = scratch("source");
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

#[test]
fn the_reference_only_path_runs_on_lavapipe() {
    // goldens_on_gpu_match_reference's path, on two rows.
    let mut run = Run::new(gpu(), Mode::References);
    run_rows(&mut run, Some(&["rect_forward", "tenbit_ramp"]));
    assert_eq!(run.notes.len(), 2, "{:?}", run.notes);
    assert!(run.pngs.is_empty());
    run.finish();
}

#[test]
fn the_other_driver_path_runs_on_lavapipe() {
    // goldens_on_host_llvmpipe's path, on one stored image: the
    // other-driver rule whatever the build, with the drift noted.
    let mut run = Run::new(gpu(), Mode::OtherDriver);
    run_rows(&mut run, Some(&["rect_forward"]));
    assert!(run.notes[0].contains("golden_psnr_db"), "{:?}", run.notes);
    run.finish();
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

/// Every Vulkan adapter that isn't a CPU, each as its own device.
fn hardware_gpus() -> (Vec<Gpu>, Vec<String>) {
    let instance = wgpu::Instance::new(Gpu::instance_descriptor());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
    let mut gpus = Vec::new();
    let mut seen = Vec::new();
    for a in adapters {
        let info = a.get_info();
        seen.push(format!(
            "{} ({:?}, {})",
            info.name, info.device_type, info.driver_info
        ));
        if info.device_type == wgpu::DeviceType::Cpu {
            continue;
        }
        let name = if info.device_pci_bus_id.is_empty() {
            info.name.clone()
        } else {
            info.device_pci_bus_id.clone()
        };
        match Gpu::headless(&AdapterChoice::Named(name)) {
            Ok(g) => gpus.push(g),
            Err(e) => panic!("{} won't open a device: {e}", info.name),
        }
    }
    (gpus, seen)
}

#[test]
#[ignore = "tier2: needs a GPU (RADV or ANV)"]
fn goldens_on_gpu_match_reference() {
    tier2::require_hw_run();
    let (gpus, seen) = hardware_gpus();
    if gpus.is_empty() {
        tier2::skip(format!("no GPU: Vulkan lists only {}", seen.join(", ")));
    }
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
