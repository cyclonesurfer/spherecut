//! The attribution matrix (M3 plan 8.3), lifted from
//! `tests/instrument.rs` (M3.7) for the dual-fisheye goldens' seam rows:
//! a planted error ([`Plant`]) in the model that unwraps one rendered
//! pair, with the cameras and the wrong models it plants; the plant's seam
//! ([`Seam`]: what the model predicts, and what the strips measure),
//! measured once a binary ([`Plant::seam`]); 4.2's checks on a true
//! model's seam ([`hold_seam`]); how far apart a model puts what its two
//! lenses show on the seam, with no images ([`apart_deg`]); and 8.3's step
//! 3 ([`Lands`], [`Misfit`], [`attribute_to`]). `tests/instrument.rs` runs
//! the matrix, and keeps the bounds its tests alone hold (step 2's, the
//! swapped tracks' bins, the apart angle's).
//!
//! The camera, its far tune and its frames are `synth::rung`'s, and the
//! X5-like rigs, the Kannala–Brandt camera and the stitches are
//! [`super::rung1`]'s. A plant's seam is measured on the binary's own
//! renders: `RenderCache::shared()` is one process's, so each binary that
//! takes these renders its own pairs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use glam::DVec3;
use spherecut_render::seam::shifts::TERMS;
use spherecut_render::seam::{BinShift, Signature, StripSpec, bin_shifts, signature, strips_of};
use spherecut_sphere::fisheye::{self, KbLens, LensModel};
use spherecut_sphere::rig::{Rig, RigLens};
use spherecut_synth::Pattern;
use spherecut_synth::lens::{
    CalibTruth, Generation, IMAGE_CIRCLE_DEG, Motion, PairOrder, Planted, Prism, PrismAt,
    PrismOrder, RigTruth, Sign, Tangential, TruthBinding, TruthCanvas, TruthEuler, TruthTz,
    TruthV6, Turn, compose, seen, x5_like_v6,
};
use spherecut_synth::rung::{FRONT_TRACK, LensFrame, SIDES, far, pair_through, track_through};

use super::budget::{NAMES, terms, worse};
use super::predict::predict_bins;
use super::rig::pats;
use super::rung1::{
    Pair, both, grid, kb, lines, near, near_lines, ready, rig_as, tuned, turned, with_lens, x5,
};

// The cameras a plant renders, and the strips it measures on.

/// Tier 1's strips: 1372 × 61, 72 bins of 5°, a search of ±3°.
pub const SPEC: StripSpec = StripSpec::TIER1;

/// The camera with lens 1 a frame late while the rig turns 10° a second
/// about `axis` (4.2, 3.4). Track 0's frame 0 is exposed as the turn
/// begins, so it is the still camera's.
pub fn late(axis: DVec3) -> RigTruth {
    RigTruth {
        late_frames: [0.0, 1.0],
        motion: Motion {
            turns: vec![Turn {
                start_s: 0.0,
                end_s: 100.0,
                axis,
                deg_per_s: 10.0,
            }],
            ..Motion::still()
        },
        ..x5().truth.clone()
    }
}

pub fn late_in_a_pan() -> &'static LensFrame {
    static FRAME: OnceLock<LensFrame> = OnceLock::new();
    FRAME.get_or_init(|| track_through(Pattern::SeamLines, &late(DVec3::Y), 1))
}

pub fn late_in_a_roll() -> &'static LensFrame {
    static FRAME: OnceLock<LensFrame> = OnceLock::new();
    FRAME.get_or_init(|| track_through(Pattern::SeamLines, &late(DVec3::Z), 1))
}

/// The camera with lens 1 at +0.5 EV and +5 % red (4.2).
pub fn gained() -> RigTruth {
    let ev = 2f64.powf(0.5);
    let mut truth = x5().truth.clone();
    truth.lenses[1].gain = [1.05 * ev, ev, ev];
    truth
}

pub fn gained_lens() -> &'static LensFrame {
    static FRAME: OnceLock<LensFrame> = OnceLock::new();
    FRAME.get_or_init(|| track_through(Pattern::SeamLines, &gained(), 1))
}

/// A V6 reading with both of its parts.
pub fn reading(pair: PairOrder, extension: bool, at: PrismAt, order: PrismOrder) -> TruthV6 {
    TruthV6 {
        tangential: Some(Tangential { pair, extension }),
        prism: Some(Prism { at, order }),
    }
}

/// The reading the V6 camera's pictures are rendered under (4.2's "wrong
/// V6 reading" reads them under another).
pub fn rendered_reading() -> TruthV6 {
    reading(PairOrder::D5D6, true, PrismAt::D9, PrismOrder::ByAxis)
}

/// The X5-like camera as a V6 calibration (`x5_like_v6`), seeing
/// `SeamLines` infinitely far through [`rendered_reading`].
pub struct V6 {
    pub cal: CalibTruth,
    pub truth: RigTruth,
    pub frames: [LensFrame; 2],
}

pub fn v6() -> &'static V6 {
    static V6: OnceLock<V6> = OnceLock::new();
    V6.get_or_init(|| {
        let cal = x5_like_v6();
        let planted = Planted {
            v6: rendered_reading(),
            ..x5().planted
        };
        let truth = compose(&cal, &planted, FRONT_TRACK, SIDES);
        V6 {
            frames: pair_through(Pattern::SeamLines, &truth),
            truth,
            cal,
        }
    })
}

/// sphere's rig of the V6 camera, its terms read as `reading`.
pub fn v6_rig(reading: TruthV6) -> Rig {
    let planted = Planted {
        v6: reading,
        ..x5().planted
    };
    rig_as(&v6().cal, &planted, &far())
}

/// Pat's V3 blocks at 768² under the X5-like camera's conventions,
/// seeing `SeamLines` infinitely far, and sphere's rig of them and of
/// Pat's V6 blocks: 3.4's "V3 against V6".
pub struct Pats {
    pub truth: RigTruth,
    pub frames: [LensFrame; 2],
    pub as_v3: Rig,
    pub as_v6: Rig,
}

pub fn pats_v3() -> &'static Pats {
    static PATS: OnceLock<Pats> = OnceLock::new();
    PATS.get_or_init(|| {
        let planted = &x5().planted;
        let (v3, v6) = (pats(Generation::V3), pats(Generation::V6));
        let truth = compose(&v3, planted, FRONT_TRACK, SIDES);
        Pats {
            frames: pair_through(Pattern::SeamLines, &truth),
            truth,
            as_v3: rig_as(&v3, planted, &far()),
            as_v6: rig_as(&v6, planted, &far()),
        }
    })
}

// Models that are wrong on purpose.

/// The X5-like rig with both of its lenses turned `deg` about +Y: a slip
/// the seam can't see. 4.2's common-mode control turns them 0.5°.
pub fn both_turned(deg: f64) -> Rig {
    let rig = x5().rig;
    Rig {
        lenses: rig.lenses.map(|lens| turned(&lens, DVec3::Y, deg)),
        ..rig
    }
}

/// `lens` with another model in its place.
pub fn with_model(lens: &RigLens, model: LensModel) -> RigLens {
    RigLens::new(
        lens.track,
        model,
        lens.lens_from_rig,
        lens.centre_m,
        lens.theta_max,
        lens.side,
    )
}

/// An equidistant lens with no distortion in a Mei lens's place: the
/// same centre, and one focal set so that at the image circle it reaches
/// as far along +x as the lens it replaces, which is how `kb_like` sets
/// its own. 4.2's "equidistant applied to Mei data".
pub fn equidistant_for(lens: &RigLens) -> RigLens {
    let LensModel::Mei(mei) = lens.model else {
        panic!("a Mei lens");
    };
    let theta = IMAGE_CIRCLE_DEG.to_radians();
    let edge = DVec3::new(theta.sin(), 0.0, theta.cos());
    let f = (lens.model.project(edge).unwrap().x - mei.cx) / theta;
    let model = LensModel::Kb(KbLens {
        base: fisheye::Model::Equidistant,
        k: [0.0; 5],
        fx: f,
        fy: f,
        cx: mei.cx,
        cy: mei.cy,
        p: [0.0; 2],
        pe: [0.0; 2],
        s: [0.0; 4],
    });
    with_model(lens, model)
}

/// A Kannala–Brandt lens read on the equisolid base: the KB rig's
/// control.
pub fn equisolid_for(lens: &RigLens) -> RigLens {
    let LensModel::Kb(kb) = lens.model else {
        panic!("a Kannala–Brandt lens");
    };
    let model = LensModel::Kb(KbLens {
        base: fisheye::Model::Equisolid,
        ..kb
    });
    with_model(lens, model)
}

/// The X5-like rig with the stored angle `angle` (yaw, pitch, roll) read
/// with the wrong sign: 3.4's Euler class.
pub fn wrong_sign(angle: usize) -> Rig {
    let mut signs = [Sign::Plus; 3];
    signs[angle] = Sign::Minus;
    let euler = TruthEuler {
        signs,
        ..TruthEuler::PLACEHOLDER
    };
    let planted = Planted {
        euler,
        ..x5().planted
    };
    rig_as(&x5().cal, &planted, &far())
}

/// A planted error (4.2's controls, 3.4's rows): the camera, its frames,
/// and the model that unwraps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Plant {
    /// Nothing: the model is the truth's.
    Truth,
    /// Lens 1 yawed, pitched and rolled 0.5° by the tune.
    Yaw,
    Pitch,
    Roll,
    /// Lens 1 turned 0.13° about +Y, 0.5 px across the side seams.
    SmallTurn,
    /// Both lenses, and lens 0 alone, under map A: f × 5376/5312 (Q4).
    MapA,
    OneLensA,
    /// Lens 1's centre 2 px along x.
    Centre,
    /// Equidistant lenses applied to Mei's pictures.
    Equidistant,
    /// The binding reversed: each block reads the other's track.
    Swapped,
    /// Lens 1 without its half turn.
    NoTurn,
    /// The scene 1.5 m away: stitched there, stitched infinitely far, and
    /// stitched there with tz's sign flipped.
    Near,
    NearAtInfinity,
    TzFlipped,
    /// Lens 1 a frame late while the rig turns 10° a second about +Y, and
    /// about the lens axis.
    LatePan,
    LateRoll,
    /// Lens 1 at +0.5 EV and +5 % red, unwrapped by the truth's model.
    Gains,
    /// Both lenses turned 0.5° about +Y.
    CommonMode,
    /// Track 1 showing `Grid` where track 0 shows `SeamLines`.
    OtherScene,
    /// The Euler class: lens 0's yaw, both pitches and both yaws read
    /// with the wrong sign.
    EulerYaw0,
    EulerPitches,
    EulerYaws,
    /// The V6 camera read as it was rendered; as `RadialOnly` on both
    /// lenses and on one; and under 6.1's two other readings.
    V6,
    V6Radial,
    V6Radial0,
    V6Radial1,
    V6Interleaved,
    V6D7,
    /// Pat's V3 pictures with one lens's model, and one lens's angles,
    /// from Pat's V6 blocks.
    PatsModel0,
    PatsModel1,
    PatsAngles0,
    PatsAngles1,
    /// The Kannala–Brandt rig, and its lenses read as equisolid.
    Kb,
    KbEquisolid,
}

/// What a [`Plant`] plants.
pub struct Setup {
    pub truth: RigTruth,
    pub frames: Pair,
    pub rig: Rig,
}

impl Plant {
    pub fn setup(self) -> Setup {
        let x5 = x5();
        let far = |rig: Rig| Setup {
            truth: x5.truth.clone(),
            frames: both(lines()),
            rig,
        };
        let near = |rig: Rig| Setup {
            truth: x5.near.clone(),
            frames: both(near_lines()),
            rig,
        };
        let lens_1 = |frame: &'static LensFrame, truth: RigTruth| Setup {
            truth,
            frames: [&lines()[0], frame],
            rig: x5.rig,
        };
        let read_v6 = |rig: Rig| Setup {
            truth: v6().truth.clone(),
            frames: both(&v6().frames),
            rig,
        };
        let pats = |b: usize, part: fn(&RigLens, &RigLens) -> RigLens| {
            let p = pats_v3();
            Setup {
                truth: p.truth.clone(),
                frames: both(&p.frames),
                rig: with_lens(&p.as_v3, b, part(&p.as_v3.lenses[b], &p.as_v6.lenses[b])),
            }
        };
        let model_of: fn(&RigLens, &RigLens) -> RigLens = |lens, from| with_model(lens, from.model);
        let angles_of: fn(&RigLens, &RigLens) -> RigLens = |lens, from| RigLens {
            lens_from_rig: from.lens_from_rig,
            ..*lens
        };
        let under_a = || {
            let planted = Planted {
                canvas: TruthCanvas::A,
                ..x5.planted
            };
            rig_as(&x5.cal, &planted, &self::far())
        };
        match self {
            Plant::Truth => far(x5.rig),
            Plant::Yaw => far(tuned(|t| t.lenses[1].ypr_deg[0] = 0.5)),
            Plant::Pitch => far(tuned(|t| t.lenses[1].ypr_deg[1] = 0.5)),
            Plant::Roll => far(tuned(|t| t.lenses[1].ypr_deg[2] = 0.5)),
            Plant::SmallTurn => far(with_lens(
                &x5.rig,
                1,
                turned(&x5.rig.lenses[1], DVec3::Y, 0.13),
            )),
            Plant::MapA => far(under_a()),
            Plant::OneLensA => far(with_lens(&x5.rig, 0, under_a().lenses[0])),
            // The tune's shift is in canvas pixels, 5376/768 to a pixel
            // of the track.
            Plant::Centre => far(tuned(|t| t.lenses[1].centre_px = [14.0, 0.0])),
            Plant::Equidistant => far(Rig {
                lenses: [
                    equidistant_for(&x5.rig.lenses[0]),
                    equidistant_for(&x5.rig.lenses[1]),
                ],
                ..x5.rig
            }),
            Plant::Swapped => {
                let planted = Planted {
                    binding: TruthBinding::Reversed,
                    ..x5.planted
                };
                far(rig_as(&x5.cal, &planted, &self::far()))
            }
            Plant::NoTurn => far(with_lens(
                &x5.rig,
                1,
                turned(&x5.rig.lenses[1], DVec3::Y, 180.0),
            )),
            Plant::Near => near(x5.near_rig),
            Plant::NearAtInfinity => near(x5.rig),
            Plant::TzFlipped => {
                let planted = Planted {
                    tz: TruthTz::Negated,
                    ..x5.planted
                };
                near(rig_as(&x5.cal, &planted, &self::near()))
            }
            Plant::LatePan => lens_1(late_in_a_pan(), late(DVec3::Y)),
            Plant::LateRoll => lens_1(late_in_a_roll(), late(DVec3::Z)),
            Plant::Gains => lens_1(gained_lens(), gained()),
            Plant::CommonMode => far(both_turned(0.5)),
            Plant::OtherScene => lens_1(&grid()[1], x5.truth.clone()),
            Plant::EulerYaw0 => far(with_lens(&x5.rig, 0, wrong_sign(0).lenses[0])),
            Plant::EulerPitches => far(wrong_sign(1)),
            Plant::EulerYaws => far(wrong_sign(0)),
            Plant::V6 => read_v6(v6_rig(rendered_reading())),
            Plant::V6Radial => read_v6(v6_rig(TruthV6::RADIAL_ONLY)),
            Plant::V6Radial0 | Plant::V6Radial1 => {
                let b = usize::from(self == Plant::V6Radial1);
                let radial = v6_rig(TruthV6::RADIAL_ONLY);
                read_v6(with_lens(&v6_rig(rendered_reading()), b, radial.lenses[b]))
            }
            Plant::V6Interleaved => read_v6(v6_rig(reading(
                PairOrder::D6D5,
                true,
                PrismAt::D9,
                PrismOrder::Interleaved,
            ))),
            Plant::V6D7 => read_v6(v6_rig(reading(
                PairOrder::D5D6,
                false,
                PrismAt::D7,
                PrismOrder::ByAxis,
            ))),
            Plant::PatsModel0 => pats(0, model_of),
            Plant::PatsModel1 => pats(1, model_of),
            Plant::PatsAngles0 => pats(0, angles_of),
            Plant::PatsAngles1 => pats(1, angles_of),
            Plant::Kb => Setup {
                truth: kb().truth.clone(),
                frames: both(&kb().frames),
                rig: kb().rig,
            },
            Plant::KbEquisolid => Setup {
                truth: kb().truth.clone(),
                frames: both(&kb().frames),
                rig: Rig {
                    lenses: [
                        equisolid_for(&kb().rig.lenses[0]),
                        equisolid_for(&kb().rig.lenses[1]),
                    ],
                    ..kb().rig
                },
            },
        }
    }

    /// The model the plant unwraps its frames with.
    pub fn rig(self) -> Rig {
        self.setup().rig
    }

    /// The plant's seam, measured once for the binary's tests.
    pub fn seam(self) -> Arc<Seam> {
        type Cell = Arc<OnceLock<Arc<Seam>>>;
        static SEAMS: Mutex<BTreeMap<Plant, Cell>> = Mutex::new(BTreeMap::new());
        let cell = SEAMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(self)
            .or_default()
            .clone();
        cell.get_or_init(|| {
            let began = Instant::now();
            let setup = self.setup();
            let seam = Seam::of(&setup);
            eprintln!(
                "{self:?}: {} of {} bins predicted, {:.4} px rms off the prediction over them; \
                 by itself {} bins, {:.4} px rms, median NCC {:.4} ({:.0} ms)\n    predicted {}\n    \
                 measured  {}",
                seam.held,
                seam.predicted.len(),
                seam.off_px,
                seam.own.valid_bins,
                seam.misregistration_px,
                seam.own.median_ncc,
                began.elapsed().as_secs_f64() * 1e3,
                line(&seam.want),
                line(&seam.got),
            );
            Arc::new(seam)
        })
        .clone()
    }
}

// The seam.

/// A strip's pixels a degree along it and across it: its own, 3.811 and
/// 3.8125, which rounding its size leaves a little off 3.81.
pub fn scales() -> (f64, f64) {
    let (width, height) = SPEC.size();
    (
        f64::from(width) / 360.0,
        f64::from(height) / (2.0 * SPEC.half_band_deg),
    )
}

/// A signature's terms on one line.
pub fn line(s: &Signature) -> String {
    let t = terms(s);
    let all: Vec<String> = (0..TERMS)
        .map(|i| format!("{} {:+.4}", NAMES[i], t[i]))
        .collect();
    format!("{}, rest {:.4}", all.join(" "), s.rest_rms_deg)
}

/// The seam of one plant: what its model predicts (8.3's step 1), and
/// what the images give (step 2).
pub struct Seam {
    /// The shifts `predict_bins` gives, with no images, and how many
    /// bins it holds: the others lie past the search, or show nothing.
    pub predicted: Vec<BinShift>,
    pub held: usize,
    /// The shifts `bin_shifts` measures on the strips.
    pub measured: Vec<BinShift>,
    /// The predicted field's signature, and the measured field's over
    /// the bins the prediction holds. Bins are never left out by what
    /// the estimator made of them.
    pub want: Signature,
    pub got: Signature,
    /// The rms of measured less predicted over the bins the prediction
    /// holds, in strip pixels: NaN if the estimator measured none at one
    /// of them, or the prediction holds none.
    pub off_px: f64,
    /// The measured field by itself, over the bins the estimator holds:
    /// its signature, and the rms of its shifts in strip pixels, 4.2's
    /// misregistration between the two lenses' strips.
    pub own: Signature,
    pub misregistration_px: f64,
}

impl Seam {
    pub fn of(setup: &Setup) -> Seam {
        let predicted = predict_bins(&setup.truth, 0, &setup.rig, &SPEC);
        let strips = strips_of(&ready(setup.frames), &setup.rig, &SPEC).unwrap();
        let measured = bin_shifts(&strips, &SPEC);
        let (sx, sy) = scales();
        let rms = |pairs: &mut dyn Iterator<Item = (f64, f64)>| {
            let (mut sum, mut n) = (0.0, 0.0);
            for (across, along) in pairs {
                sum += (across * sy).powi(2) + (along * sx).powi(2);
                n += 1.0;
            }
            (sum / n).sqrt()
        };
        let off_px = rms(&mut measured
            .iter()
            .zip(&predicted)
            .filter(|(_, p)| p.valid)
            .map(|(m, p)| (m.across_deg - p.across_deg, m.along_deg - p.along_deg)));
        let misregistration_px = rms(&mut measured
            .iter()
            .filter(|m| m.valid)
            .map(|m| (m.across_deg, m.along_deg)));
        let over_predicted: Vec<BinShift> = measured
            .iter()
            .zip(&predicted)
            .map(|(m, p)| BinShift {
                valid: m.valid && p.valid,
                ..*m
            })
            .collect();
        Seam {
            held: predicted.iter().filter(|p| p.valid).count(),
            want: signature(&predicted),
            got: signature(&over_predicted),
            off_px,
            own: signature(&measured),
            misregistration_px,
            predicted,
            measured,
        }
    }

    /// How many of the bins the prediction holds the estimator measured
    /// no shift at.
    pub fn unmeasured(&self) -> usize {
        let pairs = self.measured.iter().zip(&self.predicted);
        pairs.filter(|(m, p)| p.valid && !m.valid).count()
    }
}

/// 4.2's bounds on a seam: the misregistration between the strips, in
/// strip pixels; each term of a seam with nothing planted, in degrees;
/// and the median NCC a gross failure falls below (8.2).
pub const MISREGISTRATION_PX: f64 = 0.1;
pub const TERM_DEG: f64 = 0.03;
pub const NCC: f64 = 0.5;

/// The largest of a signature's terms, in degrees; NaN if one is.
pub fn largest_term(s: &Signature) -> f64 {
    terms(s).iter().map(|t| t.abs()).fold(0.0, worse)
}

/// 4.2's seam checks on a true model's seam: every bin measured, the
/// strips within 0.1 px rms of each other, every term under 0.03°, and
/// no NCC collapse.
pub fn hold_seam(name: &str, seam: &Seam) {
    assert_eq!(seam.own.valid_bins, seam.measured.len(), "{name}");
    assert!(
        seam.misregistration_px <= MISREGISTRATION_PX,
        "{name}: {} px",
        seam.misregistration_px
    );
    assert!(
        largest_term(&seam.own) < TERM_DEG,
        "{name}: {}",
        line(&seam.own)
    );
    assert!(
        seam.own.median_ncc >= NCC,
        "{name}: {}",
        seam.own.median_ncc
    );
    eprintln!(
        "{name}: the seam passes: {:.4} px rms, the largest term {:.4}°, median NCC {:.4}",
        seam.misregistration_px,
        largest_term(&seam.own),
        seam.own.median_ncc
    );
}

/// How far apart a model puts what its two lenses show on the seam, in
/// degrees, with no images: over the seam's points, a degree apart, the
/// mean, the least and the most of the angle between the two scene
/// directions that the truth's pictures hold at the model's texels for
/// one point. It is 0 where the model is the truth's, and the size of a
/// field too large for the strips' search to hold.
pub fn apart_deg(truth: &RigTruth, rig: &Rig) -> [f64; 3] {
    let (mut sum, mut least, mut most, mut n) = (0.0, f64::INFINITY, 0.0_f64, 0.0);
    for deg in 0..360 {
        let (sin, cos) = f64::from(deg).to_radians().sin_cos();
        let d = DVec3::new(cos, sin, 0.0);
        let shown = |b: usize| seen(truth, rig.lenses[b].track, 0, rig.texel(b, d)?);
        if let (Some(first), Some(second)) = (shown(0), shown(1)) {
            // By the cross product: an arc cosine can't tell an angle
            // under 1e-6° from none.
            let apart = (first.cross(second).length())
                .atan2(first.dot(second))
                .to_degrees();
            sum += apart;
            least = least.min(apart);
            most = most.max(apart);
            n += 1.0;
        }
    }
    [sum / n, least, most]
}

/// Where 8.3's step 3 finds a field.
#[derive(Debug, Clone, Copy)]
pub enum Lands {
    /// Nowhere: no term reaches 0.03°.
    Nowhere,
    /// On these terms. The largest predicted term is among them, and the
    /// largest measured is that term or another of them that the
    /// prediction gives the same size within 0.03°. The others of them
    /// are the ones the geometry couples to it, and come at the predicted
    /// ratio.
    On(&'static [usize]),
}

/// How far a coupled term's ratio to the dominant term may lie from the
/// predicted ratio.
pub const RATIO: f64 = 0.03;

/// Which of step 3's rules a field breaks. Each has a control that must
/// break that rule and no earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Misfit {
    /// A term has no measured value.
    Unmeasured,
    /// A term reaches 0.03° where the field should land nowhere.
    Shifted,
    /// The prediction's largest term isn't among those the test names.
    Unnamed,
    /// The images' largest term isn't the prediction's, nor a named term
    /// that the prediction gives the same size.
    Elsewhere,
    /// A term the geometry doesn't couple to the dominant one is over its
    /// bound.
    Crosstalk,
    /// A coupled term doesn't come at the predicted ratio.
    Ratio,
}

/// What step 3 found, or the rule that is broken and how.
pub type Attributed = Result<String, (Misfit, String)>;

/// 8.3's step 3 on measured terms, `got`, against a prediction's, `want`:
/// the dominant term is the one the prediction's is, every term the
/// geometry doesn't couple to it stays under max(0.03°, 15 % of it), and
/// a coupled term comes at the predicted ratio. Says what it found, or
/// what doesn't fit.
///
/// The images' largest term may be another of the named ones where the
/// prediction gives it the size of its own largest, within 0.03°: lens
/// 0's V6 terms read as `RadialOnly` are predicted 0.9692° on c1 and
/// 0.9667° on b1, 0.0025° apart, and which of two such terms an estimate
/// good to 0.001° or 0.01° puts first says nothing of the instrument.
pub fn attribute_to(want: &Signature, got: &Signature, lands: Lands) -> Attributed {
    let (want, got) = (terms(want), terms(got));
    if let Some(i) = got.iter().position(|t| !t.is_finite()) {
        return Err((Misfit::Unmeasured, format!("{} wasn't measured", NAMES[i])));
    }
    let largest = |t: &[f64; TERMS]| {
        let by_size = |a: &usize, b: &usize| t[*a].abs().total_cmp(&t[*b].abs());
        (0..TERMS).max_by(by_size).expect("eight terms")
    };
    let named = match lands {
        Lands::Nowhere => {
            let i = largest(&got);
            return if got[i].abs() < TERM_DEG {
                Ok(format!(
                    "nowhere: the largest term {} {:+.4}°",
                    NAMES[i], got[i]
                ))
            } else {
                let said = format!("{} is {:+.4}°, for no shift", NAMES[i], got[i]);
                Err((Misfit::Shifted, said))
            };
        }
        Lands::On(named) => named,
    };
    let (main, found) = (largest(&want), largest(&got));
    if !named.contains(&main) {
        let said = format!(
            "the prediction lands on {} ({:+.4}°)",
            NAMES[main], want[main]
        );
        return Err((Misfit::Unnamed, said));
    }
    // Two named terms that the prediction gives one size, within the
    // bound on a term, may come either way round in the images: the
    // ratio's rule holds the pair.
    let tied = named.contains(&found) && want[main].abs() - want[found].abs() <= TERM_DEG;
    if found != main && !tied {
        let said = format!(
            "the images land on {} ({:+.4}°), the prediction on {} ({:+.4}°)",
            NAMES[found], got[found], NAMES[main], want[main]
        );
        return Err((Misfit::Elsewhere, said));
    }
    let bound = (0.15 * got[main].abs()).max(TERM_DEG);
    let mut said = format!(
        "{} {:+.4}° for {:+.4}° predicted",
        NAMES[main], got[main], want[main]
    );
    let mut other = (0.0, "none");
    for i in (0..TERMS).filter(|&i| i != main) {
        if !named.contains(&i) {
            if got[i].abs() > bound {
                let said = format!(
                    "{} is {:+.4}°, over {bound:.4}° beside {} at {:+.4}°",
                    NAMES[i], got[i], NAMES[main], got[main]
                );
                return Err((Misfit::Crosstalk, said));
            }
            if got[i].abs() > other.0 {
                other = (got[i].abs(), NAMES[i]);
            }
            continue;
        }
        let (ratio, predicted) = (got[i] / got[main], want[i] / want[main]);
        if (ratio - predicted).abs() > RATIO {
            let said = format!(
                "{} comes at {ratio:+.4} of {}, for {predicted:+.4} predicted",
                NAMES[i], NAMES[main]
            );
            return Err((Misfit::Ratio, said));
        }
        // A coupled term is said where it is one: over a term's bound.
        if got[i].abs() > TERM_DEG {
            said += &format!(
                ", {} {:+.4}° at {ratio:+.4} of it for {predicted:+.4}",
                NAMES[i], got[i]
            );
        }
    }
    Ok(format!(
        "{said}; the largest other term {} {:.4}°",
        other.1, other.0
    ))
}
