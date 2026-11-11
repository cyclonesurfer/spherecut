//! Rigs for the stitching tests (M3 plan 4.2, 8.3): sphere's rig of the
//! camera synth composes, and the frames its lenses' planes make. synth's
//! lens truth and sphere's lens maths share no code (4.1 rule 2), and
//! these tests convert between their types only here, at the boundary,
//! field by field.
//!
//! [`pats`] and the boundary ([`calibration`], [`geometry`],
//! [`quarter_turn`], [`euler_conv`], [`v6_reading`] and [`lens_model`])
//! are a copy of those in `crates/synth/tests/lens_against_sphere.rs`,
//! which holds `Rig::build` to synth's `compose` through them (6.3's
//! Against synth): a change to either is a change to both.
//!
//! `Tune::default()`'s stitch distance is q = 0.5 m⁻¹ (2 m). A test whose
//! scene is infinitely far sets `q_per_m` to 0, or its lenses sit about
//! 0.9° apart across the seam (b · q, 3.3).

use spherecut_core::video::{ChromaSiting, Matrix, Range};
use spherecut_sphere::fisheye::{self, Canonical, KbLens, LensModel, Unified, V6Reading};
use spherecut_sphere::rig::{
    self, Binding, CalibTerms, Calibration, CanvasMap, EulerConv, Extrinsics, Geometry, LensCalib,
    Rig, RigError, RigLens, Status, Tune, TurnAxis, TzSign,
};
use spherecut_synth::lens::{
    self as truth, BlockTruth, CalibTruth, KbBase, Planted, RigTruth, StoredDist, TruthBinding,
    TruthCanvas, TruthEuler, TruthModel, TruthTurn, TruthTz, TruthV6,
};
use spherecut_synth::yuv::YuvPlanes;

use super::{Frame, Kind, color};

/// A block's square on the X5's canvas, and field 27's window.
const SRC: u32 = 5376;
const DST: u32 = 5312;

/// Pat's X5 calibration of one generation, as the string holds it
/// (docs/formats/insta360.md, "Pat's X5 samples"): block 1's cx on the
/// whole canvas.
pub fn pats(generation: truth::Generation) -> CalibTruth {
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
            StoredDist::V6 {
                d: truth::PATS_V6_D,
            },
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
pub fn calibration(cal: &CalibTruth) -> Calibration {
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
pub fn geometry(planted: &Planted, generation: truth::Generation) -> Geometry {
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

pub fn quarter_turn(q: truth::QuarterTurn) -> rig::QuarterTurn {
    match q {
        truth::QuarterTurn::Zero => rig::QuarterTurn::Zero,
        truth::QuarterTurn::Plus90 => rig::QuarterTurn::Plus90,
        truth::QuarterTurn::Minus90 => rig::QuarterTurn::Minus90,
        truth::QuarterTurn::Half => rig::QuarterTurn::Half,
    }
}

/// The `EulerConv` whose fields read as the `TruthEuler`'s do: the same
/// axes and signs by angle, the same order and roll offset.
pub fn euler_conv(e: &TruthEuler) -> EulerConv {
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

pub fn v6_reading(r: TruthV6) -> V6Reading {
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
pub fn lens_model(m: &TruthModel) -> LensModel {
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

// The rigs and their frames.

/// sphere's rig of the camera synth composes from `cal` under `planted`:
/// `Rig::build` of the same numbers through the boundary, reading the
/// blocks of the calibration's own generation, under `tune`.
/// `front_track` and `sides` are those `compose` took.
pub fn sphere_rig(
    cal: &CalibTruth,
    planted: &Planted,
    tune: &Tune,
    front_track: u8,
    sides: [u32; 2],
) -> Result<Rig, RigError> {
    Rig::build(
        &calibration(cal),
        &geometry(planted, cal.generation()),
        tune,
        front_track,
        sides,
    )
}

/// sphere's rig of a truth that no calibration gives, such as
/// `kb_like`'s (4.2's second rig): each of the truth's lenses through
/// `RigLens::new`, with its model through [`lens_model`], its `to_rig`
/// inverted, its centre and its side, calibration block b on track b. Of
/// `tune` it reads each lens's θ_max, the feather and the stitch
/// distance; the truth holds where each lens points and what it shows, so
/// the tune's angles, centre shifts and scales aren't applied. The mask's
/// margin is 2 texels, as `Rig::build` sets it.
pub fn rig_of_truth(truth: &RigTruth, tune: &Tune) -> Rig {
    Rig {
        lenses: std::array::from_fn(|b| {
            let lens = &truth.lenses[b];
            RigLens::new(
                b,
                lens_model(&lens.model),
                lens.to_rig.inverse(),
                lens.centre_m,
                tune.lenses[b].theta_max_deg.to_radians(),
                lens.side,
            )
        }),
        q: tune.q_per_m,
        feather: tune.feather_deg.to_radians(),
        margin_texels: 2.0,
    }
}

/// A lens's planes, as `lens_planes` codes them under `Coding::X5`, as
/// the frame its track's decoder gives: 8-bit I420, `side` × `side`, full
/// range, BT.709, chroma sited left, with no padding after a row.
///
/// # Panics
///
/// If the planes aren't 8-bit, or aren't those of a `side` × `side`
/// 4:2:0 picture.
pub fn lens_frame(planes: &YuvPlanes, side: u32) -> Frame {
    let YuvPlanes::U8(planes) = planes else {
        panic!("lens_frame takes 8-bit planes");
    };
    let layout = Kind::I420.layout();
    let planes = layout
        .planes
        .iter()
        .zip(planes)
        .enumerate()
        .map(|(i, (desc, bytes))| {
            let (w, h) = desc.size(side, side);
            assert_eq!(
                bytes.len(),
                w as usize * h as usize,
                "plane {i} of a {side}² 4:2:0 picture is {w}x{h}"
            );
            (bytes.clone(), w as usize, (w, h))
        })
        .collect();
    Frame {
        layout,
        size: (side, side),
        planes,
        color: color(Range::Full, Matrix::Bt709, ChromaSiting::Left),
    }
}
