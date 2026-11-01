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
//! synth's own forward and inverse ([`TruthModel`]).
//!
//! The frames are 6.2's. A lens frame is Mei's and OpenCV's: +x right in
//! the image, +y down, +z along the optical axis. The rig frame is
//! Spherecut's world (DECISIONS D33): +X right, +Y up, and the front lens
//! looking along −Z. Every frame is right-handed, and a rotation by a
//! positive angle about an axis turns by the right-hand rule.

use glam::{DQuat, DVec2, DVec3};
use spherecut_core::video::ChromaSiting;

use crate::yuv::{Chroma, YuvFormat, YuvPlanes};
use crate::{Pattern, RgbImage};

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
    #[expect(unused_variables, reason = "M3.3 unit (c) writes the body")]
    pub fn forward(&self, d: DVec3) -> Option<DVec2> {
        todo!()
    }

    /// Synth's inverse, pixel to lens-frame unit direction, which rung 0
    /// holds sphere's forward to (4.2): a damped 2-D Newton on the full
    /// forward with a numeric Jacobian and bisection as its fallback,
    /// deliberately not sphere's bracketed radial Newton, then the
    /// closed-form lift (7.1). `None` where it finds no direction.
    #[expect(unused_variables, reason = "M3.3 unit (c) writes the body")]
    pub fn inverse(&self, px: DVec2) -> Option<DVec3> {
        todo!()
    }
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
    /// frame to the scene's: the identity before the first turn.
    #[expect(unused_variables, reason = "M3.3 unit (c) writes the body")]
    pub fn at(&self, t_s: f64) -> DQuat {
        todo!()
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
#[expect(unused_variables, reason = "M3.3 unit (c) writes the body")]
pub fn compose(cal: &CalibTruth, planted: &Planted, front_track: u8, sides: [u32; 2]) -> RigTruth {
    todo!()
}

/// What track `lens` shows of `pattern` at `frame`, side × side, each
/// pixel the mean of `ss` × `ss` samples (7.1): synth's inverse, `to_rig`
/// under the rig's motion at the sample's own time, the ray from
/// `centre_m` to the scene's sphere or the ground, [`crate::color`], the
/// gain and the vignette in linear light, then the sRGB OETF. Black past
/// `theta_max` and where the inverse has no direction, as on the camera.
#[expect(unused_variables, reason = "M3.3 unit (c) writes the body")]
pub fn render_lens(pattern: Pattern, rig: &RigTruth, lens: usize, frame: u32, ss: u32) -> RgbImage {
    todo!()
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
#[expect(unused_variables, reason = "M3.3 unit (c) writes the body")]
pub fn lens_planes(img: &RgbImage, coding: Coding) -> YuvPlanes {
    todo!()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fmt::Debug;
    use std::hash::Hash;

    use spherecut_testkit::control::must_fail;

    use super::*;

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

    /// compose's E_b for 8 `TruthEuler` values, the placeholder among
    /// them, against values worked out by hand with the arithmetic in the
    /// comments (7.1).
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn compose_turns_eight_euler_values_as_worked_by_hand() {
        todo!()
    }

    /// Block 1's V6 centre and focal (cx 8075.73, so c_local 2699.73; fx
    /// 4284.40) under A, B and C: at 3840² cx 1928.38 under A and C and
    /// 1928.48 under B, f 3097.16 under A and B and 3060.29 under C; at
    /// 768² cx 385.68 under A and C and 385.70 under B (7.1).
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn compose_scales_block_1_under_each_canvas_map() {
        todo!()
    }

    /// c_1 for Pat's V6 t_1 = (0.637, 0.001, −31.525) mm under both tz
    /// signs: `Negated` flips all three components.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn compose_places_lens_1_under_both_tz_signs() {
        todo!()
    }

    /// The binding puts each block on its track, a front track that is
    /// block 1's turns both lenses 180° about +Y, and `upright` turns both
    /// a quarter turn about −Z.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn compose_follows_the_binding_the_front_track_and_upright() {
        todo!()
    }

    /// Synth's inverse inverts its forward on every truth model, each V6
    /// reading and each KB base among them.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn truth_inverse_inverts_forward() {
        todo!()
    }

    /// Synth's forward on values worked out by hand: the centre on the
    /// axis, tan θ at ξ = 0 and tan(θ/2) at ξ = 1 with no distortion, and
    /// each KB base.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn truth_forward_lands_where_worked_by_hand() {
        todo!()
    }

    /// `Motion::at` turns at each turn's rate and holds still between
    /// turns.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn motion_turns_at_its_rate_and_rests_between() {
        todo!()
    }

    /// Each pixel of `render_lens` at ss = 1 is the pattern in the
    /// direction synth's inverse and `to_rig` give its centre.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn render_lens_shows_the_scene_through_the_truth() {
        todo!()
    }

    /// Past `theta_max`, and where the inverse has no direction, the
    /// picture is black.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn render_lens_is_black_past_theta_max() {
        todo!()
    }

    /// A finite scene and the ground plane are seen from each lens's own
    /// centre, so the two lenses' pictures of one point differ by the
    /// parallax 6.2 predicts.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn render_lens_sees_the_scene_and_the_ground_from_its_centre() {
        todo!()
    }

    /// A late lens and a rolling shutter render each row at its own time
    /// under the rig's motion.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn render_lens_renders_late_lenses_and_rows_at_their_times() {
        todo!()
    }

    /// The gain and the vignette apply in linear light.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn render_lens_applies_gain_and_vignette_in_linear_light() {
        todo!()
    }

    /// `lens_planes` codes as the X5 does: full range, BT.709, chroma
    /// sited left.
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn lens_planes_codes_as_the_x5_does() {
        todo!()
    }

    /// A test binary's render cache renders a static scene once and keys
    /// it by rig truth, scene and frame (7.1).
    #[test]
    #[ignore = "M3.3 unit (c)"]
    fn the_render_cache_renders_a_static_scene_once() {
        todo!()
    }
}
