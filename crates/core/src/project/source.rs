//! What a media's pixels are, and how they map to the sphere (M3 plan
//! 11.1, D68).
//!
//! An equirect file is its own picture. A camera file of two fisheye
//! lenses keeps in the project everything a render reads: the calibration,
//! raw and parsed, with its hash; the geometry with its status; the tune;
//! and the gains beside it, Auto's exposure track included. So export, the
//! CLI and the agents repeat a render with no trailer reader and no cache,
//! the file still diffs (D47), and the server reads files while an agent
//! never does (D57). The parsed numbers are `sphere::rig`'s types, so core
//! and render need no camera crate (D48).
//!
//! Schema 1 has no source, which reads as [`Source::default`], equirect.
//! Every hash here is 16 lowercase hex digits of [`fnv1a64`], as a media's
//! [`Fingerprint::head`] is.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use spherecut_sphere::rig::{Calibration, Geometry, LensCalib, Rig, RigError, Tune};

use super::Fingerprint;
use super::file::fnv1a64;
use crate::time::Time;

/// What a media's pixels are. Written with its `kind`: `{"kind":
/// "equirect"}`, or `"dual_fisheye"` or `"eac_pair"` beside the fields of
/// [`DualFisheye`] or [`EacPair`]; a key the kind doesn't have is refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// One equirectangular picture, as M1 and M2 read every file. A
    /// variant with no fields rather than a unit one, so that a key beside
    /// its kind is refused, not ignored.
    Equirect {},
    /// A camera's two fisheye lenses, each a track of the file (the
    /// Insta360 X5).
    DualFisheye(Box<DualFisheye>),
    /// A camera's two EAC tracks (the GoPro MAX, M3.18).
    EacPair(Box<EacPair>),
}

impl Default for Source {
    /// Equirect: what a schema 1 media is, having no source.
    fn default() -> Source {
        Source::Equirect {}
    }
}

/// A camera's two fisheye lenses, and what the project renders them with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DualFisheye {
    pub camera: Camera,
    /// The two lens streams, by FFmpeg's index, in file order: track 0
    /// and track 1. The media's `video` is track 0's stream, whose timing
    /// the clip follows.
    pub tracks: [u32; 2],
    /// Which track's lens looks forward, 0 or 1 ([`Rig::build`]'s
    /// `front_track`, plan 6.2).
    pub front_track: u8,
    pub calibration: CalibSnapshot,
    pub geometry: Geometry,
    pub tune: Tune,
    pub gains: Gains,
    /// The stitch profile the tune came from (plan 17), as provenance: a
    /// later edit of the profile never changes the project.
    pub profile: Option<ProfileRef>,
}

impl DualFisheye {
    /// The rig this source renders with, for tracks whose sides, by
    /// track, are `sides`: the snapshot's calibration under the geometry
    /// and the tune, through [`Rig::build`].
    pub fn rig(&self, sides: [u32; 2]) -> Result<Rig, RigError> {
        Rig::build(
            &self.calibration.calibration(),
            &self.geometry,
            &self.tune,
            self.front_track,
            sides,
        )
    }
}

/// The camera a source came from, as its file names it. `model`,
/// `firmware` and `serial` are the trailer's record 1 fields 2, 3 and 1
/// verbatim ("Insta360 X5", "v1.1.22_build1"), never the geometry
/// table's model for the camera. `make` is the one field not from the
/// metadata: the vendor media's detection names ("Insta360").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub make: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub serial: Option<String>,
}

/// The camera's calibration as the file held it when the source was made:
/// the strings, their hash, and the parsed blocks a rig reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibSnapshot {
    /// The calibration canvas, width and height: two squares side by side
    /// (10752 × 5376 on the X5), each block's square its height
    /// ([`Calibration::src`]).
    pub canvas: [u32; 2],
    /// Field 27's window, width and height: the part of each block's
    /// square the tracks show (5312² on the X5; [`Calibration::dst`]).
    /// The square's own side when the file has no field 27.
    pub window: [u32; 2],
    /// Field 27's offset of the window in its square ([0, 0] on the X5;
    /// [0, 0] without a field 27), kept from the start since plan
    /// amendment 7 allows it. Nothing reads it until M3.6 rules on plan
    /// 3.2's U13: [`CalibSnapshot::calibration`] leaves it out, as camera's
    /// `to_calibration` does.
    pub window_offset: [u32; 2],
    pub raw: RawStrings,
    /// [`RawStrings::hash`] of `raw`, as the file's own was when the
    /// source was made.
    pub hash: String,
    /// V3's two blocks, by block, if the file holds them.
    pub v3: Option<[LensCalib; 2]>,
    /// V6's two blocks, by block, if the file holds them.
    pub v6: Option<[LensCalib; 2]>,
}

impl CalibSnapshot {
    /// The calibration a rig reads: the canvas's height as `src`, the
    /// window's width as `dst`, and the blocks. The window's offset isn't
    /// read (U13).
    pub fn calibration(&self) -> Calibration {
        Calibration {
            src: self.canvas[1],
            dst: self.window[0],
            v3: self.v3,
            v6: self.v6,
        }
    }
}

/// Each generation's working calibration string as the trailer stores it
/// (record 1's fields 5, 53, 54 and 111), or none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawStrings {
    pub v1: Option<String>,
    pub v2: Option<String>,
    pub v3: Option<String>,
    pub v6: Option<String>,
}

impl RawStrings {
    /// The strings' hash (plan 11.1): [`fnv1a64`] over V1, V2, V3 and V6
    /// in that order, each ended by a NUL byte, so that a generation the
    /// file lacks is its NUL alone; as 16 lowercase hex digits. Camera's
    /// `Calibrations::hash` hashes a file's strings the same way.
    pub fn hash(&self) -> String {
        let mut bytes = Vec::new();
        for s in [&self.v1, &self.v2, &self.v3, &self.v6] {
            if let Some(s) = s {
                bytes.extend(s.as_bytes());
            }
            bytes.push(0);
        }
        format!("{:016x}", fnv1a64(&bytes))
    }
}

/// The numbers of the tune an Auto exposure track was measured under
/// (plan 16): [`fnv1a64`] over the little-endian bytes of lens 0's
/// `ypr_deg`, lens 1's, then `q_per_m`, as 16 lowercase hex digits. A
/// `set_stitch` that changes any of them leaves the track stale.
pub fn gains_basis(tune: &Tune) -> String {
    let mut bytes = Vec::with_capacity(7 * 8);
    for lens in &tune.lenses {
        for v in lens.ypr_deg {
            bytes.extend(v.to_le_bytes());
        }
    }
    bytes.extend(tune.q_per_m.to_le_bytes());
    format!("{:016x}", fnv1a64(&bytes))
}

/// How the two lenses' levels are matched (plan 16). Written with its
/// `mode`: `{"mode": "off"}`, `{"mode": "fixed", "rgb": [[…], […]]}` or
/// `{"mode": "auto", …}`.
///
/// Every pair is by calibration block, as the tune's lenses are: a
/// [`Gains::Fixed`]'s `rgb` and what [`Gains::at`] gives are `[block 0's
/// red, green and blue gains, block 1's]`, and Auto's `log_ratio` is
/// ln(L1 / L0), block 1's level over block 0's, in linear light.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Gains {
    /// No matching: every gain 1. A variant with no fields rather than a
    /// unit one, so that a key beside its mode is refused.
    Off {},
    /// Each block's red, green and blue gains, by block.
    Fixed { rgb: [[f64; 3]; 2] },
    /// The track an analysis of the clip measured.
    Auto {
        /// Samples a second.
        rate_hz: f64,
        /// ln(L1 / L0) for red, green and blue at each sample, the first
        /// at the clip's time 0, smoothed with zero phase (plan 16).
        log_ratio: Vec<[f32; 3]>,
        /// [`gains_basis`] of the tune the track was measured under.
        basis: String,
        input: AnalysisInput,
        /// The estimator's version, so a later estimator can tell its own
        /// tracks from older ones.
        estimator: u32,
    },
}

impl Gains {
    /// Each block's red, green and blue gains at clip time `t`, by block:
    /// Off gives ones, Fixed its `rgb`, and Auto its track at `t`, linear
    /// between samples, the ratio L split as exp(+L/2) for block 0 and
    /// exp(−L/2) for block 1 so both meet at √(L0·L1) (plan 16).
    #[expect(unused_variables, reason = "M3.9 unit (a) writes the body")]
    pub fn at(&self, t: Time) -> [[f64; 3]; 2] {
        todo!("M3.9 unit (a)")
    }
}

/// What an Auto track was measured from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisInput {
    pub file: Analysed,
    /// The analysed file's own fingerprint: the master's, or its LRV's.
    pub fingerprint: Fingerprint,
    pub frames: Sampling,
}

/// Which of a recording's files an analysis decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Analysed {
    /// The media's own file.
    Master,
    /// Its low-resolution companion, when verified as its copy (plan 15).
    Lrv,
}

/// Which frames an analysis measured. Written with its `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Sampling {
    /// Every `n`th frame.
    Every { n: u32 },
    /// The keyframes alone, where a machine decodes in software only and
    /// M3.15's P-GAIN report allows it (plan 16). A variant with no fields
    /// rather than a unit one, so that a key beside its kind is refused.
    Keyframes {},
}

/// Where the tune came from: a stitch profile by name and its file's
/// hash (plan 17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRef {
    pub name: String,
    /// [`fnv1a64`] of the profile file's bytes, as 16 lowercase hex
    /// digits.
    pub hash: String,
}

impl ProfileRef {
    /// The reference to the profile `name` whose file is `bytes`.
    pub fn of(name: &str, bytes: &[u8]) -> ProfileRef {
        ProfileRef {
            name: name.to_owned(),
            hash: format!("{:016x}", fnv1a64(bytes)),
        }
    }
}

/// A camera's two EAC tracks (plan 18.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EacPair {
    pub camera: Camera,
    /// The two streams, by FFmpeg's index, in file order.
    pub tracks: [u32; 2],
    pub layout: EacLayout,
}

/// An EAC track's layout, from `PMOD` (`docs/formats/gopro.md` §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EacLayout {
    /// `PMOD[0]`: overlap texels past the cut, at capture scale.
    pub overlap: u32,
    /// `PMOD[1]`: a face and both overlaps before the side cells'
    /// horizontal squeeze.
    pub pre: u32,
    /// `PMOD[2]`: a stored side cell's width.
    pub post: u32,
    /// One track's coded width and height.
    pub size: [u32; 2],
}

/// The caps over the poles (plan 18.3, M3.20): none by default. Each is
/// boxed, which serde writes as the cap itself, so a media without caps
/// stays small in memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caps {
    pub nadir: Option<Box<Cap>>,
    pub zenith: Option<Box<Cap>>,
}

/// One pole's cap, anchored to the rig.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cap {
    pub mode: CapMode,
    /// The cap's angular radius from the pole, 5° to 35°.
    pub cap_deg: f64,
    /// The width of its soft edge, 0° to 5°.
    pub edge_deg: f64,
    /// How far a patch is turned, clockwise as seen.
    pub rotation_deg: f64,
}

/// What fills a cap. Written with its `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CapMode {
    /// The source sampled `ring_deg` past the cap's edge.
    Fill { ring_deg: f64 },
    /// A PNG, seen from inside looking at the pole.
    Patch {
        path: PathBuf,
        fingerprint: Fingerprint,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::Instant;

    use serde_json::json;
    use spherecut_sphere::fisheye::V6Reading;
    use spherecut_sphere::rig::{
        Binding, CalibTerms, CanvasMap, EulerConv, Extrinsics, Generation, LensTune, QuarterTurn,
        Status, TurnAxis, TzSign,
    };
    use spherecut_testkit::control::must_fail;

    use super::*;

    /// A test's clock, which prints its time as the test ends.
    struct Timed(&'static str, Instant);

    impl Drop for Timed {
        fn drop(&mut self) {
            eprintln!("{}: {:.3} s", self.0, self.1.elapsed().as_secs_f64());
        }
    }

    fn timed(name: &'static str) -> Timed {
        Timed(name, Instant::now())
    }

    /// The strings' hash is FNV-1a 64 over V1, V2, V3 and V6, each ended
    /// by a NUL, worked by hand: four NULs alone without strings, and my
    /// own V3 and V6 strings after V1's and V2's NULs. Control: the same
    /// two strings swapped between V3 and V6 hash otherwise.
    #[test]
    fn the_snapshot_hash_is_fnv_over_the_strings() {
        let _time = timed("the_snapshot_hash_is_fnv_over_the_strings");
        assert_eq!(RawStrings::default().hash(), "4d25767f9dce13f5");
        let mine = RawStrings {
            v1: None,
            v2: None,
            v3: Some("a v3 string of my own".into()),
            v6: Some("and a v6 one".into()),
        };
        assert_eq!(mine.hash(), "651ac8fa2b55725b");
        let every = RawStrings {
            v1: Some("one".into()),
            v2: Some("two".into()),
            v3: Some("three".into()),
            v6: Some("six".into()),
        };
        assert_eq!(every.hash(), "2c0c7cf1d0b574bb");
        let swapped = RawStrings {
            v3: mine.v6.clone(),
            v6: mine.v3.clone(),
            ..mine.clone()
        };
        assert_eq!(swapped.hash(), "083a0fcb1501ae33");
        must_fail(
            "V3's and V6's strings swapped hash as they were",
            swapped.hash() == mine.hash(),
        );
    }

    /// A camera media's source in the X5's shape, with a calibration of
    /// my own: written as 11.1 shapes it, every field present, and read
    /// back as it was.
    fn x5_like() -> DualFisheye {
        let block = |cx: f64, ypr_deg, t_m, d0: f64| LensCalib {
            xi: 1.5,
            fx: 2000.0,
            fy: 2000.5,
            cx,
            cy: 2688.25,
            ypr_deg,
            t_m,
            terms: CalibTerms::V6 {
                d: std::array::from_fn(|i| if i == 0 { d0 } else { 0.0 }),
            },
        };
        let mut tune = Tune::default();
        tune.lenses[1] = LensTune {
            ypr_deg: [0.5, 0.0, 0.0],
            ..tune.lenses[1]
        };
        let raw = RawStrings {
            v1: None,
            v2: None,
            v3: None,
            v6: Some("a v6 string of my own".into()),
        };
        DualFisheye {
            camera: Camera {
                make: Some("Insta360".into()),
                model: Some("Insta360 X5".into()),
                firmware: Some("v1.0.0_build1".into()),
                serial: Some("MYOWN0001".into()),
            },
            tracks: [0, 1],
            front_track: 0,
            calibration: CalibSnapshot {
                canvas: [10752, 5376],
                window: [5312, 5312],
                window_offset: [0, 0],
                hash: raw.hash(),
                raw,
                v3: None,
                v6: Some([
                    block(2690.5, [0.0, 0.0, 90.0], [0.0, 0.0, 0.0], 0.01),
                    block(2685.75, [179.5, 0.25, -90.0], [0.0, 0.0, -0.03], 0.02),
                ]),
            },
            geometry: Geometry {
                binding: Binding::Reversed,
                canvas: CanvasMap::C,
                extrinsics: Extrinsics::Stored(EulerConv::PLACEHOLDER),
                turn: TurnAxis::Y,
                tz: TzSign::AsStored,
                upright: QuarterTurn::Zero,
                generation: Generation::V6,
                v6: V6Reading::RADIAL_ONLY,
                status: Status::Provisional,
                evidence: "placeholder until the fit".into(),
            },
            tune,
            gains: Gains::Fixed {
                rgb: [[1.0, 1.0, 1.0], [0.875, 0.875, 0.875]],
            },
            profile: None,
        }
    }

    /// A dual-fisheye source writes 11.1's JSON, `"kind": "dual_fisheye"`
    /// beside its fields, and reads back as it was, as do Auto gains and
    /// every source, sampling and cap kind. Controls: the kind written
    /// `"fisheye"`, and a key beside the kind that no source has, are
    /// refused; so is a key beside `equirect` or `off`, whose variants
    /// have no fields.
    #[test]
    fn a_dual_fisheye_source_reads_back() {
        let _time = timed("a_dual_fisheye_source_reads_back");
        let source = Source::DualFisheye(Box::new(x5_like()));
        let v = serde_json::to_value(&source).unwrap();
        let block = |cx: f64, ypr: [f64; 3], t: [f64; 3], d0: f64| {
            let mut d = [0.0; 13];
            d[0] = d0;
            json!({"xi": 1.5, "fx": 2000.0, "fy": 2000.5, "cx": cx, "cy": 2688.25,
                "ypr_deg": ypr, "t_m": t, "terms": {"v6": {"d": d}}})
        };
        let lens = |yaw: f64| {
            json!({"ypr_deg": [yaw, 0.0, 0.0], "centre_px": [0.0, 0.0], "radius_scale": 1.0,
                "xi_delta": 0.0, "theta_max_deg": 100.0})
        };
        let written = json!({
            "kind": "dual_fisheye",
            "camera": {"make": "Insta360", "model": "Insta360 X5",
                "firmware": "v1.0.0_build1", "serial": "MYOWN0001"},
            "tracks": [0, 1],
            "front_track": 0,
            "calibration": {
                "canvas": [10752, 5376], "window": [5312, 5312], "window_offset": [0, 0],
                "raw": {"v1": null, "v2": null, "v3": null, "v6": "a v6 string of my own"},
                "hash": RawStrings { v6: Some("a v6 string of my own".into()),
                    ..RawStrings::default() }.hash(),
                "v3": null,
                "v6": [block(2690.5, [0.0, 0.0, 90.0], [0.0, 0.0, 0.0], 0.01),
                    block(2685.75, [179.5, 0.25, -90.0], [0.0, 0.0, -0.03], 0.02)],
            },
            "geometry": {"binding": "reversed", "canvas": "c",
                "extrinsics": {"stored": {"axes": ["y", "x", "z"],
                    "order": ["yaw", "pitch", "roll"], "signs": [1, 1, 1], "roll_offset": -90}},
                "turn": "y", "tz": "as_stored", "upright": 0, "generation": "v6",
                "v6": {"tangential": null, "prism": null}, "status": "provisional",
                "evidence": "placeholder until the fit"},
            "tune": {"lenses": [lens(0.0), lens(0.5)], "feather_deg": 5.0, "q_per_m": 0.5},
            "gains": {"mode": "fixed", "rgb": [[1.0, 1.0, 1.0], [0.875, 0.875, 0.875]]},
            "profile": null,
        });
        assert_eq!(v, written);
        assert_eq!(serde_json::from_value::<Source>(v.clone()).unwrap(), source);

        let mut fisheye = v.clone();
        fisheye["kind"] = json!("fisheye");
        must_fail(
            "the kind written \"fisheye\"",
            serde_json::from_value::<Source>(fisheye),
        );
        let mut extra = v.clone();
        extra["lens_count"] = json!(2);
        must_fail(
            "a key beside the kind",
            serde_json::from_value::<Source>(extra),
        );

        let equirect = json!({"kind": "equirect"});
        assert_eq!(serde_json::to_value(Source::default()).unwrap(), equirect);
        assert_eq!(
            serde_json::from_value::<Source>(equirect).unwrap(),
            Source::default()
        );
        must_fail(
            "a key beside equirect",
            serde_json::from_value::<Source>(json!({"kind": "equirect", "tune": {}})),
        );
        assert_eq!(
            serde_json::to_value(Gains::Off {}).unwrap(),
            json!({"mode": "off"})
        );
        must_fail(
            "a key beside off",
            serde_json::from_value::<Gains>(json!({"mode": "off", "rgb": []})),
        );

        // Every other kind reads back as written.
        let mut auto = x5_like();
        auto.gains = Gains::Auto {
            rate_hz: 4.0,
            log_ratio: vec![[0.0, 0.0, 0.0], [0.025, 0.05, -0.0125]],
            basis: gains_basis(&auto.tune),
            input: AnalysisInput {
                file: Analysed::Lrv,
                fingerprint: Fingerprint {
                    bytes: 1234,
                    head: "0123456789abcdef".into(),
                },
                frames: Sampling::Every { n: 6 },
            },
            estimator: 1,
        };
        auto.profile = Some(ProfileRef::of("tripod", b"{}"));
        let v = serde_json::to_value(&auto.gains).unwrap();
        assert_eq!(v["mode"], "auto");
        assert_eq!(v["input"]["frames"], json!({"kind": "every", "n": 6}));
        assert_eq!(v["input"]["file"], "lrv");
        let eac = Source::EacPair(Box::new(EacPair {
            camera: Camera::default(),
            tracks: [0, 1],
            layout: EacLayout {
                overlap: 32,
                pre: 1408,
                post: 1376,
                size: [4096, 1344],
            },
        }));
        for s in [Source::DualFisheye(Box::new(auto)), eac] {
            let v = serde_json::to_value(&s).unwrap();
            assert_eq!(serde_json::from_value::<Source>(v).unwrap(), s);
        }
        let keyframes = Sampling::Keyframes {};
        let v = serde_json::to_value(keyframes).unwrap();
        assert_eq!(v, json!({"kind": "keyframes"}));
        assert_eq!(serde_json::from_value::<Sampling>(v).unwrap(), keyframes);
        let caps = Caps {
            nadir: Some(Box::new(Cap {
                mode: CapMode::Fill { ring_deg: 2.0 },
                cap_deg: 20.0,
                edge_deg: 2.5,
                rotation_deg: 0.0,
            })),
            zenith: Some(Box::new(Cap {
                mode: CapMode::Patch {
                    path: PathBuf::from("/home/me/logo.png"),
                    fingerprint: Fingerprint {
                        bytes: 99,
                        head: "fedcba9876543210".into(),
                    },
                },
                cap_deg: 10.0,
                edge_deg: 1.0,
                rotation_deg: 45.0,
            })),
        };
        let v = serde_json::to_value(&caps).unwrap();
        assert_eq!(v["nadir"]["mode"], json!({"kind": "fill", "ring_deg": 2.0}));
        assert_eq!(serde_json::from_value::<Caps>(v).unwrap(), caps);
        assert_eq!(
            serde_json::to_value(Caps::default()).unwrap(),
            json!({"nadir": null, "zenith": null})
        );
    }

    /// The gains' basis is FNV-1a 64 over the little-endian bytes of lens
    /// 0's yaw, pitch and roll, lens 1's, then q, worked by hand for
    /// lens 1 yawed 0.5° at q = 0.5: 24 zero bytes, `00 … e0 3f`, 16 zero
    /// bytes, `00 … e0 3f`. A profile's hash is FNV-1a 64 of its bytes.
    /// Control: the lenses' tunes swapped give another basis.
    #[test]
    fn the_gains_basis_and_profile_hash_are_pinned() {
        let _time = timed("the_gains_basis_and_profile_hash_are_pinned");
        let mut tune = Tune::default();
        tune.lenses[1].ypr_deg = [0.5, 0.0, 0.0];
        assert_eq!(gains_basis(&tune), "decedafefc6ea625");
        let mut swapped = tune;
        swapped.lenses.swap(0, 1);
        assert_eq!(gains_basis(&swapped), "8d94e7ca3b60c345");
        must_fail(
            "the lenses' tunes swapped give the same basis",
            gains_basis(&swapped) == gains_basis(&tune),
        );
        // Neither the feather nor a lens's centre is in the basis.
        let mut wider = tune;
        wider.feather_deg = 10.0;
        wider.lenses[0].centre_px = [3.0, -2.0];
        assert_eq!(gains_basis(&wider), gains_basis(&tune));
        assert_eq!(
            ProfileRef::of("tripod", br#"{"name": "tripod"}"#),
            ProfileRef {
                name: "tripod".into(),
                hash: "e752d2d93e6e1fd8".into(),
            }
        );
    }
}
