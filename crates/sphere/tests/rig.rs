//! The lens pair (M3 plan 6.2 and 6.3): the Euler space and its twins,
//! `Rig::build` and its frames, the weights, the stitch distance and the
//! mask. The properties run 100,000 cases in the slice's check (19.3);
//! the small enums go exhaustively and `EulerConv` is sampled.
//!
//! The small enums are swept whole by a loop in each test, with an
//! `EulerConv` stepped through the space beside them, and drawn at random
//! by its property; the hand-worked frames are written here with glam's
//! matrices from 6.2's text, apart from `sphere::rig`'s code. Pat's
//! numbers are docs/formats/insta360.md's ("Pat's X5 samples"), in canvas
//! pixels with block 1's cx local to its square.
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): sphere's properties and rung 0
//! take at most 30 s together at `PROPTEST_CASES=100000`, shared with
//! `unified.rs` and synth's `lens_against_sphere.rs`; each test prints
//! its time and the suite's. `PROPTEST_CASES` sets the count (256 by
//! default). Two properties list or scan far more per case than the
//! rest, and run a fraction of the count: the Euler twins a twentieth,
//! as each case lists 2304 values, and the edge scan a fortieth, as each
//! scans every texel of eight edges (about 2.5 ms a case either way).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::f64::consts::{FRAC_PI_2, PI};
use std::sync::OnceLock;
use std::time::Instant;

use glam::{DMat3, DQuat, DVec2, DVec3};
use proptest::prelude::*;
use spherecut_sphere::fisheye::{
    KbLens, LensModel, Model, PairOrder, Prism, PrismAt, PrismOrder, Tangential, Unified, V6Reading,
};
use spherecut_sphere::rig::{
    Angle, Axis, Binding, CalibTerms, Calibration, CanvasMap, EulerConv, Extrinsics, Generation,
    Geometry, LensCalib, LensTune, QuarterTurn, Rig, RigError, RigLens, Sign, Status, Tune,
    TurnAxis, TzSign, theta_frame,
};
use spherecut_sphere::{angle_between, rotation_angle};
use spherecut_testkit::control::{above, at_least, at_most, must_fail};

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

/// Runs a property `n` times through proptest's runner, for the
/// properties that run a fraction of `PROPTEST_CASES`: the `proptest!`
/// macro would set its count back to the variable's.
fn run_cases<S: Strategy>(n: u32, strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>)
where
    S::Value: std::fmt::Debug,
{
    let config = ProptestConfig {
        cases: n,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    if let Err(e) = proptest::test_runner::TestRunner::new(config).run(&strategy, test) {
        panic!("{e}");
    }
}

/// A degree, in radians.
const DEG: f64 = PI / 180.0;

/// How far apart two directions may be and still be the same: the lens
/// inverse leaves under 5e-11 rad on Pat's numbers (D63).
const SAME_DIR: f64 = 1e-9;

/// How far apart two rotations may be and still be the same.
const SAME_TURN: f64 = 1e-12;

/// A block's square on the X5's canvas, and field 27's window.
const SRC: u32 = 5376;
const DST: u32 = 5312;

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

/// Pat's X5 calibration, both generations, as the camera crate will
/// fill sphere's type: block 1's cx less src.
fn pats() -> Calibration {
    let block = |xi, fx, fy, cx, cy, ypr_deg, t_m, terms| LensCalib {
        xi,
        fx,
        fy,
        cx,
        cy,
        ypr_deg,
        t_m,
        terms,
    };
    let v3 = |k, p| CalibTerms::V3 { k, p };
    Calibration {
        src: SRC,
        dst: DST,
        v3: Some([
            block(
                2.0,
                4284.620,
                4284.280,
                2701.130,
                2692.130,
                [-0.198, 0.211, 90.280],
                [0.0; 3],
                v3(
                    [0.18632051, 2.04984355, -3.24446917],
                    [-0.00143576, -0.00339240],
                ),
            ),
            block(
                2.0,
                4291.030,
                4291.030,
                8069.000 - 5376.0,
                2684.950,
                [0.239, 0.113, 89.107],
                [-0.004460, 0.000017, -0.031920],
                v3(
                    [0.18940836, 2.07073069, -3.24103332],
                    [0.00013831, -0.00299873],
                ),
            ),
        ]),
        v6: Some([
            block(
                2.0,
                4277.520,
                4277.270,
                2713.050,
                2689.210,
                [-0.310, -0.271, 90.255],
                [0.0; 3],
                CalibTerms::V6 { d: PATS_D[0] },
            ),
            block(
                2.0,
                4284.400,
                4284.760,
                8075.730 - 5376.0,
                2680.350,
                [0.120, -0.173, 89.112],
                [0.000637, 0.000001, -0.031525],
                CalibTerms::V6 { d: PATS_D[1] },
            ),
        ]),
    }
}

/// `cal` with each block's stored angles, in both generations, `ypr`'s.
fn with_angles(cal: &Calibration, ypr: [[f64; 3]; 2]) -> Calibration {
    let set = |blocks: Option<[LensCalib; 2]>| {
        blocks.map(|mut b| {
            b[0].ypr_deg = ypr[0];
            b[1].ypr_deg = ypr[1];
            b
        })
    };
    Calibration {
        v3: set(cal.v3),
        v6: set(cal.v6),
        ..*cal
    }
}

/// The geometry's small enums and the front track, which the properties
/// sweep whole.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Small {
    binding: Binding,
    canvas: CanvasMap,
    turn: TurnAxis,
    tz: TzSign,
    upright: QuarterTurn,
    generation: Generation,
    /// V6's reading; V3 reads none, and is listed once.
    v6: V6Reading,
    front_track: u8,
}

/// Every value of the small enums: 2 bindings, 3 canvas maps, 2 turn
/// axes, 2 tz signs, 4 quarter turns, V3 or V6 under each of its 21
/// readings, and 2 front tracks, 4224 in all.
fn smalls() -> &'static [Small] {
    static ALL: OnceLock<Vec<Small>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut all = Vec::new();
        let models = std::iter::once((Generation::V3, V6Reading::RADIAL_ONLY))
            .chain(V6Reading::all().map(|v| (Generation::V6, v)))
            .collect::<Vec<_>>();
        for binding in Binding::all() {
            for canvas in CanvasMap::all() {
                for turn in TurnAxis::all() {
                    for tz in TzSign::all() {
                        for upright in QuarterTurn::all() {
                            for &(generation, v6) in &models {
                                for front_track in 0..2 {
                                    all.push(Small {
                                        binding,
                                        canvas,
                                        turn,
                                        tz,
                                        upright,
                                        generation,
                                        v6,
                                        front_track,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        all
    })
}

/// The 1152 values of the Euler space, in `EulerConv::all`'s order.
fn convs() -> &'static [EulerConv] {
    static ALL: OnceLock<Vec<EulerConv>> = OnceLock::new();
    ALL.get_or_init(|| EulerConv::all().collect())
}

/// The `EulerConv` a sweep pairs with its k-th value: a step through the
/// space coprime to its size, so that a sweep of the small enums meets
/// every value of it.
fn conv_for(k: usize) -> EulerConv {
    convs()[k * 389 % convs().len()]
}

fn geometry(small: Small, conv: EulerConv) -> Geometry {
    Geometry {
        binding: small.binding,
        canvas: small.canvas,
        extrinsics: Extrinsics::Stored(conv),
        turn: small.turn,
        tz: small.tz,
        upright: small.upright,
        generation: small.generation,
        v6: small.v6,
        status: Status::Provisional,
        evidence: String::new(),
    }
}

/// The X5's placeholder geometry (5.3) under a canvas map, a generation
/// and a V6 reading: block 0 on track 0, the placeholder `EulerConv`,
/// turn axis Y, t_1 as stored, upright.
fn placeholder(canvas: CanvasMap, generation: Generation, v6: V6Reading) -> Geometry {
    let small = Small {
        binding: Binding::Direct,
        canvas,
        turn: TurnAxis::Y,
        tz: TzSign::AsStored,
        upright: QuarterTurn::Zero,
        generation,
        v6,
        front_track: 0,
    };
    geometry(small, EulerConv::PLACEHOLDER)
}

/// The rig of `small` and `conv` on `cal`, at `tune`, on tracks of
/// `sides`.
fn build(cal: &Calibration, small: Small, conv: EulerConv, tune: &Tune, sides: [u32; 2]) -> Rig {
    Rig::build(cal, &geometry(small, conv), tune, small.front_track, sides).unwrap()
}

/// The default tune with the stitch distance set.
fn tune_at(q_per_m: f64) -> Tune {
    Tune {
        q_per_m,
        ..Tune::default()
    }
}

/// The lens-frame unit direction θ off the +z axis at azimuth φ.
fn dir(theta: f64, phi: f64) -> DVec3 {
    let (st, ct) = theta.sin_cos();
    DVec3::new(st * phi.cos(), st * phi.sin(), ct)
}

/// θ of a lens-frame direction.
fn off_axis(d: DVec3) -> f64 {
    d.x.hypot(d.y).atan2(d.z)
}

/// The unit direction at height z and azimuth φ: uniform on the sphere
/// for uniform z and φ.
fn on_sphere(z: f64, phi: f64) -> DVec3 {
    let r = (1.0 - z * z).max(0.0).sqrt();
    DVec3::new(r * phi.cos(), r * phi.sin(), z)
}

/// Fourteen rig directions that no rotation but the identity fixes all
/// of: the axes both ways and the eight diagonals.
fn rays() -> [DVec3; 14] {
    let s = 1.0 / 3f64.sqrt();
    let diagonal = |i: u32| {
        let sign = |bit: u32| if i & bit == 0 { s } else { -s };
        DVec3::new(sign(1), sign(2), sign(4))
    };
    [
        DVec3::X,
        DVec3::NEG_X,
        DVec3::Y,
        DVec3::NEG_Y,
        DVec3::Z,
        DVec3::NEG_Z,
        diagonal(0),
        diagonal(1),
        diagonal(2),
        diagonal(3),
        diagonal(4),
        diagonal(5),
        diagonal(6),
        diagonal(7),
    ]
}

/// The quarter turn about +Z of k · 90°.
fn quarter_about_z(k: i32) -> DQuat {
    DQuat::from_rotation_z(f64::from(k) * FRAC_PI_2)
}

/// A tiny xorshift generator, for the seeded rigs and points.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in [lo, hi].
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in 0..n.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// Strategies.

fn small() -> impl Strategy<Value = Small> {
    (0..smalls().len()).prop_map(|i| smalls()[i])
}

fn conv() -> impl Strategy<Value = EulerConv> {
    (0..convs().len()).prop_map(|i| convs()[i])
}

/// A stored angle of at least 1° either way, up to 179°.
fn angle() -> impl Strategy<Value = f64> {
    (1.0f64..=179.0, any::<bool>()).prop_map(|(a, minus)| if minus { -a } else { a })
}

/// Both blocks' stored yaw, pitch and roll, each at least 1°.
fn angles() -> impl Strategy<Value = [[f64; 3]; 2]> {
    [[angle(), angle(), angle()], [angle(), angle(), angle()]]
}

/// A track side: the LRV's half, a 4K, 5.7K or 8K track, or 4.2's.
fn side() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(384u32),
        Just(768),
        Just(832),
        Just(1920),
        Just(2880),
        Just(3840)
    ]
}

/// One lens's tune within `validate`'s bounds (11.2), θ_max from 90° to
/// 110°: yaw, pitch and roll within ±15°, the centre within ±64 canvas
/// px, the radius scale 0.9 to 1.1.
fn lens_tune() -> impl Strategy<Value = LensTune> {
    (
        [-15.0f64..=15.0, -15.0f64..=15.0, -15.0f64..=15.0],
        [-64.0f64..=64.0, -64.0f64..=64.0],
        0.9f64..=1.1,
        90.0f64..=110.0,
    )
        .prop_map(
            |(ypr_deg, centre_px, radius_scale, theta_max_deg)| LensTune {
                ypr_deg,
                centre_px,
                radius_scale,
                xi_delta: 0.0,
                theta_max_deg,
            },
        )
}

/// A seeded lens tune within `validate`'s bounds, as [`lens_tune`] draws.
fn seeded_lens_tune(rng: &mut Rng) -> LensTune {
    LensTune {
        ypr_deg: [0; 3].map(|_| rng.range(-15.0, 15.0)),
        centre_px: [0; 2].map(|_| rng.range(-64.0, 64.0)),
        radius_scale: rng.range(0.9, 1.1),
        xi_delta: 0.0,
        theta_max_deg: rng.range(90.0, 110.0),
    }
}

// The Euler space and its twins.

/// The turn by `deg` degrees about x, y or z, as a matrix.
fn rx(deg: f64) -> DMat3 {
    DMat3::from_rotation_x(deg.to_radians())
}

fn ry(deg: f64) -> DMat3 {
    DMat3::from_rotation_y(deg.to_radians())
}

fn rz(deg: f64) -> DMat3 {
    DMat3::from_rotation_z(deg.to_radians())
}

/// The most a rotation and a matrix differ by, element by element.
fn misfit(q: DQuat, m: DMat3) -> f64 {
    let a = DMat3::from_quat(q).to_cols_array();
    let b = m.to_cols_array();
    a.iter()
        .zip(&b)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max)
}

/// The least angle between any two of `turns`, in radians, over every
/// pair.
fn closest_pair(turns: &[DQuat]) -> f64 {
    let mut least = f64::INFINITY;
    for (i, a) in turns.iter().enumerate() {
        for b in &turns[i + 1..] {
            least = least.min(rotation_angle(*a, *b));
        }
    }
    least
}

/// Generic stored angles: none a quarter turn, nor a quarter turn from
/// another's sum or difference. At a stored roll of 90°, or a yaw of 90°
/// less the roll, values of the space coincide.
const GENERIC: [f64; 3] = [31.0, -13.0, 71.0];

/// How far apart any two of the space's rotations lie at [`GENERIC`]
/// at least, in radians: 3.76° for the closest two.
const APART: f64 = DEG;

/// `EulerConv::all` lists 1152 distinct values, each axes and order a
/// permutation.
#[test]
fn euler_space_has_1152_distinct_values() {
    let _time = timed("euler_space_has_1152_distinct_values");
    let all = convs();
    assert_eq!(all.len(), 1152);
    assert_eq!(all.iter().collect::<HashSet<_>>().len(), 1152);
    for c in all {
        assert_eq!(c.axes.iter().collect::<HashSet<_>>().len(), 3, "{c:?}");
        assert_eq!(c.order.iter().collect::<HashSet<_>>().len(), 3, "{c:?}");
        assert!(c.is_valid(), "{c:?}");
    }
    assert!(all.contains(&EulerConv::PLACEHOLDER));
    // Control: yaw and pitch about one axis is no value of the space.
    must_fail(
        "yaw and pitch about one axis",
        EulerConv {
            axes: [Axis::Y, Axis::Y, Axis::Z],
            ..EulerConv::PLACEHOLDER
        }
        .is_valid(),
    );
    // Their rotations are distinct, at angles that are no quarter turn.
    let turns: Vec<DQuat> = all.iter().map(|c| c.rotation(GENERIC)).collect();
    let least = closest_pair(&turns);
    eprintln!(
        "at {GENERIC:?}° the closest two of the 1152 rotations lie {:.3}° apart",
        least.to_degrees()
    );
    assert!(least >= APART, "{least:e} rad");
    // No direction field: each value's inverse, the order reversed and
    // every sign flipped, the offset kept, is in the space already.
    let flip = |s: Sign| match s {
        Sign::Plus => Sign::Minus,
        Sign::Minus => Sign::Plus,
    };
    for c in all {
        let [a, b, z] = c.order;
        let inverse = EulerConv {
            order: [z, b, a],
            signs: c.signs.map(flip),
            ..*c
        };
        assert!(all.contains(&inverse));
        let err = rotation_angle(inverse.rotation(GENERIC), c.rotation(GENERIC).inverse());
        assert!(err <= SAME_TURN, "{c:?}: {err:e}");
    }
    // Control: with a direction field every rotation is listed twice, and
    // the space isn't distinct.
    let both_ways: Vec<DQuat> = turns.iter().flat_map(|t| [*t, t.inverse()]).collect();
    must_fail(
        "the space with a direction field",
        at_least(closest_pair(&both_ways), APART),
    );
}

/// One hand-worked value of the space: its fields, and its E at
/// (yaw, pitch, roll) = [`GENERIC`] written out from 6.2's text, signs
/// and axes by angle and the offset added to the roll before its sign.
struct Worked {
    conv: EulerConv,
    by_hand: DMat3,
}

fn worked() -> Vec<Worked> {
    use Angle::{Pitch, Roll, Yaw};
    use Axis::{X, Y, Z};
    use Sign::{Minus as M, Plus as P};
    let [y, p, r] = GENERIC;
    let conv = |axes, order, signs, roll_offset| EulerConv {
        axes,
        order,
        signs,
        roll_offset,
    };
    vec![
        // The placeholder: yaw about y, pitch about x, roll about z less
        // 90°, yaw first.
        Worked {
            conv: EulerConv::PLACEHOLDER,
            by_hand: rz(r - 90.0) * rx(p) * ry(y),
        },
        Worked {
            conv: conv([X, Y, Z], [Yaw, Pitch, Roll], [P, P, P], QuarterTurn::Zero),
            by_hand: rz(r) * ry(p) * rx(y),
        },
        // Yaw's sign is minus though yaw is applied last.
        Worked {
            conv: conv([Y, X, Z], [Roll, Pitch, Yaw], [M, P, P], QuarterTurn::Zero),
            by_hand: ry(-y) * rx(p) * rz(r),
        },
        Worked {
            conv: conv(
                [Z, X, Y],
                [Pitch, Roll, Yaw],
                [P, M, P],
                QuarterTurn::Plus90,
            ),
            by_hand: rz(y) * ry(r + 90.0) * rx(-p),
        },
        // A minus roll with an offset: −(roll − 90°).
        Worked {
            conv: conv(
                [X, Z, Y],
                [Roll, Yaw, Pitch],
                [P, P, M],
                QuarterTurn::Minus90,
            ),
            by_hand: rz(p) * rx(y) * ry(-(r - 90.0)),
        },
        Worked {
            conv: conv([Y, Z, X], [Yaw, Roll, Pitch], [M, M, M], QuarterTurn::Half),
            by_hand: rz(-p) * rx(-(r + 180.0)) * ry(-y),
        },
        Worked {
            conv: conv(
                [Z, Y, X],
                [Pitch, Yaw, Roll],
                [P, M, P],
                QuarterTurn::Minus90,
            ),
            by_hand: rx(r - 90.0) * rz(y) * ry(-p),
        },
        Worked {
            conv: conv(
                [X, Y, Z],
                [Roll, Pitch, Yaw],
                [M, P, M],
                QuarterTurn::Plus90,
            ),
            by_hand: rx(-y) * ry(p) * rz(-(r + 90.0)),
        },
    ]
}

/// `EulerConv::rotation` takes each sign with its angle, whatever the
/// order, and adds the roll offset to the stored roll before the roll's
/// sign, as `synth::lens::TruthEuler` reads the labels (amendment 3).
#[test]
fn euler_conv_reads_signs_by_angle_and_offsets_the_roll_first() {
    let _time = timed("euler_conv_reads_signs_by_angle_and_offsets_the_roll_first");
    let worked = worked();
    for w in &worked {
        let err = misfit(w.conv.rotation(GENERIC), w.by_hand);
        assert!(err <= 1e-12, "{:?}: {err:e}", w.conv);
    }
    let [y, p, r] = GENERIC;
    // Control: the roll's sign taken before its offset, −roll − 90° for
    // −(roll − 90°).
    let w = &worked[4];
    let sign_first = rz(p) * rx(y) * ry(-r - 90.0);
    must_fail(
        "the roll's sign before its offset",
        at_most(misfit(w.conv.rotation(GENERIC), sign_first), 1e-12),
    );
    // Control: the signs read by place in the order, not by angle, so
    // that the first applied, the roll, takes the minus.
    let w = &worked[2];
    let by_place = ry(y) * rx(p) * rz(-r);
    must_fail(
        "the signs read by place",
        at_most(misfit(w.conv.rotation(GENERIC), by_place), 1e-12),
    );
}

/// `EulerConv::PLACEHOLDER` is R_z(roll − 90°) · R_x(pitch) · R_y(yaw),
/// within 1° of the identity on Pat's V3 and V6 angles, and puts block
/// 0's lens along −Z (5.3).
#[test]
fn euler_placeholder_is_near_the_identity_on_pats_angles() {
    let _time = timed("euler_placeholder_is_near_the_identity_on_pats_angles");
    let cal = pats();
    let mut widest = 0.0f64;
    for blocks in [cal.v3.unwrap(), cal.v6.unwrap()] {
        for block in blocks {
            let [y, p, r] = block.ypr_deg;
            let e = EulerConv::PLACEHOLDER.rotation(block.ypr_deg);
            assert!(misfit(e, rz(r - 90.0) * rx(p) * ry(y)) <= 1e-12);
            widest = widest.max(rotation_angle(e, DQuat::IDENTITY));
        }
    }
    eprintln!(
        "on Pat's angles the placeholder's E_b lies within {:.3}° of the identity",
        widest.to_degrees()
    );
    assert!(widest <= DEG);
    for generation in Generation::all() {
        for canvas in CanvasMap::all() {
            let geo = placeholder(canvas, generation, V6Reading::RADIAL_ONLY);
            let rig = Rig::build(&cal, &geo, &Tune::default(), 0, [768; 2]).unwrap();
            let axis = |b: usize| rig.lenses[b].lens_from_rig.inverse() * DVec3::Z;
            assert!(
                angle_between(axis(0), DVec3::NEG_Z) <= DEG,
                "{generation:?}"
            );
            assert!(angle_between(axis(1), DVec3::Z) <= DEG, "{generation:?}");
        }
    }
    // Control: without the offset the stored quarter turn stays.
    let no_offset = EulerConv {
        roll_offset: QuarterTurn::Zero,
        ..EulerConv::PLACEHOLDER
    };
    let e = no_offset.rotation(cal.v6.unwrap()[0].ypr_deg);
    must_fail(
        "the placeholder without its offset",
        at_most(rotation_angle(e, DQuat::IDENTITY), DEG),
    );
}

/// One value of stage G's listing (8.4): a turn axis and an `EulerConv`,
/// with each lens's lens_from_rig without S, E_b · F · T_b.
#[derive(Debug, Clone, Copy)]
struct Listed {
    turn: TurnAxis,
    conv: EulerConv,
    l: [DQuat; 2],
}

impl Listed {
    /// Where lens b's optical axis points, in the rig frame.
    fn axis(&self, b: usize) -> DVec3 {
        self.l[b].inverse() * DVec3::Z
    }

    /// Whether stage G keeps it: block 0's lens within 5° of −Z.
    fn kept(&self) -> bool {
        angle_between(self.axis(0), DVec3::NEG_Z) <= 5.0 * DEG
    }

    /// Whether the lenses look within 10° of opposite ways.
    fn back_to_back(&self) -> bool {
        angle_between(self.axis(0), -self.axis(1)) <= 10.0 * DEG
    }

    /// The relative rotation the seam sees, lens_from_rig_1 ·
    /// rig_from_lens_0, which a common rotation leaves as it is.
    fn relative(&self) -> DQuat {
        (self.l[1] * self.l[0].inverse()).normalize()
    }

    /// Whether it has the near twins' shape (6.2): the roll applied
    /// second, about x with turn axis X or about y with turn axis Y, and
    /// an offset of 0° or 180°.
    fn near_twin_shape(&self) -> bool {
        let c = &self.conv;
        c.order[1] == Angle::Roll
            && matches!(
                (c.axes[2], self.turn),
                (Axis::X, TurnAxis::X) | (Axis::Y, TurnAxis::Y)
            )
            && matches!(c.roll_offset, QuarterTurn::Zero | QuarterTurn::Half)
    }
}

/// Stage G's 2304 values for both blocks' stored angles, written from
/// 6.2's text with glam's matrices: E_b · F · T_b, with F = diag(1, −1,
/// −1) and T_1 the half turn about the turn axis.
fn listing(ypr: [[f64; 3]; 2]) -> Vec<Listed> {
    let f = DMat3::from_diagonal(DVec3::new(1.0, -1.0, -1.0));
    let mut list = Vec::with_capacity(2 * convs().len());
    for turn in TurnAxis::all() {
        let t1 = match turn {
            TurnAxis::Y => DMat3::from_rotation_y(PI),
            TurnAxis::X => DMat3::from_rotation_x(PI),
        };
        for &conv in convs() {
            let e = |b: usize| DMat3::from_quat(conv.rotation(ypr[b]));
            let l = [e(0) * f, e(1) * f * t1].map(|m| DQuat::from_mat3(&m));
            list.push(Listed { turn, conv, l });
        }
    }
    list
}

/// How close two relative rotations lie to be the same, in radians.
const TWIN: f64 = 1e-9;

/// What a listing comes to (6.2).
#[derive(Debug)]
struct Census {
    /// The values stage G keeps.
    kept: usize,
    /// The twin groups among them: values with one relative rotation.
    groups: usize,
    /// Dropped values that are exact twins of a kept one.
    twins: usize,
    /// Dropped values that don't put the lenses back to back.
    apart: usize,
    /// The rest of the dropped values, the near twins.
    near: Vec<Listed>,
    /// The kept values' relative rotations.
    kept_relative: Vec<DQuat>,
}

impl Census {
    /// The near twins' least and greatest distance to their closest kept
    /// value, in radians.
    fn near_gap(&self) -> (f64, f64) {
        let gap = |v: &Listed| {
            self.kept_relative
                .iter()
                .map(|r| rotation_angle(*r, v.relative()))
                .fold(f64::INFINITY, f64::min)
        };
        self.near
            .iter()
            .map(gap)
            .fold((f64::INFINITY, 0.0f64), |(lo, hi), g| {
                (lo.min(g), hi.max(g))
            })
    }
}

/// The census of a listing: stage G's filter, or none, and the kept
/// values' twin groups, which fail it unless every two members differ by
/// a quarter turn about Z.
fn census(list: &[Listed], filter: bool) -> Result<Census, String> {
    // A sort key that a rotation and its negated quaternion share.
    let key = |r: DQuat| DMat3::from_quat(r).x_axis.x;
    let (kept, dropped): (Vec<&Listed>, Vec<&Listed>) =
        list.iter().partition(|v| !filter || v.kept());
    let mut sorted: Vec<(f64, DQuat, usize)> = kept
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let r = v.relative();
            (key(r), r, i)
        })
        .collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let twins_of = |r: DQuat| -> Vec<usize> {
        let k = key(r);
        let from = sorted.partition_point(|e| e.0 < k - 1e-8);
        sorted[from..]
            .iter()
            .take_while(|e| e.0 <= k + 1e-8)
            .filter(|e| rotation_angle(e.1, r) <= TWIN)
            .map(|e| e.2)
            .collect()
    };
    let mut groups = 0;
    for (i, v) in kept.iter().enumerate() {
        let twins = twins_of(v.relative());
        if twins.iter().min() == Some(&i) {
            groups += 1;
        }
        for &j in &twins {
            let common = v.l[0].inverse() * kept[j].l[0];
            if !(0..4).any(|k| rotation_angle(common, quarter_about_z(k)) <= TWIN) {
                return Err(format!(
                    "{:?} under turn {:?} and {:?} under turn {:?} differ by {:.3}° about {:?}, \
                     no quarter turn about Z",
                    v.conv,
                    v.turn,
                    kept[j].conv,
                    kept[j].turn,
                    rotation_angle(common, DQuat::IDENTITY).to_degrees(),
                    common.to_axis_angle().0
                ));
            }
        }
    }
    let (mut twins, mut apart, mut near) = (0, 0, Vec::new());
    for v in dropped {
        if !twins_of(v.relative()).is_empty() {
            twins += 1;
        } else if !v.back_to_back() {
            apart += 1;
        } else {
            near.push(*v);
        }
    }
    Ok(Census {
        kept: kept.len(),
        groups,
        twins,
        apart,
        near,
        kept_relative: sorted.iter().map(|e| e.1).collect(),
    })
}

/// 6.2's counts for each binding: 1152 kept, and of the 1152 dropped,
/// 640 exact twins, 384 not back to back and 128 near twins of the
/// shape 6.2 gives, none an exact twin. `Err` names the first count that
/// differs.
fn holds_the_counts(c: &Census) -> Result<(), String> {
    let counts = (c.kept, c.twins, c.apart, c.near.len());
    if counts != (1152, 640, 384, 128) {
        return Err(format!("kept, twins, apart, near: {counts:?}"));
    }
    if let Some(v) = c.near.iter().find(|v| !v.near_twin_shape()) {
        return Err(format!("a near twin of another shape: {v:?}"));
    }
    Ok(())
}

/// X5-like stored angles: each block's yaw and pitch 0.1° to 1.3° either
/// way and its roll as far either way of 90°, the six magnitudes at least
/// 0.06° apart. Values of the space can meet by a coincidence of the draw
/// where two magnitudes meet: with the yaws opposite and the pitches
/// equal, a pitch's sign is a common turn.
fn x5_angles() -> impl Strategy<Value = [[f64; 3]; 2]> {
    use proptest::array::{uniform5, uniform6};
    (
        0.1f64..=0.3,
        uniform5(0.06f64..=0.2),
        0usize..720,
        uniform6(any::<bool>()),
    )
        .prop_map(|(base, gaps, order, minus)| {
            let mut rising = vec![base];
            for g in gaps {
                rising.push(rising[rising.len() - 1] + g);
            }
            // The order-th of the 720 orders of the six, by its factorial
            // digits.
            let mut k = order;
            let mut m = [0.0; 6];
            for (i, slot) in m.iter_mut().enumerate() {
                let rest: usize = (1..6 - i).product();
                *slot = rising.remove(k / rest);
                k %= rest;
            }
            let a = |i: usize| if minus[i] { -m[i] } else { m[i] };
            [[a(0), a(1), 90.0 + a(2)], [a(3), a(4), 90.0 + a(5)]]
        })
}

/// The twins property at sampled angles, a twentieth of `PROPTEST_CASES`
/// (at least 16).
fn euler_twins_cases() {
    run_cases((cases() / 20).max(16), x5_angles(), |ypr| {
        let c = census(&listing(ypr), true);
        prop_assert!(c.is_ok(), "{}", c.unwrap_err());
        let counts = holds_the_counts(&c.unwrap());
        prop_assert!(counts.is_ok(), "{}", counts.unwrap_err());
        Ok(())
    });
}

/// The twin groups for sampled stored angles, once stage G's filter keeps
/// only values that put block 0's lens within 5° of −Z, differ by common
/// rotations that are exactly the quarter turns about Z (6.2). Each
/// binding drops 640 exact twins, 384 values that don't put the lenses
/// back to back and 128 near twins. The sampled angles run a twentieth of
/// `PROPTEST_CASES` (at least 16), as each lists 2304 values.
#[test]
fn euler_twins_are_quarter_turns() {
    let _time = timed("euler_twins_are_quarter_turns");
    let cal = pats();
    for (name, blocks) in [("V3", cal.v3.unwrap()), ("V6", cal.v6.unwrap())] {
        let ypr = [blocks[0].ypr_deg, blocks[1].ypr_deg];
        let c = census(&listing(ypr), true).unwrap();
        holds_the_counts(&c).unwrap();
        let gap = c.near_gap();
        assert!(gap.0 > 1e-6);
        eprintln!(
            "Pat's {name} angles: {} kept in {} twin groups; dropped {} twins, {} apart, {} near \
             twins {:.3}° to {:.3}° from the closest kept",
            c.kept,
            c.groups,
            c.twins,
            c.apart,
            c.near.len(),
            gap.0.to_degrees(),
            gap.1.to_degrees()
        );
    }
    // Through `Rig::build`, for each binding: front track 0 is block 1's
    // under `Reversed`, so S is the half turn about +Y there, which the
    // listing strips.
    let ypr = [cal.v6.unwrap()[0].ypr_deg, cal.v6.unwrap()[1].ypr_deg];
    let list = listing(ypr);
    for binding in Binding::all() {
        let s = match binding {
            Binding::Direct => DQuat::IDENTITY,
            Binding::Reversed => DQuat::from_rotation_y(PI),
        };
        let built: Vec<Listed> = list
            .iter()
            .map(|v| {
                let small = Small {
                    binding,
                    turn: v.turn,
                    generation: Generation::V6,
                    ..smalls()[0]
                };
                let rig = build(&cal, small, v.conv, &Tune::default(), [768; 2]);
                let l = [0, 1].map(|b| rig.lenses[b].lens_from_rig * s.inverse());
                for (built, listed) in l.iter().zip(&v.l) {
                    assert!(
                        rotation_angle(*built, *listed) <= SAME_TURN,
                        "{binding:?} {v:?}"
                    );
                }
                Listed { l, ..*v }
            })
            .collect();
        holds_the_counts(&census(&built, true).unwrap()).unwrap();
    }
    euler_twins_cases();
    // Control: without stage G's filter, twins differ by turns that put
    // lens 0 behind or sideways, no quarter turn about Z.
    let why = must_fail("stage G's filter off", census(&list, false));
    eprintln!("without the filter: {why}");
}

// `Rig::build` and its frames.

/// The most unproject(texel(d)) is off d, in radians, over the lenses
/// whose θ_max reaches d: infinite where either step has no answer, 0
/// where no lens reaches d.
fn round_trip(rig: &Rig, d: DVec3) -> f64 {
    let mut worst = 0.0f64;
    for b in 0..2 {
        if off_axis(rig.ray(b, d)) > rig.lenses[b].theta_max {
            continue;
        }
        let back = rig.texel(b, d).and_then(|px| rig.unproject(b, px));
        worst = worst.max(back.map_or(f64::INFINITY, |back| angle_between(back, d)));
    }
    worst
}

/// Lens b's frame as the rig's rays show it at q = 0: its columns are
/// where the rig's +X, +Y and +Z land in the lens frame.
fn frame_of(rig: &Rig, b: usize) -> DMat3 {
    DMat3::from_cols(
        rig.ray(b, DVec3::X),
        rig.ray(b, DVec3::Y),
        rig.ray(b, DVec3::Z),
    )
}

/// How far a matrix is from a proper rotation: the larger of |det − 1|
/// and the most MᵀM differs from the identity.
fn improper(m: DMat3) -> f64 {
    let gram = (m.transpose() * m - DMat3::IDENTITY).to_cols_array();
    let off = gram.iter().fold(0.0f64, |a, x| a.max(x.abs()));
    (m.determinant() - 1.0).abs().max(off)
}

/// The most two rigs' texels differ by, over [`rays`] and both blocks:
/// infinite where one has a texel and the other none.
fn texel_gap(a: &Rig, b: &Rig) -> f64 {
    let mut gap = 0.0f64;
    for d in rays() {
        for lens in 0..2 {
            gap = gap.max(match (a.texel(lens, d), b.texel(lens, d)) {
                (Some(p), Some(q)) => (p - q).length(),
                (None, None) => 0.0,
                _ => f64::INFINITY,
            });
        }
    }
    gap
}

/// How far apart in texel two values of a field must lie to differ, in
/// pixels: the same code on the same numbers gives the same bits.
const DIFFER: f64 = 1e-6;

/// Whether two lenses are the same in everything but their track.
fn same_but_track(a: &RigLens, b: &RigLens) -> bool {
    a.model == b.model
        && a.lens_from_rig == b.lens_from_rig
        && a.centre_m == b.centre_m
        && a.theta_max == b.theta_max
        && a.theta_frame == b.theta_frame
        && a.side == b.side
        && a.limit_theta == b.limit_theta
        && a.texels_per_radian == b.texels_per_radian
        && a.band_texels_per_radian == b.band_texels_per_radian
}

/// The binding flipped with the media's numbers by track, the front
/// track and the sides, flipped with it, so that the same blocks face
/// the same way on slots of the same size.
fn flipped(small: Small, sides: [u32; 2]) -> (Small, [u32; 2]) {
    let binding = match small.binding {
        Binding::Direct => Binding::Reversed,
        Binding::Reversed => Binding::Direct,
    };
    (
        Small {
            binding,
            front_track: 1 - small.front_track,
            ..small
        },
        [sides[1], sides[0]],
    )
}

/// Whether `b` is `a` with the tracks swapped and nothing else.
fn swaps_the_tracks_only(a: &Rig, b: &Rig) -> bool {
    (0..2).all(|i| {
        b.lenses[i].track == 1 - a.lenses[i].track && same_but_track(&a.lenses[i], &b.lenses[i])
    }) && (a.q, a.feather, a.margin_texels) == (b.q, b.feather, b.margin_texels)
}

/// The fields of the geometry, and the front track, for the
/// distinctness property.
const FIELDS: [&str; 10] = [
    "binding",
    "canvas",
    "turn",
    "tz",
    "upright",
    "generation",
    "v6",
    "front track",
    "Euler",
    "extrinsics",
];

/// `small` and `ext` with field `field` changed to another of its values,
/// chosen by `pick`. The V6 reading changes only under V6, which the
/// caller sets.
fn changed(small: Small, ext: Extrinsics, field: usize, pick: usize) -> (Small, Extrinsics) {
    let other = |all: Vec<CanvasMap>| all[pick % all.len()];
    let mut s = small;
    let mut e = ext;
    match field {
        0 => s.binding = flipped(small, [1, 1]).0.binding,
        1 => {
            s.canvas = other(
                [
                    CanvasMap::A,
                    CanvasMap::B,
                    CanvasMap::C,
                    CanvasMap::Fitted { scale: 1.05 },
                ]
                .into_iter()
                .filter(|&m| m != small.canvas)
                .collect(),
            )
        }
        2 => {
            s.turn = match small.turn {
                TurnAxis::Y => TurnAxis::X,
                TurnAxis::X => TurnAxis::Y,
            }
        }
        3 => {
            s.tz = match small.tz {
                TzSign::AsStored => TzSign::Negated,
                TzSign::Negated => TzSign::AsStored,
            }
        }
        4 => {
            let others: Vec<_> = QuarterTurn::all().filter(|&u| u != small.upright).collect();
            s.upright = others[pick % 3];
        }
        5 => {
            s.generation = match small.generation {
                Generation::V3 => Generation::V6,
                Generation::V6 => Generation::V3,
            }
        }
        6 => {
            let others: Vec<_> = V6Reading::all().filter(|&v| v != small.v6).collect();
            s.v6 = others[pick % others.len()];
        }
        7 => s.front_track = 1 - small.front_track,
        8 => {
            if let Extrinsics::Stored(conv) = ext {
                let i = convs().iter().position(|&c| c == conv).unwrap();
                e = Extrinsics::Stored(convs()[(i + 1 + pick % 1151) % 1152]);
            }
        }
        _ => {
            // A measured pair about 1° off Pat's, by `pick`.
            let tilt = DQuat::from_axis_angle(
                on_sphere(0.3, pick as f64),
                (0.5 + (pick % 7) as f64 / 7.0) * DEG,
            );
            e = Extrinsics::Fitted {
                relative: tilt * DQuat::from_rotation_y(PI),
                translation_m: DVec3::new(0.000637, 0.000001, -0.031525),
            };
        }
    }
    (s, e)
}

/// The rig of `small` under extrinsics `ext`.
fn build_ext(
    cal: &Calibration,
    small: Small,
    ext: Extrinsics,
    tune: &Tune,
    sides: [u32; 2],
) -> Rig {
    let geo = Geometry {
        extrinsics: ext,
        ..geometry(small, EulerConv::PLACEHOLDER)
    };
    Rig::build(cal, &geo, tune, small.front_track, sides).unwrap()
}

/// How far apart in texel the rigs of a value and of the same value
/// with one field changed lie.
fn field_gap(
    cal: &Calibration,
    small: Small,
    ext: Extrinsics,
    field: usize,
    pick: usize,
    tune: &Tune,
    sides: [u32; 2],
) -> f64 {
    let small = if field == 6 {
        Small {
            generation: Generation::V6,
            ..small
        }
    } else {
        small
    };
    let (other, other_ext) = changed(small, ext, field, pick);
    texel_gap(
        &build_ext(cal, small, ext, tune, sides),
        &build_ext(cal, other, other_ext, tune, sides),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn texel_and_unproject_invert_cases(
        small in small(),
        conv in conv(),
        ypr in angles(),
        lenses in [lens_tune(), lens_tune()],
        sides in [side(), side()],
        z in -1.0f64..=1.0,
        phi in -PI..PI,
    ) {
        let cal = with_angles(&pats(), ypr);
        let tune = Tune { lenses, ..tune_at(0.0) };
        let rig = build(&cal, small, conv, &tune, sides);
        let err = round_trip(&rig, on_sphere(z, phi));
        prop_assert!(err <= SAME_DIR, "{err:e} rad");
    }

    fn proper_rotation_cases(
        small in small(),
        conv in conv(),
        ypr in angles(),
        lenses in [lens_tune(), lens_tune()],
    ) {
        let cal = with_angles(&pats(), ypr);
        let rig = build(&cal, small, conv, &Tune { lenses, ..tune_at(0.0) }, [768; 2]);
        for b in 0..2 {
            let off = improper(frame_of(&rig, b));
            prop_assert!(off <= 1e-12, "lens {b}: {off:e}");
        }
    }

    fn fields_differ_cases(
        small in small(),
        conv in conv(),
        ypr in angles(),
        field in 0..FIELDS.len(),
        pick in 0usize..1 << 20,
        sides in [side(), side()],
    ) {
        let cal = with_angles(&pats(), ypr);
        let gap = field_gap(&cal, small, Extrinsics::Stored(conv), field, pick, &Tune::default(), sides);
        prop_assert!(gap > DIFFER, "{}: {gap:e} px", FIELDS[field]);
    }

    fn canvas_maps_invert_cases(
        x in 0.0f64..=5376.0,
        y in 0.0f64..=5376.0,
        side in side(),
        map in 0usize..4,
        scale in 0.9f64..=1.1,
    ) {
        let map = [CanvasMap::A, CanvasMap::B, CanvasMap::C, CanvasMap::Fitted { scale }][map];
        let p = DVec2::new(x, y);
        let back = map.to_canvas(map.to_slot(p, SRC, DST, side), SRC, DST, side);
        prop_assert!((back - p).length() <= 1e-9 * f64::from(SRC), "{map:?}: {p:?} came back {back:?}");
        let px = p * f64::from(side) / f64::from(SRC);
        let again = map.to_slot(map.to_canvas(px, SRC, DST, side), SRC, DST, side);
        prop_assert!((again - px).length() <= 1e-9 * f64::from(side), "{map:?}: {px:?} came back {again:?}");
    }

    fn binding_flip_cases(
        small in small(),
        conv in conv(),
        ypr in angles(),
        lenses in [lens_tune(), lens_tune()],
        sides in [side(), side()],
    ) {
        let cal = with_angles(&pats(), ypr);
        let tune = Tune { lenses, ..Tune::default() };
        let (other, other_sides) = flipped(small, sides);
        let a = build(&cal, small, conv, &tune, sides);
        let b = build(&cal, other, conv, &tune, other_sides);
        prop_assert!(swaps_the_tracks_only(&a, &b), "{small:?}");
    }
}

/// At q = 0, unproject(texel(d)) == d for every value of the small enums
/// and sampled `EulerConv`s, the turn included.
#[test]
fn rig_texel_and_unproject_invert_at_q_zero() {
    let _time = timed("rig_texel_and_unproject_invert_at_q_zero");
    let cal = pats();
    for (k, &small) in smalls().iter().enumerate() {
        let rig = build(&cal, small, conv_for(k), &tune_at(0.0), [768, 832]);
        for d in rays() {
            let err = round_trip(&rig, d);
            assert!(err <= SAME_DIR, "{small:?}: {err:e} rad at {d}");
        }
    }
    texel_and_unproject_invert_cases();
    // Control: the texel of a rig with turn axis Y unprojected through
    // the same rig with turn axis X.
    let small = smalls()[0];
    let y = build(&cal, small, EulerConv::PLACEHOLDER, &tune_at(0.0), [768; 2]);
    let x = build(
        &cal,
        Small {
            turn: TurnAxis::X,
            ..small
        },
        EulerConv::PLACEHOLDER,
        &tune_at(0.0),
        [768; 2],
    );
    let d = DVec3::new(0.3, 0.4, 0.8).normalize();
    let back = x.unproject(1, y.texel(1, d).unwrap()).unwrap();
    must_fail(
        "the turn axis swapped for unproject",
        at_most(angle_between(back, d), SAME_DIR),
    );
}

/// det(lens_from_rig) = +1 for every lens of every rig.
#[test]
fn rig_lens_from_rig_is_a_proper_rotation() {
    let _time = timed("rig_lens_from_rig_is_a_proper_rotation");
    let cal = pats();
    for (k, &small) in smalls().iter().enumerate() {
        let rig = build(&cal, small, conv_for(k), &tune_at(0.0), [768; 2]);
        for b in 0..2 {
            assert!(improper(frame_of(&rig, b)) <= 1e-12, "{small:?}");
        }
    }
    proper_rotation_cases();
    // Control: the frame with its z mirrored, diag(1, 1, −1).
    let rig = build(
        &cal,
        smalls()[0],
        EulerConv::PLACEHOLDER,
        &tune_at(0.0),
        [768; 2],
    );
    let mirror = DMat3::from_diagonal(DVec3::new(1.0, 1.0, -1.0));
    must_fail(
        "a mirrored frame",
        at_most(improper(mirror * frame_of(&rig, 0)), 1e-12),
    );
}

/// Two different values of any field differ in texel for some ray at
/// stored angles of at least 1°, all 21 `V6Reading` values among them on
/// a calibration whose d5 to d12 are all non-zero.
#[test]
fn rig_values_of_every_field_differ_in_texel() {
    let _time = timed("rig_values_of_every_field_differ_in_texel");
    let cal = pats();
    assert!(PATS_D.iter().all(|d| d[5..].iter().all(|&t| t != 0.0)));
    let tune = Tune::default();
    // Every pair of the 21 readings, of the quarter turns and of the
    // canvas maps, on Pat's V6 under the placeholder.
    let base = Small {
        generation: Generation::V6,
        ..smalls()[0]
    };
    let rig = |small: Small| build(&cal, small, EulerConv::PLACEHOLDER, &tune, [768; 2]);
    let readings: Vec<V6Reading> = V6Reading::all().collect();
    assert_eq!(readings.len(), 21);
    let mut least = f64::INFINITY;
    for (i, &a) in readings.iter().enumerate() {
        for &b in &readings[i + 1..] {
            let gap = texel_gap(&rig(Small { v6: a, ..base }), &rig(Small { v6: b, ..base }));
            assert!(gap > DIFFER, "{a:?} and {b:?}: {gap:e} px");
            least = least.min(gap);
        }
    }
    eprintln!("the 21 readings differ pairwise by at least {least:.4} px at 768²");
    let uprights: Vec<QuarterTurn> = QuarterTurn::all().collect();
    for (i, &a) in uprights.iter().enumerate() {
        for &b in &uprights[i + 1..] {
            let gap = texel_gap(
                &rig(Small { upright: a, ..base }),
                &rig(Small { upright: b, ..base }),
            );
            assert!(gap > DIFFER, "{a:?} and {b:?}");
        }
    }
    let maps = [
        CanvasMap::A,
        CanvasMap::B,
        CanvasMap::C,
        CanvasMap::Fitted { scale: 0.95 },
        CanvasMap::Fitted { scale: 1.05 },
    ];
    for (i, &a) in maps.iter().enumerate() {
        for &b in &maps[i + 1..] {
            let gap = texel_gap(
                &rig(Small { canvas: a, ..base }),
                &rig(Small { canvas: b, ..base }),
            );
            assert!(gap > DIFFER, "{a:?} and {b:?}");
        }
    }
    // Every value of the small enums, each two-valued field flipped, at
    // stored angles of 1° or more.
    let cal = with_angles(&cal, [[3.0, -7.0, 95.0], [-2.0, 11.0, 84.0]]);
    for (k, &small) in smalls().iter().enumerate() {
        let ext = Extrinsics::Stored(conv_for(k));
        for field in [0, 2, 3, 5, 7] {
            let gap = field_gap(&cal, small, ext, field, k, &tune, [768, 832]);
            assert!(gap > DIFFER, "{small:?}: {}: {gap:e} px", FIELDS[field]);
        }
    }
    fields_differ_cases();
    // Control: at q = 0 the tz sign moves nothing.
    let small = smalls()[0];
    let gap = field_gap(
        &cal,
        small,
        Extrinsics::Stored(EulerConv::PLACEHOLDER),
        3,
        0,
        &tune_at(0.0),
        [768; 2],
    );
    must_fail("tz at q = 0", above(gap, DIFFER));
    // Control: at a stored roll of exactly 90°, a minus roll with the
    // −90° offset is the plus one: the property needs angles off the
    // quarter turns.
    let quarter = with_angles(&cal, [[3.0, -7.0, 90.0], [-2.0, 11.0, 90.0]]);
    let minus = EulerConv {
        signs: [Sign::Plus, Sign::Plus, Sign::Minus],
        ..EulerConv::PLACEHOLDER
    };
    let gap = texel_gap(
        &build(&quarter, small, EulerConv::PLACEHOLDER, &tune, [768; 2]),
        &build(&quarter, small, minus, &tune, [768; 2]),
    );
    must_fail(
        "the roll's sign at a stored roll of 90°",
        above(gap, DIFFER),
    );
}

/// Canvas maps A, B and C invert.
#[test]
fn rig_canvas_maps_invert() {
    let _time = timed("rig_canvas_maps_invert");
    // The built models against 6.2's table, written out, with a shift of
    // the centre, a radius scale and a ξ delta in the tune.
    let cal = pats();
    let lens = LensTune {
        centre_px: [12.5, -40.0],
        radius_scale: 1.04,
        xi_delta: 0.01,
        ..LensTune::default()
    };
    let tune = Tune {
        lenses: [lens; 2],
        ..Tune::default()
    };
    let (src, dst) = (f64::from(SRC), f64::from(DST));
    for map in [
        CanvasMap::A,
        CanvasMap::B,
        CanvasMap::C,
        CanvasMap::Fitted { scale: 0.97 },
    ] {
        for side in [768u32, 832, 3840] {
            let s = f64::from(side);
            let small = Small {
                canvas: map,
                generation: Generation::V6,
                ..smalls()[0]
            };
            let rig = Rig::build(
                &cal,
                &geometry(small, EulerConv::PLACEHOLDER),
                &tune,
                0,
                [side; 2],
            )
            .unwrap();
            for b in 0..2 {
                let block = cal.v6.unwrap()[b];
                let c = DVec2::new(block.cx, block.cy) + DVec2::from(lens.centre_px);
                let (focal, centre) = match map {
                    CanvasMap::A => (s / dst, c * (s / src)),
                    CanvasMap::B => (s / dst, (c - DVec2::splat((src - dst) / 2.0)) * (s / dst)),
                    CanvasMap::C => (s / src, c * (s / src)),
                    CanvasMap::Fitted { scale } => (s / src * scale, c * (s / src)),
                };
                let LensModel::Mei(u) = rig.lenses[b].model else {
                    panic!("not Mei")
                };
                let want = [
                    block.fx * focal * lens.radius_scale,
                    block.fy * focal * lens.radius_scale,
                    centre.x,
                    centre.y,
                    block.xi + lens.xi_delta,
                ];
                let got = [u.fx, u.fy, u.cx, u.cy, u.xi];
                for (w, g) in want.iter().zip(&got) {
                    assert!(
                        (w - g).abs() <= 1e-9 * s,
                        "{map:?} at {side}: {want:?} {got:?}"
                    );
                }
            }
        }
    }
    canvas_maps_invert_cases();
    // Control: map B's inverse taken of map A's output.
    let p = DVec2::new(2713.05, 2689.21);
    let back = CanvasMap::B.to_canvas(CanvasMap::A.to_slot(p, SRC, DST, 768), SRC, DST, 768);
    must_fail(
        "B's inverse of A's output",
        at_most((back - p).length(), 1e-9 * f64::from(SRC)),
    );
}

/// Flipping the binding swaps the tracks and nothing else.
#[test]
fn rig_binding_flip_swaps_the_tracks_and_nothing_else() {
    let _time = timed("rig_binding_flip_swaps_the_tracks_and_nothing_else");
    let cal = pats();
    for (k, &small) in smalls().iter().enumerate() {
        let (other, sides) = flipped(small, [768, 832]);
        let a = build(&cal, small, conv_for(k), &Tune::default(), [768, 832]);
        let b = build(&cal, other, conv_for(k), &Tune::default(), sides);
        assert!(swaps_the_tracks_only(&a, &b), "{small:?}");
    }
    binding_flip_cases();
    // Control: the turn axis flipped along with the binding.
    let small = smalls()[0];
    let (other, sides) = flipped(small, [768; 2]);
    let a = build(
        &cal,
        small,
        EulerConv::PLACEHOLDER,
        &Tune::default(),
        [768; 2],
    );
    let b = build(
        &cal,
        Small {
            turn: TurnAxis::X,
            ..other
        },
        EulerConv::PLACEHOLDER,
        &Tune::default(),
        sides,
    );
    must_fail(
        "the turn flipped with the binding",
        swaps_the_tracks_only(&a, &b),
    );
}

/// The most two rigs' rays differ by over [`rays`], in radians.
fn ray_gap(a: &Rig, b: &Rig) -> f64 {
    let mut gap = 0.0f64;
    for d in rays() {
        for lens in 0..2 {
            gap = gap.max(angle_between(a.ray(lens, d), b.ray(lens, d)));
        }
    }
    gap
}

/// `TzSign::Negated` flips all of t_1, so a rig built with it at q is
/// the `AsStored` rig at −q, ray for ray.
#[test]
fn rig_tz_negated_is_as_stored_at_negative_q() {
    let _time = timed("rig_tz_negated_is_as_stored_at_negative_q");
    let cal = pats();
    let mut rng = Rng(0x7a5f_1e55);
    for (k, &small) in smalls().iter().enumerate() {
        if small.tz != TzSign::AsStored {
            continue;
        }
        let q = rng.range(0.1, 3.34);
        let negated = Small {
            tz: TzSign::Negated,
            ..small
        };
        let a = build(&cal, negated, conv_for(k), &tune_at(q), [768; 2]);
        let b = build(&cal, small, conv_for(k), &tune_at(-q), [768; 2]);
        assert!(ray_gap(&a, &b) <= 1e-15, "{small:?} at {q}");
    }
    // Control: z alone flipped in the stored t_1, the reflection 6.2's
    // sign isn't.
    let small = smalls()[0];
    let mut z_only = cal;
    if let Some(blocks) = z_only.v3.as_mut() {
        blocks[1].t_m[2] = -blocks[1].t_m[2];
    }
    let a = build(
        &z_only,
        small,
        EulerConv::PLACEHOLDER,
        &tune_at(2.0),
        [768; 2],
    );
    let b = build(
        &cal,
        small,
        EulerConv::PLACEHOLDER,
        &tune_at(-2.0),
        [768; 2],
    );
    must_fail("a z-only flip of t_1", at_most(ray_gap(&a, &b), 1e-15));
}

/// A front track that is block 1's turns both lenses 180° about +Y, and
/// `upright` turns both a quarter turn about −Z: common rotations, the
/// relative one unchanged.
#[test]
fn rig_front_track_and_upright_turn_both_lenses() {
    let _time = timed("rig_front_track_and_upright_turn_both_lenses");
    let cal = pats();
    let tune = Tune::default();
    let block1_track = |small: Small| match small.binding {
        Binding::Direct => 1,
        Binding::Reversed => 0,
    };
    // A common rotation C, as a matrix: each lens's lens_from_rig times C,
    // and the centres turned by C⁻¹.
    let turned_by = |a: &Rig, b: &Rig, c: DMat3| {
        (0..2).all(|i| {
            let want = DMat3::from_quat(a.lenses[i].lens_from_rig) * c;
            misfit(b.lenses[i].lens_from_rig, want) <= 1e-12
                && (b.lenses[i].centre_m - c.transpose() * a.lenses[i].centre_m).length() <= 1e-15
        })
    };
    let half_y = DMat3::from_axis_angle(DVec3::Y, PI);
    for (k, &small) in smalls().iter().enumerate() {
        let conv = conv_for(k);
        if small.front_track == block1_track(small) {
            continue;
        }
        let front = build(&cal, small, conv, &tune, [768; 2]);
        let back = build(
            &cal,
            Small {
                front_track: block1_track(small),
                ..small
            },
            conv,
            &tune,
            [768; 2],
        );
        assert!(turned_by(&front, &back, half_y), "{small:?}");
        if small.upright == QuarterTurn::Zero {
            for upright in QuarterTurn::all() {
                let turned = build(&cal, Small { upright, ..small }, conv, &tune, [768; 2]);
                let u = DMat3::from_axis_angle(DVec3::NEG_Z, upright.degrees().to_radians());
                assert!(turned_by(&front, &turned, u), "{small:?} {upright:?}");
            }
        }
    }
    // Control: lens 1 alone turned 180° about +Y changes the relative
    // rotation, which a common turn leaves.
    let rig = build(&cal, smalls()[0], EulerConv::PLACEHOLDER, &tune, [768; 2]);
    let mut one = rig;
    one.lenses[1].lens_from_rig = rig.lenses[1].lens_from_rig * DQuat::from_rotation_y(PI);
    let relative = |r: &Rig| r.lenses[1].lens_from_rig * r.lenses[0].lens_from_rig.inverse();
    must_fail(
        "lens 1 alone turned",
        at_most(rotation_angle(relative(&rig), relative(&one)), SAME_TURN),
    );
}

/// The tune turns a lens as D33's `Ypr` turns a view, in its block's view
/// frame (6.2): a positive yaw swings the lens's axis towards its right,
/// a positive pitch swings it up, and a positive roll turns its right
/// side down, clockwise as seen from behind it. The frames are written by
/// hand at stored angles of (0°, 0°, 90°), where the placeholder's E is
/// the identity. Lens 0 then looks along −Z with its right at +X; lens 1
/// looks along +Z, turned 180° about Y with its right at −X, or about X
/// with its right at +X and its top down.
#[test]
fn rig_tune_turns_each_lens_as_seen() {
    let _time = timed("rig_tune_turns_each_lens_as_seen");
    let cal = with_angles(&pats(), [[0.0, 0.0, 90.0]; 2]);
    let (s, c) = 10f64.to_radians().sin_cos();
    // Each lens untuned, written by hand: its block, the turn axis, and
    // where its axis (+z), its right (+x) and its top (−y) land in the rig
    // frame.
    let lenses = [
        (0, TurnAxis::Y, [DVec3::NEG_Z, DVec3::X, DVec3::Y]),
        (1, TurnAxis::Y, [DVec3::Z, DVec3::NEG_X, DVec3::Y]),
        (1, TurnAxis::X, [DVec3::Z, DVec3::X, DVec3::NEG_Y]),
    ];
    // The tune's yaw, pitch or roll of 10° alone, or none, and where it
    // puts the lens's axis and right: the yaw swings the axis towards the
    // right, lens 0's to (sin 10°, 0, −cos 10°); the pitch swings it
    // towards the top; the roll turns the right away from the top.
    let cases: Vec<_> = lenses
        .iter()
        .flat_map(|&(b, turn, [axis, right, top])| {
            [
                ([0.0, 0.0, 0.0], axis, right),
                ([10.0, 0.0, 0.0], c * axis + s * right, c * right - s * axis),
                ([0.0, 10.0, 0.0], c * axis + s * top, right),
                ([0.0, 0.0, 10.0], axis, c * right - s * top),
            ]
            .map(|(ypr, axis, right)| (b, turn, ypr, axis, right))
        })
        .collect();
    let rig = |turn: TurnAxis, b: usize, ypr: [f64; 3]| {
        let geo = Geometry {
            turn,
            ..placeholder(CanvasMap::C, Generation::V6, EVERY_TERM)
        };
        let mut tune = Tune::default();
        tune.lenses[b].ypr_deg = ypr;
        Rig::build(&cal, &geo, &tune, 0, [768; 2]).unwrap()
    };
    // The rig_from_lens with that axis and right, whose down is their
    // cross product.
    let frame = |axis: DVec3, right: DVec3| DMat3::from_cols(right, axis.cross(right), axis);
    for &(b, turn, ypr, axis, right) in &cases {
        let rig_from_lens = rig(turn, b, ypr).lenses[b].lens_from_rig.inverse();
        let err = misfit(rig_from_lens, frame(axis, right));
        assert!(
            err <= 1e-12,
            "block {b}, turn {turn:?}, tune {ypr:?}: {err:e}"
        );
    }
    let tuned = || cases.iter().filter(|case| case.2 != [0.0; 3]);
    // Control: the tune's sign flipped, Q for Qᵀ, which for one angle is
    // that angle negated. Every tuned frame misses.
    let flipped = tuned()
        .map(|&(b, turn, ypr, axis, right)| {
            let rig_from_lens = rig(turn, b, ypr.map(|a| -a)).lenses[b]
                .lens_from_rig
                .inverse();
            misfit(rig_from_lens, frame(axis, right))
        })
        .fold(f64::INFINITY, f64::min);
    must_fail("the tune's sign flipped", at_most(flipped, 1e-12));
    // Control: block 1's tune turned in the rig frame, not its view frame,
    // as Q times the untuned rig_from_lens. A yaw commutes with the half
    // turn about Y, so that case alone can't tell them apart.
    let in_rig = tuned()
        .filter(|case| case.0 == 1)
        .map(|&(b, turn, [yaw, pitch, roll], axis, right)| {
            let untuned =
                DMat3::from_quat(rig(turn, b, [0.0; 3]).lenses[b].lens_from_rig.inverse());
            let q = ry(-yaw) * rx(pitch) * rz(-roll);
            misfit(DQuat::from_mat3(&(q * untuned)), frame(axis, right))
        })
        .fold(0.0, f64::max);
    must_fail(
        "block 1's tune turned in the rig frame",
        at_most(in_rig, 1e-12),
    );
}

/// A tune of 10° in yaw, pitch or roll alone, or none, and where it puts
/// a lens whose untuned axis, right and top in the rig frame are given:
/// the tuned axis and right, as [`rig_tune_turns_each_lens_as_seen`]
/// writes them.
fn tuned_by_hand(axis: DVec3, right: DVec3, top: DVec3) -> [([f64; 3], DVec3, DVec3); 4] {
    let (s, c) = 10f64.to_radians().sin_cos();
    [
        ([0.0, 0.0, 0.0], axis, right),
        ([10.0, 0.0, 0.0], c * axis + s * right, c * right - s * axis),
        ([0.0, 10.0, 0.0], c * axis + s * top, right),
        ([0.0, 0.0, 10.0], axis, c * right - s * top),
    ]
}

/// The rig_from_lens with that axis and right, whose down is their cross
/// product.
fn frame_from(axis: DVec3, right: DVec3) -> DMat3 {
    DMat3::from_cols(right, axis.cross(right), axis)
}

/// Pat's V6 t_1 (block 1's stored translation), in metres.
const PATS_T1: [f64; 3] = [0.000637, 0.000001, -0.031525];

/// The centre c_1 turns with lens 0, its tune included, and with nothing
/// of lens 1's (6.2: c_1 = rig_from_lens_0 · (sign(tz) · t_1), the stored
/// t_1 being in lens 0's frame), written by hand at stored angles of (0°,
/// 0°, 90°), where the placeholder's E is the identity: lens 0's tuned
/// right, down and axis carry t_1's three components, and each lens sits
/// half of c_1 from the origin.
#[test]
fn rig_centre_turns_with_lens_0s_tune() {
    let _time = timed("rig_centre_turns_with_lens_0s_tune");
    let cal = with_angles(&pats(), [[0.0, 0.0, 90.0]; 2]);
    let t = DVec3::from(PATS_T1);
    let geo = placeholder(CanvasMap::C, Generation::V6, EVERY_TERM);
    let centres = |ypr: [[f64; 3]; 2], tz: TzSign| {
        let mut tune = Tune::default();
        tune.lenses[0].ypr_deg = ypr[0];
        tune.lenses[1].ypr_deg = ypr[1];
        let geo = Geometry { tz, ..geo.clone() };
        let rig = Rig::build(&cal, &geo, &tune, 0, [768; 2]).unwrap();
        [rig.lenses[0].centre_m, rig.lenses[1].centre_m]
    };
    // c_1 from lens 0's frame by hand, and how far a rig's centres lie
    // from ±c_1/2.
    let by_hand = |axis: DVec3, right: DVec3| {
        let m = frame_from(axis, right);
        t.x * m.x_axis + t.y * m.y_axis + t.z * m.z_axis
    };
    let gap = |got: [DVec3; 2], c1: DVec3| {
        (got[0] + c1 / 2.0)
            .length()
            .max((got[1] - c1 / 2.0).length())
    };
    let untuned = by_hand(DVec3::NEG_Z, DVec3::X);
    let mut worst = 0.0f64;
    for (ypr, axis, right) in tuned_by_hand(DVec3::NEG_Z, DVec3::X, DVec3::Y) {
        let c1 = by_hand(axis, right);
        // Lens 0 tuned, with lens 1 tuned too or not.
        for lens_1 in [[0.0; 3], [-7.0, 4.0, 12.0]] {
            worst = worst.max(gap(centres([ypr, lens_1], TzSign::AsStored), c1));
            worst = worst.max(gap(centres([ypr, lens_1], TzSign::Negated), -c1));
        }
    }
    eprintln!("the centres lie within {worst:e} m of c_1 worked by hand");
    assert!(worst <= 1e-15, "{worst:e} m");
    // Untuned, c_1 is t_1 read through lens 0 along −Z: behind lens 0 by
    // 31.5 mm.
    assert!((untuned - DVec3::new(0.000637, -0.000001, 0.031525)).length() <= 1e-18);
    // Control: c_1 by lens 0's untuned rotation, with lens 0 yawed.
    must_fail(
        "c_1 by lens 0's untuned rotation",
        at_most(
            gap(
                centres([[10.0, 0.0, 0.0], [0.0; 3]], TzSign::AsStored),
                untuned,
            ),
            1e-15,
        ),
    );
}

/// Under `Extrinsics::Fitted` the measured relative rotation sets block
/// 1's view frame, whatever `Geometry::turn` says: untuned,
/// lens_from_rig_1 = relative · lens_from_rig_0 with lens 0 at F · S;
/// c_1 is the measured translation turned by lens 0; and block 1's tune
/// turns lens 1 in its own view frame, as seen. Written by hand for the
/// two half turns a back-to-back pair takes, about lens 0's y (the
/// placeholder's class predicts it) and about its x (the twins (Y, 0°)
/// and (X, ±90°) predict it), each under both turn axes, and for each
/// with a measured tilt of 2° on top, the tune's frame then read from the
/// rig untuned.
#[test]
fn rig_fitted_extrinsics_compose_by_hand() {
    let _time = timed("rig_fitted_extrinsics_compose_by_hand");
    let cal = pats();
    let t = DVec3::from(PATS_T1);
    let tilt = DQuat::from_axis_angle(DVec3::new(0.3, -0.5, 0.8).normalize(), 2.0 * DEG);
    let rig = |relative: DQuat, turn: TurnAxis, ypr: [[f64; 3]; 2]| {
        let geo = Geometry {
            extrinsics: Extrinsics::Fitted {
                relative,
                translation_m: t,
            },
            turn,
            ..placeholder(CanvasMap::C, Generation::V6, EVERY_TERM)
        };
        let mut tune = Tune::default();
        tune.lenses[0].ypr_deg = ypr[0];
        tune.lenses[1].ypr_deg = ypr[1];
        Rig::build(&cal, &geo, &tune, 0, [768; 2]).unwrap()
    };
    let rig_from_lens = |r: &Rig, b: usize| DMat3::from_quat(r.lenses[b].lens_from_rig.inverse());
    // Each half turn, and where it puts lens 1's axis, right and top in
    // the rig frame: lens 0 looks along −Z with its right at +X and its
    // top at +Y.
    let halves = [
        (
            "about y",
            DQuat::from_rotation_y(PI),
            [DVec3::Z, DVec3::NEG_X, DVec3::Y],
        ),
        (
            "about x",
            DQuat::from_rotation_x(PI),
            [DVec3::Z, DVec3::X, DVec3::NEG_Y],
        ),
    ];
    // c_1, t read through lens 0 along −Z, and how far a rig's centres
    // lie from ±c_1/2.
    let c1 = t.x * DVec3::X - t.y * DVec3::Y - t.z * DVec3::Z;
    let centre_gap = |r: &Rig, c1: DVec3| {
        (r.lenses[0].centre_m + c1 / 2.0)
            .length()
            .max((r.lenses[1].centre_m - c1 / 2.0).length())
    };
    let (mut worst, mut centres) = (0.0f64, 0.0f64);
    for (name, half, [axis, right, top]) in halves {
        for turn in TurnAxis::all() {
            // Lens 0, and the centres, by hand.
            let untuned = rig(half, turn, [[0.0; 3]; 2]);
            worst = worst.max(misfit(
                untuned.lenses[0].lens_from_rig.inverse(),
                frame_from(DVec3::NEG_Z, DVec3::X),
            ));
            centres = centres.max(centre_gap(&untuned, c1));
            // Lens 1 under each tune, by hand.
            for (ypr, axis, right) in tuned_by_hand(axis, right, top) {
                let r = rig(half, turn, [[0.0; 3], ypr]);
                let err = misfit(r.lenses[1].lens_from_rig.inverse(), frame_from(axis, right));
                assert!(
                    err <= 1e-12,
                    "{name} under turn {turn:?}, tune {ypr:?}: {err:e}"
                );
                // Lens 1's tune leaves lens 0 and the centres.
                assert_eq!(r.lenses[0], untuned.lenses[0]);
                assert_eq!(r.lenses[1].centre_m, untuned.lenses[1].centre_m);
            }
            // With the tilt: the untuned relation, and lens 1's tune in the
            // frame it then has.
            let relative = tilt * half;
            let untuned = rig(relative, turn, [[0.0; 3]; 2]);
            let l = |b: usize| untuned.lenses[b].lens_from_rig;
            worst = worst.max(rotation_angle(l(1), relative * l(0)));
            let m = rig_from_lens(&untuned, 1);
            for (ypr, axis, right) in tuned_by_hand(m.z_axis, m.x_axis, -m.y_axis) {
                let r = rig(relative, turn, [[0.0; 3], ypr]);
                worst = worst.max(misfit(
                    r.lenses[1].lens_from_rig.inverse(),
                    frame_from(axis, right),
                ));
            }
        }
    }
    // Lens 0's tune turns c_1 with it, as under `Stored`.
    let (s, c) = 10f64.to_radians().sin_cos();
    let yawed = rig(
        DQuat::from_rotation_x(PI),
        TurnAxis::Y,
        [[10.0, 0.0, 0.0], [0.0; 3]],
    );
    let c1_yawed = t.x * DVec3::new(c, 0.0, s) - t.y * DVec3::Y + t.z * DVec3::new(s, 0.0, -c);
    centres = centres.max(centre_gap(&yawed, c1_yawed));
    eprintln!(
        "the fitted rigs lie within {worst:e} of their frames worked by hand, and their centres \
         within {centres:e} m"
    );
    assert!(worst <= 1e-12, "{worst:e}");
    assert!(centres <= 1e-15, "{centres:e} m");
    // Control: block 1's view frame from the turn axis, as relative · F ·
    // T_1⁻¹ · Q_tuneᵀ · T_1 · S. With the relative a half turn about x and
    // T_1 one about Y, a yaw of 10° swings lens 1's axis to its left, by
    // sin 10° along its right.
    let f = DQuat::from_rotation_x(PI);
    let t1 = DQuat::from_rotation_y(PI);
    let q = DQuat::from_rotation_y(-10f64.to_radians());
    let by_turn = DQuat::from_rotation_x(PI) * f * t1.inverse() * q.conjugate() * t1;
    let [axis, right, top] = halves[1].2;
    let (_, want_axis, want_right) = tuned_by_hand(axis, right, top)[1];
    let swung = by_turn.inverse() * DVec3::Z;
    eprintln!(
        "with T_1 from the turn axis, lens 1's yaw moves its axis {:.3} along its right",
        (swung - axis).dot(right)
    );
    must_fail(
        "block 1's view frame from the turn axis",
        at_most(
            misfit(by_turn.inverse(), frame_from(want_axis, want_right)),
            1e-12,
        ),
    );
}

/// `Rig::build` refuses a θ_max past the limit, a missing generation, a
/// block's terms of the wrong generation, a front track past 1, a side
/// of 0, numbers that aren't finite, a focal, scale, src, dst or θ_max of
/// 0 or below, a fitted relative rotation of length 0 and an Euler
/// convention outside the space, each with its `RigError`.
#[test]
fn rig_build_refuses_what_it_cannot_build() {
    let _time = timed("rig_build_refuses_what_it_cannot_build");
    let cal = pats();
    let geo = placeholder(CanvasMap::A, Generation::V6, EVERY_TERM);
    let tune = Tune::default();
    let fine = Rig::build(&cal, &geo, &tune, 0, [768; 2]).unwrap();
    let refuse = |cal: &Calibration, geo: &Geometry, tune: &Tune, front: u8, sides: [u32; 2]| {
        Rig::build(cal, geo, tune, front, sides).unwrap_err()
    };
    // θ_max past each block's limit.
    for b in 0..2 {
        let limit_deg = fine.lenses[b].limit_theta.to_degrees();
        let mut past = tune;
        past.lenses[b].theta_max_deg = limit_deg + 0.01;
        assert_eq!(
            refuse(&cal, &geo, &past, 0, [768; 2]),
            RigError::PastLimit {
                block: b,
                theta_max_deg: limit_deg + 0.01,
                limit_deg
            }
        );
        let mut at = tune;
        at.lenses[b].theta_max_deg = limit_deg - 0.01;
        assert!(Rig::build(&cal, &geo, &at, 0, [768; 2]).is_ok());
    }
    eprintln!(
        "Pat's V6 lenses stop rising at {:.3}° and {:.3}°",
        fine.lenses[0].limit_theta.to_degrees(),
        fine.lenses[1].limit_theta.to_degrees()
    );
    // A generation the file lacks, and terms of the other generation.
    let no_v3 = Calibration { v3: None, ..cal };
    let v3 = placeholder(CanvasMap::A, Generation::V3, V6Reading::RADIAL_ONLY);
    assert_eq!(
        refuse(&no_v3, &v3, &tune, 0, [768; 2]),
        RigError::NoGeneration(Generation::V3)
    );
    let mut mixed = cal;
    mixed.v6.as_mut().unwrap()[1].terms = cal.v3.unwrap()[1].terms;
    assert_eq!(
        refuse(&mixed, &geo, &tune, 0, [768; 2]),
        RigError::Terms {
            generation: Generation::V6,
            block: 1
        }
    );
    // A reading that reads d7 and d8 twice.
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
    let bad = Geometry {
        v6: twice,
        ..geo.clone()
    };
    assert_eq!(
        refuse(&cal, &bad, &tune, 0, [768; 2]),
        RigError::Reading(twice)
    );
    // A front track past 1, and a side of 0.
    assert_eq!(
        refuse(&cal, &geo, &tune, 2, [768; 2]),
        RigError::FrontTrack(2)
    );
    assert_eq!(
        refuse(&cal, &geo, &tune, 0, [768, 0]),
        RigError::Side { track: 1 }
    );
    // A NaN or an infinity in each number.
    let not_finite = |e: RigError| matches!(e, RigError::NotFinite(_));
    for bad in [f64::NAN, f64::INFINITY] {
        for (b, d) in PATS_D.iter().enumerate() {
            let blocks = |f: &dyn Fn(&mut LensCalib)| {
                let mut c = cal;
                f(&mut c.v6.as_mut().unwrap()[b]);
                c
            };
            for c in [
                blocks(&|l| l.xi = bad),
                blocks(&|l| l.fx = bad),
                blocks(&|l| l.cy = bad),
                blocks(&|l| l.ypr_deg[2] = bad),
                blocks(&|l| l.t_m[0] = bad),
                blocks(&|l| {
                    l.terms = CalibTerms::V6 {
                        d: {
                            let mut d = *d;
                            d[11] = bad;
                            d
                        },
                    }
                }),
            ] {
                assert!(not_finite(refuse(&c, &geo, &tune, 0, [768; 2])));
            }
            let lens = |f: &dyn Fn(&mut LensTune)| {
                let mut t = tune;
                f(&mut t.lenses[b]);
                t
            };
            for t in [
                lens(&|l| l.ypr_deg[1] = bad),
                lens(&|l| l.centre_px[0] = bad),
                lens(&|l| l.radius_scale = bad),
                lens(&|l| l.xi_delta = bad),
                lens(&|l| l.theta_max_deg = bad),
            ] {
                assert!(not_finite(refuse(&cal, &geo, &t, 0, [768; 2])));
            }
        }
        for t in [
            Tune {
                q_per_m: bad,
                ..tune
            },
            Tune {
                feather_deg: bad,
                ..tune
            },
        ] {
            assert!(not_finite(refuse(&cal, &geo, &t, 0, [768; 2])));
        }
        let scaled = Geometry {
            canvas: CanvasMap::Fitted { scale: bad },
            ..geo.clone()
        };
        assert!(not_finite(refuse(&cal, &scaled, &tune, 0, [768; 2])));
        let fitted = Geometry {
            extrinsics: Extrinsics::Fitted {
                relative: DQuat::from_xyzw(0.0, 1.0, 0.0, bad),
                translation_m: DVec3::ZERO,
            },
            ..geo.clone()
        };
        assert!(not_finite(refuse(&cal, &fitted, &tune, 0, [768; 2])));
    }
    // Each number the rig scales or divides by, at 0 and below, with the
    // field its refusal names. Control: each just above 0, which builds,
    // however little the rig then weighs.
    let lens_names = |b: usize| {
        [
            ["block 0's fx", "block 1's fx"][b],
            ["block 0's fy", "block 1's fy"][b],
            ["lens 0's radius scale", "lens 1's radius scale"][b],
            ["lens 0's θ_max", "lens 1's θ_max"][b],
        ]
    };
    let at = |field: usize, b: usize, v: f64| {
        let (mut c, mut g, mut t) = (cal, geo.clone(), tune);
        match field {
            0 => c.v6.as_mut().unwrap()[b].fx = v,
            1 => c.v6.as_mut().unwrap()[b].fy = v,
            2 => t.lenses[b].radius_scale = v,
            3 => t.lenses[b].theta_max_deg = v,
            _ => g.canvas = CanvasMap::Fitted { scale: v },
        }
        Rig::build(&c, &g, &t, 0, [768; 2])
    };
    for b in 0..2 {
        let names = lens_names(b);
        for (field, name) in names.iter().enumerate() {
            for v in [0.0, -1.0, -4277.5] {
                assert_eq!(at(field, b, v).unwrap_err(), RigError::NotPositive(name));
            }
            must_fail(
                &format!("{name} just above 0, refused"),
                at(field, b, 1e-6).is_err(),
            );
        }
    }
    for v in [0.0, -0.95] {
        assert_eq!(
            at(4, 0, v).unwrap_err(),
            RigError::NotPositive("the canvas map's scale")
        );
    }
    must_fail(
        "a canvas scale just above 0, refused",
        at(4, 0, 1e-6).is_err(),
    );
    for (name, c, one) in [
        (
            "the calibration's src",
            Calibration { src: 0, ..cal },
            Calibration { src: 1, ..cal },
        ),
        (
            "the calibration's dst",
            Calibration { dst: 0, ..cal },
            Calibration { dst: 1, ..cal },
        ),
    ] {
        assert_eq!(
            refuse(&c, &geo, &tune, 0, [768; 2]),
            RigError::NotPositive(name)
        );
        must_fail(
            &format!("{name} of 1, refused"),
            Rig::build(&one, &geo, &tune, 0, [768; 2]).is_err(),
        );
    }
    // A fitted relative rotation of length 0, or so short its square
    // underflows. Control: one short enough to look like it, 1e-150,
    // which still normalises.
    let fitted = |w: f64| Geometry {
        extrinsics: Extrinsics::Fitted {
            relative: DQuat::from_xyzw(0.0, w, 0.0, 0.0),
            translation_m: DVec3::from(PATS_T1),
        },
        ..geo.clone()
    };
    for w in [0.0, 1e-170] {
        assert_eq!(
            refuse(&cal, &fitted(w), &tune, 0, [768; 2]),
            RigError::NotRotation("the fitted relative rotation")
        );
    }
    must_fail(
        "a relative of length 1e-150, refused",
        Rig::build(&cal, &fitted(1e-150), &tune, 0, [768; 2]).is_err(),
    );
    // An Euler convention whose axes or order repeat one. Control: each
    // field reversed, still a permutation, which builds.
    let euler = |conv: EulerConv| Geometry {
        extrinsics: Extrinsics::Stored(conv),
        ..geo.clone()
    };
    let p = EulerConv::PLACEHOLDER;
    for (name, bad, good) in [
        (
            "the Euler convention's axes",
            EulerConv {
                axes: [Axis::Y, Axis::X, Axis::Y],
                ..p
            },
            EulerConv {
                axes: [Axis::Z, Axis::X, Axis::Y],
                ..p
            },
        ),
        (
            "the Euler convention's order",
            EulerConv {
                order: [Angle::Yaw, Angle::Roll, Angle::Roll],
                ..p
            },
            EulerConv {
                order: [Angle::Roll, Angle::Pitch, Angle::Yaw],
                ..p
            },
        ),
    ] {
        assert!(!bad.is_valid() && good.is_valid());
        assert_eq!(
            refuse(&cal, &euler(bad), &tune, 0, [768; 2]),
            RigError::NotPermutation(name)
        );
        must_fail(
            &format!("{name} reversed, refused"),
            Rig::build(&cal, &euler(good), &tune, 0, [768; 2]).is_err(),
        );
    }
    // Every refusal says what it refused.
    for (e, says) in [
        (
            RigError::NotPositive("block 1's fy"),
            "block 1's fy is not above 0",
        ),
        (
            RigError::NotPermutation("the Euler convention's order"),
            "the Euler convention's order is not a permutation",
        ),
        (
            RigError::NotRotation("the fitted relative rotation"),
            "the fitted relative rotation has no length to normalise",
        ),
    ] {
        assert_eq!(e.to_string(), says);
    }
    assert!(
        RigError::PastLimit {
            block: 1,
            theta_max_deg: 125.0,
            limit_deg: 118.2
        }
        .to_string()
        .contains("118.2")
    );
    // Control: the refusal check on a rig it can build.
    must_fail(
        "a rig it can build, refused",
        Rig::build(&cal, &geo, &tune, 0, [768; 2]).is_err(),
    );
}

/// The least of min(θ_max, θ_frame) − 90° over the lenses of a tune, in
/// degrees: the most the feather may be (11.2).
fn feather_bound(cal: &Calibration, geo: &Geometry, tune: &Tune) -> f64 {
    let frame = theta_frame(cal, geo, tune).unwrap();
    (0..2)
        .map(|b| tune.lenses[b].theta_max_deg.min(frame[b].to_degrees()) - 90.0)
        .fold(f64::INFINITY, f64::min)
}

/// `Tune::default()` lies inside every bound of `validate` (11.2) on
/// Pat's V6 numbers, the feather against min(θ_max, θ_frame) − 90° among
/// them.
#[test]
fn tune_default_lies_inside_validates_bounds() {
    let _time = timed("tune_default_lies_inside_validates_bounds");
    let cal = pats();
    let tune = Tune::default();
    for lens in &tune.lenses {
        assert!(lens.ypr_deg.iter().all(|a| a.abs() <= 15.0));
        assert!(lens.centre_px.iter().all(|c| c.abs() <= 64.0));
        assert!((0.9..=1.1).contains(&lens.radius_scale));
        assert!((85.0..=110.0).contains(&lens.theta_max_deg));
        assert_eq!(lens.xi_delta, 0.0);
    }
    assert!((0.0..=15.0).contains(&tune.feather_deg));
    assert!((0.0..=3.34).contains(&tune.q_per_m));
    let mut least = f64::INFINITY;
    for canvas in CanvasMap::all() {
        let readings = std::iter::once((Generation::V3, V6Reading::RADIAL_ONLY))
            .chain(V6Reading::all().map(|v| (Generation::V6, v)));
        for (generation, v6) in readings {
            let geo = placeholder(canvas, generation, v6);
            // Below the limit, as `build` checks.
            assert!(Rig::build(&cal, &geo, &tune, 0, [768; 2]).is_ok());
            let bound = feather_bound(&cal, &geo, &tune);
            assert!(
                tune.feather_deg <= bound,
                "{canvas:?} {generation:?} {v6:?}: {bound}°"
            );
            least = least.min(bound);
        }
    }
    eprintln!("on Pat's numbers the feather may reach {least:.3}° at least");
    // Control: a feather of 10° on Pat's V6 under map A.
    let geo = placeholder(CanvasMap::A, Generation::V6, V6Reading::RADIAL_ONLY);
    must_fail(
        "a feather of 10°",
        at_most(10.0, feather_bound(&cal, &geo, &tune)),
    );
}

// The weights, the stitch distance and the mask.

/// A rig for the weights, the distance and the mask.
#[derive(Debug)]
struct Pooled {
    name: String,
    rig: Rig,
    /// Whether it is one of the plain rigs: Pat's numbers (or the
    /// stereographic rig on Pat's rotations and centres), the tune's
    /// angles within 0.25° and q at most 0.5 m⁻¹, so that h of 2° or more
    /// at each lens leaves no direction a lens covers at a weight of 0.
    plain: bool,
    /// The LOD up to which each lens's h stays at 2° or more (6.2's
    /// shrink leaves it there), which the plain rigs' properties draw
    /// below.
    lod_max: f64,
    /// Whether its lenses are V6's radial-only reading, which a mirror
    /// leaves as they are.
    radial: bool,
}

/// The h the plain rigs keep, in radians.
const PLAIN_FEATHER: f64 = 2.0 * DEG;

/// The LOD up to which `rig`'s lenses keep h at `h` or more: margin(λ) =
/// 2^(λ+1) texels, in angle at the band's least slope, leaves
/// min(θ_max, θ_frame) − 90° − h, at most 4.
fn lod_keeping(rig: &Rig, h: f64) -> f64 {
    (0..2)
        .map(|b| {
            let l = &rig.lenses[b];
            let room = l.theta_max.min(l.theta_frame) - FRAC_PI_2 - h;
            if room > 0.0 {
                (room * l.band_texels_per_radian / rig.margin_texels).log2()
            } else {
                f64::NEG_INFINITY
            }
        })
        .fold(4.0, f64::min)
}

/// A stereographic Kannala–Brandt pair on the rotations and centres of
/// Pat's V6 under C, at the default tune: its circle meets its 768²
/// slot's left edge at θ_frame ≈ 97.4° while θ_max is 104°, and its slope
/// rises with θ, from 334 texels a radian at 90° to 441 at θ_max, so the
/// frame bounds the feather where the slope at θ_max would misplace it.
fn stereographic_rig() -> Rig {
    let cal = pats();
    let geo = placeholder(CanvasMap::C, Generation::V6, V6Reading::RADIAL_ONLY);
    let base = Rig::build(&cal, &geo, &Tune::default(), 0, [768; 2]).unwrap();
    let model = LensModel::Kb(KbLens {
        base: Model::Stereographic,
        k: [0.0; 5],
        fx: 167.0,
        fy: 167.0,
        cx: 380.0,
        cy: 386.0,
        p: [0.0; 2],
        pe: [0.0; 2],
        s: [0.0; 4],
    });
    let lenses = base.lenses.map(|l| {
        RigLens::new(
            l.track,
            model,
            l.lens_from_rig,
            l.centre_m,
            104.0 * DEG,
            768,
        )
    });
    Rig { lenses, ..base }
}

/// The pool: Pat's numbers under each generation, two readings and each
/// canvas map, at the default tune, at q = 0 and at the feather's bound;
/// seeded rigs near those; seeded rigs anywhere in `validate`'s bounds
/// (11.2), with any geometry; and [`stereographic_rig`].
fn pool() -> &'static [Pooled] {
    static POOL: OnceLock<Vec<Pooled>> = OnceLock::new();
    POOL.get_or_init(|| {
        let cal = pats();
        let mut pool = Vec::new();
        let mut add = |name: String, rig: Rig, plain: bool, radial: bool| {
            let lod_max = lod_keeping(&rig, PLAIN_FEATHER);
            pool.push(Pooled {
                name,
                rig,
                plain,
                lod_max,
                radial,
            });
        };
        let models = [
            ("V3", Generation::V3, V6Reading::RADIAL_ONLY),
            ("V6 radial-only", Generation::V6, V6Reading::RADIAL_ONLY),
            ("V6 every term", Generation::V6, EVERY_TERM),
        ];
        for (model, generation, v6) in models {
            for canvas in CanvasMap::all() {
                let geo = placeholder(canvas, generation, v6);
                let bound = feather_bound(&cal, &geo, &Tune::default());
                for (what, tune) in [
                    ("default", Tune::default()),
                    ("q = 0", tune_at(0.0)),
                    (
                        "at the feather's bound",
                        Tune {
                            feather_deg: bound,
                            ..Tune::default()
                        },
                    ),
                ] {
                    for sides in [[768, 768], [3840, 832]] {
                        let rig = Rig::build(&cal, &geo, &tune, 0, sides).unwrap();
                        let radial = generation == Generation::V6 && v6 == V6Reading::RADIAL_ONLY;
                        add(
                            format!("Pat's {model} under {canvas:?}, {what}, {sides:?}"),
                            rig,
                            true,
                            radial,
                        );
                    }
                }
            }
        }
        let mut rng = Rng(0x005e_ed0f_9001);
        for i in 0..64 {
            let near = i < 32;
            let small = smalls()[rng.below(smalls().len())];
            let conv = if near {
                EulerConv::PLACEHOLDER
            } else {
                convs()[rng.below(convs().len())]
            };
            let geo = geometry(small, conv);
            // A tune whose frame leaves room for a feather: past a radius
            // scale of about 1.09 under map A, θ_frame falls below 90°
            // and `validate` takes no feather.
            let (mut tune, bound) = loop {
                let mut lenses = [seeded_lens_tune(&mut rng), seeded_lens_tune(&mut rng)];
                if near {
                    for l in &mut lenses {
                        l.ypr_deg = l.ypr_deg.map(|a| a / 60.0);
                        l.centre_px = l.centre_px.map(|c| c / 4.0);
                        l.radius_scale = 1.0 + (l.radius_scale - 1.0) / 10.0;
                        l.theta_max_deg = l.theta_max_deg.max(98.0);
                    }
                }
                let tune = Tune {
                    lenses,
                    feather_deg: 0.0,
                    q_per_m: rng.range(0.0, if near { 0.5 } else { 3.34 }),
                };
                let bound = feather_bound(&cal, &geo, &tune).min(15.0);
                if bound >= 0.0 {
                    break (tune, bound);
                }
            };
            tune.feather_deg = if near {
                rng.range(bound.min(4.0), bound)
            } else {
                rng.range(0.0, bound)
            };
            // A plain rig's slots are 768² or more, where 2 texels leave h
            // its 2°.
            let sides = [0, 1].map(|_| {
                let at = if near { 1 + rng.below(5) } else { rng.below(6) };
                [384, 768, 832, 1920, 2880, 3840][at]
            });
            let rig = Rig::build(&cal, &geo, &tune, small.front_track, sides).unwrap();
            let radial = small.generation == Generation::V6 && small.v6 == V6Reading::RADIAL_ONLY;
            let plain = near && tune.feather_deg >= 2.0;
            add(
                format!("seeded {i}: {small:?} {conv:?} {tune:?} {sides:?}"),
                rig,
                plain,
                radial,
            );
        }
        add(
            "the stereographic rig".to_string(),
            stereographic_rig(),
            true,
            false,
        );
        for p in pool.iter().filter(|p| p.plain) {
            assert!(p.lod_max >= 0.0, "{}: h under 2° at LOD 0", p.name);
        }
        pool
    })
}

/// The narrowest h the rig's weights shrink to, in radians (6.2).
const MIN_FEATHER: f64 = 0.5 * DEG;

/// Hermite's smooth step from 0 at `lo` to 1 at `hi`, a step at `lo` when
/// they meet.
fn ramp(lo: f64, hi: f64, x: f64) -> f64 {
    if hi <= lo {
        return if x < lo {
            0.0
        } else if x > lo {
            1.0
        } else {
            0.5
        };
    }
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Raw weights normalised, or both 0.
fn normalised(w: [f64; 2]) -> [f64; 2] {
    let sum = w[0] + w[1];
    if sum > 0.0 {
        w.map(|w| w / sum)
    } else {
        [0.0; 2]
    }
}

/// The weights with h never shrunk (a control): the tune's feather at
/// every LOD, the mask kept.
fn unshrunk(rig: &Rig, d: DVec3, lod: [f64; 2]) -> [f64; 2] {
    let h = rig.feather;
    normalised([0, 1].map(|b| {
        if rig.covers(b, d, lod[b]) {
            1.0 - ramp(FRAC_PI_2 - h, FRAC_PI_2 + h, off_axis(rig.ray(b, d)))
        } else {
            0.0
        }
    }))
}

/// The weights with the margin's angle for the h shrink taken at θ_max's
/// slope alone, not the band's least (a control): on a lens whose slope
/// rises, with θ_frame the bound, the band then runs past the frame's
/// cut.
fn slope_at_theta_max(rig: &Rig, d: DVec3, lod: [f64; 2]) -> [f64; 2] {
    normalised([0, 1].map(|b| {
        if !rig.covers(b, d, lod[b]) {
            return 0.0;
        }
        let l = &rig.lenses[b];
        let margin = rig.margin_texels * lod[b].max(0.0).exp2() / l.texels_per_radian;
        let room = l.theta_max.min(l.theta_frame) - margin - FRAC_PI_2;
        let h = rig.feather.min(room.max(MIN_FEATHER));
        1.0 - ramp(FRAC_PI_2 - h, FRAC_PI_2 + h, off_axis(rig.ray(b, d)))
    }))
}

/// The weights with the frame term off (a control): the mask's θ alone,
/// and h shrunk by θ_max alone.
fn frameless(rig: &Rig, d: DVec3, lod: [f64; 2]) -> [f64; 2] {
    normalised([0, 1].map(|b| {
        let l = &rig.lenses[b];
        let margin = rig.margin_texels * lod[b].max(0.0).exp2() / l.texels_per_radian;
        let theta = off_axis(rig.ray(b, d));
        if theta > l.theta_max - margin {
            return 0.0;
        }
        let h = rig
            .feather
            .min((l.theta_max - margin - FRAC_PI_2).max(MIN_FEATHER));
        1.0 - ramp(FRAC_PI_2 - h, FRAC_PI_2 + h, theta)
    }))
}

/// The weights' check at one direction: each in [0, 1]; their sum 1
/// wherever a lens covers d with θ below 90° + h, and on a plain rig
/// wherever a lens covers d at all; 0 where neither covers it below
/// 90° + h.
fn weights_hold(rig: &Rig, plain: bool, d: DVec3, lod: [f64; 2]) -> Result<(), String> {
    let w = rig.weights(d, lod);
    if !w.iter().all(|w| (0.0..=1.0).contains(w)) {
        return Err(format!("{w:?} at {d}"));
    }
    let sum = w[0] + w[1];
    let covers = |b: usize| rig.covers(b, d, lod[b]);
    let inside =
        |b: usize| covers(b) && off_axis(rig.ray(b, d)) < FRAC_PI_2 + rig.feather_at(b, lod[b]);
    let one = inside(0) || inside(1) || (plain && (covers(0) || covers(1)));
    if one && (sum - 1.0).abs() > 1e-12 {
        return Err(format!("{w:?} at {d}, LOD {lod:?}, where a lens covers"));
    }
    if !covers(0) && !covers(1) && sum != 0.0 {
        return Err(format!("{w:?} at {d}, LOD {lod:?}, where no lens covers"));
    }
    if !plain && !inside(0) && !inside(1) && sum != 0.0 {
        return Err(format!(
            "{w:?} at {d}, LOD {lod:?}, where no lens covers below 90° + h"
        ));
    }
    Ok(())
}

/// The LODs a case draws from two numbers in [0, 1]: −1 to the rig's
/// `lod_max` on a plain rig, −1 to 4 on the rest.
fn lods(p: &Pooled, u: [f64; 2]) -> [f64; 2] {
    let top = if p.plain { p.lod_max } else { 4.0 };
    u.map(|u| -1.0 + u * (top + 1.0))
}

/// The plain rigs' places in the pool.
fn plain() -> &'static [usize] {
    static PLAIN: OnceLock<Vec<usize>> = OnceLock::new();
    PLAIN.get_or_init(|| (0..pool().len()).filter(|&i| pool()[i].plain).collect())
}

/// The radial rigs' places in the pool.
fn radial() -> &'static [usize] {
    static RADIAL: OnceLock<Vec<usize>> = OnceLock::new();
    RADIAL.get_or_init(|| (0..pool().len()).filter(|&i| pool()[i].radial).collect())
}

/// The biggest jump in the weights over a step of 1e-9 rad either way
/// across the edge of lens `b`'s mask along its meridian at azimuth φ,
/// which a bisection finds between θ = 80° and 115°.
fn jump_at_the_edge(
    rig: &Rig,
    weights: impl Fn(&Rig, DVec3, [f64; 2]) -> [f64; 2],
    b: usize,
    phi: f64,
    lod: [f64; 2],
) -> f64 {
    let to_rig = rig.lenses[b].lens_from_rig.inverse();
    let at = |theta: f64| to_rig * dir(theta, phi);
    let (mut lo, mut hi) = (80.0 * DEG, 115.0 * DEG);
    if !rig.covers(b, at(lo), lod[b]) || rig.covers(b, at(hi), lod[b]) {
        return 0.0;
    }
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if rig.covers(b, at(mid), lod[b]) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (a, z) = (
        weights(rig, at(lo - 1e-9), lod),
        weights(rig, at(hi + 1e-9), lod),
    );
    (a[0] - z[0]).abs().max((a[1] - z[1]).abs())
}

/// The biggest jump in the weights a step allows and still be
/// continuous: a step of 1e-7 rad moves them at most about 1e-5 at the
/// narrowest h.
const JUMP: f64 = 1e-3;

/// The rig made of `rig`'s lens 0 and, in lens 1's place, its mirror
/// image in the rig's plane z = 0: P = diag(1, 1, −1) in the rig frame,
/// the image's x mirrored in the slot, so that with Q = diag(−1, 1, 1)
/// lens_from_rig_1 = Q · lens_from_rig_0 · P, a rotation, and c_1 = P c_0.
/// Lens 0 must be radial, which the mirror leaves as it is.
fn mirrored(rig: &Rig) -> Rig {
    let l0 = rig.lenses[0];
    let LensModel::Mei(u) = l0.model else {
        panic!("not Mei")
    };
    let side = f64::from(l0.side);
    let model = LensModel::Mei(Unified {
        cx: side - u.cx,
        ..u
    });
    let p = DMat3::from_diagonal(DVec3::new(1.0, 1.0, -1.0));
    let q = DMat3::from_diagonal(DVec3::new(-1.0, 1.0, 1.0));
    let turn = DQuat::from_mat3(&(q * DMat3::from_quat(l0.lens_from_rig) * p));
    let l1 = RigLens::new(
        1 - l0.track,
        model,
        turn,
        p * l0.centre_m,
        l0.theta_max,
        l0.side,
    );
    Rig {
        lenses: [l0, l1],
        ..*rig
    }
}

/// How far the scene point `r` metres from the rig's origin along `d`
/// lies from lens b's ray, in metres.
fn miss(rig: &Rig, b: usize, d: DVec3, r: f64) -> f64 {
    let lens = &rig.lenses[b];
    let along = lens.lens_from_rig.inverse() * rig.ray(b, d);
    (d.normalize() * r - lens.centre_m).cross(along).length()
}

/// Whether every lens with a non-zero weight at d has its footprint,
/// 2^(λ+1) texels about its texel (6.2), inside its slot, and θ within
/// θ_max less that margin in angle.
fn footprint_inside(rig: &Rig, w: [f64; 2], d: DVec3, lod: [f64; 2]) -> Result<(), String> {
    for b in 0..2 {
        if w[b] == 0.0 {
            continue;
        }
        let lens = &rig.lenses[b];
        let margin = (lod[b].max(0.0) + 1.0).exp2();
        let px = rig
            .texel(b, d)
            .ok_or_else(|| format!("lens {b} weighs {} at {d} with no texel", w[b]))?;
        let side = f64::from(lens.side);
        if px.min_element() < margin || px.max_element() > side - margin {
            return Err(format!(
                "lens {b} weighs {} at {d}, LOD {}, at {px} on a side of {side}",
                w[b], lod[b]
            ));
        }
        let theta = off_axis(rig.ray(b, d));
        if theta > lens.theta_max - margin / lens.texels_per_radian {
            return Err(format!(
                "lens {b} weighs {} at θ {}°",
                w[b],
                theta.to_degrees()
            ));
        }
    }
    Ok(())
}

/// How many directions near lens 0's frame, θ from 93° to 101° at every
/// 0.05° and every 1° of azimuth, a weights function weighs outside the
/// mask at LOD `lod`.
fn outside_near_the_frame(
    rig: &Rig,
    weights: impl Fn(&Rig, DVec3, [f64; 2]) -> [f64; 2],
    lod: f64,
) -> usize {
    let to_rig = rig.lenses[0].lens_from_rig.inverse();
    let mut outside = 0;
    for i in 0..=160 {
        let theta = (93.0 + 0.05 * f64::from(i)) * DEG;
        for j in 0..360 {
            let d = to_rig * dir(theta, f64::from(j) * DEG);
            let lod = [lod; 2];
            if footprint_inside(rig, weights(rig, d, lod), d, lod).is_err() {
                outside += 1;
            }
        }
    }
    outside
}

/// Pat's V6 under map A, radial-only, at the feather's bound, on 768²
/// slots: the mask's rig.
fn pats_v6_a_at_the_bound() -> Rig {
    let cal = pats();
    let geo = placeholder(CanvasMap::A, Generation::V6, V6Reading::RADIAL_ONLY);
    let tune = Tune {
        feather_deg: feather_bound(&cal, &geo, &Tune::default()),
        ..Tune::default()
    };
    Rig::build(&cal, &geo, &tune, 0, [768; 2]).unwrap()
}

/// θ_frame by a scan of the listed edges of lens's slot (0 top, 1
/// bottom, 2 left, 3 right) at one sample per texel, through its inverse:
/// the least θ, a pixel the inverse finds no direction for counting as
/// the limit.
fn scanned(lens: &RigLens, edges: &[usize]) -> f64 {
    let side = f64::from(lens.side);
    let mut least = lens.limit_theta;
    for i in 0..=lens.side {
        let t = f64::from(i);
        for &e in edges {
            let px = match e {
                0 => DVec2::new(t, 0.0),
                1 => DVec2::new(t, side),
                2 => DVec2::new(0.0, t),
                _ => DVec2::new(side, t),
            };
            let theta = lens
                .model
                .unproject_within(px, lens.limit_theta)
                .map_or(lens.limit_theta, off_axis);
            least = least.min(theta);
        }
    }
    least
}

/// How close `theta_frame` and the scan agree, in radians.
const SCAN: f64 = 0.01 * DEG;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn weights_hold_cases(i in 0..pool().len(), z in -1.0f64..=1.0, phi in -PI..PI, u in [0.0f64..=1.0, 0.0f64..=1.0]) {
        let p = &pool()[i];
        let r = weights_hold(&p.rig, p.plain, on_sphere(z, phi), lods(p, u));
        prop_assert!(r.is_ok(), "{}: {}", p.name, r.unwrap_err());
    }

    fn continuous_cases(
        i in 0..plain().len(),
        z in -1.0f64..=1.0,
        phi in -PI..PI,
        u in [0.0f64..=1.0, 0.0f64..=1.0],
        turn in -PI..PI,
        b in 0usize..2,
        meridian in -PI..PI,
    ) {
        let p = &pool()[plain()[i]];
        let lod = lods(p, u);
        let d = on_sphere(z, phi);
        // A step of 1e-7 rad about an axis across d.
        let across = d.any_orthonormal_pair();
        let axis = across.0 * turn.cos() + across.1 * turn.sin();
        let near = DQuat::from_axis_angle(axis, 1e-7) * d;
        let (a, z) = (p.rig.weights(d, lod), p.rig.weights(near, lod));
        let step = (a[0] - z[0]).abs().max((a[1] - z[1]).abs());
        prop_assert!(step <= JUMP, "{}: {step:e} at {d}, LOD {lod:?}", p.name);
        let jump = jump_at_the_edge(&p.rig, Rig::weights, b, meridian, lod);
        prop_assert!(jump <= JUMP, "{}: {jump:e} across lens {b}'s edge at {meridian}, LOD {lod:?}", p.name);
    }

    fn mirrored_swap_cases(i in 0..radial().len(), z in -1.0f64..=1.0, phi in -PI..PI, u in [0.0f64..=1.0, 0.0f64..=1.0]) {
        let p = &pool()[radial()[i]];
        let rig = mirrored(&p.rig);
        let lod = lods(p, u);
        let d = on_sphere(z, phi);
        let w = rig.weights(d, lod);
        let m = rig.weights(DVec3::new(d.x, d.y, -d.z), [lod[1], lod[0]]);
        let gap = (w[0] - m[1]).abs().max((w[1] - m[0]).abs());
        prop_assert!(gap <= 1e-9, "{}: {w:?} and {m:?} at {d}", p.name);
    }

    fn distance_cases(i in 0..pool().len(), z in -1.0f64..=1.0, phi in -PI..PI, r in 0.3f64..=100.0) {
        let p = &pool()[i];
        let d = on_sphere(z, phi);
        let at = |q: f64| {
            let rig = Rig { q, ..p.rig };
            miss(&rig, 0, d, r).max(miss(&rig, 1, d, r))
        };
        let exact = at(1.0 / r);
        prop_assert!(exact <= 1e-9, "{}: {exact:e} m at {d}, R {r}", p.name);
        for sign in [-1.0, 1.0] {
            let mut last = exact;
            for k in -3..=0 {
                let off = at(1.0 / r + sign * 10f64.powi(k));
                prop_assert!(off >= last, "{}: {off:e} m after {last:e}", p.name);
                last = off;
            }
        }
    }

    fn mask_cases(i in 0..pool().len(), z in -1.0f64..=1.0, phi in -PI..PI, u in [0.0f64..=1.0, 0.0f64..=1.0]) {
        let p = &pool()[i];
        let d = on_sphere(z, phi);
        let lod = u.map(|u| -1.0 + 5.0 * u);
        let r = footprint_inside(&p.rig, p.rig.weights(d, lod), d, lod);
        prop_assert!(r.is_ok(), "{}: {}", p.name, r.unwrap_err());
    }
}

/// The scan property on random rigs, a fortieth of `PROPTEST_CASES` (at
/// least 16).
fn scan_cases() {
    let slot = || prop_oneof![Just(384u32), Just(768), Just(832)];
    let draw = (
        small(),
        conv(),
        [lens_tune(), lens_tune()],
        [slot(), slot()],
    );
    run_cases(
        (cases() / 40).max(16),
        draw,
        |(small, conv, lenses, sides)| {
            let cal = pats();
            let tune = Tune {
                lenses,
                ..Tune::default()
            };
            let rig = build(&cal, small, conv, &tune, sides);
            let public = theta_frame(&cal, &geometry(small, conv), &tune).unwrap();
            for (b, (lens, public)) in rig.lenses.iter().zip(public).enumerate() {
                // Past 110°, θ_max's bound, the frame bounds no mask, and where
                // the circle barely meets an edge, θ climbs by up to 0.06° a
                // texel on 384², more than one sample a texel can place.
                let scan = scanned(lens, &[0, 1, 2, 3]);
                if scan.min(lens.theta_frame) <= 110.0 * DEG {
                    let off = (scan - lens.theta_frame).abs();
                    prop_assert!(off <= SCAN, "lens {b}: {}° off the scan", off.to_degrees());
                }
                prop_assert!(
                    (public - lens.theta_frame).abs() <= 1e-8,
                    "{public} against {}",
                    lens.theta_frame
                );
            }
            Ok(())
        },
    );
}

/// The weights lie in [0, 1] and sum to 1 wherever a lens covers.
///
/// Wherever a lens covers below 90° + h on any rig in `validate`'s bounds,
/// and wherever a lens covers at all on the plain rigs: where no lens
/// covers below 90° + h they are 0, a hole in the picture, which a tune in
/// the bounds can open (the controls).
#[test]
fn weights_lie_in_the_unit_interval_and_sum_to_one() {
    let _time = timed("weights_lie_in_the_unit_interval_and_sum_to_one");
    eprintln!(
        "the pool: {} rigs, {} of them plain",
        pool().len(),
        plain().len()
    );
    weights_hold_cases();
    // Control: on Pat's V6 at 0.3 m with h of 1°, both lenses see the
    // seam 3° past 90°, covered and weighed 0.
    let cal = pats();
    let geo = placeholder(CanvasMap::A, Generation::V6, V6Reading::RADIAL_ONLY);
    let near = Tune {
        feather_deg: 1.0,
        q_per_m: 3.34,
        ..Tune::default()
    };
    let rig = Rig::build(&cal, &geo, &near, 0, [768; 2]).unwrap();
    must_fail(
        "a 1° feather at 0.3 m",
        weights_hold(&rig, true, DVec3::X, [0.0; 2]),
    );
    // Control: the lenses turned 15° each, 30° apart, by yaws of
    // opposite sign in their own view frames, and seam directions covered
    // past 90° + h on both.
    let mut apart = Tune::default();
    apart.lenses[0].ypr_deg[0] = 15.0;
    apart.lenses[1].ypr_deg[0] = -15.0;
    let rig = Rig::build(&cal, &geo, &apart, 0, [768; 2]).unwrap();
    let holes = (0..3600)
        .filter(|&j| {
            let d = on_sphere(0.0, f64::from(j) * 0.1 * DEG);
            weights_hold(&rig, true, d, [0.0; 2]).is_err()
        })
        .count();
    let holes = must_fail("lenses 30° apart", at_most(holes, 0));
    eprintln!("lenses 30° apart leave {holes} of 3600 horizon directions unweighed");
}

/// The weights are continuous in the direction.
///
/// On the plain rigs, up the LOD while h stays at 2° or more, at random
/// directions and across the edge of each lens's mask.
#[test]
fn weights_are_continuous() {
    let _time = timed("weights_are_continuous");
    continuous_cases();
    // Control: h never shrunk jumps across the mask's edge at LOD 3 on
    // Pat's V6 with the feather at its bound, 9.06°, where the margin
    // leaves 3.8° past 90°.
    let p = pool()
        .iter()
        .find(|p| p.name == "Pat's V6 radial-only under A, at the feather's bound, [768, 768]")
        .unwrap();
    assert!(p.lod_max >= 3.0);
    let jump = jump_at_the_edge(&p.rig, unshrunk, 0, 0.7, [3.0; 2]);
    let jump = must_fail("h never shrunk", at_most(jump, JUMP));
    eprintln!("unshrunk, the weights jump by {jump:.4} across lens 0's edge at LOD 3");
    assert!(jump_at_the_edge(&p.rig, Rig::weights, 0, 0.7, [3.0; 2]) <= JUMP);
    // Control: on the stereographic rig, the margin's angle at θ_max's
    // slope leaves h too wide for the frame's cut, towards lens 0's left
    // edge, its nearest, at LOD 3.9, where the band's slope keeps h at 2°.
    let p = pool()
        .iter()
        .find(|p| p.name == "the stereographic rig")
        .unwrap();
    let lens = &p.rig.lenses[0];
    assert!(lens.band_texels_per_radian < 0.8 * lens.texels_per_radian);
    assert!(lens.theta_frame < lens.theta_max && p.lod_max >= 3.9);
    let jump = jump_at_the_edge(&p.rig, slope_at_theta_max, 0, PI, [3.9; 2]);
    let jump = must_fail("the margin at θ_max's slope", at_most(jump, JUMP));
    eprintln!(
        "on the stereographic rig, the margin at θ_max's slope makes the weights jump by \
         {jump:.4} at the frame's cut, LOD 3.9"
    );
    assert!(jump_at_the_edge(&p.rig, Rig::weights, 0, PI, [3.9; 2]) <= JUMP);
}

/// The weights are symmetric under a lens swap with a mirrored rig.
#[test]
fn weights_are_symmetric_under_a_mirrored_swap() {
    let _time = timed("weights_are_symmetric_under_a_mirrored_swap");
    mirrored_swap_cases();
    // Control: the lenses swapped without the mirror, d kept.
    let rig = mirrored(&pool()[radial()[0]].rig);
    let d = DVec3::new(0.9, 0.2, 0.3).normalize();
    let w = rig.weights(d, [0.0; 2]);
    must_fail(
        "swapped without the mirror",
        at_most((w[0] - w[1]).abs(), 1e-9),
    );
}

/// A point at R seen with q = 1/R puts both lenses' rays through it
/// within 1e-9 m, the error growing with |q − 1/R|.
#[test]
fn distance_puts_both_rays_through_the_scene_point() {
    let _time = timed("distance_puts_both_rays_through_the_scene_point");
    distance_cases();
    // Control: q = 0 for a point 1.5 m off across the baseline.
    let rig = Rig {
        q: 0.0,
        ..pool()[0].rig
    };
    let off = must_fail(
        "q = 0 against R = 1.5 m",
        at_most(miss(&rig, 0, DVec3::X, 1.5), 1e-9),
    );
    eprintln!(
        "at q = 0 lens 0's ray misses the point 1.5 m away by {:.2} mm",
        off * 1e3
    );
}

/// For rigs within validate's bounds, Pat's V6 numbers under map A among
/// them, every direction with a non-zero weight has its footprint inside
/// its slot by the margin; with the frame term off, some footprints fall
/// outside (the control).
#[test]
fn mask_keeps_weighted_footprints_inside_their_slots() {
    let _time = timed("mask_keeps_weighted_footprints_inside_their_slots");
    let rig = pats_v6_a_at_the_bound();
    eprintln!(
        "Pat's V6 under A: θ_frame {:.3}° and {:.3}°, the feather at its bound {:.3}°",
        rig.lenses[0].theta_frame.to_degrees(),
        rig.lenses[1].theta_frame.to_degrees(),
        rig.feather.to_degrees()
    );
    for lod in [0.0, 1.0, 2.0] {
        assert_eq!(
            outside_near_the_frame(&rig, Rig::weights, lod),
            0,
            "LOD {lod}"
        );
    }
    mask_cases();
    // Control: the frame term off at LOD 1.
    let outside = must_fail(
        "the frame term off",
        at_most(outside_near_the_frame(&rig, frameless, 1.0), 0),
    );
    eprintln!(
        "with the frame term off, {outside} directions near the frame weigh outside at LOD 1"
    );
}

/// `theta_frame` matches a scan of the slot's edges at one sample per
/// texel within 0.01°: 99.1° on lens 0 of Pat's V6 under map A. Random
/// rigs run a fortieth of `PROPTEST_CASES` (at least 16), as each scans
/// every texel of eight edges.
#[test]
fn theta_frame_matches_an_edge_scan() {
    let _time = timed("theta_frame_matches_an_edge_scan");
    let cal = pats();
    for (v6, name) in [
        (V6Reading::RADIAL_ONLY, "radial-only"),
        (EVERY_TERM, "every term"),
    ] {
        for canvas in CanvasMap::all() {
            let geo = placeholder(canvas, Generation::V6, v6);
            let frame = theta_frame(&cal, &geo, &Tune::default()).unwrap();
            for side in [768, 3840] {
                let rig = Rig::build(&cal, &geo, &Tune::default(), 0, [side; 2]).unwrap();
                for (lens, frame) in rig.lenses.iter().zip(frame) {
                    let off = (scanned(lens, &[0, 1, 2, 3]) - lens.theta_frame).abs();
                    assert!(
                        off <= SCAN,
                        "{canvas:?} {name} at {side}: {}°",
                        off.to_degrees()
                    );
                    assert!((frame - lens.theta_frame).abs() <= 1e-8);
                }
            }
            eprintln!(
                "Pat's V6 {name} under {canvas:?}: θ_frame {:.3}° and {:.3}°",
                frame[0].to_degrees(),
                frame[1].to_degrees()
            );
        }
    }
    let rig = pats_v6_a_at_the_bound();
    assert!((rig.lenses[0].theta_frame.to_degrees() - 99.1).abs() <= 0.05);
    scan_cases();
    // Control: the farthest edge alone.
    let lens = &rig.lenses[0];
    let farthest = (0..4).map(|e| scanned(lens, &[e])).fold(0.0, f64::max);
    must_fail(
        "the farthest edge alone",
        at_most((farthest - lens.theta_frame).abs(), SCAN),
    );
}
