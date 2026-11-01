//! The synthetic camera's lens truth (M3 plan 7.1): the conventions synth
//! plants in a rig, and the calibration numbers it starts from.
//!
//! These are synth's own types, written from the plan's text (6.1 and
//! 6.2), and share no code with `spherecut_sphere`'s lens and rig, which
//! they are there to check (4.1 rule 2): a test converts between the two
//! only at its boundary. [`Planted`] holds one value of each convention
//! the camera's files leave open, and [`CalibTruth`] one calibration's
//! stored numbers. M3.2 prints the calibration strings from the two, and
//! M3.3 composes each lens's model, `to_rig` and centre from them.
//!
//! The frames are 6.2's. A lens frame is Mei's and OpenCV's: +x right in
//! the image, +y down, +z along the optical axis. The rig frame looks
//! along −Z. A rotation by a positive angle about an axis turns by the
//! right-hand rule.

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
    /// roll).
    pub signs: [Sign; 3],
    /// A quarter turn added to the stored roll before its sign is applied.
    /// Upright X5 video stores a roll of about 90° (3.1).
    pub roll_offset: QuarterTurn,
}

impl TruthEuler {
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
/// (sign(tz) · t_1), lens 1's centre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TruthTz {
    /// As stored: lens 1 sits behind lens 0 on lens 0's axis, the
    /// physical layout.
    AsStored,
    /// Negated.
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

/// A block's distortion terms as its generation stores them, in token
/// order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StoredDist {
    /// V3: radial k1, k2 and k3, then Brown's tangential p1 and p2.
    V3 { k: [f64; 3], p: [f64; 2] },
    /// V6: d0 to d12, read through a [`TruthV6`].
    V6 { d: [f64; 13] },
}

impl StoredDist {
    /// The generation these terms belong to.
    pub fn generation(&self) -> Generation {
        match self {
            StoredDist::V3 { .. } => Generation::V3,
            StoredDist::V6 { .. } => Generation::V6,
        }
    }
}

/// One calibration block's stored numbers, in the calibration canvas's
/// pixels, degrees and metres, as the string holds them.
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
    pub dist: StoredDist,
}

/// One camera calibration's stored numbers, a calibration string's worth
/// (insta360.md §4.1, M3 plan 3.1 and 6.1): two blocks of one generation,
/// the canvas they are in and field 27's window. Block 0 is the canvas's
/// left; which track each block describes is the planted binding's.
///
/// The type doesn't make the two blocks share a generation:
/// [`CalibTruth::generation`] says whether they do. The tracks' side is
/// not here either, since it is the recording mode's and not the
/// calibration's: whatever scales these numbers to a track takes it
/// beside them, as `Rig::build` (6.2) takes its `sides`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CalibTruth {
    pub blocks: [BlockTruth; 2],
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
    /// The generation both blocks store their terms in, or `None` when
    /// they differ, which no calibration string holds.
    pub fn generation(&self) -> Option<Generation> {
        let [b0, b1] = self.blocks.map(|b| b.dist.generation());
        (b0 == b1).then_some(b0)
    }
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

    #[test]
    fn stored_terms_name_their_generation() {
        // Lens 0 of Pat's V3 string (field 54, the same in every master
        // and LRV, plan 3.1).
        let v3 = StoredDist::V3 {
            k: [0.18632051, 2.04984355, -3.24446917],
            p: [-0.00143576, -0.00339240],
        };
        let block = BlockTruth {
            xi: 2.0,
            fx: 4284.62,
            fy: 4284.28,
            cx: 2701.13,
            cy: 2692.13,
            ypr_deg: [-0.198, 0.211, 90.28],
            t_m: [0.0; 3],
            dist: v3,
        };
        let cal = CalibTruth {
            blocks: [block; 2],
            canvas: [10752, 5376],
            dst: 5312,
            lens_type: 113,
            body: 4,
        };
        assert_eq!(cal.blocks[1].dist.generation(), Generation::V3);
        assert_eq!(cal.generation(), Some(Generation::V3));
        let v6 = StoredDist::V6 { d: [0.0; 13] };
        assert_eq!(v6.generation(), Generation::V6);
        // Control: V6's terms aren't taken for V3's.
        must_fail("V6 terms as V3", v6.generation() == Generation::V3);
        // Control: a calibration with a block of each generation has none.
        let mixed = CalibTruth {
            blocks: [block, BlockTruth { dist: v6, ..block }],
            ..cal
        };
        must_fail("a V3 block beside a V6 one", mixed.generation().is_some());
    }
}
