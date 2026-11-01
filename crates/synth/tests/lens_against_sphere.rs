//! synth's lens truth against sphere's lens maths (M3 plan 4.1 rule 2,
//! 4.2's rung 0 and 6.3's Against synth): the two share no code, and
//! these tests convert between their types only here, at the boundary,
//! field by field. Pat's numbers are docs/formats/insta360.md's ("Pat's
//! X5 samples"), as the calibration string holds them: block 1's cx on
//! the whole canvas.
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): these two share the 30 s that
//! sphere's properties and rung 0 take at `PROPTEST_CASES=100000` with
//! sphere's `rig.rs` and `unified.rs`, and each prints its time.
//! `PROPTEST_CASES` sets how many random cameras the Against-synth test
//! draws beside its sweep of the small enums (256 by default); rung 0's
//! 10k points a model don't change with it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::f64::consts::PI;
use std::time::Instant;

use glam::{DQuat, DVec2, DVec3};
use spherecut_sphere::fisheye::{self, Canonical, KbLens, LensModel, Unified, V6Reading};
use spherecut_sphere::rig::{
    self, Binding, CalibTerms, Calibration, CanvasMap, EulerConv, Extrinsics, Geometry, LensCalib,
    Rig, Status, Tune, TurnAxis, TzSign,
};
use spherecut_synth::lens::{
    self as truth, BlockTruth, CalibTruth, KbBase, Planted, RigTruth, StoredDist, TruthBinding,
    TruthCanvas, TruthEuler, TruthModel, TruthTurn, TruthTz, TruthV6,
};
use spherecut_testkit::control::{at_most, must_fail};

/// A test's clock, which prints its time as the test ends.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        eprintln!(
            "{}: {:.3} s, of the 30 s that sphere's properties and rung 0 share at 100,000 cases",
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

/// A seeded xorshift generator, so that every run draws the same cameras
/// and points.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn index(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.index(items.len())]
    }
}

/// A degree, in radians.
const DEG: f64 = PI / 180.0;

/// How far apart the two rotations may be, in radians (6.3).
const SAME_TURN: f64 = 1e-12;
/// How far apart the two models may put a direction, in pixels (6.3).
const SAME_PIXEL: f64 = 1e-9;
/// How far apart the two centres may be, in metres (6.3).
const SAME_CENTRE: f64 = 1e-12;

/// Rung 0's tolerances (4.2): sphere's forward of synth's inverse lands
/// this close to the pixel, and sphere's inverse this close to synth's.
const RUNG0_PX: f64 = 1e-6;
const RUNG0_RAD: f64 = 1e-7;
/// Rung 0's points a truth model.
const RUNG0_POINTS: usize = 10_000;

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
const EVERY_TERM: TruthV6 = TruthV6 {
    tangential: Some(truth::Tangential {
        pair: truth::PairOrder::D5D6,
        extension: true,
    }),
    prism: Some(truth::Prism {
        at: truth::PrismAt::D9,
        order: truth::PrismOrder::ByAxis,
    }),
};

/// Pat's X5 calibration of one generation, as the string holds it.
fn pats(generation: truth::Generation) -> CalibTruth {
    let block = |xi, fx, fy, cx, cy, ypr_deg, t_m| BlockTruth {
        xi,
        fx,
        fy,
        cx,
        cy,
        ypr_deg,
        t_m,
    };
    let (blocks, dist) = match generation {
        truth::Generation::V3 => (
            [
                block(
                    2.0,
                    4284.620,
                    4284.280,
                    2701.130,
                    2692.130,
                    [-0.198, 0.211, 90.280],
                    [0.0; 3],
                ),
                block(
                    2.0,
                    4291.030,
                    4291.030,
                    8069.000,
                    2684.950,
                    [0.239, 0.113, 89.107],
                    [-0.004460, 0.000017, -0.031920],
                ),
            ],
            StoredDist::V3 {
                k: [
                    [0.18632051, 2.04984355, -3.24446917],
                    [0.18940836, 2.07073069, -3.24103332],
                ],
                p: [[-0.00143576, -0.00339240], [0.00013831, -0.00299873]],
            },
        ),
        truth::Generation::V6 => (
            [
                block(
                    2.0,
                    4277.520,
                    4277.270,
                    2713.050,
                    2689.210,
                    [-0.310, -0.271, 90.255],
                    [0.0; 3],
                ),
                block(
                    2.0,
                    4284.400,
                    4284.760,
                    8075.730,
                    2680.350,
                    [0.120, -0.173, 89.112],
                    [0.000637, 0.000001, -0.031525],
                ),
            ],
            StoredDist::V6 { d: PATS_D },
        ),
    };
    CalibTruth {
        blocks,
        dist,
        canvas: [2 * SRC, SRC],
        dst: DST,
        lens_type: 113,
        body: 4,
    }
}

// The boundary: synth's types to sphere's, field by field.

/// sphere's calibration for synth's stored numbers, as the camera crate
/// fills it: src is the canvas's height, block 1's cx is made local to
/// its square, and the blocks go under their generation.
fn calibration(cal: &CalibTruth) -> Calibration {
    let src = cal.canvas[1];
    let blocks: [LensCalib; 2] = std::array::from_fn(|b| {
        let block = &cal.blocks[b];
        LensCalib {
            xi: block.xi,
            fx: block.fx,
            fy: block.fy,
            cx: if b == 0 {
                block.cx
            } else {
                block.cx - f64::from(src)
            },
            cy: block.cy,
            ypr_deg: block.ypr_deg,
            t_m: block.t_m,
            terms: match cal.dist {
                StoredDist::V3 { k, p } => CalibTerms::V3 { k: k[b], p: p[b] },
                StoredDist::V6 { d } => CalibTerms::V6 { d: d[b] },
            },
        }
    });
    let (v3, v6) = match cal.generation() {
        truth::Generation::V3 => (Some(blocks), None),
        truth::Generation::V6 => (None, Some(blocks)),
    };
    Calibration {
        src,
        dst: cal.dst,
        v3,
        v6,
    }
}

/// sphere's geometry for synth's planted conventions, reading the blocks
/// of `generation`.
fn geometry(planted: &Planted, generation: truth::Generation) -> Geometry {
    Geometry {
        binding: match planted.binding {
            TruthBinding::Direct => Binding::Direct,
            TruthBinding::Reversed => Binding::Reversed,
        },
        canvas: match planted.canvas {
            TruthCanvas::A => CanvasMap::A,
            TruthCanvas::B => CanvasMap::B,
            TruthCanvas::C => CanvasMap::C,
            TruthCanvas::Fitted { scale } => CanvasMap::Fitted { scale },
        },
        extrinsics: Extrinsics::Stored(euler_conv(&planted.euler)),
        turn: match planted.turn {
            TruthTurn::Y => TurnAxis::Y,
            TruthTurn::X => TurnAxis::X,
        },
        tz: match planted.tz {
            TruthTz::AsStored => TzSign::AsStored,
            TruthTz::Negated => TzSign::Negated,
        },
        upright: quarter_turn(planted.upright),
        generation: match generation {
            truth::Generation::V3 => rig::Generation::V3,
            truth::Generation::V6 => rig::Generation::V6,
        },
        v6: v6_reading(planted.v6),
        status: Status::Provisional,
        evidence: String::new(),
    }
}

fn quarter_turn(q: truth::QuarterTurn) -> rig::QuarterTurn {
    match q {
        truth::QuarterTurn::Zero => rig::QuarterTurn::Zero,
        truth::QuarterTurn::Plus90 => rig::QuarterTurn::Plus90,
        truth::QuarterTurn::Minus90 => rig::QuarterTurn::Minus90,
        truth::QuarterTurn::Half => rig::QuarterTurn::Half,
    }
}

/// The `EulerConv` whose fields read as the `TruthEuler`'s do: the same
/// axes and signs by angle, the same order and roll offset.
fn euler_conv(e: &TruthEuler) -> EulerConv {
    EulerConv {
        axes: e.axes.map(|axis| match axis {
            truth::Axis::X => rig::Axis::X,
            truth::Axis::Y => rig::Axis::Y,
            truth::Axis::Z => rig::Axis::Z,
        }),
        order: e.order.map(|angle| match angle {
            truth::Angle::Yaw => rig::Angle::Yaw,
            truth::Angle::Pitch => rig::Angle::Pitch,
            truth::Angle::Roll => rig::Angle::Roll,
        }),
        signs: e.signs.map(|sign| match sign {
            truth::Sign::Plus => rig::Sign::Plus,
            truth::Sign::Minus => rig::Sign::Minus,
        }),
        roll_offset: quarter_turn(e.roll_offset),
    }
}

fn v6_reading(r: TruthV6) -> V6Reading {
    V6Reading {
        tangential: r.tangential.map(|t| fisheye::Tangential {
            pair: match t.pair {
                truth::PairOrder::D5D6 => fisheye::PairOrder::D5D6,
                truth::PairOrder::D6D5 => fisheye::PairOrder::D6D5,
            },
            extension: t.extension,
        }),
        prism: r.prism.map(|p| fisheye::Prism {
            at: match p.at {
                truth::PrismAt::D7 => fisheye::PrismAt::D7,
                truth::PrismAt::D9 => fisheye::PrismAt::D9,
            },
            order: match p.order {
                truth::PrismOrder::ByAxis => fisheye::PrismOrder::ByAxis,
                truth::PrismOrder::Interleaved => fisheye::PrismOrder::Interleaved,
            },
        }),
    }
}

/// sphere's model for a truth model, term for term: V3's terms through
/// `Canonical::v3`, V6's through `Canonical::v6` under the same reading,
/// and Kannala–Brandt's focals, polynomial, tangential and prism terms
/// each to its own field.
fn lens_model(m: &TruthModel) -> LensModel {
    match *m {
        TruthModel::Mei {
            xi,
            fx,
            fy,
            cx,
            cy,
            k,
            p,
        } => LensModel::Mei(Unified {
            xi,
            fx,
            fy,
            cx,
            cy,
            dist: Canonical::v3(k, p),
        }),
        TruthModel::MeiV6 {
            xi,
            fx,
            fy,
            cx,
            cy,
            d,
            reading,
        } => LensModel::Mei(Unified {
            xi,
            fx,
            fy,
            cx,
            cy,
            dist: Canonical::v6(d, v6_reading(reading)),
        }),
        TruthModel::Kb {
            base,
            fx,
            fy,
            cx,
            cy,
            k,
            p,
            pe,
            s,
        } => LensModel::Kb(KbLens {
            base: match base {
                KbBase::Equidistant => fisheye::Model::Equidistant,
                KbBase::Equisolid => fisheye::Model::Equisolid,
                KbBase::Stereographic => fisheye::Model::Stereographic,
                KbBase::Orthographic => fisheye::Model::Orthographic,
            },
            k,
            fx,
            fy,
            cx,
            cy,
            p,
            pe,
            s,
        }),
    }
}

// The measures, the test's own.

/// The angle of the rotation `q`, in radians: 0 for the identity.
fn turn_angle(q: DQuat) -> f64 {
    2.0 * q.xyz().length().atan2(q.w.abs())
}

/// The angle between two directions, in radians.
fn apart(a: DVec3, b: DVec3) -> f64 {
    a.cross(b).length().atan2(a.dot(b))
}

/// A lens-frame direction θ off the axis at azimuth φ, in degrees.
fn at(theta_deg: f64, phi_deg: f64) -> DVec3 {
    let (st, ct) = (theta_deg * DEG).sin_cos();
    let (sp, cp) = (phi_deg * DEG).sin_cos();
    DVec3::new(st * cp, st * sp, ct)
}

/// The directions the models are compared on: the axis, and every 15°
/// out to 105°, past the rig's θ_max of 100°, at 8 azimuths.
fn directions() -> Vec<DVec3> {
    let mut dirs = vec![DVec3::Z];
    for theta in [15.0, 30.0, 45.0, 60.0, 75.0, 90.0, 100.0, 105.0] {
        for k in 0..8 {
            dirs.push(at(theta, 22.5 + 45.0 * f64::from(k)));
        }
    }
    dirs
}

/// How far `Rig::build` lands from `compose`: the largest gap between
/// the rotations (lens_from_rig against to_rig's inverse), in radians,
/// the centres, in metres, and the models, in pixels; and whether every
/// pair of lenses has the same side.
#[derive(Debug, Clone, Copy, Default)]
struct Gaps {
    turn: f64,
    centre: f64,
    model: f64,
    sides_agree: bool,
}

/// The gaps between `rig` and `truth` over both lenses, block b's lens
/// paired with the truth's lens `track(b)`, the models compared on
/// `dirs`. A direction only one model images is an infinite gap.
fn gaps(rig: &Rig, truth: &RigTruth, dirs: &[DVec3], track: impl Fn(usize) -> usize) -> Gaps {
    let mut g = Gaps {
        sides_agree: true,
        ..Gaps::default()
    };
    for (b, lens) in rig.lenses.iter().enumerate() {
        let t = &truth.lenses[track(b)];
        g.turn = g.turn.max(turn_angle(lens.lens_from_rig * t.to_rig));
        g.centre = g.centre.max((lens.centre_m - t.centre_m).length());
        g.sides_agree &= lens.side == t.side;
        for &d in dirs {
            let gap = match (lens.model.project(d), t.model.forward(d)) {
                (Some(a), Some(b)) => (a - b).length(),
                (None, None) => 0.0,
                _ => f64::INFINITY,
            };
            g.model = g.model.max(gap);
        }
    }
    g
}

impl Gaps {
    fn max(self, other: Gaps) -> Gaps {
        Gaps {
            turn: self.turn.max(other.turn),
            centre: self.centre.max(other.centre),
            model: self.model.max(other.model),
            sides_agree: self.sides_agree && other.sides_agree,
        }
    }
}

/// Builds both cameras and measures them: synth's `compose` of `cal`
/// under `planted`, and sphere's `Rig::build` of the same numbers under
/// `geo` at the default tune, paired by each block's track.
fn compare(
    cal: &CalibTruth,
    planted: &Planted,
    geo: &Geometry,
    front_track: u8,
    sides: [u32; 2],
    dirs: &[DVec3],
) -> Gaps {
    let truth = truth::compose(cal, planted, front_track, sides);
    let rig = Rig::build(&calibration(cal), geo, &Tune::default(), front_track, sides)
        .unwrap_or_else(|e| panic!("{planted:?} at front track {front_track}: {e}"));
    gaps(&rig, &truth, dirs, |b| rig.lenses[b].track)
}

/// Every value of the small enums (6.3), with the X5's placeholder Euler
/// convention for the caller to replace: the binding, canvas maps A, B
/// and C, the turn axis, the tz sign, the upright turn, the generation
/// with V6's 21 readings (V3 reads none), and the front track. 2 · 3 · 2
/// · 2 · 4 · 22 · 2 = 4224 values.
fn smalls() -> Vec<(Planted, truth::Generation, u8)> {
    let readings: Vec<(truth::Generation, TruthV6)> =
        std::iter::once((truth::Generation::V3, TruthV6::RADIAL_ONLY))
            .chain(TruthV6::all().map(|v6| (truth::Generation::V6, v6)))
            .collect();
    let mut all = Vec::new();
    for binding in TruthBinding::all() {
        for canvas in TruthCanvas::all() {
            for turn in TruthTurn::all() {
                for tz in TruthTz::all() {
                    for upright in truth::QuarterTurn::all() {
                        for &(generation, v6) in &readings {
                            for front in [0, 1] {
                                let planted = Planted {
                                    binding,
                                    canvas,
                                    euler: TruthEuler::PLACEHOLDER,
                                    turn,
                                    upright,
                                    tz,
                                    v6,
                                };
                                all.push((planted, generation, front));
                            }
                        }
                    }
                }
            }
        }
    }
    all
}

/// The tracks' sides the sweep builds at: an X5 master's 768² proxy on
/// track 0 and an LRV half's 832 on track 1, so that a side read by
/// block rather than by track shows.
const SIDES: [u32; 2] = [768, 832];

/// Against synth (6.3): at the default tune, `Rig::build` agrees with
/// `synth::lens::compose` on every value of the small enums and on
/// sampled `EulerConv`s: lens_from_rig is the inverse of to_rig within
/// 1e-12, the models within 1e-9 px and the centres within 1e-12 m.
///
/// The sweep takes all 4224 small values on Pat's V3 or V6 numbers, the
/// k-th with the Euler convention 389 k mod 1152 of synth's list, so that
/// every one of the 1152 comes up three or four times; then
/// `PROPTEST_CASES` seeded draws take random stored angles, translations,
/// focals and centres, a random map among them `Fitted` at a random
/// scale, random sides and a random convention. Each block's lens is
/// paired with the truth's for its track. Controls: the wrong V6 reading,
/// lenses paired by block under `Reversed`, and the tz sign crossed.
#[test]
fn rig_build_agrees_with_synth_compose() {
    let _t = timed("rig_build_agrees_with_synth_compose");
    let eulers: Vec<TruthEuler> = TruthEuler::all().collect();
    let convs: HashSet<EulerConv> = eulers.iter().map(euler_conv).collect();
    assert_eq!(
        convs.len(),
        1152,
        "the boundary keeps the Euler space whole"
    );
    let readings: HashSet<V6Reading> = TruthV6::all().map(v6_reading).collect();
    assert_eq!(
        readings.len(),
        21,
        "the boundary keeps the V6 readings whole"
    );
    let dirs = directions();
    let cals = [pats(truth::Generation::V3), pats(truth::Generation::V6)];
    let cal_of = |generation| match generation {
        truth::Generation::V3 => &cals[0],
        truth::Generation::V6 => &cals[1],
    };

    let smalls = smalls();
    assert_eq!(smalls.len(), 4224);
    let mut swept = Gaps {
        sides_agree: true,
        ..Gaps::default()
    };
    for (k, &(planted, generation, front)) in smalls.iter().enumerate() {
        let planted = Planted {
            euler: eulers[k * 389 % 1152],
            ..planted
        };
        let g = compare(
            cal_of(generation),
            &planted,
            &geometry(&planted, generation),
            front,
            SIDES,
            &dirs,
        );
        assert!(
            g.turn <= SAME_TURN && g.centre <= SAME_CENTRE && g.model <= SAME_PIXEL,
            "{planted:?}, {generation:?}, front track {front}: {g:?}"
        );
        assert!(g.sides_agree, "{planted:?}: the sides differ");
        swept = swept.max(g);
    }

    let mut rng = Rng(0x5eed_a6a1_4572_c0de);
    let maps = [TruthCanvas::A, TruthCanvas::B, TruthCanvas::C];
    let mut drawn = Gaps {
        sides_agree: true,
        ..Gaps::default()
    };
    for case in 0..cases() {
        let generation = rng.pick(&[truth::Generation::V3, truth::Generation::V6]);
        let mut cal = pats(generation);
        for block in &mut cal.blocks {
            block.ypr_deg = [0; 3].map(|_| rng.range(-180.0, 180.0));
            block.fx *= rng.range(0.98, 1.02);
            block.fy *= rng.range(0.98, 1.02);
            block.cx += rng.range(-40.0, 40.0);
            block.cy += rng.range(-40.0, 40.0);
        }
        cal.blocks[1].t_m = [0; 3].map(|_| rng.range(-0.05, 0.05));
        let canvas = if rng.index(4) == 0 {
            TruthCanvas::Fitted {
                scale: rng.range(0.9, 1.1),
            }
        } else {
            rng.pick(&maps)
        };
        let planted = Planted {
            binding: rng.pick(&[TruthBinding::Direct, TruthBinding::Reversed]),
            canvas,
            euler: eulers[rng.index(eulers.len())],
            turn: rng.pick(&[TruthTurn::Y, TruthTurn::X]),
            upright: rng.pick(&[
                truth::QuarterTurn::Zero,
                truth::QuarterTurn::Plus90,
                truth::QuarterTurn::Minus90,
                truth::QuarterTurn::Half,
            ]),
            tz: rng.pick(&[TruthTz::AsStored, TruthTz::Negated]),
            v6: TruthV6::all().nth(rng.index(21)).unwrap(),
        };
        let front = rng.pick(&[0, 1]);
        let sides = [0; 2].map(|_| 256 + rng.index(3841) as u32);
        let g = compare(
            &cal,
            &planted,
            &geometry(&planted, generation),
            front,
            sides,
            &dirs,
        );
        assert!(
            g.turn <= SAME_TURN && g.centre <= SAME_CENTRE && g.model <= SAME_PIXEL,
            "case {case}: {planted:?}, {generation:?}, front track {front}, sides {sides:?}, \
             {cal:?}: {g:?}"
        );
        assert!(g.sides_agree, "case {case}: the sides differ");
        drawn = drawn.max(g);
    }
    eprintln!(
        "against synth: the sweep's 4224 agree within {:.2e} rad, {:.2e} m and {:.2e} px; \
         {} draws within {:.2e} rad, {:.2e} m and {:.2e} px",
        swept.turn,
        swept.centre,
        swept.model,
        cases(),
        drawn.turn,
        drawn.centre,
        drawn.model
    );

    // The controls, on Pat's V6 at the placeholder.
    let planted = Planted {
        binding: TruthBinding::Direct,
        canvas: TruthCanvas::A,
        euler: TruthEuler::PLACEHOLDER,
        turn: TruthTurn::Y,
        upright: truth::QuarterTurn::Zero,
        tz: TruthTz::AsStored,
        v6: EVERY_TERM,
    };
    let v6 = truth::Generation::V6;
    let wrong_reading = compare(
        &cals[1],
        &planted,
        &Geometry {
            v6: V6Reading::RADIAL_ONLY,
            ..geometry(&planted, v6)
        },
        0,
        SIDES,
        &dirs,
    );
    let off = must_fail(
        "the wrong V6 reading",
        at_most(wrong_reading.model, SAME_PIXEL),
    );
    eprintln!("the wrong V6 reading: the models {off:.3} px apart");

    let reversed = Planted {
        binding: TruthBinding::Reversed,
        ..planted
    };
    let rig = Rig::build(
        &calibration(&cals[1]),
        &geometry(&reversed, v6),
        &Tune::default(),
        0,
        SIDES,
    )
    .unwrap();
    let by_block = gaps(
        &rig,
        &truth::compose(&cals[1], &reversed, 0, SIDES),
        &dirs,
        |b| b,
    );
    let off = must_fail(
        "lenses paired by block under Reversed",
        at_most(by_block.turn, SAME_TURN),
    );
    eprintln!("paired by block: the rotations {:.1}° apart", off / DEG);

    let crossed = compare(
        &cals[1],
        &planted,
        &Geometry {
            tz: TzSign::Negated,
            ..geometry(&planted, v6)
        },
        0,
        SIDES,
        &dirs,
    );
    let off = must_fail("the tz sign crossed", at_most(crossed.centre, SAME_CENTRE));
    eprintln!("the tz sign crossed: the centres {:.2} mm apart", off * 1e3);
}

/// What rung 0 found on one truth model: the farthest sphere's forward
/// of synth's inverse landed from its pixel, and sphere's inverse from
/// synth's, with how many pixels were drawn to find the points.
#[derive(Debug, Clone, Copy)]
struct Rung0 {
    px: f64,
    rad: f64,
    drawn: usize,
}

/// Rung 0 on one model (4.2): `n` seeded pixels of its slot, of side
/// `side`, at which synth's inverse finds a direction no further than
/// `cap` off the axis. A pixel synth finds none for is drawn again. For
/// each point, sphere's forward of synth's direction against the pixel,
/// and sphere's inverse of the pixel against synth's direction; either
/// with no answer is an infinite gap.
fn rung0(
    truth: &TruthModel,
    sphere: &LensModel,
    side: u32,
    cap: f64,
    n: usize,
    seed: u64,
) -> Rung0 {
    let mut rng = Rng(seed);
    let limit = sphere.limit_theta();
    let s = f64::from(side);
    let mut found = Rung0 {
        px: 0.0,
        rad: 0.0,
        drawn: 0,
    };
    let mut points = 0;
    while points < n {
        found.drawn += 1;
        let px = DVec2::new(rng.range(0.0, s), rng.range(0.0, s));
        let Some(d) = truth.inverse(px) else {
            continue;
        };
        if DVec2::new(d.x, d.y).length().atan2(d.z) > cap {
            continue;
        }
        points += 1;
        let forward = sphere
            .project(d)
            .map_or(f64::INFINITY, |p| (p - px).length());
        let inverse = sphere
            .unproject_within(px, limit)
            .map_or(f64::INFINITY, |e| apart(d, e));
        found.px = found.px.max(forward);
        found.rad = found.rad.max(inverse);
    }
    found
}

/// The θ where rung 0 stops for sphere's `model`: the truth's image
/// circle (106°), or a degree inside the model's `limit_theta`, where
/// the radius stops rising, whichever comes first.
fn cap(model: &LensModel) -> f64 {
    (truth::IMAGE_CIRCLE_DEG * DEG).min(model.limit_theta() - DEG)
}

/// θ_d = θ (1 + k1 θ² + … + k5 θ¹⁰).
fn kb_theta_d(theta: f64, k: [f64; 5]) -> f64 {
    let t2 = theta * theta;
    theta * (1.0 + t2 * (k[0] + t2 * (k[1] + t2 * (k[2] + t2 * (k[3] + t2 * k[4])))))
}

/// Rung 0's truth models, named, with their slots' sides: the X5-like
/// rig's two V3 lenses, composed at 768² under C as 4.2's round trip
/// takes them; Pat's two V6 lenses at 768² under A, read radially and
/// through every term; and Kannala–Brandt lenses of `KB_LIKE_K` on each
/// base on the X5-like lenses' centres, the equidistant ones
/// `kb_like`'s, the others with the focal that takes the image circle
/// (or an orthographic base's 90°) as far along +x as the Mei lens's.
fn rung0_models() -> Vec<(String, TruthModel, u32)> {
    let side = truth::X5_LIKE_SIDE;
    let x5 = truth::compose(
        &truth::x5_like(),
        &Planted {
            binding: TruthBinding::Direct,
            canvas: TruthCanvas::C,
            euler: TruthEuler::PLACEHOLDER,
            turn: TruthTurn::Y,
            upright: truth::QuarterTurn::Zero,
            tz: TruthTz::AsStored,
            v6: TruthV6::RADIAL_ONLY,
        },
        0,
        [side; 2],
    );
    let mut models = Vec::new();
    for (t, lens) in x5.lenses.iter().enumerate() {
        models.push((format!("X5-like V3, track {t}"), lens.model, side));
    }
    for (name, reading) in [
        ("radially", TruthV6::RADIAL_ONLY),
        ("through every term", EVERY_TERM),
    ] {
        let planted = Planted {
            binding: TruthBinding::Direct,
            canvas: TruthCanvas::A,
            euler: TruthEuler::PLACEHOLDER,
            turn: TruthTurn::Y,
            upright: truth::QuarterTurn::Zero,
            tz: TruthTz::AsStored,
            v6: reading,
        };
        let rig = truth::compose(&pats(truth::Generation::V6), &planted, 0, [side; 2]);
        for (t, lens) in rig.lenses.iter().enumerate() {
            models.push((format!("Pat's V6 {name}, track {t}"), lens.model, side));
        }
    }
    let kb = truth::kb_like(&x5);
    let circle = truth::IMAGE_CIRCLE_DEG * DEG;
    let theta_d = kb_theta_d(circle, truth::KB_LIKE_K);
    for t in 0..2 {
        models.push((
            format!("equidistant KB, track {t}"),
            kb.lenses[t].model,
            side,
        ));
        let TruthModel::Mei { cx, cy, .. } = x5.lenses[t].model else {
            unreachable!("the X5-like lenses are V3 Mei");
        };
        let reach = x5.lenses[t]
            .model
            .forward(at(truth::IMAGE_CIRCLE_DEG, 0.0))
            .unwrap()
            .x
            - cx;
        for (base, radius) in [
            (KbBase::Equisolid, 2.0 * (theta_d / 2.0).sin()),
            (KbBase::Stereographic, 2.0 * (theta_d / 2.0).tan()),
            (KbBase::Orthographic, 1.0),
        ] {
            models.push((
                format!("{base:?} KB, track {t}"),
                TruthModel::Kb {
                    base,
                    fx: reach / radius,
                    fy: reach / radius,
                    cx,
                    cy,
                    k: truth::KB_LIKE_K,
                    p: [0.0; 2],
                    pe: [0.0; 2],
                    s: [0.0; 4],
                },
                side,
            ));
        }
    }
    models
}

/// Rung 0 (4.2): sphere's forward of synth's inverse is the identity on
/// 10k seeded points per truth model, within 1e-6 px and 1e-7 rad.
///
/// Every truth model synth renders comes up: V3 Mei, V6 Mei under two
/// readings, and Kannala–Brandt on each of its four bases. The points
/// are seeded pixels of each slot that synth's inverse finds a direction
/// for, up to the image circle or a degree inside the model's limit; and
/// beside the forward, sphere's own inverse of each pixel lands within
/// 1e-7 rad of synth's. Controls: the wrong V6 reading, which both checks
/// see, and equidistant applied to Mei data.
#[test]
fn rung0_sphere_forward_of_synth_inverse_is_the_identity() {
    let _t = timed("rung0_sphere_forward_of_synth_inverse_is_the_identity");
    let models = rung0_models();
    assert_eq!(models.len(), 14);
    for (i, (name, truth, side)) in models.iter().enumerate() {
        let sphere = lens_model(truth);
        let cap = cap(&sphere);
        let found = rung0(
            truth,
            &sphere,
            *side,
            cap,
            RUNG0_POINTS,
            0x0005_eed0 + i as u64,
        );
        eprintln!(
            "rung 0, {name}: within {:.2e} px and {:.2e} rad, out to {:.2}°, {} pixels drawn",
            found.px,
            found.rad,
            cap / DEG,
            found.drawn
        );
        assert!(
            found.px <= RUNG0_PX && found.rad <= RUNG0_RAD,
            "{name}: {found:?}"
        );
    }

    // The controls, on 1000 points each.
    let (_, truth, side) = models
        .iter()
        .find(|(name, ..)| name == "Pat's V6 through every term, track 0")
        .unwrap();
    let TruthModel::MeiV6 { d, reading, .. } = *truth else {
        unreachable!("Pat's V6 lens");
    };
    assert_eq!(reading, EVERY_TERM);
    let LensModel::Mei(unified) = lens_model(truth) else {
        unreachable!("a Mei lens");
    };
    let radially = LensModel::Mei(Unified {
        dist: Canonical::v6(d, V6Reading::RADIAL_ONLY),
        ..unified
    });
    let wrong = rung0(truth, &radially, *side, cap(&radially), 1000, 0x0bad);
    let px = must_fail("the wrong V6 reading, forward", at_most(wrong.px, RUNG0_PX));
    let rad = must_fail(
        "the wrong V6 reading, inverse",
        at_most(wrong.rad, RUNG0_RAD),
    );
    eprintln!("the wrong V6 reading: {px:.3} px and {:.4}° off", rad / DEG);

    let (name, truth, side) = &models[0];
    let TruthModel::Mei { fx, fy, cx, cy, .. } = *truth else {
        unreachable!("{name} is Mei");
    };
    let equidistant = LensModel::Kb(KbLens {
        base: fisheye::Model::Equidistant,
        k: [0.0; 5],
        fx,
        fy,
        cx,
        cy,
        p: [0.0; 2],
        pe: [0.0; 2],
        s: [0.0; 4],
    });
    let wrong = rung0(
        truth,
        &equidistant,
        *side,
        cap(&lens_model(truth)),
        1000,
        0x0bad,
    );
    let px = must_fail(
        "equidistant applied to Mei data",
        at_most(wrong.px, RUNG0_PX),
    );
    eprintln!("equidistant applied to Mei data: {px:.1} px off");
}
