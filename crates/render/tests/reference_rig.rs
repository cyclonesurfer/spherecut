//! The CPU reference's stitch (M3 plan 10.3 and 10.4):
//! `reference::render_rig` and its debug modes 4 to 7, and what samples
//! through it, the strips (8.2) and the masks (8.1). No GPU: the shader's
//! stitch is M3.7's, and is held to this.
//!
//! The scene is rung 1's (4.2): synth's X5-like camera at 768² a lens,
//! seeing `SeamLines` infinitely far, each lens's picture coded as the
//! X5 codes it, and sphere's rig of the same numbers through the tests'
//! boundary (`common::rig`), stitched at q = 0. `SeamLines` is nowhere
//! black, so a lens drawn alone is black exactly where its mask leaves it
//! out. The tests that want another binding, or none of the scene's size,
//! take the same camera a quarter the size with its binding reversed
//! ([`reversed`]).
//!
//! The tests hold the pictures to a sampler of their own ([`Planes`]),
//! written from 10.3's text and the X5's coding apart from the
//! reference's: the mip chain by `halve_codes`, bilinear and trilinear
//! taps, chroma at its siting, BT.709's matrix and the sRGB EOTF, at each
//! lens's texel (`Rig::texel`) and the LOD of its footprint, which the
//! tests take from differences of `Rig::texel` half a pixel either side
//! ([`probe`]). The linear target is f16, so a picture's value is held to
//! the nearest f16 of the test's own ([`steps_off`]): half a step off at
//! most, which a blend made after rounding, a gain on display values or
//! another LOD miss.
//!
//! Budget (4.1 rule 5, 19.6): 2 s of the 15 s that M3.4's suites share;
//! each test prints its time and the suite's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
use std::sync::OnceLock;
use std::time::Instant;

use glam::{DQuat, DVec2, DVec3};
use spherecut_core::video::VideoPlanes;
use spherecut_render::compare::{View, Weights, psnr, psnr_weighted};
use spherecut_render::plane::mip_levels;
use spherecut_render::readback::round_f16;
use spherecut_render::reference::{
    ImageF64, RigDebug, RigOptions, halve_codes, lens_footprint, render_rig, uv_footprint,
};
use spherecut_render::seam::{StripSpec, band_weights, lens_coverage, strips};
use spherecut_render::{Framing, RenderError, ViewParams};
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_sphere::rig::{LensTune, Rig, Tune};
use spherecut_synth::lens::{
    Coding, Planted, RenderCache, TruthBinding, X5_LIKE_SIDE, compose, lens_planes, render_lens,
    x5_like, x5_like_planted,
};
use spherecut_synth::{Pattern, render_view_ideal};
use spherecut_testkit::control::{above, at_least, at_most, must_fail};

use common::Frame;
use common::rig::{lens_frame, sphere_rig};

/// The suite's budget in seconds (the module's doc).
const BUDGET_S: f64 = 2.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and how
/// long the suite has run. The tests run side by side, so none of them
/// can time the rest: the last line printed is the suite's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the suite {suite:.3} s so far, of its {BUDGET_S} s of M3.4's budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// How many threads `render_rig` deals its rows to here.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

// The scene.

/// The side of a lens's track, and how many samples a side synth takes
/// for each of its pixels (rung 1's: 2).
const SIDE: u32 = X5_LIKE_SIDE;
const SS: u32 = 2;

/// Unit gains.
const UNIT: [[f64; 3]; 2] = [[1.0; 3]; 2];

/// The scene every test stitches: the tracks' frames, by track, and
/// sphere's rigs of the camera.
struct Scene {
    /// The rig at the default tune, q = 0: θ_max is 100°, inside the frame
    /// of each lens's slot but for the mask's margin.
    rig: Rig,
    /// The same with θ_max at 104°, past where the slot's frame cuts each
    /// lens's circle (99.8° to 102.8°), so that the mask's frame term is
    /// what bounds the lens there (10.5's `dfish_mask_margin`).
    wide: Rig,
    frames: [Frame; 2],
    planes: [Planes; 2],
}

/// The default tune at θ_max = `theta_max_deg` on both lenses, stitched
/// infinitely far, where the scene is.
fn tune(theta_max_deg: f64) -> Tune {
    Tune {
        lenses: [LensTune {
            theta_max_deg,
            ..LensTune::default()
        }; 2],
        q_per_m: 0.0,
        ..Tune::default()
    }
}

fn scene() -> &'static Scene {
    static SCENE: OnceLock<Scene> = OnceLock::new();
    SCENE.get_or_init(|| {
        let began = Instant::now();
        let (cal, planted) = (x5_like(), x5_like_planted());
        let truth = &compose(&cal, &planted, 0, [SIDE; 2]);
        // Side by side, so that one track is coded while the other is
        // drawn.
        let frames = std::thread::scope(|s| {
            let tracks = [0, 1].map(|track| {
                s.spawn(move || {
                    let picture =
                        RenderCache::shared().render(Pattern::SeamLines, truth, track, 0, SS);
                    lens_frame(&lens_planes(&picture, Coding::X5), SIDE)
                })
            });
            tracks.map(|track| track.join().expect("a track's frame"))
        });
        let rig = |theta_max_deg| {
            sphere_rig(&cal, &planted, &tune(theta_max_deg), 0, [SIDE; 2])
                .expect("the X5-like rig builds")
        };
        let scene = Scene {
            rig: rig(100.0),
            wide: rig(104.0),
            planes: [Planes::of(&frames[0]), Planes::of(&frames[1])],
            frames,
        };
        eprintln!(
            "the scene, two lenses at {SIDE}² and their mips: {:.3} s",
            began.elapsed().as_secs_f64()
        );
        scene
    })
}

impl Scene {
    fn lenses(&self) -> [&dyn VideoPlanes; 2] {
        [&self.frames[0], &self.frames[1]]
    }

    /// `rig`'s picture of the scene through `view`.
    fn render(
        &self,
        rig: &Rig,
        gains: [[f64; 3]; 2],
        (view, size): &Shot,
        debug: RigDebug,
    ) -> ImageF64 {
        let opts = RigOptions {
            debug,
            level0: false,
        };
        render_rig(self.lenses(), rig, gains, view, *size, &opts).expect("the pair renders")
    }
}

/// `rig` with its mask's frame term defeated: each lens's side, which the
/// term reads, set past its slot's, so that a sample past the picture's
/// right or bottom edge counts as inside (10.5's control, "frame term
/// off").
fn frame_term_defeated(rig: &Rig) -> Rig {
    let mut loose = *rig;
    for lens in &mut loose.lenses {
        lens.side *= 2;
    }
    loose
}

/// A lens's side in the quarter-size camera.
const QUARTER: u32 = SIDE / 4;

/// The camera with its binding reversed, a quarter the size, for the
/// tests that want its binding or need none of the scene's size: block
/// 0's lens is on track 1 and block 1's on track 0, which is the front
/// one.
struct Reversed {
    /// Sphere's rig of it at the default tune, q = 0.
    rig: Rig,
    frames: [Frame; 2],
    planes: [Planes; 2],
}

fn reversed() -> &'static Reversed {
    static REVERSED: OnceLock<Reversed> = OnceLock::new();
    REVERSED.get_or_init(|| {
        let cal = x5_like();
        let planted = Planted {
            binding: TruthBinding::Reversed,
            ..x5_like_planted()
        };
        let truth = compose(&cal, &planted, 0, [QUARTER; 2]);
        let frames = [0, 1].map(|track| {
            let picture = render_lens(Pattern::SeamLines, &truth, track, 0, SS);
            lens_frame(&lens_planes(&picture, Coding::X5), QUARTER)
        });
        let rig = sphere_rig(&cal, &planted, &tune(100.0), 0, [QUARTER; 2]);
        let rig = rig.expect("the reversed rig builds");
        assert_eq!([rig.lenses[0].track, rig.lenses[1].track], [1, 0]);
        Reversed {
            rig,
            planes: [Planes::of(&frames[0]), Planes::of(&frames[1])],
            frames,
        }
    })
}

impl Reversed {
    fn lenses(&self) -> [&dyn VideoPlanes; 2] {
        [&self.frames[0], &self.frames[1]]
    }
}

// The views.

/// A view and the size of its picture.
type Shot = (ViewParams, (u32, u32));

/// The whole sphere, `width` wide.
fn equirect(width: u32) -> Shot {
    (ViewParams::equirect(DQuat::IDENTITY), (width, width / 2))
}

/// A rectilinear view `hfov` radians wide, `side` square, looking where
/// `turn` takes −Z.
fn towards(turn: DQuat, hfov: f64, side: u32) -> Shot {
    let lens = Lens::new(Projection::Rectilinear, hfov);
    (ViewParams::new(turn, lens), (side, side))
}

/// The 90° view along +X, onto the seam on the right (4.2's seam views).
fn right(side: u32) -> Shot {
    towards(DQuat::from_rotation_y(-FRAC_PI_2), FRAC_PI_2, side)
}

/// The same 45° wide: at 192² it is 4 px/°, about the lenses' own scale
/// at the seam.
fn right_close(side: u32) -> Shot {
    towards(DQuat::from_rotation_y(-FRAC_PI_2), FRAC_PI_4, side)
}

/// The 90° view along +Y, onto the seam at the zenith.
fn zenith(side: u32) -> Shot {
    towards(DQuat::from_rotation_x(FRAC_PI_2), FRAC_PI_2, side)
}

// What the tests work out for themselves.

/// What a test works out for one output pixel: its direction, each
/// lens's texel there, and the LOD of each lens's footprint, log2 of the
/// longer of the texel's two central differences across the pixel
/// (10.3). A lens with no image for one of the pixel's rays has no LOD:
/// an infinite one.
#[derive(Debug, Clone, Copy)]
struct Probe {
    d: DVec3,
    texel: [Option<DVec2>; 2],
    lod: [f64; 2],
}

/// The probe at point `(x, y)` of a grid whose points look along `dir`.
fn probe(rig: &Rig, dir: &impl Fn(f64, f64) -> DVec3, x: f64, y: f64) -> Probe {
    let d = dir(x, y);
    Probe {
        d,
        texel: [0, 1].map(|b| rig.texel(b, d)),
        lod: [0, 1].map(|b| {
            let at = |dx: f64, dy: f64| rig.texel(b, dir(x + dx, y + dy));
            let side = |dx: f64, dy: f64| Some((at(dx, dy)? - at(-dx, -dy)?).length());
            match (side(0.5, 0.0), side(0.0, 0.5)) {
                (Some(across), Some(down)) => across.max(down).log2(),
                _ => f64::INFINITY,
            }
        }),
    }
}

/// The probe of every pixel of a `size` grid, row after row, worked out
/// on four threads.
fn probes(rig: &Rig, dir: impl Fn(f64, f64) -> DVec3 + Sync, size: (u32, u32)) -> Vec<Probe> {
    let rows: Vec<u32> = (0..size.1).collect();
    let dir = &dir;
    std::thread::scope(|s| {
        let parts: Vec<_> = rows
            .chunks(rows.len().div_ceil(4).max(1))
            .map(|rows| {
                s.spawn(move || {
                    let mut part = Vec::with_capacity(rows.len() * size.0 as usize);
                    for &j in rows {
                        for i in 0..size.0 {
                            part.push(probe(rig, dir, f64::from(i) + 0.5, f64::from(j) + 0.5));
                        }
                    }
                    part
                })
            })
            .collect();
        parts
            .into_iter()
            .flat_map(|part| part.join().expect("a probe thread"))
            .collect()
    })
}

/// The probes of a shot's pixels.
fn view_probes(rig: &Rig, (view, size): &Shot) -> Vec<Probe> {
    probes(rig, |x, y| view.dir(*size, x, y), *size)
}

/// The angle of a lens-frame direction off the lens's axis, +z.
fn off_axis(ray: DVec3) -> f64 {
    ray.x.hypot(ray.y).atan2(ray.z)
}

/// One track's picture as the tests' own sampler reads it: each plane's
/// codes level by level, as the upload leaves them (the area mips of
/// `halve_codes`, as many as `mip_levels` gives the plane).
struct Planes {
    /// Luma, Cb and Cr: each level's side and codes.
    chains: [Vec<(u32, Vec<u8>)>; 3],
}

impl Planes {
    /// The planes of a square 8-bit 4:2:0 frame whose rows have no
    /// padding, as `lens_frame` makes them.
    fn of(frame: &Frame) -> Planes {
        let chain = |index: usize| {
            let (codes, stride, (w, h)) = &frame.planes[index];
            assert_eq!((*stride, w), (*w as usize, h), "a square, unpadded plane");
            let mut levels = vec![(*w, codes.clone())];
            for _ in 1..mip_levels(*w, *h) {
                let (side, codes) = levels.last().expect("level 0");
                let (w, _, next) = halve_codes(codes, (*side, *side), 1);
                levels.push((w, next));
            }
            levels
        };
        Planes {
            chains: [chain(0), chain(1), chain(2)],
        }
    }

    /// The picture's side.
    fn side(&self) -> u32 {
        self.chains[0][0].0
    }

    /// The bilinear blend of a level's four codes about `at`, in the
    /// level's texels from its corner (texel k's centre at k + ½), the
    /// edge's repeated past it; over 255.
    fn bilinear((side, codes): &(u32, Vec<u8>), at: DVec2) -> f64 {
        let (x, y) = (at.x - 0.5, at.y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (a, b) = (x - x0, y - y0);
        let last = i64::from(*side) - 1;
        let code = |i: f64, j: f64| {
            let (i, j) = ((i as i64).clamp(0, last), (j as i64).clamp(0, last));
            f64::from(codes[j as usize * *side as usize + i as usize]) / 255.0
        };
        let top = code(x0, y0) * (1.0 - a) + code(x0 + 1.0, y0) * a;
        let bottom = code(x0, y0 + 1.0) * (1.0 - a) + code(x0 + 1.0, y0 + 1.0) * a;
        top * (1.0 - b) + bottom * b
    }

    /// A plane's trilinear tap at `at`, in its level-0 texels, at `lod`
    /// clamped to its chain.
    fn trilinear(chain: &[(u32, Vec<u8>)], at: DVec2, lod: f64) -> f64 {
        let last = chain.len() - 1;
        let lod = lod.clamp(0.0, last as f64);
        let level = (lod.floor() as usize).min(last);
        let tap = |level: usize| Planes::bilinear(&chain[level], at / f64::from(1u32 << level));
        if level == last {
            return tap(level);
        }
        let t = lod - level as f64;
        tap(level) * (1.0 - t) + tap(level + 1) * t
    }

    /// The linear light the picture shows at `texel` of its luma plane,
    /// for a footprint of LOD `lod` in luma texels, before any rounding:
    /// the X5's coding undone (3.1). A chroma sample sits on the left
    /// luma sample of its pair and midway between its two rows, so luma
    /// (x, y) is chroma (x/2 + ¼, y/2), and a chroma texel is two luma
    /// texels, an LOD less; full range, Cb and Cr about 128; BT.709; the
    /// sRGB EOTF.
    fn light(&self, texel: DVec2, lod: f64) -> [f64; 3] {
        let y = Planes::trilinear(&self.chains[0], texel, lod);
        let chroma_at = DVec2::new(texel.x / 2.0 + 0.25, texel.y / 2.0);
        let chroma = |plane: usize| {
            Planes::trilinear(&self.chains[plane], chroma_at, lod - 1.0) - 128.0 / 255.0
        };
        let (pb, pr) = (chroma(1), chroma(2));
        let (kr, kb) = (0.2126, 0.0722);
        let r = y + 2.0 * (1.0 - kr) * pr;
        let b = y + 2.0 * (1.0 - kb) * pb;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b].map(|v| eotf(v.clamp(0.0, 1.0)))
    }

    /// Whether a luma tap at `texel` and `lod` reads past the picture's
    /// edge on either level it blends, where a sampler repeats the edge's
    /// texels.
    fn reads_past_the_edge(&self, texel: DVec2, lod: f64) -> bool {
        let chain = &self.chains[0];
        let last = chain.len() - 1;
        let lod = lod.clamp(0.0, last as f64);
        let level = (lod.floor() as usize).min(last);
        let fraction = lod - level as f64;
        let past = |level: usize| {
            let at = texel / f64::from(1u32 << level) - 0.5;
            at.min_element() < 0.0 || at.max_element() > f64::from(chain[level].0) - 1.0
        };
        past(level) || (level < last && fraction > 0.0 && past(level + 1))
    }
}

/// The sRGB EOTF.
fn eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB OETF.
fn oetf(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// The spacing of the f16 values about `v`: 2^(e − 10) for 2^e ≤ |v| <
/// 2^(e + 1), and 2^−24 among the subnormals.
fn f16_step(v: f64) -> f64 {
    (v.abs().log2().floor().max(-14.0) - 10.0).exp2()
}

/// How far `got`, a value the linear target holds, lies from `want`, the
/// value before rounding, in the target's steps there. The nearest f16 is
/// half a step off at most.
fn linear_steps_off(got: f64, want: f64) -> f64 {
    (got - want).abs() / f16_step(want)
}

/// The same for `got` as a finished picture holds it, in display values:
/// the target clamps to 0 to 1, then the OETF.
fn steps_off(got: f64, want: f64) -> f64 {
    linear_steps_off(eotf(got), want.clamp(0.0, 1.0))
}

/// What a value the nearest f16 of its own may be off by, in steps: a
/// half, and room for the last bits of two ways of summing the same
/// taps.
const NEAREST: f64 = 0.5 + 1e-6;

/// The most any channel of `picture`'s pixel `n` is off `want`, in the
/// target's steps.
fn pixel_steps(picture: &ImageF64, n: usize, want: [f64; 3]) -> f64 {
    pixel(picture, n)
        .into_iter()
        .zip(want)
        .map(|(got, want)| steps_off(got, want))
        .fold(0.0, f64::max)
}

/// The most any channel of `picture` is off what `want` gives for its
/// pixel (the pixel's index, row after row), in the target's steps.
fn worst_steps(picture: &ImageF64, want: impl Fn(usize) -> [f64; 3]) -> f64 {
    (0..picture.data.len() / 3)
        .map(|n| pixel_steps(picture, n, want(n)))
        .fold(0.0, f64::max)
}

/// The stitch at one pixel, in linear light before the target rounds it:
/// each lens the rig weighs, as `lit` gives it, blended by the rig's
/// weights at the lenses' own LODs.
fn blended(rig: &Rig, p: &Probe, lit: impl Fn(usize) -> [f64; 3]) -> [f64; 3] {
    mixed(rig.weights(p.d, p.lod), lit)
}

/// The blend by the weights `w` of each lens they weigh, as `lit` gives
/// it.
fn mixed(w: [f64; 2], lit: impl Fn(usize) -> [f64; 3]) -> [f64; 3] {
    let mut blend = [0.0; 3];
    for b in 0..2 {
        if w[b] > 0.0 {
            let rgb = lit(b);
            for c in 0..3 {
                blend[c] += w[b] * rgb[c] / (w[0] + w[1]);
            }
        }
    }
    blend
}

/// Pixel `n`'s three values.
fn pixel(picture: &ImageF64, n: usize) -> [f64; 3] {
    [
        picture.data[3 * n],
        picture.data[3 * n + 1],
        picture.data[3 * n + 2],
    ]
}

const BLACK: [f64; 3] = [0.0; 3];

/// Each lens's footprint is its texel's change across the pixel's four
/// ±½ px rays, projected through that lens, and its LOD the log2 of the
/// footprint's longer side in texels (10.3).
#[test]
fn rig_footprint_is_the_projected_half_pixel_rays() {
    let _t = timed("rig_footprint_is_the_projected_half_pixel_rays");
    let s = scene();
    let rig = &s.rig;

    // The footprint the reference gives is the texel's central difference
    // over the pixel, and so the texel's slope a pixel long: a difference
    // a thousandth of a pixel either side, scaled up, comes to the same
    // within the map's curvature over a pixel. A lens has no footprint
    // where one of the five rays has no image, past its model's end.
    let (mut curve, mut doubled, mut none) = (0.0f64, 0.0f64, 0usize);
    for shot in [right(192), equirect(128)] {
        let (view, size) = &shot;
        // Every third pixel each way.
        for (j, i) in (0..size.1)
            .step_by(3)
            .flat_map(|j| (0..size.0).step_by(3).map(move |i| (j, i)))
        {
            let (x, y) = (f64::from(i) + 0.5, f64::from(j) + 0.5);
            let d = view.dir(*size, x, y);
            for b in 0..2 {
                let at = |x: f64, y: f64| rig.texel(b, view.dir(*size, x, y));
                let across = |dx: f64, dy: f64| Some(at(x + dx, y + dy)? - at(x - dx, y - dy)?);
                let own = match (at(x, y), across(0.5, 0.0), across(0.0, 0.5)) {
                    (Some(_), Some(gx), Some(gy)) => Some((gx, gy)),
                    _ => None,
                };
                assert_eq!(
                    lens_footprint(rig, b, view, *size, i, j),
                    own,
                    "lens {b} at ({i}, {j})"
                );
                let Some((gx, gy)) = own else {
                    none += 1;
                    continue;
                };
                if !rig.covers(b, d, gx.length().max(gy.length()).log2()) {
                    continue;
                }
                let h = 1e-3;
                let slope =
                    [across(h, 0.0), across(0.0, h)].map(|g| g.expect("a texel") / (2.0 * h));
                let whole = [across(1.0, 0.0), across(0.0, 1.0)].map(|g| g.expect("a texel"));
                for (k, g) in [gx, gy].into_iter().enumerate() {
                    curve = curve.max((g - slope[k]).length() / slope[k].length());
                    doubled = doubled.max((whole[k] - slope[k]).length() / slope[k].length());
                }
            }
        }
    }
    eprintln!(
        "a footprint against the texel's slope: {curve:.2e} of it at most; {none} of the pixels \
         tried have none"
    );
    assert!(none > 100, "{none}");
    assert!(curve < 5e-3, "{curve}");
    let off = must_fail("rays a whole pixel either side", at_most(doubled, 5e-3));
    assert!(off > 0.9, "{off}");

    // And the lens is sampled there: each lens alone is, at every pixel
    // its mask covers, the test's own trilinear taps at the lens's texel
    // and its footprint's LOD, as the nearest f16; black elsewhere. On
    // the seam view the LODs lie about 0 to 1, and on the small equirect
    // past each chain's last level.
    let (mut worst, mut worst_uv) = (0.0f64, 0.0f64);
    let mut lods = [f64::INFINITY, f64::NEG_INFINITY];
    for shot in [right(192), equirect(128)] {
        let (view, size) = &shot;
        let probes = view_probes(rig, &shot);
        // The LOD of the sphere's own footprint, as if the slot were an
        // equirect picture of its side.
        let uv_lod = |n: usize| {
            let (i, j) = (n as u32 % size.0, n as u32 / size.0);
            let (gx, gy) = uv_footprint(view, *size, i, j);
            (gx.length().max(gy.length()) * f64::from(SIDE)).log2()
        };
        for (b, debug) in [(0, RigDebug::Lens0), (1, RigDebug::Lens1)] {
            let picture = s.render(rig, UNIT, &shot, debug);
            let planes = &s.planes[rig.lenses[b].track];
            let covered = |n: usize| rig.covers(b, probes[n].d, probes[n].lod[b]);
            let light = |n: usize, lod: f64| {
                if covered(n) {
                    planes.light(probes[n].texel[b].expect("a texel"), lod)
                } else {
                    BLACK
                }
            };
            for (n, p) in probes.iter().enumerate() {
                if covered(n) {
                    lods = [lods[0].min(p.lod[b]), lods[1].max(p.lod[b])];
                } else {
                    assert_eq!(pixel(&picture, n), BLACK, "lens {b}, pixel {n}");
                }
            }
            worst = worst.max(worst_steps(&picture, |n| light(n, probes[n].lod[b])));
            worst_uv = worst_uv.max(worst_steps(&picture, |n| light(n, uv_lod(n))));
        }
    }
    eprintln!(
        "each lens alone against the test's taps: {worst:.4} f16 steps at most, LODs {:.2} to \
         {:.2}; at uv_footprint's LOD {worst_uv:.0} steps",
        lods[0], lods[1]
    );
    assert!(lods[0] < 0.5 && lods[1] > 3.5, "{lods:?}");
    assert!(worst <= NEAREST, "{worst}");
    let off = must_fail(
        "uv_footprint's LOD for the rays'",
        at_most(worst_uv, NEAREST),
    );
    assert!(off > 100.0, "{off}");
}

/// The mask and the blend are the rig's own, at each lens's own LOD:
/// `Rig::covers` and `Rig::weights`, with the frame term and the feather's
/// shrink (6.2).
#[test]
fn rig_mask_and_weights_are_the_rigs() {
    let _t = timed("rig_mask_and_weights_are_the_rigs");
    let s = scene();

    // Mode 6 is `Rig::weights` at the lenses' own LODs, as the nearest
    // f16, and the coverage `Rig::covers` there, at every pixel: on the
    // rig, and with θ_max past the frame; at the source's own scale and
    // minified, where the margin grows and the feather shrinks.
    let (mut framed, mut shrunk, mut at_lod_0) = (0usize, 0usize, 0usize);
    for (name, rig) in [("the rig", &s.rig), ("θ_max 104°", &s.wide)] {
        for shot in [equirect(192), right(128), equirect(64)] {
            let (view, size) = &shot;
            let probes = view_probes(rig, &shot);
            let weights = s.render(rig, UNIT, &shot, RigDebug::Weights);
            let coverage = [0, 1].map(|b| lens_coverage(rig, b, view, *size));
            for (n, p) in probes.iter().enumerate() {
                let w = rig.weights(p.d, p.lod);
                let written = [round_f16(w[0]), 0.0, round_f16(w[1])];
                assert_eq!(pixel(&weights, n), written, "{name}, {size:?}, pixel {n}");
                let unmargined = rig.weights(p.d, [0.0; 2]);
                at_lod_0 +=
                    usize::from(written != [unmargined[0], 0.0, unmargined[1]].map(round_f16));
                for (b, coverage) in coverage.iter().enumerate() {
                    let covers = rig.covers(b, p.d, p.lod[b]);
                    assert_eq!(
                        coverage[n],
                        if covers { 1.0 } else { 0.0 },
                        "{name}, {size:?}, lens {b}, pixel {n}"
                    );
                    // What bites: the frame term where θ alone would
                    // cover, and the shrink where both lenses weigh.
                    let lens = &rig.lenses[b];
                    let margin = rig.margin_texels * p.lod[b].max(0.0).exp2();
                    let within = off_axis(rig.ray(b, p.d))
                        <= lens.theta_max - margin / lens.texels_per_radian;
                    framed += usize::from(within && !covers);
                    let both = w[0] > 0.0 && w[1] > 0.0;
                    shrunk += usize::from(both && rig.feather_at(b, p.lod[b]) < rig.feather);
                }
            }
        }
    }
    eprintln!(
        "{framed} samples left out by the frame term alone, {shrunk} blended over a shrunk \
         feather; {at_lod_0} pixels' weights differ from LOD 0's"
    );
    assert!(framed > 100 && shrunk > 100, "{framed} {shrunk}");
    let off = must_fail("the weights at LOD 0", at_most(at_lod_0, 0));
    assert!(off > 100, "{off}");

    // The blend is of the lenses' linear light, before the target rounds
    // it: the stitched picture is the nearest f16 of the test's own blend
    // of its own taps. Blending what each lens alone rounds to lands up
    // to a step off.
    let shot = right(256);
    let probes = view_probes(&s.rig, &shot);
    let began = Instant::now();
    let stitched = s.render(&s.rig, UNIT, &shot, RigDebug::Off);
    eprintln!(
        "render_rig, a 90° view of the seam at 256²: {:.1} ms on {} threads",
        began.elapsed().as_secs_f64() * 1e3,
        threads()
    );
    let light = |n: usize, b: usize| {
        let p = &probes[n];
        s.planes[s.rig.lenses[b].track].light(p.texel[b].expect("a texel"), p.lod[b])
    };
    let blend = worst_steps(&stitched, |n| blended(&s.rig, &probes[n], |b| light(n, b)));
    let rounded_first = worst_steps(&stitched, |n| {
        blended(&s.rig, &probes[n], |b| light(n, b).map(round_f16))
    });
    let band = probes
        .iter()
        .filter(|p| s.rig.weights(p.d, p.lod).iter().all(|w| *w > 0.0))
        .count();
    eprintln!(
        "the stitch against the test's blend: {blend:.4} f16 steps at most over {band} blended \
         pixels; blended after rounding {rounded_first:.3}"
    );
    assert!(band > 5000, "{band}");
    assert!(blend <= NEAREST, "{blend}");
    let off = must_fail("the blend after rounding", at_most(rounded_first, NEAREST));
    assert!(off > 0.7, "{off}");

    // The frame term keeps every sample off the slot's edge: with θ_max
    // past the frame, lens 0 alone is black outside its coverage. With
    // the term defeated the edge's texels repeat past it, as streaks
    // (10.5's control).
    let shot = equirect(192);
    let outside: Vec<bool> = lens_coverage(&s.wide, 0, &shot.0, shot.1)
        .iter()
        .map(|w| *w == 0.0)
        .collect();
    let brightest_outside = |rig: &Rig| {
        let picture = s.render(rig, UNIT, &shot, RigDebug::Lens0);
        (0..outside.len())
            .filter(|n| outside[*n])
            .flat_map(|n| pixel(&picture, n))
            .fold(0.0, f64::max)
    };
    assert_eq!(brightest_outside(&s.wide), 0.0);
    let streak = must_fail(
        "the frame term defeated",
        at_most(
            brightest_outside(&frame_term_defeated(&s.wide)),
            4.0 / 255.0,
        ),
    );
    eprintln!(
        "the frame term defeated: {:.1} LSB outside lens 0's coverage",
        streak * 255.0
    );
}

/// Each lens's gain multiplies its linear light, after its EOTF and
/// before the blend.
#[test]
fn rig_gains_apply_in_linear_light() {
    let _t = timed("rig_gains_apply_in_linear_light");
    let s = scene();
    let rig = &s.rig;
    // Lens 1 two stops up with 5 % more red, so that its brighter values
    // pass 1 and clamp, and lens 0 turned down by channel, so that the
    // two lenses' gains can't be taken for each other.
    let gains = [[0.9, 1.0, 0.8], [4.2, 4.0, 4.0]];
    let shot = right(192);
    let probes = view_probes(rig, &shot);
    let light = |n: usize, b: usize| {
        let p = &probes[n];
        s.planes[rig.lenses[b].track].light(p.texel[b].expect("a texel"), p.lod[b])
    };
    let gained = |rgb: [f64; 3], gain: [f64; 3]| [0, 1, 2].map(|c| rgb[c] * gain[c]);

    let stitched = s.render(rig, gains, &shot, RigDebug::Off);
    let against = |lit: &dyn Fn(usize, usize) -> [f64; 3]| {
        worst_steps(&stitched, |n| blended(rig, &probes[n], |b| lit(n, b)))
    };
    let linear = against(&|n, b| gained(light(n, b), gains[b]));
    let on_display = against(&|n, b| gained(light(n, b).map(oetf), gains[b]).map(eotf));
    let one_gain = against(&|n, b| gained(light(n, b), gains[0]));
    let no_gain = against(&|n, b| light(n, b));
    // The pixels the target clamps: a channel of their own blend passes 1.
    let clamped = (0..probes.len())
        .filter(|n| {
            blended(rig, &probes[*n], |b| gained(light(*n, b), gains[b]))
                .iter()
                .any(|v| *v > 1.0)
        })
        .count();
    eprintln!(
        "gains in linear light: {linear:.4} f16 steps at most, {clamped} pixels at the clamp; on \
         display values {on_display:.0}, lens 0's on both {one_gain:.0}, none {no_gain:.0}"
    );
    assert!(clamped > 100, "{clamped}");
    assert!(linear <= NEAREST, "{linear}");
    let off = must_fail("the gain on display values", at_most(on_display, NEAREST));
    assert!(off > 100.0, "{off}");
    must_fail("lens 0's gain on both lenses", at_most(one_gain, NEAREST));
    must_fail("the gains left out", at_most(no_gain, NEAREST));

    // A lens alone is drawn with its gain, and the seam difference is of
    // the gained lenses.
    let alone = s.render(rig, gains, &shot, RigDebug::Lens1);
    let covered = |n: usize, b: usize| rig.covers(b, probes[n].d, probes[n].lod[b]);
    let lens_1 = worst_steps(&alone, |n| {
        if covered(n, 1) {
            gained(light(n, 1), gains[1])
        } else {
            BLACK
        }
    });
    assert!(lens_1 <= NEAREST, "{lens_1}");
    let difference = s.render(rig, gains, &shot, RigDebug::SeamDiff);
    let mut worst = [0.0f64; 2];
    for n in (0..probes.len()).filter(|n| covered(*n, 0) && covered(*n, 1)) {
        let lit = [0, 1].map(|b| gained(light(n, b), gains[b]));
        let unlit = [0, 1].map(|b| light(n, b));
        for c in 0..3 {
            let got = difference.data[3 * n + c];
            let off = |of: &[[f64; 3]; 2]| linear_steps_off(got, (of[0][c] - of[1][c]).abs() * 8.0);
            worst = [worst[0].max(off(&lit)), worst[1].max(off(&unlit))];
        }
    }
    assert!(worst[0] <= NEAREST, "{worst:?}");
    must_fail(
        "the seam difference of the lenses before their gains",
        at_most(worst[1], NEAREST),
    );

    // A gain is finite and 0 or more.
    for bad in [f64::NAN, f64::INFINITY, -0.5] {
        let mut gains = UNIT;
        gains[1][2] = bad;
        let opts = RigOptions::default();
        let refused = render_rig(s.lenses(), rig, gains, &shot.0, (8, 8), &opts);
        assert!(
            matches!(&refused, Err(RenderError::Invalid(why)) if why.contains("gain")),
            "{bad}: {refused:?}"
        );
    }
}

/// The view's dim multiplies the blend in linear light past the view's
/// frame, before the target rounds it, and `level0` takes each lens's
/// taps from level 0 alone while the mask and the weights stay at the
/// footprint's LOD (10.3: both as `render` has them).
#[test]
fn rig_dims_past_the_frame_and_level0_takes_level_0() {
    let _t = timed("rig_dims_past_the_frame_and_level0_takes_level_0");
    // Neither needs the scene's size: the quarter-size camera.
    let camera = reversed();
    let (planes, rig) = (&camera.planes, &camera.rig);
    let render = |(view, size): &Shot, debug: RigDebug, level0: bool| {
        let opts = RigOptions { debug, level0 };
        render_rig(camera.lenses(), rig, UNIT, view, *size, &opts).expect("the pair renders")
    };

    // The seam view's lens over a frame of 32² in the middle of a 64²
    // target, and what lies past the frame at 0.4 of its light: not a
    // power of two, which would round as the undimmed value does.
    let dim = 0.4;
    let (view, size) = right(64);
    let framing = Framing {
        rect: [16.0, 16.0, 32.0, 32.0],
        dim,
    };
    let shot = (view.framed(framing), size);
    let probes = view_probes(rig, &shot);
    let framed = |n: usize| {
        let (i, j) = (n as u32 % size.0, n as u32 / size.0);
        (16..48).contains(&i) && (16..48).contains(&j)
    };
    let dim_at = |n: usize| if framed(n) { 1.0 } else { dim };
    // What the test works out once for each pixel: the weights, each
    // lens's light where its mask covers, and their blend.
    let weights: Vec<[f64; 2]> = probes.iter().map(|p| rig.weights(p.d, p.lod)).collect();
    let covers = |n: usize, b: usize| rig.covers(b, probes[n].d, probes[n].lod[b]);
    let lights: Vec<[[f64; 3]; 2]> = (0..probes.len())
        .map(|n| {
            [0, 1].map(|b| {
                let p = &probes[n];
                if covers(n, b) {
                    planes[rig.lenses[b].track].light(p.texel[b].expect("a texel"), p.lod[b])
                } else {
                    BLACK
                }
            })
        })
        .collect();
    let blends: Vec<[f64; 3]> = (0..probes.len())
        .map(|n| mixed(weights[n], |b| lights[n][b]))
        .collect();

    // The stitch, and a lens alone, which ends as the stitch does.
    let stitched = render(&shot, RigDebug::Off, false);
    let against = |dimmed: &dyn Fn(f64, usize) -> f64| {
        worst_steps(&stitched, |n| blends[n].map(|v| dimmed(v, n)))
    };
    let in_linear = against(&|v, n| v * dim_at(n));
    let after_rounding = against(&|v, n| round_f16(v) * dim_at(n));
    let on_display = against(&|v, n| eotf(oetf(v) * dim_at(n)));
    let everywhere = against(&|v, _| v * dim);
    let nowhere = against(&|v, _| v);
    let alone = render(&shot, RigDebug::Lens1, false);
    let lens_1 = worst_steps(&alone, |n| lights[n][1].map(|v| v * dim_at(n)));
    eprintln!(
        "the dim, 0.4 past a frame of 32² in 64²: {in_linear:.4} f16 steps at most in linear \
         light, a lens alone {lens_1:.4}; after rounding {after_rounding:.3}, on display values \
         {on_display:.0}, inside the frame too {everywhere:.0}, left out {nowhere:.0}"
    );
    assert!(in_linear <= NEAREST, "{in_linear}");
    assert!(lens_1 <= NEAREST, "{lens_1}");
    let off = must_fail(
        "the dim after the target rounds",
        at_most(after_rounding, NEAREST),
    );
    assert!(off > 0.7, "{off}");
    let off = must_fail("the dim on display values", at_most(on_display, NEAREST));
    assert!(off > 100.0, "{off}");
    must_fail("the dim inside the frame too", at_most(everywhere, NEAREST));
    must_fail("the dim left out", at_most(nowhere, NEAREST));

    // Modes 6 and 7 are written as they are measured, with no dim.
    let written = render(&shot, RigDebug::Weights, false);
    let difference = render(&shot, RigDebug::SeamDiff, false);
    let (mut dimmed_weights, mut both_past) = (0usize, 0usize);
    let mut worst = [0.0f64; 2];
    for (n, w) in weights.iter().enumerate() {
        assert_eq!(pixel(&written, n), [w[0], 0.0, w[1]].map(round_f16), "{n}");
        let dimmed = [w[0], 0.0, w[1]].map(|w| round_f16(w * dim_at(n)));
        dimmed_weights += usize::from(pixel(&written, n) != dimmed);
        if !(covers(n, 0) && covers(n, 1)) {
            assert_eq!(pixel(&difference, n), BLACK, "pixel {n}");
            continue;
        }
        both_past += usize::from(!framed(n));
        for (c, got) in pixel(&difference, n).into_iter().enumerate() {
            let apart = 8.0 * (lights[n][0][c] - lights[n][1][c]).abs();
            worst = [
                worst[0].max(linear_steps_off(got, apart)),
                worst[1].max(linear_steps_off(got, apart * dim_at(n))),
            ];
        }
    }
    assert!(both_past > 100, "{both_past}");
    assert!(worst[0] <= NEAREST, "{worst:?}");
    let off = must_fail("the weights dimmed", at_most(dimmed_weights, 0));
    assert!(off > 1000, "{off}");
    must_fail("the seam difference dimmed", at_most(worst[1], NEAREST));

    // Level 0 alone, on a small equirect: each lens's taps are level
    // 0's, and the mask and the weights still the footprint's, as mode 6
    // shows them.
    let shot = equirect(96);
    let probes = view_probes(rig, &shot);
    let picture = render(&shot, RigDebug::Off, true);
    // The test's blend of each lens's taps at `tap_lod` of its own LOD,
    // under the weights at `mask_lod` of it.
    let against = |tap_lod: &dyn Fn(f64) -> f64, mask_lod: &dyn Fn(f64) -> f64| {
        worst_steps(&picture, |n| {
            let p = &probes[n];
            mixed(rig.weights(p.d, p.lod.map(mask_lod)), |b| {
                let texel = p.texel[b].expect("a texel");
                planes[rig.lenses[b].track].light(texel, tap_lod(p.lod[b]))
            })
        })
    };
    let own = |lod: f64| lod;
    // A lens with no footprint has no LOD, at level 0 either.
    let zero = |lod: f64| if lod.is_finite() { 0.0 } else { lod };
    let (at_level_0, trilinear, mask_at_0) = (
        against(&zero, &own),
        against(&own, &own),
        against(&zero, &zero),
    );
    let lods = probes
        .iter()
        .flat_map(|p| p.lod)
        .filter(|lod| lod.is_finite())
        .fold([f64::INFINITY, f64::NEG_INFINITY], |range, lod| {
            [range[0].min(lod), range[1].max(lod)]
        });
    eprintln!(
        "level 0 alone on a 96 × 48 equirect, footprints of LOD {:.2} to {:.2}: {at_level_0:.4} \
         f16 steps at most from the test's level-0 taps; from its trilinear taps {trilinear:.0}, \
         under the mask at LOD 0 {mask_at_0:.0}",
        lods[0], lods[1]
    );
    // Every footprint is past level 0, so its trilinear taps aren't level
    // 0's.
    assert!(lods[0] > 0.5, "{lods:?}");
    assert!(at_level_0 <= NEAREST, "{at_level_0}");
    let off = must_fail(
        "the trilinear taps for level 0's",
        at_most(trilinear, NEAREST),
    );
    assert!(off > 100.0, "{off}");
    let off = must_fail(
        "the mask at LOD 0 under level0",
        at_most(mask_at_0, NEAREST),
    );
    assert!(off > 100.0, "{off}");
    let written = render(&shot, RigDebug::Weights, true);
    for (n, p) in probes.iter().enumerate() {
        let w = rig.weights(p.d, p.lod);
        assert_eq!(pixel(&written, n), [w[0], 0.0, w[1]].map(round_f16), "{n}");
    }
}

/// Modes 4 and 5 draw each lens alone, 6 the weights (red w0, blue w1)
/// and 7 the seam difference where both cover; the stitch they are read
/// beside is black where neither lens weighs.
#[test]
fn rig_debug_modes_4_to_7() {
    let _t = timed("rig_debug_modes_4_to_7");
    let s = scene();
    let rig = &s.rig;
    let mut largest = 0.0f64;
    let (mut means, mut both_cover) = ([0.0f64; 2], 0usize);
    // The seam on the right at the source's own scale, whose seam
    // difference is reported, and the whole sphere, minified.
    let seam = right_close(192);
    for shot in [seam, equirect(128)] {
        let reported = shot == seam;
        let probes = view_probes(rig, &shot);
        let mode = |debug| s.render(rig, UNIT, &shot, debug);
        let stitched = mode(RigDebug::Off);
        let alone = [mode(RigDebug::Lens0), mode(RigDebug::Lens1)];
        let weights = mode(RigDebug::Weights);
        let difference = mode(RigDebug::SeamDiff);
        let (mut one_lens, mut swapped, mut reds, mut unscaled) =
            ([0usize; 2], 0usize, 0usize, 0.0f64);
        let (mut gap, mut drawn_in_wider) = (0usize, 0usize);
        for (n, p) in probes.iter().enumerate() {
            let w = rig.weights(p.d, p.lod);
            let covers = [0, 1].map(|b| rig.covers(b, p.d, p.lod[b]));
            // The stitch is black where neither lens weighs, and only
            // there: where the masks' margins, which grow with the LOD,
            // leave a gap at the seam. A margin an LOD wider leaves a
            // wider gap, which the stitch draws in.
            if w == [0.0; 2] {
                assert_eq!(pixel(&stitched, n), BLACK, "pixel {n}");
                gap += 1;
            } else {
                assert_ne!(pixel(&stitched, n), BLACK, "pixel {n}");
            }
            if !reported {
                let wider = rig.weights(p.d, p.lod.map(|lod| lod + 1.0));
                drawn_in_wider += usize::from(wider == [0.0; 2] && pixel(&stitched, n) != BLACK);
            }
            // Modes 4 and 5: the stitch where the other lens weighs
            // nothing, to the bit; black where the lens doesn't cover.
            for b in 0..2 {
                if !covers[b] {
                    assert_eq!(pixel(&alone[b], n), BLACK, "lens {b}, pixel {n}");
                }
                if w[b] > 0.0 && w[1 - b] == 0.0 {
                    assert_eq!(
                        pixel(&alone[b], n),
                        pixel(&stitched, n),
                        "lens {b}, pixel {n}"
                    );
                    one_lens[b] += 1;
                    swapped += usize::from(pixel(&alone[1 - b], n) != pixel(&stitched, n));
                }
            }
            // Mode 6: block 0's weight in red and block 1's in blue, as
            // the nearest f16, which sum to 1 wherever a lens covers.
            let [red, green, blue] = pixel(&weights, n);
            assert_eq!([red, green, blue], [round_f16(w[0]), 0.0, round_f16(w[1])]);
            if covers[0] || covers[1] {
                assert!((red + blue - 1.0).abs() <= 2f64.powi(-11), "{red} + {blue}");
            } else {
                assert_eq!(red + blue, 0.0, "pixel {n}");
            }
            reds += usize::from([red, blue] != [round_f16(w[1]), round_f16(w[0])]);
            // Mode 7: 8 |lin0 − lin1| where both cover, as the nearest
            // f16; black elsewhere.
            if covers[0] && covers[1] {
                let light = [0, 1].map(|b| {
                    s.planes[rig.lenses[b].track].light(p.texel[b].expect("a texel"), p.lod[b])
                });
                for (c, got) in pixel(&difference, n).into_iter().enumerate() {
                    let apart = (light[0][c] - light[1][c]).abs();
                    let off = linear_steps_off(got, 8.0 * apart);
                    assert!(off <= NEAREST, "pixel {n}: {got} for {apart}, {off} steps");
                    unscaled = unscaled.max(linear_steps_off(got, apart));
                    if reported {
                        largest = largest.max(got);
                        means[0] += got;
                    }
                }
                both_cover += usize::from(reported);
            } else {
                assert_eq!(pixel(&difference, n), BLACK, "pixel {n}");
            }
        }
        assert!(one_lens[0] > 1000 && one_lens[1] > 1000, "{one_lens:?}");
        let off = must_fail("mode 5 taken for mode 4", at_most(swapped, 0));
        assert!(off > 1000, "{off}");
        let off = must_fail("the weights' red taken for block 1's", at_most(reds, 0));
        assert!(off > 1000, "{off}");
        must_fail(
            "the seam difference without its × 8",
            at_most(unscaled, NEAREST),
        );
        // At the source's own scale the lenses' masks overlap all along
        // the seam; on the small equirect they leave a gap.
        if reported {
            assert_eq!(gap, 0);
        } else {
            let (pixels, wider) = (probes.len(), drawn_in_wider + gap);
            eprintln!(
                "the stitch at {} × {}: black at the {gap} of its {pixels} pixels that \
                 neither lens weighs; a margin an LOD wider leaves {wider}",
                shot.1.0, shot.1.1
            );
            assert!(gap > 100, "{gap}");
            let off = must_fail(
                "the gap of a margin an LOD wider",
                at_most(drawn_in_wider, 0),
            );
            assert!(off > 100, "{off}");
        }
    }
    means[0] /= (3 * both_cover) as f64;

    // Red is the front lens's: straight ahead (−Z, an equirect's centre)
    // it is 1, and straight behind blue is.
    let (view, size) = equirect(64);
    let weights = s.render(rig, UNIT, &(view, size), RigDebug::Weights);
    assert_eq!(weights.at(32, 16), [1.0, 0.0, 0.0]);
    assert_eq!(weights.at(0, 16), [0.0, 0.0, 1.0]);

    // What mode 7 reads on the true rig is what two lenses' samplings of
    // one scene leave. A lens out of place raises it: lens 1 yawed 0.5°.
    let yawed = {
        let mut tune = tune(100.0);
        tune.lenses[1].ypr_deg[0] = 0.5;
        sphere_rig(&x5_like(), &x5_like_planted(), &tune, 0, [SIDE; 2]).expect("the rig builds")
    };
    let probes = view_probes(&yawed, &seam);
    let difference = s.render(&yawed, UNIT, &seam, RigDebug::SeamDiff);
    let mut yawed_cover = 0usize;
    for (n, p) in probes.iter().enumerate() {
        if yawed.covers(0, p.d, p.lod[0]) && yawed.covers(1, p.d, p.lod[1]) {
            means[1] += pixel(&difference, n).iter().sum::<f64>();
            yawed_cover += 1;
        }
    }
    means[1] /= (3 * yawed_cover) as f64;
    eprintln!(
        "mode 7 on the true rig, a 45° view of the seam at 192²: largest {largest:.4} (a linear \
         difference of {:.5}), mean {:.5} over {both_cover} pixels both lenses cover; with lens 1 \
         yawed 0.5° the mean is {:.5}",
        largest / 8.0,
        means[0],
        means[1]
    );
    let off = must_fail("lens 1 yawed 0.5°", at_most(means[1], 2.0 * means[0]));
    assert!(off > 3.0 * means[0], "{off} against {}", means[0]);
}

/// `lenses` go by track and the rig's lenses by block: block b samples
/// its own track's picture, under a reversed binding too, where the
/// gains and the strips still go by block.
#[test]
fn rig_lenses_come_from_their_tracks() {
    let _t = timed("rig_lenses_come_from_their_tracks");
    let camera = reversed();
    let (planes, rig) = (&camera.planes, camera.rig);
    assert_eq!(planes[0].side(), QUARTER);

    let by_track = camera.lenses();
    let by_block: [&dyn VideoPlanes; 2] = [by_track[1], by_track[0]];
    let shot = equirect(256);
    let (view, size) = &shot;
    let probes = view_probes(&rig, &shot);
    let render = |lenses: [&dyn VideoPlanes; 2], gains: [[f64; 3]; 2], debug: RigDebug| {
        let opts = RigOptions {
            debug,
            level0: false,
        };
        render_rig(lenses, &rig, gains, view, *size, &opts).expect("the pair renders")
    };

    // Each block alone is the test's taps of its own track's planes
    // times the block's own gain: under this binding block 0's are track
    // 1's planes, with `gains[0]`. Of the planes at the block's own index
    // it isn't, nor with its track's gain.
    let gains = [[0.9, 1.0, 0.8], [0.5, 0.6, 0.7]];
    let (mut own, mut indexed, mut of_track) = (0.0f64, 0.0f64, 0.0f64);
    for (b, debug) in [(0, RigDebug::Lens0), (1, RigDebug::Lens1)] {
        let picture = render(by_track, gains, debug);
        let track = rig.lenses[b].track;
        for (n, p) in probes.iter().enumerate() {
            if !rig.covers(b, p.d, p.lod[b]) {
                assert_eq!(pixel(&picture, n), BLACK, "lens {b}, pixel {n}");
                continue;
            }
            let texel = p.texel[b].expect("a texel");
            let light = [track, b].map(|track| planes[track].light(texel, p.lod[b]));
            let off = |light: [f64; 3], gain: [f64; 3]| {
                pixel_steps(&picture, n, [0, 1, 2].map(|c| light[c] * gain[c]))
            };
            own = own.max(off(light[0], gains[b]));
            indexed = indexed.max(off(light[1], gains[b]));
            of_track = of_track.max(off(light[0], gains[track]));
        }
    }
    assert!(own <= NEAREST, "{own}");
    let planes_off = must_fail(
        "each block's planes at its own index",
        at_most(indexed, NEAREST),
    );
    assert!(planes_off > 100.0, "{planes_off}");
    let gains_off = must_fail("the gains by track", at_most(of_track, NEAREST));
    assert!(gains_off > 100.0, "{gains_off}");

    // And the strips come back by block: block b's strip is the test's
    // taps of its own track's planes at its own texels, unrounded, which
    // the other block's strip isn't.
    let spec = StripSpec {
        px_per_deg: 1.0,
        ..StripSpec::TIER1
    };
    let unwrapped = strips(&rig, by_track, &spec).expect("the strips");
    let strip_probes = crate::probes(&rig, |x, y| spec.dir(x, y), spec.size());
    let (mut both_valid, mut strip_own, mut strip_swapped) = (0usize, 0.0f64, 0.0f64);
    for (n, p) in strip_probes.iter().enumerate() {
        let valid = [0, 1].map(|b| unwrapped.lenses[b].valid[n]);
        assert_eq!(valid, [0, 1].map(|b| rig.covers(b, p.d, p.lod[b])), "{n}");
        if valid != [true; 2] {
            continue;
        }
        both_valid += 1;
        for b in 0..2 {
            let texel = p.texel[b].expect("a texel");
            let want = planes[rig.lenses[b].track].light(texel, p.lod[b]);
            for (c, want) in want.into_iter().enumerate() {
                let off = |strip: usize| (unwrapped.lenses[strip].rgb[n][c] - want).abs();
                strip_own = strip_own.max(off(b));
                strip_swapped = strip_swapped.max(off(rig.lenses[b].track));
            }
        }
    }
    assert!(both_valid > 4000, "{both_valid}");
    assert!(strip_own <= 1e-12, "{strip_own}");
    let strips_off = must_fail("the strips by track", at_most(strip_swapped, 1e-12));
    eprintln!(
        "the reversed camera, each lens alone: {own:.4} f16 steps at most from its track's planes \
         and its block's gain; from the planes at its index {planes_off:.0}, with its track's \
         gain {gains_off:.0}; its strips over {both_valid} samples both lenses hold: \
         {strip_own:.1e} from the test's taps by block, {strips_off:.3} by track"
    );
    assert!(strips_off > 0.01, "{strips_off}");

    // And the stitch shows the scene: against the ideal more than 12°
    // from the seam it holds what a quarter-size lens can, and with the
    // planes handed over by block it shows each half of the sphere where
    // the other belongs.
    let ideal = render_view_ideal(
        Pattern::SeamLines,
        |x, y| Some(view.dir(*size, x, y)),
        *size,
        4,
    );
    let ideal: Vec<f64> = ideal.pixels.iter().flatten().copied().collect();
    let far = band_weights(&rig, view, *size, 12.0, 90.0);
    let against_ideal = |lenses: [&dyn VideoPlanes; 2]| {
        let picture = render(lenses, UNIT, RigDebug::Off);
        let (w, h) = (size.0 as usize, size.1 as usize);
        let weights = Weights {
            width: w,
            height: h,
            w: &far,
        };
        psnr_weighted(
            View::new(w, h, 3, &picture.data),
            View::new(w, h, 3, &ideal),
            &weights,
        )
        .expect("the pictures compare")
    };
    let stitched = against_ideal(by_track);
    let handed_over = must_fail(
        "the planes by block under a reversed binding",
        at_least(against_ideal(by_block), stitched - 6.0),
    );
    eprintln!(
        "the reversed camera against the ideal, more than 12° from the seam: {stitched:.2} dB by \
         track, {handed_over:.2} dB by block"
    );
    assert!(stitched > 20.0, "{stitched}");

    // A rig whose block names a track the pair doesn't have is refused.
    let mut third = rig;
    third.lenses[1].track = 2;
    let refused = render_rig(by_track, &third, UNIT, view, (8, 4), &RigOptions::default());
    assert!(
        matches!(&refused, Err(RenderError::Invalid(why)) if why.contains("track 2")),
        "{refused:?}"
    );
}

/// A strip's sample outside its lens's mask is marked invalid, never
/// clamped to the slot's edge (8.2, 10.3).
#[test]
fn strips_mark_samples_outside_invalid() {
    let _t = timed("strips_mark_samples_outside_invalid");
    let s = scene();

    // Tier 1's strips of the true rig: both wholly inside their masks,
    // the band of ±8° lying inside the overlap with room for the margin
    // (8.2).
    let began = Instant::now();
    let tier1 = strips(&s.rig, s.lenses(), &StripSpec::TIER1).expect("the strips");
    eprintln!(
        "strips at Tier 1's spec, 1372 × 61 a lens: {:.1} ms",
        began.elapsed().as_secs_f64() * 1e3
    );
    for strip in &tier1.lenses {
        assert_eq!((strip.width, strip.height), (1372, 61));
        assert_eq!((strip.rgb.len(), strip.valid.len()), (1372 * 61, 1372 * 61));
        assert!(strip.valid.iter().all(|v| *v));
    }

    // A band of ±13° passes each lens's mask: at θ_max 100° the mask's θ
    // bound ends it, and at 104° the slot's frame does. A sample is valid
    // exactly where `Rig::covers` at its own footprint's LOD; a valid one
    // is the test's own taps, unrounded, none of which reads past the
    // picture's edge; an invalid one is 0.
    let spec = StripSpec {
        px_per_deg: 1.5,
        half_band_deg: 13.0,
        ..StripSpec::TIER1
    };
    let size = spec.size();
    assert_eq!(size, (540, 39));
    let dir = |x: f64, y: f64| spec.dir(x, y);
    // The valid samples, the invalid ones, those the frame term alone
    // leaves out, and the valid ones that read past the edge.
    let survey = |rig: &Rig, masked_by: &Rig| {
        let strips = strips(rig, s.lenses(), &spec).expect("the strips");
        let probes = probes(rig, dir, size);
        let (mut valid, mut invalid, mut framed, mut clamped) = (0usize, 0usize, 0usize, 0usize);
        let (mut worst, mut worst_rounded) = (0.0f64, 0.0f64);
        for b in 0..2 {
            let strip = &strips.lenses[b];
            assert_eq!((strip.width, strip.height), size);
            let planes = &s.planes[rig.lenses[b].track];
            for (n, p) in probes.iter().enumerate() {
                assert_eq!(
                    strip.valid[n],
                    rig.covers(b, p.d, p.lod[b]),
                    "lens {b}, {n}"
                );
                if !strip.valid[n] {
                    assert_eq!(strip.rgb[n], BLACK, "lens {b}, {n}");
                    invalid += 1;
                    let lens = &masked_by.lenses[b];
                    let margin = rig.margin_texels * p.lod[b].max(0.0).exp2();
                    let within = off_axis(rig.ray(b, p.d))
                        <= lens.theta_max - margin / lens.texels_per_radian;
                    framed += usize::from(within);
                    continue;
                }
                valid += 1;
                let texel = p.texel[b].expect("a texel");
                clamped += usize::from(planes.reads_past_the_edge(texel, p.lod[b]));
                let want = planes.light(texel, p.lod[b]);
                for (got, want) in strip.rgb[n].into_iter().zip(want) {
                    worst = worst.max((got - want).abs());
                    worst_rounded = worst_rounded.max((got - round_f16(want)).abs());
                }
            }
        }
        (valid, invalid, framed, clamped, worst, worst_rounded)
    };
    for (name, rig) in [("θ_max 100°", &s.rig), ("θ_max 104°", &s.wide)] {
        let (valid, invalid, framed, clamped, worst, worst_rounded) = survey(rig, rig);
        eprintln!(
            "strips of ±13° at {name}: {valid} samples valid, {invalid} invalid, {framed} of \
             them by the frame term alone; {worst:.1e} from the test's taps"
        );
        assert!(valid > 15_000 && invalid > 1000, "{valid} {invalid}");
        assert_eq!(clamped, 0, "{name}");
        assert!(worst <= 1e-12, "{worst}");
        must_fail("the strips rounded to f16", at_most(worst_rounded, 1e-12));
        if name == "θ_max 104°" {
            assert!(framed > 200, "{framed}");
        }
    }
    // With the frame term defeated, samples past the picture's edge are
    // valid, and repeat the edge's texels.
    let loose = frame_term_defeated(&s.wide);
    let (_, _, _, clamped, _, _) = survey(&loose, &s.wide);
    let off = must_fail("the frame term defeated", at_most(clamped, 0));
    eprintln!("the frame term defeated: {off} valid samples read past the picture's edge");
    assert!(off > 30, "{off}");

    // A spec with no strip to unwrap is refused.
    for bad in [
        StripSpec {
            px_per_deg: 0.0,
            ..StripSpec::TIER1
        },
        StripSpec {
            px_per_deg: f64::NAN,
            ..StripSpec::TIER1
        },
        StripSpec {
            half_band_deg: 91.0,
            ..StripSpec::TIER1
        },
        StripSpec {
            px_per_deg: 0.001,
            ..StripSpec::TIER1
        },
        StripSpec {
            px_per_deg: 1e4,
            ..StripSpec::TIER1
        },
    ] {
        let refused = strips(&s.rig, s.lenses(), &bad);
        assert!(
            matches!(&refused, Err(RenderError::Invalid(why)) if why.contains("strip")),
            "{bad:?}: {:?}",
            refused.map(|_| ())
        );
    }
}

/// `band_weights` and `lens_coverage` are the pixels `render_rig` draws:
/// each lens's coverage is where modes 4 and 5 draw it (8.1).
#[test]
fn masks_match_what_render_rig_draws() {
    let _t = timed("masks_match_what_render_rig_draws");
    let s = scene();

    // 4.2's equirect output: the band from 0° to 90° is every pixel the
    // stitch draws, all of them here.
    let shot = equirect(512);
    let began = Instant::now();
    let stitched = s.render(&s.rig, UNIT, &shot, RigDebug::Off);
    eprintln!(
        "render_rig, the equirect at 512 × 256: {:.1} ms on {} threads",
        began.elapsed().as_secs_f64() * 1e3,
        threads()
    );
    let all = band_weights(&s.rig, &shot.0, shot.1, 0.0, 90.0);
    assert_eq!(all.len(), 512 * 256);
    for (n, weight) in all.iter().enumerate() {
        assert_eq!((*weight, pixel(&stitched, n) != BLACK), (1.0, true), "{n}");
    }

    // On a small equirect the masks' margins, which grow with the LOD,
    // leave a gap at the seam: neither lens's coverage holds it, the
    // stitch leaves it black (`rig_debug_modes_4_to_7`) and the band
    // leaves it out. By its angle alone, the band from 0° to 90° is
    // every pixel, the gap's too.
    let (view, size) = equirect(128);
    let all = band_weights(&s.rig, &view, size, 0.0, 90.0);
    let coverage = [0, 1].map(|b| lens_coverage(&s.rig, b, &view, size));
    let mut gap = 0usize;
    for (n, weight) in all.iter().enumerate() {
        let covered = coverage[0][n] + coverage[1][n] > 0.0;
        assert_eq!(*weight, if covered { 1.0 } else { 0.0 }, "pixel {n}");
        gap += usize::from(!covered);
    }
    let off = must_fail("the band where no lens covers", at_most(gap, 0));
    eprintln!(
        "the masks of a 128 × 64 equirect leave out {off} of its {} pixels, which no lens covers",
        all.len()
    );
    assert!(off > 100, "{off}");

    let (mut other_lens, mut off_the_axis) = (0usize, 0usize);
    let shots = [equirect(192), right(128), zenith(128), equirect(128)];
    for (name, rig, shots) in [
        ("the rig", &s.rig, &shots[..3]),
        ("θ_max 104°", &s.wide, &shots[3..]),
    ] {
        for shot in shots {
            let (view, size) = shot;
            let (w, h) = (size.0 as usize, size.1 as usize);
            let mode = |debug| s.render(rig, UNIT, shot, debug);
            let stitched = mode(RigDebug::Off);
            let alone = [mode(RigDebug::Lens0), mode(RigDebug::Lens1)];
            let coverage = [0, 1].map(|b| lens_coverage(rig, b, view, *size));
            let all = band_weights(rig, view, *size, 0.0, 90.0);
            let near = band_weights(rig, view, *size, 0.0, 12.0);
            let far = band_weights(rig, view, *size, 12.0, 90.0);
            let weight = |holds: bool| if holds { 1.0 } else { 0.0 };
            let mut counted = [0usize; 2];
            for n in 0..w * h {
                // A lens's coverage is where it is drawn alone, and the
                // whole band where anything is drawn at all.
                for b in 0..2 {
                    let drawn = pixel(&alone[b], n) != BLACK;
                    assert_eq!(coverage[b][n], weight(drawn), "{name}, lens {b}, {n}");
                    other_lens += usize::from((coverage[1 - b][n] == 1.0) != drawn);
                }
                let drawn = pixel(&stitched, n) != BLACK;
                assert_eq!(all[n], weight(drawn), "{name}, pixel {n}");
                assert_eq!(drawn, coverage[0][n] + coverage[1][n] > 0.0, "{name}, {n}");
                // The band is by the angle off the seam's plane, z = 0:
                // 90° less the angle to the nearer of ±Z.
                let (x, y) = ((n % w) as f64 + 0.5, (n / w) as f64 + 0.5);
                let d = view.dir(*size, x, y);
                let off_seam = 90.0 - d.z.abs().min(1.0).acos().to_degrees();
                if (off_seam - 12.0).abs() < 1e-9 {
                    continue;
                }
                let in_far = drawn && off_seam > 12.0;
                assert_eq!(far[n], weight(in_far), "{name}, pixel {n}");
                assert_eq!(near[n], weight(drawn && !in_far), "{name}, pixel {n}");
                counted[0] += usize::from(near[n] == 1.0);
                counted[1] += usize::from(far[n] == 1.0);
                // From the front lens's axis, −Z, 12° to 90° is another
                // band.
                let off_axis = (-d.z).clamp(-1.0, 1.0).acos().to_degrees();
                let band = drawn && (12.0..=90.0).contains(&off_axis);
                off_the_axis += usize::from(in_far != band);
            }
            assert!(
                counted[0] > 1000 && counted[1] > 1000,
                "{name}: {counted:?}"
            );
            if name != "the rig" {
                continue;
            }

            // The masks do their work as weights: more than 12° from the
            // seam the stitch is one lens's picture, each over its own
            // coverage, which ends 9.5° past the seam; within 12° it
            // isn't.
            for b in 0..2 {
                let masked = |band: &[f64]| -> Vec<f64> {
                    band.iter().zip(&coverage[b]).map(|(a, c)| a * c).collect()
                };
                let against = |mask: &[f64]| {
                    let weights = Weights {
                        width: w,
                        height: h,
                        w: mask,
                    };
                    psnr_weighted(
                        View::new(w, h, 3, &stitched.data),
                        View::new(w, h, 3, &alone[b].data),
                        &weights,
                    )
                };
                assert_eq!(against(&masked(&far)), Ok(f64::INFINITY), "lens {b}");
                must_fail(
                    "the band within 12° of the seam",
                    above(against(&masked(&near)).expect("a band"), 60.0),
                );
            }
            // A band no pixel lies in weighs nothing, which the metrics
            // refuse.
            let none = band_weights(rig, view, *size, 91.0, 95.0);
            assert!(none.iter().all(|w| *w == 0.0));
            let weights = Weights {
                width: w,
                height: h,
                w: &none,
            };
            let picture = View::new(w, h, 3, &stitched.data);
            assert!(psnr_weighted(picture, picture, &weights).is_err());
            assert_eq!(psnr(picture, picture), Ok(f64::INFINITY));
        }
    }
    let off = must_fail(
        "block 1's coverage for block 0's picture",
        at_most(other_lens, 0),
    );
    assert!(off > 50_000, "{off}");
    let off = must_fail(
        "the band off the front lens's axis",
        at_most(off_the_axis, 0),
    );
    assert!(off > 10_000, "{off}");
}
