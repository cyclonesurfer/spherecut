//! The CPU reference (M1 plan section 4.5): the projection pass in f64,
//! with no GPU. It follows `shaders/project.wgsl` step by step, and
//! stores each mip level and the linear target as the GPU stores them:
//! an 8-bit level as the exact area mean of the codes rounded to the
//! nearest code ([`halve_codes`]), the rest as the nearest f16, both with
//! ties to even, which the shaders round to themselves so that no
//! driver's conversion changes them (`shaders/mips.wgsl`, and `round_f16`
//! in `shaders/common.wgsl`). What remains between the two is the GPU's
//! own precision: its `atan2`, its filtering weights and its LOD
//! fraction.
//! The Tier 1 tests hold `render_still` to it at calibrated tolerances.
//!
//! [`render_rig`] is the stitch of a lens pair the same way (M3 plan
//! 10.3), written before the shader's, which is held to it in turn.

use std::f64::consts::{PI, TAU};

use glam::{DVec2, DVec3};
use spherecut_core::video::{Transfer, VideoPlanes};
use spherecut_sphere::rig::Rig;
use spherecut_sphere::{equirect, pixel};

use crate::color::{self, Placement};
use crate::error::RenderError;
use crate::plane::{LayoutKind, PlaneFormat, SlotShape};
use crate::project::{DebugMode, ViewParams};
use crate::readback::round_f16;

/// A rendered picture in f64: R, G, B per pixel, rows top to bottom,
/// as the encode pass computes them before it rounds them to codes.
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

/// What [`render_rig`] writes instead of the stitched picture, for checks:
/// M3 plan 10.3's debug modes 4 to 7, which are [`crate::DebugMode`]'s
/// for a lens pair. [`RigDebug::mode`] and [`crate::DebugMode::rig`] join
/// the two; this one holds no equirect mode, which the reference draws
/// through [`Options`] (and level 0 through [`RigOptions::level0`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RigDebug {
    #[default]
    Off,
    /// Mode 4: calibration block 0's lens alone, wherever its mask covers,
    /// with no feather; black elsewhere.
    Lens0,
    /// Mode 5: block 1's lens alone.
    Lens1,
    /// Mode 6: the weights the lenses blend by, block 0's in red and
    /// block 1's in blue.
    Weights,
    /// Mode 7: the seam difference, |g0·lin0 − g1·lin1| × 8 in linear
    /// light, the frame's gains applied, where both lenses cover; black
    /// elsewhere.
    SeamDiff,
}

impl RigDebug {
    /// The shader's debug mode that draws this one: `Off`, or modes 4 to
    /// 7. The inverse of [`crate::DebugMode::rig`].
    pub fn mode(self) -> DebugMode {
        match self {
            RigDebug::Off => DebugMode::Off,
            RigDebug::Lens0 => DebugMode::Lens0,
            RigDebug::Lens1 => DebugMode::Lens1,
            RigDebug::Weights => DebugMode::Weights,
            RigDebug::SeamDiff => DebugMode::SeamDiff,
        }
    }
}

/// How [`render_rig`] renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RigOptions {
    pub debug: RigDebug,
    /// Level 0 only, as `DebugMode::Level0`.
    pub level0: bool,
}

impl RigOptions {
    /// The shader's debug mode that renders as these options do: `level0`
    /// alone is [`DebugMode::Level0`], and `debug` alone its own mode.
    /// `Invalid` for level 0 beside a debug mode other than `Off`, which
    /// the shader, one mode a pipeline, has no form for.
    pub fn mode(self) -> Result<DebugMode, RenderError> {
        match (self.level0, self.debug) {
            (false, debug) => Ok(debug.mode()),
            (true, RigDebug::Off) => Ok(DebugMode::Level0),
            (true, debug) => Err(RenderError::Invalid(format!(
                "level 0 beside {debug:?}: the shader draws one debug mode at a time"
            ))),
        }
    }
}

/// The world direction through point `(x, y)` of a `size` view, in
/// pixels from the top-left corner (a pixel's centre is at i + 0.5):
/// [`ViewParams::dir`].
pub fn sphere_dir(view: &ViewParams, size: (u32, u32), x: f64, y: f64) -> DVec3 {
    view.dir(size, x, y)
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
        source.shade(&source.footprint(view, size, px), opts.level0, linear)
    }))
}

/// `frame` through each of `views`, averaged in linear light as
/// [`crate::Blur`] does, `size` pixels large: each view's linear target,
/// their mean as its nearest half, then the OETF, as the encode pass
/// computes it.
pub fn render_blurred(
    frame: &dyn VideoPlanes,
    views: &[ViewParams],
    size: (u32, u32),
    opts: &Options,
) -> Result<ImageF64, RenderError> {
    let n = views.len().max(1) as f64;
    let mut sum = vec![0.0; 3 * size.0 as usize * size.1 as usize];
    for v in views {
        let img = render(frame, v, size, opts)?;
        for (s, d) in sum.iter_mut().zip(&img.data) {
            // The target's half, back from the display value.
            *s += srgb_eotf(*d);
        }
    }
    Ok(ImageF64 {
        width: size.0,
        height: size.1,
        data: sum
            .into_iter()
            .map(|s| srgb_oetf(round_f16(s / n).clamp(0.0, 1.0)))
            .collect(),
    })
}

/// A lens pair stitched through `view`, `size` pixels large, as the encode
/// pass computes it: M3 plan 10.3's stitch in f64 (10.4). `lenses` are the
/// two tracks' pictures, by track, so calibration block b of `rig` samples
/// `lenses[rig.lenses[b].track]`; `gains` are the frame's gains on linear
/// RGB, by block.
///
/// For each output pixel and each lens: the parallax-corrected ray
/// ([`Rig::ray`]) through the lens's model to its slot; a footprint from
/// the pixel's four ±½ px rays, projected through the same lens
/// ([`lens_footprint`]), and the LOD it gives; the mask at that LOD
/// ([`Rig::covers`]: θ_max less the margin, and the frame term); trilinear
/// taps, or level 0 alone under `opts.level0`, which leaves the mask at
/// the footprint's LOD; the slot's own matrix and EOTF; then the lens's
/// gain in linear light. The lenses blend by [`Rig::weights`] at their own
/// LODs, (w0 rgb0 + w1 rgb1)/(w0 + w1), and a lens whose weight is 0 isn't
/// sampled. The weights sum to 1 wherever a lens covers the pixel, a
/// covering lens taking whole what no feather weighs, so the stitch is
/// black only where no lens covers. Then the view's dim, f16 and the
/// OETF, as [`render`] ends. `opts.debug` writes one of [`RigDebug`]'s
/// pictures instead: a lens alone ends as the stitch does, with its gain;
/// the weights and the seam difference are written as [`lod_map`] writes,
/// as the nearest f16 with no dim and no OETF, the weights as
/// [`Rig::weights`] gives them (normalised) and the difference as
/// |g0 lin0 − g1 lin1| × 8 for each channel, the gains applied.
///
/// A lens's model is in the pixels of its track's picture, so a texel
/// becomes a uv over the picture's own size, and a tap past the picture's
/// edge clamps to it; the mask's frame term, which keeps every footprint
/// off that edge, reads the rig's own side for the lens
/// (`RigLens::side`), which [`Rig::build`] takes from the track. Where a
/// lens's model has no image for one of a pixel's five rays the lens has
/// no footprint there, and doesn't cover the pixel. A fisheye view past
/// θ = π is black, as [`render`]'s is. The rows are dealt to threads
/// ([`row_threads`]), and the picture is the same on any number of them.
///
/// What `fs_rig`, the shader's stitch, matches that 10.3 doesn't say: the
/// mask's θ test is made in angle. 10.3 writes it as cos θ ≥ cos(θ_max −
/// margin · 2^λ / texels per radian), and [`Rig::covers`] compares θ with
/// that bound. The two agree while the bound is 0 or more. Once the
/// margin's angle passes θ_max the bound is negative, the cosine is even,
/// and the cosine form covers directions that the angle refuses. On Pat's
/// V3 lens 0 at 768² (174.73 texels a radian at a θ_max of 100°) the
/// bound is below 0 from LOD 7.253, while the frame term still passes the
/// lens's axis up to LOD 7.577 and the slot's middle, half a degree off
/// it, up to 7.584. Of the 401 directions from 0° to 40° off the axis
/// along a meridian, the cosine form covers 14 at LOD 7.272 (a bound of
/// −1.36°) and 108 at 7.4 (−10.77°), whichever the meridian, and 10 to 26
/// at 7.565 (−24.19°), by the meridian, where the angle covers none; at
/// LOD 7.202 both cover 35 and at 7.635 neither any. So `fs_rig` compares
/// θ in angle, and `tests/rig_shader.rs`'s
/// `fs_rig_tests_the_mask_in_angle` holds it at such an LOD, on a 5 × 5
/// output, with the unguarded cosine form as its control.
///
/// This builds both pictures' mip chains and renders once: a caller with
/// more than one picture to make of one pair of frames makes a
/// [`LensPair`] and calls [`render_rig_of`].
///
/// Refuses a gain that isn't finite or is below 0, a rig whose block
/// reads a track other than 0 or 1 or whose blocks both read one track,
/// and planes [`render`] would refuse, in that order: a call refused for
/// its gains or its rig builds no chain.
pub fn render_rig(
    lenses: [&dyn VideoPlanes; 2],
    rig: &Rig,
    gains: [[f64; 3]; 2],
    view: &ViewParams,
    size: (u32, u32),
    opts: &RigOptions,
) -> Result<ImageF64, RenderError> {
    // What `render_rig_of` refuses, before the chains are paid for.
    check_gains(&gains)?;
    check_tracks(rig)?;
    render_rig_of(&LensPair::new(lenses)?, rig, gains, view, size, opts)
}

/// [`render_rig`] of a pair made ready once: the same picture, bit for
/// bit, without the pair's mip chains built again. Everything of the rig
/// (the binding, θ_max, the feather, q), the gains, the view and the
/// options are this call's, so one pair serves any number of rigs and
/// views of its two frames.
///
/// Refuses what [`render_rig`] refuses of the rig and the gains.
pub fn render_rig_of(
    pair: &LensPair,
    rig: &Rig,
    gains: [[f64; 3]; 2],
    view: &ViewParams,
    size: (u32, u32),
    opts: &RigOptions,
) -> Result<ImageF64, RenderError> {
    check_gains(&gains)?;
    check_tracks(rig)?;
    let pixels = per_pixel_rows(size, |px| pair.shade(rig, &gains, view, size, px, opts));
    Ok(ImageF64 {
        width: size.0,
        height: size.1,
        data: pixels.into_iter().flatten().collect(),
    })
}

/// Refuses a gain that isn't a finite number of 0 or more.
pub(crate) fn check_gains(gains: &[[f64; 3]; 2]) -> Result<(), RenderError> {
    match gains
        .iter()
        .flatten()
        .find(|g| !(g.is_finite() && **g >= 0.0))
    {
        Some(gain) => Err(RenderError::Invalid(format!(
            "a lens's gain is {gain}: a gain is finite and 0 or more"
        ))),
        None => Ok(()),
    }
}

/// Pixel `(i, j)`'s footprint in calibration block `b`'s slot, in texels:
/// how the lens's texel ([`Rig::texel`]) changes across one pixel to the
/// right and one pixel down, from central differences along the
/// directions half a pixel either side, the four rays [`uv_footprint`]
/// casts, projected through the lens (M3 plan 10.3). The lens samples at
/// the LOD log2 of the longer of the two, and its mask takes its margin
/// from that LOD. `None` where the lens's model has no image for the
/// pixel's centre or for one of the four rays: the lens has no footprint
/// there, and doesn't cover the pixel.
///
/// # Panics
///
/// If `b` is neither 0 nor 1.
pub fn lens_footprint(
    rig: &Rig,
    b: usize,
    view: &ViewParams,
    size: (u32, u32),
    i: u32,
    j: u32,
) -> Option<(DVec2, DVec2)> {
    check_block(b);
    let rays = Rays::at(|x, y| view.dir(size, x, y), pixel::centre(i, j));
    Footprint::of(rig, b, &rays).map(|f| (f.across[0], f.across[1]))
}

/// What the flat pass draws (`fs_flat`, the GUI's raw view, and
/// `StillOptions::flat`): `frame`'s picture scaled to `size`, clamped at
/// its edges, as the encode pass computes it.
pub fn render_flat(
    frame: &dyn VideoPlanes,
    size: (u32, u32),
    opts: &Options,
) -> Result<ImageF64, RenderError> {
    let mut source = Source::new(frame, &opts.placement)?;
    source.wraps = false;
    let linear = frame.color().transfer == Transfer::Linear;
    Ok(per_pixel(size, |px| {
        source.shade(&source.flat_footprint(size, px), opts.level0, linear)
    }))
}

/// What `DebugMode::LodHeatmap` writes, read back with `Depth::Linear`:
/// luma's and chroma's LOD as sampled, then luma's before clamping.
///
/// This is one equirect frame's map, the frame placed on the sphere by
/// `placement`. A lens pair's is [`rig_lod_map`].
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

/// What `DebugMode::LodHeatmap` writes for a lens pair, read back with
/// `Depth::Linear`: at each pixel, [`lod_map`]'s three numbers (luma's
/// and chroma's LOD as sampled, then luma's before clamping, each rounded
/// to the nearest f16) for the calibration block that weighs more there
/// (`Rig::weights` at each lens's footprint LOD), block 0 at a tie, and
/// black where no lens covers. `pair` holds each track's picture, and
/// chroma's LOD is taken inside it as its sampling takes it. A fisheye
/// view past θ = π is black, as [`render_rig`]'s is.
///
/// Refuses what [`render_rig`] refuses of the rig.
pub fn rig_lod_map(
    pair: &LensPair,
    rig: &Rig,
    view: &ViewParams,
    size: (u32, u32),
) -> Result<ImageF64, RenderError> {
    check_tracks(rig)?;
    let pixels = per_pixel_rows(size, |(i, j)| {
        let c = pixel::centre(i, j);
        if !in_view(view, size, c) {
            return [0.0; 3];
        }
        let rays = Rays::at(|x, y| view.dir(size, x, y), c);
        let feet = [0, 1].map(|b| Footprint::of(rig, b, &rays));
        let lods = feet.map(|f| f.map_or(f64::INFINITY, |f| f.lod()));
        let w = rig.weights(rays.d, lods);
        if w[0] + w[1] <= 0.0 {
            return [0.0; 3];
        }
        let b = usize::from(w[1] > w[0]);
        // A lens that weighs has a footprint: it covers the pixel.
        let Some(f) = feet[b] else {
            return [0.0; 3];
        };
        let source = &pair.sources[rig.lenses[b].track];
        let s = source.lens_sampled(&f);
        [
            s.lod[0].clamp(0.0, s.levels[0] - 1.0),
            s.lod[1].clamp(0.0, s.levels[1] - 1.0),
            s.lod[0],
        ]
        .map(round_f16)
    });
    Ok(ImageF64 {
        width: size.0,
        height: size.1,
        data: pixels.into_iter().flatten().collect(),
    })
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

/// `f` of each pixel of a `size` grid, row after row from the top. The
/// rows are dealt to threads in turn, and each value is `f`'s of its own
/// pixel alone, so the result doesn't depend on how many threads there
/// are.
pub(crate) fn per_pixel_rows<T: Copy + Default + Send>(
    size: (u32, u32),
    f: impl Fn((u32, u32)) -> T + Sync,
) -> Vec<T> {
    rows_on(row_threads(), size, f)
}

/// How many threads [`render_rig`], the masks and the strips of
/// [`crate::seam`] deal their rows to: the machine's parallelism, and 16
/// at most. Each call starts its own, so a caller that runs them from
/// threads of its own multiplies the two counts.
pub fn row_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(16)
}

/// [`per_pixel_rows`] on `threads` threads.
fn rows_on<T: Copy + Default + Send>(
    threads: usize,
    size: (u32, u32),
    f: impl Fn((u32, u32)) -> T + Sync,
) -> Vec<T> {
    let mut values = vec![T::default(); size.0 as usize * size.1 as usize];
    let mut dealt: Vec<Vec<(u32, &mut [T])>> = (0..threads.max(1)).map(|_| Vec::new()).collect();
    for (j, row) in values.chunks_mut(size.0.max(1) as usize).enumerate() {
        let k = j % dealt.len();
        dealt[k].push((j as u32, row));
    }
    let f = &f;
    std::thread::scope(|s| {
        for rows in dealt {
            s.spawn(move || {
                for (j, row) in rows {
                    for (i, value) in row.iter_mut().enumerate() {
                        *value = f((i as u32, j));
                    }
                }
            });
        }
    });
    values
}

/// The directions through a point of an output grid and half a pixel
/// either side of it: the centre's, then those to its right, to its left,
/// below and above, which the footprint's central differences take.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rays {
    pub(crate) d: DVec3,
    sides: [DVec3; 4],
}

impl Rays {
    /// The rays at point `c` of a grid whose point `(x, y)`, in pixels
    /// from the top-left corner, looks along `dir(x, y)`.
    pub(crate) fn at(dir: impl Fn(f64, f64) -> DVec3, c: DVec2) -> Rays {
        Rays {
            d: dir(c.x, c.y),
            sides: [
                dir(c.x + 0.5, c.y),
                dir(c.x - 0.5, c.y),
                dir(c.x, c.y + 0.5),
                dir(c.x, c.y - 0.5),
            ],
        }
    }
}

/// Where one lens of a rig samples for one output pixel, in its slot's
/// texels: the texel at the pixel's centre, and how it changes across the
/// pixel, to the right and down ([`lens_footprint`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Footprint {
    texel: DVec2,
    across: [DVec2; 2],
}

impl Footprint {
    /// Block `b`'s footprint for `rays`, or `None` where its model has no
    /// image for one of them.
    pub(crate) fn of(rig: &Rig, b: usize, rays: &Rays) -> Option<Footprint> {
        let at = |k: usize| rig.texel(b, rays.sides[k]);
        Some(Footprint {
            texel: rig.texel(b, rays.d)?,
            across: [at(0)? - at(1)?, at(2)? - at(3)?],
        })
    }

    /// The LOD the lens samples luma at, before it is clamped to the
    /// chain, and its mask takes its margin from: log2 of the footprint's
    /// longer side.
    pub(crate) fn lod(&self) -> f64 {
        lod(self.across, DVec2::ONE)
    }
}

/// Whether block `b`'s mask covers the pixel `rays` go through
/// ([`Rig::covers`] at the footprint's LOD): where [`render_rig`] draws
/// the lens alone. A lens with no footprint covers nothing.
pub(crate) fn lens_covers(rig: &Rig, b: usize, rays: &Rays) -> bool {
    Footprint::of(rig, b, rays).is_some_and(|f| rig.covers(b, rays.d, f.lod()))
}

/// Whether a view shows anything at point `c` of its `size` target: a
/// fisheye past θ = π would show the sphere twice (`inside` in
/// `shaders/project.wgsl`).
pub(crate) fn in_view(view: &ViewParams, size: (u32, u32), c: DVec2) -> bool {
    view.theta(size, c.x, c.y) <= PI + 1e-4
}

/// Refuses a block index that no rig has: `b` is 0 or 1.
pub(crate) fn check_block(b: usize) {
    assert!(b < 2, "block {b}: a rig's blocks are 0 and 1");
}

/// Refuses a rig that a pair of pictures can't be sampled under: a block
/// that reads a track other than 0 or 1, or both blocks reading one
/// track. [`Rig::build`] gives each block its own track of the two, so
/// only a rig put together by hand can fail; nothing renders two blocks
/// from one picture (an LRV's two halves are two pictures, 15), and a rig
/// that did would be a block's track written wrong.
pub(crate) fn check_tracks(rig: &Rig) -> Result<(), RenderError> {
    let tracks = [rig.lenses[0].track, rig.lenses[1].track];
    for (b, track) in tracks.into_iter().enumerate() {
        if track > 1 {
            return Err(RenderError::Invalid(format!(
                "block {b} of the rig reads track {track}: a pair has tracks 0 and 1"
            )));
        }
    }
    if tracks[0] == tracks[1] {
        return Err(RenderError::Invalid(format!(
            "both blocks of the rig read track {}: each block reads its own track",
            tracks[0]
        )));
    }
    Ok(())
}

/// A lens pair's two pictures made ready to sample: each track's planes
/// as the shader sees them, clamped at their edges, with their mip chains
/// in f64. The chains are most of what a small picture or a strip costs
/// (11 to 17 ms a pair of 768² and about half a second a pair of 3840²,
/// whose chains take about 470 MB), and they are the planes' alone:
/// nothing of a rig is here, neither its binding nor a gain nor θ_max. So
/// a caller that samples one pair of frames more than once, under one rig
/// or under many as a fit does (8.4), makes the pair once and hands it to
/// [`render_rig_of`] and [`crate::seam::strips_of`] with each rig.
pub struct LensPair {
    /// By track.
    sources: [Source; 2],
    /// Whether each track's values are linear already, by track.
    linear: [bool; 2],
}

impl std::fmt::Debug for LensPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LensPair")
            .field("sizes", &[self.sources[0].size0, self.sources[1].size0])
            .field("linear", &self.linear)
            .finish_non_exhaustive()
    }
}

impl LensPair {
    /// The pair for the two tracks' pictures, by track. Refuses planes
    /// [`render`] would refuse.
    pub fn new(lenses: [&dyn VideoPlanes; 2]) -> Result<LensPair, RenderError> {
        let source = |frame: &dyn VideoPlanes| {
            let mut source = Source::new(frame, &Placement::FULL)?;
            // Fisheye uv has no wrap.
            source.wraps = false;
            Ok::<Source, RenderError>(source)
        };
        Ok(LensPair {
            sources: [source(lenses[0])?, source(lenses[1])?],
            linear: lenses.map(|frame| frame.color().transfer == Transfer::Linear),
        })
    }

    /// Block `b` of `rig`'s linear light at unit gain where `f` samples
    /// its track's picture: trilinear taps (level 0 alone when `level0`),
    /// the matrix and the EOTF, with no rounding. `rig` is one that
    /// [`check_tracks`] passed.
    pub(crate) fn light(&self, rig: &Rig, b: usize, f: &Footprint, level0: bool) -> [f64; 3] {
        let track = rig.lenses[b].track;
        let source = &self.sources[track];
        source.light(&source.lens_sampled(f), level0, self.linear[track])
    }

    /// What [`render_rig_of`] writes at pixel `(i, j)` under `rig`, one
    /// that [`check_tracks`] passed.
    fn shade(
        &self,
        rig: &Rig,
        gains: &[[f64; 3]; 2],
        view: &ViewParams,
        size: (u32, u32),
        (i, j): (u32, u32),
        opts: &RigOptions,
    ) -> [f64; 3] {
        let c = pixel::centre(i, j);
        if !in_view(view, size, c) {
            return [0.0; 3];
        }
        let rays = Rays::at(|x, y| view.dir(size, x, y), c);
        let feet = [0, 1].map(|b| Footprint::of(rig, b, &rays));
        // A lens with no footprint has no LOD: an infinite one, whose
        // margin no mask covers.
        let lods = feet.map(|f| f.map_or(f64::INFINITY, |f| f.lod()));
        let covers = |b: usize| feet[b].is_some() && rig.covers(b, rays.d, lods[b]);
        // The lens's light with its gain, in linear light.
        let lit = |b: usize| {
            let light = feet[b].map_or([0.0; 3], |f| self.light(rig, b, &f, opts.level0));
            [0, 1, 2].map(|k| light[k] * gains[b][k])
        };
        // The linear target is f16; the encode pass undoes the EOTF.
        let dim = view.dim_at(c.x, c.y);
        let picture = |rgb: [f64; 3]| rgb.map(|v| srgb_oetf(round_f16(v * dim).clamp(0.0, 1.0)));
        let alone = |b: usize| {
            if covers(b) { picture(lit(b)) } else { [0.0; 3] }
        };
        match opts.debug {
            RigDebug::Off => {
                let w = rig.weights(rays.d, lods);
                let sum = w[0] + w[1];
                if sum > 0.0 {
                    let mut blend = [0.0; 3];
                    for (b, weight) in w.into_iter().enumerate() {
                        if weight > 0.0 {
                            for (acc, v) in blend.iter_mut().zip(lit(b)) {
                                *acc += weight * v;
                            }
                        }
                    }
                    picture(blend.map(|v| v / sum))
                } else {
                    [0.0; 3]
                }
            }
            RigDebug::Lens0 => alone(0),
            RigDebug::Lens1 => alone(1),
            RigDebug::Weights => {
                let w = rig.weights(rays.d, lods);
                [w[0], 0.0, w[1]].map(round_f16)
            }
            RigDebug::SeamDiff => {
                if covers(0) && covers(1) {
                    let (a, b) = (lit(0), lit(1));
                    [0, 1, 2].map(|k| round_f16((a[k] - b[k]).abs() * 8.0))
                } else {
                    [0.0; 3]
                }
            }
        }
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
    /// What linear light is multiplied by: past a framing's frame, its
    /// dim.
    dim: f64,
}

impl Source {
    fn new(frame: &dyn VideoPlanes, placement: &Placement) -> Result<Source, RenderError> {
        // The shown part only, from the top-left corner, as the upload
        // copies it.
        let shape = SlotShape::of(frame.layout(), placement.shown(frame.size()))?;
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
        let theta = view.theta(size, c.x, c.y);
        let (gx, gy) = uv_footprint(view, size, i, j);
        let suv = sphere_uv(d);
        let uv = suv * self.uv_scale + self.uv_offset;
        let g = [gx * self.uv_scale, gy * self.uv_scale];
        let gc = g.map(|v| v * self.chroma_scale);
        let [u0, v0, u1, v1] = self.cover;
        Sampled {
            uv,
            cuv: uv * self.chroma_scale + self.chroma_offset,
            lod: [lod(g, self.size0), lod(gc, self.size1)],
            levels: self.levels,
            inside: theta <= PI + 1e-4 && suv.x >= u0 && suv.y >= v0 && suv.x <= u1 && suv.y <= v1,
            dim: view.dim_at(c.x, c.y),
        }
    }

    /// Where pixel `(i, j)` of the flat pass samples: the picture's uv
    /// at the pixel's centre over `size`, a pixel's footprint 1 / size.
    fn flat_footprint(&self, size: (u32, u32), (i, j): (u32, u32)) -> Sampled {
        let c = pixel::centre(i, j);
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        let uv = DVec2::new(c.x / w, c.y / h) * self.uv_scale + self.uv_offset;
        let g = [
            DVec2::new(self.uv_scale.x / w, 0.0),
            DVec2::new(0.0, self.uv_scale.y / h),
        ];
        let gc = g.map(|v| v * self.chroma_scale);
        Sampled {
            uv,
            cuv: uv * self.chroma_scale + self.chroma_offset,
            lod: [lod(g, self.size0), lod(gc, self.size1)],
            levels: self.levels,
            inside: true,
            dim: 1.0,
        }
    }

    /// Where a lens's footprint `f`, in this picture's texels, samples:
    /// the texel as a uv over the picture's own size, and chroma by the
    /// chroma transform, as [`Source::flat_footprint`]'s.
    fn lens_sampled(&self, f: &Footprint) -> Sampled {
        let uv = f.texel / self.size0;
        let g = f.across.map(|v| v / self.size0);
        let gc = g.map(|v| v * self.chroma_scale);
        Sampled {
            uv,
            cuv: uv * self.chroma_scale + self.chroma_offset,
            lod: [f.lod(), lod(gc, self.size1)],
            levels: self.levels,
            inside: true,
            dim: 1.0,
        }
    }

    /// The linear light at `s`, before the target rounds it: trilinear
    /// taps (level 0 alone when `level0`), the matrix, and the EOTF
    /// unless the values are `linear`.
    fn light(&self, s: &Sampled, level0: bool, linear: bool) -> [f64; 3] {
        let lods = if level0 {
            [0.0, 0.0]
        } else {
            [
                s.lod[0].clamp(0.0, s.levels[0] - 1.0),
                s.lod[1].clamp(0.0, s.levels[1] - 1.0),
            ]
        };
        let y = self.planes[0].sample(s.uv, lods[0], self.wraps);
        let c1 = self
            .planes
            .get(1)
            .map_or([0.0; 2], |p| p.sample(s.cuv, lods[1], self.wraps));
        let c2 = self
            .planes
            .get(2)
            .map_or([0.0; 2], |p| p.sample(s.cuv, lods[1], self.wraps));
        let samples = [
            y[0],
            c1[0],
            if self.kind == LayoutKind::SemiPlanar {
                c1[1]
            } else {
                c2[0]
            },
            1.0,
        ];
        let rgb = self.matrix.map(|row| {
            (0..4)
                .map(|i| row[i] * samples[i])
                .sum::<f64>()
                .clamp(0.0, 1.0)
        });
        rgb.map(|v| if linear { v } else { srgb_eotf(v) })
    }

    /// The colour at `s`, as the encode pass computes it: the linear
    /// light ([`Source::light`]), the dim, f16, then the OETF.
    fn shade(&self, s: &Sampled, level0: bool, linear: bool) -> [f64; 3] {
        if !s.inside {
            return [0.0; 3];
        }
        self.light(s, level0, linear).map(|v| {
            // The linear target is f16; the encode pass undoes the EOTF.
            let lin = round_f16(v * s.dim);
            srgb_oetf(lin.clamp(0.0, 1.0))
        })
    }
}

/// log2 of the longer side of a footprint `g`, in texels of a `size`
/// texture (`lod` in `shaders/project.wgsl`).
fn lod(g: [DVec2; 2], size: DVec2) -> f64 {
    (g[0] * size)
        .length()
        .max((g[1] * size).length())
        .max(1e-30)
        .log2()
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
    /// stored in the plane's format: an 8-bit one by [`halve_codes`], a
    /// deep one as the nearest f16.
    fn halve(&self, format: PlaneFormat) -> Level {
        let (sw, sh) = (self.w, self.h);
        if !format.is_deep() {
            // The texels are codes / 255, which times 255 round back to
            // the codes.
            let codes: Vec<u8> = self
                .data
                .iter()
                .map(|v| (v * 255.0).round() as u8)
                .collect();
            let (w, h, next) = halve_codes(&codes, (sw, sh), self.comps);
            return Level {
                w,
                h,
                comps: self.comps,
                data: next.iter().map(|&k| f64::from(k) / 255.0).collect(),
            };
        }
        let (dw, dh) = ((sw / 2).max(1), (sh / 2).max(1));
        let weights = |j: u32, ss: u32, ds: u32| -> Vec<(u32, f64)> {
            overlaps(j, ss, ds)
                .map(|(x, o)| (x, f64::from(o) / f64::from(ss)))
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
                    data.push(round_f16(acc));
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

/// The source texels destination texel `j` covers, of a level `ss` texels
/// across halved to `ds`, and the length each shares with it, in units
/// where the destination texel is `ss` long and a source texel `ds`: so
/// the lengths sum to `ss`, and at an odd size the first and last are
/// parts. As `shaders/mips.wgsl` works them out.
fn overlaps(j: u32, ss: u32, ds: u32) -> impl Iterator<Item = (u32, u32)> {
    let (lo, hi) = (j * ss, j * ss + ss);
    (lo / ds..=(hi - 1) / ds).map(move |x| (x, hi.min((x + 1) * ds) - lo.max(x * ds)))
}

/// The next mip level of an 8-bit plane, `size` texels of `comps`
/// components each (rows top to bottom, components interleaved), as the
/// mip pass makes it (`shaders/mips.wgsl`, DECISIONS D31): each
/// destination texel's area-weighted mean of the codes it covers, exact
/// (in integers), rounded to the nearest code, ties to even. Returns the
/// level's size and codes.
///
/// Ties to even, because a 2×2 mean is a whole code or a quarter, a half
/// or three quarters past one, so ties are common, and each level is
/// made from the one above, so a rule that breaks every tie one way
/// moves each level further that way: on the grid, against the same
/// chain unrounded, ties up (this reference's f64 `round` until the pass
/// rounded its means) put luma levels 1 to 5 a mean 0.11 to 0.74 codes
/// high, and RADV's export of the unrounded means as far low, where ties
/// to even keeps every level within 0.02 codes. It is also the rule of
/// every other rounding in the pipeline (`round_f16`, the ingest and
/// encode passes, WGSL's `round`).
pub fn halve_codes(codes: &[u8], (sw, sh): (u32, u32), comps: usize) -> (u32, u32, Vec<u8>) {
    let (dw, dh) = ((sw / 2).max(1), (sh / 2).max(1));
    let area = u64::from(sw) * u64::from(sh);
    let columns: Vec<Vec<(u32, u32)>> = (0..dw).map(|i| overlaps(i, sw, dw).collect()).collect();
    let mut out = Vec::with_capacity(dw as usize * dh as usize * comps);
    for j in 0..dh {
        let wy: Vec<(u32, u32)> = overlaps(j, sh, dh).collect();
        for wx in &columns {
            for c in 0..comps {
                let mut n = 0u64;
                for &(y, a) in &wy {
                    for &(x, b) in wx {
                        let code = codes[(y as usize * sw as usize + x as usize) * comps + c];
                        n += u64::from(a) * u64::from(b) * u64::from(code);
                    }
                }
                let (k, r) = (n / area, n % area);
                let up = 2 * r > area || (2 * r == area && k % 2 == 1);
                out.push((k + u64::from(up)) as u8);
            }
        }
    }
    (dw, dh, out)
}

pub(crate) fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB OETF in f64, as the encode and YUV passes apply it in f32.
pub fn srgb_oetf(l: f64) -> f64 {
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
    fn rows_come_out_alike_on_any_number_of_threads() {
        // Each pixel's own coordinates, so a row dealt to the wrong place
        // shows.
        let f = |(i, j): (u32, u32)| [f64::from(i), f64::from(j), f64::from(i * 100 + j)];
        let one = rows_on(1, (7, 13), f);
        assert_eq!(one.len(), 7 * 13);
        for (n, value) in one.iter().enumerate() {
            let (i, j) = ((n % 7) as u32, (n / 7) as u32);
            assert_eq!(*value, f((i, j)), "pixel ({i}, {j})");
        }
        for threads in [0, 2, 3, 4, 13, 16, 64] {
            assert_eq!(rows_on(threads, (7, 13), f), one, "{threads} threads");
        }
        // No pixels: nothing to deal.
        assert!(rows_on(4, (0, 5), f).is_empty());
        assert!(rows_on(4, (5, 0), f).is_empty());
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
    fn eight_bit_means_round_to_the_nearest_code_ties_to_even() {
        // 2×2 blocks whose means are 0.25, 0.5, 0.75, 1.5, 2.5 and 3.5
        // codes past a whole one, in R8; f64's `round` takes each half up.
        let blocks: [[u8; 4]; 6] = [
            [0, 0, 0, 1],
            [0, 0, 1, 1],
            [0, 1, 1, 1],
            [1, 1, 2, 2],
            [2, 3, 2, 3],
            [3, 4, 3, 4],
        ];
        let codes: Vec<u8> = (0..2)
            .flat_map(|row| {
                blocks
                    .iter()
                    .flat_map(move |b| [b[2 * row], b[2 * row + 1]])
            })
            .collect();
        let (w, h, got) = halve_codes(&codes, (12, 2), 1);
        assert_eq!((w, h, got), (6, 1, vec![0, 0, 1, 2, 2, 4]));
        // Rg8: each component alone.
        let (_, _, got) = halve_codes(&[0, 9, 1, 9, 0, 10, 1, 10], (2, 2), 2);
        assert_eq!(got, vec![0, 10]);
        // Odd: 5 across into 2, weights 2, 2 and 1 of 5 for texel 0 and
        // 1, 2 and 2 for texel 1, so 1, 2, 3, 0, 0 is 9 / 5 and 3 / 5; 5 × 2
        // into 2 × 1 puts 5 / 10 and 15 / 10 on ties.
        assert_eq!(halve_codes(&[1, 2, 3, 0, 0], (5, 1), 1).2, vec![2, 1]);
        let tie = [1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(halve_codes(&tie, (5, 2), 1).2, vec![0, 0]);
        let tie = [1, 1, 1, 0, 0, 2, 2, 2, 0, 0];
        assert_eq!(halve_codes(&tie, (5, 2), 1).2, vec![2, 0]);
        // Every overlap sums to the source size, at the largest sizes too.
        for (ss, j) in [(5, 1), (16383, 8190), (16384, 8191), (3, 0), (1, 0)] {
            let ds = (ss / 2).max(1);
            assert_eq!(overlaps(j, ss, ds).map(|o| o.1).sum::<u32>(), ss);
        }
        // At 16383 × 3 the sum of 255s is 255 · 16383 · 3 over the area:
        // 255 exactly.
        let full = vec![255u8; 16383 * 3];
        let (w, h, got) = halve_codes(&full, (16383, 3), 1);
        assert_eq!((w, h), (8191, 1));
        assert!(got.iter().all(|&k| k == 255));
    }
}
