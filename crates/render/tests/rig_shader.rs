//! The shader's stitch on lavapipe (M3 plan 10.1 to 10.4): `fs_rig`
//! held to `reference::render_rig` and its debug modes 4 to 7, the
//! weights, ties, mask and level 0 as amendment 5 corrects them, a pair
//! blurred and coded to YUV, what `render_from` and `rig::pack` refuse,
//! and the strip pass against `seam::strips_of`. M3.7 unit (a) writes
//! them.
//!
//! The CPU rule here is fixed, never blessed: a picture holds when its
//! PSNR against `reference::render_rig_of`'s is 40 dB or more and no
//! channel of any pixel is more than 0.1 off ([`rule`]). The pictures are
//! made here or by synth at 192² a lens, the X5-like camera composed at
//! that side and sphere's rig of it ([`small`]), except where a test
//! names 768². Every test prints its own time and the binary's, of the
//! binary's [`BUDGET_S`] (19.6's Tier 1 share).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use glam::{DQuat, DVec2, DVec3};
use spherecut_core::video::{ChromaSiting, Matrix, Range, VideoPlanes};
use spherecut_render::compare::{View, psnr, sample_diff};
use spherecut_render::readback::round_f16;
use spherecut_render::reference::{
    ImageF64, LensPair, RigDebug, RigOptions, lens_footprint, render_rig_of, rig_lod_map, srgb_oetf,
};
use spherecut_render::rig::{SlotGeom, pack};
use spherecut_render::seam::{StripSpec, Strips, strip_coverage, strips_of};
use spherecut_render::yuv::{self, YuvLayout};
use spherecut_render::{
    DebugMode, Depth, GpuFrame, Image, LensFrame as StillLens, LinearTarget, PlaneSource,
    RenderError, RigUniform, SlotShape, SourceFrames, SourceRef, StillOptions, StillRenderer,
    ViewParams, wgpu,
};
use spherecut_sphere::fisheye::{Canonical, KbLens, LensModel, Model, Unified};
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_sphere::rig::{LensTune, Rig, RigLens, Tune};
use spherecut_synth::Pattern;
use spherecut_synth::lens::{Generation, RigTruth, compose, kb_like, x5_like, x5_like_planted};
use spherecut_synth::rung::{FRONT_TRACK, LensFrame, far, pair_through};
use spherecut_testkit::control::{above, at_most, must_fail};

use common::rig::{pats, rig_of_truth, sphere_rig};
use common::rung1::{NEAR_M, UNIT, near, turned};
use common::{Frame, Kind, color, gpu};

// The clock.

/// The binary's share of the slice's Tier 1 time, in seconds (19.6).
const BUDGET_S: f64 = 0.8;

/// The binary's tests.
const TESTS: [&str; 15] = [
    "fs_rig_matches_render_rig_of",
    "fs_rig_modes_4_to_7_follow_the_blocks",
    "fs_rig_weights_light_the_ring_no_feather_weighs",
    "fs_rig_ties_like_the_mirror_pair",
    "fs_rig_tests_the_mask_in_angle",
    "fs_rig_level0_keeps_the_footprints_mask",
    "fs_rig_lens_without_an_image_doesnt_cover",
    "fs_rig_blurs_a_pair_in_linear_light",
    "render_yuv_takes_a_pair",
    "render_from_refuses_what_has_no_gpu_form",
    "pack_refuses_what_the_gpu_cant_draw",
    "pack_keeps_a_side_unlike_its_slot",
    "record_refuses_a_rig_packed_for_other_slots",
    "gpu_strips_match_strips_of",
    "rig_lod_map_matches_mode_3",
];

/// When the binary's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and how
/// long the binary has run. The tests run side by side, so the last line
/// printed is the binary's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    assert!(TESTS.contains(&name), "{name} isn't among TESTS");
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let binary = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the binary {binary:.3} s so far, of its {BUDGET_S} s",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

// The CPU rule.

/// The CPU rule's floor on PSNR, in dB, and its most any channel may be
/// off, in unit scale.
const RULE_DB: f64 = 40.0;
const RULE_CHANNEL: f64 = 0.1;

/// What a picture that held the rule came to.
#[derive(Debug, Clone, Copy)]
struct Held {
    db: f64,
    worst: f64,
}

/// `got` against `want`, R, G and B in unit scale (any number of
/// samples, three a pixel), under the CPU rule: PSNR 40 dB or more and
/// no channel more than 0.1 off; NaN is infinitely far.
fn rule(got: &[f64], want: &[f64]) -> Result<Held, String> {
    assert_eq!(got.len(), want.len(), "the pictures' sizes");
    let n = got.len() / 3;
    let db = psnr(View::new(n, 1, 3, got), View::new(n, 1, 3, want)).unwrap();
    let worst = got
        .iter()
        .zip(want)
        .map(|(a, b)| sample_diff(*a, *b))
        .fold(0.0, f64::max);
    if db >= RULE_DB && worst <= RULE_CHANNEL {
        Ok(Held { db, worst })
    } else {
        Err(format!(
            "{db:.2} dB, a channel {worst:.4} off: the rule is {RULE_DB} dB and {RULE_CHANNEL}"
        ))
    }
}

/// [`rule`] of a GPU picture against the reference's.
fn image_rule(got: &Image, want: &ImageF64) -> Result<Held, String> {
    assert_eq!((got.width, got.height), (want.width, want.height));
    rule(&got.unit_rgb(), &want.data)
}

/// The step between f16 values about `v`.
fn f16_step(v: f64) -> f64 {
    let v = v.abs();
    if v < 2f64.powi(-14) {
        2f64.powi(-24)
    } else {
        2f64.powf(v.log2().floor() - 10.0)
    }
}

/// Whether `a` and `b` are within an f16 step of each other.
fn f16_near(a: f64, b: f64) -> bool {
    sample_diff(a, b) <= f16_step(a.abs().max(b.abs()))
}

/// The share of pixels whose every channel is within an f16 step, and
/// the worst channel's difference.
fn near_share(got: &[f64], want: &[f64]) -> (f64, f64) {
    let pixels = got.len() / 3;
    let near = got
        .chunks_exact(3)
        .zip(want.chunks_exact(3))
        .filter(|(a, b)| a.iter().zip(*b).all(|(a, b)| f16_near(*a, *b)))
        .count();
    let worst = got
        .iter()
        .zip(want)
        .map(|(a, b)| sample_diff(*a, *b))
        .fold(0.0, f64::max);
    (near as f64 / pixels as f64, worst)
}

/// The sRGB EOTF in f64.
fn eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

// The renderer and the frames.

/// One renderer for the binary: its pipelines are made once, the pair's
/// side by side as it is made, since the tests draw in turn under its
/// lock and building them one at a time there was a third of the
/// binary's time.
fn renderer() -> MutexGuard<'static, StillRenderer> {
    static RENDERER: OnceLock<Mutex<StillRenderer>> = OnceLock::new();
    RENDERER
        .get_or_init(|| {
            let made = StillRenderer::new(gpu());
            let (projector, filter) = (made.projector(), StillOptions::default().filter);
            std::thread::scope(|s| {
                for debug in DebugMode::RIG {
                    s.spawn(move || projector.prepare_rig(filter, debug));
                }
                s.spawn(move || projector.prepare_strip(filter));
            });
            Mutex::new(made)
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// A view and its size.
type Shot = (ViewParams, (u32, u32));

fn equirect(width: u32, height: u32) -> Shot {
    (ViewParams::equirect(DQuat::IDENTITY), (width, height))
}

/// 90° rectilinear at 128² on the +X seam.
fn plus_x() -> Shot {
    (
        ViewParams::new(
            DQuat::from_rotation_y(-FRAC_PI_2),
            Lens::new(Projection::Rectilinear, FRAC_PI_2),
        ),
        (128, 128),
    )
}

/// The pair `frames` (by track) under `rig`, each lens's own bytes.
fn stitched<'a>(
    frames: [&'a dyn VideoPlanes; 2],
    rig: &'a Rig,
    gains: [[f64; 3]; 2],
) -> SourceFrames<'a> {
    SourceFrames::Pair {
        lenses: frames.map(|frame| StillLens {
            frame,
            from: PlaneSource::Bytes,
        }),
        rig,
        gains,
    }
}

fn opts(debug: DebugMode, depth: Depth) -> StillOptions {
    StillOptions {
        debug,
        depth,
        ..StillOptions::default()
    }
}

/// The pair `frames` stitched by `rig` on the GPU through `shot`.
fn draw(
    frames: [&dyn VideoPlanes; 2],
    rig: &Rig,
    gains: [[f64; 3]; 2],
    (view, size): &Shot,
    debug: DebugMode,
    depth: Depth,
) -> Image {
    renderer()
        .render_blurred(
            gpu(),
            stitched(frames, rig, gains),
            std::slice::from_ref(view),
            *size,
            &opts(debug, depth),
        )
        .unwrap()
}

/// The reference's picture of `pair` under `rig` through `shot`.
fn cpu(
    pair: &LensPair,
    rig: &Rig,
    gains: [[f64; 3]; 2],
    (view, size): &Shot,
    debug: RigDebug,
    level0: bool,
) -> ImageF64 {
    render_rig_of(pair, rig, gains, view, *size, &RigOptions { debug, level0 }).unwrap()
}

/// `draw` and `reference` side by side: the reference is worked out on
/// another thread while the draw waits its turn at the renderer.
fn beside<A, B: Send>(draw: impl FnOnce() -> A, reference: impl FnOnce() -> B + Send) -> (A, B) {
    std::thread::scope(|s| {
        let reference = s.spawn(reference);
        let drawn = draw();
        let worked = reference
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e));
        (drawn, worked)
    })
}

/// The lens side of the binary's camera.
const SMALL: u32 = 192;

/// The X5-like camera composed at [`SMALL`]² a lens, seeing `SeamLines`
/// infinitely far and [`NEAR_M`] away, with sphere's rig of each and
/// their pairs made ready for the reference.
struct Small {
    truth: RigTruth,
    far: [LensFrame; 2],
    near: [LensFrame; 2],
    far_rig: Rig,
    near_rig: Rig,
    far_pair: LensPair,
    near_pair: LensPair,
}

fn small() -> &'static Small {
    static SMALL_CAMERA: OnceLock<Small> = OnceLock::new();
    SMALL_CAMERA.get_or_init(|| {
        let (cal, planted) = (x5_like(), x5_like_planted());
        let truth = compose(&cal, &planted, FRONT_TRACK, [SMALL; 2]);
        let near_truth = RigTruth {
            scene_radius_m: Some(NEAR_M),
            ..truth.clone()
        };
        // Far and near side by side: each is a lens per thread.
        let (far_frames, near_frames) = std::thread::scope(|s| {
            let near = s.spawn(|| pair_through(Pattern::SeamLines, &near_truth));
            let far = pair_through(Pattern::SeamLines, &truth);
            (far, near.join().unwrap())
        });
        let rig = |tune: &Tune| sphere_rig(&cal, &planted, tune, FRONT_TRACK, [SMALL; 2]).unwrap();
        let ready = |f: &[LensFrame; 2]| LensPair::new([&f[0], &f[1]]).unwrap();
        Small {
            far_pair: ready(&far_frames),
            near_pair: ready(&near_frames),
            far_rig: rig(&far()),
            near_rig: rig(&near()),
            far: far_frames,
            near: near_frames,
            truth,
        }
    })
}

impl Small {
    fn far(&self) -> [&dyn VideoPlanes; 2] {
        [&self.far[0], &self.far[1]]
    }

    fn near(&self) -> [&dyn VideoPlanes; 2] {
        [&self.near[0], &self.near[1]]
    }
}

/// An 8-bit I420 picture `side`² as the X5 codes one (full range,
/// BT.709, chroma sited left) whose luma at column x is `luma(x)` and
/// whose chroma is neutral.
fn columns(side: u32, luma: impl Fn(u32) -> u8) -> Frame {
    let row: Vec<u8> = (0..side).map(luma).collect();
    let y = row.repeat(side as usize);
    let c = side / 2;
    let chroma = vec![128u8; (c * c) as usize];
    Frame {
        layout: Kind::I420.layout(),
        size: (side, side),
        planes: vec![
            (y, side as usize, (side, side)),
            (chroma.clone(), c as usize, (c, c)),
            (chroma, c as usize, (c, c)),
        ],
        color: color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
    }
}

/// A flat grey picture `side`² at luma code `y`.
fn flat(side: u32, y: u8) -> Frame {
    columns(side, |_| y)
}

/// A flat grey 768² picture, the lens side of the tests that name 768².
fn grey_768() -> &'static Frame {
    static GREY: OnceLock<Frame> = OnceLock::new();
    GREY.get_or_init(|| flat(768, 128))
}

// The CPU's view of one pixel.

/// What the reference knows at a pixel's centre: its direction, each
/// lens's footprint LOD (infinite with no footprint) and whether it has
/// a footprint.
struct Probe {
    d: DVec3,
    lods: [f64; 2],
    feet: [bool; 2],
}

fn probe(rig: &Rig, shot: &Shot, i: u32, j: u32) -> Probe {
    probe_where(rig, shot, i, j, |_| true)
}

/// [`probe`] for the tests that ask only what covers and weighs: a lens
/// whose centre ray lies past its θ_max covers nothing at any LOD
/// (Rig::covers' margin is 2 texels or more), so its footprint isn't
/// worked out there and it has none.
fn probe_covering(rig: &Rig, shot: &Shot, i: u32, j: u32) -> Probe {
    let d = shot.0.dir(shot.1, f64::from(i) + 0.5, f64::from(j) + 0.5);
    probe_where(rig, shot, i, j, |b| {
        off_axis(rig.ray(b, d)) <= rig.lenses[b].theta_max
    })
}

/// [`probe`], looking for lens `b`'s footprint only where `look(b)`.
fn probe_where(
    rig: &Rig,
    (view, size): &Shot,
    i: u32,
    j: u32,
    look: impl Fn(usize) -> bool,
) -> Probe {
    let d = view.dir(*size, f64::from(i) + 0.5, f64::from(j) + 0.5);
    let feet = [0, 1].map(|b| {
        look(b)
            .then(|| lens_footprint(rig, b, view, *size, i, j))
            .flatten()
    });
    Probe {
        d,
        lods: feet.map(|f| {
            f.map_or(f64::INFINITY, |(a, b)| {
                a.length().max(b.length()).max(1e-30).log2()
            })
        }),
        feet: feet.map(|f| f.is_some()),
    }
}

impl Probe {
    /// Whether lens `b` covers the pixel, as the reference draws it.
    fn covers(&self, rig: &Rig, b: usize) -> bool {
        self.feet[b] && rig.covers(b, self.d, self.lods[b])
    }

    /// Each covering lens's feathered weight before it is normalised,
    /// written from the rule (Rig::weights' doc): 1 − smoothstep(c_b −
    /// h_b, c_b + h_b, θ_b), a step at c_b where h_b is 0; 0 where the
    /// lens doesn't cover.
    fn feathered(&self, rig: &Rig) -> [f64; 2] {
        [0, 1].map(|b| {
            if !self.covers(rig, b) {
                return 0.0;
            }
            let theta = off_axis(rig.ray(b, self.d));
            let c = rig.band_centre(b);
            let h = rig.feather_at(b, self.lods[b]);
            1.0 - smoothstep(c - h, c + h, theta)
        })
    }
}

/// θ of a lens-frame direction, atan2(hypot(x, y), z).
fn off_axis(v: DVec3) -> f64 {
    v.x.hypot(v.y).atan2(v.z)
}

/// Hermite's step, a step at `lo` (½ there) where `hi` ≤ `lo`.
fn smoothstep(lo: f64, hi: f64, x: f64) -> f64 {
    if hi <= lo {
        return if x < lo {
            0.0
        } else if x == lo {
            0.5
        } else {
            1.0
        };
    }
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `f` of every pixel of `size`, row after row, on the machine's threads.
fn pixels<T: Send + Default + Clone>(size: (u32, u32), f: impl Fn(u32, u32) -> T + Sync) -> Vec<T> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let (w, h) = size;
    let mut out = vec![T::default(); w as usize * h as usize];
    let rows_each = (h as usize).div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (k, chunk) in out.chunks_mut(rows_each * w as usize).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (n, v) in chunk.iter_mut().enumerate() {
                    let j = (k * rows_each + n / w as usize) as u32;
                    *v = f(n as u32 % w, j);
                }
            });
        }
    });
    out
}

/// Pixel `n`'s R, G and B of a picture's unit values.
fn rgb(values: &[f64], n: usize) -> [f64; 3] {
    [values[3 * n], values[3 * n + 1], values[3 * n + 2]]
}

// The tests.

/// The stitch of rung 1's scenes against `reference::render_rig_of`.
///
/// `SeamLines` at 192², infinitely far and at q = 1/1.5, through the
/// 256 × 128 equirect and a 90° 128² view on the +X seam: the CPU rule.
/// Control: the GPU stitched at q = 0 against the CPU at q = 1/1.5.
#[test]
fn fs_rig_matches_render_rig_of() {
    let _time = timed("fs_rig_matches_render_rig_of");
    let s = small();
    for (scene, frames, rig, pair) in [
        ("infinitely far", s.far(), &s.far_rig, &s.far_pair),
        ("at 1.5 m", s.near(), &s.near_rig, &s.near_pair),
    ] {
        for (what, shot) in [("the equirect", equirect(256, 128)), ("+X", plus_x())] {
            let (got, want) = beside(
                || draw(frames, rig, UNIT, &shot, DebugMode::Off, Depth::Eight),
                || cpu(pair, rig, UNIT, &shot, RigDebug::Off, false),
            );
            let held = image_rule(&got, &want).unwrap_or_else(|e| panic!("{scene}, {what}: {e}"));
            eprintln!(
                "{scene}, {what}: {:.2} dB, a channel {:.4} off at most",
                held.db, held.worst
            );
        }
    }
    let shot = plus_x();
    let (got, want) = beside(
        || {
            draw(
                s.near(),
                &s.far_rig,
                UNIT,
                &shot,
                DebugMode::Off,
                Depth::Eight,
            )
        },
        || cpu(&s.near_pair, &s.near_rig, UNIT, &shot, RigDebug::Off, false),
    );
    let off = must_fail(
        "the GPU stitched at q = 0 against the CPU at q = 1/1.5",
        image_rule(&got, &want),
    );
    eprintln!("the GPU at q = 0 against the CPU at q = 1/1.5, +X: {off}");
}

/// Debug modes 4 to 7 draw each block alone, the weights and the seam
/// difference, by calibration block whatever the binding.
///
/// A binding that puts block 0 on track 1, with unlike gains: modes 4 and
/// 5 under the CPU rule; 6 and 7 at `Depth::Linear` within an f16 step
/// of `RigDebug`'s pictures on 99.9 % of pixels, 7 passing 1 somewhere.
/// Mode 6 holds that on the SeamLines pair. Mode 7 is each lens's taps
/// times 8, and lavapipe's filtering of 8-bit texels is not f64's: on
/// SeamLines 13.6 % of its pixels miss by more than a step (0.063 at
/// worst), so mode 7 is held there under the CPU rule, and within an f16
/// step on a flat pair (codes 70 and 200 by track), whose taps are exact.
/// Controls: mode 4 taken as track 0's picture; mode 7 read back at 8
/// bits, which loses what passes 1.
#[test]
fn fs_rig_modes_4_to_7_follow_the_blocks() {
    let _time = timed("fs_rig_modes_4_to_7_follow_the_blocks");
    let s = small();
    let gains = [[1.2, 1.0, 0.9], [0.8, 1.1, 1.0]];
    // Block 0's lens on track 1: the tracks' pictures swapped with it.
    let mut rig = s.far_rig;
    rig.lenses[0].track = 1;
    rig.lenses[1].track = 0;
    let frames: [&dyn VideoPlanes; 2] = [&s.far[1], &s.far[0]];
    let pair = LensPair::new(frames).unwrap();
    let shot = plus_x();
    let mut alone = Vec::new();
    for (mode, debug) in [
        (DebugMode::Lens0, RigDebug::Lens0),
        (DebugMode::Lens1, RigDebug::Lens1),
    ] {
        let (got, want) = beside(
            || draw(frames, &rig, gains, &shot, mode, Depth::Eight),
            || cpu(&pair, &rig, gains, &shot, debug, false),
        );
        let held = image_rule(&got, &want).unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        eprintln!(
            "{mode:?}, block 0 on track 1: {:.2} dB, a channel {:.4} off at most",
            held.db, held.worst
        );
        alone.push(got);
    }
    // Mode 6 and mode 7 within an f16 step: the weights on SeamLines, the
    // seam difference on the flat pair.
    let greys = [flat(SMALL, 70), flat(SMALL, 200)];
    let flats: [&dyn VideoPlanes; 2] = [&greys[0], &greys[1]];
    let flat_pair = LensPair::new(flats).unwrap();
    let mut seam = Vec::new();
    for (mode, debug, frames, pair) in [
        (DebugMode::Weights, RigDebug::Weights, frames, &pair),
        (DebugMode::SeamDiff, RigDebug::SeamDiff, flats, &flat_pair),
    ] {
        let (got, want) = beside(
            || draw(frames, &rig, gains, &shot, mode, Depth::Linear).unit_rgb(),
            || cpu(pair, &rig, gains, &shot, debug, false),
        );
        let (share, worst) = near_share(&got, &want.data);
        eprintln!(
            "{mode:?}: {:.4} % of pixels within an f16 step, the worst channel {worst:.5} off",
            100.0 * share
        );
        assert!(share >= 0.999, "{mode:?}: {share}");
        seam.push(got);
    }
    let top = seam[1].iter().copied().fold(0.0, f64::max);
    eprintln!("mode 7 of the flat pair: {top:.4} at most");
    assert!(top > 1.0, "mode 7 never passes 1: {top}");
    // Mode 7 of SeamLines, under the CPU rule.
    let (got, (want, track_0)) = beside(
        || {
            draw(
                frames,
                &rig,
                gains,
                &shot,
                DebugMode::SeamDiff,
                Depth::Linear,
            )
            .unit_rgb()
        },
        || {
            (
                cpu(&pair, &rig, gains, &shot, RigDebug::SeamDiff, false),
                // Track 0's picture is block 1's.
                cpu(&pair, &rig, gains, &shot, RigDebug::Lens1, false),
            )
        },
    );
    let (share, _) = near_share(&got, &want.data);
    let held = rule(&got, &want.data).unwrap_or_else(|e| panic!("SeamDiff of SeamLines: {e}"));
    eprintln!(
        "SeamDiff of SeamLines: {:.2} dB, a channel {:.4} off at most; {:.2} % of pixels within \
         an f16 step",
        held.db,
        held.worst,
        100.0 * share
    );
    let off = must_fail(
        "mode 4 taken as track 0's picture",
        image_rule(&alone[0], &track_0),
    );
    eprintln!("mode 4 against track 0's lens alone: {off}");
    let eight = draw(flats, &rig, gains, &shot, DebugMode::SeamDiff, Depth::Eight);
    let eight_top = eight.unit_rgb().into_iter().fold(0.0, f64::max);
    must_fail("mode 7 read back at 8 bits passes 1", above(eight_top, 1.0));
}

/// Mode 6 lights the ring where no feather weighs as the reference does.
///
/// Tune's feather of 0 at q = 0.5 m⁻¹ on x5_like through sphere_rig at
/// [768; 2], a flat grey pair, mode 6 on a 192 × 96 equirect: every pixel
/// that `Rig::weights` gives (1, 0) or (0, 1) at its centre reads so.
/// F025 centres both steps on the seam's plane, so the default lenses'
/// ring (D63's 337 pixels, the old rule's, 2δ = 0.92° wide) is gone but
/// for the sliver their axes' 0.57° tilt leaves between the steps on one
/// side: 27 pixels, all covered by both lenses. The ring stands wider
/// where lens 1's θ_max is so near 90° that its mask ends short of lens
/// 0's step (Rig::weights' doc), and where each lens is turned 3° away
/// from the other (reference_rig.rs's case). Control: 10.3's text,
/// normalised feathered weights and black where both are 0, computed on
/// the CPU, holds the GPU's picture: it must not.
#[test]
fn fs_rig_weights_light_the_ring_no_feather_weighs() {
    let _time = timed("fs_rig_weights_light_the_ring_no_feather_weighs");
    let grey = grey_768();
    let lens = |yaw: f64, theta_max_deg: f64| LensTune {
        ypr_deg: [yaw, 0.0, 0.0],
        theta_max_deg,
        ..LensTune::default()
    };
    let step = |lenses| Tune {
        lenses,
        feather_deg: 0.0,
        q_per_m: 0.5,
    };
    let shot = equirect(192, 96);
    // Each rig's ring: exactly the default lenses' sliver, and more than
    // 50 pixels on the others.
    for (name, tune, sliver) in [
        ("the default lenses", step([lens(0.0, 100.0); 2]), true),
        (
            "lens 1 ending at 91°",
            step([lens(0.0, 100.0), lens(0.0, 91.0)]),
            false,
        ),
        (
            "each lens turned 3° away",
            step([lens(3.0, 100.0), lens(-3.0, 100.0)]),
            false,
        ),
    ] {
        let rig = sphere_rig(&x5_like(), &x5_like_planted(), &tune, FRONT_TRACK, [768; 2]).unwrap();
        // Per pixel, from the reference: Rig::weights, whether they give
        // one lens all, whether a lens covers it that no feather weighs
        // (and whether both do), and 10.3's text.
        let (got, want) = beside(
            || {
                draw(
                    [grey, grey],
                    &rig,
                    UNIT,
                    &shot,
                    DebugMode::Weights,
                    Depth::Linear,
                )
                .unit_rgb()
            },
            || {
                pixels(shot.1, |i, j| {
                    let p = probe_covering(&rig, &shot, i, j);
                    let w = rig.weights(p.d, p.lods);
                    let whole = w == [1.0, 0.0] || w == [0.0, 1.0];
                    let covers = [p.covers(&rig, 0), p.covers(&rig, 1)];
                    let f = p.feathered(&rig);
                    let ring = (covers[0] || covers[1]) && f == [0.0; 2];
                    let sum = f[0] + f[1];
                    let text = if sum > 0.0 {
                        [f[0] / sum, 0.0, f[1] / sum]
                    } else {
                        [0.0; 3]
                    };
                    (w, whole, ring, ring && covers == [true; 2], text)
                })
            },
        );
        // And whether the GPU reads otherwise than the weights where they
        // give one lens all, and otherwise than 10.3's text.
        let checks: Vec<[bool; 5]> = (want.iter().enumerate())
            .map(|(n, &(w, whole, ring, both, text))| {
                let have = rgb(&got, n);
                let wrong = whole && have != [w[0], 0.0, w[1]];
                let unlike = !(0..3).all(|k| f16_near(have[k], round_f16(text[k])));
                [whole, wrong, ring, both, unlike]
            })
            .collect();
        let count = |k: usize| checks.iter().filter(|c| c[k]).count();
        let (whole, wrong, ring, both, unlike) = (count(0), count(1), count(2), count(3), count(4));
        eprintln!(
            "{name}, a 192 × 96 equirect: {whole} pixels one lens takes whole, {wrong} read \
             otherwise; a lens covers {ring} pixels no feather weighs, both cover {both} of \
             them; 10.3's text reads {unlike} pixels otherwise"
        );
        assert_eq!(wrong, 0, "{name}");
        if sliver {
            assert_eq!((ring, both), (27, 27), "{name}: the axes' tilt's sliver");
        } else {
            assert!(ring > 50, "{name}: {ring}");
        }
        must_fail(
            "10.3's text, black where both feathered weights are 0",
            at_most(unlike, 0),
        );
    }
}

/// Sphere's `mirror_pair` (sphere/tests/rig.rs), copied: two lenses that
/// are one another's mirror image in the plane z = 0 to the last bit, a
/// stereographic Kannala–Brandt lens on each, stitched at 0.3 m with no
/// feather. The half turn (0, 1, 0, 0) negates x and z and rounds nothing,
/// so a direction in the plane has one θ on both lenses, in f64 and in
/// f32 alike.
fn mirror_pair() -> Rig {
    let model = stereographic(167.0, 380.0, 386.0);
    let centre = DVec3::new(0.01, 0.0, 0.016);
    let image = DVec3::new(0.01, 0.0, -0.016);
    let half_turn = DQuat::from_xyzw(0.0, 1.0, 0.0, 0.0);
    let lens =
        |track, turn, centre| RigLens::new(track, model, turn, centre, 104f64.to_radians(), 768);
    Rig {
        lenses: [lens(0, DQuat::IDENTITY, centre), lens(1, half_turn, image)],
        q: 3.34,
        feather: 0.0,
        margin_texels: 2.0,
    }
}

/// A stereographic Kannala–Brandt lens of focal `f` with its principal
/// point at (`cx`, `cy`), as sphere's `stereographic_rig` makes one at
/// 167, 380 and 386.
fn stereographic(f: f64, cx: f64, cy: f64) -> LensModel {
    LensModel::Kb(KbLens {
        base: Model::Stereographic,
        k: [0.0; 5],
        fx: f,
        fy: f,
        cx,
        cy,
        p: [0.0; 2],
        pe: [0.0; 2],
        s: [0.0; 4],
    })
}

/// A tie between the lenses falls as it does for the mirrored pair.
///
/// The mirror pair, a flat grey pair, mode 6 at its feather of 0 on a
/// 720 × 361 equirect turned so that its middle row lies on the mirror
/// plane z = 0 (the turn (½, ½, ½, ½) takes view (x, y, z) to rig (z, x,
/// y), and the middle row's latitude is 0 in f32): ½ each wherever the
/// lenses tie, which is the middle row where both cover, and 0 or 1 by
/// the smaller θ everywhere else.
///
/// Control: a rule comparing θ − h. On the mirror pair the bands meet
/// across the plane (F025 centres both on it), so no feather leaves both
/// weights 0 at any h > 0 and the brief's control rig (the mirror pair at
/// a feather of 2° with lens 1's θ_max lowered) has no pixel between the
/// bands' ends where both cover. So the control rig is the mirror pair's
/// model at a focal of 150 with its centres on the axes, each lens turned
/// 5° towards +X, which opens a gap past both bands about −X, at a
/// feather of 4° with lens 1's principal point moved so that θ_frame cuts
/// its room to under 0.5° (feather_at shrinks its h to 0.5°) while its far
/// side still covers past both bands. A θ − h rule computed on the CPU
/// must not hold the GPU's mode 6 on the pixels between the bands' ends.
/// That rig is the mirror pair turned alike, so it ties on the plane too,
/// where the reference's choice in f64 is its last bit's: pixels whose
/// θ_b − c_b lie within 1e-6 of each other are counted apart.
#[test]
fn fs_rig_ties_like_the_mirror_pair() {
    let _time = timed("fs_rig_ties_like_the_mirror_pair");
    let grey = grey_768();
    let rig = mirror_pair();
    let turn = DQuat::from_xyzw(0.5, 0.5, 0.5, 0.5);
    let shot = (ViewParams::equirect(turn), (720, 361));
    let middle = 180;
    // Per pixel, from the reference: whether it lies on the middle row
    // where both cover, and Rig::weights off that row.
    let (got, want) = beside(
        || {
            draw(
                [grey, grey],
                &rig,
                UNIT,
                &shot,
                DebugMode::Weights,
                Depth::Linear,
            )
            .unit_rgb()
        },
        || {
            pixels(shot.1, |i, j| {
                let p = probe_covering(&rig, &shot, i, j);
                if j == middle {
                    // The plane's direction, (−cos λ, sin λ, 0) at
                    // longitude λ.
                    let lon = (f64::from(i) + 0.5) / 720.0 * TAU - PI;
                    let d = DVec3::new(-lon.cos(), lon.sin(), 0.0);
                    let both = (0..2).all(|b| p.feet[b] && rig.covers(b, d, p.lods[b]));
                    let theta = [0, 1].map(|b| off_axis(rig.ray(b, d)));
                    assert_eq!(theta[0], theta[1], "pixel {i} of the middle row");
                    if both {
                        assert_eq!(rig.weights(d, p.lods), [0.5; 2], "pixel {i}");
                    }
                    return (true, both, [0.0; 2]);
                }
                let w = rig.weights(p.d, p.lods);
                assert!(
                    w == [1.0, 0.0] || w == [0.0, 1.0] || w == [0.0; 2],
                    "({i}, {j}): {w:?}"
                );
                (false, false, w)
            })
        },
    );
    // Per pixel: whether the GPU reads a tie, whether it lies on the
    // middle row where both cover, and whether the GPU reads otherwise
    // than the rule.
    let checks: Vec<[bool; 3]> = (want.iter().enumerate())
        .map(|(n, &(on_row, both, w))| {
            let have = rgb(&got, n);
            let tie = have == [0.5, 0.0, 0.5];
            if on_row {
                [tie, both, both && !tie]
            } else {
                [tie, false, have != [w[0], 0.0, w[1]]]
            }
        })
        .collect();
    let count = |checks: &[[bool; 3]], k: usize| checks.iter().filter(|c| c[k]).count();
    let (ties, row, wrong) = (count(&checks, 0), count(&checks, 1), count(&checks, 2));
    eprintln!(
        "the mirror pair, 720 × 361: {ties} pixels tie at ½ each, {row} of the middle row where \
         both cover; {wrong} pixels read otherwise than the rule"
    );
    assert_eq!(wrong, 0);
    assert_eq!(ties, row, "a tie off the middle row");
    assert!(row > 200, "{row}");

    // The control's rig and its pixels between the bands' ends.
    let apart = shrunk_apart();
    let shot = (ViewParams::equirect(turn), (360, 181));
    // The control rig is the mirror pair turned alike, so its lenses tie
    // on the plane too: there the reference's choice is its last bit's
    // (θ_b − c_b 2e-16 apart), and the GPU's f32 reads ½. Directions
    // whose θ_b − c_b lie within f32's reach of each other are counted
    // apart and held to nothing. Per pixel, from the reference: whether
    // it is one of those, Rig::weights, and θ − h's pick where both cover
    // and no feather weighs.
    let (got, want) = beside(
        || {
            draw(
                [grey, grey],
                &apart,
                UNIT,
                &shot,
                DebugMode::Weights,
                Depth::Linear,
            )
            .unit_rgb()
        },
        || {
            pixels(shot.1, |i, j| {
                let p = probe_covering(&apart, &shot, i, j);
                let w = apart.weights(p.d, p.lods);
                let theta = [0, 1].map(|b| off_axis(apart.ray(b, p.d)));
                let near = [0, 1].map(|b| theta[b] - apart.band_centre(b));
                if (near[0] - near[1]).abs() < 1e-6 {
                    return (true, w, None);
                }
                let both = p.covers(&apart, 0) && p.covers(&apart, 1);
                if !both || p.feathered(&apart) != [0.0; 2] {
                    return (false, w, None);
                }
                let past = [0, 1].map(|b| theta[b] - apart.feather_at(b, p.lods[b]));
                let by_h = match past[0].partial_cmp(&past[1]) {
                    Some(std::cmp::Ordering::Less) => [1.0, 0.0, 0.0],
                    Some(std::cmp::Ordering::Greater) => [0.0, 0.0, 1.0],
                    _ => [0.5, 0.0, 0.5],
                };
                (false, w, Some(by_h))
            })
        },
    );
    let checks: Vec<[bool; 4]> = (want.iter().enumerate())
        .map(|(n, &(tied, w, by_h))| {
            if tied {
                return [false, false, false, true];
            }
            let have = rgb(&got, n);
            let wrong = (w == [1.0, 0.0] || w == [0.0, 1.0]) && have != [w[0], 0.0, w[1]];
            match by_h {
                None => [wrong, false, false, false],
                Some(by_h) => [wrong, by_h != [w[0], 0.0, w[1]], by_h != have, false],
            }
        })
        .collect();
    let count = |checks: &[[bool; 4]], k: usize| checks.iter().filter(|c| c[k]).count();
    let (wrong, between, unlike, tied) = (
        count(&checks, 0),
        count(&checks, 1),
        count(&checks, 2),
        count(&checks, 3),
    );
    eprintln!(
        "lenses turned 5° to +X, h 4° and 0.5°, 360 × 181: {between} pixels lie between the \
         switch by θ and by θ − h, where both cover and no feather weighs; the GPU reads {wrong} \
         pixels otherwise than Rig::weights, and θ − h {unlike} otherwise than the GPU; {tied} \
         pixels on the plane, where the lenses tie, held to nothing"
    );
    assert_eq!(wrong, 0);
    assert!(between > 20, "{between}");
    let off = must_fail("a rule comparing θ − h", at_most(unlike, 0));
    assert_eq!(off, between);
}

/// The control rig of [`fs_rig_ties_like_the_mirror_pair`]: stereographic
/// lenses of focal 150 at 768², centres 16 mm along each axis, each lens
/// turned 5° about Y towards +X, stitched at 0.3 m at a feather of 4°;
/// lens 1's principal point 319 px from its slot's left edge, so that
/// θ_frame (93.5°) leaves it under 0.5° of room past its band's centre
/// (93.06°) and feather_at gives it 0.5°, while lens 0 keeps its 4°.
fn shrunk_apart() -> Rig {
    let mirror = mirror_pair();
    let lens = |b: usize, cx: f64, centre: DVec3| {
        let l = &mirror.lenses[b];
        let deg = if b == 0 { 5.0 } else { -5.0 };
        let base = RigLens::new(
            b,
            stereographic(150.0, cx, 384.0),
            l.lens_from_rig,
            centre,
            104f64.to_radians(),
            768,
        );
        turned(&base, DVec3::Y, deg)
    };
    Rig {
        lenses: [
            lens(0, 384.0, DVec3::new(0.0, 0.0, 0.016)),
            lens(1, 319.0, DVec3::new(0.0, 0.0, -0.016)),
        ],
        feather: 4f64.to_radians(),
        ..mirror
    }
}

/// The mask is tested in angle, against θ_max, not by its cosine.
///
/// Pat's V3 lens 0 at 768² under x5_like_planted (render_rig's doc: 174.73
/// texels a radian at a θ_max of 100°), a flat grey pair, mode 4 through
/// a 5 × 5 fisheye centred on the lens's axis, minified so that the
/// middle pixel's λ lies between 7.26 and 7.57, where θ_max less the
/// margin is below 0: no pixel lit that Rig::covers refuses at the
/// footprint's LOD. Control: the unguarded cosine form over the same rays
/// covers some of them.
#[test]
fn fs_rig_tests_the_mask_in_angle() {
    let _time = timed("fs_rig_tests_the_mask_in_angle");
    let grey = grey_768();
    let rig = sphere_rig(
        &pats(Generation::V3),
        &x5_like_planted(),
        &far(),
        FRONT_TRACK,
        [768; 2],
    )
    .unwrap();
    let axis = rig.lenses[0].lens_from_rig.inverse() * DVec3::Z;
    let turn = DQuat::from_rotation_arc(DVec3::NEG_Z, axis);
    let size = (5, 5);
    let shot_at = |fov: f64| {
        (
            ViewParams::new(turn, Lens::new(Projection::Fisheye, fov)),
            size,
        )
    };
    let middle_lod = |fov: f64| probe(&rig, &shot_at(fov), 2, 2).lods[0];
    // The field of view whose middle pixel's λ is 7.4, by bisection.
    let (mut lo, mut hi) = (10f64.to_radians(), 300f64.to_radians());
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if middle_lod(mid) < 7.4 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let shot = shot_at(lo);
    let lambda = middle_lod(lo);
    assert!((7.26..7.57).contains(&lambda), "{lambda}");
    let got = draw(
        [grey, grey],
        &rig,
        UNIT,
        &shot,
        DebugMode::Lens0,
        Depth::Eight,
    )
    .unit_rgb();
    let lens = &rig.lenses[0];
    let (mut lit_refused, mut by_cosine) = (0, 0);
    for j in 0..size.1 {
        for i in 0..size.0 {
            let p = probe(&rig, &shot, i, j);
            let n = (j * size.0 + i) as usize;
            if rgb(&got, n) != [0.0; 3] && !p.covers(&rig, 0) {
                lit_refused += 1;
            }
            // 10.3's cosine form, unguarded, with the frame term.
            let margin = rig.margin_texels * p.lods[0].max(0.0).exp2();
            let ray = rig.ray(0, p.d);
            let bound = lens.theta_max - margin / lens.texels_per_radian;
            let side = f64::from(lens.side);
            let framed = lens
                .model
                .project(ray)
                .is_some_and(|px| px.min_element() >= margin && px.max_element() <= side - margin);
            if p.feet[0] && off_axis(ray).cos() >= bound.cos() && framed {
                by_cosine += 1;
            }
        }
    }
    eprintln!(
        "Pat's V3 lens 0, 768², a 5 × 5 fisheye of {:.2}° on its axis: λ {lambda:.4} in the \
         middle; {lit_refused} pixels lit that the angle refuses; the cosine form covers \
         {by_cosine}",
        lo.to_degrees()
    );
    assert_eq!(lit_refused, 0);
    must_fail("the unguarded cosine form", at_most(by_cosine, 0));
}

/// Level 0 keeps the mask its footprint's LOD would give.
///
/// `DebugMode::Level0` against render_rig_of with level0 true on a
/// minified 64 × 32 equirect, of `SeamLines` and of a pair whose lenses
/// are unlike greys: the CPU rule. Control: the greys' stitch with each
/// lens's coverage and feather taken at LOD 0 (Rig::weights at 0) differs
/// from the GPU's.
#[test]
fn fs_rig_level0_keeps_the_footprints_mask() {
    let _time = timed("fs_rig_level0_keeps_the_footprints_mask");
    let s = small();
    let shot = equirect(64, 32);
    let (got, want) = beside(
        || {
            draw(
                s.far(),
                &s.far_rig,
                UNIT,
                &shot,
                DebugMode::Level0,
                Depth::Eight,
            )
        },
        || cpu(&s.far_pair, &s.far_rig, UNIT, &shot, RigDebug::Off, true),
    );
    let held = image_rule(&got, &want).unwrap();
    eprintln!(
        "SeamLines at level 0, 64 × 32: {:.2} dB, a channel {:.4} off at most",
        held.db, held.worst
    );
    let codes = [70u8, 200];
    let greys = codes.map(|y| flat(SMALL, y));
    let frames: [&dyn VideoPlanes; 2] = [&greys[0], &greys[1]];
    let pair = LensPair::new(frames).unwrap();
    let (got, want) = beside(
        || {
            draw(
                frames,
                &s.far_rig,
                UNIT,
                &shot,
                DebugMode::Level0,
                Depth::Eight,
            )
        },
        || cpu(&pair, &s.far_rig, UNIT, &shot, RigDebug::Off, true),
    );
    let held = image_rule(&got, &want).unwrap();
    eprintln!(
        "two greys at level 0, 64 × 32: {:.2} dB, a channel {:.4} off at most",
        held.db, held.worst
    );
    // Each track's grey in linear light, by block.
    let light = |b: usize| eotf(f64::from(codes[s.far_rig.lenses[b].track]) / 255.0);
    let at_0: Vec<f64> = pixels(shot.1, |i, j| {
        let d = shot.0.dir(shot.1, f64::from(i) + 0.5, f64::from(j) + 0.5);
        let w = s.far_rig.weights(d, [0.0; 2]);
        let v = srgb_oetf(round_f16(w[0] * light(0) + w[1] * light(1)).clamp(0.0, 1.0));
        [v; 3]
    })
    .into_iter()
    .flatten()
    .collect();
    let off = must_fail(
        "coverage and feathers taken at LOD 0",
        rule(&got.unit_rgb(), &at_0),
    );
    eprintln!("the greys stitched by LOD 0's weights: {off}");
}

/// A lens whose ray lands outside its picture doesn't cover the pixel.
///
/// A Mei lens at ξ = 0.5, whose image ends at θ = 120° (z + ξ = 0), at a
/// focal of 1.5 px on a 768² slot, so that its frame term passes centres
/// up to 119.8° and a 360 × 180 equirect's side rays, half a degree off,
/// fall past the end for some pixels whose centre it covers: mode 4 is
/// render_rig_of's, under the CPU rule, and black wherever a side ray
/// has no image. Control: coverage judged on the centre ray alone, at LOD
/// 0, lights them.
#[test]
fn fs_rig_lens_without_an_image_doesnt_cover() {
    let _time = timed("fs_rig_lens_without_an_image_doesnt_cover");
    let grey = grey_768();
    let model = LensModel::Mei(Unified {
        xi: 0.5,
        fx: 1.5,
        fy: 1.5,
        cx: 384.0,
        cy: 384.0,
        dist: Canonical::NONE,
    });
    let lens =
        |track, turn| RigLens::new(track, model, turn, DVec3::ZERO, 119.9f64.to_radians(), 768);
    let rig = Rig {
        lenses: [
            lens(0, DQuat::IDENTITY),
            lens(1, DQuat::from_xyzw(0.0, 1.0, 0.0, 0.0)),
        ],
        q: 0.0,
        feather: 0.0,
        margin_texels: 2.0,
    };
    let shot = equirect(360, 180);
    let frames: [&dyn VideoPlanes; 2] = [grey, grey];
    let pair = LensPair::new(frames).unwrap();
    // Per pixel, from the reference: no footprint but the centre covered
    // at LOD 0.
    let (got, (want, without)) = beside(
        || draw(frames, &rig, UNIT, &shot, DebugMode::Lens0, Depth::Eight),
        || {
            let without = pixels(shot.1, |i, j| {
                let p = probe(&rig, &shot, i, j);
                !p.feet[0] && rig.covers(0, p.d, 0.0)
            });
            (
                cpu(&pair, &rig, UNIT, &shot, RigDebug::Lens0, false),
                without,
            )
        },
    );
    let held = image_rule(&got, &want).unwrap();
    let got = got.unit_rgb();
    // And whether the GPU lights them.
    let checks: Vec<[bool; 2]> = (without.iter().enumerate())
        .map(|(n, &no_image)| [no_image, no_image && rgb(&got, n) != [0.0; 3]])
        .collect();
    let no_image = checks.iter().filter(|c| c[0]).count();
    let lit = checks.iter().filter(|c| c[1]).count();
    eprintln!(
        "a Mei lens at ξ = 0.5, 360 × 180: {:.2} dB, a channel {:.4} off at most; {no_image} \
         pixels have a side ray past the image's end and a centre the mask passes at LOD 0, \
         {lit} of them lit",
        held.db, held.worst
    );
    assert!(no_image > 20, "{no_image}");
    assert_eq!(lit, 0);
    let unlit = must_fail(
        "coverage judged on the centre ray alone",
        at_most(no_image - lit, 0),
    );
    assert_eq!(unlit, no_image);
}

/// Motion blur sums a pair's stitched pictures in linear light.
///
/// render_blurred of a pair through two equirect views 90° apart in yaw,
/// so that each pixel sums two unlike parts of the stitch, against the
/// mean of render_rig_of's two pictures taken back through the EOTF,
/// rounded to f16, then the OETF: the CPU rule. Control: the plain mean
/// of the display values. (Views 3° apart differ too little: their
/// display mean held the rule, at 54.06 dB and 0.033 off.)
#[test]
fn fs_rig_blurs_a_pair_in_linear_light() {
    let _time = timed("fs_rig_blurs_a_pair_in_linear_light");
    let s = small();
    let size = (256, 128);
    let views = [
        ViewParams::equirect(DQuat::IDENTITY),
        ViewParams::equirect(DQuat::from_rotation_y(90f64.to_radians())),
    ];
    let (got, pictures) = beside(
        || {
            renderer()
                .render_blurred(
                    gpu(),
                    stitched(s.far(), &s.far_rig, UNIT),
                    &views,
                    size,
                    &StillOptions::default(),
                )
                .unwrap()
                .unit_rgb()
        },
        || {
            views.map(|v| {
                cpu(
                    &s.far_pair,
                    &s.far_rig,
                    UNIT,
                    &(v, size),
                    RigDebug::Off,
                    false,
                )
            })
        },
    );
    let mean = |f: fn(f64, f64) -> f64| -> Vec<f64> {
        pictures[0]
            .data
            .iter()
            .zip(&pictures[1].data)
            .map(|(a, b)| f(*a, *b))
            .collect()
    };
    let linear = mean(|a, b| srgb_oetf(round_f16(0.5 * (eotf(a) + eotf(b))).clamp(0.0, 1.0)));
    let held = rule(&got, &linear).unwrap();
    eprintln!(
        "two views 90° apart, blurred: {:.2} dB, a channel {:.4} off at most",
        held.db, held.worst
    );
    let display = mean(|a, b| 0.5 * (a + b));
    let off = must_fail("the plain mean of display values", rule(&got, &display));
    eprintln!("blurred against the display values' mean: {off}");
}

/// `StillRenderer::render_yuv` codes a pair's stitch as it codes an
/// equirect's.
///
/// render_yuv of the pair to NV12 against yuv.rs's CPU conversion of the
/// same pair's linear picture: within one code. Control: the lenses'
/// tracks swapped.
#[test]
fn render_yuv_takes_a_pair() {
    let _time = timed("render_yuv_takes_a_pair");
    let s = small();
    let (view, size) = plus_x();
    let linear = draw(
        s.far(),
        &s.far_rig,
        UNIT,
        &(view, size),
        DebugMode::Off,
        Depth::Linear,
    );
    let codes = yuv::reference(&linear.unit_rgb(), size);
    let worst = |frames: [&dyn VideoPlanes; 2]| {
        let got = renderer()
            .render_yuv(
                gpu(),
                stitched(frames, &s.far_rig, UNIT),
                std::slice::from_ref(&view),
                size,
                &StillOptions::default(),
                YuvLayout::Nv12,
            )
            .unwrap();
        let (w, h) = size;
        let mut worst = 0;
        for y in 0..h {
            for x in 0..w {
                let want = yuv::to_code(codes.y[(y * w + x) as usize], 8);
                worst = worst.max(got.sample(0, x, y).abs_diff(want));
            }
        }
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                let i = (y * w / 2 + x) as usize;
                for (p, v) in [(1, codes.cb[i]), (2, codes.cr[i])] {
                    worst = worst.max(got.sample(p, x, y).abs_diff(yuv::to_code(v, 8)));
                }
            }
        }
        worst
    };
    let held = worst(s.far());
    eprintln!("NV12 of the pair's stitch: within {held} codes of its linear picture's");
    assert!(held <= 1, "{held}");
    let off = must_fail(
        "the lenses' tracks swapped",
        at_most(worst([&s.far[1], &s.far[0]]), 1),
    );
    eprintln!("tracks swapped: {off} codes off");
}

/// `render_from` refuses what has no form on the GPU: a pair drawn flat,
/// and an equirect drawn in a pair's debug modes, 4 to 7.
///
/// opts.flat with a pair, modes 4 to 7 with one frame, level 0 beside
/// mode 4 in RigOptions, and a pair whose gain `rig::pack` refuses are
/// each `Invalid`, naming the cause. Control: the same calls with a valid
/// source or option render.
#[test]
fn render_from_refuses_what_has_no_gpu_form() {
    let _time = timed("render_from_refuses_what_has_no_gpu_form");
    let s = small();
    let (view, size) = (ViewParams::equirect(DQuat::IDENTITY), (32, 16));
    let views = std::slice::from_ref(&view);
    let invalid = |r: &Result<Image, RenderError>, says: &str| matches!(r, Err(RenderError::Invalid(m)) if m.contains(says));
    let flat = StillOptions {
        flat: true,
        ..StillOptions::default()
    };
    let plain = StillOptions::default();
    let mut r = renderer();
    let pair = |gains| stitched(s.far(), &s.far_rig, gains);
    let pair_flat = r.render_blurred(gpu(), pair(UNIT), views, size, &flat);
    assert!(invalid(&pair_flat, "flat"), "{pair_flat:?}");
    let one_flat = r.render_blurred(gpu(), &s.far[0], views, size, &flat);
    must_fail("one frame drawn flat refused", invalid(&one_flat, "flat"));
    for mode in [
        DebugMode::Lens0,
        DebugMode::Lens1,
        DebugMode::Weights,
        DebugMode::SeamDiff,
    ] {
        let name = format!("{mode:?}");
        let mode = opts(mode, Depth::Eight);
        let one = r.render_blurred(gpu(), &s.far[0], views, size, &mode);
        assert!(invalid(&one, &name), "{name}: {one:?}");
        let both = r.render_blurred(gpu(), pair(UNIT), views, size, &mode);
        must_fail(
            "a pair drawn in its own debug mode refused",
            invalid(&both, &name),
        );
    }
    let level0_beside = RigOptions {
        debug: RigDebug::Lens0,
        level0: true,
    }
    .mode();
    assert!(
        matches!(&level0_beside, Err(RenderError::Invalid(m)) if m.contains("level 0")),
        "{level0_beside:?}"
    );
    let alone = RigOptions {
        debug: RigDebug::Lens0,
        level0: false,
    }
    .mode();
    must_fail(
        "mode 4 alone refused",
        matches!(alone, Err(RenderError::Invalid(_))),
    );
    let bad = r.render_blurred(
        gpu(),
        pair([[1.0, -1.0, 1.0], [1.0; 3]]),
        views,
        size,
        &plain,
    );
    assert!(invalid(&bad, "gain"), "{bad:?}");
    let good = r.render_blurred(gpu(), pair(UNIT), views, size, &plain);
    must_fail("unit gains refused", invalid(&good, "gain"));
    assert!(good.is_ok(), "{good:?}");
}

/// `rig::pack` refuses a Mei lens beside a Kannala–Brandt one, a gain
/// that isn't finite and 0 or more, and a binding that isn't tracks 0
/// and 1, one each.
///
/// A Mei–KB rig (made here from synth's lens truths: no file makes one),
/// a gain of −1 and one of NaN, both blocks on track 0: each `Invalid`,
/// naming its cause. Control: the X5 rig packs.
#[test]
fn pack_refuses_what_the_gpu_cant_draw() {
    let _time = timed("pack_refuses_what_the_gpu_cant_draw");
    let s = small();
    let slots = [SlotGeom {
        luma: (SMALL, SMALL),
    }; 2];
    let kb = rig_of_truth(&kb_like(&s.truth), &far());
    let mixed = Rig {
        lenses: [s.far_rig.lenses[0], kb.lenses[1]],
        ..s.far_rig
    };
    let invalid = |r: Result<RigUniform, RenderError>, says: &[&str]| match r {
        Err(RenderError::Invalid(m)) => says.iter().all(|s| m.contains(s)),
        _ => false,
    };
    assert!(invalid(
        pack(&mixed, slots, UNIT),
        &["Mei", "Kannala–Brandt"]
    ));
    for gain in [-1.0, f64::NAN] {
        let gains = [[1.0; 3], [1.0, gain, 1.0]];
        assert!(invalid(pack(&s.far_rig, slots, gains), &["gain"]), "{gain}");
    }
    let mut one_track = s.far_rig;
    one_track.lenses[1].track = 0;
    assert!(invalid(pack(&one_track, slots, UNIT), &["track"]));
    let x5 = pack(&s.far_rig, slots, UNIT);
    must_fail("the X5 rig refused", x5.is_err());
}

/// `rig::pack` keeps a `RigLens::side` unlike its slot's luma size, so
/// the frame term reads the side and the uv the slot.
///
/// frame_term_defeated's rig: reference_rig.rs's wide rig, x5_like at
/// θ_max 104° on [768; 2], each side doubled. It packs, and its mode 4
/// lights the pixels whose texel lies past the slot's right edge as
/// render_rig_of's does, under the CPU rule, on a pair made here whose
/// luma ramps across the columns, so those pixels show the right edge,
/// clamped. The view is a 20° one, 64², on lens 0's texel 2 past the
/// right edge where the circle passes it most (θ 101.55° there), at
/// about the slot's own scale: the mask's margin in angle, 2 texels at
/// 2^λ over 150 texels a radian (0.76° at λ ≤ 0), leaves 1.7° past the
/// edge to light. At 192², with 37.5 texels a radian, the margin's 3.06°
/// passes the 2.45° the circle passes the edge by, and nothing past it
/// is lit. Controls: the true rig lights none of them; those pixels held
/// against the left edge's light, which a wrapping sampler reads, fail.
#[test]
fn pack_keeps_a_side_unlike_its_slot() {
    let _time = timed("pack_keeps_a_side_unlike_its_slot");
    const SIDE: u32 = 768;
    let wide = Tune {
        lenses: [LensTune {
            theta_max_deg: 104.0,
            ..LensTune::default()
        }; 2],
        ..far()
    };
    let wide = sphere_rig(
        &x5_like(),
        &x5_like_planted(),
        &wide,
        FRONT_TRACK,
        [SIDE; 2],
    )
    .unwrap();
    let mut loose = wide;
    for lens in &mut loose.lenses {
        lens.side *= 2;
    }
    let slots = [SlotGeom { luma: (SIDE, SIDE) }; 2];
    let packed = pack(&loose, slots, UNIT).unwrap();
    for lens in packed.lens {
        assert_eq!(lens.frame[2], 2.0 * SIDE as f32);
        assert_eq!(&lens.slot[..2], &[SIDE as f32; 2]);
    }
    // Codes 16 at the left edge to 207 at the right.
    let ramp = columns(SIDE, |x| (16 + x / 4) as u8);
    let frames: [&dyn VideoPlanes; 2] = [&ramp, &ramp];
    let pair = LensPair::new(frames).unwrap();
    let edge = f64::from(SIDE);
    let past_edge = loose
        .unproject(0, DVec2::new(edge + 2.0, 381.0))
        .expect("lens 0 sees past its slot's edge");
    let shot = (
        ViewParams::new(
            DQuat::from_rotation_arc(DVec3::NEG_Z, past_edge),
            Lens::new(Projection::Rectilinear, 20f64.to_radians()),
        ),
        (64, 64),
    );
    let got = draw(frames, &loose, UNIT, &shot, DebugMode::Lens0, Depth::Eight);
    let want = cpu(&pair, &loose, UNIT, &shot, RigDebug::Lens0, false);
    let held = image_rule(&got, &want).unwrap();
    let got = got.unit_rgb();
    let truly = draw(frames, &wide, UNIT, &shot, DebugMode::Lens0, Depth::Eight).unit_rgb();
    // The pixels whose texel lies past the slot's right edge, where the
    // doubled side covers and every tap reads the edge's column.
    let past: Vec<usize> = pixels(shot.1, |i, j| {
        let p = probe(&loose, &shot, i, j);
        let past = p.covers(&loose, 0) && loose.texel(0, p.d).is_some_and(|t| t.x >= edge);
        past.then_some((j * shot.1.0 + i) as usize)
    })
    .into_iter()
    .flatten()
    .collect();
    let lit_truly = past.iter().filter(|&&n| rgb(&truly, n) != [0.0; 3]).count();
    eprintln!(
        "the sides doubled: {:.2} dB, a channel {:.4} off at most; {} pixels past the slot's \
         right edge lit, {lit_truly} of them by the true rig",
        held.db,
        held.worst,
        past.len()
    );
    assert!(past.len() > 10, "{}", past.len());
    must_fail(
        "the true rig lights pixels past the edge",
        above(lit_truly, 0),
    );
    let left = srgb_oetf(round_f16(eotf(16.0 / 255.0)));
    let got_past: Vec<f64> = past.iter().flat_map(|&n| rgb(&got, n)).collect();
    let wrapped = vec![left; got_past.len()];
    let off = must_fail(
        "the left edge's light, as a wrapping sampler reads it",
        rule(&got_past, &wrapped),
    );
    eprintln!("past the edge against the left edge's light: {off}");
}

/// `Projector::record` and `record_strip` refuse a pair whose rig was
/// packed for slots of other sizes than those bound (the M3.7 review's N1
/// and N3): `fs_rig` divides a texel by the packed size, so it would
/// sample the wrong place, and the CPU reference, which divides by the
/// picture's own, can't see it.
///
/// The X5 rig packed for two 192² slots, drawn with a 384² slot on
/// either track, is `Invalid` in both passes, naming the slot; so is a
/// rig packed for a 192² and a 384² slot with its blocks on tracks 1 and
/// 0, drawn with the slots swapped. Control: the slots each rig was
/// packed for record, the blocks' tracks swapped too.
#[test]
fn record_refuses_a_rig_packed_for_other_slots() {
    let _time = timed("record_refuses_a_rig_packed_for_other_slots");
    let s = small();
    let r = renderer();
    let projector = r.projector();
    let filter = StillOptions::default().filter;
    let slot = |side: u32| {
        GpuFrame::new(
            gpu(),
            &SlotShape::of(s.far[0].layout(), (side, side)).unwrap(),
        )
        .unwrap()
    };
    let (own, big) = (slot(SMALL), slot(2 * SMALL));
    let spec = StripSpec {
        px_per_deg: 0.25,
        ..StripSpec::TIER1
    };
    let view = ViewParams::equirect(DQuat::IDENTITY);
    let target = LinearTarget::new(gpu(), (32, 16)).unwrap();
    let strip = LinearTarget::new(gpu(), spec.size()).unwrap();
    // Both passes of `rig` over `lenses`, by track, recorded and never
    // submitted: what each says.
    let both = |lenses: [&GpuFrame; 2], rig: &RigUniform| {
        let mut enc = gpu()
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let source = SourceRef::Rig { lenses, rig };
        [
            projector.record(
                &mut enc,
                source,
                &view,
                &target,
                filter,
                DebugMode::Off,
                None,
            ),
            projector.record_strip(
                &mut enc,
                lenses,
                rig,
                &spec,
                &strip,
                filter,
                DebugMode::Lens0,
            ),
        ]
    };
    let refused = |lenses: [&GpuFrame; 2], rig: &RigUniform| -> Result<(), String> {
        match both(lenses, rig) {
            [Err(RenderError::Invalid(a)), Err(RenderError::Invalid(b))]
                if a.contains("slot") && b.contains("slot") =>
            {
                Ok(())
            }
            said => Err(format!("{said:?}")),
        }
    };
    let packed = pack(&s.far_rig, [SlotGeom::of(&own); 2], UNIT).unwrap();
    refused([&own, &big], &packed).unwrap();
    refused([&big, &own], &packed).unwrap();
    let mut crossed = s.far_rig;
    let [t0, t1] = [crossed.lenses[0].track, crossed.lenses[1].track];
    (crossed.lenses[0].track, crossed.lenses[1].track) = (t1, t0);
    let mixed = pack(&crossed, [SlotGeom::of(&own), SlotGeom::of(&big)], UNIT).unwrap();
    refused([&big, &own], &mixed).unwrap();
    for (lenses, rig) in [([&own, &own], &packed), ([&own, &big], &mixed)] {
        let said = must_fail("the slots the rig was packed for", refused(lenses, rig));
        assert!(said.contains("Ok(())"), "{said}");
    }
}

/// `StillRenderer::strips` matches `seam::strips_of` on the strips' grid.
///
/// The 192² `SeamLines` pair at 0.95 px/° (192²'s own scale): `valid`
/// equals strip_coverage, each valid sample holds the CPU rule against
/// strips_of's, and is an IEEE half, as `StillRenderer::strips` says (its
/// target is `Rgba16Float`, M3 plan amendment 5; the M3.7 review's N2).
/// Controls: the strips drawn as mode 4 ends a picture (f16, then the
/// OETF) miss the rule; strips_of's own strips, which aren't rounded, are
/// not halves.
#[test]
fn gpu_strips_match_strips_of() {
    let _time = timed("gpu_strips_match_strips_of");
    let s = small();
    let spec = StripSpec {
        px_per_deg: 0.95,
        ..StripSpec::TIER1
    };
    let lenses = s.far().map(|frame| StillLens {
        frame,
        from: PlaneSource::Bytes,
    });
    let (got, (want, coverage)) = beside(
        || renderer().strips(gpu(), lenses, &s.far_rig, &spec).unwrap(),
        || {
            (
                strips_of(&s.far_pair, &s.far_rig, &spec).unwrap(),
                strip_coverage(&s.far_rig, &spec).unwrap(),
            )
        },
    );
    let (mut ours, mut theirs, mut drawn) = (Vec::new(), Vec::new(), Vec::new());
    for (b, covered) in coverage.iter().enumerate() {
        let (g, w) = (&got.lenses[b], &want.lenses[b]);
        assert_eq!((g.width, g.height), spec.size());
        assert_eq!(&g.valid, covered, "block {b}'s valid");
        assert_eq!(g.valid, w.valid, "block {b}'s valid");
        for (n, valid) in g.valid.iter().enumerate() {
            if *valid {
                ours.extend(g.rgb[n]);
                theirs.extend(w.rgb[n]);
                drawn.extend(g.rgb[n].map(|v| srgb_oetf(round_f16(v).clamp(0.0, 1.0))));
            } else {
                assert_eq!(g.rgb[n], [0.0; 3], "block {b}'s sample {n}");
            }
        }
    }
    let held = rule(&ours, &theirs).unwrap();
    eprintln!(
        "strips at 0.95 px/°, {} valid samples: {:.2} dB, a channel {:.4} off at most",
        ours.len() / 3,
        held.db,
        held.worst
    );
    let off = must_fail(
        "the strips drawn as mode 4 ends a picture",
        rule(&drawn, &theirs),
    );
    eprintln!("the strips through the OETF: {off}");
    halves(&got).unwrap();
    let unrounded = must_fail("strips_of's strips as halves", halves(&want));
    eprintln!("strips_of's strips: {unrounded}");
}

/// Whether every valid sample of `strips` is an IEEE half: the first that
/// isn't, if any.
fn halves(strips: &Strips) -> Result<(), String> {
    for (b, strip) in strips.lenses.iter().enumerate() {
        for (n, (rgb, valid)) in strip.rgb.iter().zip(&strip.valid).enumerate() {
            if *valid && rgb.iter().any(|&v| round_f16(v) != v) {
                return Err(format!("block {b}'s sample {n}, {rgb:?}, isn't a half"));
            }
        }
    }
    Ok(())
}

/// `reference::rig_lod_map` is what debug mode 3 draws for a pair.
///
/// DebugMode::LodHeatmap at `Depth::Linear` against rig_lod_map: no value
/// more than ½ a level off (D32's LOD rule, as goldens.rs's lod_heatmap
/// holds it). Control: block 0's map drawn wherever block 0 covers (its
/// partner never weighing) misses in block 1's region.
#[test]
fn rig_lod_map_matches_mode_3() {
    let _time = timed("rig_lod_map_matches_mode_3");
    let s = small();
    let shot = equirect(256, 128);
    let mut alone = s.far_rig;
    alone.lenses[1].theta_max = 0.0;
    let (got, (want, block_0)) = beside(
        || {
            draw(
                s.far(),
                &s.far_rig,
                UNIT,
                &shot,
                DebugMode::LodHeatmap,
                Depth::Linear,
            )
            .unit_rgb()
        },
        || {
            (
                rig_lod_map(&s.far_pair, &s.far_rig, &shot.0, shot.1).unwrap(),
                rig_lod_map(&s.far_pair, &alone, &shot.0, shot.1).unwrap(),
            )
        },
    );
    let off_by = |want: &ImageF64| {
        let diffs = got.iter().zip(&want.data).map(|(a, b)| sample_diff(*a, *b));
        let worst = diffs.clone().fold(0.0, f64::max);
        (worst, diffs.filter(|d| *d > 0.5).count())
    };
    let (worst, over) = off_by(&want);
    eprintln!("mode 3 of the pair: the worst value {worst:.4} levels off, {over} more than ½");
    assert_eq!(over, 0, "{worst}");
    let missed = must_fail(
        "block 0's map wherever it covers",
        at_most(off_by(&block_0).1, 0),
    );
    eprintln!("block 0's map against mode 3: {missed} values more than ½ a level off");
}
