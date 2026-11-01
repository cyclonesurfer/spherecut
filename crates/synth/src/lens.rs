//! The synthetic camera's lens truth (M3 plan 7.1): the conventions synth
//! plants in a rig, and the calibration numbers it starts from.
//!
//! These are synth's own types, written from the plan's text (6.1 and
//! 6.2), and share no code with `spherecut_sphere`'s lens and rig, which
//! they are there to check (4.1 rule 2): a test converts between the two
//! only at its boundary. [`Planted`] holds one value of each convention
//! the camera's files leave open, and [`CalibTruth`] one calibration's
//! stored numbers. M3.2 prints the calibration strings from the two;
//! [`compose`] builds a [`RigTruth`] from them, each lens's model, `to_rig`
//! and centre, and [`render_lens`] renders what each lens sees through
//! synth's own forward and inverse ([`TruthModel`]), which
//! [`lens_planes`] codes as the X5 does. [`x5_like`] and [`kb_like`] are
//! 4.2's two rigs, and a [`RenderCache`] keeps a test binary's renders
//! to a budget.
//!
//! The frames are 6.2's. A lens frame is Mei's and OpenCV's: +x right in
//! the image, +y down, +z along the optical axis. The rig frame is
//! Spherecut's world (DECISIONS D33): +X right, +Y up, and the front lens
//! looking along −Z. Every frame is right-handed, and a rotation by a
//! positive angle about an axis turns by the right-hand rule.

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::{FRAC_PI_2, PI};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use glam::{DMat2, DQuat, DVec2, DVec3};
use spherecut_core::video::ChromaSiting;

use crate::yuv::{Chroma, YuvFormat, YuvPlanes};
use crate::{Pattern, Rgb, RgbImage};

/// Which track each calibration block describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruthBinding {
    /// Block b reads track b.
    Direct,
    /// Block b reads track 1 − b.
    Reversed,
}

impl TruthBinding {
    pub fn all() -> impl Iterator<Item = TruthBinding> {
        [TruthBinding::Direct, TruthBinding::Reversed].into_iter()
    }
}

/// How a block's numbers, in canvas pixels, scale to a square track of
/// side s. src is the side of a block's square on the canvas (5376 on the
/// X5), dst is field 27's (5312), and c_local is the block's centre less
/// its offset on the canvas (block 1's cx less src).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TruthCanvas {
    /// Focal f · s/dst, centre c_local · s/src.
    A,
    /// Focal f · s/dst, centre (c_local − (src − dst)/2) · s/dst: a
    /// window of dst centred on the block's square.
    B,
    /// Focal f · s/src, centre c_local · s/src: the track shows the whole
    /// square.
    C,
    /// C's focal times `scale`, and C's centre.
    Fitted { scale: f64 },
}

impl TruthCanvas {
    /// A, B and C. `Fitted` takes any scale, so it isn't listed.
    pub fn all() -> impl Iterator<Item = TruthCanvas> {
        [TruthCanvas::A, TruthCanvas::B, TruthCanvas::C].into_iter()
    }
}

/// A coordinate axis of the frame a rotation turns in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    pub fn all() -> impl Iterator<Item = Axis> {
        [Axis::X, Axis::Y, Axis::Z].into_iter()
    }
}

/// One of a block's three stored angles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Angle {
    Yaw,
    Pitch,
    Roll,
}

impl Angle {
    pub fn all() -> impl Iterator<Item = Angle> {
        [Angle::Yaw, Angle::Pitch, Angle::Roll].into_iter()
    }
}

/// The sign a stored angle is taken with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sign {
    Plus,
    Minus,
}

impl Sign {
    pub fn all() -> impl Iterator<Item = Sign> {
        [Sign::Plus, Sign::Minus].into_iter()
    }
}

/// A quarter turn: 0°, +90°, −90° or 180°.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuarterTurn {
    Zero,
    Plus90,
    Minus90,
    Half,
}

impl QuarterTurn {
    pub fn all() -> impl Iterator<Item = QuarterTurn> {
        [
            QuarterTurn::Zero,
            QuarterTurn::Plus90,
            QuarterTurn::Minus90,
            QuarterTurn::Half,
        ]
        .into_iter()
    }

    /// The turn in degrees.
    pub fn degrees(self) -> f64 {
        match self {
            QuarterTurn::Zero => 0.0,
            QuarterTurn::Plus90 => 90.0,
            QuarterTurn::Minus90 => -90.0,
            QuarterTurn::Half => 180.0,
        }
    }
}

/// How a block's stored yaw, pitch and roll compose into its rotation
/// E_b, from the view frame (after F = diag(1, −1, −1)) to the lens frame
/// (6.2). The space is generic, so that no reader's composition is copied
/// or favoured: 6 axis assignments, 6 orders, 8 sign patterns and 4 roll
/// offsets, 1152 values in all. It has no direction field, rig to lens or
/// lens to rig: the inverse of one value is another, its order reversed
/// and every sign flipped.
///
/// 6.2 leaves two of the labels' readings open, and synth fixes them
/// here: a sign goes with its angle, not with its place in the order,
/// and the roll offset is added before the roll's sign is applied. So
/// angle a turns about `axes[a]` by `signs[a]` · (its stored value, plus
/// `roll_offset` when a is the roll). Either way the space holds the same
/// 1152 rotations: the readings decide which value names which, and so
/// which value is the placeholder and how 6.3's Against-synth test pairs
/// each `EulerConv` with a `TruthEuler`. `sphere`'s `EulerConv` reads
/// them the same way, field for field, and M3.3 settled the placeholder
/// as [`TruthEuler::PLACEHOLDER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TruthEuler {
    /// The axis each stored angle turns about, by angle (yaw, pitch,
    /// roll): a permutation of X, Y and Z.
    pub axes: [Axis; 3],
    /// The order the three rotations compose in, first applied first: a
    /// permutation of yaw, pitch and roll. `[a, b, c]` is
    /// E = R_c · R_b · R_a, acting on column vectors.
    pub order: [Angle; 3],
    /// The sign each stored angle is taken with, by angle (yaw, pitch,
    /// roll), whatever the order.
    pub signs: [Sign; 3],
    /// A quarter turn added to the stored roll before its sign is applied.
    /// Upright X5 video stores a roll of about 90° (3.1).
    pub roll_offset: QuarterTurn,
}

impl TruthEuler {
    /// The X5's placeholder until M3.6's fit (5.3, 7.1), "the identity
    /// with the −90° roll offset", as M3.3 settled it: the value whose
    /// every field reads the angles as they are named. All signs are plus;
    /// the order is yaw, pitch, roll, first applied first; and the axes are
    /// those D33 gives the names, carried into the lens frame by F: yaw
    /// about the up-down axis y, pitch about the right axis x, roll about
    /// the optical axis z, about which the X5's sensors sit a quarter turn
    /// (3.1). So E_b = R_z(roll − 90°) · R_x(pitch) · R_y(yaw), the value
    /// of 11.1's worked JSON, and on Pat's angles each block's E_b is
    /// within 1° of the identity.
    pub const PLACEHOLDER: TruthEuler = TruthEuler {
        axes: [Axis::Y, Axis::X, Axis::Z],
        order: [Angle::Yaw, Angle::Pitch, Angle::Roll],
        signs: [Sign::Plus; 3],
        roll_offset: QuarterTurn::Minus90,
    };

    /// All 1152 values.
    pub fn all() -> impl Iterator<Item = TruthEuler> {
        permutations([Axis::X, Axis::Y, Axis::Z])
            .into_iter()
            .flat_map(|axes| {
                permutations([Angle::Yaw, Angle::Pitch, Angle::Roll])
                    .into_iter()
                    .map(move |order| (axes, order))
            })
            .flat_map(|(axes, order)| sign_patterns().map(move |signs| (axes, order, signs)))
            .flat_map(|(axes, order, signs)| {
                QuarterTurn::all().map(move |roll_offset| TruthEuler {
                    axes,
                    order,
                    signs,
                    roll_offset,
                })
            })
    }
}

/// The six orderings of `items`.
fn permutations<T: Copy>([a, b, c]: [T; 3]) -> [[T; 3]; 6] {
    [
        [a, b, c],
        [a, c, b],
        [b, a, c],
        [b, c, a],
        [c, a, b],
        [c, b, a],
    ]
}

/// The eight patterns of three signs.
fn sign_patterns() -> impl Iterator<Item = [Sign; 3]> {
    Sign::all().flat_map(|y| Sign::all().flat_map(move |p| Sign::all().map(move |r| [y, p, r])))
}

/// The axis of the 180° turn T_1 from the rig frame to block 1's view
/// frame, which the strings omit (insta360.md §4.5). Block 0's T_0 is the
/// identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruthTurn {
    Y,
    X,
}

impl TruthTurn {
    pub fn all() -> impl Iterator<Item = TruthTurn> {
        [TruthTurn::Y, TruthTurn::X].into_iter()
    }
}

/// The sign of the stored translation in 6.2's c_1 = rig_from_lens_0 ·
/// (sign(tz) · t_1), lens 1's centre: a sign on the whole vector.
///
/// M3.3 settled that `Negated` flips all of t_1, not only its z. What is
/// open is which way the stored vector points along the baseline, from
/// lens 0 to lens 1 or back, and that negates the whole vector; flipping
/// z alone would be a reflection, which no change between right-handed
/// frames gives either. With c_0 = 0, flipping all of c_1 flips both
/// lenses' c_b − o, so `Negated` at q is exactly `AsStored` at −q, as
/// 8.6's tz rule reads it (under the wrong sign the wall fits q < 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruthTz {
    /// As stored: t_1 is lens 1's centre in lens 0's frame, behind lens 0
    /// on its axis, the physical layout.
    AsStored,
    /// −t_1, all three components: the stored vector read from lens 1 to
    /// lens 0.
    Negated,
}

impl TruthTz {
    pub fn all() -> impl Iterator<Item = TruthTz> {
        [TruthTz::AsStored, TruthTz::Negated].into_iter()
    }
}

/// The order of V6's tangential pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PairOrder {
    /// p1 = d5, p2 = d6.
    D5D6,
    /// p1 = d6, p2 = d5.
    D6D5,
}

impl PairOrder {
    pub fn all() -> impl Iterator<Item = PairOrder> {
        [PairOrder::D5D6, PairOrder::D6D5].into_iter()
    }
}

/// V6's tangential terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tangential {
    pub pair: PairOrder,
    /// Whether d7 and d8 are the pair's r² extensions p1' and p2', in the
    /// pair's order: (d7, d8) after D5D6, (d8, d7) after D6D5.
    pub extension: bool,
}

impl Tangential {
    pub fn all() -> impl Iterator<Item = Tangential> {
        PairOrder::all().flat_map(|pair| {
            [false, true]
                .into_iter()
                .map(move |extension| Tangential { pair, extension })
        })
    }
}

/// Where V6's four thin-prism terms start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrismAt {
    /// d7 to d10, only without a tangential extension, which reads d7 and
    /// d8.
    D7,
    /// d9 to d12.
    D9,
}

impl PrismAt {
    pub fn all() -> impl Iterator<Item = PrismAt> {
        [PrismAt::D7, PrismAt::D9].into_iter()
    }
}

/// How V6's four thin-prism terms, first to fourth, give s1 to s4, where
/// x gains s1 r² + s2 r⁴ and y gains s3 r² + s4 r⁴ (6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrismOrder {
    /// x's from the first and second, y's from the third and fourth.
    ByAxis,
    /// x's from the first and third, y's from the second and fourth.
    Interleaved,
}

impl PrismOrder {
    pub fn all() -> impl Iterator<Item = PrismOrder> {
        [PrismOrder::ByAxis, PrismOrder::Interleaved].into_iter()
    }
}

/// V6's thin-prism terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Prism {
    pub at: PrismAt,
    pub order: PrismOrder,
}

impl Prism {
    pub fn all() -> impl Iterator<Item = Prism> {
        PrismAt::all().flat_map(|at| PrismOrder::all().map(move |order| Prism { at, order }))
    }
}

/// What V6's d5 to d12 mean: 6.1's generic space of 21 values, which
/// names none after a source. d0 to d4 are always the radial terms, and a
/// term no field reads is 0 in the canonical form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TruthV6 {
    pub tangential: Option<Tangential>,
    pub prism: Option<Prism>,
}

impl TruthV6 {
    /// d0 to d4 alone.
    pub const RADIAL_ONLY: TruthV6 = TruthV6 {
        tangential: None,
        prism: None,
    };

    /// All 21 values: 5 tangential readings (none, or a pair order with
    /// or without the extension) by 5 prism readings (none, or a start
    /// and an order), less the 4 whose extension and prism at d7 would
    /// both read d7 and d8.
    pub fn all() -> impl Iterator<Item = TruthV6> {
        let tangential = std::iter::once(None).chain(Tangential::all().map(Some));
        tangential
            .flat_map(|tangential| {
                std::iter::once(None)
                    .chain(Prism::all().map(Some))
                    .map(move |prism| TruthV6 { tangential, prism })
            })
            .filter(|v| v.is_valid())
    }

    /// Whether this is one of the 21: no term read twice, which only a
    /// tangential extension with the prism at d7 does.
    pub fn is_valid(&self) -> bool {
        let extension = self.tangential.is_some_and(|t| t.extension);
        let at_d7 = self.prism.is_some_and(|p| p.at == PrismAt::D7);
        !(extension && at_d7)
    }
}

/// The conventions synth plants: how a camera's stored calibration
/// becomes each lens's model, `to_rig` and centre: one value of each
/// convention 6.2 leaves open. M3.2's `print` and M3.3's `compose` take it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Planted {
    pub binding: TruthBinding,
    pub canvas: TruthCanvas,
    /// How each block's stored angles compose (E_b).
    pub euler: TruthEuler,
    /// The axis of block 1's 180° turn (T_1).
    pub turn: TruthTurn,
    /// The quarter turn U about −Z that the rig applies to both lenses
    /// (6.2's S = U · Y). A rotation common to both, which the seam can't
    /// see; it sets which of a class's twins is upright.
    pub upright: QuarterTurn,
    pub tz: TruthTz,
    /// What V6's d5 to d12 mean; V3's strings don't read it.
    pub v6: TruthV6,
}

/// The two calibration generations synth writes (insta360.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Generation {
    /// 19 tokens a block, Mei with k1 k2 k3 p1 p2 (fields 54 and 56).
    V3,
    /// 27 tokens a block, Mei with d0 to d12 (fields 111 and 112).
    V6,
}

impl Generation {
    pub fn all() -> impl Iterator<Item = Generation> {
        [Generation::V3, Generation::V6].into_iter()
    }
}

/// Both blocks' distortion terms, as their generation stores them, by
/// block. One generation holds both, as one calibration string does, so
/// a calibration whose blocks differ in generation can't be built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StoredDist {
    /// V3: radial k1, k2 and k3, then Brown's tangential p1 and p2.
    V3 { k: [[f64; 3]; 2], p: [[f64; 2]; 2] },
    /// V6: d0 to d12, read through a [`TruthV6`].
    V6 { d: [[f64; 13]; 2] },
}

impl StoredDist {
    /// The generation these terms belong to.
    pub fn generation(&self) -> Generation {
        match self {
            StoredDist::V3 { .. } => Generation::V3,
            StoredDist::V6 { .. } => Generation::V6,
        }
    }

    /// Block `b`'s terms in token order (insta360.md §4.1): k1 k2 k3 p1
    /// p2, or d0 to d12.
    pub fn terms(&self, b: usize) -> Vec<f64> {
        match self {
            StoredDist::V3 { k, p } => k[b].iter().chain(&p[b]).copied().collect(),
            StoredDist::V6 { d } => d[b].to_vec(),
        }
    }
}

/// One calibration block's stored numbers, in the calibration canvas's
/// pixels, degrees and metres, as the string holds them, but for its
/// distortion terms, which [`CalibTruth::dist`] holds for both blocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockTruth {
    /// Mei's ξ (2 on the X5).
    pub xi: f64,
    pub fx: f64,
    pub fy: f64,
    /// The centre on the whole canvas: block 1's cx includes block 0's
    /// width.
    pub cx: f64,
    pub cy: f64,
    /// The stored yaw, pitch and roll, in degrees.
    pub ypr_deg: [f64; 3],
    /// The stored translation tx, ty and tz, in metres (block 0's is 0).
    pub t_m: [f64; 3],
}

/// One camera calibration's stored numbers, a calibration string's worth
/// (insta360.md §4.1, M3 plan 3.1 and 6.1): two blocks and their
/// distortion terms, of one generation, the canvas they are in and field
/// 27's window. Block 0 is the canvas's left; which track each block
/// describes is the planted binding's.
///
/// The generation is the terms' ([`CalibTruth::generation`]), so a
/// printer needs no other. The tracks' side is not here, since it is the
/// recording mode's and not the calibration's: whatever scales these
/// numbers to a track takes it beside them, as `Rig::build` (6.2) takes
/// its `sides`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibTruth {
    pub blocks: [BlockTruth; 2],
    /// Both blocks' distortion terms.
    pub dist: StoredDist,
    /// The whole side-by-side canvas, width and height (10752 × 5376 on
    /// the X5): each block's square is its height on a side.
    pub canvas: [u32; 2],
    /// Field 27's dst, the side of the window the tracks show (5312 on
    /// the X5); its src is a block's square.
    pub dst: u32,
    /// The lens type each block ends with (113 on the X5's bare lens).
    pub lens_type: u32,
    /// The packed word's body code, its bits 8 to 15 (4 on the X5).
    pub body: u8,
}

impl CalibTruth {
    /// The generation both blocks store their terms in.
    pub fn generation(&self) -> Generation {
        self.dist.generation()
    }
}

/// Where the X5's image circle ends, in degrees off each lens's axis
/// (M3 plan 7.1): the `theta_max` [`compose`] gives each lens, so that
/// the square frame cuts the circle on all four sides as the camera's
/// does, while the rig's own θ_max stays 100°.
pub const IMAGE_CIRCLE_DEG: f64 = 106.0;

/// The base a Kannala–Brandt truth applies after its polynomial in θ:
/// synth's own, written from 6.1's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KbBase {
    /// r = θ_d.
    Equidistant,
    /// r = 2 sin(θ_d/2).
    Equisolid,
    /// r = 2 tan(θ_d/2).
    Stereographic,
    /// r = sin θ_d, for θ_d up to 90°.
    Orthographic,
}

impl KbBase {
    pub fn all() -> impl Iterator<Item = KbBase> {
        [
            KbBase::Equidistant,
            KbBase::Equisolid,
            KbBase::Stereographic,
            KbBase::Orthographic,
        ]
        .into_iter()
    }
}

/// A truth lens's projection, in its slot's pixels (7.1), written from
/// 6.1's formulas with synth's own code. For the Mei models a unit
/// lens-frame direction (x, y, z) lands on the plane at m = (x, y)/(z + ξ)
/// and is distorted there; for `Kb` it lands at base(θ_d) (cos φ, sin φ)
/// with θ_d = θ (1 + k1 θ² + … + k5 θ¹⁰). The pixel is (fx xd + cx,
/// fy yd + cy), with fx = fy = f for `Kb`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TruthModel {
    /// V3: radial k1 to k3 and Brown's p1 and p2.
    Mei {
        xi: f64,
        fx: f64,
        fy: f64,
        cx: f64,
        cy: f64,
        k: [f64; 3],
        p: [f64; 2],
    },
    /// V6: d0 to d12, read through `reading`.
    MeiV6 {
        xi: f64,
        fx: f64,
        fy: f64,
        cx: f64,
        cy: f64,
        d: [f64; 13],
        reading: TruthV6,
    },
    /// Kannala–Brandt on a base: docs/PROMPT.md's model, and the
    /// no-calibration fallback (4.2's second rig).
    Kb {
        base: KbBase,
        f: f64,
        cx: f64,
        cy: f64,
        k: [f64; 5],
    },
}

impl TruthModel {
    /// Synth's forward: lens-frame direction to pixel, term by term from
    /// 6.1. `None` where the model has no image (the lift's denominator
    /// at or below 0, or an orthographic base past 90°).
    pub fn forward(&self, d: DVec3) -> Option<DVec2> {
        self.pixel(self.param(d)?)
    }

    /// Synth's inverse, pixel to lens-frame unit direction, which rung 0
    /// holds sphere's forward to (4.2): a damped 2-D Newton on the full
    /// forward with a numeric Jacobian and bisection as its fallback,
    /// deliberately not sphere's bracketed radial Newton, then the
    /// closed-form lift (7.1). `None` where it finds no direction.
    pub fn inverse(&self, px: DVec2) -> Option<DVec3> {
        self.invert(px, PI)
    }

    /// The focal lengths and the centre, in pixels.
    fn intrinsics(&self) -> (DVec2, DVec2) {
        match *self {
            TruthModel::Mei { fx, fy, cx, cy, .. } | TruthModel::MeiV6 { fx, fy, cx, cy, .. } => {
                (DVec2::new(fx, fy), DVec2::new(cx, cy))
            }
            TruthModel::Kb { f, cx, cy, .. } => (DVec2::splat(f), DVec2::new(cx, cy)),
        }
    }

    /// The point w the model distorts for direction `d`: the plane point
    /// m = (x, y)/(z + ξ) of a Mei model, or θ (cos φ, sin φ) for `Kb`,
    /// which [`TruthModel::lift`] turns back into the direction.
    fn param(&self, d: DVec3) -> Option<DVec2> {
        let len = d.length();
        if !(len > 0.0 && len.is_finite()) {
            return None;
        }
        let d = d / len;
        match *self {
            TruthModel::Mei { xi, .. } | TruthModel::MeiV6 { xi, .. } => {
                let denom = d.z + xi;
                (denom > 0.0).then(|| DVec2::new(d.x, d.y) / denom)
            }
            TruthModel::Kb { .. } => {
                let across = DVec2::new(d.x, d.y);
                let off_axis = across.length();
                let theta = off_axis.atan2(d.z);
                Some(if off_axis > 0.0 {
                    across * (theta / off_axis)
                } else {
                    DVec2::new(theta, 0.0)
                })
            }
        }
    }

    /// The distorted, normalised image point for `w`: (xd, yd), which the
    /// focal lengths and centre take to the pixel.
    fn distort(&self, w: DVec2) -> Option<DVec2> {
        match *self {
            TruthModel::Mei { k, p, .. } => Some(brown(
                w,
                [k[0], k[1], k[2], 0.0, 0.0],
                p,
                [0.0; 2],
                [0.0; 4],
            )),
            TruthModel::MeiV6 { d, reading, .. } => {
                let (k, p, pe, s) = v6_terms(&d, reading);
                Some(brown(w, k, p, pe, s))
            }
            TruthModel::Kb { base, k, .. } => {
                let theta = w.length();
                if theta == 0.0 {
                    return Some(DVec2::ZERO);
                }
                let t2 = theta * theta;
                let theta_d = theta
                    * (1.0 + t2 * (k[0] + t2 * (k[1] + t2 * (k[2] + t2 * (k[3] + t2 * k[4])))));
                let r = match base {
                    KbBase::Equidistant => theta_d,
                    KbBase::Equisolid => 2.0 * (theta_d / 2.0).sin(),
                    KbBase::Stereographic if theta_d < PI => 2.0 * (theta_d / 2.0).tan(),
                    KbBase::Orthographic if theta_d <= FRAC_PI_2 => theta_d.sin(),
                    KbBase::Stereographic | KbBase::Orthographic => return None,
                };
                Some(w * (r / theta))
            }
        }
    }

    /// The pixel for `w`.
    fn pixel(&self, w: DVec2) -> Option<DVec2> {
        let (f, c) = self.intrinsics();
        Some(f * self.distort(w)? + c)
    }

    /// The unit direction `w` stands for, in closed form: for a Mei
    /// model the lift η = (ξ + √(1 + (1 − ξ²) ρ²))/(ρ² + 1) with ρ = |m|,
    /// and the direction (η m, η − ξ), `None` where the root's argument
    /// is negative (past the fold); for `Kb`, θ = |w| off the axis,
    /// `None` past π.
    fn lift(&self, w: DVec2) -> Option<DVec3> {
        match *self {
            TruthModel::Mei { xi, .. } | TruthModel::MeiV6 { xi, .. } => {
                let rho2 = w.length_squared();
                let disc = 1.0 + (1.0 - xi * xi) * rho2;
                if disc.is_nan() || disc < 0.0 {
                    return None;
                }
                let eta = (xi + disc.sqrt()) / (rho2 + 1.0);
                Some(DVec3::new(eta * w.x, eta * w.y, eta - xi).normalize())
            }
            TruthModel::Kb { .. } => {
                let theta = w.length();
                if theta.is_nan() || theta > PI {
                    return None;
                }
                if theta == 0.0 {
                    return Some(DVec3::Z);
                }
                let across = w * (theta.sin() / theta);
                Some(DVec3::new(across.x, across.y, theta.cos()))
            }
        }
    }

    /// The model's w at θ off the axis along the unit image direction
    /// `u`, with no distortion: where the bisection looks.
    fn along(&self, theta: f64, u: DVec2) -> DVec2 {
        match *self {
            TruthModel::Mei { xi, .. } | TruthModel::MeiV6 { xi, .. } => {
                u * (theta.sin() / (theta.cos() + xi))
            }
            TruthModel::Kb { .. } => u * theta,
        }
    }

    /// The largest θ [`TruthModel::along`] reaches: for a Mei model with
    /// ξ > 1 the fold at acos(−1/ξ), and with ξ ≤ 1 just short of
    /// acos(−ξ), where z + ξ vanishes; π for `Kb`.
    fn reach(&self) -> f64 {
        match *self {
            TruthModel::Mei { xi, .. } | TruthModel::MeiV6 { xi, .. } => {
                if xi > 1.0 {
                    (-1.0 / xi).acos()
                } else {
                    (-xi).acos() - 1e-6
                }
            }
            TruthModel::Kb { .. } => PI,
        }
    }

    /// Where the Newton starts for the normalised image point `t`: the w
    /// that maps to it with no distortion. A Mei model's plane point is
    /// `t` itself; a `Kb` base inverts in closed form, and `None` past the
    /// base's image (r > 2 for equisolid, r > 1 for orthographic).
    fn start(&self, t: DVec2) -> Option<DVec2> {
        let TruthModel::Kb { base, .. } = *self else {
            return Some(t);
        };
        let r = t.length();
        if r == 0.0 {
            return Some(DVec2::ZERO);
        }
        let theta_d = match base {
            KbBase::Equidistant => r,
            KbBase::Equisolid if r <= 2.0 => 2.0 * (r / 2.0).asin(),
            KbBase::Stereographic => 2.0 * (r / 2.0).atan(),
            KbBase::Orthographic if r <= 1.0 => r.asin(),
            KbBase::Equisolid | KbBase::Orthographic => return None,
        };
        Some(t * (theta_d / r))
    }

    /// The direction seen at `px` no further than `cap` off the axis.
    ///
    /// The Newton runs on the normalised point from where the model
    /// without distortion would put it. Its root is the direction's when
    /// the forward keeps its orientation there (the Jacobian's
    /// determinant positive): the models synth renders fold at most once
    /// as θ grows, so that root is the only one on the image's first
    /// sheet, and a root past the lift's fold or past `cap` means there
    /// is no direction to find. When the Newton finds no root, or one
    /// past a fold of the distortion, the bisection walks out from the
    /// axis along the target's own image direction in 1° steps, while
    /// the image still moves outwards, brackets the target's radius and
    /// halves the bracket; the Newton then starts again from there.
    fn invert(&self, px: DVec2, cap: f64) -> Option<DVec3> {
        let (f, c) = self.intrinsics();
        let t = (px - c) / f;
        let distort = |w: DVec2| self.distort(w);
        let w = newton(distort, t, self.start(t)?)
            .filter(|&(_, det)| det > 0.0)
            .or_else(|| newton(distort, t, self.bisect(t, cap)?).filter(|&(_, det)| det > 0.0))?
            .0;
        let d = self.lift(w)?;
        (off_axis(d) <= cap).then_some(d)
    }

    /// The bisection's w for the normalised point `t`, looking no further
    /// than 1° past `cap`.
    fn bisect(&self, t: DVec2, cap: f64) -> Option<DVec2> {
        let r = t.length();
        if r == 0.0 {
            return Some(DVec2::ZERO);
        }
        let u = t / r;
        let outward = |theta: f64| Some(self.distort(self.along(theta, u))?.dot(u));
        let step = 1f64.to_radians();
        let end = self.reach().min(cap + step);
        let (mut lo, mut at_lo) = (0.0, 0.0);
        while lo < end {
            let hi = (lo + step).min(end);
            let at_hi = outward(hi)?;
            if at_hi < at_lo {
                // The image turned back before reaching t.
                return None;
            }
            if at_hi >= r {
                let (mut a, mut b) = (lo, hi);
                for _ in 0..64 {
                    let mid = 0.5 * (a + b);
                    if outward(mid)? < r {
                        a = mid;
                    } else {
                        b = mid;
                    }
                }
                return Some(self.along(0.5 * (a + b), u));
            }
            (lo, at_lo) = (hi, at_hi);
        }
        None
    }
}

/// The angle of the lens-frame direction `d` off the optical axis +z.
fn off_axis(d: DVec3) -> f64 {
    DVec2::new(d.x, d.y).length().atan2(d.z)
}

/// The Mei distortion of the plane point `m` (6.1): radial
/// R = 1 + k1 r² + … + k5 r¹⁰, Brown's tangential pair p with its r²
/// extension pe (P1 = p1 + p1' r², P2 = p2 + p2' r²), and the thin prism
/// s, s1 r² + s2 r⁴ on x and s3 r² + s4 r⁴ on y.
fn brown(m: DVec2, k: [f64; 5], p: [f64; 2], pe: [f64; 2], s: [f64; 4]) -> DVec2 {
    let (x, y) = (m.x, m.y);
    let r2 = x * x + y * y;
    let r4 = r2 * r2;
    let radial = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
    let p1 = p[0] + pe[0] * r2;
    let p2 = p[1] + pe[1] * r2;
    DVec2::new(
        x * radial + 2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x) + s[0] * r2 + s[1] * r4,
        y * radial + 2.0 * p2 * x * y + p1 * (r2 + 2.0 * y * y) + s[2] * r2 + s[3] * r4,
    )
}

/// What V6's d0 to d12 give `brown` under `reading` (6.1's table): d0 to
/// d4 radial; the pair d5 and d6 in the reading's order, and with the
/// extension d7 and d8 in the same order; the four prism terms from d7
/// or d9, by axis (x's first and second) or interleaved (x's first and
/// third). A term the reading doesn't take is 0.
fn v6_terms(d: &[f64; 13], reading: TruthV6) -> ([f64; 5], [f64; 2], [f64; 2], [f64; 4]) {
    let k = [d[0], d[1], d[2], d[3], d[4]];
    let (mut p, mut pe, mut s) = ([0.0; 2], [0.0; 2], [0.0; 4]);
    if let Some(t) = reading.tangential {
        let (first, second) = match t.pair {
            PairOrder::D5D6 => (0, 1),
            PairOrder::D6D5 => (1, 0),
        };
        p = [d[5 + first], d[5 + second]];
        if t.extension {
            pe = [d[7 + first], d[7 + second]];
        }
    }
    if let Some(prism) = reading.prism {
        let at = match prism.at {
            PrismAt::D7 => 7,
            PrismAt::D9 => 9,
        };
        let q = [d[at], d[at + 1], d[at + 2], d[at + 3]];
        s = match prism.order {
            PrismOrder::ByAxis => q,
            PrismOrder::Interleaved => [q[0], q[2], q[1], q[3]],
        };
    }
    (k, p, pe, s)
}

/// How small the Newton's residual gets before it stops, and how small it
/// must be for the root to count, in normalised image units (at the X5's
/// canvas focal of about 4300 px, 4e-12 and 4e-9 px).
const NEWTON_SETTLED: f64 = 1e-15;
const NEWTON_ROOT: f64 = 1e-12;
/// The Newton's step limit, and the step of its central differences.
const NEWTON_STEPS: usize = 60;
const NEWTON_H: f64 = 1e-6;

/// The root of `f(w) = target` from `start`, and the determinant of `f`'s
/// Jacobian there: Newton's method on the 2-D map with a Jacobian by
/// central differences, each step halved until the residual falls (the
/// damping). `None` when the residual stalls above [`NEWTON_ROOT`], the
/// Jacobian is singular, or `f` has no value at a point it needs.
fn newton(f: impl Fn(DVec2) -> Option<DVec2>, target: DVec2, start: DVec2) -> Option<(DVec2, f64)> {
    let jacobian = |w: DVec2| -> Option<DMat2> {
        let (hx, hy) = (DVec2::new(NEWTON_H, 0.0), DVec2::new(0.0, NEWTON_H));
        Some(DMat2::from_cols(
            (f(w + hx)? - f(w - hx)?) / (2.0 * NEWTON_H),
            (f(w + hy)? - f(w - hy)?) / (2.0 * NEWTON_H),
        ))
    };
    let mut w = start;
    let mut miss = f(w)? - target;
    for _ in 0..NEWTON_STEPS {
        if miss.length() <= NEWTON_SETTLED {
            break;
        }
        let j = jacobian(w)?;
        let det = j.determinant();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let step = -(j.inverse() * miss);
        let mut scale = 1.0;
        let mut moved = false;
        while scale > 1e-9 {
            let next = w + step * scale;
            if let Some(v) = f(next) {
                let next_miss = v - target;
                if next_miss.length() < miss.length() {
                    (w, miss, moved) = (next, next_miss, true);
                    break;
                }
            }
            scale *= 0.5;
        }
        if !moved {
            break;
        }
    }
    // A NaN residual fails this too.
    (miss.length() <= NEWTON_ROOT).then_some(())?;
    Some((w, jacobian(w)?.determinant()))
}

/// One lens of a [`RigTruth`]: what a track shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensTruth {
    /// The projection, in the slot's pixels.
    pub model: TruthModel,
    /// The rotation from the lens frame to the rig frame, the inverse of
    /// `sphere::rig`'s `lens_from_rig`.
    pub to_rig: DQuat,
    /// The lens's centre from the rig's origin in metres, 6.2's c_b − o,
    /// with o midway between the lenses.
    pub centre_m: DVec3,
    /// Where the image circle ends, in radians off the axis: black beyond,
    /// as on the camera.
    pub theta_max: f64,
    /// The square track's side.
    pub side: u32,
    /// A gain per channel, in linear light.
    pub gain: [f64; 3],
    /// The falloff, in linear light: 1 + v1 θ² + v2 θ⁴ + v3 θ⁶ with θ in
    /// radians off the axis. All 0 for none.
    pub vignette: [f64; 3],
}

/// A synthetic camera (7.1): two lenses, the scene they see and how the
/// rig moves through it.
#[derive(Debug, Clone, PartialEq)]
pub struct RigTruth {
    /// By track: `lenses[t]` is what track t shows. A test that compares
    /// with `sphere::rig::Rig`, whose lenses go by block, pairs each by
    /// the block's track.
    pub lenses: [LensTruth; 2],
    /// The radius of the scene's sphere about the rig's origin, in
    /// metres, or `None` for an infinitely far scene.
    pub scene_radius_m: Option<f64>,
    /// A ground plane this far below the rig's origin, in metres (8.4's
    /// ground model, 8.5): a ray that meets it before the scene shows the
    /// pattern in the direction of the point it meets.
    pub ground_m: Option<f64>,
    pub motion: Motion,
    /// How late each track's lens exposes, in frames: a lens late by Δt
    /// sees the rig as it is Δt later (3.4).
    pub late_frames: [f64; 2],
    /// The rolling shutter's readout, in seconds: row y of a lens's
    /// picture is exposed (y + ½)/side of it after its frame's time.
    pub readout_s: f64,
}

/// How the rig turns through a still scene over a clip: at rest, but for
/// turns at constant rates (8.5's pans between rests).
#[derive(Debug, Clone, PartialEq)]
pub struct Motion {
    /// The frame rate, which turns frame numbers and `late_frames` into
    /// seconds.
    pub fps: f64,
    /// The turns, in time order and none overlapping. Empty for a rig
    /// that holds still.
    pub turns: Vec<Turn>,
}

impl Motion {
    /// A rig that holds still, at the X5's 29.97 frames a second.
    pub fn still() -> Motion {
        Motion {
            fps: 30000.0 / 1001.0,
            turns: Vec::new(),
        }
    }

    /// The rig's orientation at `t_s` seconds, the rotation from the rig
    /// frame to the scene's: the identity before the first turn. Each
    /// turn, about an axis in the scene's frame, applies after those
    /// before it, for as much of it as has passed by `t_s`.
    pub fn at(&self, t_s: f64) -> DQuat {
        self.turns.iter().fold(DQuat::IDENTITY, |q, turn| {
            let held = (t_s - turn.start_s).clamp(0.0, (turn.end_s - turn.start_s).max(0.0));
            DQuat::from_axis_angle(turn.axis.normalize(), (turn.deg_per_s * held).to_radians()) * q
        })
    }

    /// Which rest the rig holds through all of `from_s` to `to_s`: k
    /// after k turns, ends included, or `None` when a turn moves it in
    /// between. [`Motion::at`] gives the same orientation, to the bit,
    /// anywhere in one rest.
    pub fn rest(&self, from_s: f64, to_s: f64) -> Option<usize> {
        let mut after = f64::NEG_INFINITY;
        for (k, turn) in self.turns.iter().enumerate() {
            if from_s >= after && to_s <= turn.start_s {
                return Some(k);
            }
            after = turn.end_s;
        }
        (from_s >= after).then_some(self.turns.len())
    }
}

/// One turn of the rig: about `axis` (a unit vector in the scene's
/// frame) at `deg_per_s`, by the right-hand rule, from `start_s` to
/// `end_s` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Turn {
    pub start_s: f64,
    pub end_s: f64,
    pub axis: DVec3,
    pub deg_per_s: f64,
}

/// The synthetic camera a calibration and a planted convention give
/// (7.1), composed from 6.2's text with glam's axis-angle quaternions and
/// never through `sphere::rig` (4.1 rule 2): each lens's model in its
/// slot's pixels under the planted canvas map, its `to_rig` from the
/// stored angles under the planted Euler convention and roll offset, T_b,
/// S and U, and its centre from c_1 under the planted tz sign. The
/// binding puts block b's lens on its track, and when `front_track` is
/// block 1's track both lenses turn 180° about +Y. `sides` are the
/// tracks' sides, by track. The rest is a test's to set: the result's
/// scene is infinitely far with no ground, the rig still, both lenses on
/// time with no rolling shutter, unit gains, no vignette, and each
/// `theta_max` at [`IMAGE_CIRCLE_DEG`].
///
/// Block b's lens frame is reached from the rig frame as 6.2 writes it at
/// the default tune, lens_from_rig_b = E_b · F · T_b · S: first S = U · Y
/// (Y the turn for the front track, U the planted quarter turn about −Z),
/// then T_b (none for block 0, the planted 180° turn for block 1), then F
/// = diag(1, −1, −1), a half turn about x, and last E_b. `to_rig` is its
/// inverse. Lens 1's centre is c_1 = to_rig_0 · (±t_1), the sign the
/// planted tz's, lens 0's is c_0 = 0, and each `centre_m` is c_b less
/// their midpoint.
///
/// # Panics
///
/// If `front_track` is not 0 or 1.
pub fn compose(cal: &CalibTruth, planted: &Planted, front_track: u8, sides: [u32; 2]) -> RigTruth {
    assert!(
        front_track < 2,
        "front_track {front_track}: a pair has tracks 0 and 1"
    );
    let track = |b: usize| match planted.binding {
        TruthBinding::Direct => b,
        TruthBinding::Reversed => 1 - b,
    };
    let y = if usize::from(front_track) == track(1) {
        DQuat::from_axis_angle(DVec3::Y, PI)
    } else {
        DQuat::IDENTITY
    };
    let u = DQuat::from_axis_angle(DVec3::NEG_Z, planted.upright.degrees().to_radians());
    let s = u * y;
    let f = DQuat::from_axis_angle(DVec3::X, PI);
    let turn_axis = match planted.turn {
        TruthTurn::Y => DVec3::Y,
        TruthTurn::X => DVec3::X,
    };
    let t = [DQuat::IDENTITY, DQuat::from_axis_angle(turn_axis, PI)];
    let to_rig: [DQuat; 2] = std::array::from_fn(|b| {
        let e = euler_rotation(&planted.euler, cal.blocks[b].ypr_deg);
        (e * f * t[b] * s).inverse()
    });
    let sign = match planted.tz {
        TruthTz::AsStored => 1.0,
        TruthTz::Negated => -1.0,
    };
    let c1 = to_rig[0] * (DVec3::from(cal.blocks[1].t_m) * sign);
    let centres = [-c1 / 2.0, c1 / 2.0];
    let mut by_block: [LensTruth; 2] = std::array::from_fn(|b| {
        let side = sides[track(b)];
        LensTruth {
            model: slot_model(cal, b, planted, side),
            to_rig: to_rig[b],
            centre_m: centres[b],
            theta_max: IMAGE_CIRCLE_DEG.to_radians(),
            side,
            gain: [1.0; 3],
            vignette: [0.0; 3],
        }
    });
    if planted.binding == TruthBinding::Reversed {
        by_block.swap(0, 1);
    }
    RigTruth {
        lenses: by_block,
        scene_radius_m: None,
        ground_m: None,
        motion: Motion::still(),
        late_frames: [0.0; 2],
        readout_s: 0.0,
    }
}

/// E_b for stored yaw, pitch and roll in degrees under `euler`, from the
/// view frame (after F) to the lens frame: angle a turns about
/// `axes[a]` by `signs[a]` · (its stored value, plus the roll offset
/// when a is the roll), and `order` composes the three, first applied
/// first.
fn euler_rotation(euler: &TruthEuler, ypr_deg: [f64; 3]) -> DQuat {
    let turn = |angle: Angle| {
        let i = match angle {
            Angle::Yaw => 0,
            Angle::Pitch => 1,
            Angle::Roll => 2,
        };
        let offset = match angle {
            Angle::Roll => euler.roll_offset.degrees(),
            Angle::Yaw | Angle::Pitch => 0.0,
        };
        let sign = match euler.signs[i] {
            Sign::Plus => 1.0,
            Sign::Minus => -1.0,
        };
        let axis = match euler.axes[i] {
            Axis::X => DVec3::X,
            Axis::Y => DVec3::Y,
            Axis::Z => DVec3::Z,
        };
        DQuat::from_axis_angle(axis, (sign * (ypr_deg[i] + offset)).to_radians())
    };
    let [first, second, third] = euler.order;
    turn(third) * turn(second) * turn(first)
}

/// Block b's model in the pixels of a square track of side `side`, under
/// the planted canvas map (6.2's table): src is a block's square on the
/// canvas (its height), dst field 27's window, and the centre is taken
/// in the block's own square, block 1's less src.
///
/// The stored centre is read as counting from the canvas's pixel
/// corners, as synth's own pixels do (pixel (i, j) covers i to i + 1 and
/// j to j + 1, [`render_lens`]), so it scales as the table writes it, c ·
/// s/src under A and C, with no half-pixel shift; `sphere::rig` reads it
/// the same way. Whether the X5 counts its cx and cy from pixel corners
/// or, as OpenCV does, from pixel centres is a camera assumption the plan
/// doesn't list: from centres, the slot's centre would sit 0.5 · s/src
/// (0.5 · s/dst under B) further along each axis, 0.36 px at 3840² and
/// 0.07 px at 768².
fn slot_model(cal: &CalibTruth, b: usize, planted: &Planted, side: u32) -> TruthModel {
    let block = &cal.blocks[b];
    let s = f64::from(side);
    let src = f64::from(cal.canvas[1]);
    let dst = f64::from(cal.dst);
    let local = DVec2::new(block.cx - b as f64 * src, block.cy);
    let (focal, centre) = match planted.canvas {
        TruthCanvas::A => (s / dst, local * (s / src)),
        TruthCanvas::B => (
            s / dst,
            (local - DVec2::splat((src - dst) / 2.0)) * (s / dst),
        ),
        TruthCanvas::C => (s / src, local * (s / src)),
        TruthCanvas::Fitted { scale } => (s / src * scale, local * (s / src)),
    };
    let (xi, fx, fy) = (block.xi, block.fx * focal, block.fy * focal);
    let (cx, cy) = (centre.x, centre.y);
    match cal.dist {
        StoredDist::V3 { k, p } => TruthModel::Mei {
            xi,
            fx,
            fy,
            cx,
            cy,
            k: k[b],
            p: p[b],
        },
        StoredDist::V6 { d } => TruthModel::MeiV6 {
            xi,
            fx,
            fy,
            cx,
            cy,
            d: d[b],
            reading: planted.v6,
        },
    }
}

/// What track `lens` shows of `pattern` at `frame`, side × side, each
/// pixel the mean of `ss` × `ss` samples (7.1): synth's inverse, `to_rig`
/// under the rig's motion at the sample's own time, the ray from
/// `centre_m` to the scene's sphere or the ground, [`crate::color`], the
/// gain and the vignette in linear light, then the sRGB OETF. Black past
/// `theta_max` and where the inverse has no direction, as on the camera.
///
/// Pixel (i, j) covers i to i + 1 and j to j + 1 in the model's pixels,
/// so its centre is (i + ½, j + ½), and its samples sit where
/// [`crate::render_view_ideal`]'s do. The mean is of display values, as
/// there, so with unit gains and no vignette, when the trip through
/// linear light is skipped, each sample is the pattern's own value. Row
/// j is exposed at [`exposed_at`]'s time, every sample in it alike. Rows
/// are dealt to threads in turn, and the result doesn't depend on how
/// many there are.
pub fn render_lens(pattern: Pattern, rig: &RigTruth, lens: usize, frame: u32, ss: u32) -> RgbImage {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(16);
    lens_on_threads(pattern, rig, lens, frame, ss, threads)
}

/// [`render_lens`] on `threads` threads.
fn lens_on_threads(
    pattern: Pattern,
    rig: &RigTruth,
    lens: usize,
    frame: u32,
    ss: u32,
    threads: usize,
) -> RgbImage {
    let truth = &rig.lenses[lens];
    let side = truth.side;
    let n = ss.max(1);
    let inv = 1.0 / f64::from(n * n);
    let mut pixels = vec![[0.0; 3]; side as usize * side as usize];
    let mut dealt: Vec<Vec<(usize, &mut [Rgb])>> =
        (0..threads.max(1)).map(|_| Vec::new()).collect();
    for (j, row) in pixels.chunks_mut(side.max(1) as usize).enumerate() {
        let k = j % dealt.len();
        dealt[k].push((j, row));
    }
    std::thread::scope(|s| {
        for rows in dealt {
            s.spawn(move || {
                for (j, row) in rows {
                    let turned = rig.motion.at(exposed_at(rig, lens, frame, j as u32));
                    let to_scene = turned * truth.to_rig;
                    let centre = turned * truth.centre_m;
                    for (i, px) in row.iter_mut().enumerate() {
                        let mut acc = [0.0; 3];
                        for b in 0..n {
                            for a in 0..n {
                                let at = DVec2::new(
                                    i as f64 + (f64::from(a) + 0.5) / f64::from(n),
                                    j as f64 + (f64::from(b) + 0.5) / f64::from(n),
                                );
                                let Some(d) = truth.model.invert(at, truth.theta_max) else {
                                    continue;
                                };
                                let seen = sees(rig, centre, to_scene * d);
                                let c =
                                    expose(crate::color(pattern, seen, frame), truth, off_axis(d));
                                for (acc, c) in acc.iter_mut().zip(c) {
                                    *acc += c;
                                }
                            }
                        }
                        *px = acc.map(|v| v * inv);
                    }
                }
            });
        }
    });
    RgbImage {
        width: side,
        height: side,
        pixels,
    }
}

/// When row `row` of track `lens`'s picture of `frame` is exposed, in
/// seconds: the frame's time, the lens's lateness after it, then the
/// rolling shutter's share of its readout, (row + ½)/side of it.
pub fn exposed_at(rig: &RigTruth, lens: usize, frame: u32, row: u32) -> f64 {
    let side = f64::from(rig.lenses[lens].side);
    (f64::from(frame) + rig.late_frames[lens]) / rig.motion.fps
        + rig.readout_s * (f64::from(row) + 0.5) / side
}

/// The point a ray from `centre` along `d`, both in the scene's frame,
/// meets: on the ground plane when it meets that first, else on the
/// scene's sphere about the origin. With neither, the scene is infinitely
/// far and the ray's own direction is what it sees. Either way the
/// pattern is read in the direction of the result.
fn sees(rig: &RigTruth, centre: DVec3, d: DVec3) -> DVec3 {
    let d = d.normalize();
    let mut reach = f64::INFINITY;
    if let Some(r) = rig.scene_radius_m {
        // |centre + s d| = r, for the s > 0 of a centre inside the sphere.
        let along = centre.dot(d);
        let disc = along * along - (centre.length_squared() - r * r);
        if disc >= 0.0 {
            reach = disc.sqrt() - along;
        }
    }
    if let Some(h) = rig.ground_m
        && d.y < 0.0
    {
        let s = (-h - centre.y) / d.y;
        if s > 0.0 && s < reach {
            reach = s;
        }
    }
    if reach.is_finite() {
        centre + d * reach
    } else {
        d
    }
}

/// A sample of display value `c` after the lens's gain and vignette,
/// applied in linear light θ off the axis and clipped to white, as a
/// sensor clips. Unit gains with no vignette leave it as it is.
fn expose(c: Rgb, lens: &LensTruth, theta: f64) -> Rgb {
    if lens.gain == [1.0; 3] && lens.vignette == [0.0; 3] {
        return c;
    }
    let t2 = theta * theta;
    let [v1, v2, v3] = lens.vignette;
    let falloff = 1.0 + t2 * (v1 + t2 * (v2 + t2 * v3));
    std::array::from_fn(|k| srgb_oetf((srgb_eotf(c[k]) * lens.gain[k] * falloff).clamp(0.0, 1.0)))
}

/// The sRGB transfer function's inverse: display value to linear light.
fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB transfer function: linear light to display value.
fn srgb_oetf(l: f64) -> f64 {
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Whether `pattern` changes from frame to frame: `Grid`'s marker and
/// counter move, and the other scenes hold still.
fn moves(pattern: Pattern) -> bool {
    match pattern {
        Pattern::Grid => true,
        Pattern::HighFrequency | Pattern::Seam | Pattern::SeamLines => false,
    }
}

/// A test binary's renders (7.1), keyed by rig truth, scene and frame,
/// and held to a budget in bytes. A still scene through a rig at rest is
/// rendered once: its frames differ only by what is added after
/// rendering, the frame codes (M3.1's `synth::code`) and the noise. So
/// the key holds the frame only when the scene moves or a turn moves the
/// rig during the frame's exposure, and otherwise the rest it falls in
/// ([`Motion::rest`]). Each key renders once, even when threads ask for
/// it together, for as long as the cache holds it.
///
/// A picture holds 24 bytes a pixel (f64 RGB), 13.5 MiB at 768², and a
/// moving rig is keyed by frame, so the cache holds no more than its
/// budget, [`RenderCache::BUDGET`] (1 GiB, 75 pictures at 768²) unless
/// [`RenderCache::with_budget`] sets another. When a new picture would
/// take it past the budget, the pictures asked for least recently go
/// first; one larger than the whole budget is rendered each time it is
/// asked for and never kept. The budget bounds what the cache holds: a
/// picture a caller still holds stays alive until the caller drops it,
/// and one pushed out while it was being rendered is rendered again when
/// it is next asked for.
///
/// [`RenderCache::shared`] is for pictures that more than one test asks
/// for, as 8.3's attribution matrix shares rung 1's moving renders. A
/// test whose pictures no other test asks for, such as a sweep through a
/// moving rig's frames that reads each once, or one that holds many
/// pictures at once, calls [`render_lens`] directly or keeps a
/// [`RenderCache::new`] of its own, so as not to push out the pictures
/// the others share.
pub struct RenderCache {
    held: Mutex<Held>,
    budget: usize,
    made: AtomicU64,
}

/// What a [`RenderCache`] holds.
#[derive(Default)]
struct Held {
    renders: HashMap<String, Kept>,
    /// The keys by when they were last asked for, the least recent first.
    by_use: BTreeMap<u64, String>,
    bytes: usize,
    /// Counts the asks, to order `by_use`.
    clock: u64,
}

/// One picture a [`RenderCache`] holds, or is rendering: its cell, its
/// size in bytes, and the count of the ask that last asked for it.
struct Kept {
    cell: Arc<OnceLock<Arc<RgbImage>>>,
    bytes: usize,
    used: u64,
}

impl Held {
    /// The cell for `key`, now the most recently asked for: the one held,
    /// or a new one of `bytes`, for which the least recently asked for
    /// make room under `budget`.
    fn ask(&mut self, key: String, bytes: usize, budget: usize) -> Arc<OnceLock<Arc<RgbImage>>> {
        self.clock += 1;
        let now = self.clock;
        if let Some(kept) = self.renders.get_mut(&key) {
            self.by_use.remove(&kept.used);
            kept.used = now;
            self.by_use.insert(now, key);
            return Arc::clone(&kept.cell);
        }
        while self.bytes + bytes > budget {
            let Some((_, oldest)) = self.by_use.pop_first() else {
                break;
            };
            if let Some(gone) = self.renders.remove(&oldest) {
                self.bytes -= gone.bytes;
            }
        }
        let cell = Arc::default();
        self.renders.insert(
            key.clone(),
            Kept {
                cell: Arc::clone(&cell),
                bytes,
                used: now,
            },
        );
        self.by_use.insert(now, key);
        self.bytes += bytes;
        cell
    }
}

impl std::fmt::Debug for RenderCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderCache")
            .field("held", &self.held())
            .field("held_bytes", &self.held_bytes())
            .field("budget", &self.budget)
            .field("made", &self.made())
            .finish()
    }
}

impl Default for RenderCache {
    fn default() -> RenderCache {
        RenderCache::new()
    }
}

impl RenderCache {
    /// The budget a cache holds to unless it is given another: 1 GiB.
    pub const BUDGET: usize = 1 << 30;

    /// A cache of [`RenderCache::BUDGET`].
    pub fn new() -> RenderCache {
        RenderCache::with_budget(RenderCache::BUDGET)
    }

    /// A cache that holds at most `bytes` of pictures.
    pub fn with_budget(bytes: usize) -> RenderCache {
        RenderCache {
            held: Mutex::new(Held::default()),
            budget: bytes,
            made: AtomicU64::new(0),
        }
    }

    /// The test binary's own cache, of [`RenderCache::BUDGET`].
    pub fn shared() -> &'static RenderCache {
        static SHARED: OnceLock<RenderCache> = OnceLock::new();
        SHARED.get_or_init(RenderCache::new)
    }

    /// [`render_lens`]'s picture, rendered the first time it is asked for
    /// and kept while the budget allows.
    pub fn render(
        &self,
        pattern: Pattern,
        rig: &RigTruth,
        lens: usize,
        frame: u32,
        ss: u32,
    ) -> Arc<RgbImage> {
        let render = || {
            self.made.fetch_add(1, Ordering::Relaxed);
            Arc::new(render_lens(pattern, rig, lens, frame, ss))
        };
        let bytes = picture_bytes(rig.lenses[lens].side);
        if bytes > self.budget {
            return render();
        }
        let cell = self.lock().ask(
            render_key(pattern, rig, lens, frame, ss),
            bytes,
            self.budget,
        );
        cell.get_or_init(render).clone()
    }

    /// How many pictures this cache has rendered.
    pub fn made(&self) -> u64 {
        self.made.load(Ordering::Relaxed)
    }

    /// How many pictures it holds, those being rendered among them.
    pub fn held(&self) -> usize {
        self.lock().renders.len()
    }

    /// How many bytes of pictures it holds, those being rendered among
    /// them: never more than its budget.
    pub fn held_bytes(&self) -> usize {
        self.lock().bytes
    }

    /// Its budget, in bytes.
    pub fn budget(&self) -> usize {
        self.budget
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The bytes a [`render_lens`] picture of side `side` holds.
fn picture_bytes(side: u32) -> usize {
    let side = side as usize;
    side * side * std::mem::size_of::<Rgb>()
}

/// A render's key in a [`RenderCache`]: the rig truth whole, by its
/// `Debug` form, which writes every field and each number exactly, the
/// scene, the lens and the samples, and when the frame's picture is
/// taken.
fn render_key(pattern: Pattern, rig: &RigTruth, lens: usize, frame: u32, ss: u32) -> String {
    let side = rig.lenses[lens].side;
    let first = exposed_at(rig, lens, frame, 0);
    let last = exposed_at(rig, lens, frame, side.saturating_sub(1));
    let when = match rig.motion.rest(first, last) {
        Some(rest) if !moves(pattern) => format!("rest {rest}"),
        _ => format!("frame {frame}"),
    };
    format!("{pattern:?} lens {lens} ss {ss} {when} {rig:?}")
}

/// How a lens's picture becomes a track's planes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coding {
    /// The matrix, the range and the subsampling.
    pub format: YuvFormat,
    /// Where each chroma sample sits.
    pub siting: ChromaSiting,
    /// 8 or 10.
    pub bits: u32,
}

impl Coding {
    /// The X5's (3.1): 8-bit 4:2:0, full range, BT.709, chroma sited left.
    pub const X5: Coding = Coding {
        format: YuvFormat {
            kr: 0.2126,
            kb: 0.0722,
            full_range: true,
            chroma: Chroma::Yuv420,
        },
        siting: ChromaSiting::Left,
        bits: 8,
    };
}

/// A lens's picture as the planes its track holds under `coding`, chroma
/// taken at the coding's siting.
///
/// Each chroma sample is a triangle filter of the full-resolution Cb and
/// Cr around its site, 2^s luma samples wide on each side for 2^s luma
/// samples per chroma sample, so its weights' centre is the site: under
/// X5's left siting [¼, ½, ¼] across, on the left column of its pair,
/// and [⅛, ⅜, ⅜, ⅛] down, midway between its two rows. Samples past an
/// edge repeat the edge's. The sides should be multiples of the
/// subsampling.
pub fn lens_planes(img: &RgbImage, coding: Coding) -> YuvPlanes {
    let format = &coding.format;
    let (w, h) = (img.width as usize, img.height as usize);
    let (sx, sy) = format.chroma.shift();
    let (cw, ch) = (w >> sx, h >> sy);
    let (ox, oy) = coding.siting.offset();
    let ypbpr: Vec<(f64, f64, f64)> = img.pixels.iter().map(|&c| format.rgb_to_ypbpr(c)).collect();
    let luma: Vec<u16> = ypbpr
        .iter()
        .map(|&(y, _, _)| format.quant(y, 0.0, 0.0, coding.bits).0)
        .collect();
    let (mut cb, mut cr) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for cy in 0..ch {
        let rows = chroma_taps(cy, sy, oy, h);
        for cx in 0..cw {
            let cols = chroma_taps(cx, sx, ox, w);
            let (mut pb, mut pr) = (0.0, 0.0);
            for &(y, wy) in &rows {
                for &(x, wx) in &cols {
                    let (_, b, r) = ypbpr[y * w + x];
                    pb += wy * wx * b;
                    pr += wy * wx * r;
                }
            }
            let (_, u, v) = format.quant(0.0, pb, pr, coding.bits);
            cb.push(u);
            cr.push(v);
        }
    }
    if coding.bits == 8 {
        YuvPlanes::U8([luma, cb, cr].map(|p| p.into_iter().map(|v| v as u8).collect()))
    } else {
        YuvPlanes::U16([luma, cb, cr])
    }
}

/// The luma samples, across or down, that chroma sample `c` is made from
/// and their weights, for 2^`shift` luma samples per chroma sample: a
/// triangle 2^shift wide each side of the site, c · 2^shift + `offset` ·
/// (2^shift − 1) (`ChromaSiting::offset`), clamped to the `len` samples
/// there are and summing to 1.
fn chroma_taps(c: usize, shift: u32, offset: f64, len: usize) -> Vec<(usize, f64)> {
    if shift == 0 {
        return vec![(c.min(len.saturating_sub(1)), 1.0)];
    }
    let n = f64::from(1u32 << shift);
    let site = c as f64 * n + offset * (n - 1.0);
    let first = (site - n).floor() as i64 + 1;
    let last = (site + n).ceil() as i64 - 1;
    let last_index = len.saturating_sub(1) as i64;
    let taps: Vec<(usize, f64)> = (first..=last)
        .map(|x| {
            let weight = (1.0 - (x as f64 - site).abs() / n).max(0.0);
            (x.clamp(0, last_index) as usize, weight)
        })
        .filter(|&(_, weight)| weight > 0.0)
        .collect();
    let total: f64 = taps.iter().map(|&(_, weight)| weight).sum();
    taps.into_iter()
        .map(|(x, weight)| (x, weight / total))
        .collect()
}

/// The side of the X5-like truth's square tracks (4.2).
pub const X5_LIKE_SIDE: u32 = 768;

/// The X5-like calibration of 4.2's round trip, in the X5's canvas
/// (10752 × 5376, field 27's window 5312): V3 Mei with ξ = 2 and Pat's V3
/// focal lengths and Brown coefficients (insta360.md, "Pat's X5
/// samples"), rolls of 90.5° and 89.3°, yaw and pitch of a few tenths of
/// a degree on each lens, principal points 2 to 3 px off the slot's
/// centre at 768² under maps A and C (lens 0 at (+2.4, −2.1) px, lens 1
/// at (−2.7, +2.2) px; B moves them under 0.05 px more), and t_1 with
/// tz = −32 mm. [`compose`]d at [`X5_LIKE_SIDE`] under C, its frame
/// reaches 99.8° to 102.8° off the axis along the image's axes, about the
/// rig's 200°, and its circle, which ends at [`IMAGE_CIRCLE_DEG`], has
/// r(106°) of 393 to 399 px by lens and direction (7.1 rounds it to 395),
/// past the half side of 384 on all four sides, as the camera's is. Under
/// A each r is 1.2% longer.
pub fn x5_like() -> CalibTruth {
    // At 768² under C a canvas pixel is 1/7 of a slot pixel, so the
    // centres are 2688 + 7 · offset, and block 1's 5376 further.
    CalibTruth {
        blocks: [
            BlockTruth {
                xi: 2.0,
                fx: 4284.62,
                fy: 4284.28,
                cx: 2704.8,
                cy: 2673.3,
                ypr_deg: [0.3, -0.2, 90.5],
                t_m: [0.0; 3],
            },
            BlockTruth {
                xi: 2.0,
                fx: 4291.03,
                fy: 4291.03,
                cx: 8045.1,
                cy: 2703.4,
                ypr_deg: [-0.25, 0.35, 89.3],
                t_m: [-0.00446, 0.000017, -0.032],
            },
        ],
        dist: StoredDist::V3 {
            k: [
                [0.18632051, 2.04984355, -3.24446917],
                [0.18940836, 2.07073069, -3.24103332],
            ],
            p: [[-0.00143576, -0.00339240], [0.00013831, -0.00299873]],
        },
        canvas: [10752, 5376],
        dst: 5312,
        lens_type: 113,
        body: 4,
    }
}

/// The distortion of [`kb_like`]'s lenses, k1 to k5.
pub const KB_LIKE_K: [f64; 5] = [0.012, -0.0021, 0.0003, 0.0, 0.0];

/// 4.2's second rig: `rig` with each lens an equidistant Kannala–Brandt
/// lens of [`KB_LIKE_K`] on the same centre, its focal set so that at
/// [`IMAGE_CIRCLE_DEG`] it reaches as far along +x as the lens it
/// replaces. The rest, `to_rig` and the centres among it, is `rig`'s.
/// It shows that the source path is not Mei's alone.
pub fn kb_like(rig: &RigTruth) -> RigTruth {
    let mut kb = rig.clone();
    let theta = IMAGE_CIRCLE_DEG.to_radians();
    let t2 = theta * theta;
    let k = KB_LIKE_K;
    let theta_d = theta * (1.0 + t2 * (k[0] + t2 * (k[1] + t2 * (k[2] + t2 * (k[3] + t2 * k[4])))));
    for lens in &mut kb.lenses {
        let (f, c) = lens.model.intrinsics();
        let edge = DVec3::new(theta.sin(), 0.0, theta.cos());
        let reach = lens
            .model
            .forward(edge)
            .map_or(f.x * theta, |px| px.x - c.x);
        lens.model = TruthModel::Kb {
            base: KbBase::Equidistant,
            f: reach / theta_d,
            cx: c.x,
            cy: c.y,
            k,
        };
    }
    kb
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fmt::Debug;
    use std::hash::Hash;

    use spherecut_testkit::bench;
    use spherecut_testkit::control::{at_most, must_fail};

    use super::*;
    use crate::AXIS_MARKERS;
    use crate::yuv::to_yuv;

    /// How many distinct values `values` holds, or the first one it holds
    /// twice.
    fn distinct<T: Eq + Hash + Debug + Copy>(values: &[T]) -> Result<usize, T> {
        let mut seen = HashSet::new();
        for v in values {
            if !seen.insert(*v) {
                return Err(*v);
            }
        }
        Ok(seen.len())
    }

    /// Whether `three` holds each of `of` once.
    fn is_permutation<T: PartialEq>(three: &[T; 3], of: [T; 3]) -> bool {
        of.iter()
            .all(|x| three.iter().filter(|y| *y == x).count() == 1)
    }

    #[test]
    fn the_euler_space_has_1152_distinct_values() {
        let all: Vec<TruthEuler> = TruthEuler::all().collect();
        assert_eq!(distinct(&all), Ok(1152));
        for e in &all {
            assert!(
                is_permutation(&e.axes, [Axis::X, Axis::Y, Axis::Z]),
                "{e:?}"
            );
            assert!(
                is_permutation(&e.order, [Angle::Yaw, Angle::Pitch, Angle::Roll]),
                "{e:?}"
            );
        }
        // Each field takes each of its values equally often.
        let count = |f: &dyn Fn(&TruthEuler) -> bool| all.iter().filter(|e| f(e)).count();
        assert_eq!(count(&|e| e.axes[2] == Axis::Z), 1152 / 3);
        assert_eq!(count(&|e| e.order[0] == Angle::Roll), 1152 / 3);
        assert_eq!(count(&|e| e.signs == [Sign::Minus; 3]), 1152 / 8);
        assert_eq!(count(&|e| e.roll_offset == QuarterTurn::Minus90), 1152 / 4);
        // Controls: a value listed twice, and an axis used twice.
        let mut twice = all.clone();
        twice.push(all[700]);
        assert_eq!(
            must_fail("a value listed twice", distinct(&twice)),
            all[700]
        );
        must_fail(
            "an axis used twice",
            is_permutation(&[Axis::X, Axis::X, Axis::Z], [Axis::X, Axis::Y, Axis::Z]),
        );
    }

    /// The d terms a reading takes, by index: 0 to 4 always, then its
    /// pair, extension and prism.
    fn terms_read(v: &TruthV6) -> Vec<usize> {
        let mut read: Vec<usize> = (0..5).collect();
        if let Some(t) = v.tangential {
            read.extend([5, 6]);
            if t.extension {
                read.extend([7, 8]);
            }
        }
        if let Some(p) = v.prism {
            let first = match p.at {
                PrismAt::D7 => 7,
                PrismAt::D9 => 9,
            };
            read.extend(first..first + 4);
        }
        read
    }

    /// Whether `v` reads each of d0 to d12 at most once.
    fn reads_each_once(v: &TruthV6) -> Result<(), usize> {
        let read = terms_read(v);
        match (0..13).find(|i| read.iter().filter(|r| *r == i).count() > 1) {
            Some(i) => Err(i),
            None => Ok(()),
        }
    }

    #[test]
    fn the_v6_space_has_21_distinct_values() {
        let all: Vec<TruthV6> = TruthV6::all().collect();
        assert_eq!(distinct(&all), Ok(21));
        assert!(all.contains(&TruthV6::RADIAL_ONLY));
        for v in &all {
            assert_eq!(reads_each_once(v), Ok(()), "{v:?}");
        }
        // The whole product is 25: the four left out read d7 twice, and
        // is_valid says so too.
        let product: Vec<TruthV6> = std::iter::once(None)
            .chain(Tangential::all().map(Some))
            .flat_map(|tangential| {
                std::iter::once(None)
                    .chain(Prism::all().map(Some))
                    .map(move |prism| TruthV6 { tangential, prism })
            })
            .collect();
        assert_eq!(distinct(&product), Ok(25));
        let left_out: Vec<&TruthV6> = product.iter().filter(|v| !all.contains(v)).collect();
        assert_eq!(left_out.len(), 4);
        for v in left_out {
            assert!(!v.is_valid());
            // Control: each reads d7 twice, from the extension and the
            // prism.
            assert_eq!(
                must_fail("an extension with the prism at d7", reads_each_once(v)),
                7
            );
        }
        // Control: a reading listed twice.
        let mut twice = all.clone();
        twice.push(TruthV6::RADIAL_ONLY);
        assert_eq!(
            must_fail("a reading listed twice", distinct(&twice)),
            TruthV6::RADIAL_ONLY
        );
    }

    #[test]
    fn the_small_spaces_list_each_value_once() {
        fn count<T: Eq + Hash + Debug + Copy>(values: impl Iterator<Item = T>) -> Result<usize, T> {
            distinct(&values.collect::<Vec<T>>())
        }
        assert_eq!(count(TruthBinding::all()), Ok(2));
        assert_eq!(count(Axis::all()), Ok(3));
        assert_eq!(count(Angle::all()), Ok(3));
        assert_eq!(count(Sign::all()), Ok(2));
        assert_eq!(count(QuarterTurn::all()), Ok(4));
        assert_eq!(count(TruthTurn::all()), Ok(2));
        assert_eq!(count(TruthTz::all()), Ok(2));
        assert_eq!(count(PairOrder::all()), Ok(2));
        assert_eq!(count(Tangential::all()), Ok(4));
        assert_eq!(count(PrismAt::all()), Ok(2));
        assert_eq!(count(PrismOrder::all()), Ok(2));
        assert_eq!(count(Prism::all()), Ok(4));
        assert_eq!(count(Generation::all()), Ok(2));
        // TruthCanvas holds an f64, so it is compared by value.
        let canvas: Vec<TruthCanvas> = TruthCanvas::all().collect();
        assert_eq!(canvas, [TruthCanvas::A, TruthCanvas::B, TruthCanvas::C]);
        let degrees: Vec<f64> = QuarterTurn::all().map(QuarterTurn::degrees).collect();
        assert_eq!(degrees, [0.0, 90.0, -90.0, 180.0]);
    }

    /// The first block whose terms, in token order, aren't `want`'s.
    fn terms_differ(dist: &StoredDist, want: [&[f64]; 2]) -> Result<(), usize> {
        match (0..2).find(|&b| dist.terms(b) != want[b]) {
            Some(b) => Err(b),
            None => Ok(()),
        }
    }

    #[test]
    fn stored_terms_name_their_generation_and_block() {
        // Pat's V3 and V6 strings, the same in all 7 masters and 7 LRVs
        // (insta360.md, "Pat's X5 samples", its calibration strings).
        // Each number is its token as stored, less its trailing zeros:
        // none is rounded.
        let (k, p) = (
            [
                [0.18632051, 2.04984355, -3.24446917],
                [0.18940836, 2.07073069, -3.24103332],
            ],
            [[-0.00143576, -0.00339240], [0.00013831, -0.00299873]],
        );
        let v3_terms: [&[f64]; 2] = [
            &[
                0.18632051,
                2.04984355,
                -3.24446917,
                -0.00143576,
                -0.00339240,
            ],
            &[0.18940836, 2.07073069, -3.24103332, 0.00013831, -0.00299873],
        ];
        let blocks = [
            BlockTruth {
                xi: 2.0,
                fx: 4284.62,
                fy: 4284.28,
                cx: 2701.13,
                cy: 2692.13,
                ypr_deg: [-0.198, 0.211, 90.28],
                t_m: [0.0; 3],
            },
            BlockTruth {
                xi: 2.0,
                fx: 4291.03,
                fy: 4291.03,
                cx: 8069.0,
                cy: 2684.95,
                ypr_deg: [0.239, 0.113, 89.107],
                t_m: [-0.00446, 0.000017, -0.03192],
            },
        ];
        let cal = CalibTruth {
            blocks,
            dist: StoredDist::V3 { k, p },
            canvas: [10752, 5376],
            dst: 5312,
            lens_type: 113,
            body: 4,
        };
        assert_eq!(cal.generation(), Generation::V3);
        assert_eq!(terms_differ(&cal.dist, v3_terms), Ok(()));
        let d = [
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
        // Only the terms are Pat's V6 string's: the blocks (xi, focal,
        // centre, angles and t) stay V3's, which is all this test needs.
        let v6 = CalibTruth {
            dist: StoredDist::V6 { d },
            ..cal
        };
        assert_eq!(v6.generation(), Generation::V6);
        assert_eq!(terms_differ(&v6.dist, [&d[0], &d[1]]), Ok(()));
        // Control: the blocks' terms swapped, so that each block would
        // read the other's.
        let swapped = StoredDist::V3 {
            k: [k[1], k[0]],
            p: [p[1], p[0]],
        };
        assert_eq!(
            must_fail(
                "each block with the other's terms",
                terms_differ(&swapped, v3_terms)
            ),
            0
        );
    }

    /// Pat's V3 lens 0 in a 768² slot under map C: focal and centre
    /// times 768/5376 (insta360.md, "Pat's X5 samples").
    fn pats_v3_at_768() -> TruthModel {
        let s = 768.0 / 5376.0;
        TruthModel::Mei {
            xi: 2.0,
            fx: 4284.62 * s,
            fy: 4284.28 * s,
            cx: 2701.13 * s,
            cy: 2692.13 * s,
            k: [0.18632051, 2.04984355, -3.24446917],
            p: [-0.00143576, -0.00339240],
        }
    }

    /// Pat's V6 lens 0's d0 to d12.
    const PATS_V6_D0: [f64; 13] = [
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
    ];

    /// Pat's V6 lens 0 in a 768² slot under map C, read through
    /// `reading`.
    fn pats_v6_at_768(reading: TruthV6) -> TruthModel {
        let s = 768.0 / 5376.0;
        TruthModel::MeiV6 {
            xi: 2.0,
            fx: 4277.52 * s,
            fy: 4277.27 * s,
            cx: 2713.05 * s,
            cy: 2689.21 * s,
            d: PATS_V6_D0,
            reading,
        }
    }

    /// A Kannala–Brandt lens on `base` in a 768² slot, its circle about
    /// as wide as the X5's.
    fn kb_at_768(base: KbBase) -> TruthModel {
        TruthModel::Kb {
            base,
            f: 230.0,
            cx: 386.4,
            cy: 381.9,
            k: [0.012, -0.0021, 0.0003, -0.00002, 0.000001],
        }
    }

    /// Every truth model the tests invert, with how far off the axis each
    /// is checked: Pat's V3, Pat's V6 under each of the 21 readings, and
    /// a KB lens on each base, the orthographic one short of where its
    /// base ends.
    fn every_model() -> Vec<(TruthModel, f64)> {
        let mut models = vec![(pats_v3_at_768(), 106.0)];
        models.extend(TruthV6::all().map(|v| (pats_v6_at_768(v), 106.0)));
        models.extend(KbBase::all().map(|base| {
            let reach = if base == KbBase::Orthographic {
                80.0
            } else {
                106.0
            };
            (kb_at_768(base), reach)
        }));
        models
    }

    /// The unit direction θ off the axis at azimuth φ, in degrees.
    fn lens_dir(theta: f64, phi: f64) -> DVec3 {
        let (st, ct) = theta.to_radians().sin_cos();
        let (sp, cp) = phi.to_radians().sin_cos();
        DVec3::new(st * cp, st * sp, ct)
    }

    /// The angle between two directions, in radians, accurate at small
    /// angles.
    fn angle(a: DVec3, b: DVec3) -> f64 {
        a.cross(b).length().atan2(a.dot(b))
    }

    /// The worst of `inverse(forward(d))` against d, in radians, and of
    /// `forward(inverse(px))` against px, in pixels, for `invert` as the
    /// inverse, over a grid of directions up to `reach` degrees off the
    /// axis. Infinite when the inverse finds nothing.
    fn round_trip(
        model: &TruthModel,
        reach: f64,
        invert: impl Fn(DVec2) -> Option<DVec3>,
    ) -> (f64, f64) {
        let (mut rad, mut px) = (0.0f64, 0.0f64);
        let thetas = [0.0, 0.5, 10.0, 30.0, 60.0, 75.0, 90.0, 100.0, reach];
        for theta in thetas.into_iter().filter(|&t| t <= reach) {
            for phi in [0.0, 37.0, 90.0, 151.0, 200.0, 290.0] {
                let d = lens_dir(theta, phi);
                let at = model.forward(d).unwrap();
                match invert(at) {
                    Some(back) => {
                        rad = rad.max(angle(back, d));
                        px = px.max((model.forward(back).unwrap() - at).length());
                    }
                    None => return (f64::INFINITY, f64::INFINITY),
                }
            }
        }
        (rad, px)
    }

    /// Synth's inverse inverts its forward on every truth model, each V6
    /// reading and each KB base among them.
    #[test]
    fn truth_inverse_inverts_forward() {
        for (model, reach) in every_model() {
            let (rad, px) = round_trip(&model, reach, |p| model.inverse(p));
            assert!(rad <= 1e-12, "{model:?}: {rad:e} rad");
            assert!(px <= 1e-9, "{model:?}: {px:e} px");
        }
        // Past the X5-like lens's fold at 120° there is no direction: the
        // slot's corner lies 543 px from the centre, beyond the 412 px
        // the lens reaches.
        assert_eq!(pats_v3_at_768().inverse(DVec2::new(0.5, 0.5)), None);
        let ortho = kb_at_768(KbBase::Orthographic);
        assert_eq!(ortho.inverse(DVec2::new(386.4 + 240.0, 381.9)), None);
        // The bisection finds what the Newton does, from where it starts.
        let model = pats_v6_at_768(TruthV6::RADIAL_ONLY);
        for theta in [5.0, 50.0, 99.0] {
            let px = model.forward(lens_dir(theta, 63.0)).unwrap();
            let t = (px - DVec2::new(2713.05, 2689.21) * (768.0 / 5376.0))
                / (DVec2::new(4277.52, 4277.27) * (768.0 / 5376.0));
            let from = model.bisect(t, PI).unwrap();
            let (w, det) = newton(|w| model.distort(w), t, from).unwrap();
            assert!(det > 0.0);
            assert!(angle(model.lift(w).unwrap(), lens_dir(theta, 63.0)) <= 1e-12);
        }
        // Controls: the undistorted guess alone, and another V6 reading's
        // inverse, miss by far more.
        let (_, guess_px) = round_trip(&model, 106.0, |p| {
            let (f, c) = model.intrinsics();
            model.lift(model.start((p - c) / f)?)
        });
        assert!(must_fail("no Newton", at_most(guess_px, 1e-9)) > 1.0);
        let sourced = pats_v6_at_768(TruthV6 {
            tangential: Some(Tangential {
                pair: PairOrder::D5D6,
                extension: true,
            }),
            prism: Some(Prism {
                at: PrismAt::D9,
                order: PrismOrder::ByAxis,
            }),
        });
        let (rad, _) = round_trip(&sourced, 106.0, |p| model.inverse(p));
        assert!(must_fail("another reading's inverse", at_most(rad, 1e-12)) > 1e-4);
    }

    /// Synth's forward on values worked out by hand: the centre on the
    /// axis, tan θ at ξ = 0 and tan(θ/2) at ξ = 1 with no distortion, and
    /// each KB base.
    #[test]
    fn truth_forward_lands_where_worked_by_hand() {
        let close = |a: DVec2, b: [f64; 2]| (a - DVec2::from(b)).length() < 1e-9;
        let mei = |xi: f64, k: [f64; 3], p: [f64; 2]| TruthModel::Mei {
            xi,
            fx: 200.0,
            fy: 210.0,
            cx: 100.5,
            cy: 99.5,
            k,
            p,
        };
        // The axis lands on the centre whatever the distortion.
        for (model, _) in every_model() {
            assert_eq!(model.forward(DVec3::Z), Some(model.intrinsics().1));
        }
        // ξ = 0 is the pinhole: θ = 30° along +x lands at tan 30° =
        // 0.5773503, so u = 100.5 + 200 · 0.5773503 = 215.9700538.
        let pinhole = mei(0.0, [0.0; 3], [0.0; 2]);
        assert!(close(
            pinhole.forward(lens_dir(30.0, 0.0)).unwrap(),
            [215.9700538379, 99.5]
        ));
        // It sees nothing behind it: z + ξ = cos 100° < 0.
        assert_eq!(pinhole.forward(lens_dir(100.0, 0.0)), None);
        // ξ = 1 is the stereographic tan(θ/2): θ = 90° along +y, the
        // direction (0, 1, 0), lands at m = (0, 1/(0 + 1)) = (0, 1), so
        // v = 99.5 + 210 · 1 = 309.5.
        let stereo = mei(1.0, [0.0; 3], [0.0; 2]);
        assert!(close(stereo.forward(DVec3::Y).unwrap(), [100.5, 309.5]));
        // The direction (2, 2, 1)/3 lands at m = (2/3)/(1/3 + 1) = 0.5 on
        // each axis at ξ = 1: r² = 0.5, r⁴ = 0.25, r⁶ = 0.125. With k =
        // (0.1, 0.01, 0.001), R = 1 + 0.05 + 0.0025 + 0.000125 = 1.052625;
        // with p = (0.002, 0.003),
        //   xd = 0.5 R + 2 p1 · 0.25 + p2 (0.5 + 2 · 0.25)
        //      = 0.5263125 + 0.001 + 0.003 = 0.5303125,
        //   yd = 0.5 R + 2 p2 · 0.25 + p1 (0.5 + 2 · 0.25)
        //      = 0.5263125 + 0.0015 + 0.002 = 0.5298125,
        // so u = 100.5 + 200 xd = 206.5625, v = 99.5 + 210 yd = 210.760625.
        let corner = DVec3::new(2.0, 2.0, 1.0) / 3.0;
        let v3 = mei(1.0, [0.1, 0.01, 0.001], [0.002, 0.003]);
        assert!(close(v3.forward(corner).unwrap(), [206.5625, 210.760625]));
        // The same point through a V6 reading, {D6D5 with extension, D9
        // Interleaved}, with d = (0.1, 0.01, 0.001, 0.0001, 0.00001, then
        // 0.005 to 0.012 by 0.001): R = 1 + 0.05 + 0.0025 + 0.000125 +
        // 0.00000625 + 0.0000003125 = 1.0526315625. The pair reversed
        // gives p1 = d6 = 0.006, p2 = d5 = 0.005, and its extension p1' =
        // d8 = 0.008, p2' = d7 = 0.007, so P1 = 0.006 + 0.008 · 0.5 =
        // 0.010 and P2 = 0.005 + 0.007 · 0.5 = 0.0085. The prism from d9,
        // interleaved, gives x s1 = d9 = 0.009 and s2 = d11 = 0.011, and y
        // s3 = d10 = 0.010 and s4 = d12 = 0.012. So
        //   xd = 0.52631578125 + 2 · 0.010 · 0.25 + 0.0085 · 1
        //        + 0.009 · 0.5 + 0.011 · 0.25 = 0.54706578125,
        //   yd = 0.52631578125 + 2 · 0.0085 · 0.25 + 0.010 · 1
        //        + 0.010 · 0.5 + 0.012 · 0.25 = 0.54856578125,
        // and u = 100.5 + 200 xd = 209.91315625, v = 99.5 + 210 yd =
        // 214.6988140625.
        let d = [
            0.1, 0.01, 0.001, 0.0001, 0.00001, 0.005, 0.006, 0.007, 0.008, 0.009, 0.010, 0.011,
            0.012,
        ];
        let v6 = |order: PrismOrder| TruthModel::MeiV6 {
            xi: 1.0,
            fx: 200.0,
            fy: 210.0,
            cx: 100.5,
            cy: 99.5,
            d,
            reading: TruthV6 {
                tangential: Some(Tangential {
                    pair: PairOrder::D6D5,
                    extension: true,
                }),
                prism: Some(Prism {
                    at: PrismAt::D9,
                    order,
                }),
            },
        };
        let want = [209.91315625, 214.6988140625];
        assert!(close(
            v6(PrismOrder::Interleaved).forward(corner).unwrap(),
            want
        ));
        // Control: by axis, s2 and s3 trade places.
        must_fail(
            "the prism by axis",
            close(v6(PrismOrder::ByAxis).forward(corner).unwrap(), want),
        );
        // KB bases at θ = 60° along +x, f = 100, centre (50, 60), no
        // distortion: r = π/3 = 1.0471976, 2 sin 30° = 1, 2 tan 30° =
        // 1.1547005 and sin 60° = 0.8660254.
        let kb = |base: KbBase, k1: f64| TruthModel::Kb {
            base,
            f: 100.0,
            cx: 50.0,
            cy: 60.0,
            k: [k1, 0.0, 0.0, 0.0, 0.0],
        };
        for (base, r) in [
            (KbBase::Equidistant, PI / 3.0),
            (KbBase::Equisolid, 1.0),
            (KbBase::Stereographic, 1.1547005383793),
            (KbBase::Orthographic, 0.8660254037844),
        ] {
            let at = kb(base, 0.0).forward(lens_dir(60.0, 0.0)).unwrap();
            assert!(close(at, [50.0 + 100.0 * r, 60.0]), "{base:?}: {at}");
        }
        // The polynomial before the base: with k1 = 0.1, θ_d = θ (1 + 0.1
        // θ²) = 1.0471976 · 1.1096623 = 1.1620356, and equidistant r = θ_d,
        // so u = 50 + 116.20356 = 166.2035613.
        let at = kb(KbBase::Equidistant, 0.1)
            .forward(lens_dir(60.0, 0.0))
            .unwrap();
        assert!(close(at, [166.2035612975, 60.0]), "{at}");
        // The orthographic base ends at θ_d = 90°.
        assert_eq!(
            kb(KbBase::Orthographic, 0.0).forward(lens_dir(100.0, 0.0)),
            None
        );
    }

    /// Pat's V6 lens 1's d0 to d12.
    const PATS_V6_D1: [f64; 13] = [
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
    ];

    /// Pat's V6 calibration (insta360.md, "Pat's X5 samples").
    fn pats_v6() -> CalibTruth {
        CalibTruth {
            blocks: [
                BlockTruth {
                    xi: 2.0,
                    fx: 4277.52,
                    fy: 4277.27,
                    cx: 2713.05,
                    cy: 2689.21,
                    ypr_deg: [-0.310, -0.271, 90.255],
                    t_m: [0.0; 3],
                },
                BlockTruth {
                    xi: 2.0,
                    fx: 4284.40,
                    fy: 4284.76,
                    cx: 8075.73,
                    cy: 2680.35,
                    ypr_deg: [0.120, -0.173, 89.112],
                    t_m: [0.000637, 0.000001, -0.031525],
                },
            ],
            dist: StoredDist::V6 {
                d: [PATS_V6_D0, PATS_V6_D1],
            },
            canvas: [10752, 5376],
            dst: 5312,
            lens_type: 113,
            body: 4,
        }
    }

    /// Pat's V6 calibration with both blocks' stored angles (0, 0, 90),
    /// so that under the placeholder each E_b is R_z(90° − 90°), the
    /// identity, and the frames can be worked by hand.
    fn square_v6() -> CalibTruth {
        let mut cal = pats_v6();
        for block in &mut cal.blocks {
            block.ypr_deg = [0.0, 0.0, 90.0];
        }
        cal
    }

    /// The conventions the compose tests start from: the binding direct,
    /// map C, `euler`, the turn about Y, no upright turn, tz as stored
    /// and V6 radial only.
    fn planted(euler: TruthEuler) -> Planted {
        Planted {
            binding: TruthBinding::Direct,
            canvas: TruthCanvas::C,
            euler,
            turn: TruthTurn::Y,
            upright: QuarterTurn::Zero,
            tz: TruthTz::AsStored,
            v6: TruthV6::RADIAL_ONLY,
        }
    }

    /// Whether `got` is `want` within 1e-12 on each axis.
    fn near(got: DVec3, want: [f64; 3]) -> bool {
        (got - DVec3::from(want)).abs().max_element() <= 1e-12
    }

    /// Whether `rotate` takes x, y and z to `cols`, within 1e-12.
    fn takes_axes(rotate: impl Fn(DVec3) -> DVec3, cols: [[f64; 3]; 3]) -> bool {
        [DVec3::X, DVec3::Y, DVec3::Z]
            .into_iter()
            .zip(cols)
            .all(|(axis, col)| near(rotate(axis), col))
    }

    /// F = diag(1, −1, −1), written out.
    fn diag_f(v: DVec3) -> DVec3 {
        DVec3::new(v.x, -v.y, -v.z)
    }

    /// compose's E_b for 8 `TruthEuler` values, the placeholder among
    /// them, and block 1's whole lens_from_rig under each turn axis,
    /// against values worked out by hand with the arithmetic in the
    /// comments (7.1).
    #[test]
    fn compose_turns_eight_euler_values_as_worked_by_hand() {
        use Angle::{Pitch, Roll, Yaw};
        use Axis::{X, Y, Z};
        use Sign::{Minus, Plus};
        let euler = |axes, order, signs, roll_offset| TruthEuler {
            axes,
            order,
            signs,
            roll_offset,
        };
        // With the binding direct, front track 0 and no upright turn, S and
        // T_0 are the identity, so lens_from_rig_0 = E_0 · F and E_0 v =
        // lens_from_rig_0 (F v), F being its own inverse. R_x(a) takes y
        // to (0, cos a, sin a) and z to (0, −sin a, cos a); R_y(a) takes z
        // to (sin a, 0, cos a) and x to (cos a, 0, −sin a); R_z(a) takes x
        // to (cos a, sin a, 0) and y to (−sin a, cos a, 0).
        let e0 = |e: TruthEuler, ypr: [f64; 3]| {
            let mut cal = pats_v6();
            cal.blocks[0].ypr_deg = ypr;
            let to_rig = compose(&cal, &planted(e), 0, [768; 2]).lenses[0].to_rig;
            move |v: DVec3| to_rig.inverse() * diag_f(v)
        };
        let (c30, s30) = (3f64.sqrt() / 2.0, 0.5);
        let cases = [
            // 1. The placeholder on (0, 0, 90): E = R_z(90 − 90) · R_x(0)
            //    · R_y(0), the identity.
            (
                TruthEuler::PLACEHOLDER,
                [0.0, 0.0, 90.0],
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            ),
            // 2. The placeholder on (90, 90, 90): E = R_z(0) · R_x(90) ·
            //    R_y(90). R_y(90) takes x to −z, and R_x(90) −z to y; y
            //    stays, then goes to z; z goes to x, which stays.
            (
                TruthEuler::PLACEHOLDER,
                [90.0, 90.0, 90.0],
                [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]],
            ),
            // 3. The placeholder on (0, 0, 120): E = R_z(120 − 90) =
            //    R_z(30): x to (cos 30, sin 30, 0), y to (−sin 30,
            //    cos 30, 0).
            (
                TruthEuler::PLACEHOLDER,
                [0.0, 0.0, 120.0],
                [[c30, s30, 0.0], [-s30, c30, 0.0], [0.0, 0.0, 1.0]],
            ),
            // 4. The pitch's sign minus, on (0, 30, 0): E = R_x(−30): y
            //    to (0, cos 30, −sin 30), z to (0, sin 30, cos 30).
            (
                euler(
                    [Y, X, Z],
                    [Yaw, Pitch, Roll],
                    [Plus, Minus, Plus],
                    QuarterTurn::Zero,
                ),
                [0.0, 30.0, 0.0],
                [[1.0, 0.0, 0.0], [0.0, c30, -s30], [0.0, s30, c30]],
            ),
            // 5. The roll's sign applies after its offset: on (0, 0,
            //    −60) with the roll's sign minus and +90°, the roll is
            //    −(−60 + 90) = −30, so E = R_z(−30): x to (cos 30,
            //    −sin 30, 0), y to (sin 30, cos 30, 0). The sign first
            //    would give +60 + 90 = 150.
            (
                euler(
                    [Y, X, Z],
                    [Yaw, Pitch, Roll],
                    [Plus, Plus, Minus],
                    QuarterTurn::Plus90,
                ),
                [0.0, 0.0, -60.0],
                [[c30, -s30, 0.0], [s30, c30, 0.0], [0.0, 0.0, 1.0]],
            ),
            // 6. Signs go with their angle, not their place: in the order
            //    roll, pitch, yaw, signs[0] is still the yaw's, so on (90,
            //    0, 0) E = R_y(−90): x to (0, 0, 1), z to (−1, 0, 0). By
            //    place the yaw, applied third, would take signs[2], plus.
            (
                euler(
                    [Y, X, Z],
                    [Roll, Pitch, Yaw],
                    [Minus, Plus, Plus],
                    QuarterTurn::Zero,
                ),
                [90.0, 0.0, 0.0],
                [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]],
            ),
            // 7. The yaw about z and the roll about x, with the half-turn
            //    offset, on (90, 0, 0): E = R_x(0 + 180) · R_y(0) ·
            //    R_z(90). R_z(90) takes x to y and y to −x, and R_x(180)
            //    flips y and z: x to −y, y to −x, z to −z.
            (
                euler([Z, Y, X], [Yaw, Pitch, Roll], [Plus; 3], QuarterTurn::Half),
                [90.0, 0.0, 0.0],
                [[0.0, -1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, -1.0]],
            ),
            // 8. The yaw about x, the pitch about z and the roll about y,
            //    in the order pitch, roll, yaw, the roll's sign minus, on
            //    (90, 30, 90): E = R_x(90) · R_y(−90) · R_z(30). R_y(−90)
            //    takes (a, b, c) to (−c, b, a) and R_x(90) takes it to
            //    (a, −c, b). x: R_z(30) gives (cos 30, sin 30, 0), then
            //    (0, sin 30, cos 30), then (0, −cos 30, sin 30). y:
            //    (−sin 30, cos 30, 0), then (0, cos 30, −sin 30), then
            //    (0, sin 30, cos 30). z: (0, 0, 1), then (−1, 0, 0), which
            //    R_x(90) keeps.
            (
                euler(
                    [X, Z, Y],
                    [Pitch, Roll, Yaw],
                    [Plus, Plus, Minus],
                    QuarterTurn::Zero,
                ),
                [90.0, 30.0, 90.0],
                [[0.0, -c30, s30], [0.0, s30, c30], [-1.0, 0.0, 0.0]],
            ),
        ];
        for (i, (e, ypr, cols)) in cases.into_iter().enumerate() {
            assert!(takes_axes(e0(e, ypr), cols), "case {}: {e:?}", i + 1);
        }
        // Controls: case 5 with the sign before the offset (R_z(150)), and
        // case 8 in the reverse order (R_z(30) · R_y(−90) · R_x(90)).
        let (c150, s150) = (-c30, 0.5);
        must_fail(
            "the roll's sign before its offset",
            takes_axes(
                e0(cases[4].0, cases[4].1),
                [[c150, s150, 0.0], [-s150, c150, 0.0], [0.0, 0.0, 1.0]],
            ),
        );
        let reversed = TruthEuler {
            order: [Yaw, Roll, Pitch],
            ..cases[7].0
        };
        must_fail(
            "case 8 in the reverse order",
            takes_axes(e0(reversed, cases[7].1), cases[7].2),
        );

        // Block 1, where E_1 and T_1 are both turns: with S the identity,
        // lens_from_rig_1 = E_1 · F · T_1, which takes rig X, Y and Z to
        // the columns below.
        //  9. The placeholder on (0, 30, 90) under turn Y: E_1 = R_z(90 −
        //     90) · R_x(30) · R_y(0) = R_x(30), and T_1 = R_y(180) =
        //     diag(−1, 1, −1), so F · T_1 = diag(−1, −1, 1), taking X to
        //     −x, Y to −y and Z to z. R_x(30) then keeps −x, takes −y to
        //     (0, −cos 30, −sin 30) and z to (0, −sin 30, cos 30).
        // 10. The placeholder on (30, 0, 90) under turn X: E_1 = R_y(30),
        //     and T_1 = R_x(180) = F, so F · T_1 is the identity and
        //     lens_from_rig_1 = R_y(30): X to (cos 30, 0, −sin 30), Y
        //     kept, Z to (sin 30, 0, cos 30).
        let lens_1 = |turn: TruthTurn, ypr: [f64; 3]| {
            let mut cal = pats_v6();
            cal.blocks[1].ypr_deg = ypr;
            let planted = Planted {
                turn,
                ..planted(TruthEuler::PLACEHOLDER)
            };
            let to_rig = compose(&cal, &planted, 0, [768; 2]).lenses[1].to_rig;
            move |v: DVec3| to_rig.inverse() * v
        };
        let under_y = || lens_1(TruthTurn::Y, [0.0, 30.0, 90.0]);
        let under_x = || lens_1(TruthTurn::X, [30.0, 0.0, 90.0]);
        assert!(
            takes_axes(
                under_y(),
                [[-1.0, 0.0, 0.0], [0.0, -c30, -s30], [0.0, -s30, c30]]
            ),
            "case 9"
        );
        assert!(
            takes_axes(
                under_x(),
                [[c30, 0.0, -s30], [0.0, 1.0, 0.0], [s30, 0.0, c30]]
            ),
            "case 10"
        );
        // Controls: T_1 on the left, T_1 · E_1 · F. In case 9, F takes Y to
        // −y, R_x(30) that to (0, −cos 30, −sin 30) and R_y(180) to (0,
        // −cos 30, sin 30); Z goes to −z, then (0, sin 30, −cos 30), then
        // (0, sin 30, cos 30). In case 10, R_x(180) · R_y(30) · R_x(180) =
        // R_y(−30): X to (cos 30, 0, sin 30), Z to (−sin 30, 0, cos 30). A
        // yaw under turn Y, or a pitch under X, turns about T_1's own axis
        // and commutes with it, so it could not tell the two apart.
        must_fail(
            "T_1 on the left, under turn Y",
            takes_axes(
                under_y(),
                [[-1.0, 0.0, 0.0], [0.0, -c30, s30], [0.0, s30, c30]],
            ),
        );
        must_fail(
            "T_1 on the left, under turn X",
            takes_axes(
                under_x(),
                [[c30, 0.0, s30], [0.0, 1.0, 0.0], [-s30, 0.0, c30]],
            ),
        );
    }

    /// Block 1's V6 centre and focal (cx 8075.73, so c_local 2699.73; fx
    /// 4284.40) under A, B and C: at 3840² cx 1928.38 under A and C and
    /// 1928.48 under B, f 3097.16 under A and B and 3060.29 under C; at
    /// 768² cx 385.68 under A and C and 385.70 under B (7.1).
    #[test]
    fn compose_scales_block_1_under_each_canvas_map() {
        // Block 1's square starts at src = 5376, so c_local = 8075.73 −
        // 5376 = 2699.73 and cy stays 2680.35; dst = 5312, and B's window
        // starts (5376 − 5312)/2 = 32 px into the square.
        //   A, C: cx = 2699.73 · 3840/5376 = 10366963.2/5376 = 1928.3785714
        //         cy = 2680.35 · 3840/5376 = 1914.5357143
        //   B:    cx = (2699.73 − 32) · 3840/5312 = 10244083.2/5312
        //            = 1928.4795181
        //         cy = (2680.35 − 32) · 3840/5312 = 1914.4698795
        //   A, B: fx = 4284.40 · 3840/5312 = 16452096/5312 = 3097.1566265
        //         fy = 4284.76 · 3840/5312 = 3097.4168675
        //   C:    fx = 16452096/5376 = 3060.2857143
        //         fy = 4284.76 · 3840/5376 = 3060.5428571
        //   C × 1.012 (Fitted): fx = 3060.2857143 · 1.012 = 3097.0091429
        //                       fy = 3060.5428571 · 1.012 = 3097.2693714
        // and at 768², A and C: cx = 2699.73/7 = 385.6757143; B: 2667.73 ·
        // 768/5312 = 385.6959036.
        let rows: [(u32, TruthCanvas, [f64; 4]); 7] = [
            (
                3840,
                TruthCanvas::A,
                [1928.3785714, 1914.5357143, 3097.1566265, 3097.4168675],
            ),
            (
                3840,
                TruthCanvas::B,
                [1928.4795181, 1914.4698795, 3097.1566265, 3097.4168675],
            ),
            (
                3840,
                TruthCanvas::C,
                [1928.3785714, 1914.5357143, 3060.2857143, 3060.5428571],
            ),
            (
                3840,
                TruthCanvas::Fitted { scale: 1.012 },
                [1928.3785714, 1914.5357143, 3097.0091429, 3097.2693714],
            ),
            (
                768,
                TruthCanvas::A,
                [385.6757143, f64::NAN, f64::NAN, f64::NAN],
            ),
            (
                768,
                TruthCanvas::B,
                [385.6959036, f64::NAN, f64::NAN, f64::NAN],
            ),
            (
                768,
                TruthCanvas::C,
                [385.6757143, f64::NAN, f64::NAN, f64::NAN],
            ),
        ];
        // Block 1's (cx, cy, fx, fy) at `side` under `canvas`.
        let block_1 = |side: u32, canvas: TruthCanvas| {
            let rig = compose(
                &pats_v6(),
                &Planted {
                    canvas,
                    ..planted(TruthEuler::PLACEHOLDER)
                },
                0,
                [side; 2],
            );
            match rig.lenses[1].model {
                TruthModel::MeiV6 {
                    xi,
                    fx,
                    fy,
                    cx,
                    cy,
                    d,
                    reading,
                } => {
                    assert_eq!((xi, d, reading), (2.0, PATS_V6_D1, TruthV6::RADIAL_ONLY));
                    [cx, cy, fx, fy]
                }
                other => panic!("block 1 is not V6: {other:?}"),
            }
        };
        for (side, canvas, want) in rows {
            let got = block_1(side, canvas);
            for (g, w) in got.into_iter().zip(want).filter(|(_, w)| !w.is_nan()) {
                assert!((g - w).abs() < 5e-8, "{side}² {canvas:?}: {got:?}");
            }
        }
        // C read as A is the focal 5376/5312 = 1.012 apart (Q4).
        let (a, c) = (block_1(3840, TruthCanvas::A), block_1(3840, TruthCanvas::C));
        assert!((a[2] / c[2] - 5376.0 / 5312.0).abs() < 1e-12);
        // Controls: B's centre is A's moved by 0.1 px at 3840², and C's
        // focal is not A's.
        let b = block_1(3840, TruthCanvas::B);
        let moved = must_fail("B's centre as A's", at_most((b[0] - a[0]).abs(), 0.05));
        assert!((moved - 0.1009467).abs() < 1e-6);
        must_fail("C's focal as A's", at_most((c[2] - a[2]).abs(), 0.05));
    }

    /// c_1 for Pat's V6 t_1 = (0.637, 0.001, −31.525) mm under both tz
    /// signs: `Negated` flips all three components.
    #[test]
    fn compose_places_lens_1_under_both_tz_signs() {
        // With block 0's angles (0, 0, 90) under the placeholder, E_0 is
        // the identity, so rig_from_lens_0 = (E_0 · F · T_0 · S)⁻¹ = F
        // with the binding direct, front track 0 and no upright turn, and
        //   c_1 = F t_1 = (0.637, −0.001, 31.525) mm,
        // lens 1 31.5 mm behind lens 0, which looks along −Z. The centres
        // are c_b − o with o = c_1/2: lens 0 at −c_1/2 = (−0.3185, 0.0005,
        // −15.7625) mm and lens 1 at c_1/2. Negated takes −t_1, so both
        // flip.
        let rig = |tz: TruthTz, upright: QuarterTurn| {
            compose(
                &square_v6(),
                &Planted {
                    tz,
                    upright,
                    ..planted(TruthEuler::PLACEHOLDER)
                },
                0,
                [768; 2],
            )
        };
        let mm = |v: DVec3| v * 1000.0;
        let close = |got: DVec3, want: [f64; 3]| (mm(got) - DVec3::from(want)).length() < 1e-9;
        let stored = rig(TruthTz::AsStored, QuarterTurn::Zero);
        assert!(near(stored.lenses[0].to_rig * DVec3::Z, [0.0, 0.0, -1.0]));
        assert!(close(
            stored.lenses[0].centre_m,
            [-0.3185, 0.0005, -15.7625]
        ));
        assert!(close(stored.lenses[1].centre_m, [0.3185, -0.0005, 15.7625]));
        let negated = rig(TruthTz::Negated, QuarterTurn::Zero);
        assert!(close(
            negated.lenses[0].centre_m,
            [0.3185, -0.0005, 15.7625]
        ));
        assert!(close(
            negated.lenses[1].centre_m,
            [-0.3185, 0.0005, -15.7625]
        ));
        // Exactly: Negated flips both lenses' c_b − o, so at q it is
        // AsStored at −q (8.6's tz rule).
        for t in 0..2 {
            assert_eq!(negated.lenses[t].centre_m, -stored.lenses[t].centre_m);
        }
        // With the upright turn +90°, rig_from_lens_0 = U⁻¹ · F, and U⁻¹
        // is +90° about +Z, taking (a, b, c) to (−b, a, c): c_1 = (0.001,
        // 0.637, 31.525) mm, and lens 1's centre half of it.
        let upright = rig(TruthTz::AsStored, QuarterTurn::Plus90);
        assert!(close(upright.lenses[1].centre_m, [0.0005, 0.3185, 15.7625]));
        let upright = rig(TruthTz::Negated, QuarterTurn::Plus90);
        assert!(close(
            upright.lenses[1].centre_m,
            [-0.0005, -0.3185, -15.7625]
        ));
        // On Pat's own angles c_1 is turned by E_0, under 0.5°, and keeps
        // t_1's length, √(0.637² + 0.001² + 31.525²) = 31.5314350 mm.
        let pats = compose(&pats_v6(), &planted(TruthEuler::PLACEHOLDER), 0, [768; 2]);
        let c1 = pats.lenses[1].centre_m - pats.lenses[0].centre_m;
        assert!((mm(c1).length() - 31.5314350).abs() < 1e-6);
        assert!(angle(c1, DVec3::new(0.637, -0.001, 31.525)) < 0.5f64.to_radians());
        // Control: flipping tz alone, a reflection, puts lens 1 elsewhere.
        let mut z_only = square_v6();
        z_only.blocks[1].t_m[2] = -z_only.blocks[1].t_m[2];
        let z_only = compose(&z_only, &planted(TruthEuler::PLACEHOLDER), 0, [768; 2]);
        must_fail(
            "tz flipped alone as Negated",
            near(
                z_only.lenses[1].centre_m,
                negated.lenses[1].centre_m.to_array(),
            ),
        );
    }

    /// The binding puts each block on its track, a front track that is
    /// block 1's turns both lenses 180° about +Y, and `upright` turns both
    /// a quarter turn about −Z.
    #[test]
    fn compose_follows_the_binding_the_front_track_and_upright() {
        let cal = square_v6();
        let base = planted(TruthEuler::PLACEHOLDER);
        let reversed = Planted {
            binding: TruthBinding::Reversed,
            ..base
        };
        // Reversed puts block 0 on track 1 and block 1 on track 0, each
        // with its track's side; neither run turns for the front track,
        // since in both it is block 0's.
        let direct = compose(&cal, &base, 0, [832, 768]);
        let swapped = compose(&cal, &reversed, 1, [768, 832]);
        assert_eq!(swapped.lenses[1], direct.lenses[0]);
        assert_eq!(swapped.lenses[0], direct.lenses[1]);
        assert_eq!(swapped.lenses[1].side, 832);
        assert_eq!(
            RigTruth {
                lenses: direct.lenses,
                ..swapped.clone()
            },
            direct
        );
        // Block 0's model, by its focal: 4277.52 · 832/5376 = 661.98.
        let TruthModel::MeiV6 { fx, .. } = swapped.lenses[1].model else {
            panic!("not V6");
        };
        assert!((fx - 4277.52 * 832.0 / 5376.0).abs() < 1e-9);
        // Control: the tracks left as the blocks are.
        must_fail(
            "Reversed with the tracks unswapped",
            swapped.lenses[0] == direct.lenses[0],
        );

        // Block 1 is turned 180° about the planted axis. With E_1 the
        // identity, rig_from_lens_1 = T_1⁻¹ · F: about Y its image right x
        // goes to F x = x, then to −X; its down y to −y, then stays −Y,
        // down; its axis z to −z, then +Z, behind. About X: right stays
        // +X and down goes to +Y, so the picture is upside down.
        let lens_1 = |turn: TruthTurn| {
            let to_rig = compose(&cal, &Planted { turn, ..base }, 0, [768; 2]).lenses[1].to_rig;
            move |v: DVec3| to_rig * v
        };
        assert!(takes_axes(
            lens_1(TruthTurn::Y),
            [[-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]
        ));
        assert!(takes_axes(
            lens_1(TruthTurn::X),
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        ));

        // A front track that is block 1's adds Y, 180° about +Y, to S, so
        // rig_from_lens gains Y⁻¹ on the left: every lens direction and
        // centre (a, b, c) becomes (−a, b, −c), and block 1 looks along
        // −Z.
        let front_0 = compose(&cal, &base, 0, [768; 2]);
        let front_1 = compose(&cal, &base, 1, [768; 2]);
        let half_y = |v: DVec3| DVec3::new(-v.x, v.y, -v.z);
        // Each lens of `turned` is `plain`'s with `by` after it.
        let turned_by = |turned: &RigTruth, plain: &RigTruth, by: &dyn Fn(DVec3) -> DVec3| {
            (0..2).all(|t| {
                let (a, b) = (&turned.lenses[t], &plain.lenses[t]);
                takes_axes(
                    |v| a.to_rig * v,
                    [DVec3::X, DVec3::Y, DVec3::Z].map(|v| by(b.to_rig * v).to_array()),
                ) && near(a.centre_m, by(b.centre_m).to_array())
            })
        };
        assert!(turned_by(&front_1, &front_0, &half_y));
        assert!(near(front_1.lenses[1].to_rig * DVec3::Z, [0.0, 0.0, -1.0]));
        // Upright +90° is U = 90° about −Z, so rig_from_lens gains U⁻¹,
        // 90° about +Z: (a, b, c) becomes (−b, a, c), and lens 0's image
        // right, +X upright, points up.
        let quarter = compose(
            &cal,
            &Planted {
                upright: QuarterTurn::Plus90,
                ..base
            },
            0,
            [768; 2],
        );
        let quarter_z = |v: DVec3| DVec3::new(-v.y, v.x, v.z);
        assert!(turned_by(&quarter, &front_0, &quarter_z));
        assert!(near(quarter.lenses[0].to_rig * DVec3::X, [0.0, 1.0, 0.0]));
        // Controls: the front track's turn is not about +X, and the
        // upright turn not about +Z the other way.
        let half_x = |v: DVec3| DVec3::new(v.x, -v.y, -v.z);
        must_fail(
            "the front track's turn about +X",
            turned_by(&front_1, &front_0, &half_x),
        );
        let quarter_back = |v: DVec3| DVec3::new(v.y, -v.x, v.z);
        must_fail(
            "upright about +Z",
            turned_by(&quarter, &front_0, &quarter_back),
        );
    }

    /// Turns about +Y from 0.5 s to 1.5 s at 10°/s, then about +X from
    /// 2 s to 2.5 s at −20°/s.
    fn two_turns() -> Motion {
        Motion {
            fps: 30.0,
            turns: vec![
                Turn {
                    start_s: 0.5,
                    end_s: 1.5,
                    axis: DVec3::Y,
                    deg_per_s: 10.0,
                },
                Turn {
                    start_s: 2.0,
                    end_s: 2.5,
                    axis: DVec3::X,
                    deg_per_s: -20.0,
                },
            ],
        }
    }

    /// `Motion::at` turns at each turn's rate and holds still between
    /// turns.
    #[test]
    fn motion_turns_at_its_rate_and_rests_between() {
        // cos 5° = 0.9961946980917455, sin 5° = 0.08715574274765817,
        // cos 10° = 0.984807753012208, sin 10° = 0.17364817766693033.
        // At 1 s the first turn is half done: R_y(5°). From 1.5 s to 2 s
        // the rig rests at R_y(10°). At 2.25 s the second turn is half
        // done, R_x(−5°) · R_y(10°), whose columns are
        //   x: (cos 10°, −sin 10° sin 5°, −sin 10° cos 5°)
        //   y: (0, cos 5°, −sin 5°)
        //   z: (sin 10°, cos 10° sin 5°, cos 10° cos 5°).
        let (c5, s5) = (0.9961946980917455, 0.08715574274765817);
        let (c10, s10) = (0.984807753012208, 0.17364817766693033);
        let half_y = [[c5, 0.0, -s5], [0.0, 1.0, 0.0], [s5, 0.0, c5]];
        let rest_1 = [[c10, 0.0, -s10], [0.0, 1.0, 0.0], [s10, 0.0, c10]];
        let half_x = [
            [c10, -s10 * s5, -s10 * c5],
            [0.0, c5, -s5],
            [s10, c10 * s5, c10 * c5],
        ];
        let motion = two_turns();
        assert_eq!(motion.at(0.2), DQuat::IDENTITY);
        assert_eq!(motion.at(0.5), DQuat::IDENTITY);
        assert!(takes_axes(|v| motion.at(1.0) * v, half_y));
        assert!(takes_axes(|v| motion.at(1.5) * v, rest_1));
        assert!(takes_axes(|v| motion.at(1.8) * v, rest_1));
        assert!(takes_axes(|v| motion.at(2.25) * v, half_x));
        // Anywhere in one rest, the same orientation to the bit.
        assert_eq!(motion.at(1.5), motion.at(1.99));
        assert_eq!(motion.at(2.5), motion.at(60.0));
        assert_eq!(Motion::still().at(7.0), DQuat::IDENTITY);

        // The rest an interval falls in, its ends included.
        assert_eq!(motion.rest(-1.0, 0.5), Some(0));
        assert_eq!(motion.rest(0.4, 0.6), None);
        assert_eq!(motion.rest(1.0, 1.0), None);
        assert_eq!(motion.rest(1.5, 2.0), Some(1));
        assert_eq!(motion.rest(1.6, 1.9), Some(1));
        assert_eq!(motion.rest(1.9, 2.1), None);
        assert_eq!(motion.rest(2.5, 9.0), Some(2));
        assert_eq!(motion.rest(0.2, 2.7), None);
        assert_eq!(Motion::still().rest(0.0, 100.0), Some(0));

        // Controls: the turns composed the other way round, and the first
        // turn at 9°/s.
        let ry10 = DQuat::from_axis_angle(DVec3::Y, 10f64.to_radians());
        let rx5 = DQuat::from_axis_angle(DVec3::X, -5f64.to_radians());
        must_fail(
            "the turns composed in reverse",
            takes_axes(|v| ry10 * rx5 * v, half_x),
        );
        let mut slow = two_turns();
        slow.turns[0].deg_per_s = 9.0;
        must_fail(
            "the first turn at 9°/s",
            takes_axes(|v| slow.at(1.0) * v, half_y),
        );
    }

    /// The rig the render tests start from: Pat's V6 calibration with
    /// square angles under the placeholder ([`square_v6`]), both tracks
    /// `side` square. By 6.2's frames, lens 0 looks along −Z with its
    /// image's +x along +X and +y along −Y, and lens 1 along +Z with +x
    /// along −X and +y along −Y; each centre is 15.76 mm out along its
    /// own axis.
    fn square_rig(side: u32) -> RigTruth {
        compose(
            &square_v6(),
            &planted(TruthEuler::PLACEHOLDER),
            0,
            [side; 2],
        )
    }

    /// The pixel whose square holds `at`.
    fn pixel_at(at: DVec2) -> (u32, u32) {
        (at.x.floor() as u32, at.y.floor() as u32)
    }

    /// The centre of pixel (i, j).
    fn pixel_centre(i: u32, j: u32) -> DVec2 {
        DVec2::new(f64::from(i) + 0.5, f64::from(j) + 0.5)
    }

    /// How many pixels of `picture`, track `lens`'s at frame 0 and ss = 1
    /// with the scene infinitely far and the rig still, are not the
    /// pattern in the direction `to_rig` gives synth's inverse at the
    /// pixel's centre moved by `shift`, or black where it finds none.
    fn miswired(picture: &RgbImage, pattern: Pattern, lens: &LensTruth, shift: DVec2) -> usize {
        let mut wrong = 0;
        for j in 0..picture.height {
            for i in 0..picture.width {
                let want = lens
                    .model
                    .invert(pixel_centre(i, j) + shift, lens.theta_max)
                    .map_or([0.0; 3], |d| {
                        crate::color(pattern, (lens.to_rig * d).normalize(), 0)
                    });
                if picture.get(i, j) != want {
                    wrong += 1;
                }
            }
        }
        wrong
    }

    /// Each pixel of `render_lens` at ss = 1 is the pattern in the
    /// direction synth's inverse and `to_rig` give its centre.
    #[test]
    fn render_lens_shows_the_scene_through_the_truth() {
        // Each axis disc's centre, as a direction in each lens's frame,
        // where `square_rig`'s frames put it. A glyph's centre cell is
        // never inked, and it spans more than a pixel here.
        let [red, green, blue, yellow, white, black] = AXIS_MARKERS.map(|(_, c)| c);
        let discs: [[(DVec3, Rgb); 5]; 2] = [
            [
                (DVec3::Z, red),
                (DVec3::X, green),
                (DVec3::NEG_X, yellow),
                (DVec3::NEG_Y, white),
                (DVec3::Y, black),
            ],
            [
                (DVec3::Z, blue),
                (DVec3::X, yellow),
                (DVec3::NEG_X, green),
                (DVec3::NEG_Y, white),
                (DVec3::Y, black),
            ],
        ];
        let rig = square_rig(384);
        for (lens, own) in discs.iter().enumerate() {
            let picture = render_lens(Pattern::Grid, &rig, lens, 0, 1);
            let model = rig.lenses[lens].model;
            let shows = |list: &[(DVec3, Rgb)]| {
                list.iter().all(|&(d, want)| {
                    let (i, j) = pixel_at(model.forward(d).unwrap());
                    assert!(i < picture.width && j < picture.height, "({i}, {j})");
                    picture.get(i, j) == want
                })
            };
            assert!(shows(own), "lens {lens}");
            // Control: the other lens's discs.
            must_fail("the other lens's discs", shows(&discs[1 - lens]));
        }

        // Every pixel, at a size the check can afford.
        let small = square_rig(96);
        for lens in 0..2 {
            let picture = render_lens(Pattern::Grid, &small, lens, 0, 1);
            let truth = &small.lenses[lens];
            assert_eq!(
                miswired(&picture, Pattern::Grid, truth, DVec2::ZERO),
                0,
                "lens {lens}"
            );
            // The picture doesn't depend on how many threads render it.
            for threads in [1, 3] {
                let other = lens_on_threads(Pattern::Grid, &small, lens, 0, 1, threads);
                assert_eq!(other.pixels, picture.pixels, "{threads} threads");
            }
            // Controls: the other lens's truth, and each sample half a
            // pixel right.
            let swapped = must_fail(
                "the other lens's truth",
                at_most(
                    miswired(
                        &picture,
                        Pattern::Grid,
                        &small.lenses[1 - lens],
                        DVec2::ZERO,
                    ),
                    0,
                ),
            );
            let shifted = must_fail(
                "half a pixel right",
                at_most(
                    miswired(&picture, Pattern::Grid, truth, DVec2::new(0.5, 0.0)),
                    0,
                ),
            );
            assert!(
                swapped > 96 * 96 / 4 && shifted > 96 * 96 / 4,
                "{swapped}, {shifted}"
            );
        }
    }

    /// Past `theta_max`, and where the inverse has no direction, the
    /// picture is black.
    #[test]
    fn render_lens_is_black_past_theta_max() {
        // HighFrequency is never black, so a black pixel is one the
        // render found no direction for within `theta_max`.
        let mut rig = square_rig(96);
        let model = rig.lenses[0].model;
        let blacks_where = |picture: &RgbImage, cap: f64| -> Result<(), (u32, u32)> {
            for j in 0..picture.height {
                for i in 0..picture.width {
                    let black = picture.get(i, j) == [0.0; 3];
                    if black != model.invert(pixel_centre(i, j), cap).is_none() {
                        return Err((i, j));
                    }
                }
            }
            Ok(())
        };
        let picture = render_lens(Pattern::HighFrequency, &rig, 0, 0, 1);
        assert_eq!(blacks_where(&picture, rig.lenses[0].theta_max), Ok(()));
        // Both kinds of black are there: past 106° with a direction, and
        // past the model's fold at 120° with none.
        let (mut no_direction, mut past_max) = (0, 0);
        for j in 0..96 {
            for i in 0..96 {
                match model.inverse(pixel_centre(i, j)) {
                    None => no_direction += 1,
                    Some(d) if off_axis(d) > rig.lenses[0].theta_max => past_max += 1,
                    Some(_) => {}
                }
            }
        }
        assert!(
            no_direction > 0 && past_max > 0,
            "{no_direction}, {past_max}"
        );
        // With `theta_max` at 90°, the black comes in to 90°.
        rig.lenses[0].theta_max = FRAC_PI_2;
        let narrower = render_lens(Pattern::HighFrequency, &rig, 0, 0, 1);
        assert_eq!(blacks_where(&narrower, FRAC_PI_2), Ok(()));
        // Control: the 106° picture held to 90°.
        must_fail(
            "the 106° picture held to 90°",
            blacks_where(&picture, FRAC_PI_2),
        );
    }

    /// A finite scene and the ground plane are seen from each lens's own
    /// centre, so the two lenses' pictures of one point differ by the
    /// parallax 6.2 predicts.
    #[test]
    fn render_lens_sees_the_scene_and_the_ground_from_its_centre() {
        // By hand. From (0, 0, 1) along +X, a sphere of radius 2 is met
        // where 1 + s² = 4, at (√3, 0, 1), in the direction (√3/2, 0, ½).
        // From the origin along (1, −1, 0)/√2, a ground 1 m down is met
        // at s = √2, at (1, −1, 0); a sphere of radius 1.2 comes before
        // it, at 1.2 along the ray, and one of radius 2 after.
        let mut rig = square_rig(64);
        rig.scene_radius_m = Some(2.0);
        let s3 = 3f64.sqrt();
        assert!(near(
            sees(&rig, DVec3::Z, DVec3::X).normalize(),
            [s3 / 2.0, 0.0, 0.5]
        ));
        let down = DVec3::new(1.0, -1.0, 0.0).normalize();
        rig.scene_radius_m = None;
        rig.ground_m = Some(1.0);
        assert!(near(sees(&rig, DVec3::ZERO, down), [1.0, -1.0, 0.0]));
        // A ray that misses the ground sees the scene, here infinitely
        // far.
        assert_eq!(sees(&rig, DVec3::ZERO, DVec3::Y), DVec3::Y);
        rig.scene_radius_m = Some(1.2);
        assert!(near(sees(&rig, DVec3::ZERO, down), (down * 1.2).to_array()));
        rig.scene_radius_m = Some(2.0);
        assert!(near(sees(&rig, DVec3::ZERO, down), [1.0, -1.0, 0.0]));

        // 6.2's parallax: lens b sees the scene's point e along
        // lens_from_rig_b · (e − c_b)/|e − c_b|, c_b its centre. From the
        // pixel there, the render's own path (synth's inverse, `to_rig`,
        // the ray from the centre) comes back to e: on the sphere, and on
        // the ground for the last two.
        let mut rig = square_rig(384);
        rig.scene_radius_m = Some(2.0);
        rig.ground_m = Some(1.5);
        let points = [
            DVec3::new(2.0, 0.0, 0.0),
            DVec3::new(1.0, 0.3, 0.1).normalize() * 2.0,
            DVec3::new(-1.0, 0.2, -0.05).normalize() * 2.0,
            DVec3::new(0.0, 0.6, 1.0).normalize() * 2.0,
            DVec3::new(0.8, -1.5, 0.3),
            DVec3::new(-0.4, -1.5, -1.1),
        ];
        let back = |lens: &LensTruth, centre: DVec3, at: DVec2| {
            let d = lens.model.invert(at, lens.theta_max).unwrap();
            sees(&rig, centre, lens.to_rig * d)
        };
        let mut seen_by = [0; 2];
        for e in points {
            for (b, lens) in rig.lenses.iter().enumerate() {
                let d = lens.to_rig.inverse() * (e - lens.centre_m).normalize();
                if off_axis(d) > lens.theta_max {
                    continue;
                }
                seen_by[b] += 1;
                let at = lens.model.forward(d).unwrap();
                let miss = (back(lens, lens.centre_m, at) - e).length();
                assert!(miss <= 1e-9, "lens {b}, {e}: {miss} m");
                // Infinitely far, the point would be elsewhere in the
                // picture.
                let far = lens.model.forward(lens.to_rig.inverse() * e).unwrap();
                assert!((far - at).length() > 0.1, "lens {b}, {e}");
                // Control: the same pixel seen from the rig's origin.
                let off = must_fail(
                    "seen from the rig's origin",
                    at_most((back(lens, DVec3::ZERO, at) - e).length(), 1e-9),
                );
                assert!(off > 1e-3, "lens {b}, {e}: {off} m");
            }
        }
        assert_eq!(seen_by, [5, 5]);

        // In the picture. On a sphere 7 cm out, the Grid's right disc
        // sits 102.6° off lens 0's axis and 102.7° off lens 1's, 12.6°
        // and 12.7° past where an infinitely far one would, which is
        // more than the disc's 8°. The frame, which reaches about 100°
        // along the image's axes at 384², gets 64 px more on each side.
        let mut close = square_rig(384);
        close.scene_radius_m = Some(0.07);
        for lens in &mut close.lenses {
            lens.side = 512;
            if let TruthModel::MeiV6 { cx, cy, .. } = &mut lens.model {
                *cx += 64.0;
                *cy += 64.0;
            }
        }
        let green = AXIS_MARKERS[1].1;
        for (b, lens) in close.lenses.iter().enumerate() {
            let picture = render_lens(Pattern::Grid, &close, b, 0, 1);
            let shows_disc = |d: DVec3| {
                let (i, j) = pixel_at(lens.model.forward(d).unwrap());
                assert!(i < picture.width && j < picture.height, "({i}, {j})");
                picture.get(i, j) == green
            };
            let e = DVec3::X * 0.07;
            assert!(
                shows_disc(lens.to_rig.inverse() * (e - lens.centre_m).normalize()),
                "lens {b}"
            );
            // Control: the disc where an infinitely far scene puts it.
            must_fail(
                "the disc where an infinitely far scene puts it",
                shows_disc(lens.to_rig.inverse() * DVec3::X),
            );
        }

        // A ground 7 cm down changes the rows that look down, seen from
        // the lens's centre, and leaves those that look up. Image +y is
        // −Y in both lenses, and a radial-only distortion keeps each
        // pixel on its side of the centre's row.
        let open = square_rig(192);
        let mut grounded = open.clone();
        grounded.ground_m = Some(0.07);
        for (b, lens) in open.lenses.iter().enumerate() {
            let sky = render_lens(Pattern::Grid, &open, b, 0, 1);
            let ground = render_lens(Pattern::Grid, &grounded, b, 0, 1);
            let cy = lens.model.intrinsics().1.y;
            let (mut up_changed, mut down_changed, mut down_seen) = (0, 0, 0);
            for j in 0..192 {
                for i in 0..192 {
                    let changed = usize::from(sky.get(i, j) != ground.get(i, j));
                    if f64::from(j) + 0.5 < cy {
                        up_changed += changed;
                    } else if lens
                        .model
                        .invert(pixel_centre(i, j), lens.theta_max)
                        .is_some()
                    {
                        down_seen += 1;
                        down_changed += changed;
                    }
                }
            }
            assert_eq!(up_changed, 0, "lens {b}");
            assert!(
                down_changed * 4 >= down_seen * 3,
                "lens {b}: {down_changed} of {down_seen}"
            );
        }
    }

    /// A late lens and a rolling shutter render each row at its own time
    /// under the rig's motion.
    #[test]
    fn render_lens_renders_late_lenses_and_rows_at_their_times() {
        // At 32 frames a second, with a readout of 1/32 s over 64 rows,
        // every time here is a short binary fraction, so the sums are
        // exact and the pictures compare to the bit. The rig turns about
        // +Y at 30°/s throughout, about a degree a frame, which moves
        // HighFrequency's 1° cells.
        let mut rig = square_rig(64);
        rig.motion = Motion {
            fps: 32.0,
            turns: vec![Turn {
                start_s: 0.0,
                end_s: 2.0,
                axis: DVec3::Y,
                deg_per_s: 30.0,
            }],
        };
        let shot = |rig: &RigTruth, lens: usize, frame: u32| {
            render_lens(Pattern::HighFrequency, rig, lens, frame, 1).pixels
        };
        // A lens a frame late shows frame 10 as it shows frame 11 on time,
        // and the other lens is as it was.
        let mut late = rig.clone();
        late.late_frames = [0.0, 1.0];
        assert_eq!(shot(&late, 1, 10), shot(&rig, 1, 11));
        assert_eq!(shot(&late, 0, 10), shot(&rig, 0, 10));
        // Control: the late lens's frame 10 taken for its frame 10 on time.
        must_fail(
            "lens 1 a frame late taken for on time",
            shot(&late, 1, 10) == shot(&rig, 1, 10),
        );

        // A rolling shutter exposes row y (y + ½)/64 of its readout after
        // the frame's time, as a global shutter late by (y + ½)/64 of a
        // frame does.
        let mut rolling = rig.clone();
        rolling.readout_s = 1.0 / 32.0;
        let rolled = shot(&rolling, 0, 10);
        let row = |pixels: &[Rgb], y: u32| pixels[64 * y as usize..64 * (y as usize + 1)].to_vec();
        let late_by = |frames: f64| {
            let mut global = rig.clone();
            global.late_frames = [frames, 0.0];
            global
        };
        for y in [0, 21, 40, 63] {
            let global = late_by((f64::from(y) + 0.5) / 64.0);
            assert_eq!(
                exposed_at(&rolling, 0, 10, y),
                exposed_at(&global, 0, 10, y)
            );
            assert_eq!(row(&rolled, y), row(&shot(&global, 0, 10), y), "row {y}");
        }
        // Controls: row 40 at the frame's own time, and at row 0's.
        must_fail(
            "row 40 at the frame's time",
            row(&rolled, 40) == row(&shot(&rig, 0, 10), 40),
        );
        must_fail(
            "row 40 at row 0's time",
            row(&rolled, 40) == row(&shot(&late_by(0.5 / 64.0), 0, 10), 40),
        );
    }

    /// The gain and the vignette apply in linear light.
    #[test]
    fn render_lens_applies_gain_and_vignette_in_linear_light() {
        // HighFrequency is 0.9 or 0.1 on every channel, 0.7874122893956174
        // and 0.010022825574869039 in linear light. A gain of 2 clips the
        // first at white and takes the second to 0.020045651149738078,
        // shown as 0.15190017739635536; a gain of ½ takes them to
        // 0.3937061446978087 and 0.005011412787434520, shown as
        // 0.6604416292086154 and 0.06111879845794282. A gain of 1 leaves
        // them, and black stays black.
        let worked = |k: usize, v: f64| match k {
            0 if v == 0.9 => 1.0,
            0 if v == 0.1 => 0.15190017739635536,
            2 if v == 0.9 => 0.6604416292086154,
            2 if v == 0.1 => 0.06111879845794282,
            _ => v,
        };
        // The worst channel of `picture` against the worked value of
        // `plain`'s.
        let worst = |picture: &[Rgb], plain: &[Rgb]| {
            picture
                .iter()
                .zip(plain)
                .flat_map(|(got, was)| (0..3).map(move |k| (got[k] - worked(k, was[k])).abs()))
                .fold(0.0, f64::max)
        };
        let mut rig = square_rig(96);
        let plain = render_lens(Pattern::HighFrequency, &rig, 0, 0, 1).pixels;
        rig.lenses[0].gain = [2.0, 1.0, 0.5];
        let gained = render_lens(Pattern::HighFrequency, &rig, 0, 0, 1).pixels;
        let err = worst(&gained, &plain);
        assert!(err <= 1e-12, "{err}");
        // Control: the gain on display values.
        let display: Vec<Rgb> = plain
            .iter()
            .map(|c| std::array::from_fn(|k| (c[k] * rig.lenses[0].gain[k]).min(1.0)))
            .collect();
        must_fail(
            "the gain on display values",
            at_most(worst(&display, &plain), 1e-12),
        );

        // A vignette of v1 = −0.1 falls off to 1 − 0.1 (π/2)² =
        // 0.753259889972766 at 90°, which takes 0.9 to 0.793653466025421
        // and 0.1 to 0.08273956778423064.
        let mut lens = rig.lenses[0];
        lens.gain = [1.0; 3];
        lens.vignette = [-0.1, 0.0, 0.0];
        let want = [0.793653466025421, 0.08273956778423064, 0.0];
        let off = |got: Rgb| {
            got.iter()
                .zip(want)
                .map(|(g, w)| (g - w).abs())
                .fold(0.0, f64::max)
        };
        let at_90 = off(expose([0.9, 0.1, 0.0], &lens, FRAC_PI_2));
        assert!(at_90 <= 1e-12, "{at_90}");
        // Control: the falloff on display values.
        let fall = 0.753259889972766;
        must_fail(
            "the falloff on display values",
            at_most(off([0.9 * fall, 0.1 * fall, 0.0]), 1e-12),
        );
        // The render takes each pixel's own θ.
        rig.lenses[0] = lens;
        let vignetted = render_lens(Pattern::HighFrequency, &rig, 0, 0, 1);
        for j in 0..96 {
            for i in 0..96 {
                let want = match lens.model.invert(pixel_centre(i, j), lens.theta_max) {
                    Some(d) => expose(plain[(j * 96 + i) as usize], &lens, off_axis(d)),
                    None => [0.0; 3],
                };
                assert_eq!(vignetted.get(i, j), want, "({i}, {j})");
            }
        }
    }

    /// `pattern`'s colors on a grid of directions about the seam z = 0:
    /// every 0.1° of azimuth, every 0.5° of elevation within 30° of the
    /// seam, and a few rows beyond.
    fn about_the_seam(pattern: Pattern) -> Vec<Rgb> {
        let offs = (-60..=60)
            .map(|k| f64::from(k) * 0.5)
            .chain([-89.5, -60.0, -40.0, 40.0, 60.0, 89.5]);
        offs.flat_map(|off: f64| {
            (-1800..=1800).map(move |k| {
                let (sa, ca) = (f64::from(k) * 0.1).to_radians().sin_cos();
                let (so, co) = off.to_radians().sin_cos();
                crate::color(pattern, DVec3::new(co * ca, co * sa, so), 0)
            })
        })
        .collect()
    }

    /// `SeamLines` stays below white under 4.2's exposure control, lens 1
    /// at +0.5 EV and +5 % red, so lens 1's picture matched back by its
    /// gain shows the scene's own values everywhere, and no step is left
    /// where it meets lens 0's at the seam.
    #[test]
    fn seam_lines_stays_below_white_under_the_exposure_control() {
        let scene = about_the_seam(Pattern::SeamLines);
        let channels = || scene.iter().flatten().copied();
        assert_eq!(channels().fold(0.0, f64::max), crate::SEAM_LINES_PEAK);
        let darkest = channels().fold(1.0, f64::min);
        assert!((darkest - 0.05).abs() < 1e-12, "{darkest}");
        // The worst channel of `colors` seen through `lens` and matched
        // back by its gain in linear light, against the scene's own.
        let worst = |colors: &[Rgb], lens: &LensTruth| {
            colors
                .iter()
                .flat_map(|c| {
                    let seen = expose(*c, lens, 0.0);
                    (0..3).map(move |k| (srgb_oetf(srgb_eotf(seen[k]) / lens.gain[k]) - c[k]).abs())
                })
                .fold(0.0, f64::max)
        };
        // At +0.5 EV and +5 % red, red clips past OETF(1/(√2 · 1.05)) =
        // 0.840 and green and blue past OETF(1/√2) = 0.858.
        let ev = 2f64.sqrt();
        let mut lens = square_rig(16).lenses[1];
        lens.gain = [1.05 * ev, ev, ev];
        let err = worst(&scene, &lens);
        assert!(err <= 1e-12, "{err}");
        // Control: Grid, whose equator and axis discs are white.
        must_fail(
            "Grid's white",
            at_most(worst(&about_the_seam(Pattern::Grid), &lens), 1e-12),
        );
        // Control: a gain that takes the peak just past white, which the
        // check sees only because the samples reach the peak.
        lens.gain = [1.001 / srgb_eotf(crate::SEAM_LINES_PEAK); 3];
        must_fail(
            "the peak just past white",
            at_most(worst(&scene, &lens), 1e-12),
        );
    }

    /// The Y, Cb and Cr planes of 8-bit `planes`.
    fn planes_8(planes: YuvPlanes) -> [Vec<u8>; 3] {
        match planes {
            YuvPlanes::U8(p) => p,
            YuvPlanes::U16(_) => panic!("16-bit planes where 8-bit ones were asked for"),
        }
    }

    /// `lens_planes` codes as the X5 does: full range, BT.709, chroma
    /// sited left.
    #[test]
    fn lens_planes_codes_as_the_x5_does() {
        // Full range BT.709 at 8 bits: Y = 255 (0.2126 R + 0.7152 G +
        // 0.0722 B), Cb = 128 + 255 (B − Y)/1.8556 and Cr = 128 +
        // 255 (R − Y)/1.5748 with Y from 0 to 1, each rounded and held to
        // 0 to 255.
        //   white: Y 255, Cb 128, Cr 128; black: 0, 128, 128
        //   red: Y 54.21 → 54, Cb 128 − 29.22 = 98.78 → 99,
        //        Cr 128 + 127.5 = 255.5 → 255
        //   blue: Y 18.41 → 18, Cb 255.5 → 255, Cr 128 − 11.69 → 116
        let flat = |c: Rgb| RgbImage {
            width: 8,
            height: 8,
            pixels: vec![c; 64],
        };
        let codes = |c: Rgb, coding: Coding, want: [u8; 3]| -> Result<(), [Vec<u8>; 3]> {
            let planes = planes_8(lens_planes(&flat(c), coding));
            let sizes = planes.iter().map(Vec::len).eq([64, 16, 16]);
            let held = planes
                .iter()
                .zip(want)
                .all(|(p, w)| p.iter().all(|&v| v == w));
            if sizes && held { Ok(()) } else { Err(planes) }
        };
        let flats: [(Rgb, [u8; 3]); 4] = [
            ([1.0; 3], [255, 128, 128]),
            ([0.0; 3], [0, 128, 128]),
            ([1.0, 0.0, 0.0], [54, 99, 255]),
            ([0.0, 0.0, 1.0], [18, 255, 116]),
        ];
        for (c, want) in flats {
            assert_eq!(codes(c, Coding::X5, want), Ok(()), "{c:?}");
        }
        // Control: limited range, whose white is 235.
        let limited = Coding {
            format: YuvFormat {
                full_range: false,
                ..Coding::X5.format
            },
            ..Coding::X5
        };
        let e = must_fail("limited range", codes([1.0; 3], limited, [255, 128, 128]));
        assert_eq!(e[0][0], 235);
        // At 10 bits, white is 1023 and no colour 512.
        let ten = Coding {
            bits: 10,
            ..Coding::X5
        };
        let YuvPlanes::U16([y, u, v]) = lens_planes(&flat([1.0; 3]), ten) else {
            panic!("8-bit planes at 10 bits");
        };
        assert!(y.iter().all(|&s| s == 1023) && u.iter().chain(&v).all(|&s| s == 512));

        // Siting. A red ramp R = x/15 across 16 columns has
        // Cr = 128 + 127.5 R. Sited left, chroma column c weighs columns
        // 2c − 1, 2c and 2c + 1 by ¼, ½ and ¼, so R = 2c/15 and
        // Cr = 128 + 17c wherever the edge doesn't reach, c = 1 to 7.
        // Centre-sited, the mean of columns 2c and 2c + 1,
        // R = (4c + 1)/30, would give 132.25 + 17c.
        let ramp = |down: bool| RgbImage {
            width: 16,
            height: 16,
            pixels: (0..256u32)
                .map(|n| {
                    let t = if down { n / 16 } else { n % 16 };
                    [f64::from(t) / 15.0, 0.0, 0.0]
                })
                .collect(),
        };
        let sited_left = |cr: &[u8]| -> Result<(), (usize, u8)> {
            for (n, &v) in cr.iter().enumerate() {
                let c = n % 8;
                if (1..8).contains(&c) && usize::from(v) != 128 + 17 * c {
                    return Err((n, v));
                }
            }
            Ok(())
        };
        let [_, _, across] = planes_8(lens_planes(&ramp(false), Coding::X5));
        assert_eq!(sited_left(&across), Ok(()));
        // Control: synth's `to_yuv`, whose 4:2:0 chroma is centre-sited.
        let [_, _, centred] = planes_8(to_yuv(&ramp(false), 8, &Coding::X5.format));
        let (n, v) = must_fail("centre-sited chroma", sited_left(&centred));
        assert_eq!(usize::from(v), 132 + 17 * (n % 8));

        // Down, chroma row r sits midway between rows 2r and 2r + 1 and
        // weighs rows 2r − 1 to 2r + 2 by ⅛, ⅜, ⅜ and ⅛, so
        // R = (2r + ½)/15 and Cr = 132.25 + 17r → 132 + 17r, for r = 1
        // to 6.
        let midway = |cr: &[u8]| -> Result<(), (usize, u8)> {
            for (n, &v) in cr.iter().enumerate() {
                let r = n / 8;
                if (1..7).contains(&r) && usize::from(v) != 132 + 17 * r {
                    return Err((n, v));
                }
            }
            Ok(())
        };
        let [_, _, down] = planes_8(lens_planes(&ramp(true), Coding::X5));
        assert_eq!(midway(&down), Ok(()));
        // Control: chroma sited with the top row, R = 2r/15, which gives
        // 128 + 17r.
        let top = Coding {
            siting: ChromaSiting::TopLeft,
            ..Coding::X5
        };
        let [_, _, topped] = planes_8(lens_planes(&ramp(true), top));
        let (n, v) = must_fail("chroma sited with the top row", midway(&topped));
        assert_eq!(usize::from(v), 128 + 17 * (n / 8));
    }

    /// A test binary's render cache renders a static scene once and keys
    /// it by rig truth, scene and frame (7.1).
    #[test]
    fn the_render_cache_renders_a_static_scene_once() {
        // The scenes the cache keeps across frames are the same at every
        // frame, on a 1° grid of directions over the sphere; Grid's
        // marker and counter move.
        let dirs: Vec<DVec3> = (0..180)
            .flat_map(|a| (0..360).map(move |b| lens_dir(f64::from(a) + 0.5, f64::from(b))))
            .collect();
        for pattern in Pattern::ALL {
            let changes = dirs
                .iter()
                .any(|&d| crate::color(pattern, d, 0) != crate::color(pattern, d, 123));
            assert_eq!(changes, moves(pattern), "{pattern:?}");
        }

        let cache = RenderCache::new();
        let rig = square_rig(32);
        let render = |pattern: Pattern, rig: &RigTruth, lens: usize, frame: u32| {
            cache.render(pattern, rig, lens, frame, 1)
        };
        let first = render(Pattern::SeamLines, &rig, 0, 0);
        assert!(Arc::ptr_eq(
            &first,
            &render(Pattern::SeamLines, &rig, 0, 57)
        ));
        assert_eq!(cache.made(), 1);
        assert_eq!(
            first.pixels,
            render_lens(Pattern::SeamLines, &rig, 0, 57, 1).pixels
        );
        // The lens, the samples, the scene and the truth are each in the
        // key.
        let mut brighter = rig.clone();
        brighter.lenses[0].gain = [1.5; 3];
        let others = [
            render(Pattern::SeamLines, &rig, 1, 0),
            cache.render(Pattern::SeamLines, &rig, 0, 0, 2),
            render(Pattern::HighFrequency, &rig, 0, 0),
            render(Pattern::SeamLines, &brighter, 0, 0),
        ];
        assert!(others.iter().all(|other| !Arc::ptr_eq(&first, other)));
        assert_eq!(cache.made(), 5);
        // Grid moves, so each of its frames is its own.
        let grid = render(Pattern::Grid, &rig, 0, 0);
        assert!(Arc::ptr_eq(&grid, &render(Pattern::Grid, &rig, 0, 0)));
        // Control: Grid's frames 0 and 1 kept as one.
        must_fail(
            "Grid's frames 0 and 1 kept as one",
            Arc::ptr_eq(&grid, &render(Pattern::Grid, &rig, 0, 1)),
        );
        assert_eq!(cache.made(), 7);

        // Through a turn from 1 s to 2 s at 30 frames a second, the
        // frames of each rest share a render, frame 30 at the turn's start
        // among rest 0's, and frames in the turn don't.
        let mut turning = rig.clone();
        turning.motion = Motion {
            fps: 30.0,
            turns: vec![Turn {
                start_s: 1.0,
                end_s: 2.0,
                axis: DVec3::Y,
                deg_per_s: 20.0,
            }],
        };
        let at = |frame: u32| render(Pattern::SeamLines, &turning, 0, frame);
        assert!(Arc::ptr_eq(&at(0), &at(30)));
        assert!(Arc::ptr_eq(&at(60), &at(75)));
        assert!(!Arc::ptr_eq(&at(0), &at(60)));
        assert_ne!(at(45).pixels, at(46).pixels);
        // A frame kept from earlier in its rest is the frame rendered
        // alone, to the bit.
        assert_eq!(
            at(75).pixels,
            render_lens(Pattern::SeamLines, &turning, 0, 75, 1).pixels
        );
        // Control: frames 45 and 46, in the turn, kept as one.
        must_fail(
            "frames in the turn kept as one",
            Arc::ptr_eq(&at(45), &at(46)),
        );
        // With a 50 ms readout, frame 29's last rows are exposed after
        // 1 s, in the turn, while all of frame 28's are before.
        let mut rolling = turning.clone();
        rolling.readout_s = 0.05;
        let rolled = |frame: u32| render(Pattern::SeamLines, &rolling, 0, frame);
        assert!(Arc::ptr_eq(&rolled(0), &rolled(28)));
        must_fail(
            "frame 29 kept with rest 0",
            Arc::ptr_eq(&rolled(0), &rolled(29)),
        );

        // Threads that ask for one picture at once get one render.
        let fresh = RenderCache::new();
        let pictures: Vec<Arc<RgbImage>> = std::thread::scope(|s| {
            let asks: Vec<_> = (0..4)
                .map(|_| s.spawn(|| fresh.render(Pattern::SeamLines, &rig, 0, 0, 1)))
                .collect();
            asks.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(fresh.made(), 1);
        assert!(pictures.iter().all(|p| Arc::ptr_eq(p, &pictures[0])));
        assert!(std::ptr::eq(RenderCache::shared(), RenderCache::shared()));
    }

    /// A render cache holds no more than its budget, the pictures asked
    /// for least recently going first, and keeps none larger than it.
    #[test]
    fn the_render_cache_holds_to_its_budget() {
        // 1 GiB by default, 75 pictures at 768², 13.5 MiB each.
        assert_eq!(RenderCache::BUDGET, 1 << 30);
        assert_eq!(RenderCache::new().budget(), RenderCache::BUDGET);
        assert_eq!(RenderCache::shared().budget(), RenderCache::BUDGET);
        assert_eq!(picture_bytes(768), 14_155_776);
        assert_eq!(RenderCache::BUDGET / picture_bytes(768), 75);

        // Grid moves, so each frame is a picture of its own: 32² of them,
        // 24 KiB each, to a budget of three.
        let rig = square_rig(32);
        let picture = picture_bytes(32);
        assert_eq!(picture, 32 * 32 * 24);
        let grid = |cache: &RenderCache, frame: u32| cache.render(Pattern::Grid, &rig, 0, frame, 1);
        let within = |cache: &RenderCache| at_most(cache.held_bytes(), 3 * picture);
        let cache = RenderCache::with_budget(3 * picture);
        let first = grid(&cache, 0);
        grid(&cache, 1);
        grid(&cache, 2);
        assert_eq!((cache.made(), cache.held()), (3, 3));
        // Frame 0, asked for again, is kept and becomes the most recent,
        // so frame 3 pushes out frame 1, now the least recently asked for.
        assert!(Arc::ptr_eq(&first, &grid(&cache, 0)));
        grid(&cache, 3);
        assert_eq!((cache.made(), cache.held()), (4, 3));
        assert_eq!(within(&cache).passes(), Some(true));
        assert!(Arc::ptr_eq(&first, &grid(&cache, 0)));
        assert_eq!(cache.made(), 4);
        // Frame 1 is rendered again, to the same picture, and pushes out
        // frame 2.
        let again = grid(&cache, 1);
        assert_eq!(cache.made(), 5);
        assert_eq!(
            again.pixels,
            render_lens(Pattern::Grid, &rig, 0, 1, 1).pixels
        );
        grid(&cache, 2);
        assert_eq!(cache.made(), 6);
        assert_eq!(within(&cache).passes(), Some(true));
        // Controls: the same asks of a cache with room for ten hold four
        // pictures, past the bound; and frame 0, not asked for again
        // before frame 3, goes first, as it would first in, first out.
        let roomy = RenderCache::with_budget(10 * picture);
        for frame in [0, 1, 2, 0, 3, 0, 1, 2] {
            grid(&roomy, frame);
        }
        assert_eq!(must_fail("room for ten", within(&roomy)), 4 * picture);
        let unasked = RenderCache::with_budget(3 * picture);
        let first = grid(&unasked, 0);
        for frame in 1..4 {
            grid(&unasked, frame);
        }
        must_fail(
            "frame 0 kept, not asked for again",
            Arc::ptr_eq(&first, &grid(&unasked, 0)),
        );

        // A picture larger than the whole budget is rendered at each ask
        // and never kept.
        let small = RenderCache::with_budget(picture - 1);
        let (a, b) = (grid(&small, 0), grid(&small, 0));
        assert_eq!((small.made(), small.held(), small.held_bytes()), (2, 0, 0));
        assert_eq!(a.pixels, b.pixels);
    }

    /// Whether the directions `theta` degrees off `lens`'s axis along its
    /// image's +x, +y, −x and −y all land outside its frame, or the first
    /// that lands inside.
    fn cut(lens: &LensTruth, theta: f64) -> Result<(), DVec2> {
        let side = f64::from(lens.side);
        for phi in [0.0, 90.0, 180.0, 270.0] {
            let at = lens.model.forward(lens_dir(theta, phi)).unwrap();
            if (0.0..side).contains(&at.x) && (0.0..side).contains(&at.y) {
                return Err(at);
            }
        }
        Ok(())
    }

    /// The X5-like truth at 768² under maps A, B and C: centres 2 to 3 px
    /// off the slot's on each axis, and a frame that cuts the image
    /// circle on all four sides at 106°, where r is 393 to 399 px under C.
    #[test]
    fn x5_like_cuts_its_circle_on_all_four_sides() {
        let cal = x5_like();
        for canvas in [TruthCanvas::A, TruthCanvas::B, TruthCanvas::C] {
            let planted = Planted {
                canvas,
                ..planted(TruthEuler::PLACEHOLDER)
            };
            let rig = compose(&cal, &planted, 0, [X5_LIKE_SIDE; 2]);
            for (t, lens) in rig.lenses.iter().enumerate() {
                let c = lens.model.intrinsics().1;
                let off = (c - DVec2::splat(384.0)).abs();
                assert!(
                    off.min_element() >= 2.0 && off.max_element() <= 3.0,
                    "{canvas:?}, track {t}: centre {c}"
                );
                assert_eq!(lens.theta_max, IMAGE_CIRCLE_DEG.to_radians());
                assert_eq!(cut(lens, IMAGE_CIRCLE_DEG), Ok(()), "{canvas:?}, track {t}");
                // Control: 95° off the axis, inside on all four sides.
                must_fail("95° taken for the circle's edge", cut(lens, 95.0));
                if canvas == TruthCanvas::C {
                    for k in 0..8 {
                        let d = lens_dir(IMAGE_CIRCLE_DEG, f64::from(k) * 45.0);
                        let r = (lens.model.forward(d).unwrap() - c).length();
                        assert!((393.0..=399.0).contains(&r), "track {t}, {k}: r = {r}");
                    }
                }
            }
        }
    }

    /// `kb_like` swaps each lens for an equidistant KB lens of
    /// `KB_LIKE_K` on the same centre that reaches as far along +x at
    /// 106°, and keeps the rest of the rig.
    #[test]
    fn kb_like_reaches_as_far_as_the_lens_it_replaces() {
        let rig = compose(
            &x5_like(),
            &planted(TruthEuler::PLACEHOLDER),
            0,
            [X5_LIKE_SIDE; 2],
        );
        let kb = kb_like(&rig);
        assert_eq!(
            RigTruth {
                lenses: rig.lenses,
                ..kb.clone()
            },
            rig
        );
        let edge = lens_dir(IMAGE_CIRCLE_DEG, 0.0);
        let reach = |model: &TruthModel| model.forward(edge).unwrap().x;
        for (mei, lens) in rig.lenses.iter().zip(&kb.lenses) {
            let TruthModel::Kb { base, f, cx, cy, k } = lens.model else {
                panic!("{:?} for a KB lens", lens.model);
            };
            assert_eq!((base, k), (KbBase::Equidistant, KB_LIKE_K));
            assert_eq!(
                LensTruth {
                    model: mei.model,
                    ..*lens
                },
                *mei
            );
            assert_eq!(lens.model.forward(DVec3::Z), Some(mei.model.intrinsics().1));
            let err = (reach(&lens.model) - reach(&mei.model)).abs();
            assert!(err <= 1e-9, "{err} px");
            assert_eq!(cut(lens, IMAGE_CIRCLE_DEG), Ok(()));
            let (rad, px) = round_trip(&lens.model, 106.0, |at| lens.model.inverse(at));
            assert!(rad <= 1e-12 && px <= 1e-9, "{rad} rad, {px} px");
            // Control: Mei's own focal length for the KB lens's.
            let naive = TruthModel::Kb {
                base,
                f: mei.model.intrinsics().0.x,
                cx,
                cy,
                k,
            };
            assert_ne!(f, mei.model.intrinsics().0.x);
            must_fail(
                "Mei's focal length",
                at_most((reach(&naive) - reach(&mei.model)).abs(), 1e-9),
            );
        }
    }

    /// The cost of a frame of the X5-like rig on both lenses at 768² and
    /// ss = 2 (7.1 guesses about 2 s on 4 threads).
    #[test]
    #[ignore = "tier2: benchmark, the CPU only"]
    fn bench_render_lens_x5_like_pair() {
        let rig = compose(
            &x5_like(),
            &planted(TruthEuler::PLACEHOLDER),
            0,
            [X5_LIKE_SIDE; 2],
        );
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(16);
        bench::note(format!("render_lens threads: {threads}"));
        bench::measure("render_lens: X5-like, 2 × 768², ss 2", "s", || {
            let start = std::time::Instant::now();
            for lens in 0..2 {
                std::hint::black_box(render_lens(Pattern::SeamLines, &rig, lens, 0, 2));
            }
            start.elapsed().as_secs_f64()
        });
    }
}
