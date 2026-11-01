//! A stitched lens pair (M3 plan 6.2, DECISIONS D64): the conventions a
//! camera's files leave open, held as data in [`Geometry`], and the
//! [`Rig`] that [`Rig::build`] makes from a calibration, a geometry and a
//! tune.
//!
//! The rig frame is Spherecut's world at zero stabilisation (D33): +X
//! right, +Y up, and the front lens looking along −Z. A lens frame is
//! [`Unified`](crate::fisheye::Unified)'s: +x right in the image, +y
//! down, +z along the optical axis. Every frame is right-handed, and a
//! rotation by a positive angle about an axis turns by the right-hand
//! rule. For calibration block b, a rig-frame direction reaches the lens
//! frame as
//!
//! ```text
//! lens_from_rig_b = E_b · F · Q_tune_bᵀ · T_b · S
//! ```
//!
//! where S = U · Y: Y is the identity, or the 180° turn about +Y that
//! puts the front track's lens at −Z, and U the quarter turn about −Z
//! that [`Geometry::upright`] names; T_b is the identity for block 0 and,
//! for block 1, a 180° turn about [`Geometry::turn`], which the strings
//! omit; Q_tune_b is D33's yaw, pitch and roll of block b's tune in that
//! view frame; F = diag(1, −1, −1); and E_b is block b's stored yaw, pitch
//! and roll, composed by [`Geometry::extrinsics`]. The lenses sit at
//!
//! ```text
//! c_0 = 0,  c_1 = rig_from_lens_0 · (sign(tz) · t_1),  o = (c_0 + c_1)/2
//! ```
//!
//! and a scene point 1/q from the origin o along the rig direction d is
//! seen by lens b along lens_from_rig_b · normalize(d − q (c_b − o)).
//! Under [`Extrinsics::Fitted`] the two formulas read differently: both
//! E_b are the identity, T_1 is F⁻¹ · relative · F, so that
//! [`Geometry::turn`] isn't read, and sign(tz) · t_1 is the measured
//! `translation_m` as it stands, so that [`Geometry::tz`] isn't read
//! either (see [`Rig::build`]).
//!
//! Each convention is a small generic space ([`EulerConv`], 1152 values;
//! `fisheye::V6Reading`, 21), so that no reader's choice is copied or
//! favoured, and settling one changes a table entry, never the shader.
//! These types share no code with the synthetic camera's
//! (`spherecut_synth::lens`), which composes the same rig from the plan's
//! text with its own code to check this one (4.1 rule 2).

use std::f64::consts::{FRAC_PI_2, PI};
use std::fmt;

use glam::{DQuat, DVec2, DVec3, DVec4};

use crate::fisheye::{Canonical, F, LensModel, Unified, V6Reading};
use crate::orient::Ypr;

/// How a camera's calibration maps to its video, as data (6.2): one
/// value of each convention the files leave open, with its status and
/// the evidence for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    pub binding: Binding,
    pub canvas: CanvasMap,
    pub extrinsics: Extrinsics,
    pub turn: TurnAxis,
    pub tz: TzSign,
    /// The quarter turn U about −Z applied to both lenses, which the seam
    /// can't see; 8.6's up-and-down rule sets it for whichever of a
    /// class's twins the class reports.
    pub upright: QuarterTurn,
    /// Which calibration the rig reads, V3 or V6.
    pub generation: Generation,
    /// What V6's d5 to d12 mean; V3 doesn't read it.
    pub v6: V6Reading,
    pub status: Status,
    /// A sentence and a docs path.
    pub evidence: String,
}

/// Which track each calibration block describes (U2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    /// Block b reads track b.
    Direct,
    /// Block b reads track 1 − b.
    Reversed,
}

impl Binding {
    pub fn all() -> impl Iterator<Item = Binding> {
        [Binding::Direct, Binding::Reversed].into_iter()
    }
}

/// How a block's numbers, in canvas pixels, scale to a square track of
/// side s (U3). src is the side of a block's square on the canvas
/// ([`Calibration::src`], 5376 on the X5), dst is field 27's window
/// ([`Calibration::dst`], 5312), and c_local is the block's centre in its
/// own square ([`LensCalib::cx`]). An LRV half has s = 832.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CanvasMap {
    /// Focal f · s/dst, centre c_local · s/src.
    A,
    /// Focal f · s/dst, centre (c_local − (src − dst)/2) · s/dst: a window
    /// of dst centred on the block's square.
    B,
    /// Focal f · s/src, centre c_local · s/src: the track shows the whole
    /// square.
    C,
    /// C's focal times `scale`, and C's centre: the fit's own scale (8.6).
    Fitted { scale: f64 },
}

impl CanvasMap {
    /// A, B and C. `Fitted` takes any scale, so it isn't listed.
    pub fn all() -> impl Iterator<Item = CanvasMap> {
        [CanvasMap::A, CanvasMap::B, CanvasMap::C].into_iter()
    }

    /// What a block's focal, in canvas pixels, is multiplied by on a
    /// square track of side `side`: s/dst under A and B, s/src under C,
    /// and C's times the scale under `Fitted`.
    pub fn focal_scale(self, src: u32, dst: u32, side: u32) -> f64 {
        let (src, dst, s) = (f64::from(src), f64::from(dst), f64::from(side));
        match self {
            CanvasMap::A | CanvasMap::B => s / dst,
            CanvasMap::C => s / src,
            CanvasMap::Fitted { scale } => s / src * scale,
        }
    }

    /// A point of a block's own square in canvas pixels, such as its
    /// centre c_local, in the pixels of a square track of side `side`:
    /// c_local · s/src, or under B (c_local − (src − dst)/2) · s/dst.
    pub fn to_slot(self, c_local: DVec2, src: u32, dst: u32, side: u32) -> DVec2 {
        let (src, dst, s) = (f64::from(src), f64::from(dst), f64::from(side));
        match self {
            CanvasMap::B => (c_local - DVec2::splat((src - dst) / 2.0)) * (s / dst),
            CanvasMap::A | CanvasMap::C | CanvasMap::Fitted { .. } => c_local * (s / src),
        }
    }

    /// The inverse of [`CanvasMap::to_slot`]: a point of a track of side
    /// `side` in its block's square, in canvas pixels. The Stitch pane
    /// turns a drag in the slot into the tune's canvas pixels by it.
    pub fn to_canvas(self, px: DVec2, src: u32, dst: u32, side: u32) -> DVec2 {
        let (src, dst, s) = (f64::from(src), f64::from(dst), f64::from(side));
        match self {
            CanvasMap::B => px * (dst / s) + DVec2::splat((src - dst) / 2.0),
            CanvasMap::A | CanvasMap::C | CanvasMap::Fitted { .. } => px * (src / s),
        }
    }
}

/// How the lenses' rotations and centres come from the calibration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Extrinsics {
    /// Each block's stored angles, composed by an [`EulerConv`] into its
    /// E_b, and its stored translation under [`Geometry::tz`].
    Stored(EulerConv),
    /// A measured relative rotation and translation, for a camera whose
    /// composition lies outside the space (8.4): E_0 is the identity, the
    /// gauge the seam can't see; `relative` turns lens 0's frame into lens
    /// 1's, the back-to-back turn included, so that with both lenses'
    /// angles untuned lens_from_rig_1 = relative · lens_from_rig_0; and
    /// `translation_m` is lens 1's centre in lens 0's frame, the vector
    /// 6.2 writes as sign(tz) · t_1, so [`Geometry::tz`] doesn't apply.
    /// `relative` carries the turn, so [`Geometry::turn`] doesn't apply
    /// either: block 1's view frame is lens 0's carried by it (see
    /// [`Rig::build`]). It need not be of unit length, but may not be 0.
    Fitted {
        relative: DQuat,
        translation_m: DVec3,
    },
}

/// The axis of the 180° turn T_1 from the rig frame to block 1's view
/// frame, which the strings omit (insta360.md §4.5). Under
/// [`Extrinsics::Fitted`] the measured relative rotation carries the turn,
/// and this isn't read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnAxis {
    Y,
    X,
}

impl TurnAxis {
    pub fn all() -> impl Iterator<Item = TurnAxis> {
        [TurnAxis::Y, TurnAxis::X].into_iter()
    }

    /// T_1, the 180° turn about this axis.
    fn half_turn(self) -> DQuat {
        match self {
            TurnAxis::Y => DQuat::from_rotation_y(PI),
            TurnAxis::X => DQuat::from_rotation_x(PI),
        }
    }
}

/// The sign 6.2's c_1 = rig_from_lens_0 · (sign(tz) · t_1) gives the
/// stored translation t_1 (U5): a sign, so it scales the whole vector.
///
/// `Negated` flips all of t_1, not only its z. What a camera can leave
/// open is which way the stored vector points along the baseline, from
/// lens 0 to lens 1 or back, and that negates the whole vector. Flipping
/// z alone would be the reflection diag(1, 1, −1), which neither that nor
/// any change between the right-handed frames used here (D33; det F =
/// +1) gives. And since c_0 = 0, flipping all of c_1 flips c_b − o for
/// both lenses, so `Negated` at q is exactly `AsStored` at −q: what 8.6's
/// tz rule reads, since under the wrong sign the wall fits q < 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TzSign {
    /// t_1 is lens 1's centre in lens 0's frame: with tz = −31.5 mm it
    /// sits behind lens 0 on lens 0's axis, the physical layout.
    AsStored,
    /// −t_1, all three components: the stored vector read from lens 1 to
    /// lens 0.
    Negated,
}

impl TzSign {
    pub fn all() -> impl Iterator<Item = TzSign> {
        [TzSign::AsStored, TzSign::Negated].into_iter()
    }

    /// The sign on t_1.
    fn sign(self) -> f64 {
        match self {
            TzSign::AsStored => 1.0,
            TzSign::Negated => -1.0,
        }
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
    const ALL: [QuarterTurn; 4] = [
        QuarterTurn::Zero,
        QuarterTurn::Plus90,
        QuarterTurn::Minus90,
        QuarterTurn::Half,
    ];

    pub fn all() -> impl Iterator<Item = QuarterTurn> {
        QuarterTurn::ALL.into_iter()
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

    /// U, this turn about −Z.
    fn about_neg_z(self) -> DQuat {
        DQuat::from_rotation_z(-self.degrees().to_radians())
    }
}

/// The calibration generation the rig reads (insta360.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Generation {
    /// Mei with k1 k2 k3 and p1 p2.
    V3,
    /// Mei with d0 to d12, read through a [`V6Reading`].
    V6,
}

impl Generation {
    pub fn all() -> impl Iterator<Item = Generation> {
        [Generation::V3, Generation::V6].into_iter()
    }
}

/// How far a geometry is to be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// A guess or a placeholder, or measured on other firmware.
    Provisional,
    /// Settled on real footage by 8.6's rules.
    Settled,
    /// The fit's own numbers, where no listed value fits (8.7).
    Fitted,
    /// Pat's or an agent's choice.
    User,
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

    /// The turn by `rad` radians about this axis.
    fn turn(self, rad: f64) -> DQuat {
        match self {
            Axis::X => DQuat::from_rotation_x(rad),
            Axis::Y => DQuat::from_rotation_y(rad),
            Axis::Z => DQuat::from_rotation_z(rad),
        }
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

    /// Where the angle sits in a stored (yaw, pitch, roll) triple, and in
    /// [`EulerConv`]'s `axes` and `signs`.
    fn index(self) -> usize {
        match self {
            Angle::Yaw => 0,
            Angle::Pitch => 1,
            Angle::Roll => 2,
        }
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

/// How a block's stored yaw, pitch and roll compose into its rotation
/// E_b, from the view frame after F to the lens frame (6.2): 6 axis
/// assignments, 6 orders, 8 sign patterns and 4 roll offsets, 1152 values
/// in all. It has no direction field, rig to lens or lens to rig: the
/// inverse of R_a(α) R_b(β) R_c(γ) is R_c(−γ) R_b(−β) R_a(−α), another
/// value of the space, so with one every rotation would be listed twice.
///
/// The labels read as the synthetic camera's `TruthEuler` does (amendment
/// 3): angle a turns about `axes[a]` by `signs[a]` · (its stored value,
/// plus `roll_offset` when a is the roll), the signs and axes indexed by
/// angle whatever the order, and the offset added before the roll's sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EulerConv {
    /// The axis each stored angle turns about, by angle (yaw, pitch,
    /// roll): a permutation of X, Y and Z.
    pub axes: [Axis; 3],
    /// The order the three rotations compose in, first applied first: a
    /// permutation of yaw, pitch and roll. `[a, b, c]` is
    /// E = R_c · R_b · R_a, acting on column vectors.
    pub order: [Angle; 3],
    /// The sign each stored angle is taken with, by angle (yaw, pitch,
    /// roll).
    pub signs: [Sign; 3],
    /// A quarter turn added to the stored roll before its sign. Upright
    /// X5 video stores a roll of about 90° (3.1).
    pub roll_offset: QuarterTurn,
}

impl EulerConv {
    /// The X5's placeholder until M3.6's fit (5.3), "the identity with
    /// the −90° roll offset": the value whose every field reads the angles
    /// as they are named. The signs are all plus and the order is yaw,
    /// pitch, roll, first applied first; the axes are D33's meanings of the
    /// names carried into the lens frame by F: yaw about the up-down axis
    /// (y), pitch about the right axis (x), roll about the view axis (z),
    /// the optical axis, about which the X5's sensors sit a quarter turn
    /// (3.1). So E_b = R_z(roll − 90°) · R_x(pitch) · R_y(yaw), and 11.1's
    /// worked JSON (`"axes": ["y", "x", "z"]`, `"order": ["yaw", "pitch",
    /// "roll"]`, `"signs": [1, 1, 1]`, `"roll_offset": -90`) is this value.
    /// The offset cancels the stored quarter turn, so on Pat's V3 and V6
    /// angles each block's E_b is within 1° of the identity and lens 0
    /// looks along −Z.
    pub const PLACEHOLDER: EulerConv = EulerConv {
        axes: [Axis::Y, Axis::X, Axis::Z],
        order: [Angle::Yaw, Angle::Pitch, Angle::Roll],
        signs: [Sign::Plus; 3],
        roll_offset: QuarterTurn::Minus90,
    };

    /// Whether this is one of the space's 1152 values: `axes` and `order`
    /// each a permutation. The fields are public, so a value can be made
    /// that isn't; [`Rig::build`] refuses it.
    pub fn is_valid(&self) -> bool {
        all_differ(self.axes) && all_differ(self.order)
    }

    /// All 1152 values.
    pub fn all() -> impl Iterator<Item = EulerConv> {
        use Angle::{Pitch, Roll, Yaw};
        use Axis::{X, Y, Z};
        const AXES: [[Axis; 3]; 6] = [
            [X, Y, Z],
            [X, Z, Y],
            [Y, X, Z],
            [Y, Z, X],
            [Z, X, Y],
            [Z, Y, X],
        ];
        const ORDERS: [[Angle; 3]; 6] = [
            [Yaw, Pitch, Roll],
            [Yaw, Roll, Pitch],
            [Pitch, Yaw, Roll],
            [Pitch, Roll, Yaw],
            [Roll, Yaw, Pitch],
            [Roll, Pitch, Yaw],
        ];
        let sign = |minus: bool| if minus { Sign::Minus } else { Sign::Plus };
        (0..6 * 6 * 8 * 4).map(move |i: usize| EulerConv {
            axes: AXES[i / 192],
            order: ORDERS[i / 32 % 6],
            signs: [i & 16 != 0, i & 8 != 0, i & 4 != 0].map(sign),
            roll_offset: QuarterTurn::ALL[i % 4],
        })
    }

    /// E_b for a block's stored yaw, pitch and roll in degrees: the
    /// rotation from the view frame after F to the lens frame. Angle a
    /// turns about `axes[a]` by `signs[a]` · (its stored value, plus the
    /// roll offset when a is the roll), and the three compose in `order`,
    /// first applied first.
    pub fn rotation(&self, ypr_deg: [f64; 3]) -> DQuat {
        let turn = |angle: Angle| {
            let i = angle.index();
            let mut deg = ypr_deg[i];
            if angle == Angle::Roll {
                deg += self.roll_offset.degrees();
            }
            if self.signs[i] == Sign::Minus {
                deg = -deg;
            }
            self.axes[i].turn(deg.to_radians())
        };
        let [first, second, third] = self.order;
        (turn(third) * turn(second) * turn(first)).normalize()
    }
}

/// A camera's stored calibration, as [`Rig::build`] reads it: both
/// blocks of each generation the file holds, in canvas pixels, degrees
/// and metres. This is sphere's own type, so that core, render and the
/// project need no camera crate; the camera crate fills it from the
/// strings (M3.5, plan 5.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// The side of each block's square on the canvas, the canvas's height
    /// (src: 5376 on the X5).
    pub src: u32,
    /// Field 27's window (dst: 5312 on the X5), which maps A and B read.
    pub dst: u32,
    /// V3's two blocks, by block, if the file holds them.
    pub v3: Option<[LensCalib; 2]>,
    /// V6's two blocks, by block, if the file holds them.
    pub v6: Option<[LensCalib; 2]>,
}

/// One calibration block's stored numbers. Block 0 is the canvas's left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensCalib {
    /// Mei's ξ.
    pub xi: f64,
    pub fx: f64,
    pub fy: f64,
    /// The principal point in canvas pixels, local to the block's own
    /// square: block 1's stored cx less src (8075.73 − 5376 = 2699.73 on
    /// Pat's V6). It and `cy` are read as corner-origin, pixel (i, j)
    /// covering [i, i + 1) × [j, j + 1), as a slot's pixels are, so that a
    /// canvas map scales the centre as it scales the side. That is an
    /// assumption about the camera, which M3.6's fit can test.
    pub cx: f64,
    pub cy: f64,
    /// The stored yaw, pitch and roll, in degrees.
    pub ypr_deg: [f64; 3],
    /// The stored translation tx, ty and tz, in metres (block 0's is 0).
    pub t_m: [f64; 3],
    pub terms: CalibTerms,
}

/// A block's distortion terms as its generation stores them, in token
/// order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalibTerms {
    /// Radial k1, k2 and k3, then Brown's p1 and p2.
    V3 { k: [f64; 3], p: [f64; 2] },
    /// d0 to d12, read through a [`V6Reading`].
    V6 { d: [f64; 13] },
}

impl CalibTerms {
    /// The generation these terms belong to.
    pub fn generation(&self) -> Generation {
        match self {
            CalibTerms::V3 { .. } => Generation::V3,
            CalibTerms::V6 { .. } => Generation::V6,
        }
    }
}

/// Adjustments on top of the geometry (6.2, 11.1): one per lens, and the
/// seam's width and distance. The gains aren't here: under Auto they
/// change every frame and sphere can't name core's types, so they are the
/// media's, in core. The default is what a new source opens with
/// (11.5), inside every one of `validate`'s bounds (11.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tune {
    /// By calibration block.
    pub lenses: [LensTune; 2],
    /// The feather's half width h, in degrees: each lens's weight falls
    /// from 1 to 0 over 90° ± h (the pane's seam width is 2h). 0 to 15,
    /// and at most min(θ_max, θ_frame) − 90° for each lens. 5 by default,
    /// question 3's recommendation.
    pub feather_deg: f64,
    /// The stitch distance as q = 1/D, in m⁻¹: 0 is infinitely far. 0 to
    /// 3.34 (D ≥ 0.3 m). 0.5 by default, 2 m, question 3's recommendation
    /// and 11.1's worked example, until Pat answers question 3.
    pub q_per_m: f64,
}

impl Default for Tune {
    fn default() -> Tune {
        Tune {
            lenses: [LensTune::default(); 2],
            feather_deg: 5.0,
            q_per_m: 0.5,
        }
    }
}

/// One lens's adjustments (6.2, 11.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensTune {
    /// D33's yaw, pitch and roll of the lens in its block's view frame,
    /// in degrees: yaw > 0 turns the lens right, as seen. Each within
    /// ±15°; 0 by default.
    pub ypr_deg: [f64; 3],
    /// A shift of the principal point in canvas pixels, so that one tune
    /// serves the master and the LRV. Within ±64; 0 by default.
    pub centre_px: [f64; 2],
    /// A factor on the focal, 0.9 to 1.1; 1 by default.
    pub radius_scale: f64,
    /// Added to ξ: 0 unless an own calibration fits it (8.7).
    pub xi_delta: f64,
    /// θ_max, the widest angle off the axis the lens is used to, in
    /// degrees: 85 to 110 and below the model's `limit_theta`; the pane's
    /// field of view is twice it. 100 by default, where the X5's
    /// calibration puts its image height (3.1).
    pub theta_max_deg: f64,
}

impl Default for LensTune {
    fn default() -> LensTune {
        LensTune {
            ypr_deg: [0.0; 3],
            centre_px: [0.0; 2],
            radius_scale: 1.0,
            xi_delta: 0.0,
            theta_max_deg: 100.0,
        }
    }
}

/// A lens pair ready to sample (6.2): each block's model in its slot's
/// pixels, its rotation and centre, and the seam's numbers. It is built
/// once per source revision; the frame's gains go to the renderer beside
/// it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rig {
    /// By calibration block.
    pub lenses: [RigLens; 2],
    /// The stitch distance as q = 1/D, in m⁻¹.
    pub q: f64,
    /// The feather's half width h, in radians.
    pub feather: f64,
    /// The mask's margin at LOD 0, in texels: at LOD λ it is
    /// margin_texels · 2^λ (2^(λ+1) texels, 6.2, so 2), with λ clamped at
    /// 0, so that a negative LOD's footprint is LOD 0's.
    pub margin_texels: f64,
}

/// One block's lens in a [`Rig`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigLens {
    /// The track whose picture this block describes, from the binding.
    pub track: usize,
    /// The model in its slot's pixels: the canvas map, the track's side
    /// and the tune folded in.
    pub model: LensModel,
    /// The rotation from the rig frame to the lens frame.
    pub lens_from_rig: DQuat,
    /// c_b − o, in metres: the lens's centre from the rig's origin, the
    /// midpoint between the lenses.
    pub centre_m: DVec3,
    /// θ_max, in radians.
    pub theta_max: f64,
    /// θ_frame (6.1), in radians: the smallest θ at which the lens's
    /// circle meets its slot's edge.
    pub theta_frame: f64,
    /// The slot's side in texels, its track's, which the mask's frame
    /// term reads.
    pub side: u32,
    /// The model's `limit_theta`, in radians, found once: where
    /// [`Rig::unproject`]'s inverse stops.
    pub limit_theta: f64,
    /// How many texels the image moves per radian of θ at θ_max, the
    /// least over the azimuth: the mask's margin in angle is
    /// margin_texels · 2^λ over it (6.2's "texels in degrees at θ_max",
    /// 10.3's texels per radian).
    pub texels_per_radian: f64,
    /// The least texels per radian from 90° to θ_max, over the azimuth:
    /// what [`Rig::feather_at`] turns the margin into an angle by. Where
    /// the slope falls towards θ_max, as on Pat's lenses, it is
    /// `texels_per_radian`; where it rises, as on a stereographic lens (Mei
    /// at ξ = 1 is one), it is the slope at 90°, so that the feather's band
    /// still ends inside the mask where the frame cuts it short. It is
    /// found at the range's two ends, which bound a slope that is monotone
    /// over it, as each lens model's is here.
    pub band_texels_per_radian: f64,
}

/// The azimuths, every 10°, over which [`RigLens::texels_per_radian`]
/// takes its least value.
const AZIMUTHS: u32 = 36;

/// The step of the backward difference that measures texels per radian:
/// backward, so that a θ_max at the limit doesn't step past the fold.
const SLOPE_STEP: f64 = 1e-5;

/// The narrowest feather the mask's margin or the frame can shrink h to
/// (6.2), in radians.
const MIN_FEATHER: f64 = 0.5 * PI / 180.0;

impl RigLens {
    /// A lens from its model, rotation, centre, θ_max (radians) and slot
    /// side, with the numbers it derives found once: the model's
    /// `limit_theta`, its θ_frame on a slot of `side`, and its texels per
    /// radian at θ_max and from 90° to θ_max. [`Rig::build`] makes its
    /// lenses by this; a test or a camera with no calibration (4.2's
    /// Kannala–Brandt rig) makes a [`Rig`] of its own from it.
    pub fn new(
        track: usize,
        model: LensModel,
        lens_from_rig: DQuat,
        centre_m: DVec3,
        theta_max: f64,
        side: u32,
    ) -> RigLens {
        let limit_theta = model.limit_theta();
        let [at_theta_max, at_90] =
            texels_per_radian(&model, [theta_max, theta_max.min(FRAC_PI_2)]);
        RigLens {
            track,
            model,
            lens_from_rig,
            centre_m,
            theta_max,
            theta_frame: frame_theta(&model, limit_theta, f64::from(side)),
            side,
            limit_theta,
            texels_per_radian: at_theta_max,
            band_texels_per_radian: at_theta_max.min(at_90),
        }
    }
}

/// Why [`Rig::build`] or [`theta_frame`] refused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RigError {
    /// The calibration holds no blocks of the generation the geometry
    /// reads.
    NoGeneration(Generation),
    /// A block's terms are of another generation than the blocks they
    /// are listed with.
    Terms {
        generation: Generation,
        block: usize,
    },
    /// The geometry's V6 reading isn't one of the 21: it reads d7 and d8
    /// twice ([`V6Reading::is_valid`]).
    Reading(V6Reading),
    /// θ_max lies past where the lens's r(θ) stops rising.
    PastLimit {
        block: usize,
        theta_max_deg: f64,
        limit_deg: f64,
    },
    /// `front_track` is neither 0 nor 1.
    FrontTrack(u8),
    /// A track's side is 0.
    Side { track: usize },
    /// A number the rig is built from is NaN or infinite.
    NotFinite(&'static str),
    /// A number the rig scales or divides by is 0 or below: a block's
    /// focal, a lens's radius scale or θ_max, the canvas map's scale, or
    /// the calibration's src or dst. A focal of 0 leaves a lens no texels
    /// per radian, so that it covers nothing; a negative one mirrors or
    /// turns its image.
    NotPositive(&'static str),
    /// The Euler convention's axes or its order isn't a permutation, so
    /// it isn't one of the space's 1152 values.
    NotPermutation(&'static str),
    /// A quaternion that should give a rotation has no length to
    /// normalise: the fitted relative rotation.
    NotRotation(&'static str),
}

impl fmt::Display for RigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RigError::NoGeneration(g) => write!(f, "the calibration has no {g:?} blocks"),
            RigError::Terms { generation, block } => {
                write!(f, "block {block}'s terms are not {generation:?}'s")
            }
            RigError::Reading(v) => {
                write!(f, "the V6 reading {v:?} reads d7 and d8 twice")
            }
            RigError::PastLimit {
                block,
                theta_max_deg,
                limit_deg,
            } => write!(
                f,
                "block {block}'s θ_max of {theta_max_deg}° lies past its lens's limit of \
                 {limit_deg}°"
            ),
            RigError::FrontTrack(t) => write!(f, "front track {t} is neither 0 nor 1"),
            RigError::Side { track } => write!(f, "track {track}'s side is 0"),
            RigError::NotFinite(what) => write!(f, "{what} is not a finite number"),
            RigError::NotPositive(what) => write!(f, "{what} is not above 0"),
            RigError::NotPermutation(what) => write!(f, "{what} is not a permutation"),
            RigError::NotRotation(what) => write!(f, "{what} has no length to normalise"),
        }
    }
}

impl std::error::Error for RigError {}

impl Rig {
    /// The rig for a calibration under a geometry and a tune (6.2).
    /// `front_track` is the media's (11.1): when it is block 1's track,
    /// both lenses turn 180° about +Y. `sides` are the tracks' sides, by
    /// track. Refuses a θ_max past a lens's `limit_theta`, and numbers it
    /// can't build a rig that weighs anything from: NaN or infinite ones,
    /// a focal, scale, side or θ_max of 0 or below, an Euler convention
    /// outside the space, and a fitted relative rotation of length 0.
    ///
    /// Block b's lens_from_rig is E_b · F · Q_tune_bᵀ · T_b · S as the
    /// module's doc writes it, where Q_tune_b is the tune's yaw, pitch and
    /// roll as `orient::Ypr` reads them (D33) in block b's view frame, so
    /// that a positive yaw turns the lens right as seen through it. Under
    /// [`Extrinsics::Fitted`] both E_b are the identity, and block 1's view
    /// frame is lens 0's carried by the measured rotation: T_1 is F⁻¹ ·
    /// relative · F, the half turn about X or Y and the measured tilt
    /// together, in place of a half turn about [`Geometry::turn`]. So,
    /// untuned, lens_from_rig_1 = relative · lens_from_rig_0, and block 1's
    /// tune turns it in its own view frame whichever way the relative
    /// rotation turns it over, which a half turn about a fixed axis would
    /// not: with the relative turning about X and T_1 about Y, a yaw would
    /// swing lens 1 to its left. The centre c_1 turns with lens 0, its tune
    /// included, as the stored t_1 is in lens 0's frame. Each model is
    /// `Unified` in its track's pixels: the canvas map's focal times the
    /// tune's radius scale, its centre at the block's c_local plus the
    /// tune's shift in canvas pixels, and ξ plus the tune's `xi_delta`.
    pub fn build(
        cal: &Calibration,
        geo: &Geometry,
        tune: &Tune,
        front_track: u8,
        sides: [u32; 2],
    ) -> Result<Rig, RigError> {
        if front_track > 1 {
            return Err(RigError::FrontTrack(front_track));
        }
        let blocks = blocks(cal, geo, tune)?;
        finite("the stitch distance", &[tune.q_per_m])?;
        finite("the feather", &[tune.feather_deg])?;
        for (track, &side) in sides.iter().enumerate() {
            if side == 0 {
                return Err(RigError::Side { track });
            }
        }
        let track = |b: usize| match geo.binding {
            Binding::Direct => b,
            Binding::Reversed => 1 - b,
        };
        let y = if usize::from(front_track) == track(1) {
            DQuat::from_rotation_y(PI)
        } else {
            DQuat::IDENTITY
        };
        let s = geo.upright.about_neg_z() * y;
        // E_b · F, each block's view frame T_b · S, and t_1 as c_1's
        // formula takes it.
        let (ef, view, t_1) = match geo.extrinsics {
            Extrinsics::Stored(conv) => {
                if !all_differ(conv.axes) {
                    return Err(RigError::NotPermutation("the Euler convention's axes"));
                }
                if !all_differ(conv.order) {
                    return Err(RigError::NotPermutation("the Euler convention's order"));
                }
                (
                    [0, 1].map(|b| conv.rotation(blocks[b].ypr_deg) * F),
                    [s, geo.turn.half_turn() * s],
                    DVec3::from(blocks[1].t_m) * geo.tz.sign(),
                )
            }
            Extrinsics::Fitted {
                relative,
                translation_m,
            } => {
                finite(
                    "the fitted extrinsics",
                    &[
                        relative.x,
                        relative.y,
                        relative.z,
                        relative.w,
                        translation_m.x,
                        translation_m.y,
                        translation_m.z,
                    ],
                )?;
                let relative = DVec4::from(relative)
                    .try_normalize()
                    .map(DQuat::from_vec4)
                    .ok_or(RigError::NotRotation("the fitted relative rotation"))?;
                ([F, F], [s, F.inverse() * relative * F * s], translation_m)
            }
        };
        let lens_from_rig: [DQuat; 2] = std::array::from_fn(|b| {
            let [yaw, pitch, roll] = tune.lenses[b].ypr_deg;
            let q_tune = Ypr::from_degrees(yaw, pitch, roll).to_quat();
            (ef[b] * q_tune.conjugate() * view[b]).normalize()
        });
        let c1 = lens_from_rig[0].inverse() * t_1;
        let centres = [-c1 / 2.0, c1 / 2.0];
        let lenses: [RigLens; 2] = std::array::from_fn(|b| {
            let side = sides[track(b)];
            let lens_tune = &tune.lenses[b];
            RigLens::new(
                track(b),
                slot_model(cal, geo, &blocks[b], lens_tune, side),
                lens_from_rig[b],
                centres[b],
                lens_tune.theta_max_deg.to_radians(),
                side,
            )
        });
        for (b, lens) in lenses.iter().enumerate() {
            if lens.theta_max > lens.limit_theta {
                return Err(RigError::PastLimit {
                    block: b,
                    theta_max_deg: tune.lenses[b].theta_max_deg,
                    limit_deg: lens.limit_theta.to_degrees(),
                });
            }
        }
        Ok(Rig {
            lenses,
            q: tune.q_per_m,
            feather: tune.feather_deg.to_radians(),
            margin_texels: 2.0,
        })
    }

    /// The direction in lens `b`'s frame of the scene point 1/q from the
    /// origin along the rig direction `d`: lens_from_rig_b ·
    /// normalize(d − q (c_b − o)).
    pub fn ray(&self, b: usize, d: DVec3) -> DVec3 {
        let lens = &self.lenses[b];
        lens.lens_from_rig * (d.normalize() - self.q * lens.centre_m).normalize()
    }

    /// Where lens `b`'s slot shows the rig direction `d`, in its slot's
    /// pixels: its model's projection of [`Rig::ray`]. `None` where the
    /// model has no image.
    pub fn texel(&self, b: usize, d: DVec3) -> Option<DVec2> {
        self.lenses[b].model.project(self.ray(b, d))
    }

    /// Both lenses' weights for the rig direction `d`, lens b sampled at
    /// LOD `lod[b]` (6.2): w_b = 1 − smoothstep(90° − h, 90° + h, θ_b) on
    /// the parallax-corrected ray, 0 past θ_max − margin(λ_b) or where the
    /// sample's footprint leaves its slot (10.3's frame term), then
    /// normalised. h shrinks, never below 0.5°, where the margin or the
    /// frame leaves less overlap than h. Both are 0 where neither lens
    /// covers `d`.
    pub fn weights(&self, d: DVec3, lod: [f64; 2]) -> [f64; 2] {
        let w = [0, 1].map(|b| {
            if !self.covers(b, d, lod[b]) {
                return 0.0;
            }
            let h = self.feather_at(b, lod[b]);
            1.0 - smoothstep(FRAC_PI_2 - h, FRAC_PI_2 + h, off_axis(self.ray(b, d)))
        });
        let sum = w[0] + w[1];
        if sum > 0.0 {
            w.map(|w| w / sum)
        } else {
            [0.0; 2]
        }
    }

    /// Whether lens `b`'s sample for the rig direction `d` at LOD `lod`
    /// lies inside its mask (6.2, 10.3): θ_b on the parallax-corrected ray
    /// at most θ_max − margin(λ), and the sample's footprint, its texel
    /// with margin(λ) = margin_texels · 2^λ texels about it, inside its
    /// slot. λ is clamped at 0: a negative LOD's footprint is taken as LOD
    /// 0's, which bilinear sampling still reaches. 10.3's text writes the
    /// margin as margin · 2^λ, unclamped; its WGSL must clamp λ as this
    /// does.
    pub fn covers(&self, b: usize, d: DVec3, lod: f64) -> bool {
        let lens = &self.lenses[b];
        let margin = self.margin_px(lod);
        let ray = self.ray(b, d);
        let theta = off_axis(ray);
        // A NaN θ, from a NaN direction, covers nothing.
        if theta.is_nan() || theta > lens.theta_max - margin / lens.texels_per_radian {
            return false;
        }
        let side = f64::from(lens.side);
        lens.model
            .project(ray)
            .is_some_and(|px| px.min_element() >= margin && px.max_element() <= side - margin)
    }

    /// The feather's half width lens `b` blends over at LOD `lod`, in
    /// radians (6.2): the rig's h, shrunk where min(θ_max, θ_frame) less
    /// the margin in angle leaves less than h past 90°, but never below
    /// 0.5° (nor above h, nor below 0: a feather of 0 or below is a hard
    /// switch at 90°). The margin's angle is taken at the least slope
    /// from 90° to θ_max ([`RigLens::band_texels_per_radian`]), so that
    /// the band ends inside the mask whether the slope falls towards
    /// θ_max or rises: on a rising one, with θ_frame the bound, the slope
    /// at θ_max would put the margin's angle short of the frame's cut.
    pub fn feather_at(&self, b: usize, lod: f64) -> f64 {
        let lens = &self.lenses[b];
        let margin = self.margin_px(lod) / lens.band_texels_per_radian;
        let room = lens.theta_max.min(lens.theta_frame) - margin - FRAC_PI_2;
        self.feather.min(room.max(MIN_FEATHER)).max(0.0)
    }

    /// The rig-frame unit direction lens `b` sees at pixel `px` of its
    /// slot, from its own centre: at q = 0 the inverse of [`Rig::texel`].
    /// `None` past the model's `limit_theta`, as `Unified::unproject` is.
    pub fn unproject(&self, b: usize, px: DVec2) -> Option<DVec3> {
        let lens = &self.lenses[b];
        let d = lens.model.unproject_within(px, lens.limit_theta)?;
        Some(lens.lens_from_rig.inverse() * d)
    }

    /// margin(λ) in texels: margin_texels · 2^λ, with λ clamped at 0, so
    /// that a negative LOD's footprint is LOD 0's. 10.3's text doesn't
    /// clamp λ; its WGSL must, as this does.
    fn margin_px(&self, lod: f64) -> f64 {
        self.margin_texels * lod.max(0.0).exp2()
    }
}

/// Each block's θ_frame (6.1), in radians, by block: the smallest θ at
/// which the lens's circle meets the edge of its square slot, found by
/// Brent's search, golden sections with parabolic steps, along each edge
/// through the inverse. Every
/// canvas map scales with the side, so it needs no slot size, and
/// `validate` (11.2) can bound the feather with it from the project
/// alone.
pub fn theta_frame(cal: &Calibration, geo: &Geometry, tune: &Tune) -> Result<[f64; 2], RigError> {
    let blocks = blocks(cal, geo, tune)?;
    // Any side gives the same angles; the canvas's own square keeps the
    // pixels the calibration's.
    Ok([0, 1].map(|b| {
        let model = slot_model(cal, geo, &blocks[b], &tune.lenses[b], cal.src);
        frame_theta(&model, model.limit_theta(), f64::from(cal.src))
    }))
}

/// The blocks of the generation `geo` reads, checked: there, of that
/// generation, read through one of the 21 V6 readings, finite, and with
/// focals above 0, with the tune's numbers for each lens (its radius
/// scale and θ_max above 0), the canvas map's scale and src and dst.
fn blocks(cal: &Calibration, geo: &Geometry, tune: &Tune) -> Result<[LensCalib; 2], RigError> {
    if cal.src == 0 {
        return Err(RigError::NotPositive("the calibration's src"));
    }
    if cal.dst == 0 {
        return Err(RigError::NotPositive("the calibration's dst"));
    }
    let blocks = match geo.generation {
        Generation::V3 => cal.v3,
        Generation::V6 => cal.v6,
    }
    .ok_or(RigError::NoGeneration(geo.generation))?;
    if geo.generation == Generation::V6 && !geo.v6.is_valid() {
        return Err(RigError::Reading(geo.v6));
    }
    const BLOCK: [&str; 2] = ["block 0's calibration", "block 1's calibration"];
    const TUNE: [&str; 2] = ["lens 0's tune", "lens 1's tune"];
    const FX: [&str; 2] = ["block 0's fx", "block 1's fx"];
    const FY: [&str; 2] = ["block 0's fy", "block 1's fy"];
    const RADIUS: [&str; 2] = ["lens 0's radius scale", "lens 1's radius scale"];
    const THETA_MAX: [&str; 2] = ["lens 0's θ_max", "lens 1's θ_max"];
    for (b, block) in blocks.iter().enumerate() {
        if block.terms.generation() != geo.generation {
            return Err(RigError::Terms {
                generation: geo.generation,
                block: b,
            });
        }
        let terms: &[f64] = match &block.terms {
            CalibTerms::V3 { k, p } => &[k[0], k[1], k[2], p[0], p[1]],
            CalibTerms::V6 { d } => d,
        };
        finite(BLOCK[b], terms)?;
        finite(
            BLOCK[b],
            &[block.xi, block.fx, block.fy, block.cx, block.cy],
        )?;
        finite(BLOCK[b], &block.ypr_deg)?;
        finite(BLOCK[b], &block.t_m)?;
        let t = &tune.lenses[b];
        finite(TUNE[b], &t.ypr_deg)?;
        finite(TUNE[b], &t.centre_px)?;
        finite(TUNE[b], &[t.radius_scale, t.xi_delta, t.theta_max_deg])?;
        positive(FX[b], block.fx)?;
        positive(FY[b], block.fy)?;
        positive(RADIUS[b], t.radius_scale)?;
        positive(THETA_MAX[b], t.theta_max_deg)?;
    }
    if let CanvasMap::Fitted { scale } = geo.canvas {
        finite("the canvas map's scale", &[scale])?;
        positive("the canvas map's scale", scale)?;
    }
    Ok(blocks)
}

/// `Err(NotFinite(what))` if any of `values` is NaN or infinite.
fn finite(what: &'static str, values: &[f64]) -> Result<(), RigError> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(RigError::NotFinite(what))
    }
}

/// `Err(NotPositive(what))` unless `value` is above 0.
fn positive(what: &'static str, value: f64) -> Result<(), RigError> {
    if value > 0.0 {
        Ok(())
    } else {
        Err(RigError::NotPositive(what))
    }
}

/// Whether the three are all different: a permutation of three values.
fn all_differ<T: PartialEq>([a, b, c]: [T; 3]) -> bool {
    a != b && b != c && a != c
}

/// Block `block`'s model in the pixels of a square track of side `side`
/// under the geometry's canvas map, generation and V6 reading, with the
/// lens's tune folded in.
fn slot_model(
    cal: &Calibration,
    geo: &Geometry,
    block: &LensCalib,
    tune: &LensTune,
    side: u32,
) -> LensModel {
    let focal = geo.canvas.focal_scale(cal.src, cal.dst, side) * tune.radius_scale;
    let local = DVec2::new(block.cx, block.cy) + DVec2::from(tune.centre_px);
    let centre = geo.canvas.to_slot(local, cal.src, cal.dst, side);
    let dist = match block.terms {
        CalibTerms::V3 { k, p } => Canonical::v3(k, p),
        CalibTerms::V6 { d } => Canonical::v6(d, geo.v6),
    };
    LensModel::Mei(Unified {
        xi: block.xi + tune.xi_delta,
        fx: block.fx * focal,
        fy: block.fy * focal,
        cx: centre.x,
        cy: centre.y,
        dist,
    })
}

/// θ of a lens-frame direction, off the +z axis.
fn off_axis(d: DVec3) -> f64 {
    d.x.hypot(d.y).atan2(d.z)
}

/// 1 − w's ramp: 0 below `lo`, 1 above `hi`, Hermite's smooth step
/// between. With `hi` ≤ `lo` it is a step at `lo`, ½ at `lo` itself:
/// [`Rig::feather_at`] gives no h below 0, so a feather of 0 or below is a
/// hard switch at 90° that gives each lens ½ on the edge. 10.3's WGSL must
/// take that case apart, since WGSL's `smoothstep` is undefined when its
/// edges meet.
fn smoothstep(lo: f64, hi: f64, x: f64) -> f64 {
    if hi <= lo {
        return match x.partial_cmp(&lo) {
            Some(std::cmp::Ordering::Less) => 0.0,
            Some(std::cmp::Ordering::Equal) => 0.5,
            _ => 1.0,
        };
    }
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The least over the azimuth of how far, in pixels, the image moves per
/// radian of θ, at each of `thetas`: one pass over the azimuths, each
/// angle's sine and cosine found once.
fn texels_per_radian<const N: usize>(model: &LensModel, thetas: [f64; N]) -> [f64; N] {
    let at = thetas.map(f64::sin_cos);
    let before = thetas.map(|theta| (theta - SLOPE_STEP).sin_cos());
    let mut least = [f64::INFINITY; N];
    for j in 0..AZIMUTHS {
        let (sin_phi, cos_phi) = (f64::from(j) * 2.0 * PI / f64::from(AZIMUTHS)).sin_cos();
        let pixel =
            |(st, ct): (f64, f64)| model.project(DVec3::new(st * cos_phi, st * sin_phi, ct));
        for (i, least) in least.iter_mut().enumerate() {
            if let (Some(a), Some(b)) = (pixel(at[i]), pixel(before[i])) {
                *least = least.min((a - b).length() / SLOPE_STEP);
            }
        }
    }
    least
}

/// θ_frame of a model on a square slot of side `side`, its inverse
/// stopping at `limit`: the least θ over the slot's four edges. Along an
/// edge θ has one minimum, near the foot of the perpendicular from the
/// principal point (exactly there for a radial lens), so a search over a
/// sixteenth of the side either way of the foot finds it, and over the
/// whole edge should it land on that bracket's end. An edge pixel the
/// inverse finds no direction for lies past the limit, and counts as the
/// limit; a circle that meets no edge gives the limit.
///
/// Past the limit the search sees the limit plus a millionth of the
/// pixel's distance from the principal point in sides, which the result
/// never keeps: a slope toward the foot, so that on an edge the circle
/// barely meets, where θ is the limit but for a few pixels about the foot,
/// the search still walks to them rather than settling on the flat.
fn frame_theta(model: &LensModel, limit: f64, side: f64) -> f64 {
    let (cx, cy) = match model {
        LensModel::Mei(u) => (u.cx, u.cy),
        LensModel::Kb(k) => (k.cx, k.cy),
    };
    let centre = DVec2::new(cx, cy);
    let theta = |px: DVec2| match model.unproject_within(px, limit) {
        Some(d) => off_axis(d),
        None => limit + 1e-6 * (px - centre).length() / side,
    };
    let tol = side * FRAME_TOL;
    let edge = |foot: f64, at: &dyn Fn(f64) -> DVec2| {
        let f = |t: f64| theta(at(t));
        let lo = (foot - side / 16.0).clamp(0.0, side);
        let hi = (foot + side / 16.0).clamp(0.0, side);
        let (t, min) = golden_min(f, lo, hi, tol);
        let at_end = (t - lo < 3.0 * tol && lo > 0.0) || (hi - t < 3.0 * tol && hi < side);
        if at_end {
            golden_min(f, 0.0, side, tol).1
        } else {
            min
        }
    };
    [
        edge(cx, &|t| DVec2::new(t, 0.0)),
        edge(cx, &|t| DVec2::new(t, side)),
        edge(cy, &|t| DVec2::new(0.0, t)),
        edge(cy, &|t| DVec2::new(side, t)),
    ]
    .into_iter()
    .fold(limit, f64::min)
}

/// How closely [`frame_theta`]'s searches place the minimum along an
/// edge, as a fraction of the side. Near the minimum θ is quadratic
/// along the edge, its second derivative under 10/s² rad a pixel² on
/// Pat's lenses under A or C. The search stops with its best point within
/// twice this of the minimum, 2e-5 s, so θ is off by under 2e-9 rad. A
/// finer place would gain nothing: 1e-6 s moves θ by under 5e-12 rad, a
/// tenth of the inverse's own angle error.
const FRAME_TOL: f64 = 1e-5;

/// Where on [lo, hi] `f` takes its least value, where it has one
/// minimum, and that value: Brent's `localmin` (R. P. Brent, *Algorithms
/// for Minimization without Derivatives*, Prentice-Hall 1973, chapter 5),
/// written from the book's description with an absolute tolerance alone,
/// since `tol` is far above the relative one Brent adds. Each step fits a
/// parabola through the three best points so far, x, w and v, and takes
/// its vertex when that lies inside the bracket and moves less than half
/// the step before last; otherwise it takes a golden section of the
/// larger side of x. So a minimum as smooth as θ along an edge takes a
/// handful of steps, not forty. It stops once x lies within 2 `tol` of
/// both ends of the bracket about it.
fn golden_min(f: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64, tol: f64) -> (f64, f64) {
    // (3 − √5)/2, the share of a side a golden section takes.
    const GOLD: f64 = 0.381_966_011_250_105_2;
    // x is the best point so far, w the second best, and v the best
    // before w.
    let mut x = lo + GOLD * (hi - lo);
    let mut fx = f(x);
    let (mut w, mut fw, mut v, mut fv) = (x, fx, x, fx);
    // d is the step just taken, and e the one before it.
    let (mut d, mut e) = (0.0f64, 0.0f64);
    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if (x - mid).abs() <= 2.0 * tol - 0.5 * (hi - lo) {
            break;
        }
        // The parabola through x, w and v puts its vertex at x + p/q,
        // with q ≥ 0; r keeps the step before last. With no step before
        // last to compare against, p, q and r stay 0 and the golden
        // section is taken.
        let (mut p, mut q, mut r) = (0.0f64, 0.0f64, 0.0f64);
        if e.abs() > tol {
            r = (x - w) * (fx - fv);
            q = (x - v) * (fx - fw);
            p = (x - v) * q - (x - w) * r;
            q = 2.0 * (q - r);
            if q > 0.0 {
                p = -p;
            } else {
                q = -q;
            }
            r = e;
            e = d;
        }
        if p.abs() < (0.5 * q * r).abs() && p > q * (lo - x) && p < q * (hi - x) {
            // The parabola's step, kept 2 tol inside the bracket.
            d = p / q;
            let u = x + d;
            if u - lo < 2.0 * tol || hi - u < 2.0 * tol {
                d = if x < mid { tol } else { -tol };
            }
        } else {
            // A golden section of the larger side.
            e = if x < mid { hi - x } else { lo - x };
            d = GOLD * e;
        }
        // f is never sampled within tol of x.
        let u = if d.abs() >= tol {
            x + d
        } else if d > 0.0 {
            x + tol
        } else {
            x - tol
        };
        let fu = f(u);
        // The bracket shrinks to the side of u or x that keeps the
        // least, and x, w and v move up.
        if fu <= fx {
            if u < x {
                hi = x;
            } else {
                lo = x;
            }
            (v, fv, w, fw, x, fx) = (w, fw, x, fx, u, fu);
        } else {
            if u < x {
                lo = u;
            } else {
                hi = u;
            }
            if fu <= fw || w == x {
                (v, fv, w, fw) = (w, fw, u, fu);
            } else if fu <= fv || v == x || v == w {
                (v, fv) = (u, fu);
            }
        }
    }
    (x, fx)
}
