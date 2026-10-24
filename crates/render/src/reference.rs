//! The CPU reference (M1 plan section 4.5): the projection pass in f64,
//! with no GPU. It follows `shaders/project.wgsl` step by step, and
//! stores each mip level and the linear target as the GPU stores them:
//! unorm8 rounded to nearest, or the nearest f16, ties to even, which the
//! shaders round to themselves so that no driver's conversion changes it
//! (`round_f16` in `shaders/common.wgsl`). What remains between the two
//! is the GPU's own precision: its `atan2`, its filtering weights and its
//! LOD fraction.
//! The Tier 1 tests hold `render_still` to it at calibrated tolerances.

use std::f64::consts::{PI, TAU};

use glam::{DVec2, DVec3};
use spherecut_core::video::{Transfer, VideoPlanes};
use spherecut_sphere::{equirect, pixel};

use crate::color::{self, Placement};
use crate::error::RenderError;
use crate::plane::{LayoutKind, PlaneFormat, SlotShape};
use crate::project::ViewParams;
use crate::readback::round_f16;

/// A rendered picture in f64: R, G, B per pixel, rows top to bottom,
/// as the encode pass computes them before it stores them.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageF64 {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f64>,
}

impl ImageF64 {
    pub fn at(&self, x: u32, y: u32) -> [f64; 3] {
        let i = 3 * (y as usize * self.width as usize + x as usize);
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }
}

/// How the reference renders.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Options {
    /// Level 0 only, as `DebugMode::Level0`.
    pub level0: bool,
    pub placement: Placement,
}

/// The world direction through point `(x, y)` of a `size` view, in
/// pixels from the top-left corner (a pixel's centre is at i + 0.5).
pub fn sphere_dir(view: &ViewParams, size: (u32, u32), x: f64, y: f64) -> DVec3 {
    let p = pixel::to_lens(size, x, y);
    (view.orientation * view.lens.unproject(p)).normalize()
}

/// Pixel `(i, j)`'s footprint in sphere uv: how uv changes across one
/// pixel to the right and one pixel down, from central differences of
/// the direction half a pixel either side, as the shader computes it.
pub fn uv_footprint(view: &ViewParams, size: (u32, u32), i: u32, j: u32) -> (DVec2, DVec2) {
    let c = pixel::centre(i, j);
    let d = sphere_dir(view, size, c.x, c.y);
    let across = |dx: f64, dy: f64| {
        sphere_dir(view, size, c.x + dx, c.y + dy) - sphere_dir(view, size, c.x - dx, c.y - dy)
    };
    (
        equirect::uv_gradient(d, across(0.5, 0.0)),
        equirect::uv_gradient(d, across(0.0, 0.5)),
    )
}

/// The shader's sphere uv of `d`: u in [0, 1] and the latitude by
/// `atan2` (`uv_of` in `shaders/project.wgsl`).
pub fn sphere_uv(d: DVec3) -> DVec2 {
    DVec2::new(
        0.5 + d.x.atan2(-d.z) / TAU,
        0.5 - d.y.atan2(d.x.hypot(d.z)) / PI,
    )
}

/// What `render_still` renders with `Filter::Isotropic`: the same
/// central differences, LOD, area mips and bilinear taps.
pub fn render_trilinear(
    frame: &dyn VideoPlanes,
    view: &ViewParams,
    size: (u32, u32),
) -> Result<ImageF64, RenderError> {
    render(frame, view, size, &Options::default())
}

/// What `render_still` renders with `DebugMode::Level0`.
pub fn render_level0(
    frame: &dyn VideoPlanes,
    view: &ViewParams,
    size: (u32, u32),
) -> Result<ImageF64, RenderError> {
    render(
        frame,
        view,
        size,
        &Options {
            level0: true,
            ..Options::default()
        },
    )
}

/// `frame` through `view`, `size` pixels large, as the encode pass
/// computes it.
pub fn render(
    frame: &dyn VideoPlanes,
    view: &ViewParams,
    size: (u32, u32),
    opts: &Options,
) -> Result<ImageF64, RenderError> {
    let source = Source::new(frame, &opts.placement)?;
    let linear = frame.color().transfer == Transfer::Linear;
    Ok(per_pixel(size, |px| {
        let s = source.footprint(view, size, px);
        let lods = if opts.level0 {
            [0.0, 0.0]
        } else {
            [
                s.lod[0].clamp(0.0, s.levels[0] - 1.0),
                s.lod[1].clamp(0.0, s.levels[1] - 1.0),
            ]
        };
        let y = source.planes[0].sample(s.uv, lods[0], source.wraps);
        let c1 = source
            .planes
            .get(1)
            .map_or([0.0; 2], |p| p.sample(s.cuv, lods[1], source.wraps));
        let c2 = source
            .planes
            .get(2)
            .map_or([0.0; 2], |p| p.sample(s.cuv, lods[1], source.wraps));
        let samples = [
            y[0],
            c1[0],
            if source.kind == LayoutKind::SemiPlanar {
                c1[1]
            } else {
                c2[0]
            },
            1.0,
        ];
        let rgb = source.matrix.map(|row| {
            (0..4)
                .map(|i| row[i] * samples[i])
                .sum::<f64>()
                .clamp(0.0, 1.0)
        });
        if !s.inside {
            return [0.0; 3];
        }
        rgb.map(|v| {
            // The linear target is f16; the encode pass undoes the EOTF.
            let lin = round_f16(if linear { v } else { srgb_eotf(v) });
            srgb_oetf(lin.clamp(0.0, 1.0))
        })
    }))
}

/// What `DebugMode::LodHeatmap` writes, read back with `Depth::Linear`:
/// luma's and chroma's LOD as sampled, then luma's before clamping.
pub fn lod_map(
    frame: &dyn VideoPlanes,
    view: &ViewParams,
    size: (u32, u32),
    placement: &Placement,
) -> Result<ImageF64, RenderError> {
    let source = Source::new(frame, placement)?;
    Ok(per_pixel(size, |px| {
        let s = source.footprint(view, size, px);
        [
            s.lod[0].clamp(0.0, s.levels[0] - 1.0),
            s.lod[1].clamp(0.0, s.levels[1] - 1.0),
            s.lod[0],
        ]
        .map(round_f16)
    }))
}

fn per_pixel(size: (u32, u32), f: impl Fn((u32, u32)) -> [f64; 3]) -> ImageF64 {
    let mut data = Vec::with_capacity(3 * size.0 as usize * size.1 as usize);
    for j in 0..size.1 {
        for i in 0..size.0 {
            data.extend_from_slice(&f((i, j)));
        }
    }
    ImageF64 {
        width: size.0,
        height: size.1,
        data,
    }
}

/// A frame as the shader sees it: its planes' mip chains as the GPU
/// stores them, and its `Source` uniform in f64.
struct Source {
    kind: LayoutKind,
    planes: Vec<Pyramid>,
    matrix: [[f64; 4]; 3],
    cover: [f64; 4],
    uv_scale: DVec2,
    uv_offset: DVec2,
    chroma_scale: DVec2,
    chroma_offset: DVec2,
    size0: DVec2,
    size1: DVec2,
    levels: [f64; 2],
    wraps: bool,
}

/// Where one output pixel samples.
struct Sampled {
    uv: DVec2,
    cuv: DVec2,
    lod: [f64; 2],
    levels: [f64; 2],
    inside: bool,
}

impl Source {
    fn new(frame: &dyn VideoPlanes, placement: &Placement) -> Result<Source, RenderError> {
        let shape = SlotShape::of(frame.layout(), frame.size())?;
        let mut planes = Vec::with_capacity(shape.planes.len());
        for (i, p) in shape.planes.iter().enumerate() {
            let data = frame.plane(i).ok_or_else(|| {
                RenderError::Invalid(format!("{} has no plane {i}", frame.layout().name))
            })?;
            let mut level = Level::load(&data, p.format, p.size, &shape, i)?;
            let mut levels = Vec::with_capacity(p.levels as usize);
            for _ in 1..p.levels {
                let next = level.halve(p.format);
                levels.push(level);
                level = next;
            }
            levels.push(level);
            planes.push(Pyramid { levels });
        }
        let u = color::source_uniform(&shape, frame.color(), placement);
        let (uv_scale, uv_offset) = placement.uv_transform();
        let subsampled = matches!(shape.kind, LayoutKind::SemiPlanar | LayoutKind::Planar);
        let (cs, co) = if subsampled {
            color::chroma_transform(&shape, frame.color().siting)
        } else {
            ([1.0, 1.0], [0.0, 0.0])
        };
        let luma = shape.planes[0];
        let chroma = shape.planes.get(1).copied().unwrap_or(luma);
        let size =
            |p: crate::plane::PlaneShape| DVec2::new(f64::from(p.size.0), f64::from(p.size.1));
        Ok(Source {
            kind: shape.kind,
            planes,
            matrix: color::fold(frame.color(), shape.kind, shape.bit_depth),
            cover: placement.cover(),
            uv_scale: DVec2::from(uv_scale),
            uv_offset: DVec2::from(uv_offset),
            chroma_scale: DVec2::from(cs),
            chroma_offset: DVec2::from(co),
            size0: size(luma),
            size1: size(chroma),
            levels: u.levels.map(f64::from),
            wraps: placement.wraps(),
        })
    }

    fn footprint(&self, view: &ViewParams, size: (u32, u32), (i, j): (u32, u32)) -> Sampled {
        let c = pixel::centre(i, j);
        let d = sphere_dir(view, size, c.x, c.y);
        let theta = view.lens.theta(pixel::to_lens(size, c.x, c.y).length());
        let (gx, gy) = uv_footprint(view, size, i, j);
        let suv = sphere_uv(d);
        let uv = suv * self.uv_scale + self.uv_offset;
        let g = [gx * self.uv_scale, gy * self.uv_scale];
        let gc = g.map(|v| v * self.chroma_scale);
        let lod = |g: [DVec2; 2], size: DVec2| {
            (g[0] * size)
                .length()
                .max((g[1] * size).length())
                .max(1e-30)
                .log2()
        };
        let [u0, v0, u1, v1] = self.cover;
        Sampled {
            uv,
            cuv: uv * self.chroma_scale + self.chroma_offset,
            lod: [lod(g, self.size0), lod(gc, self.size1)],
            levels: self.levels,
            inside: theta <= PI + 1e-4 && suv.x >= u0 && suv.y >= v0 && suv.x <= u1 && suv.y <= v1,
        }
    }
}

/// One plane's mip chain.
struct Pyramid {
    levels: Vec<Level>,
}

impl Pyramid {
    /// Trilinear at `lod`, already clamped to the chain: what a sampler
    /// with linear mag, min and mip filters gives.
    fn sample(&self, uv: DVec2, lod: f64, wrap_u: bool) -> [f64; 2] {
        let last = self.levels.len() - 1;
        let l0 = (lod.floor().max(0.0) as usize).min(last);
        let l1 = (l0 + 1).min(last);
        let t = (lod - l0 as f64).clamp(0.0, 1.0);
        let a = self.levels[l0].bilinear(uv, wrap_u);
        if t == 0.0 || l1 == l0 {
            return a;
        }
        let b = self.levels[l1].bilinear(uv, wrap_u);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }
}

/// One mip level of a plane, each texel's one or two components as the
/// texture holds them.
struct Level {
    w: u32,
    h: u32,
    comps: usize,
    data: Vec<f64>,
}

impl Level {
    /// Level 0 as the upload leaves it: 8-bit codes / 255, or a deep
    /// plane's codes shifted, clamped, over 2^b − 1 and rounded to f16.
    fn load(
        data: &spherecut_core::video::PlaneData<'_>,
        format: PlaneFormat,
        (w, h): (u32, u32),
        shape: &SlotShape,
        i: usize,
    ) -> Result<Level, RenderError> {
        let comps = format.components() as usize;
        let row_bytes = w as usize * format.bytes_per_texel() as usize;
        let shift = if shape.msb_aligned {
            16 - u32::from(shape.bit_depth)
        } else {
            0
        };
        let max = (1u32 << shape.bit_depth) - 1;
        let mut out = Vec::with_capacity(w as usize * h as usize * comps);
        for y in 0..h {
            let row = data.row(y, row_bytes).ok_or_else(|| {
                RenderError::Invalid(format!("plane {i} is shorter than {w}x{h}"))
            })?;
            if format.is_deep() {
                out.extend(row.chunks_exact(2).map(|b| {
                    let code = (u32::from(u16::from_le_bytes([b[0], b[1]])) >> shift).min(max);
                    round_f16(f64::from(code) / f64::from(max))
                }));
            } else {
                out.extend(row.iter().map(|&b| f64::from(b) / 255.0));
            }
        }
        Ok(Level {
            w,
            h,
            comps,
            data: out,
        })
    }

    /// The next level down, as the mip pass makes it: each destination
    /// texel the area-weighted mean of the source texels it covers, then
    /// stored in the plane's format.
    fn halve(&self, format: PlaneFormat) -> Level {
        let (sw, sh) = (self.w, self.h);
        let (dw, dh) = ((sw / 2).max(1), (sh / 2).max(1));
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
                    let mut acc = 0.0;
                    for &(y, a) in &wy {
                        for &(x, b) in &wx {
                            acc += a * b * self.get(x, y, c);
                        }
                    }
                    data.push(store(format, acc));
                }
            }
        }
        Level {
            w: dw,
            h: dh,
            comps: self.comps,
            data,
        }
    }

    fn get(&self, x: u32, y: u32, c: usize) -> f64 {
        self.data[(y as usize * self.w as usize + x as usize) * self.comps + c]
    }

    /// The four nearest texels' bilinear blend at `uv`, texel centres at
    /// (i + 0.5) / w: U repeats or clamps, V clamps.
    fn bilinear(&self, uv: DVec2, wrap_u: bool) -> [f64; 2] {
        let (w, h) = (i64::from(self.w), i64::from(self.h));
        let x = uv.x * self.w as f64 - 0.5;
        let y = uv.y * self.h as f64 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (a, b) = (x - x0, y - y0);
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
            let t = |i: i64, j: i64| self.get(col(i) as u32, row(j) as u32, c);
            let top = t(x0, y0) * (1.0 - a) + t(x0 + 1, y0) * a;
            let bottom = t(x0, y0 + 1) * (1.0 - a) + t(x0 + 1, y0 + 1) * a;
            *o = top * (1.0 - b) + bottom * b;
        }
        out
    }
}

/// A value as a texture of `format` stores it.
fn store(format: PlaneFormat, v: f64) -> f64 {
    match format {
        PlaneFormat::R8 | PlaneFormat::Rg8 => (v.clamp(0.0, 1.0) * 255.0).round() / 255.0,
        PlaneFormat::R16F | PlaneFormat::Rg16F => round_f16(v),
    }
}

fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_oetf(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(w: u32, h: u32, f: impl Fn(u32, u32) -> f64) -> Level {
        Level {
            w,
            h,
            comps: 1,
            data: (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| f(x, y))
                .collect(),
        }
    }

    #[test]
    fn halving_keeps_the_mean_at_odd_sizes() {
        // Weights over a 5-texel row into 2: 1, 1, ½ and ½, 1, 1, of 2.5.
        let l = level(5, 3, |x, y| f64::from(x * 3 + y));
        let mean = l.data.iter().sum::<f64>() / l.data.len() as f64;
        let h = l.halve(PlaneFormat::R16F);
        assert_eq!((h.w, h.h), (2, 1));
        let got = h.data.iter().sum::<f64>() / 2.0;
        assert!((got - mean).abs() < 1e-2, "{got} {mean}");
        // Texel 0 covers columns 0, 1 and half of 2, and all three rows,
        // since 3 rows halve to 1: the exact mean of its area.
        let want = (0..3)
            .flat_map(|x| (0..3).map(move |y| (x, y)))
            .map(|(x, y)| {
                let wx = if x == 2 { 0.5 } else { 1.0 };
                wx * f64::from(x * 3 + y)
            })
            .sum::<f64>()
            / (2.5 * 3.0);
        assert!(
            (h.data[0] - round_f16(want)).abs() < 1e-12,
            "{} {want}",
            h.data[0]
        );
    }

    #[test]
    fn bilinear_wraps_u_and_clamps_v() {
        let l = level(4, 2, |x, y| f64::from(x + 10 * y));
        // Texel centres come back exactly.
        assert_eq!(l.bilinear(DVec2::new(0.375, 0.25), true)[0], 1.0);
        // Halfway across the seam: columns 3 and 0.
        assert_eq!(l.bilinear(DVec2::new(0.0, 0.25), true)[0], 1.5);
        assert_eq!(l.bilinear(DVec2::new(0.0, 0.25), false)[0], 0.0);
        // Past the top row, V clamps.
        assert_eq!(l.bilinear(DVec2::new(0.125, 0.0), true)[0], 0.0);
        assert_eq!(l.bilinear(DVec2::new(0.125, 1.0), true)[0], 10.0);
    }

    #[test]
    fn unorm_levels_store_codes() {
        assert_eq!(store(PlaneFormat::R8, 0.5), 128.0 / 255.0);
        assert_eq!(store(PlaneFormat::Rg8, 1.2), 1.0);
        assert_eq!(store(PlaneFormat::R16F, 0.1), round_f16(0.1));
    }
}
