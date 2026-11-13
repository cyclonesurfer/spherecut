//! Rung 1's scenes (M3 plan 4.2), lifted from `tests/instrument.rs`
//! (M3.7) for rung 2 and the dual-fisheye goldens to share: the X5-like
//! camera's near scene and its rigs, the Kannala–Brandt camera and its
//! `SeamLines` pair, the views and the stripe of each that holds the band
//! ([`view`]), the ideals, the stitches ([`stitch`], on a pair made
//! [`ready`]), the scores ([`Score`]), the masks ([`Masks`]) and the five
//! scenes ([`Scene`]) with their rows and true rigs. `tests/instrument.rs`
//! holds them to rung 1's floors and band rule, and keeps what its tests
//! alone use: the blessing, the band's rule and its controls, a scene's
//! words and floors in the manifest, and the stripe's own check.
//!
//! The camera, its far tune, its frames (a track or a pair, through any
//! truth) and its views are `synth::rung`'s, which every rung shares
//! (amendment 8): this module calls them and copies none. The rig of a
//! calibration is sphere's through the boundary ([`super::rig`]'s
//! `sphere_rig`), and that of a truth no calibration gives is
//! `rig_of_truth`'s.
//!
//! What is made once here (a camera, a pair's frames, its mip chains, an
//! ideal, a true rig's masks and scores) is made once for the test
//! binary: `RenderCache::shared()` is one process's, as these statics
//! are, so each binary that takes them renders its own pairs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use glam::{DQuat, DVec3};
use spherecut_render::compare::{View, Weights, equirect_coslat, psnr_weighted, ssim_weighted};
use spherecut_render::reference::{ImageF64, LensPair, RigDebug, RigOptions, render_rig_of};
use spherecut_render::seam::{band_weights, lens_coverage};
use spherecut_render::{Framing, ViewParams};
use spherecut_sphere::rig::{Rig, RigLens, Tune};
use spherecut_synth::lens::{CalibTruth, Planted, RigTruth, kb_like};
use spherecut_synth::rung::{self, FRONT_TRACK, LensFrame, SIDES, VIEW_SIDE, far, pair_through};
use spherecut_synth::{Pattern, render_view_ideal};

use super::rig::{rig_of_truth, sphere_rig};

// The cameras and their pictures: rung 1's camera, its far tune, its
// frames and its views are `synth::rung`'s ([`SIDE`]² a lens, [`SS`]²
// samples a pixel, track [`FRONT_TRACK`] in front, [`far`]).

/// Unit gains.
pub const UNIT: [[f64; 3]; 2] = [[1.0; 3]; 2];

/// The scene's radius where it isn't infinitely far, in metres (4.2).
pub const NEAR_M: f64 = 1.5;

/// The default tune stitched at the near scene.
pub fn near() -> Tune {
    Tune {
        q_per_m: 1.0 / NEAR_M,
        ..Tune::default()
    }
}

/// Rung 1's camera (4.2), `synth::rung`'s: synth's X5-like calibration
/// under its planted conventions, and sphere's rig of the same numbers.
pub struct X5 {
    pub cal: CalibTruth,
    pub planted: Planted,
    /// The camera seeing the scene infinitely far, and [`NEAR_M`] away.
    pub truth: RigTruth,
    pub near: RigTruth,
    /// sphere's rig of it stitched at each of the two.
    pub rig: Rig,
    pub near_rig: Rig,
}

pub fn x5() -> &'static X5 {
    static X5: OnceLock<X5> = OnceLock::new();
    X5.get_or_init(|| {
        let camera = rung::camera();
        let (cal, planted) = (&camera.cal, &camera.planted);
        X5 {
            near: RigTruth {
                scene_radius_m: Some(NEAR_M),
                ..camera.truth.clone()
            },
            rig: rig_as(cal, planted, &far()),
            near_rig: rig_as(cal, planted, &near()),
            truth: camera.truth.clone(),
            cal: *cal,
            planted: *planted,
        }
    })
}

/// sphere's rig of `cal` as `planted` reads it, under `tune`.
pub fn rig_as(cal: &CalibTruth, planted: &Planted, tune: &Tune) -> Rig {
    sphere_rig(cal, planted, tune, FRONT_TRACK, SIDES).unwrap()
}

/// The X5-like rig stitched infinitely far, with its tune changed.
pub fn tuned(change: impl FnOnce(&mut Tune)) -> Rig {
    let mut tune = far();
    change(&mut tune);
    rig_as(&x5().cal, &x5().planted, &tune)
}

/// The two tracks' frames of one moment, by track.
pub type Pair = [&'static LensFrame; 2];

pub fn both(frames: &'static [LensFrame; 2]) -> Pair {
    [&frames[0], &frames[1]]
}

/// The X5-like camera's frames of `SeamLines` and of `Grid`, infinitely
/// far (`synth::rung`'s pairs), and of each [`NEAR_M`] away.
pub fn lines() -> &'static [LensFrame; 2] {
    static FRAMES: OnceLock<[LensFrame; 2]> = OnceLock::new();
    FRAMES.get_or_init(|| rung::pair(Pattern::SeamLines))
}

pub fn grid() -> &'static [LensFrame; 2] {
    static FRAMES: OnceLock<[LensFrame; 2]> = OnceLock::new();
    FRAMES.get_or_init(|| rung::pair(Pattern::Grid))
}

pub fn near_lines() -> &'static [LensFrame; 2] {
    static FRAMES: OnceLock<[LensFrame; 2]> = OnceLock::new();
    FRAMES.get_or_init(|| pair_through(Pattern::SeamLines, &x5().near))
}

pub fn near_grid() -> &'static [LensFrame; 2] {
    static FRAMES: OnceLock<[LensFrame; 2]> = OnceLock::new();
    FRAMES.get_or_init(|| pair_through(Pattern::Grid, &x5().near))
}

/// 4.2's second rig: the X5-like camera with `kb_like`'s equidistant
/// Kannala–Brandt lenses, seeing `SeamLines` infinitely far, and sphere's
/// rig of that truth.
pub struct Kb {
    pub truth: RigTruth,
    pub frames: [LensFrame; 2],
    pub rig: Rig,
}

pub fn kb() -> &'static Kb {
    static KB: OnceLock<Kb> = OnceLock::new();
    KB.get_or_init(|| {
        let truth = kb_like(&x5().truth);
        Kb {
            frames: pair_through(Pattern::SeamLines, &truth),
            rig: rig_of_truth(&truth, &far()),
            truth,
        }
    })
}

// Models that are wrong on purpose.

/// `lens` turned `deg` about `about`, an axis of the rig.
pub fn turned(lens: &RigLens, about: DVec3, deg: f64) -> RigLens {
    RigLens {
        lens_from_rig: lens.lens_from_rig
            * DQuat::from_axis_angle(about, deg.to_radians()).inverse(),
        ..*lens
    }
}

/// `rig` with `lens` for its block `b`.
pub fn with_lens(rig: &Rig, b: usize, lens: RigLens) -> Rig {
    let mut lenses = rig.lenses;
    lenses[b] = lens;
    Rig { lenses, ..*rig }
}

// The views, the ideal and the scores.

/// A view and the size of its picture.
pub type Shot = (ViewParams, (u32, u32));

/// One of `synth::rung`'s views as render's type.
pub fn shot(view: rung::Shot) -> Shot {
    let params = match view.lens {
        None => ViewParams::equirect(view.turn),
        Some(lens) => ViewParams::new(view.turn, lens),
    };
    (params, view.size)
}

/// 4.2's equirect, `synth::rung`'s.
pub fn equirect() -> Shot {
    shot(rung::equirect())
}

/// 4.2's rectilinear views, `synth::rung`'s: 90° at [`VIEW_SIDE`]², on
/// each side seam and each pole of the rig, where the seam's circle
/// crosses ±X and ±Y.
pub const VIEWS: [&str; 4] = ["+X", "−X", "+Y", "−Y"];

/// The views in the manifest's keys.
pub const VIEW_KEYS: [&str; 4] = ["pos_x", "neg_x", "pos_y", "neg_y"];

/// View `k`, whole.
pub fn whole_view(k: usize) -> Shot {
    shot(rung::whole_view(k))
}

/// How many of a view's columns (on a pole, its rows) hold the band: the
/// feather's ±5° is ±11 px at the view's middle and ±16 px at its edge,
/// 45° along the seam.
pub const STRIPE: u32 = 48;

/// Whether the seam runs down view `k`, as it does on a side seam; on a
/// pole it runs across.
pub fn seam_runs_down(k: usize) -> bool {
    k < 2
}

/// The stripe of view `k` that the band check reads: the view's own
/// middle [`STRIPE`] columns on a side seam, and its middle rows on a
/// pole. It is the view framed in a smaller target, so each pixel is the
/// whole view's own, from the same rays; the band check reads no pixel
/// outside the band, and [`Scene::band`] holds the band to lie inside the
/// stripe, so the rest of the view isn't rendered.
pub fn view(k: usize) -> Shot {
    let (whole, _) = whole_view(k);
    let side = f64::from(VIEW_SIDE);
    let first = f64::from((VIEW_SIDE - STRIPE) / 2);
    let (rect, size) = if seam_runs_down(k) {
        ([-first, 0.0, side, side], (STRIPE, VIEW_SIDE))
    } else {
        ([0.0, -first, side, side], (VIEW_SIDE, STRIPE))
    };
    (whole.framed(Framing { rect, dim: 1.0 }), size)
}

/// The samples a side the ideal takes for a pixel: the render goldens' 8
/// for the equirect's 0.70° a pixel, and 4 for the views' 0.35°. At 8 the
/// stitch of `SeamLines` scores within 0.05 dB of what it scores at 16.
/// `Grid`'s score still moves by a dB with the count (31.1, 28.4 and
/// 29.6 dB at 4, 8 and 16), its lines lying along the equirect's rows
/// and columns, so its floors hold the stitch to this ideal and say
/// little of how good the stitch is.
pub const IDEAL_N: u32 = 8;
pub const IDEAL_N_VIEW: u32 = 4;

pub fn ideal(pattern: Pattern, (view, size): &Shot, n: u32) -> Vec<f64> {
    let image = render_view_ideal(pattern, |x, y| Some(view.dir(*size, x, y)), *size, n);
    image.pixels.into_iter().flatten().collect()
}

/// A pattern's place among the ideals.
pub fn slot(pattern: Pattern) -> usize {
    match pattern {
        Pattern::SeamLines => 0,
        Pattern::Grid => 1,
        _ => panic!("rung 1's scene is SeamLines and Grid"),
    }
}

/// The ideal equirect of a pattern, and its ideal view `k`.
pub fn ideal_equirect(pattern: Pattern) -> &'static [f64] {
    static IDEALS: [OnceLock<Vec<f64>>; 2] = [const { OnceLock::new() }; 2];
    IDEALS[slot(pattern)].get_or_init(|| ideal(pattern, &equirect(), IDEAL_N))
}

pub fn ideal_view(pattern: Pattern, k: usize) -> &'static [f64] {
    static IDEALS: [[OnceLock<Vec<f64>>; 4]; 2] = [const { [const { OnceLock::new() }; 4] }; 2];
    IDEALS[slot(pattern)][k].get_or_init(|| ideal(pattern, &view(k), IDEAL_N_VIEW))
}

/// A pair of frames made ready to sample, once for the binary: its two
/// pictures' mip chains, which `render_rig` and `strips` build again at
/// each call (about 10 ms a pair at 768²) and every stitch and strip of
/// the pair here shares. `render_rig` and `strips` are `render_rig_of`
/// and `strips_of` on a pair made for the call, and the rig reference's
/// own tests hold the two ways to the same bits
/// (`a_pair_made_ready_once_serves_every_rig`). The frames are the
/// binary's own, each made once, so a pair is told by where its two
/// frames lie.
pub fn ready(frames: Pair) -> Arc<LensPair> {
    type Cell = Arc<OnceLock<Arc<LensPair>>>;
    static PAIRS: Mutex<BTreeMap<[usize; 2], Cell>> = Mutex::new(BTreeMap::new());
    let cell = PAIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(frames.map(|frame| std::ptr::from_ref(frame).addr()))
        .or_default()
        .clone();
    cell.get_or_init(|| Arc::new(LensPair::new([frames[0], frames[1]]).unwrap()))
        .clone()
}

/// `rig`'s picture of a pair through a shot, in display light:
/// `render_rig`'s, by `render_rig_of` on the pair made [`ready`].
pub fn stitch(
    frames: Pair,
    rig: &Rig,
    gains: [[f64; 3]; 2],
    shot: &Shot,
    debug: RigDebug,
) -> ImageF64 {
    let opts = RigOptions {
        debug,
        level0: false,
    };
    render_rig_of(&ready(frames), rig, gains, &shot.0, shot.1, &opts).unwrap()
}

/// A lens alone: 10.3's debug modes 4 and 5.
pub const ALONE: [RigDebug; 2] = [RigDebug::Lens0, RigDebug::Lens1];

/// A picture against the ideal under a mask: PSNR in dB, and SSIM.
#[derive(Debug, Clone, Copy)]
pub struct Score {
    pub psnr: f64,
    pub ssim: f64,
}

pub fn psnr(got: &ImageF64, want: &[f64], mask: &[f64]) -> f64 {
    let (width, height) = (got.width as usize, got.height as usize);
    let weights = Weights {
        width,
        height,
        w: mask,
    };
    let got = View::new(width, height, 3, &got.data[..]);
    psnr_weighted(got, View::new(width, height, 3, want), &weights).unwrap()
}

pub fn ssim(got: &[f64], want: &[f64], mask: &[f64], (width, height): (usize, usize)) -> f64 {
    let weights = Weights {
        width,
        height,
        w: mask,
    };
    let got = View::new(width, height, 3, got);
    ssim_weighted(got, View::new(width, height, 3, want), &weights).unwrap()
}

/// A picture's score: its PSNR under `mask`, and its SSIM over the
/// windows whose centres `windows` holds.
pub fn score(got: &ImageF64, want: &[f64], mask: &[f64], windows: &[f64]) -> Score {
    let size = (got.width as usize, got.height as usize);
    Score {
        psnr: psnr(got, want, mask),
        ssim: ssim(&got.data, want, windows, size),
    }
}

/// The equirect's masks for a true rig (8.1), each weighted by
/// cos(latitude): more than 12° from the seam, and each lens's own
/// coverage; and, unweighted, where both lenses cover. A mask is the
/// true rig's, so that a wrong model's holes count against it.
pub struct Masks {
    pub past_12: Vec<f64>,
    pub alone: [Vec<f64>; 2],
    /// Each lens's coverage for the SSIM, whose windows count by their
    /// centres: the pixels whose whole window the lens covers
    /// ([`whole_windows`]). A lens alone is black past its coverage, so a
    /// window that reached past it would hold the ideal's scene against
    /// black, which is the mask's edge and nothing of the stitch.
    pub alone_windows: [Vec<f64>; 2],
    pub overlap: Vec<f64>,
}

/// 4.2's "more than 12° from the seam".
pub const OFF_SEAM_DEG: f64 = 12.0;

/// How far an SSIM window reaches from its centre, in pixels: the half
/// width of `ssim_weighted`'s 11 × 11.
pub const WINDOW_REACH: usize = 5;

/// `mask` less every pixel whose 11 × 11 window holds a pixel of weight
/// 0 or passes the picture's edge: the centres of the windows that lie
/// whole inside the mask.
pub fn whole_windows(mask: &[f64], width: usize) -> Vec<f64> {
    let (height, reach) = (mask.len() / width, WINDOW_REACH);
    let covered = |x: usize, y: usize| {
        let row = |j: usize| &mask[j * width + x - reach..=j * width + x + reach];
        (y - reach..=y + reach).all(|j| row(j).iter().all(|w| *w > 0.0))
    };
    let whole = |x: usize, y: usize| {
        let inside = (reach..width.saturating_sub(reach)).contains(&x)
            && (reach..height.saturating_sub(reach)).contains(&y);
        inside && covered(x, y)
    };
    (0..mask.len())
        .map(|at| {
            if whole(at % width, at / width) {
                mask[at]
            } else {
                0.0
            }
        })
        .collect()
}

impl Masks {
    pub fn of(rig: &Rig) -> Masks {
        let (view, size) = equirect();
        let width = size.0 as usize;
        let coslat = equirect_coslat(width, size.1 as usize);
        let weigh =
            |mask: &[f64]| -> Vec<f64> { mask.iter().zip(&coslat).map(|(m, c)| m * c).collect() };
        let cover = [0, 1].map(|b| lens_coverage(rig, b, &view, size));
        Masks {
            past_12: weigh(&band_weights(rig, &view, size, OFF_SEAM_DEG, 90.0)),
            alone: [weigh(&cover[0]), weigh(&cover[1])],
            alone_windows: [0, 1].map(|b| weigh(&whole_windows(&cover[b], width))),
            overlap: cover[0].iter().zip(&cover[1]).map(|(a, b)| a * b).collect(),
        }
    }
}

/// A pair of pictures that rung 1 stitches and holds to the ideal, with
/// its true rig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// The X5-like camera: `SeamLines` and `Grid` infinitely far, and
    /// each 1.5 m away.
    Lines,
    Grid,
    NearLines,
    NearGrid,
    /// The Kannala–Brandt camera, `SeamLines` infinitely far.
    KbLines,
}

/// How many scenes there are, for what is kept by scene.
pub const SCENES: usize = 5;

impl Scene {
    /// The scene's row in the manifest.
    pub fn row(self) -> &'static str {
        match self {
            Scene::Lines => "x5_seamlines",
            Scene::Grid => "x5_grid",
            Scene::NearLines => "x5_seamlines_1m5",
            Scene::NearGrid => "x5_grid_1m5",
            Scene::KbLines => "kb_seamlines",
        }
    }

    pub fn pattern(self) -> Pattern {
        match self {
            Scene::Grid | Scene::NearGrid => Pattern::Grid,
            Scene::Lines | Scene::NearLines | Scene::KbLines => Pattern::SeamLines,
        }
    }

    pub fn frames(self) -> Pair {
        match self {
            Scene::Lines => both(lines()),
            Scene::Grid => both(grid()),
            Scene::NearLines => both(near_lines()),
            Scene::NearGrid => both(near_grid()),
            Scene::KbLines => both(&kb().frames),
        }
    }

    /// The scene's true rig.
    pub fn rig(self) -> Rig {
        match self {
            Scene::Lines | Scene::Grid => x5().rig,
            Scene::NearLines | Scene::NearGrid => x5().near_rig,
            Scene::KbLines => kb().rig,
        }
    }

    /// Which of the true rigs is the scene's: the X5-like camera's
    /// stitched infinitely far, the same stitched 1.5 m away, and the
    /// Kannala–Brandt camera's. What depends on the rig alone (the masks,
    /// the band) is kept by this, for the two patterns to share.
    pub fn camera(self) -> usize {
        match self {
            Scene::Lines | Scene::Grid => 0,
            Scene::NearLines | Scene::NearGrid => 1,
            Scene::KbLines => 2,
        }
    }

    /// The true rig with lens 1 yawed 0.5° (`angle` 0) or rolled 0.5°
    /// (`angle` 2), the band check's controls: by the tune on the X5-like
    /// camera, at the scene's own stitch distance, and turned about +Y or
    /// about the lens axis on the Kannala–Brandt rig, which takes no
    /// angles from a tune. A yaw moves the seam across itself, most on
    /// its sides, ±X, and not at all on its poles, ±Y; a roll moves it
    /// along itself by the same 0.5° all round.
    pub fn turned(self, angle: usize) -> Rig {
        match self {
            Scene::Lines | Scene::Grid => tuned(|t| t.lenses[1].ypr_deg[angle] = 0.5),
            Scene::NearLines | Scene::NearGrid => {
                let mut tune = near();
                tune.lenses[1].ypr_deg[angle] = 0.5;
                rig_as(&x5().cal, &x5().planted, &tune)
            }
            Scene::KbLines => {
                let rig = kb().rig;
                let about = [DVec3::Y, DVec3::X, DVec3::Z][angle];
                with_lens(&rig, 1, turned(&rig.lenses[1], about, 0.5))
            }
        }
    }

    /// The true rig's masks.
    pub fn masks(self) -> &'static Masks {
        static MASKS: [OnceLock<Masks>; 3] = [const { OnceLock::new() }; 3];
        MASKS[self.camera()].get_or_init(|| Masks::of(&self.rig()))
    }

    /// `model`'s stitch of the scene against the ideal, more than 12°
    /// from the seam.
    pub fn stitched(self, model: &Rig) -> Score {
        let got = stitch(self.frames(), model, UNIT, &equirect(), RigDebug::Off);
        let mask = &self.masks().past_12;
        score(&got, ideal_equirect(self.pattern()), mask, mask)
    }

    /// `model`'s block `b` alone against the ideal, over the true rig's
    /// coverage of it: the PSNR over every pixel of it, the SSIM over
    /// the windows that lie whole inside it.
    pub fn alone(self, model: &Rig, b: usize) -> Score {
        let got = stitch(self.frames(), model, UNIT, &equirect(), ALONE[b]);
        let masks = self.masks();
        score(
            &got,
            ideal_equirect(self.pattern()),
            &masks.alone[b],
            &masks.alone_windows[b],
        )
    }

    /// The true rig's three scores, measured once: the stitch, and each
    /// lens alone.
    pub fn truth(self) -> &'static [Score; 3] {
        static SCORES: [OnceLock<[Score; 3]>; SCENES] = [const { OnceLock::new() }; SCENES];
        SCORES[self as usize].get_or_init(|| {
            let rig = self.rig();
            [
                self.stitched(&rig),
                self.alone(&rig, 0),
                self.alone(&rig, 1),
            ]
        })
    }
}

/// 4.2's band check on the 256² views: the stitch against the better of
/// the two lenses alone, over the pixels where both of the true rig's
/// weights are non-zero.
impl Scene {
    /// The true rig's band in view `k`'s stripe: 1 where both of its
    /// weights are non-zero, 0 elsewhere, from 10.3's debug mode 6. The
    /// band lies inside the stripe: no pixel of the stripe's two outer
    /// lines is in it.
    pub fn band(self, k: usize) -> &'static [f64] {
        static BANDS: [[OnceLock<Vec<f64>>; 4]; 3] = [const { [const { OnceLock::new() }; 4] }; 3];
        BANDS[self.camera()][k].get_or_init(|| {
            let weights = stitch(
                self.frames(),
                &self.rig(),
                UNIT,
                &view(k),
                RigDebug::Weights,
            );
            let in_band = |px: &[f64]| if px[0] > 0.0 && px[2] > 0.0 { 1.0 } else { 0.0 };
            let band: Vec<f64> = weights.data.chunks_exact(3).map(in_band).collect();
            let (width, height) = (weights.width as usize, weights.height as usize);
            let outer = |n: usize| {
                let (x, y) = (n % width, n / width);
                if seam_runs_down(k) {
                    x == 0 || x == width - 1
                } else {
                    y == 0 || y == height - 1
                }
            };
            let (inside, edge) = (0..band.len()).fold((0, 0), |(inside, edge), n| {
                let held = usize::from(band[n] > 0.0);
                (inside + held, edge + held * usize::from(outer(n)))
            });
            assert!(
                inside > 0 && edge == 0,
                "{} {}: the band is {inside} pixels of the stripe, {edge} on its outer lines",
                self.row(),
                VIEWS[k]
            );
            band
        })
    }

    /// One of `model`'s pictures of view `k` against the ideal over the
    /// band: its PSNR, in dB.
    pub fn in_band(self, model: &Rig, k: usize, debug: RigDebug) -> f64 {
        let got = stitch(self.frames(), model, UNIT, &view(k), debug);
        psnr(&got, ideal_view(self.pattern(), k), self.band(k))
    }

    /// The three PSNRs of a model in the band of view `k`: its stitch,
    /// and each of its lenses alone.
    pub fn band_scores(self, model: &Rig, k: usize) -> [f64; 3] {
        [RigDebug::Off, ALONE[0], ALONE[1]].map(|debug| self.in_band(model, k, debug))
    }

    /// The true rig's three, measured once.
    pub fn band_truth(self, k: usize) -> &'static [f64; 3] {
        static SCORES: [[OnceLock<[f64; 3]>; 4]; SCENES] =
            [const { [const { OnceLock::new() }; 4] }; SCENES];
        SCORES[self as usize][k].get_or_init(|| self.band_scores(&self.rig(), k))
    }

    /// The three for a model that differs from the true rig in lens 1
    /// alone: lens 0's picture is the true rig's.
    pub fn band_scores_lens_1(self, model: &Rig, k: usize) -> [f64; 3] {
        assert_eq!(model.lenses[0], self.rig().lenses[0]);
        [
            self.in_band(model, k, RigDebug::Off),
            self.band_truth(k)[1],
            self.in_band(model, k, ALONE[1]),
        ]
    }
}
