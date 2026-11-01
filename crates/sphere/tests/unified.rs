//! The stitching lens models (M3 plan 6.1 and 6.3): `fisheye::Unified`
//! and its inverse, the canonical distortion and every reading that maps
//! onto it, the 21 V6 readings, and `KbLens`. The properties run 100,000
//! cases in the slice's check (19.3).
//!
//! Each reading's formula is written here term by term from its fields,
//! and the two sourced readings as their sources name the terms, apart
//! from `Canonical`'s code. Pat's numbers are docs/formats/insta360.md's
//! ("Pat's X5 samples"), in canvas pixels with block 1's cx local to its
//! square.
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): sphere's properties and rung 0
//! take at most 30 s together at `PROPTEST_CASES=100000`, shared with
//! `rig.rs` and synth's rung 0; each test prints its time and the
//! suite's. `PROPTEST_CASES` sets the count (256 by default).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::f64::consts::{FRAC_PI_2, PI};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use glam::{DVec2, DVec3};
use proptest::array::{uniform3, uniform5, uniform8};
use proptest::prelude::*;
use spherecut_sphere::angle_between;
use spherecut_sphere::fisheye::{
    Canonical, F, KbLens, Lens, Model, PairOrder, Prism, PrismAt, PrismOrder, Tangential, Unified,
    V6Reading,
};
use spherecut_testkit::control::{at_most, below, must_fail};

/// The suite's budget in seconds, at 100,000 cases, shared as the module
/// doc says.
const BUDGET_S: f64 = 30.0;

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
            "{}: {:.3} s; the suite {suite:.3} s so far, of a {BUDGET_S} s budget at 100,000 \
             cases",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

/// 6.3's round-trip tolerances: in angle, and in the lens's own pixels.
const ANGLE_TOL: f64 = 1e-7;
const PIXEL_TOL: f64 = 1e-6;

/// Pat's V3 blocks' ξ, fx, fy, cx (local) and cy.
const PATS_V3: [[f64; 5]; 2] = [
    [2.0, 4284.620, 4284.280, 2701.130, 2692.130],
    [2.0, 4291.030, 4291.030, 8069.000 - 5376.0, 2684.950],
];

/// Their k1 to k3.
const PATS_V3_K: [[f64; 3]; 2] = [
    [0.18632051, 2.04984355, -3.24446917],
    [0.18940836, 2.07073069, -3.24103332],
];

/// Their p1 and p2.
const PATS_V3_P: [[f64; 2]; 2] = [[-0.00143576, -0.00339240], [0.00013831, -0.00299873]];

/// Pat's V6 blocks' ξ, fx, fy, cx (local) and cy.
const PATS_V6: [[f64; 5]; 2] = [
    [2.0, 4277.520, 4277.270, 2713.050, 2689.210],
    [2.0, 4284.400, 4284.760, 8075.730 - 5376.0, 2680.350],
];

/// Pat's V6 d0 to d12, by block. Every one of d5 to d12 is non-zero.
const PATS_D: [[f64; 13]; 2] = [
    [
        0.28032997,
        0.89045405,
        2.04927588,
        -8.05179787,
        0.0,
        0.00120200,
        -0.00259487,
        0.02324706,
        0.00533920,
        -0.01560208,
        0.00019357,
        -0.04580523,
        0.00197187,
    ],
    [
        0.27665332,
        0.98522973,
        1.83605266,
        -7.99374485,
        0.0,
        0.00021056,
        -0.00147626,
        0.01940425,
        0.00331838,
        -0.01330665,
        0.00074511,
        -0.02859299,
        0.00888746,
    ],
];

/// The reading that reads every one of d5 to d12: the pair in order with
/// its extension, and the prism at d9 by axis.
const EVERY_TERM: V6Reading = V6Reading {
    tangential: Some(Tangential {
        pair: PairOrder::D5D6,
        extension: true,
    }),
    prism: Some(Prism {
        at: PrismAt::D9,
        order: PrismOrder::ByAxis,
    }),
};

/// A lens of ξ, fx, fy, cx and cy, as the blocks above list them.
fn lens_of([xi, fx, fy, cx, cy]: [f64; 5], dist: Canonical) -> Unified {
    Unified {
        xi,
        fx,
        fy,
        cx,
        cy,
        dist,
    }
}

fn pats_v3(block: usize) -> Unified {
    lens_of(
        PATS_V3[block],
        Canonical::v3(PATS_V3_K[block], PATS_V3_P[block]),
    )
}

fn pats_v6(block: usize, reading: V6Reading) -> Unified {
    lens_of(PATS_V6[block], Canonical::v6(PATS_D[block], reading))
}

/// Pat's four lenses, named, V6 read through `EVERY_TERM`.
fn pats_lenses() -> [(&'static str, Unified); 4] {
    [
        ("V3 block 0", pats_v3(0)),
        ("V3 block 1", pats_v3(1)),
        ("V6 block 0", pats_v6(0, EVERY_TERM)),
        ("V6 block 1", pats_v6(1, EVERY_TERM)),
    ]
}

/// The unit direction θ off the +z axis at azimuth φ, in the lens frame.
fn dir(theta: f64, phi: f64) -> DVec3 {
    let (st, ct) = theta.sin_cos();
    DVec3::new(st * phi.cos(), st * phi.sin(), ct)
}

/// θ of a lens-frame direction.
fn theta_of(d: DVec3) -> f64 {
    d.x.hypot(d.y).atan2(d.z)
}

/// The model's own end for ξ, worked here from 6.1's text.
fn end_theta(xi: f64) -> f64 {
    if xi > 1.0 {
        (-1.0 / xi).acos()
    } else {
        (-xi).acos()
    }
}

/// The plane radius sin θ/(cos θ + ξ).
fn plane(xi: f64, theta: f64) -> f64 {
    theta.sin() / (theta.cos() + xi)
}

/// The same lens with only its radial terms, at unit focal and no
/// offset: its pixels are the radial curve's plane points.
fn radial_only(lens: &Unified) -> Unified {
    Unified {
        fx: 1.0,
        fy: 1.0,
        cx: 0.0,
        cy: 0.0,
        dist: Canonical {
            k: lens.dist.k,
            ..Canonical::NONE
        },
        ..*lens
    }
}

/// r(θ) of a lens: the radial curve's plane radius at θ.
fn radius(lens: &Unified, theta: f64) -> f64 {
    radial_only(lens).project(dir(theta, 0.0)).unwrap().length()
}

/// The most passes the inverse may take on 6.3's draws. 100,000 random
/// draws take 12 or 13, and the worst found over the corners of the
/// draw space (every multiplier at an end, ξ from 0 to 2.5, θ at the top
/// of the range, φ every 10°) is 15, at ξ = 0.5; one more allows for
/// azimuths between those steps. 6.1's "at most 10" doesn't hold.
const MAX_DRAWN_PASSES: u32 = 16;

/// The most passes the round trip's draws took in this run.
static MOST_PASSES: AtomicU32 = AtomicU32::new(0);

/// One draw of 6.3's generator: a lens, and a direction it images up to
/// min(100°, `limit_theta` − 1°), which is `top`.
#[derive(Debug, Clone, Copy)]
struct Draw {
    lens: Unified,
    d: DVec3,
    top: f64,
}

/// 6.3's lenses: ξ in [0, 2.5], then terms around Pat's V6 block 0 (the
/// radial ones × 0.8 to 1.2, the rest × −1.5 to 1.5) scaled to that ξ. A
/// term of degree n in m is multiplied by (m_X5/m_top)^(n − 1), where
/// m_top is the plane radius at min(100°, the model's end − 1°) and m_X5
/// the X5's at 100°, so that the distortion at m is the X5's at
/// m · m_X5/m_top, shrunk back by the same factor. The focal grows by the
/// factor too, so the image is the X5's size in canvas pixels.
fn scaled_lens(
    xi: f64,
    radial: [f64; 5],
    rest: [f64; 8],
    focal: f64,
    aspect: f64,
    offset: DVec2,
) -> Unified {
    let s = plane(2.0, 100f64.to_radians())
        / plane(
            xi,
            100f64.to_radians().min(end_theta(xi) - 1f64.to_radians()),
        );
    let d = PATS_D[0];
    let k: [f64; 5] = std::array::from_fn(|i| d[i] * radial[i] * s.powi(2 * i as i32 + 2));
    // p1 and p2, and s1 and s3, are of degree 2; p1', p2', s2 and s4 of 4.
    let dist = Canonical {
        k,
        p: [d[5] * rest[0] * s, d[6] * rest[1] * s],
        pe: [d[7] * rest[2] * s.powi(3), d[8] * rest[3] * s.powi(3)],
        s: [
            d[9] * rest[4] * s,
            d[10] * rest[5] * s.powi(3),
            d[11] * rest[6] * s,
            d[12] * rest[7] * s.powi(3),
        ],
    };
    let fx = 4277.52 * s * focal;
    Unified {
        xi,
        fx,
        fy: fx * (1.0 + aspect),
        cx: 2688.0 + offset.x,
        cy: 2688.0 + offset.y,
        dist,
    }
}

fn draw() -> impl Strategy<Value = Draw> {
    (
        0.0f64..=2.5,
        uniform5(0.8f64..=1.2),
        uniform8(-1.5f64..=1.5),
        (0.98f64..=1.02, -0.001f64..=0.001),
        (-20.0f64..=20.0, -20.0f64..=20.0),
        0.0f64..=1.0,
        -PI..PI,
    )
        .prop_map(
            |(xi, radial, rest, (focal, aspect), (dx, dy), along, phi)| {
                let lens = scaled_lens(xi, radial, rest, focal, aspect, DVec2::new(dx, dy));
                let top = 100f64
                    .to_radians()
                    .min(lens.limit_theta() - 1f64.to_radians());
                Draw {
                    lens,
                    d: dir(along * top, phi),
                    top,
                }
            },
        )
}

/// V3's formula as docs/formats/insta360.md §4.3 writes it, term by term.
fn v3_formula(k: [f64; 3], p: [f64; 2], m: DVec2) -> DVec2 {
    let (mx, my) = (m.x, m.y);
    let r2 = mx * mx + my * my;
    let big_r = 1.0 + k[0] * r2 + k[1] * r2 * r2 + k[2] * r2 * r2 * r2;
    DVec2::new(
        mx * big_r + 2.0 * p[0] * mx * my + p[1] * (r2 + 2.0 * mx * mx),
        my * big_r + 2.0 * p[1] * mx * my + p[0] * (r2 + 2.0 * my * my),
    )
}

/// Reading `v`'s own formula for d0 to d12 at the plane point m, written
/// term by term from its fields (6.1): d0 to d4 radial; the pair by its
/// order, each with its r² extension when the reading has one; the prism
/// from where it starts, by its order.
fn reading_formula(d: &[f64; 13], v: V6Reading, m: DVec2) -> DVec2 {
    let (x, y) = (m.x, m.y);
    let r2 = x * x + y * y;
    let r4 = r2 * r2;
    let radial =
        1.0 + d[0] * r2 + d[1] * r4 + d[2] * r4 * r2 + d[3] * r4 * r4 + d[4] * r4 * r4 * r2;
    let mut xd = x * radial;
    let mut yd = y * radial;
    if let Some(t) = v.tangential {
        let (first, second, first_ext, second_ext) = match t.pair {
            PairOrder::D5D6 => (d[5], d[6], d[7], d[8]),
            PairOrder::D6D5 => (d[6], d[5], d[8], d[7]),
        };
        let (big_p1, big_p2) = if t.extension {
            (first + first_ext * r2, second + second_ext * r2)
        } else {
            (first, second)
        };
        xd += 2.0 * big_p1 * x * y + big_p2 * (r2 + 2.0 * x * x);
        yd += 2.0 * big_p2 * x * y + big_p1 * (r2 + 2.0 * y * y);
    }
    if let Some(p) = v.prism {
        let at = match p.at {
            PrismAt::D7 => 7,
            PrismAt::D9 => 9,
        };
        let (x2, x4, y2, y4) = match p.order {
            PrismOrder::ByAxis => (d[at], d[at + 1], d[at + 2], d[at + 3]),
            PrismOrder::Interleaved => (d[at], d[at + 2], d[at + 1], d[at + 3]),
        };
        xd += x2 * r2 + x4 * r4;
        yd += y2 * r2 + y4 * r4;
    }
    DVec2::new(xd, yd)
}

/// GyroView's and raw360-formats' reading as 6.1's table names its terms:
/// k1..k5, p1..p4, s1..s4 in token order, p3 and p4 read as the r²
/// extensions of p1 and p2, and the thin prism's s1, s2 on x and s3, s4
/// on y.
fn gyroview_formula(d: &[f64; 13], m: DVec2) -> DVec2 {
    let [k1, k2, k3, k4, k5, p1, p2, p3, p4, s1, s2, s3, s4] = *d;
    let (x, y) = (m.x, m.y);
    let r2 = x * x + y * y;
    let radial =
        1.0 + k1 * r2 + k2 * r2.powi(2) + k3 * r2.powi(3) + k4 * r2.powi(4) + k5 * r2.powi(5);
    DVec2::new(
        x * radial
            + 2.0 * (p1 + p3 * r2) * x * y
            + (p2 + p4 * r2) * (r2 + 2.0 * x * x)
            + s1 * r2
            + s2 * r2.powi(2),
        y * radial
            + 2.0 * (p2 + p4 * r2) * x * y
            + (p1 + p3 * r2) * (r2 + 2.0 * y * y)
            + s3 * r2
            + s4 * r2.powi(2),
    )
}

/// orbis's reading as 6.1's table names its terms: k1..k4, one reserved,
/// p1, p2, s1..s4 and two reserved, in token order; the reserved terms
/// read nothing.
fn orbis_formula(d: &[f64; 13], m: DVec2) -> DVec2 {
    let [k1, k2, k3, k4, _, p1, p2, s1, s2, s3, s4, _, _] = *d;
    let (x, y) = (m.x, m.y);
    let r2 = x * x + y * y;
    let radial = 1.0 + k1 * r2 + k2 * r2.powi(2) + k3 * r2.powi(3) + k4 * r2.powi(4);
    DVec2::new(
        x * radial + 2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x) + s1 * r2 + s2 * r2.powi(2),
        y * radial + 2.0 * p2 * x * y + p1 * (r2 + 2.0 * y * y) + s3 * r2 + s4 * r2.powi(2),
    )
}

/// How far two evaluations of a distortion differ, relative to its size:
/// the order of the sums is all that may differ.
fn misfit(a: DVec2, b: DVec2) -> f64 {
    (a - b).length() / (1.0 + a.length())
}

/// The tolerance on `misfit` for two evaluations of one formula.
const SAME_FORMULA: f64 = 1e-13;

/// A plane point up to 0.8 from the centre, past the X5's 0.58 at the
/// fold.
fn plane_point() -> impl Strategy<Value = DVec2> {
    (0.0f64..=0.8, -PI..PI).prop_map(|(r, phi)| DVec2::new(r * phi.cos(), r * phi.sin()))
}

/// d0 to d12 around Pat's V6 block 0 (the radial ones × 0.8 to 1.2, the
/// rest × −1.5 to 1.5), with d4, which is 0 on Pat's unit, drawn in
/// [−1, 1].
fn terms() -> impl Strategy<Value = [f64; 13]> {
    (
        uniform5(0.8f64..=1.2),
        uniform8(-1.5f64..=1.5),
        -1.0f64..=1.0,
    )
        .prop_map(|(radial, rest, d4)| {
            let mut d = PATS_D[0];
            for (t, f) in d.iter_mut().zip(radial.iter().chain(rest.iter())) {
                *t *= f;
            }
            d[4] = d4;
            d
        })
}

/// The d indices a reading's canonical form takes, by putting 1 in one
/// term at a time: each, with how many of the canonical form's fields it
/// lands in.
fn terms_read(v: V6Reading) -> Vec<(usize, usize)> {
    (5..13)
        .filter_map(|i| {
            let mut d = [0.0; 13];
            d[i] = 1.0;
            let c = Canonical::v6(d, v);
            let fields =
                c.p.iter()
                    .chain(&c.pe)
                    .chain(&c.s)
                    .filter(|&&t| t == 1.0)
                    .count();
            (fields > 0).then_some((i, fields))
        })
        .collect()
}

/// Whether a reading reads each of d5 to d12 at most once, or the first
/// term it reads twice.
fn reads_each_once(v: V6Reading) -> Result<(), usize> {
    match terms_read(v).into_iter().find(|&(_, fields)| fields > 1) {
        Some((i, _)) => Err(i),
        None => Ok(()),
    }
}

/// Whether the 21 readings give 21 different distortions of terms `d`
/// (each pair apart by more than 1e-9 at one of a few plane points), or
/// the first pair that doesn't.
fn readings_distinct(d: [f64; 13]) -> Result<(), (V6Reading, V6Reading)> {
    let points = [
        DVec2::new(0.31, 0.17),
        DVec2::new(-0.22, 0.41),
        DVec2::new(0.47, -0.09),
    ];
    let all: Vec<V6Reading> = V6Reading::all().collect();
    for (i, &a) in all.iter().enumerate() {
        for &b in &all[i + 1..] {
            let apart = points.iter().any(|&m| {
                (Canonical::v6(d, a).distort(m) - Canonical::v6(d, b).distort(m)).length() > 1e-9
            });
            if !apart {
                return Err((a, b));
            }
        }
    }
    Ok(())
}

/// The inverse's worst angle over θ up to `max_deg` in steps of
/// `step_deg`, and φ every `phi_deg`, after at most `passes` passes, with
/// the most passes it took and the worst residual it left, the plane
/// point lifted whatever its residual.
fn sweep(
    lens: &Unified,
    max_deg: f64,
    step_deg: f64,
    phi_deg: f64,
    passes: u32,
) -> (f64, u32, f64) {
    let mut worst = (0.0f64, 0u32, 0.0f64);
    let steps = (max_deg / step_deg).round() as u32;
    for i in 0..=steps {
        let theta = (f64::from(i) * step_deg).to_radians();
        for j in 0..(360.0 / phi_deg) as u32 {
            let d = dir(theta, (f64::from(j) * phi_deg).to_radians());
            let px = lens.project(d).unwrap();
            let inv = lens.invert(px, passes).unwrap();
            worst.0 = worst.0.max(angle_between(lens.lift(inv.m), d));
            worst.1 = worst.1.max(inv.passes);
            worst.2 = worst.2.max(inv.residual);
        }
    }
    worst
}

// The properties, each run whole by a test below that times it.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn unproject_inverts_project_cases(w in draw()) {
        // The range reaches at least 88° at every ξ (6.3).
        prop_assert!(w.top >= 88f64.to_radians(), "the range ends at {}°", w.top.to_degrees());
        let px = w.lens.project(w.d);
        prop_assert!(px.is_some());
        let px = px.unwrap();
        let back = w.lens.unproject(px);
        prop_assert!(back.is_some(), "no direction for {:?}", px);
        let err = angle_between(back.unwrap(), w.d);
        prop_assert!(err <= ANGLE_TOL, "{err:e} rad off");
        let passes = w.lens.invert(px, Unified::PASSES).unwrap().passes;
        MOST_PASSES.fetch_max(passes, Ordering::Relaxed);
        prop_assert!(passes <= MAX_DRAWN_PASSES, "{passes} passes");
    }

    fn project_inverts_unproject_cases(w in draw()) {
        let px = w.lens.project(w.d).unwrap();
        let back = w.lens.unproject(px).unwrap();
        let again = w.lens.project(back);
        prop_assert!(again.is_some());
        let err = (again.unwrap() - px).length();
        prop_assert!(err <= PIXEL_TOL, "{err:e} px off");
    }

    fn radius_rises_cases(w in draw()) {
        let limit = w.lens.limit_theta();
        let mut last = 0.0;
        for i in 1..=256 {
            // The last step stops a hair short of the limit, which at the
            // projection's own fold may be past what `project` images by
            // a rounding.
            let theta = limit * (f64::from(i) / 256.0).min(1.0 - 1e-12);
            let r = radius(&w.lens, theta);
            prop_assert!(r > last, "r falls at {}° of a limit of {}°", theta.to_degrees(), limit.to_degrees());
            last = r;
        }
    }

    fn none_past_limit_cases(w in draw(), beyond in 1e-6f64..=1.0, spot in (-1.6f64..=1.6, -1.6f64..=1.6)) {
        let limit = w.lens.limit_theta();
        // Past the radial curve's edge, in the direction of the draw.
        let flat = radial_only(&w.lens);
        let phi = w.d.y.atan2(w.d.x);
        let edge = flat.project(dir(limit * (1.0 - 1e-12), phi)).unwrap();
        prop_assert_eq!(flat.unproject(edge * (1.0 + beyond)), None);
        // Anywhere on the canvas, with every term, never past the limit.
        let reach = w.lens.fx * edge.length();
        let px = DVec2::new(w.lens.cx, w.lens.cy) + DVec2::new(spot.0, spot.1) * reach;
        if let Some(d) = w.lens.unproject(px) {
            prop_assert!(theta_of(d) <= limit + 1e-9, "θ {}° past {}°", theta_of(d).to_degrees(), limit.to_degrees());
        }
    }

    fn v3_formula_cases(k in uniform3(0.8f64..=1.2), p in (-1.5f64..=1.5, -1.5f64..=1.5), m in plane_point()) {
        let (k3, p3) = (PATS_V3_K[0], PATS_V3_P[0]);
        let k = [k3[0] * k[0], k3[1] * k[1], k3[2] * k[2]];
        let p = [p3[0] * p.0, p3[1] * p.1];
        let err = misfit(v3_formula(k, p, m), Canonical::v3(k, p).distort(m));
        prop_assert!(err <= SAME_FORMULA, "{err:e}");
    }

    fn v6_formula_cases(d in terms(), m in plane_point()) {
        for v in V6Reading::all() {
            let err = misfit(reading_formula(&d, v, m), Canonical::v6(d, v).distort(m));
            prop_assert!(err <= SAME_FORMULA, "{v:?}: {err:e}");
        }
    }

    fn sourced_cases(d in terms(), m in plane_point()) {
        let gyroview = V6Reading { tangential: Some(Tangential { pair: PairOrder::D5D6, extension: true }), prism: Some(Prism { at: PrismAt::D9, order: PrismOrder::ByAxis }) };
        let orbis = V6Reading { tangential: Some(Tangential { pair: PairOrder::D5D6, extension: false }), prism: Some(Prism { at: PrismAt::D7, order: PrismOrder::ByAxis }) };
        let err = misfit(gyroview_formula(&d, m), Canonical::v6(d, gyroview).distort(m));
        prop_assert!(err <= SAME_FORMULA, "GyroView's: {err:e}");
        // orbis reserves d4, which is 0 on Pat's unit.
        let mut d = d;
        d[4] = 0.0;
        let err = misfit(orbis_formula(&d, m), Canonical::v6(d, orbis).distort(m));
        prop_assert!(err <= SAME_FORMULA, "orbis's: {err:e}");
    }

    fn kb_round_trip_cases(
        base in prop_oneof![Just(Model::Equidistant), Just(Model::Equisolid), Just(Model::Stereographic), Just(Model::Orthographic)],
        k in (-0.05f64..=0.05, -0.01f64..=0.01, -0.002f64..=0.002, -5e-4f64..=5e-4, -1e-4f64..=1e-4),
        terms in uniform8(-1.0f64..=1.0),
        (fx, aspect, cx, cy) in (800.0f64..=1200.0, -0.001f64..=0.001, 900.0f64..=1100.0, 900.0f64..=1100.0),
        along in 0.0f64..=1.0,
        phi in -PI..PI,
    ) {
        let bare = KbLens { base, k: [k.0, k.1, k.2, k.3, k.4], fx: 1.0, fy: 1.0, cx: 0.0, cy: 0.0, p: [0.0; 2], pe: [0.0; 2], s: [0.0; 4] };
        let top = 100f64.to_radians().min(bare.limit_theta() - 1f64.to_radians());
        // The tangential and prism terms move the plane point at the top
        // of the range by up to 2e-3 of its radius for those of degree 2
        // and 1e-3 for those of degree 4, about as much as the X5's do.
        let r = bare.project(dir(top, 0.0)).unwrap().length();
        let [a, b, c, e, f, g, h, i] = terms;
        let lens = KbLens {
            fx,
            fy: fx * (1.0 + aspect),
            cx,
            cy,
            p: [2e-3 * a / r, 2e-3 * b / r],
            pe: [1e-3 * c / r.powi(3), 1e-3 * e / r.powi(3)],
            s: [2e-3 * f / r, 1e-3 * g / r.powi(3), 2e-3 * h / r, 1e-3 * i / r.powi(3)],
            ..bare
        };
        let d = dir(along * top, phi);
        let px = lens.project(d);
        prop_assert!(px.is_some());
        let px = px.unwrap();
        let back = lens.unproject(px);
        prop_assert!(back.is_some(), "no direction for {:?}", px);
        let back = back.unwrap();
        let err = angle_between(back, d);
        prop_assert!(err <= ANGLE_TOL, "{err:e} rad off");
        let err = (lens.project(back).unwrap() - px).length();
        prop_assert!(err <= PIXEL_TOL, "{err:e} px off");
    }
}

/// unproject(project(d)) is within 1e-7 rad of d for ξ in [0, 2.5] and
/// θ up to min(100°, limit_theta − 1°), the coefficients drawn around
/// Pat's V6 and scaled to ξ (6.3).
#[test]
fn unified_unproject_inverts_project() {
    let _time = timed("unified_unproject_inverts_project");
    unproject_inverts_project_cases();
    eprintln!(
        "the draws took at most {} passes",
        MOST_PASSES.load(Ordering::Relaxed)
    );
    // Control: the distorted plane point lifted as it stands, the
    // distortion not undone.
    let lens = pats_v6(0, EVERY_TERM);
    let d = dir(60f64.to_radians(), 0.5);
    let px = lens.project(d).unwrap();
    let distorted = DVec2::new((px.x - lens.cx) / lens.fx, (px.y - lens.cy) / lens.fy);
    must_fail(
        "the distortion left in",
        at_most(angle_between(lens.lift(distorted), d), ANGLE_TOL),
    );
}

/// project(unproject(px)) is within 1e-6 px of px, in the lens's own
/// pixels, over the same draws.
#[test]
fn unified_project_inverts_unproject() {
    let _time = timed("unified_project_inverts_unproject");
    project_inverts_unproject_cases();
    // Control: three passes leave the pixel off by more on Pat's V6 at
    // 100°.
    let lens = pats_v6(0, EVERY_TERM);
    let worst = (0..72)
        .map(|j| {
            let px = lens
                .project(dir(100f64.to_radians(), f64::from(j) * 5f64.to_radians()))
                .unwrap();
            let m = lens.invert(px, 3).unwrap().m;
            (lens.project(lens.lift(m)).unwrap() - px).length()
        })
        .fold(0.0, f64::max);
    let off = must_fail("three passes", at_most(worst, PIXEL_TOL));
    eprintln!("three passes leave up to {off:e} px on Pat's V6 block 0 at 100°");
}

/// With no distortion, ξ = 0 gives r = tan θ and ξ = 1 gives tan(θ/2).
#[test]
fn unified_is_tan_at_xi_0_and_tan_half_at_xi_1() {
    let _time = timed("unified_is_tan_at_xi_0_and_tan_half_at_xi_1");
    let ideal = |xi| Unified {
        xi,
        fx: 1.0,
        fy: 1.0,
        cx: 0.0,
        cy: 0.0,
        dist: Canonical::NONE,
    };
    // The worst relative misfit to r(θ) over θ below `max_deg`, and to
    // the azimuth.
    let worst = |xi: f64, max_deg: u32, r: &dyn Fn(f64) -> f64| {
        let mut worst = 0.0f64;
        for i in 0..max_deg {
            let theta = f64::from(i).to_radians();
            for j in 0..24 {
                let phi = f64::from(j) * 15f64.to_radians();
                let px = ideal(xi).project(dir(theta, phi)).unwrap();
                let want = DVec2::new(phi.cos(), phi.sin()) * r(theta);
                worst = worst.max((px - want).length() / (1.0 + want.length()));
            }
        }
        worst
    };
    // Up to 175° at ξ = 1, where cos θ + 1 still holds its digits.
    assert!(worst(0.0, 90, &f64::tan) <= 1e-13);
    assert!(worst(1.0, 176, &|t: f64| (t / 2.0).tan()) <= 1e-13);
    // ...and the inverse gives the angles back.
    for (xi, max) in [(0.0, 89), (1.0, 179)] {
        for i in 0..max {
            let d = dir(f64::from(i).to_radians(), 0.7);
            let back = ideal(xi).unproject(ideal(xi).project(d).unwrap()).unwrap();
            assert!(angle_between(back, d) <= ANGLE_TOL, "ξ {xi} at {i}°");
        }
    }
    // Control: ξ = 1 read as tan θ.
    must_fail(
        "ξ = 1 against tan θ",
        at_most(worst(1.0, 90, &f64::tan), 1e-13),
    );
}

/// r(θ) rises all the way up to limit_theta.
#[test]
fn unified_radius_rises_up_to_limit_theta() {
    let _time = timed("unified_radius_rises_up_to_limit_theta");
    radius_rises_cases();
    // Control: a polynomial that folds at 45° (ξ = 0, k1 = −1/3), held to
    // rise up to the model's own end, 90°, short by a degree.
    let folds = Unified {
        xi: 0.0,
        fx: 1.0,
        fy: 1.0,
        cx: 0.0,
        cy: 0.0,
        dist: Canonical {
            k: [-1.0 / 3.0, 0.0, 0.0, 0.0, 0.0],
            ..Canonical::NONE
        },
    };
    let rises_to = |top: f64| {
        (1..=256).all(|i| {
            radius(&folds, top * f64::from(i) / 256.0)
                > radius(&folds, top * f64::from(i - 1) / 256.0)
        })
    };
    assert!(rises_to(folds.limit_theta()));
    must_fail("the end taken for the limit", rises_to(89f64.to_radians()));
}

/// unproject is `None` past limit_theta.
#[test]
fn unified_unproject_is_none_past_limit_theta() {
    let _time = timed("unified_unproject_is_none_past_limit_theta");
    none_past_limit_cases();
    // Control: a pixel a millionth inside the edge has a direction.
    let flat = radial_only(&pats_v6(0, EVERY_TERM));
    let edge = flat
        .project(dir(flat.limit_theta() * (1.0 - 1e-12), 0.3))
        .unwrap();
    must_fail(
        "a pixel inside the edge",
        flat.unproject(edge * (1.0 - 1e-6)).is_none(),
    );
}

/// limit_theta is acos(−1/ξ) for ξ > 1 (120° at ξ = 2), acos(−ξ) for
/// ξ ≤ 1 (90° at ξ = 0), and earlier where the polynomial folds: 120.1°
/// on Pat's V6.
#[test]
fn unified_limit_theta_is_where_the_model_folds() {
    let _time = timed("unified_limit_theta_is_where_the_model_folds");
    let lens = |xi: f64, k: [f64; 5]| Unified {
        xi,
        fx: 1.0,
        fy: 1.0,
        cx: 0.0,
        cy: 0.0,
        dist: Canonical {
            k,
            ..Canonical::NONE
        },
    };
    // With no distortion, the model's own end.
    for xi in [0.0, 0.3, 0.5, 0.9, 1.0, 1.2, 2.0, 2.5] {
        let limit = lens(xi, [0.0; 5]).limit_theta();
        assert_eq!(limit, end_theta(xi), "ξ {xi}");
    }
    assert!((lens(2.0, [0.0; 5]).limit_theta() - 120f64.to_radians()).abs() < 1e-12);
    assert!((lens(0.0, [0.0; 5]).limit_theta() - FRAC_PI_2).abs() < 1e-12);
    // Where the polynomial folds first, worked by hand: with R = 1 + k1 r²
    // the curve's slope 1 + 3 k1 m² is 0 at m = 1 for k1 = −1/3, which is
    // 45° at ξ = 0 (m = tan θ) and 90° at ξ = 1 (m = tan θ/2); with
    // R = 1 − r⁴/5 the slope 1 − m⁴ is 0 at m = 1 too, where
    // sin θ − cos θ = 1/2 at ξ = 1/2: θ = 45° + asin(1/(2√2)).
    let folds = [
        (
            lens(0.0, [-1.0 / 3.0, 0.0, 0.0, 0.0, 0.0]),
            45f64.to_radians(),
        ),
        (lens(1.0, [-1.0 / 3.0, 0.0, 0.0, 0.0, 0.0]), FRAC_PI_2),
        (
            lens(0.5, [0.0, -0.2, 0.0, 0.0, 0.0]),
            45f64.to_radians() + (0.5 / 2f64.sqrt()).asin(),
        ),
    ];
    // The radius at θ + 0.01° against the radius at θ: below it once
    // the curve has folded.
    let falls_after =
        |l: &Unified, theta: f64| below(radius(l, theta + 0.01f64.to_radians()), radius(l, theta));
    for (l, at) in &folds {
        let limit = l.limit_theta();
        assert!(
            (limit - at).abs() < 1e-9,
            "{}° for {}°",
            limit.to_degrees(),
            at.to_degrees()
        );
        assert!(radius(l, limit) > radius(l, limit - 0.01f64.to_radians()));
        assert_eq!(falls_after(l, limit).passes(), Some(true));
    }
    // Pat's lenses: the curve stops rising where the projection folds,
    // 120.0° at ξ = 2. The polynomial alone would fold later, at the
    // plane radius where ξ = 0's limit puts it.
    for (name, l) in pats_lenses() {
        let limit = l.limit_theta();
        assert!(
            (limit - 120f64.to_radians()).abs() < 1e-12,
            "{name}: {}°",
            limit.to_degrees()
        );
        let alone = lens(0.0, l.dist.k).limit_theta().tan();
        assert!(
            alone > plane(2.0, limit),
            "{name}: the polynomial folds at m = {alone}"
        );
        assert!(radius(&l, limit * (1.0 - 1e-12)) > radius(&l, limit - 0.01f64.to_radians()));
        eprintln!(
            "{name}: limit_theta {:.6}°; the polynomial alone folds at m = {alone:.4}, past the \
             fold's {:.4}",
            limit.to_degrees(),
            plane(2.0, limit)
        );
    }
    // Control: a limit 0.1° short of the fold, where the curve still
    // rises.
    let (l, at) = &folds[0];
    must_fail(
        "0.1° short of the fold",
        falls_after(l, at - 0.1f64.to_radians()),
    );
}

/// The inverse meets 6.3's tolerances on Pat's V3 and V6 lenses up to
/// 119°, where three passes would not (6.1).
#[test]
fn unified_inverts_pats_lenses_to_119_degrees() {
    let _time = timed("unified_inverts_pats_lenses_to_119_degrees");
    for (name, lens) in pats_lenses() {
        let (angle, passes, residual) = sweep(&lens, 119.0, 0.5, 7.5, Unified::PASSES);
        eprintln!(
            "{name}: up to 119°, at most {passes} passes, residual up to {residual:e}, angle up \
             to {angle:e} rad"
        );
        assert!(passes <= 7, "{name}: {passes} passes");
        assert!(residual < Unified::RESIDUAL);
        assert!(angle <= ANGLE_TOL, "{name}: {angle:e} rad");
        // The pixel comes back too, at the lens's canvas focal.
        for i in 0..=119 {
            let px = lens.project(dir(f64::from(i).to_radians(), 1.1)).unwrap();
            let again = lens.project(lens.unproject(px).unwrap()).unwrap();
            assert!((again - px).length() <= PIXEL_TOL, "{name} at {i}°");
        }
        // Control: three passes, Gyroflow's count, at θ up to 100°.
        let (three, _, _) = sweep(&lens, 100.0, 0.5, 7.5, 3);
        let off = must_fail("three passes", at_most(three, ANGLE_TOL));
        eprintln!("{name}: three passes leave up to {off:e} rad at θ ≤ 100°");
    }
    // Every V6 reading, more coarsely. A reading that leaves some of the
    // terms out can take more passes than the one that reads them all:
    // up to 9 on a grid of 0.1° by 5°.
    let mut most = 0;
    for v in V6Reading::all() {
        for block in 0..2 {
            let (angle, passes, _) = sweep(&pats_v6(block, v), 119.0, 1.0, 15.0, Unified::PASSES);
            assert!(passes <= 9, "{v:?} on block {block}: {passes} passes");
            assert!(angle <= ANGLE_TOL, "{v:?} on block {block}: {angle:e} rad");
            most = most.max(passes);
        }
    }
    eprintln!("every V6 reading: at most {most} passes up to 119°");
}

/// V3's formula, written term by term, equals the canonical evaluation
/// of `Canonical::v3`.
#[test]
fn canonical_v3_is_its_own_formula() {
    let _time = timed("canonical_v3_is_its_own_formula");
    v3_formula_cases();
    // Control: p1 and p2 swapped.
    let (k, p) = (PATS_V3_K[0], PATS_V3_P[0]);
    let m = DVec2::new(0.3, -0.2);
    must_fail(
        "p1 and p2 swapped",
        at_most(
            misfit(
                v3_formula(k, [p[1], p[0]], m),
                Canonical::v3(k, p).distort(m),
            ),
            SAME_FORMULA,
        ),
    );
}

/// Each of the 21 readings' own formula, written term by term from its
/// fields, equals the canonical evaluation of `Canonical::v6`.
#[test]
fn canonical_each_v6_reading_is_its_own_formula() {
    let _time = timed("canonical_each_v6_reading_is_its_own_formula");
    v6_formula_cases();
    // Control: a reading's formula with the prism's second and third
    // terms swapped, against the canonical form.
    let d = PATS_D[0];
    let m = DVec2::new(0.35, 0.25);
    let mut swapped = d;
    swapped.swap(10, 11);
    must_fail(
        "the prism's second and third terms swapped",
        at_most(
            misfit(
                reading_formula(&swapped, EVERY_TERM, m),
                Canonical::v6(d, EVERY_TERM).distort(m),
            ),
            SAME_FORMULA,
        ),
    );
}

/// The two sourced readings, written out as their sources state them
/// (orbis's where d4 = 0), equal {D5D6 with extension, D9 ByAxis} and
/// {D5D6, D7 ByAxis} (6.1), so a slip in a permutation is caught.
#[test]
fn canonical_sourced_readings_are_the_values_named() {
    let _time = timed("canonical_sourced_readings_are_the_values_named");
    sourced_cases();
    // Controls: each source against its neighbour, one field changed.
    let d = PATS_D[1];
    let m = DVec2::new(-0.3, 0.4);
    let pair_swapped = V6Reading {
        tangential: Some(Tangential {
            pair: PairOrder::D6D5,
            extension: true,
        }),
        ..EVERY_TERM
    };
    must_fail(
        "GyroView's against the pair swapped",
        at_most(
            misfit(
                gyroview_formula(&d, m),
                Canonical::v6(d, pair_swapped).distort(m),
            ),
            SAME_FORMULA,
        ),
    );
    let interleaved = V6Reading {
        tangential: Some(Tangential {
            pair: PairOrder::D5D6,
            extension: false,
        }),
        prism: Some(Prism {
            at: PrismAt::D7,
            order: PrismOrder::Interleaved,
        }),
    };
    must_fail(
        "orbis's against the prism interleaved",
        at_most(
            misfit(
                orbis_formula(&d, m),
                Canonical::v6(d, interleaved).distort(m),
            ),
            SAME_FORMULA,
        ),
    );
}

/// `V6Reading::all` lists 21 distinct values, each reading every term at
/// most once.
#[test]
fn v6_space_has_21_distinct_readings() {
    let _time = timed("v6_space_has_21_distinct_readings");
    let all: Vec<V6Reading> = V6Reading::all().collect();
    assert_eq!(all.len(), 21);
    assert_eq!(all.iter().collect::<HashSet<_>>().len(), 21);
    assert_eq!(all[0], V6Reading::RADIAL_ONLY);
    assert!(terms_read(V6Reading::RADIAL_ONLY).is_empty());
    for &v in &all {
        assert!(v.is_valid());
        assert_eq!(reads_each_once(v), Ok(()), "{v:?}");
    }
    // The whole product of 5 tangential and 5 prism readings, less the 4
    // that read d7 and d8 twice, which are exactly the ones left out.
    let tangential: Vec<Option<Tangential>> =
        std::iter::once(None)
            .chain(PairOrder::all().flat_map(|pair| {
                [false, true].map(|extension| Some(Tangential { pair, extension }))
            }))
            .collect();
    let prism: Vec<Option<Prism>> = std::iter::once(None)
        .chain(Prism::all().map(Some))
        .collect();
    let mut left_out = 0;
    for &t in &tangential {
        for &p in &prism {
            let v = V6Reading {
                tangential: t,
                prism: p,
            };
            if !all.contains(&v) {
                left_out += 1;
                assert!(!v.is_valid());
                assert_eq!(reads_each_once(v), Err(7), "{v:?}");
            }
        }
    }
    assert_eq!((tangential.len() * prism.len(), left_out), (25, 4));
    // Control: a reading with an extension and a prism at d7.
    let twice = V6Reading {
        tangential: Some(Tangential {
            pair: PairOrder::D5D6,
            extension: true,
        }),
        prism: Some(Prism {
            at: PrismAt::D7,
            order: PrismOrder::ByAxis,
        }),
    };
    assert_eq!(must_fail("d7 and d8 read twice", reads_each_once(twice)), 7);
}

/// On terms whose d5 to d12 are all non-zero, the 21 readings give 21
/// different canonical forms.
#[test]
fn v6_readings_give_distinct_canonical_forms() {
    let _time = timed("v6_readings_give_distinct_canonical_forms");
    for d in PATS_D {
        assert!(d[5..].iter().all(|&t| t != 0.0));
        assert_eq!(readings_distinct(d), Ok(()));
    }
    // Control: with d5 to d12 all equal, the pair's order can't be told.
    let mut same = PATS_D[0];
    same[5..].fill(0.01);
    must_fail("d5 to d12 all equal", readings_distinct(same));
}

/// `KbLens` round-trips on each base, its tangential and prism terms
/// included.
#[test]
fn kb_lens_unproject_inverts_project() {
    let _time = timed("kb_lens_unproject_inverts_project");
    kb_round_trip_cases();
    // Control: the tangential and prism terms left out of the inverse.
    let lens = KbLens {
        base: Model::Equidistant,
        k: [0.01, -0.002, 0.0, 0.0, 0.0],
        fx: 1000.0,
        fy: 1000.0,
        cx: 1000.0,
        cy: 1000.0,
        p: [1e-3, -1e-3],
        pe: [2e-4, 0.0],
        s: [5e-4, 0.0, -5e-4, 1e-4],
    };
    let bare = KbLens {
        p: [0.0; 2],
        pe: [0.0; 2],
        s: [0.0; 4],
        ..lens
    };
    let d = dir(80f64.to_radians(), 0.5);
    let back = bare.unproject(lens.project(d).unwrap()).unwrap();
    must_fail(
        "the terms left in",
        at_most(angle_between(back, d), ANGLE_TOL),
    );
}

/// `KbLens` applies its base after the polynomial in θ, as
/// `fisheye::Lens::radius` does.
#[test]
fn kb_lens_applies_its_base_after_the_polynomial() {
    let _time = timed("kb_lens_applies_its_base_after_the_polynomial");
    let bases = [
        Model::Equidistant,
        Model::Equisolid,
        Model::Stereographic,
        Model::Orthographic,
    ];
    // F turns sphere's lens frame into Mei's: (x, y, z) to (x, −y, −z),
    // and back.
    let d = DVec3::new(0.3, -0.5, 0.8).normalize();
    assert!((F * d - DVec3::new(d.x, -d.y, -d.z)).length() < 1e-15);
    assert!((F * (F * d) - d).length() < 1e-15);
    // Through F, a KbLens at unit focal is `Lens` with +y down, on every
    // base, k5 aside.
    let k = [0.02, -0.004, 3e-4, -2e-5];
    for base in bases {
        let lens = Lens { model: base, k };
        let kb = KbLens {
            base,
            k: [k[0], k[1], k[2], k[3], 0.0],
            fx: 1.0,
            fy: 1.0,
            cx: 0.0,
            cy: 0.0,
            p: [0.0; 2],
            pe: [0.0; 2],
            s: [0.0; 4],
        };
        for i in 0..=80 {
            for j in 0..12 {
                let (theta, phi) = (f64::from(i).to_radians(), f64::from(j) * 30f64.to_radians());
                let (st, ct) = theta.sin_cos();
                // In sphere's frame: down −Z, +y up.
                let d = DVec3::new(st * phi.cos(), st * phi.sin(), -ct);
                let l = lens.project(d);
                let want = DVec2::new(l.x, -l.y);
                assert!(
                    (kb.project(F * d).unwrap() - want).length() < 1e-13,
                    "{base:?}"
                );
            }
        }
    }
    // With k5 too, worked from the definition: θ_d = θ (1 + k1 θ² + … +
    // k5 θ¹⁰), then r = base(θ_d), along +x at a focal of 2 and a centre
    // of (3, 4).
    let k5 = [0.03, -0.01, 2e-3, -4e-4, 5e-5];
    let theta = 70f64.to_radians();
    let theta_d = theta
        * (1.0
            + k5[0] * theta.powi(2)
            + k5[1] * theta.powi(4)
            + k5[2] * theta.powi(6)
            + k5[3] * theta.powi(8)
            + k5[4] * theta.powi(10));
    let base_first = |base: Model, t: f64| match base {
        Model::Equidistant => t,
        Model::Equisolid => 2.0 * (t / 2.0).sin(),
        Model::Stereographic => 2.0 * (t / 2.0).tan(),
        Model::Orthographic => t.sin(),
    };
    let poly = |t: f64| {
        t * (1.0
            + k5[0] * t.powi(2)
            + k5[1] * t.powi(4)
            + k5[2] * t.powi(6)
            + k5[3] * t.powi(8)
            + k5[4] * t.powi(10))
    };
    for base in bases {
        let kb = KbLens {
            base,
            k: k5,
            fx: 2.0,
            fy: 2.0,
            cx: 3.0,
            cy: 4.0,
            p: [0.0; 2],
            pe: [0.0; 2],
            s: [0.0; 4],
        };
        let px = kb.project(dir(theta, 0.0)).unwrap();
        let r = base_first(base, theta_d);
        assert!(
            (px - DVec2::new(3.0 + 2.0 * r, 4.0)).length() < 1e-13,
            "{base:?}"
        );
        // Control: the base applied before the polynomial, which only the
        // equidistant base, the identity, can't tell.
        if base != Model::Equidistant {
            let before = poly(base_first(base, theta));
            must_fail(
                "the base before the polynomial",
                at_most((px.x - (3.0 + 2.0 * before)).abs(), 1e-9),
            );
        }
    }
    // The base's own fold bounds the limit: θ_d = 90° on an orthographic
    // base, and nothing short of the back axis on an equidistant one.
    let ideal = |base| KbLens {
        base,
        k: [0.0; 5],
        fx: 1.0,
        fy: 1.0,
        cx: 0.0,
        cy: 0.0,
        p: [0.0; 2],
        pe: [0.0; 2],
        s: [0.0; 4],
    };
    assert!((ideal(Model::Orthographic).limit_theta() - FRAC_PI_2).abs() < 1e-12);
    assert_eq!(ideal(Model::Equidistant).limit_theta(), PI);
    assert_eq!(
        ideal(Model::Orthographic).project(dir(91f64.to_radians(), 0.0)),
        None
    );
}
