//! Where synth's lens truth meets sphere's lens maths in the tests (the
//! project review's F137, M3 plan amendment 7): the one home of Pat's
//! calibration as synth's truths ([`pats`]) and of the boundary, the
//! conversion from synth's types to sphere's, field by field
//! ([`calibration`], [`geometry`], [`quarter_turn`], [`euler_conv`],
//! [`v6_reading`] and [`lens_model`]), with [`sphere_rig`], `Rig::build`
//! through it.
//!
//! This is test code, and only ever a dev-dependency. synth's library
//! shares no code with sphere's (4.1 rule 2): synth composes each lens
//! from 6.2's text with its own code, and a test converts between the two
//! only here, at the boundary, so that what one gets wrong the other
//! can't mirror. This crate depends on both; neither library depends on
//! it.
//!
//! synth's `tests/lens_against_sphere.rs` holds `Rig::build` to synth's
//! `compose` through these (6.3's Against synth), render's stitching
//! tests re-export them from `tests/common/rig.rs`, and rung 3 (4.2, in
//! agent's tests) and camera's tests take them from here instead of
//! copies of their own. A change here is a change for all of them.
//!
//! [`pats`] parses `spherecut_testkit::pats`, the one copy of Pat's
//! strings, through `testkit::pats::tokens` by the format notes' token
//! layout (`docs/formats/insta360.md` §4.1), with its own code and never
//! camera's parser, which these truths check (4.1 rule 2). Copies of
//! Pat's numbers are left in synth (`src/insv.rs`'s tests and
//! `src/lens.rs`: `PATS_V6_D`, `x5_like`'s focal lengths and terms),
//! sphere (`tests/rig.rs`, `tests/unified.rs`), camera (`tests/strings.rs`,
//! `tests/synth_insv.rs`, `tests/tier2_samples.rs`) and cli
//! (`tests/insv/mod.rs`), each held to `testkit::pats` by a drift test of
//! its own (M3.5's units (a), (b) and (d)).

#![forbid(unsafe_code)]

use spherecut_sphere::fisheye::{self, Canonical, KbLens, LensModel, Unified, V6Reading};
use spherecut_sphere::rig::{
    self, Binding, CalibTerms, Calibration, CanvasMap, EulerConv, Extrinsics, Geometry, LensCalib,
    Rig, RigError, Status, Tune, TurnAxis, TzSign,
};
use spherecut_synth::lens::{
    self as truth, BlockTruth, CalibTruth, KbBase, Planted, StoredDist, TruthBinding, TruthCanvas,
    TruthEuler, TruthModel, TruthTurn, TruthTz, TruthV6,
};
use spherecut_testkit::pats as testkit_pats;

/// A block's square on the X5's canvas, and field 27's window.
pub const SRC: u32 = 5376;
pub const DST: u32 = 5312;

/// Pat's X5 calibration of one generation, as the string holds it
/// (docs/formats/insta360.md, "Pat's X5 samples"): block 1's cx on the
/// whole canvas.
///
/// The numbers are `testkit::pats::V3`'s or `V6`'s, read by insta360.md
/// §4.1's layout: the lens count, then a block a lens of `xi fx fy cx cy
/// yaw pitch roll tx ty tz`, its terms (V3's k1 k2 k3 p1 p2, V6's d0 to
/// d12) and `w h type`, then the packed word, whose bits 8 to 15 are the
/// body. Field 27's window isn't in the strings: dst is [`DST`].
///
/// # Panics
///
/// If the string hasn't two blocks of its generation's tokens, which
/// would make it no calibration of Pat's.
pub fn pats(generation: truth::Generation) -> CalibTruth {
    let (string, terms) = match generation {
        truth::Generation::V3 => (testkit_pats::V3, 5),
        truth::Generation::V6 => (testkit_pats::V6, 13),
    };
    // A block: ξ, the focals, the centre, the angles, t, the terms and
    // w h type.
    let block = 11 + terms + 3;
    let numbers = testkit_pats::tokens(string);
    assert_eq!(
        numbers.len(),
        2 * block + 2,
        "Pat's {generation:?} string has two blocks of {block} tokens"
    );
    let at = |b: usize, k: usize| numbers[1 + b * block + k];
    let term = |b: usize, k: usize| at(b, 11 + k);
    // The integer tokens, which a u32 holds.
    let whole = |v: f64| v as u32;
    let blocks = std::array::from_fn(|b| BlockTruth {
        xi: at(b, 0),
        fx: at(b, 1),
        fy: at(b, 2),
        cx: at(b, 3),
        cy: at(b, 4),
        ypr_deg: [at(b, 5), at(b, 6), at(b, 7)],
        t_m: [at(b, 8), at(b, 9), at(b, 10)],
    });
    let dist = match generation {
        truth::Generation::V3 => StoredDist::V3 {
            k: std::array::from_fn(|b| std::array::from_fn(|k| term(b, k))),
            p: std::array::from_fn(|b| std::array::from_fn(|k| term(b, 3 + k))),
        },
        truth::Generation::V6 => StoredDist::V6 {
            d: std::array::from_fn(|b| std::array::from_fn(|k| term(b, k))),
        },
    };
    let tail = 11 + terms;
    let word = whole(numbers[numbers.len() - 1]);
    CalibTruth {
        blocks,
        dist,
        canvas: [whole(at(0, tail)), whole(at(0, tail + 1))],
        dst: DST,
        lens_type: whole(at(0, tail + 2)),
        body: ((word >> 8) & 0xff) as u8,
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

/// sphere's quarter turn for synth's.
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

/// sphere's V6 reading for synth's, slot for slot.
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
