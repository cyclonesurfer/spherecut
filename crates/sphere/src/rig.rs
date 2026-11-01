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
//!
//! Each convention is a small generic space ([`EulerConv`], 1152 values;
//! `fisheye::V6Reading`, 21), so that no reader's choice is copied or
//! favoured, and settling one changes a table entry, never the shader.
//! These types share no code with the synthetic camera's
//! (`spherecut_synth::lens`), which composes the same rig from the plan's
//! text with its own code to check this one (4.1 rule 2).

use std::fmt;

use glam::{DQuat, DVec2, DVec3};

use crate::fisheye::{LensModel, V6Reading};

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
    Fitted {
        relative: DQuat,
        translation_m: DVec3,
    },
}

/// The axis of the 180° turn T_1 from the rig frame to block 1's view
/// frame, which the strings omit (insta360.md §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnAxis {
    Y,
    X,
}

impl TurnAxis {
    pub fn all() -> impl Iterator<Item = TurnAxis> {
        [TurnAxis::Y, TurnAxis::X].into_iter()
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
    /// rotation from the view frame after F to the lens frame.
    #[expect(unused_variables, reason = "M3.3 unit (b) writes the body")]
    pub fn rotation(&self, ypr_deg: [f64; 3]) -> DQuat {
        todo!()
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
    /// Pat's V6).
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
    /// margin_texels · 2^λ (2^(λ+1) texels, 6.2, so 2).
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
}

impl fmt::Display for RigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RigError::NoGeneration(g) => write!(f, "the calibration has no {g:?} blocks"),
            RigError::Terms { generation, block } => {
                write!(f, "block {block}'s terms are not {generation:?}'s")
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
        }
    }
}

impl std::error::Error for RigError {}

impl Rig {
    /// The rig for a calibration under a geometry and a tune (6.2).
    /// `front_track` is the media's (11.1): when it is block 1's track,
    /// both lenses turn 180° about +Y. `sides` are the tracks' sides, by
    /// track. Refuses a θ_max past a lens's `limit_theta`.
    #[expect(unused_variables, reason = "M3.3 unit (b) writes the body")]
    pub fn build(
        cal: &Calibration,
        geo: &Geometry,
        tune: &Tune,
        front_track: u8,
        sides: [u32; 2],
    ) -> Result<Rig, RigError> {
        todo!()
    }

    /// The direction in lens `b`'s frame of the scene point 1/q from the
    /// origin along the rig direction `d`: lens_from_rig_b ·
    /// normalize(d − q (c_b − o)).
    #[expect(unused_variables, reason = "M3.3 unit (b) writes the body")]
    pub fn ray(&self, b: usize, d: DVec3) -> DVec3 {
        todo!()
    }

    /// Where lens `b`'s slot shows the rig direction `d`, in its slot's
    /// pixels: its model's projection of [`Rig::ray`]. `None` where the
    /// model has no image.
    #[expect(unused_variables, reason = "M3.3 unit (b) writes the body")]
    pub fn texel(&self, b: usize, d: DVec3) -> Option<DVec2> {
        todo!()
    }

    /// Both lenses' weights for the rig direction `d`, lens b sampled at
    /// LOD `lod[b]` (6.2): w_b = 1 − smoothstep(90° − h, 90° + h, θ_b) on
    /// the parallax-corrected ray, 0 past θ_max − margin(λ_b) or where the
    /// sample's footprint leaves its slot (10.3's frame term), then
    /// normalised. h shrinks, never below 0.5°, where the margin or the
    /// frame leaves less overlap than h. Both are 0 where neither lens
    /// covers `d`.
    #[expect(unused_variables, reason = "M3.3 unit (b) writes the body")]
    pub fn weights(&self, d: DVec3, lod: [f64; 2]) -> [f64; 2] {
        todo!()
    }

    /// The rig-frame unit direction lens `b` sees at pixel `px` of its
    /// slot, from its own centre: at q = 0 the inverse of [`Rig::texel`].
    #[expect(unused_variables, reason = "M3.3 unit (b) writes the body")]
    pub fn unproject(&self, b: usize, px: DVec2) -> Option<DVec3> {
        todo!()
    }
}

/// Each block's θ_frame (6.1), in radians, by block: the smallest θ at
/// which the lens's circle meets the edge of its square slot, found by a
/// golden-section search along each edge through the inverse. Every
/// canvas map scales with the side, so it needs no slot size, and
/// `validate` (11.2) can bound the feather with it from the project
/// alone.
#[expect(unused_variables, reason = "M3.3 unit (b) writes the body")]
pub fn theta_frame(cal: &Calibration, geo: &Geometry, tune: &Tune) -> Result<[f64; 2], RigError> {
    todo!()
}
