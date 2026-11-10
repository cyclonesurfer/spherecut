//! The seam instrument's own tests (M3 plan 8.2, and 8.3's self-tests):
//! `seam::bin_shifts` and `seam::signature` on strips and bins whose
//! shifts are planted, and the predicted fields (`common::predict`)
//! against 3.4's arithmetic. They prove the instrument consistent with
//! itself; only real footage speaks for the camera.
//!
//! The strips are made here and not by `seam::strips`: the band is
//! synth's `SeamLines` through `render_view_ideal` over `StripSpec::dir`,
//! in linear light, and lens 1's strip is that picture resampled a
//! planted shift away, by a Lanczos kernel the estimator doesn't use.
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): 2.5 s of M3.4's 15 s, which each
//! test prints its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::f64::consts::PI;
use std::sync::OnceLock;
use std::time::Instant;

use glam::DVec3;
use proptest::prelude::*;
use spherecut_render::seam::shifts::{
    Method, TERMS, bin_count, bin_shifts_by, patch, signature_of,
};
use spherecut_render::seam::{
    BinShift, Signature, Strip, StripSpec, Strips, bin_shifts, signature,
};
use spherecut_sphere::rig::{Rig, RigLens, Tune};
use spherecut_synth::lens::{
    Axis, CalibTruth, Generation, Motion, PairOrder, Planted, Prism, PrismAt, PrismOrder, RigTruth,
    Sign, Tangential, TruthBinding, TruthCanvas, TruthEuler, TruthTurn, TruthTz, TruthV6, Turn,
    X5_LIKE_SIDE, compose, x5_like, x5_like_planted, x5_like_v6,
};
use spherecut_synth::{Pattern, render_view_ideal};
use spherecut_testkit::control::{at_least, at_most, must_fail};

use common::predict::{predict_bins, predict_through};
use common::rig::{pats, sphere_rig};

/// The seam's own tests' share of M3.4's budget, in seconds (19.6).
const BUDGET_S: f64 = 2.5;

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
            "{}: {:.3} s; the suite {suite:.3} s so far, of the seam tests' {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

// The strips.

/// Tier 1's strips: 1372 × 61, 72 bins of 5°, a search of ±3°.
const SPEC: StripSpec = StripSpec::TIER1;

/// A strip's pixels a degree along it and across it: its own, 3.811 and
/// 3.8125, which rounding its size leaves a little off 3.81.
fn scales() -> (f64, f64) {
    let (width, height) = SPEC.size();
    (
        f64::from(width) / 360.0,
        f64::from(height) / (2.0 * SPEC.half_band_deg),
    )
}

/// The rows a [`Tall`] picture has above a strip's first and below its
/// last: enough for a shift of 12 px across with the resampling's reach.
const MARGIN: usize = 16;

/// How many points a side `render_view_ideal` averages each pixel over.
const SUPERSAMPLE: u32 = 4;

/// A picture of the band, as a strip is but [`MARGIN`] rows taller at
/// each edge, so that a strip resampled from it across the seam has
/// something to read. In linear light.
struct Tall {
    width: usize,
    rgb: Vec<[f64; 3]>,
}

fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// `SeamLines` over the band: the scene itself at each pixel's
/// directions, the sRGB EOTF of its values.
fn scene() -> &'static Tall {
    static SCENE: OnceLock<Tall> = OnceLock::new();
    SCENE.get_or_init(|| {
        let (width, height) = SPEC.size();
        let image = render_view_ideal(
            Pattern::SeamLines,
            |x, y| Some(SPEC.dir(x, y - MARGIN as f64)),
            (width, height + 2 * MARGIN as u32),
            SUPERSAMPLE,
        );
        Tall {
            width: width as usize,
            rgb: image.pixels.iter().map(|p| p.map(srgb_eotf)).collect(),
        }
    })
}

/// A picture whose grey at the strip's point (x, y) is `value`'s, read at
/// each pixel's centre.
fn tall_of(value: impl Fn(f64, f64) -> f64) -> Tall {
    let (width, height) = SPEC.size();
    let rows = height as usize + 2 * MARGIN;
    let rgb = (0..rows)
        .flat_map(|j| (0..width).map(move |i| (i, j)))
        .map(|(i, j)| [value(f64::from(i) + 0.5, j as f64 + 0.5 - MARGIN as f64); 3])
        .collect();
    Tall {
        width: width as usize,
        rgb,
    }
}

/// The strip at the middle of `tall`: lens 0's.
fn strip(tall: &Tall) -> Strip {
    carried(tall, |_| (0.0, 0.0))
}

/// Lanczos' kernel of four lobes a side.
fn lanczos(x: f64) -> f64 {
    const LOBES: f64 = 4.0;
    if x == 0.0 {
        1.0
    } else if x.abs() >= LOBES {
        0.0
    } else {
        LOBES * (PI * x).sin() * (PI * x / LOBES).sin() / (PI * PI * x * x)
    }
}

/// The eight samples that give a picture `back` samples before a sample,
/// and their weights: the first one's offset from that sample, then the
/// weights, which sum to 1.
fn taps(back: f64) -> (i64, [f64; 8]) {
    let whole = (-back).floor();
    let part = -back - whole;
    let weights: [f64; 8] = std::array::from_fn(|k| lanczos(k as f64 - 3.0 - part));
    let sum: f64 = weights.iter().sum();
    (whole as i64 - 3, weights.map(|w| w / sum))
}

/// Lens 1's strip of a shift field planted in `tall`: what `tall` shows
/// at (φ, ε), this shows at (φ + along, ε + across), for `field(φ)` =
/// (across, along) in degrees, φ in degrees. So `bin_shifts` of
/// `strip(tall)` and this should give `field` back.
fn carried(tall: &Tall, field: impl Fn(f64) -> (f64, f64)) -> Strip {
    let (width, height) = SPEC.size();
    let (w, h) = (width as usize, height as usize);
    let (sx, sy) = scales();
    let mut rgb = vec![[0.0; 3]; w * h];
    let mut column = vec![[0.0; 3]; h + 2 * MARGIN];
    for i in 0..w {
        // The φ the scene left to arrive at this column's.
        let here = (i as f64 + 0.5) / sx;
        let mut from = here;
        for _ in 0..8 {
            from = here - field(from).1;
        }
        let (across, along) = field(from);
        // The strip's y runs down, against ε.
        let (first_x, wx) = taps(along * sx);
        let (first_y, wy) = taps(-across * sy);
        for (j, sum) in column.iter_mut().enumerate() {
            *sum = [0.0; 3];
            for (k, weight) in wx.iter().enumerate() {
                let x = (i as i64 + first_x + k as i64).rem_euclid(w as i64) as usize;
                for (sum, v) in sum.iter_mut().zip(tall.rgb[j * tall.width + x]) {
                    *sum += weight * v;
                }
            }
        }
        for j in 0..h {
            let mut sum = [0.0; 3];
            for (k, weight) in wy.iter().enumerate() {
                let y = (j + MARGIN) as i64 + first_y + k as i64;
                for (sum, v) in sum.iter_mut().zip(column[y as usize]) {
                    *sum += weight * v;
                }
            }
            rgb[j * w + i] = sum;
        }
    }
    Strip {
        width,
        height,
        valid: vec![true; w * h],
        rgb,
    }
}

/// A field that is one shift everywhere: (u, v) in strip pixels, u along
/// the strip and v down it.
fn uniform(u: f64, v: f64) -> impl Fn(f64) -> (f64, f64) {
    let (sx, sy) = scales();
    move |_| (-v / sy, u / sx)
}

/// `strip` with each channel times its gain.
fn gained(strip: &Strip, gains: [f64; 3]) -> Strip {
    Strip {
        rgb: strip
            .rgb
            .iter()
            .map(|p| [p[0] * gains[0], p[1] * gains[1], p[2] * gains[2]])
            .collect(),
        ..strip.clone()
    }
}

/// The scene sampled again a shift of (−2.5, 0.25) px away, as a second
/// lens samples it: not `scene()` resampled. The two samplings alias the
/// 1° checker differently, most of all half a pixel apart.
fn scene_again() -> &'static Strip {
    static AGAIN: OnceLock<Strip> = OnceLock::new();
    AGAIN.get_or_init(|| {
        let (width, height) = SPEC.size();
        let (u, v) = SCENE_AGAIN;
        let image = render_view_ideal(
            Pattern::SeamLines,
            |x, y| Some(SPEC.dir(x - u, y - v)),
            (width, height),
            SUPERSAMPLE,
        );
        Strip {
            width,
            height,
            rgb: image.pixels.iter().map(|p| p.map(srgb_eotf)).collect(),
            valid: vec![true; image.pixels.len()],
        }
    })
}

/// [`scene_again`]'s shift, in strip pixels: whole steps of the
/// supersampling's lattice, so that both samplings read the same points
/// of the scene.
const SCENE_AGAIN: (f64, f64) = (-2.5, 0.25);

// What came back.

/// The greater of the worst miss so far and one more, and NaN from the
/// first that is NaN on: a miss that is no number (an invalid bin, a term
/// the fit couldn't say) is never passed over for the misses around it.
fn worse(worst: f64, miss: f64) -> f64 {
    if worst.is_nan() || miss <= worst {
        worst
    } else {
        miss
    }
}

/// The furthest any bin's shift lies from (u, v), in strip pixels; NaN
/// if a bin is invalid.
fn worst_miss(bins: &[BinShift], (u, v): (f64, f64)) -> f64 {
    let (sx, sy) = scales();
    bins.iter()
        .map(|b| (b.along_deg * sx - u).hypot(-b.across_deg * sy - v))
        .fold(0.0, worse)
}

/// A signature's eight terms, in `signature_of`'s order.
fn terms(s: &Signature) -> [f64; TERMS] {
    [s.a0, s.a1, s.b1, s.a2, s.b2, s.c0, s.c1, s.d1]
}

const NAMES: [&str; TERMS] = ["a0", "a1", "b1", "a2", "b2", "c0", "c1", "d1"];

/// The furthest any term of `got` lies from `want`'s; NaN if one is.
fn worst_term(got: [f64; TERMS], want: [f64; TERMS]) -> f64 {
    got.iter()
        .zip(want)
        .map(|(g, w)| (g - w).abs())
        .fold(0.0, worse)
}

/// The field of the eight terms, as `carried` takes it.
fn field_of(t: [f64; TERMS]) -> impl Fn(f64) -> (f64, f64) {
    move |phi_deg| {
        let (sin, cos) = phi_deg.to_radians().sin_cos();
        let (sin2, cos2) = (2.0 * phi_deg).to_radians().sin_cos();
        (
            t[0] + t[1] * cos + t[2] * sin + t[3] * cos2 + t[4] * sin2,
            t[5] + t[6] * cos + t[7] * sin,
        )
    }
}

/// A pure shift planted by resampling one strip comes back within
/// 0.02 px.
#[test]
fn a_pure_shift_comes_back_within_0_02_px() {
    let _t = timed("a_pure_shift_comes_back_within_0_02_px");
    const WITHIN: f64 = 0.02;
    let (sx, sy) = scales();
    let lens0 = strip(scene());
    let pair = |shift: (f64, f64)| Strips {
        lenses: [lens0.clone(), carried(scene(), uniform(shift.0, shift.1))],
    };

    // Quarters and halves of a pixel, where an interpolation errs most,
    // shifts of no round number, one at the search's edge along the
    // seam (2.97°), and the focal control's 2.2° across (3.4), which is
    // more than the 1° checker's period: there the bins with a line in
    // them place the others.
    let shifts = [
        (0.75, -1.25),
        (-2.5, 0.25),
        (0.37, 0.61),
        (-5.5, 4.5),
        (11.3, -0.5),
        (0.25, 8.5),
    ];
    for shift in shifts {
        let bins = bin_shifts(&pair(shift), &SPEC);
        assert_eq!(bins.len(), 72);
        assert!(bins.iter().all(|b| b.valid), "{shift:?}");
        let miss = worst_miss(&bins, shift);
        let s = signature(&bins);
        eprintln!(
            "a shift of {shift:?} px: every bin within {miss:.4} px; a0 {:+.4}, c0 {:+.4} px; \
             NCC {:.4}",
            -s.a0 * sy,
            s.c0 * sx,
            s.median_ncc
        );
        assert!(
            at_most(miss, WITHIN).passes() == Some(true),
            "{shift:?}: {miss}"
        );
        assert!(s.median_ncc > 0.999, "{shift:?}: {}", s.median_ncc);
    }

    // Control: the coarse search alone, with no Lucas–Kanade step, lands
    // on whole pixels.
    let strips = pair(shifts[0]);
    let coarse = Method {
        lk_steps: 0,
        ..Method::PLAN
    };
    let miss = must_fail(
        "NCC alone",
        at_most(
            worst_miss(&bin_shifts_by(&strips, &SPEC, &coarse), shifts[0]),
            WITHIN,
        ),
    );
    eprintln!("control, NCC alone: off by {miss:.4} px");
    assert!((miss - 0.25 * 2f64.sqrt()).abs() < 1e-9, "{miss}");

    // Control: cells of 1° with no lines repeat every 2° across, and
    // nothing places the 2.2° shift: it comes back at the match nearest
    // zero, a period short.
    let cells = tall_of(|x, y| 0.3 + 0.15 * (PI * x / sx).sin() * (PI * y / sy).sin());
    let big = shifts[5];
    let strips = Strips {
        lenses: [strip(&cells), carried(&cells, uniform(big.0, big.1))],
    };
    let bins = bin_shifts(&strips, &SPEC);
    assert!(bins.iter().all(|b| b.valid));
    let miss = must_fail(
        "cells with no lines",
        at_most(worst_miss(&bins, big), WITHIN),
    );
    eprintln!("control, cells with no lines: off by {miss:.4} px");
    assert!((miss - 2.0 * sy).abs() < WITHIN, "{miss}");

    // The scene sampled twice, as two lenses sample it, is no pure shift:
    // each sampling aliases the checker its own way. Reported for the
    // attribution matrix (8.3), and held loosely: every bin within
    // 0.05 px, and the uniform terms within 0.005 px.
    let strips = Strips {
        lenses: [lens0.clone(), scene_again().clone()],
    };
    let bins = bin_shifts(&strips, &SPEC);
    let s = signature(&bins);
    let miss = worst_miss(&bins, SCENE_AGAIN);
    let uniform_miss = (s.c0 * sx - SCENE_AGAIN.0).hypot(-s.a0 * sy - SCENE_AGAIN.1);
    eprintln!(
        "the scene sampled again {SCENE_AGAIN:?} px away: every bin within {miss:.4} px, the \
         uniform terms within {uniform_miss:.4} px; NCC {:.4}, log gains {:?}",
        s.median_ncc, s.log_gain
    );
    assert!(at_most(miss, 0.05).passes() == Some(true), "{miss}");
    assert!(
        at_most(uniform_miss, 0.005).passes() == Some(true),
        "{uniform_miss}"
    );
}

/// Swapping the two strips flips the sign of every term.
#[test]
fn swapping_the_strips_flips_every_term() {
    let _t = timed("swapping_the_strips_flips_every_term");
    // Every term planted, in degrees, and lens 1 a fifth brighter.
    let planted = [0.30, -0.20, 0.25, 0.15, -0.10, 0.20, 0.18, -0.22];
    let gain = 1.2f64;
    let lens0 = strip(scene());
    let lens1 = gained(&carried(scene(), field_of(planted)), [gain; 3]);
    let there = signature(&bin_shifts(
        &Strips {
            lenses: [lens0.clone(), lens1.clone()],
        },
        &SPEC,
    ));
    let back = signature(&bin_shifts(
        &Strips {
            lenses: [lens1.clone(), lens0.clone()],
        },
        &SPEC,
    ));
    for (name, (t, b)) in NAMES.iter().zip(terms(&there).iter().zip(terms(&back))) {
        eprintln!("{name}: {t:+.5}° there, {b:+.5}° back");
    }
    eprintln!(
        "log gain {:+.5} there, {:+.5} back; rest {:.5}° and {:.5}°",
        there.log_gain[1], back.log_gain[1], there.rest_rms_deg, back.rest_rms_deg
    );
    assert_eq!((there.valid_bins, back.valid_bins), (72, 72));

    // The field comes back as planted.
    let miss = worst_term(terms(&there), planted);
    eprintln!("the planted terms within {miss:.5}°");
    assert!(at_most(miss, 0.002).passes() == Some(true), "{miss}");

    // Swapped, every term flips. The field that carries lens 1's strip
    // back is the planted one's inverse, which is its negative but for
    // the product of the field and its slope: 0.3° · 0.3° in radians,
    // 0.0016°.
    let flipped = |s: &Signature| {
        let negated = terms(&there).map(|t| -t);
        at_most(worst_term(terms(s), negated), 0.005)
    };
    assert!(flipped(&back).passes() == Some(true), "{}", flipped(&back));
    assert!(
        (back.log_gain[1] + gain.ln()).abs() < 0.002,
        "{:?}",
        back.log_gain
    );
    assert!(
        (there.log_gain[1] - gain.ln()).abs() < 0.002,
        "{:?}",
        there.log_gain
    );

    // Control: the strips as they were flip nothing.
    let miss = must_fail("the strips unswapped", flipped(&there));
    eprintln!("control, the strips unswapped: a term {miss:.4}° from its negative");
    assert!((miss - 0.6).abs() < 0.01, "{miss}");
}

/// Gains on one strip leave the geometric terms under 0.005°, and come
/// back as the log gains.
#[test]
fn gains_leave_the_geometry_under_0_005_deg() {
    let _t = timed("gains_leave_the_geometry_under_0_005_deg");
    const WITHIN: f64 = 0.005;
    // 4.2's exposure control: lens 1 at +0.5 EV and +5 % red.
    let gains = [2f64.sqrt() * 1.05, 2f64.sqrt(), 2f64.sqrt()];
    let want = gains.map(f64::ln);
    // Lens 1 is the scene and lens 0 the strip resampled a shift away, so
    // that the brighter strip has none of the resampling's overshoot to
    // clip.
    let shift = (0.75, -1.25);
    let lens0 = carried(scene(), uniform(shift.0, shift.1));
    let plain = Strips {
        lenses: [lens0.clone(), strip(scene())],
    };
    let bright = Strips {
        lenses: [lens0, gained(&strip(scene()), gains)],
    };
    assert!(bright.lenses[1].rgb.iter().flatten().all(|&v| v <= 0.95));

    let measure = |strips: &Strips, method: &Method| {
        let bins = bin_shifts_by(strips, &SPEC, method);
        (signature(&bins), bins)
    };
    let (unit, unit_bins) = measure(&plain, &Method::PLAN);
    let (lit, lit_bins) = measure(&bright, &Method::PLAN);
    let moved = |a: &[BinShift], b: &[BinShift]| {
        a.iter()
            .zip(b)
            .map(|(a, b)| (a.across_deg - b.across_deg).hypot(a.along_deg - b.along_deg))
            .fold(0.0, worse)
    };
    let geometry = worst_term(terms(&lit), terms(&unit));
    eprintln!(
        "gains {gains:?}: the terms move {geometry:.2e}°, a bin at most {:.2e}°; log gains {:?} \
         for {want:?}",
        moved(&lit_bins, &unit_bins),
        lit.log_gain
    );
    assert_eq!((unit.valid_bins, lit.valid_bins), (72, 72));
    assert!(
        at_most(geometry, WITHIN).passes() == Some(true),
        "{geometry}"
    );
    // And the shift is the planted one, backwards.
    let miss = worst_miss(&lit_bins, (-shift.0, -shift.1));
    assert!(at_most(miss, 0.02).passes() == Some(true), "{miss}");
    for (got, want) in lit.log_gain.iter().zip(want) {
        assert!((got - want).abs() < 0.001, "{:?}", lit.log_gain);
    }
    assert!(
        unit.log_gain.iter().all(|g| g.abs() < 0.001),
        "{:?}",
        unit.log_gain
    );

    // Control: steps on the patches as they stand take the gain for a
    // shift.
    let unnormalised = Method {
        normalise: false,
        ..Method::PLAN
    };
    let (raw, raw_bins) = measure(&bright, &unnormalised);
    let off = must_fail(
        "patches not normalised",
        at_most(worst_term(terms(&raw), terms(&unit)), WITHIN),
    );
    eprintln!(
        "control, patches not normalised: a term moves {off:.4}°, a bin {:.4}°",
        moved(&raw_bins, &unit_bins)
    );
    assert!(off > 2.0 * WITHIN, "{off}");

    // The scene sampled twice, with the gains on the second sampling:
    // the geometry holds as well, and the log gains come back within 1 %
    // (their bias is the two samplings' aliasing, which no gain is).
    let again = |gains: [f64; 3]| Strips {
        lenses: [strip(scene()), gained(scene_again(), gains)],
    };
    let (unit, _) = measure(&again([1.0; 3]), &Method::PLAN);
    let (lit, _) = measure(&again(gains), &Method::PLAN);
    let geometry = worst_term(terms(&lit), terms(&unit));
    eprintln!(
        "the scene sampled again: the terms move {geometry:.2e}°; log gains {:?} with the \
         gains, {:?} without",
        lit.log_gain, unit.log_gain
    );
    assert!(
        at_most(geometry, WITHIN).passes() == Some(true),
        "{geometry}"
    );
    for (got, want) in lit.log_gain.iter().zip(want) {
        assert!((got - want).abs() < 0.01, "{:?}", lit.log_gain);
    }
}

/// A bin without texture is marked invalid, never given a zero shift.
#[test]
fn a_textureless_bin_is_invalid() {
    let _t = timed("a_textureless_bin_is_invalid");
    let (sx, sy) = scales();
    let columns = |bins: std::ops::RangeInclusive<usize>| {
        patch(&SPEC, *bins.start()).columns.start as usize
            ..patch(&SPEC, *bins.end()).columns.end as usize
    };
    // The scene, with bins 9 to 13 one flat grey and bins 29 to 33 a
    // ramp, which a gain and a bias fit at any shift.
    let (flat, ramp) = (columns(9..=13), columns(29..=33));
    let mut tall = Tall {
        width: scene().width,
        rgb: scene().rgb.clone(),
    };
    for (at, p) in tall.rgb.iter_mut().enumerate() {
        let (i, j) = (at % tall.width, at / tall.width);
        if flat.contains(&i) {
            *p = [0.2; 3];
        } else if ramp.contains(&i) {
            *p = [0.1 + 0.004 * (i - ramp.start) as f64 + 0.002 * j as f64; 3];
        }
    }
    let shift = (0.75, -1.25);
    let lens0 = strip(&tall);
    let mut lens1 = carried(&tall, uniform(shift.0, shift.1));
    // Lens 1 covers nothing of bins 49 to 53, and lens 0 is clipped over
    // bins 64 to 68.
    let (uncovered, clipped) = (columns(49..=53), columns(64..=68));
    for at in 0..lens1.valid.len() {
        lens1.valid[at] = !uncovered.contains(&(at % tall.width));
    }
    let mut lens0 = lens0;
    for (at, p) in lens0.rgb.iter_mut().enumerate() {
        if clipped.contains(&(at % tall.width)) {
            *p = [0.97, p[1], p[2]];
        }
    }
    let strips = Strips {
        lenses: [lens0, lens1],
    };

    // The middle three bins of each stretch have nothing to measure
    // from, whatever the search reaches.
    let empty: Vec<usize> = [10, 30, 50, 65].iter().flat_map(|&k| k..k + 3).collect();
    let nothing_claimed = |bins: &[BinShift]| empty.iter().all(|&k| !bins[k].valid);
    let bins = bin_shifts(&strips, &SPEC);
    assert!(nothing_claimed(&bins));
    for &k in &empty {
        let b = &bins[k];
        assert!(
            b.across_deg.is_nan() && b.along_deg.is_nan() && b.ncc.is_nan(),
            "{b:?}"
        );
        assert_eq!(b.info, [[0.0; 2]; 2]);
        assert!(b.log_gain.iter().all(|g| g.is_nan()), "{b:?}");
    }
    // Every bin that does measure has the planted shift, and the fit is
    // theirs alone. A bin at a stretch's edge measures from the sliver of
    // texture it has left, less closely.
    let measured: Vec<BinShift> = bins.iter().copied().filter(|b| b.valid).collect();
    let whole: Vec<BinShift> = (0..72)
        .filter(|k| !(8..=14).contains(k) && !(28..=34).contains(k))
        .filter(|k| !(48..=54).contains(k) && !(63..=69).contains(k))
        .map(|k| bins[k])
        .collect();
    let (miss, edge_miss) = (worst_miss(&whole, shift), worst_miss(&measured, shift));
    let s = signature(&bins);
    eprintln!(
        "{} bins measure: those beside no stretch within {miss:.4} px, those at a stretch's \
         edge within {edge_miss:.4} px; a0 {:+.4}, c0 {:+.4} px",
        measured.len(),
        -s.a0 * sy,
        s.c0 * sx
    );
    assert!(
        measured.len() >= 72 - 20 && measured.len() <= 72 - 12,
        "{}",
        measured.len()
    );
    assert!(at_most(miss, 0.02).passes() == Some(true), "{miss}");
    assert!(
        at_most(edge_miss, 0.05).passes() == Some(true),
        "{edge_miss}"
    );
    assert_eq!(s.valid_bins, measured.len());
    assert!(
        (s.c0 * sx - shift.0).hypot(-s.a0 * sy - shift.1) < 0.005,
        "{s:?}"
    );

    // Control: with the validity rule off, each such bin is passed off as
    // a shift of zero.
    let loose = Method {
        validity: false,
        ..Method::PLAN
    };
    let claimed = bin_shifts_by(&strips, &SPEC, &loose);
    must_fail("the validity rule off", nothing_claimed(&claimed));
    for &k in &empty {
        let b = &claimed[k];
        assert!(
            b.valid && b.across_deg == 0.0 && b.along_deg == 0.0,
            "{b:?}"
        );
    }
    // Strips that aren't the spec's measure nothing either.
    let short = Strips {
        lenses: [
            strip(scene()),
            Strip {
                height: 60,
                ..strip(scene())
            },
        ],
    };
    assert!(bin_shifts(&short, &SPEC).iter().all(|b| !b.valid));

    // A spec whose bins aren't a positive number of degrees is one bin,
    // and bins narrower than a strip's pixel are a bin a column: never
    // more bins than a strip has columns, which no machine could hold.
    let of = |bin_deg: f64| StripSpec { bin_deg, ..SPEC };
    for bin_deg in [0.0, -5.0, f64::NAN, f64::INFINITY, 1000.0] {
        assert_eq!(bin_count(&of(bin_deg)), 1, "{bin_deg}");
    }
    for bin_deg in [0.25, 1e-9, f64::MIN_POSITIVE, 5e-324] {
        assert_eq!(bin_count(&of(bin_deg)), SPEC.size().0 as usize, "{bin_deg}");
    }
    // Control: Tier 1's bins are 72, not one.
    must_fail("a bin of 5°", bin_count(&SPEC) == 1);
    assert_eq!(bin_count(&SPEC), 72);
    // And the one bin measures: the scene against itself has no shift.
    let lens = strip(scene());
    let same = Strips {
        lenses: [lens.clone(), lens],
    };
    let bins = bin_shifts(&same, &of(0.0));
    assert_eq!(bins.len(), 1);
    assert!(bins[0].valid && bins[0].phi_deg == 180.0, "{bins:?}");
    let miss = worst_miss(&bins, (0.0, 0.0));
    assert!(at_most(miss, 0.02).passes() == Some(true), "{miss}");
}

/// A grey that repeats nowhere a search reaches: waves of unrelated
/// lengths, each bent by another, at the strip's point (x, y). Along the
/// strip each is a whole number of waves a turn, so that the picture has
/// no edge where φ wraps. A bin of it has one match and no more.
fn grain(x: f64, y: f64) -> f64 {
    let turn = 2.0 * PI * x / f64::from(SPEC.size().0);
    0.3 + 0.05 * (71.0 * turn + 0.7 * (y / 2.3).sin()).sin()
        + 0.05 * (y / 2.7 + 1.3 * (28.0 * turn).sin()).sin()
        + 0.04 * (41.0 * turn + y / 3.1).sin()
        + 0.04 * (23.0 * turn - y / 4.6 + (7.0 * turn).sin()).cos()
        + 0.03 * (17.0 * turn).sin() * (y / 4.1).cos()
}

/// A step in the shift from one bin to the next, as a wall beside things
/// further off leaves, gives no false shift where samples are left out.
/// A bin with one match that its search tried every shift against keeps
/// it, whatever shift its neighbour hands it. A bin that can't tell
/// starts from the handed shift, and measures nothing unless its steps
/// settle at a match of its own.
#[test]
fn a_step_in_the_shift_gives_no_false_shift() {
    let _t = timed("a_step_in_the_shift_gives_no_false_shift");
    // Away from the steps, as a pure shift comes back; beside one, a bin
    // reads a few columns of the other side through the smoothing.
    const WITHIN: f64 = 0.02;
    const BESIDE: f64 = 0.5;
    // From φ = 180° on, lens 1 shows the seam 0.9° further across: more
    // than the 0.5° a bin takes from its neighbour on trust, and 3.4 px,
    // more than three steps of a pixel cross.
    const STEP_DEG: f64 = 0.9;
    let (sx, sy) = scales();
    let field = |phi_deg: f64| (if phi_deg >= 180.0 { STEP_DEG } else { 0.0 }, 0.0);
    let past = 36..72;
    let beside = [35, 36, 71, 0];
    let tall = tall_of(grain);
    let whole = Strips {
        lenses: [strip(&tall), carried(&tall, field)],
    };
    let width = whole.lenses[0].width as usize;

    // A bin's miss, in strip pixels and NaN of an invalid bin; then the
    // worst away from the steps and the worst beside one, of every bin
    // or of those that measure.
    let miss = |b: &BinShift| {
        let (across, along) = field(b.phi_deg);
        ((b.along_deg - along) * sx).hypot((b.across_deg - across) * sy)
    };
    let worst_of = |bins: &[BinShift], measured_only: bool| {
        let of = |beside_a_step: bool| {
            bins.iter()
                .enumerate()
                .filter(|(k, b)| beside.contains(k) == beside_a_step && (b.valid || !measured_only))
                .map(|(_, b)| miss(b))
                .fold(0.0, worse)
        };
        (of(false), of(true))
    };
    let worst = |bins: &[BinShift]| worst_of(bins, false);
    let every_bin_measures = |bins: &[BinShift]| bins.iter().all(|b| b.valid);
    let unmeasured = |bins: &[BinShift]| past.clone().filter(|&k| !bins[k].valid).count();

    // With every sample there, each bin has its own side's shift.
    let bins = bin_shifts(&whole, &SPEC);
    let (away, by) = worst(&bins);
    eprintln!(
        "a step of {STEP_DEG}° at 180° and at 0°: every bin within {away:.4} px, and {by:.4} px \
         beside a step"
    );
    assert!(every_bin_measures(&bins));
    assert!(at_most(away, WITHIN).passes() == Some(true), "{away}");
    assert!(at_most(by, BESIDE).passes() == Some(true), "{by}");

    // One sample left out at the middle of each patch past the step, of
    // lens 0's strip in the first half of those bins and of lens 1's in
    // the rest: each bin still has its one match, and keeps it.
    let mut holed = whole.clone();
    for k in past.clone() {
        let p = patch(&SPEC, k);
        let row = ((p.rows.start + p.rows.end) / 2) as usize;
        let column = ((p.columns.start + p.columns.end) / 2) as usize;
        holed.lenses[usize::from(k >= 54)].valid[row * width + column] = false;
    }
    let bins = bin_shifts(&holed, &SPEC);
    let (away, by) = worst(&bins);
    eprintln!(
        "a sample left out of each bin past the step: every bin within {away:.4} px, and \
         {by:.4} px beside a step"
    );
    assert!(every_bin_measures(&bins));
    assert!(at_most(away, WITHIN).passes() == Some(true), "{away}");
    assert!(at_most(by, BESIDE).passes() == Some(true), "{by}");

    // Control: lens 1 covering nothing below row 40 past the step, the
    // search can't try the shifts that reach there, so a bin's one match
    // may be only the one it could try: it isn't kept against the shift
    // handed over the step, and the bins there don't measure.
    const CUT_ROW: usize = 40;
    let uncovered = patch(&SPEC, past.start).columns.start as usize;
    let mut cut = whole.clone();
    for (at, valid) in cut.lenses[1].valid.iter_mut().enumerate() {
        *valid = at % width < uncovered || at / width < CUT_ROW;
    }
    let bins = bin_shifts(&cut, &SPEC);
    must_fail("the search cut short", every_bin_measures(&bins));

    // They start from the handed shift instead, and three steps of a
    // pixel are still moving when they end: each bin is invalid, never a
    // shift on the way. The bins before the step measure as they did.
    let false_shift = |bins: &[BinShift]| at_most(worst_of(bins, true).0, WITHIN);
    let (away, by) = worst_of(&bins, true);
    eprintln!(
        "the search cut short past the step: {} of {} bins there invalid; the bins that \
         measure within {away:.4} px, and {by:.4} px beside a step",
        unmeasured(&bins),
        past.len()
    );
    assert_eq!(unmeasured(&bins), past.len());
    assert_eq!(bins.iter().filter(|b| b.valid).count(), 72 - past.len());
    assert!(
        false_shift(&bins).passes() == Some(true),
        "{}",
        false_shift(&bins)
    );
    assert!(at_most(by, BESIDE).passes() == Some(true), "{by}");

    // Control: with the validity rule off, wherever the steps stopped is
    // passed off as each such bin's shift: 3 px on the way and 0.4 px
    // short, with an NCC that a match might have. Only the last step's
    // length says the shift isn't there yet.
    let loose = Method {
        validity: false,
        ..Method::PLAN
    };
    let claimed = bin_shifts_by(&cut, &SPEC, &loose);
    let off = must_fail("the validity rule off", false_shift(&claimed));
    assert_eq!(unmeasured(&claimed), 0);
    let inner = || past.clone().filter(|k| !beside.contains(k));
    for b in inner().map(|k| &claimed[k]) {
        assert!(miss(b) > 10.0 * WITHIN && miss(b) < 1.0, "{b:?}");
        assert!(b.ncc > 0.97, "{b:?}");
    }
    eprintln!(
        "control, the validity rule off: the bins past the step {:.4} to {off:.4} px from their \
         shift, with an NCC of {:.4} or more",
        inner().map(|k| miss(&claimed[k])).fold(off, f64::min),
        inner().map(|k| claimed[k].ncc).fold(1.0, f64::min)
    );

    // With no steps there is nothing to settle, and the handed shift
    // would stand as it is. But a bin's NCC there is far under its
    // match's, and so the bin is as invalid.
    let coarse = Method {
        lk_steps: 0,
        ..Method::PLAN
    };
    let bins = bin_shifts_by(&cut, &SPEC, &coarse);
    assert_eq!(unmeasured(&bins), past.len());
    assert_eq!(bins.iter().filter(|b| b.valid).count(), 72 - past.len());

    // Control: with the validity rule off as well, each such bin is
    // passed off as having its neighbour's shift, the whole step from
    // its own.
    let loose = Method {
        validity: false,
        ..coarse
    };
    let claimed = bin_shifts_by(&cut, &SPEC, &loose);
    let off = must_fail("no steps, and the validity rule off", false_shift(&claimed));
    assert_eq!(unmeasured(&claimed), 0);
    for b in inner().map(|k| &claimed[k]) {
        assert!((miss(b) - STEP_DEG * sy).abs() < BESIDE, "{b:?}");
        assert!(b.ncc < 0.9, "{b:?}");
    }
    eprintln!(
        "control, no steps and the validity rule off: the bins past the step {:.4} to {off:.4} \
         px from their shift, for a step of {:.4} px, with an NCC of {:.4} or less",
        inner().map(|k| miss(&claimed[k])).fold(off, f64::min),
        STEP_DEG * sy,
        inner().map(|k| claimed[k].ncc).fold(-1.0, f64::max)
    );
}

/// Texture along a single line gives information across the line and
/// none along it: the aperture problem, kept out of the fit by each
/// bin's normal matrix.
#[test]
fn a_line_informs_across_it_only() {
    let _t = timed("a_line_informs_across_it_only");
    let (sx, sy) = scales();
    let (width, height) = SPEC.size();
    // One line through the middle of each bin, 75° from the seam and
    // leaning the other way in the next bin, 5 px wide and nothing
    // beyond, so that no patch holds anything of a second line.
    const LEAN: f64 = 75.0;
    const HALF_WIDTH: f64 = 2.5;
    let centre = |k: usize| {
        let columns = patch(&SPEC, k % 72).columns;
        f64::from(columns.start + columns.end) / 2.0
    };
    let lean = |k: usize| {
        if k.is_multiple_of(2) {
            LEAN
        } else {
            180.0 - LEAN
        }
    };
    let lines = tall_of(|x, y| {
        let bin = ((x / f64::from(width) * 72.0) as usize).min(71);
        let mut value = 0.2;
        for k in [bin + 71, bin, bin + 1] {
            // The nearest turn of this bin's centre.
            let dx = (x - centre(k) + 0.5 * f64::from(width)).rem_euclid(f64::from(width))
                - 0.5 * f64::from(width);
            let (sin, cos) = lean(k).to_radians().sin_cos();
            let off = dx * sin - (y - 0.5 * f64::from(height)) * cos;
            if off.abs() < HALF_WIDTH {
                value += 0.25 * (1.0 + (PI * off / HALF_WIDTH).cos());
            }
        }
        value
    });
    // A uniform shift each way and a tilt.
    let mut planted = [0.0; TERMS];
    (planted[0], planted[1], planted[5]) = (0.30, 0.25, -0.20);
    let strips = Strips {
        lenses: [strip(&lines), carried(&lines, field_of(planted))],
    };
    let bins = bin_shifts(&strips, &SPEC);
    assert!(bins.iter().all(|b| b.valid));

    // Each bin's information is its line's: none along the line, and the
    // shift it measured is the planted one across the line, whatever it
    // is along it.
    let (mut along_line, mut across_line, mut slack) = (0.0f64, 0.0f64, 0.0f64);
    for (k, b) in bins.iter().enumerate() {
        let (sin, cos) = lean(k).to_radians().sin_cos();
        // A strip pixel along the line, in degrees (across, along).
        let line = [-sin / sy, cos / sx];
        let quad = |v: [f64; 2]| {
            v[0] * (b.info[0][0] * v[0] + b.info[0][1] * v[1])
                + v[1] * (b.info[1][0] * v[0] + b.info[1][1] * v[1])
        };
        let normal = [cos / sy, sin / sx];
        slack = worse(slack, quad(line) / quad(normal));
        let (across, along) = field_of(planted)(b.phi_deg);
        let miss = [(b.along_deg - along) * sx, -(b.across_deg - across) * sy];
        along_line = worse(along_line, (miss[0] * cos + miss[1] * sin).abs());
        across_line = worse(across_line, (miss[0] * sin - miss[1] * cos).abs());
    }
    eprintln!(
        "a line a bin: the information along the line is {slack:.2e} of that across it; the \
         shifts are within {across_line:.4} px across their lines and up to {along_line:.3} px \
         off along them"
    );
    assert!(slack < 1e-3, "{slack}");
    assert!(
        at_most(across_line, 0.02).passes() == Some(true),
        "{across_line}"
    );
    assert!(along_line > 0.5, "{along_line}");

    // The fit, weighing each bin by its information, has the field.
    let held = |bins: &[BinShift]| at_most(worst_term(terms(&signature(bins)), planted), 0.01);
    let s = signature(&bins);
    eprintln!(
        "the fit: a0 {:+.4}°, a1 {:+.4}°, c0 {:+.4}°, the terms within {:.4}°",
        s.a0,
        s.a1,
        s.c0,
        worst_term(terms(&s), planted)
    );
    assert!(held(&bins).passes() == Some(true), "{}", held(&bins));

    // Control: with the information dropped, each bin as sure along its
    // line as across it, the fit takes the bins' shifts along their
    // lines for shifts of the seam.
    let unweighted: Vec<BinShift> = bins
        .iter()
        .map(|b| BinShift {
            info: [[1.0, 0.0], [0.0, 1.0]],
            ..*b
        })
        .collect();
    let off = must_fail("the information dropped", held(&unweighted));
    eprintln!("control, the information dropped: a term {off:.4}° off");
    assert!(off > 0.1, "{off}");
}

/// Bins of a field of known terms: all of `phis` valid, each with the
/// field's shift at its φ and `info`.
fn bins_of(
    t: [f64; TERMS],
    phis: impl Iterator<Item = f64>,
    info: impl Fn(f64) -> [[f64; 2]; 2],
) -> Vec<BinShift> {
    phis.map(|phi_deg| {
        let (across_deg, along_deg) = field_of(t)(phi_deg);
        BinShift {
            phi_deg,
            across_deg,
            along_deg,
            info: info(phi_deg),
            ncc: 0.9,
            log_gain: [0.1, 0.2, 0.3],
            valid: true,
        }
    })
    .collect()
}

/// The centres of 72 bins of 5°.
fn ring() -> impl Iterator<Item = f64> {
    (0..72).map(|k| 2.5 + 5.0 * f64::from(k))
}

/// The information of a line `lean_deg` from the seam, of weight
/// `weight`: all of it across the line.
fn line_info(lean_deg: f64, weight: f64) -> [[f64; 2]; 2] {
    // Across the line, in (across, along).
    let (sin, cos) = lean_deg.to_radians().sin_cos();
    let n = [cos, sin];
    [
        [weight * n[0] * n[0], weight * n[0] * n[1]],
        [weight * n[0] * n[1], weight * n[1] * n[1]],
    ]
}

const IDENTITY: [[f64; 2]; 2] = [[1.0, 0.0], [0.0, 1.0]];

/// An invalid bin, as `bin_shifts` gives one.
fn invalid(phi_deg: f64) -> BinShift {
    BinShift {
        phi_deg,
        across_deg: f64::NAN,
        along_deg: f64::NAN,
        info: [[0.0; 2]; 2],
        ncc: f64::NAN,
        log_gain: [f64::NAN; 3],
        valid: false,
    }
}

/// `signature` gives back each term of a field of bins made from known
/// terms.
#[test]
fn signature_recovers_each_term() {
    let _t = timed("signature_recovers_each_term");
    const EXACT: f64 = 1e-9;
    let planted = [0.31, -0.22, 0.17, 0.13, -0.09, -0.41, 0.19, 0.27];

    // Each term alone, then all together.
    for only in 0..TERMS {
        let mut one = [0.0; TERMS];
        one[only] = planted[only];
        let s = signature(&bins_of(one, ring(), |_| IDENTITY));
        assert!(worst_term(terms(&s), one) < EXACT, "{}: {s:?}", NAMES[only]);
        assert!(s.rest_rms_deg < EXACT, "{s:?}");
    }
    let bins = bins_of(planted, ring(), |_| IDENTITY);
    let s = signature(&bins);
    assert!(worst_term(terms(&s), planted) < EXACT, "{s:?}");
    assert_eq!(s.valid_bins, 72);
    assert_eq!((s.median_ncc, s.log_gain), (0.9, [0.1, 0.2, 0.3]));

    // What the terms can't hold is left: a third harmonic across, of
    // 0.2°, which over the whole ring leans on no term, is an rms of
    // 0.2°/√2.
    let mut waved = bins.clone();
    for b in &mut waved {
        b.across_deg += 0.2 * (3.0 * b.phi_deg).to_radians().cos();
    }
    let s = signature(&waved);
    assert!(worst_term(terms(&s), planted) < EXACT, "{s:?}");
    assert!((s.rest_rms_deg - 0.2 / 2f64.sqrt()).abs() < EXACT, "{s:?}");

    // A line a bin, leaning by turns, each bin's shift off by anything
    // along its line and a third of the bins invalid: the same terms.
    let lean = |phi: f64| 25.0 + 37.0 * (phi / 5.0).floor();
    let mut lined = bins_of(planted, ring(), |phi| {
        line_info(lean(phi), 3.0 + phi / 90.0)
    });
    for (k, b) in lined.iter_mut().enumerate() {
        let (sin, cos) = lean(b.phi_deg).to_radians().sin_cos();
        let slide = 0.7 * (k as f64).sin();
        b.across_deg -= slide * sin;
        b.along_deg += slide * cos;
        if k % 3 == 1 {
            *b = invalid(b.phi_deg);
        }
    }
    let s = signature(&lined);
    assert!(worst_term(terms(&s), planted) < EXACT, "{s:?}");
    // What is left is the rounding of each slide against its bin's
    // information, which has nothing along the line but to 1 in 10¹⁶.
    assert!(s.rest_rms_deg < 1e-6, "{s:?}");
    assert_eq!(s.valid_bins, 48);

    // Control: with a2 and b2 left out of the fit, a planted a2 lands on
    // the other terms, wherever the bins don't cover the ring: here a
    // quarter of it is missing, as a clipped sky is.
    let gap = |phi: &f64| !(45.0..135.0).contains(phi);
    let partial = bins_of(planted, ring().filter(gap), |_| IDENTITY);
    assert_eq!(partial.len(), 54);
    let s = signature(&partial);
    assert!(worst_term(terms(&s), planted) < EXACT, "{s:?}");
    let mut first_only = [true; TERMS];
    (first_only[3], first_only[4]) = (false, false);
    let short = signature_of(&partial, first_only);
    assert_eq!((short.a2, short.b2), (0.0, 0.0));
    let others = |s: &Signature| {
        let (got, want) = (terms(s), planted);
        [0, 1, 2, 5, 6, 7]
            .iter()
            .map(|&i| (got[i] - want[i]).abs())
            .fold(0.0, worse)
    };
    let off = must_fail("the fit without a2 and b2", at_most(others(&short), EXACT));
    eprintln!(
        "control, the fit without a2 and b2: a0 {:+.4}° for {:+.4}°, a1 {:+.4}° for {:+.4}°, b1 \
         {:+.4}° for {:+.4}°; rest {:.4}°",
        short.a0, planted[0], short.a1, planted[1], short.b1, planted[2], short.rest_rms_deg
    );
    assert!(off > 0.05, "{off}");
    // The along terms don't lean on the across ones, and stay.
    assert!((short.c0 - planted[5]).abs() < EXACT, "{short:?}");

    // A term the bins don't determine is NaN, never a zero, and so is the
    // miss these checks hold, whichever term it is and however far off
    // the terms before it are.
    for undetermined in 0..TERMS {
        let mut got = planted.map(|t| t + 5.0);
        got[undetermined] = f64::NAN;
        assert!(worst_term(got, planted).is_nan(), "{}", NAMES[undetermined]);
    }
    let none = signature(&[invalid(2.5), invalid(7.5)]);
    assert!(terms(&none).iter().all(|t| t.is_nan()), "{none:?}");
    assert!(
        none.rest_rms_deg.is_nan() && none.median_ncc.is_nan(),
        "{none:?}"
    );
    assert_eq!(none.valid_bins, 0);
    // Lines along the seam say nothing of a shift along it.
    let across_only = signature(&bins_of(planted, ring(), |_| line_info(0.0, 1.0)));
    let got = terms(&across_only);
    assert!(
        (0..5).all(|i| (got[i] - planted[i]).abs() < EXACT),
        "{across_only:?}"
    );
    assert!(got[5..].iter().all(|t| t.is_nan()), "{across_only:?}");
    // Two bins hold a uniform shift and one harmonic's worth, no more.
    let two = signature(&bins_of(planted, [10.0, 100.0].into_iter(), |_| IDENTITY));
    assert!(terms(&two).iter().any(|t| t.is_nan()), "{two:?}");
    assert_eq!(two.valid_bins, 2);
}

/// How many cases a property runs: `PROPTEST_CASES`, or 256.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases(),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    /// Whatever the terms, each bin's information (a line's, or more) and
    /// whichever bins are valid, the fit gives the terms back. M3 plan
    /// 19.3 names no property for M3.4; this one runs with the others at
    /// `PROPTEST_CASES`.
    #[test]
    fn signature_recovers_any_field(
        planted in proptest::array::uniform8(-3.0..3.0f64),
        infos in proptest::collection::vec((0.0..180.0f64, 0.5..50.0f64, 0.0..50.0f64), 72),
        slides in proptest::collection::vec(-3.0..3.0f64, 72),
        dropped in proptest::collection::vec(any::<bool>(), 72),
    ) {
        let mut bins = bins_of(planted, ring(), |_| IDENTITY);
        for (k, b) in bins.iter_mut().enumerate() {
            // A line's information, and as often some along the line too:
            // then the shift is held both ways and slides nowhere.
            let (lean, across, along) = infos[k];
            let lone = k.is_multiple_of(2);
            let line = line_info(lean, across);
            let other = line_info(lean + 90.0, if lone { 0.0 } else { along });
            b.info = [
                [line[0][0] + other[0][0], line[0][1] + other[0][1]],
                [line[1][0] + other[1][0], line[1][1] + other[1][1]],
            ];
            if lone {
                let (sin, cos) = lean.to_radians().sin_cos();
                b.across_deg -= slides[k] * sin;
                b.along_deg += slides[k] * cos;
            }
            // Two bins in three stay, so that the ring is covered.
            if dropped[k] && k.is_multiple_of(3) {
                *b = invalid(b.phi_deg);
            }
        }
        let s = signature(&bins);
        prop_assert!(worst_term(terms(&s), planted) < 1e-6, "{s:?}");
        prop_assert!(s.rest_rms_deg < 1e-6, "{s:?}");
        prop_assert_eq!(s.valid_bins, bins.iter().filter(|b| b.valid).count());
    }
}

// The predicted fields.

/// 3.4's scales at 768²: source pixels a degree, across the seam and
/// along it.
const ACROSS_PX: f64 = 3.81;
const ALONG_PX: f64 = 6.0;

/// What a principal point a pixel off at 768² moves the seam by, in
/// degrees across and along: half of 3.4's row for cx + 2 px.
const CENTRE_ACROSS_DEG: f64 = 0.52 / 2.0;
const CENTRE_ALONG_DEG: f64 = 0.33 / 2.0;

/// The terms' places in [`terms`].
const A0: usize = 0;
const A1: usize = 1;
const B1: usize = 2;
const C0: usize = 5;
const C1: usize = 6;
const D1: usize = 7;

/// How far a predicted figure may lie from 3.4's, in pixels at 768²: 15 %
/// of the table's figure, or 0.2 px where that is more. The plan gives
/// its table no tolerance; this one is the slice's.
fn slack_px(table_px: f64) -> f64 {
    (0.15 * table_px.abs()).max(0.2)
}

/// 3.4's rows as the test holds them: each prints as it is held, and the
/// ones that miss wait for the test's end, so that a run reports every
/// row.
#[derive(Default)]
struct Budget {
    missed: Vec<String>,
}

impl Budget {
    fn report(&mut self, line: String, fits: bool) {
        eprintln!("3.4 | {line}{}", if fits { "" } else { ": MISSES" });
        if !fits {
            self.missed.push(line);
        }
    }

    /// Holds a predicted figure to the table's, both in pixels at 768².
    fn hold(&mut self, row: &str, got_px: f64, table_px: f64) {
        let fits = (got_px - table_px).abs() <= slack_px(table_px);
        self.report(
            format!("{row}: {got_px:.2} px predicted, {table_px:.2} px in the table"),
            fits,
        );
    }

    /// Holds a predicted figure to `worked_px`, 3.4's arithmetic worked
    /// again, where the table's own figure is wrong: the prediction fits
    /// the one and misses the other.
    fn hold_corrected(&mut self, row: &str, got_px: f64, worked_px: f64, table_px: f64) {
        let fits = (got_px - worked_px).abs() <= slack_px(worked_px);
        let table_fits = (got_px - table_px).abs() <= slack_px(table_px);
        self.report(
            format!(
                "{row}: {got_px:.2} px predicted, {worked_px:.2} px worked again, {table_px:.2} \
                 px in the table, which is wrong"
            ),
            fits && !table_fits,
        );
    }

    /// Holds a predicted figure to one 3.4's arithmetic gives and its
    /// table doesn't print.
    fn hold_worked(&mut self, row: &str, got_px: f64, worked_px: f64) {
        let fits = (got_px - worked_px).abs() <= slack_px(worked_px);
        self.report(
            format!("{row}: {got_px:.2} px predicted, {worked_px:.2} px worked, not in the table"),
            fits,
        );
    }

    /// Holds a field to the terms its row names, by 8.3's rule for the
    /// measured ones: every other term under 0.03°, or 15 % of the
    /// largest named.
    fn terms(&mut self, row: &str, s: &Signature, named: &[usize]) {
        let t = terms(s);
        let size = |i: &usize| t[*i].abs();
        let main = named.iter().map(size).fold(0.0, worse);
        let other = (0..TERMS)
            .filter(|i| !named.contains(i))
            .max_by(|a, b| size(a).total_cmp(&size(b)))
            .expect("a term the row doesn't name");
        let names: Vec<&str> = named.iter().map(|&i| NAMES[i]).collect();
        self.report(
            format!(
                "{row}: on {} ({main:.4}°), the largest other term {} ({:.4}°)",
                names.join(", "),
                NAMES[other],
                t[other].abs()
            ),
            main.is_finite() && t[other].abs() <= (0.15 * main).max(0.03),
        );
    }
}

/// How far a field's bins move, in pixels at 768² by 3.4's scales, over
/// its valid bins.
struct Sizes {
    /// The least and the most a bin moves.
    least: f64,
    most: f64,
    /// The most a bin moves along the seam.
    along: f64,
}

fn sizes(bins: &[BinShift]) -> Sizes {
    let mut sizes = Sizes {
        least: f64::INFINITY,
        most: 0.0,
        along: 0.0,
    };
    for b in bins.iter().filter(|b| b.valid) {
        let (across, along) = (b.across_deg * ACROSS_PX, b.along_deg * ALONG_PX);
        // `min` and `max` would pass a NaN over.
        assert!(across.is_finite() && along.is_finite(), "{b:?}");
        sizes.least = sizes.least.min(across.hypot(along));
        sizes.most = sizes.most.max(across.hypot(along));
        sizes.along = sizes.along.max(along.abs());
    }
    sizes
}

/// A field's first harmonic across the seam, and along it, in degrees.
fn harmonics(s: &Signature) -> (f64, f64) {
    (s.a1.hypot(s.b1), s.c1.hypot(s.d1))
}

/// The radius of the seam's circle in a lens's picture, in its slot's
/// pixels: r(90°) from where the lens's axis shows, averaged round the
/// lens.
fn seam_radius(lens: &RigLens) -> f64 {
    let at = |d: DVec3| lens.model.project(d).expect("a direction the lens shows");
    let axis = at(DVec3::Z);
    let round = (0..360).map(|deg| {
        let (sin, cos) = f64::from(deg).to_radians().sin_cos();
        (at(DVec3::new(cos, sin, 0.0)) - axis).length()
    });
    round.sum::<f64>() / 360.0
}

/// How far apart a calibration's two principal points lie, each in its
/// own square, in pixels at 768² under map C: block 0's less block 1's.
fn centres_apart(cal: &CalibTruth) -> [f64; 2] {
    let src = f64::from(cal.canvas[1]);
    let to_slot = f64::from(X5_LIKE_SIDE) / src;
    let [first, second] = &cal.blocks;
    [
        (first.cx - (second.cx - src)) * to_slot,
        (first.cy - second.cy) * to_slot,
    ]
}

/// `direct` with each track's lens taking, through `part`, what `reversed`
/// has on that track: a part of what reversing the binding swaps.
fn swapped(direct: &Rig, reversed: &Rig, part: impl Fn(&mut RigLens, &RigLens)) -> Rig {
    let mut lenses = direct.lenses;
    for lens in &mut lenses {
        let there = reversed.lenses.iter().find(|l| l.track == lens.track);
        part(lens, there.expect("a lens on each track"));
    }
    Rig { lenses, ..*direct }
}

/// `predict_bins`' fields for the differences 3.4's error budget plants
/// have the terms and the sizes 3.4 works out, each within 15 % or 0.2 px
/// at 768² (the plan gives no tolerance; this is the slice's). No images:
/// the attribution matrix holds the images to these fields.
///
/// Three of the table's figures don't hold, and the test holds the
/// predictions to 3.4's arithmetic worked again:
///
/// - **V3 against V6, lens 0's a0.** The table's 0.15° is fx's −0.17 %
///   of r(90°) alone. V6's radial terms give most of it back: the two
///   models' r(90°), averaged round the lens (`seam_radius`), differ by
///   0.14 px at 768², 0.036°.
/// - **The binding reversed.** The table puts the swapped centres on
///   c1/d1 only. By its own cx row a centre a pixel off at 768² is 0.26°
///   across as well as 0.165° along, so the swap's a1/b1 is twice the
///   lenses' angle difference along the turn axis plus twice 0.26° a
///   pixel of the centres' difference across it. On Pat's V6 at the
///   placeholder that is −0.86° + 0.99° = 0.13°. In Tier 1's rig, whose
///   centres lie 5.1 px apart, it is 1.10° + 2.65° = 3.75°, 14 px and not
///   "up to 9 px", and part of the seam is past the ±3° search.
/// - **The Euler class.** The table's 0.3° to 0.6° is one lens's: a wrong
///   sign on a stored angle α turns the lens 2α. With both lenses read
///   wrong, the pair's tilt is twice the yaws' difference (a yaw turns
///   about T_1's axis, so the two errors subtract) or twice the pitches'
///   sum: 1.1° and 0.3° in Tier 1's rig.
#[test]
fn predicted_fields_match_the_error_budget() {
    let _t = timed("predicted_fields_match_the_error_budget");
    let sides = [X5_LIKE_SIDE; 2];
    let far = Tune {
        q_per_m: 0.0,
        ..Tune::default()
    };
    let rig_of = |cal: &CalibTruth, planted: &Planted, tune: &Tune| {
        sphere_rig(cal, planted, tune, 0, sides).expect("a rig")
    };
    let predict = |truth: &RigTruth, rig: &Rig| predict_bins(truth, 0, rig, &SPEC);
    let mut budget = Budget::default();

    // Tier 1's rig, and the model that is its truth, which leaves no
    // shift.
    let cal = x5_like();
    let planted = x5_like_planted();
    let truth = compose(&cal, &planted, 0, sides);
    let model = rig_of(&cal, &planted, &far);
    let same = predict(&truth, &model);
    assert_eq!(same.len(), bin_count(&SPEC));
    assert!(same.iter().all(|b| b.valid) && sizes(&same).most < 1e-6);

    // Q4: map A's focal is 5376/5312 of C's, on both lenses and on one.
    let under_a = rig_of(
        &cal,
        &Planted {
            canvas: TruthCanvas::A,
            ..planted
        },
        &far,
    );
    let q4 = |s: &Signature| s.a0.abs() * ACROSS_PX;
    let s = signature(&predict(&truth, &under_a));
    budget.hold("Q4, C against A, a0", q4(&s), 8.3);
    budget.terms("Q4, C against A", &s, &[A0]);
    let one = Rig {
        lenses: [under_a.lenses[0], model.lenses[1]],
        ..model
    };
    let s = signature(&predict(&truth, &one));
    budget.hold("Q4, one lens under A, a0", q4(&s), 1.09 * ACROSS_PX);

    // Control: with the model's own inverse for the truth's, each strip
    // shows the direction it asked for, and a model under A predicts no
    // shift at all.
    let own = predict_through(&under_a, &SPEC, |b, px| under_a.unproject(b, px));
    let zero = must_fail(
        "the model's own inverse for the truth's",
        at_least(q4(&signature(&own)), 8.3 - slack_px(8.3)),
    );
    eprintln!("control, the model's own inverse for the truth's: a0 {zero:.2e} px for 8.3 px");
    assert!(own.iter().all(|b| b.valid) && sizes(&own).most < 1e-6);

    // A relative roll, a relative tilt, and lens 1's centre 2 px off: the
    // tune's shift is in canvas pixels, 5376/768 to a pixel of the track.
    let tuned = |tune: &dyn Fn(&mut Tune)| {
        let mut t = far;
        tune(&mut t);
        rig_of(&cal, &planted, &t)
    };
    let s = signature(&predict(&truth, &tuned(&|t| t.lenses[1].ypr_deg[2] = 0.5)));
    budget.hold("relative roll 0.5°, c0", s.c0.abs() * ALONG_PX, 3.0);
    budget.terms("relative roll 0.5°", &s, &[C0]);
    for (angle, term, about) in [(0, A1, "a yaw"), (1, B1, "a pitch")] {
        let s = signature(&predict(
            &truth,
            &tuned(&|t| t.lenses[1].ypr_deg[angle] = 0.5),
        ));
        let row = format!("relative tilt 0.5°, {about}");
        budget.hold(
            &format!("{row}, {}", NAMES[term]),
            terms(&s)[term].abs() * ACROSS_PX,
            1.9,
        );
        budget.terms(&row, &s, &[term]);
    }
    let bins = predict(&truth, &tuned(&|t| t.lenses[1].centre_px = [14.0, 0.0]));
    let s = signature(&bins);
    let (across, along) = harmonics(&s);
    budget.hold(
        "cx + 2 px on lens 1, a1/b1",
        across * ACROSS_PX,
        0.52 * ACROSS_PX,
    );
    budget.hold(
        "cx + 2 px on lens 1, c1/d1",
        along * ALONG_PX,
        0.33 * ALONG_PX,
    );
    budget.hold(
        "cx + 2 px on lens 1, the least a bin moves",
        sizes(&bins).least,
        2.0,
    );
    budget.hold(
        "cx + 2 px on lens 1, the most a bin moves",
        sizes(&bins).most,
        2.0,
    );
    budget.terms("cx + 2 px on lens 1", &s, &[A1, B1, C1, D1]);
    // Along is a quarter turn round the seam from across.
    let in_phase = (s.a1 * s.c1 + s.b1 * s.d1).abs() / (across * along);
    assert!(in_phase < 0.02, "{in_phase}: {s:?}");

    // The stitch distance, and the sign of tz.
    let scene_at = |radius_m: f64| RigTruth {
        scene_radius_m: Some(radius_m),
        ..truth.clone()
    };
    let s = signature(&predict(&scene_at(1.5), &model));
    budget.hold("∞ against 1.5 m, a0", s.a0.abs() * ACROSS_PX, 4.6);
    budget.terms("∞ against 1.5 m", &s, &[A0]);
    let s = signature(&predict(&scene_at(10.0), &model));
    budget.hold(
        "∞ against 10 m, a0",
        s.a0.abs() * ACROSS_PX,
        0.18 * ACROSS_PX,
    );
    let near = Tune {
        q_per_m: 1.0 / 1.5,
        ..far
    };
    let fitted = predict(&scene_at(1.5), &rig_of(&cal, &planted, &near));
    assert!(sizes(&fitted).most < 1e-6, "{:?}", signature(&fitted));
    let flipped = Planted {
        tz: TruthTz::Negated,
        ..planted
    };
    let s = signature(&predict(&scene_at(1.5), &rig_of(&cal, &flipped, &near)));
    budget.hold("tz flipped at 1.5 m, a0", s.a0.abs() * ACROSS_PX, 9.2);
    budget.terms("tz flipped at 1.5 m", &s, &[A0]);

    // Lens 1 a frame late while the rig turns 10° a second: a rigid turn
    // of 0.33° about the turn's axis.
    for (axis, term, px, table, turn) in [
        (DVec3::Y, A1, ACROSS_PX, 1.3, "a pan about +Y"),
        (DVec3::X, B1, ACROSS_PX, 1.3, "a tilt about +X"),
        (DVec3::Z, C0, ALONG_PX, 2.0, "a turn about the lens axis"),
    ] {
        let late = RigTruth {
            late_frames: [0.0, 1.0],
            motion: Motion {
                turns: vec![Turn {
                    start_s: 0.0,
                    end_s: 100.0,
                    axis,
                    deg_per_s: 10.0,
                }],
                ..Motion::still()
            },
            ..truth.clone()
        };
        let s = signature(&predict_bins(&late, 5, &model, &SPEC));
        let row = format!("lens 1 a frame late in {turn}");
        budget.hold(
            &format!("{row}, {}", NAMES[term]),
            terms(&s)[term].abs() * px,
            table,
        );
        budget.terms(&row, &s, &[term]);
    }

    // The Euler class, by a wrong sign on a stored angle: on lens 0
    // alone, whose yaw is 0.3°, and on both lenses.
    let ypr = [cal.blocks[0].ypr_deg, cal.blocks[1].ypr_deg];
    let wrong_sign = |angle: usize| {
        let mut signs = [Sign::Plus; 3];
        signs[angle] = Sign::Minus;
        let euler = TruthEuler {
            signs,
            ..TruthEuler::PLACEHOLDER
        };
        rig_of(&cal, &Planted { euler, ..planted }, &far)
    };
    let yaw_wrong = wrong_sign(0);
    let one = Rig {
        lenses: [yaw_wrong.lenses[0], model.lenses[1]],
        ..model
    };
    let s = signature(&predict(&truth, &one));
    budget.hold(
        "the Euler class, lens 0's yaw of 0.3° with the wrong sign, a1",
        s.a1.abs() * ACROSS_PX,
        2.3,
    );
    budget.terms(
        "the Euler class, lens 0's yaw with the wrong sign",
        &s,
        &[A1],
    );
    let s = signature(&predict(&truth, &wrong_sign(1)));
    let worked = 2.0 * (ypr[0][1] + ypr[1][1]).abs();
    assert!((worked - 0.3).abs() < 1e-9);
    budget.hold(
        "the Euler class, both pitches with the wrong sign, b1",
        s.b1.abs() * ACROSS_PX,
        1.2,
    );
    budget.terms(
        "the Euler class, both pitches with the wrong sign",
        &s,
        &[B1],
    );
    let s = signature(&predict(&truth, &yaw_wrong));
    let worked = 2.0 * (ypr[0][0] - ypr[1][0]).abs();
    budget.hold_corrected(
        "the Euler class, both yaws with the wrong sign, a1",
        s.a1.abs() * ACROSS_PX,
        worked * ACROSS_PX,
        2.3,
    );
    budget.terms("the Euler class, both yaws with the wrong sign", &s, &[A1]);

    // The V6 readings against RadialOnly, the stored centres held, a lens
    // at a time: 8.2's figures at 3840², by lens, are five times 768²'s.
    let v6 = x5_like_v6();
    let read = |v6: TruthV6| Planted { v6, ..planted };
    let reading = |pair, extension, at, order| TruthV6 {
        tangential: Some(Tangential { pair, extension }),
        prism: Some(Prism { at, order }),
    };
    let readings = [
        (
            "{D6D5 with extension, D9 Interleaved}",
            reading(PairOrder::D6D5, true, PrismAt::D9, PrismOrder::Interleaved),
            [15.5, 11.9],
        ),
        (
            "{D5D6 with extension, D9 ByAxis}",
            reading(PairOrder::D5D6, true, PrismAt::D9, PrismOrder::ByAxis),
            [33.0, 20.5],
        ),
        (
            "{D5D6, D7 ByAxis}",
            reading(PairOrder::D5D6, false, PrismAt::D7, PrismOrder::ByAxis),
            [20.1, 17.5],
        ),
    ];
    let radial = rig_of(&v6, &read(TruthV6::RADIAL_ONLY), &far);
    let read_as: Vec<(RigTruth, Rig)> = readings
        .iter()
        .map(|&(_, r, _)| {
            (
                compose(&v6, &read(r), 0, sides),
                rig_of(&v6, &read(r), &far),
            )
        })
        .collect();
    let one_lens = |rig: &Rig, b: usize, from: &Rig| {
        let mut lenses = rig.lenses;
        lenses[b] = from.lenses[b];
        Rig { lenses, ..*rig }
    };
    let (mut moved, mut along) = ([f64::INFINITY, 0.0_f64], [f64::INFINITY, 0.0_f64]);
    for (&(name, _, at_3840), (truth_r, model_r)) in readings.iter().zip(&read_as) {
        for (b, at_3840) in at_3840.into_iter().enumerate() {
            let bins = predict(truth_r, &one_lens(model_r, b, &radial));
            let size = sizes(&bins);
            let row = format!("V6 reading {name} against RadialOnly, lens {b}");
            budget.hold(
                &format!("{row}, the most a bin moves"),
                size.most,
                at_3840 / 5.0,
            );
            budget.terms(&row, &signature(&bins), &[A1, B1, C1, D1]);
            moved = [moved[0].min(size.most), moved[1].max(size.most)];
            along = [along[0].min(size.along), along[1].max(size.along)];
        }
    }
    budget.hold("the V6 readings, the least a lens's moves", moved[0], 2.4);
    budget.hold("the V6 readings, the most a lens's moves", moved[1], 6.6);
    budget.hold(
        "the V6 readings, the least of it along",
        along[0],
        12.0 / 5.0,
    );
    budget.hold(
        "the V6 readings, the most of it along",
        along[1],
        32.0 / 5.0,
    );
    // And from each other, a lens at a time.
    let mut apart = [f64::INFINITY, 0.0_f64];
    for (i, (truth_r, model_r)) in read_as.iter().enumerate() {
        for (_, other) in &read_as[i + 1..] {
            for b in 0..2 {
                let most = sizes(&predict(truth_r, &one_lens(model_r, b, other))).most;
                apart = [apart[0].min(most), apart[1].max(most)];
            }
        }
    }
    budget.hold(
        "the V6 readings, the least two lie apart",
        apart[0],
        20.0 / 5.0,
    );
    budget.hold(
        "the V6 readings, the most two lie apart",
        apart[1],
        36.0 / 5.0,
    );

    // V3 against V6 on Pat's blocks, a lens at a time: the model's
    // intrinsics, then its angles, from V3 on a truth from V6. The table's
    // figures, by lens: what the intrinsics move a bin by, least and
    // most, and the angles' tilt.
    let (pats_v3, pats_v6) = (pats(Generation::V3), pats(Generation::V6));
    let truth_v6 = compose(&pats_v6, &planted, 0, sides);
    let (as_v3, as_v6) = (
        rig_of(&pats_v3, &planted, &far),
        rig_of(&pats_v6, &planted, &far),
    );
    assert!(sizes(&predict(&truth_v6, &as_v6)).most < 1e-6);
    for (b, (moves, tilt)) in [([2.2, 3.4], 1.8), ([1.6, 2.6], 1.1)]
        .into_iter()
        .enumerate()
    {
        let with = |lens: RigLens| {
            let mut lenses = as_v6.lenses;
            lenses[b] = lens;
            Rig { lenses, ..as_v6 }
        };
        let row = format!("V3 against V6, lens {b}");
        let bins = predict(
            &truth_v6,
            &with(RigLens {
                model: as_v3.lenses[b].model,
                ..as_v6.lenses[b]
            }),
        );
        let (size, s) = (sizes(&bins), signature(&bins));
        budget.hold(
            &format!("{row}, the least the intrinsics move a bin"),
            size.least,
            moves[0],
        );
        budget.hold(
            &format!("{row}, the most the intrinsics move a bin"),
            size.most,
            moves[1],
        );
        // a0: the seam's circle is this much longer in V6's picture.
        let longer = seam_radius(&as_v6.lenses[b]) - seam_radius(&as_v3.lenses[b]);
        if b == 0 {
            budget.hold_corrected(
                &format!("{row}, a0"),
                s.a0 * ACROSS_PX,
                longer,
                0.15 * ACROSS_PX,
            );
        } else {
            budget.hold_worked(&format!("{row}, a0"), s.a0 * ACROSS_PX, longer);
        }
        let s = signature(&predict(
            &truth_v6,
            &with(RigLens {
                lens_from_rig: as_v3.lenses[b].lens_from_rig,
                ..as_v6.lenses[b]
            }),
        ));
        budget.hold(
            &format!("{row}, the angles' tilt, a1/b1"),
            harmonics(&s).0 * ACROSS_PX,
            tilt,
        );
        budget.terms(&format!("{row}, the angles"), &s, &[A1, B1]);
    }

    // The binding reversed, on Pat's V6 angles and centres and in Tier
    // 1's rig, in its parts and whole.
    let angles = |lens: &mut RigLens, from: &RigLens| lens.lens_from_rig = from.lens_from_rig;
    let models = |lens: &mut RigLens, from: &RigLens| lens.model = from.model;
    let reversed = |p: &Planted| Planted {
        binding: TruthBinding::Reversed,
        ..*p
    };
    for (name, cal, tier_1) in [("Pat's V6", &pats_v6, false), ("Tier 1's rig", &cal, true)] {
        let row = format!("the binding reversed, {name}");
        let truth = compose(cal, &planted, 0, sides);
        let direct = rig_of(cal, &planted, &far);
        let swap = rig_of(cal, &reversed(&planted), &far);
        let ypr = [cal.blocks[0].ypr_deg, cal.blocks[1].ypr_deg];
        let centres = centres_apart(cal);
        // The angles alone: twice the yaws' difference, a yaw turning
        // about T_1's axis under the placeholder. The table's "at most
        // 1.1° with the roll about the lens axis".
        let tilt = 2.0 * (ypr[0][0] - ypr[1][0]);
        let s = signature(&predict(&truth, &swapped(&direct, &swap, angles)));
        let got = harmonics(&s).0 * ACROSS_PX;
        budget.hold_worked(
            &format!("{row}, the angles' tilt"),
            got,
            tilt.abs() * ACROSS_PX,
        );
        budget.terms(&format!("{row}, the angles"), &s, &[A1, B1]);
        assert!(tier_1 || got <= 1.1 * ACROSS_PX, "{got}");
        // The models alone: the centres' difference, across the seam.
        let across = 2.0 * CENTRE_ACROSS_DEG * centres[0];
        let s = signature(&predict(&truth, &swapped(&direct, &swap, models)));
        let got = harmonics(&s).0 * ACROSS_PX;
        budget.hold_worked(
            &format!("{row}, the models' a1/b1"),
            got,
            across.abs() * ACROSS_PX,
        );
        // The whole swap: the two together across, and the centres'
        // other part along.
        let bins = predict(&truth, &swap);
        let s = signature(&bins);
        let (got_across, got_along) = harmonics(&s);
        let (got_across, got_along) = (got_across * ACROSS_PX, got_along * ALONG_PX);
        let whole = (tilt + across).abs() * ACROSS_PX;
        let along = 2.0 * CENTRE_ALONG_DEG * centres[1].abs() * ALONG_PX;
        eprintln!(
            "3.4 | {row}: {} of {} bins inside the search",
            s.valid_bins,
            bins.len()
        );
        if tier_1 {
            // "Up to 9 px across in Tier 1's rig", and past the search
            // over part of the seam.
            budget.hold_corrected(&format!("{row}, a1/b1"), got_across, whole, 9.0);
            assert!(s.valid_bins < bins.len(), "{s:?}");
        } else {
            budget.hold_worked(&format!("{row}, a1/b1"), got_across, whole);
            budget.hold(&format!("{row}, c1/d1"), got_along, 0.4 * ALONG_PX);
            assert_eq!(s.valid_bins, bins.len());
        }
        budget.hold_worked(&format!("{row}, c1/d1"), got_along, along);
    }
    // Pat's V6 under two more conventions. Where the stored roll turns
    // about T_1's axis, the angles' tilt is twice the rolls' difference,
    // the table's "up to 2.3°". With T_1 about X it is twice the
    // pitches', and the centres' x parts are the ones that add along the
    // seam, the table's "0.6° along".
    let pats_swap = |p: &Planted| {
        let truth = compose(&pats_v6, p, 0, sides);
        let direct = rig_of(&pats_v6, p, &far);
        let swap = rig_of(&pats_v6, &reversed(p), &far);
        let tilt = signature(&predict(&truth, &swapped(&direct, &swap, angles)));
        let whole = signature(&predict(&truth, &swap));
        (
            harmonics(&tilt).0 * ACROSS_PX,
            harmonics(&whole).1 * ALONG_PX,
        )
    };
    let ypr = [pats_v6.blocks[0].ypr_deg, pats_v6.blocks[1].ypr_deg];
    let between = |angle: usize| 2.0 * (ypr[0][angle] - ypr[1][angle]).abs() * ACROSS_PX;
    let row = "the binding reversed, Pat's V6 with the roll about T_1's axis";
    let (tilt, _) = pats_swap(&Planted {
        euler: TruthEuler {
            axes: [Axis::X, Axis::Z, Axis::Y],
            ..TruthEuler::PLACEHOLDER
        },
        ..planted
    });
    budget.hold(&format!("{row}, the angles' tilt"), tilt, 2.3 * ACROSS_PX);
    budget.hold_worked(&format!("{row}, the angles' tilt"), tilt, between(2));
    let row = "the binding reversed, Pat's V6 with T_1 about X";
    let (tilt, along) = pats_swap(&Planted {
        turn: TruthTurn::X,
        ..planted
    });
    budget.hold_worked(&format!("{row}, the angles' tilt"), tilt, between(1));
    budget.hold(&format!("{row}, c1/d1"), along, 0.6 * ALONG_PX);
    let worked = 2.0 * CENTRE_ALONG_DEG * centres_apart(&pats_v6)[0].abs() * ALONG_PX;
    budget.hold_worked(&format!("{row}, c1/d1"), along, worked);

    assert!(
        budget.missed.is_empty(),
        "3.4's rows that the predictions miss: {:#?}",
        budget.missed
    );
}
