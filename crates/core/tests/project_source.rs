//! A media's source in the project (M3 plan 11.1 and 11.2): `validate`'s
//! rules for the tune, the gains, the caps and the snapshot, the feather
//! held short of each lens's frame, the coverage rule, and the Auto
//! gains read along their track.
//!
//! M3.9's types stage names the tests; unit (a) writes them and the
//! suite's budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::OnceLock;
use std::time::Instant;

use spherecut_core::project::source::{AnalysisInput, Analysed, Gains, Sampling};
use spherecut_core::project::Fingerprint;
use spherecut_core::{Rational, Time};
use spherecut_testkit::control::must_fail;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run. The tests run side by side, so none of
/// them can time the rest: the last line printed is the suite's time.
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
            "{}: {:.3} s; the suite {suite:.3} s so far",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// Every number `validate` bounds (11.2), at each end (valid) and a step
/// past it (refused, naming the media and the field), and the structural
/// rules (distinct tracks, front track 0 or 1, the geometry's generation
/// in the snapshot, the hash of the raw strings); the row count pinned so
/// no rule drops unseen.
#[test]
#[ignore = "M3.9 unit (a)"]
fn validate_at_each_bound_and_just_past() {
    let _time = timed("validate_at_each_bound_and_just_past");
    todo!()
}

/// The largest feather that validates is max(0, min(θ_max, θ_frame) −
/// 90°), each lens in turn, θ_frame from `rig::theta_frame` (11.2).
/// Control: a rule from θ_max alone passes a feather the frame cuts.
#[test]
#[ignore = "M3.9 unit (a)"]
fn the_feather_stops_short_of_the_frame() {
    let _time = timed("the_feather_stops_short_of_the_frame");
    todo!()
}

/// `Gains::at`: ones for Off, `rgb` for Fixed, and Auto's track exact at
/// its samples, linear between them, held past its ends, with g0 · g1 = 1
/// (plan 16). Control: a nearest-sample read fails the midpoint.
#[test]
fn gains_at_follows_its_track() {
    let _time = timed("gains_at_follows_its_track");
    // Exact for every time here, each a multiple of 1/16 s.
    let at = |s: f64| Time::seconds(Rational::new((s * 1024.0).round() as i64, 1024).unwrap());
    assert_eq!(Gains::Off {}.at(at(3.0)), [[1.0; 3]; 2]);
    let rgb = [[1.1, 1.0, 0.9], [0.8, 1.0, 1.25]];
    assert_eq!(Gains::Fixed { rgb }.at(at(3.0)), rgb);

    // A ramp of 0 to 0.2 over 8 samples at 4 a second, red rising, green
    // at half of it and blue falling.
    let rate = 4.0;
    let track: Vec<[f32; 3]> = (0..8)
        .map(|k| {
            let v = 0.2 * k as f32 / 7.0;
            [v, v / 2.0, -v]
        })
        .collect();
    let auto = auto(rate, track.clone());
    // What the split gives for a log ratio `l` per channel.
    let split = |l: [f64; 3]| {
        let mut g = [[0.0; 3]; 2];
        for c in 0..3 {
            g[0][c] = (l[c] / 2.0).exp();
            g[1][c] = (-l[c] / 2.0).exp();
        }
        g
    };
    let sample = |k: usize| track[k].map(f64::from);
    // Exact at each sample.
    for k in 0..8 {
        assert_eq!(auto.at(at(k as f64 / rate)), split(sample(k)), "sample {k}");
    }
    // Linear between: a quarter and half of the way.
    let near = |a: [[f64; 3]; 2], b: [[f64; 3]; 2]| {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() <= 1e-12)
    };
    let between = |k: usize, f: f64| {
        let (a, b) = (sample(k), sample(k + 1));
        split([0, 1, 2].map(|c| a[c] + (b[c] - a[c]) * f))
    };
    for k in 0..7 {
        for f in [0.25, 0.5] {
            let g = auto.at(at((k as f64 + f) / rate));
            assert!(near(g, between(k, f)), "sample {k} + {f}: {g:?}");
        }
    }
    // Held past its ends, a time before 0 too.
    assert_eq!(auto.at(at(-1.0)), split(sample(0)));
    assert_eq!(auto.at(at(100.0)), split(sample(7)));
    // Both blocks meet at the geometric mean: g0 · g1 = 1.
    for s in [-1.0, 0.0, 0.375, 1.0, 1.125, 1.75, 9.0] {
        let g = auto.at(at(s));
        for c in 0..3 {
            assert!((g[0][c] * g[1][c] - 1.0).abs() <= 1e-15, "{s} s: {g:?}");
        }
    }
    // A track with no samples (validate refuses it) gives ones.
    assert_eq!(auto_empty().at(at(1.0)), [[1.0; 3]; 2]);

    // Control: a nearest-sample read is off at the midpoint.
    let nearest = |t: f64| split(sample(((t * rate).round() as usize).min(7)));
    must_fail(
        "a nearest-sample read of the track",
        near(nearest(3.5 / rate), between(3, 0.5)),
    );
}

/// An Auto track of `log_ratio` at `rate_hz`, its provenance a stand-in.
fn auto(rate_hz: f64, log_ratio: Vec<[f32; 3]>) -> Gains {
    Gains::Auto {
        rate_hz,
        log_ratio,
        basis: "0000000000000000".into(),
        input: AnalysisInput {
            file: Analysed::Master,
            fingerprint: Fingerprint {
                bytes: 1,
                head: "0000000000000000".into(),
            },
            frames: Sampling::Every { n: 1 },
        },
        estimator: 1,
    }
}

fn auto_empty() -> Gains {
    auto(4.0, Vec::new())
}

/// F038: `validate` refuses ξ + `xi_delta` ≤ −1 for each lens, the rule
/// `Rig::build` applies, and holds `xi_delta` to the range this slice
/// proposes beside the tune's others (11.2).
#[test]
#[ignore = "M3.9 unit (a)"]
fn xi_delta_keeps_xi_above_minus_one() {
    let _time = timed("xi_delta_keeps_xi_above_minus_one");
    todo!()
}

/// A tune under which some direction is covered by neither lens at the
/// project's q is refused (the coverage rule): `Rig::weights` about the
/// seam is never (0, 0) at tunes the rule passes, and is somewhere at
/// tunes it refuses. Control: the rule with the angle between the lenses'
/// axes left out passes a yawed tune whose hole the grid finds.
#[test]
#[ignore = "M3.9 unit (a)"]
fn the_coverage_rule_finds_the_holes() {
    let _time = timed("the_coverage_rule_finds_the_holes");
    todo!()
}
