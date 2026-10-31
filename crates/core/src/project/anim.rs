//! A clip's view over time (M2 plan section 6, DECISIONS D50): key
//! tracks for orientation, field of view and projection, and look-at
//! targets, and ([`ViewAnim::at`]) the view they give at any time.

use std::fmt;

use glam::DQuat;
use serde::{Deserialize, Serialize};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::LensShape;
use spherecut_sphere::rotation_angle;
use thiserror::Error;

use super::{KeyId, MediaId, TargetId};
use crate::time::Time;

mod eval;

pub use eval::{Aim, MAX_AIM_PITCH_DEG, View, follow_spin};
pub use spherecut_sphere::spline::{Ease, Side};

/// How a clip's orientation keys are joined (M2 plan section 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrientMode {
    /// Yaw, pitch and roll interpolated as numbers, so a move between
    /// level keys stays level.
    #[default]
    Level,
    /// Along SO(3), for moves the angles can't do well.
    Free,
}

/// A clip's view: three tracks of keys, each with at least one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewAnim {
    pub mode: OrientMode,
    pub orientation: KeyTrack<Orient>,
    /// The output frame's horizontal field of view, in degrees.
    pub fov: KeyTrack<f64>,
    /// Each key a projection or a morph between two (D33's slider).
    pub projection: KeyTrack<LensShape>,
}

/// A track's keys, in increasing time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyTrack<V> {
    pub keys: Vec<Key<V>>,
}

impl<V> KeyTrack<V> {
    pub fn one(id: KeyId, value: V, ease: Ease) -> Self {
        KeyTrack {
            keys: vec![Key {
                id,
                at: Time::ZERO,
                value,
                ease,
                spin: 0,
            }],
        }
    }

    pub fn key(&self, id: KeyId) -> Option<&Key<V>> {
        self.keys.iter().find(|k| k.id == id)
    }

    /// Where a key at `at` goes to keep the keys in time order.
    pub(crate) fn insert(&mut self, key: Key<V>) {
        let i = self.keys.partition_point(|k| k.at < key.at);
        self.keys.insert(i, key);
    }
}

/// A value at a time, in the clip's source time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key<V> {
    #[serde(default)]
    pub id: KeyId,
    pub at: Time,
    pub value: V,
    #[serde(default)]
    pub ease: Ease,
    /// Extra whole turns on the segment from this key to the next (yaw in
    /// Level mode, about the rotation's axis in Free); 0 is the shortest
    /// way. Orientation keys only.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub spin: i8,
}

fn is_zero(n: &i8) -> bool {
    *n == 0
}

/// What an orientation key holds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Orient {
    /// A view orientation.
    View(Orientation),
    /// Look at a target, level but for `roll_deg`.
    Aim {
        target: TargetId,
        #[serde(default)]
        roll_deg: f64,
    },
}

/// A look-at target: something in one media file, as a direction over
/// that file's time. Only clips of that media aim at it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default)]
    pub id: TargetId,
    pub name: String,
    /// The media the target is in; its path is in this media's time.
    pub media: MediaId,
    /// One key for a fixed point, more for a moving one.
    pub path: KeyTrack<AimDir>,
}

/// A direction, as the yaw and pitch of a view looking at it, in degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AimDir {
    pub yaw_deg: f64,
    pub pitch_deg: f64,
}

/// A view orientation (D33: `world = q · view`), always a unit
/// quaternion.
///
/// Written as `{"quat": [x, y, z, w], "ypr_deg": [yaw, pitch, roll]}`.
/// `ypr_deg` is for people and agents reading the file. Reading takes
/// either: `quat` alone, `ypr_deg` alone (built as D33 says), or both, in
/// which case they must agree within [`Orientation::YPR_TOLERANCE_DEG`],
/// so an edit of one that forgets the other is refused instead of half
/// read.
#[derive(Clone, Copy, PartialEq)]
pub struct Orientation(DQuat);

#[derive(Debug, Clone, Copy, PartialEq, Error)]
pub enum OrientationError {
    #[error("a quaternion needs four finite numbers")]
    NotFinite,
    #[error("a quaternion of length near 0 is no rotation")]
    Zero,
    #[error("an orientation needs `quat` or `ypr_deg`")]
    Missing,
    #[error("`quat` and `ypr_deg` disagree by {0:.4}°: edit `quat`, or give only `ypr_deg`")]
    Disagree(f64),
}

impl Orientation {
    pub const IDENTITY: Orientation = Orientation(DQuat::IDENTITY);

    /// How far, in degrees, a file's `ypr_deg` may be from its `quat`:
    /// well above the rounding of the written degrees (6 decimals).
    pub const YPR_TOLERANCE_DEG: f64 = 1e-3;

    /// `q` made unit length. A `q` within 1e−12 of unit length is kept
    /// as it is, so a saved orientation loads back bit for bit.
    pub fn new(q: DQuat) -> Result<Orientation, OrientationError> {
        if !q.is_finite() {
            return Err(OrientationError::NotFinite);
        }
        let len = q.length();
        if len < 1e-6 {
            return Err(OrientationError::Zero);
        }
        if (len - 1.0).abs() < 1e-12 {
            return Ok(Orientation(q));
        }
        Ok(Orientation(q / len))
    }

    pub fn from_ypr(ypr: Ypr) -> Result<Orientation, OrientationError> {
        Orientation::new(ypr.to_quat())
    }

    pub fn quat(self) -> DQuat {
        self.0
    }

    pub fn ypr(self) -> Ypr {
        Ypr::from_quat(self.0)
    }

    /// Yaw, pitch and roll in degrees, rounded to 6 decimals as written.
    pub fn ypr_deg(self) -> [f64; 3] {
        let y = self.ypr();
        let r = |a: f64| (a.to_degrees() * 1e6).round() / 1e6;
        [r(y.yaw), r(y.pitch), r(y.roll)]
    }
}

impl fmt::Debug for Orientation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [y, p, r] = self.ypr_deg();
        write!(f, "Orientation(yaw {y}°, pitch {p}°, roll {r}°)")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename = "Orientation", deny_unknown_fields)]
struct OrientationRepr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quat: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ypr_deg: Option<[f64; 3]>,
}

impl Serialize for Orientation {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        OrientationRepr {
            quat: Some(self.0.to_array()),
            ypr_deg: Some(self.ypr_deg()),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Orientation {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let r = OrientationRepr::deserialize(d)?;
        Orientation::try_from(r).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<OrientationRepr> for Orientation {
    type Error = OrientationError;

    fn try_from(r: OrientationRepr) -> Result<Self, OrientationError> {
        let from_ypr = |[y, p, r]: [f64; 3]| {
            if !(y.is_finite() && p.is_finite() && r.is_finite()) {
                return Err(OrientationError::NotFinite);
            }
            Orientation::from_ypr(Ypr::from_degrees(y, p, r))
        };
        match (r.quat, r.ypr_deg) {
            (None, None) => Err(OrientationError::Missing),
            (None, Some(ypr)) => from_ypr(ypr),
            (Some(q), ypr) => {
                let o = Orientation::new(DQuat::from_array(q))?;
                if let Some(ypr) = ypr {
                    let off = rotation_angle(o.0, from_ypr(ypr)?.0).to_degrees();
                    if off > Orientation::YPR_TOLERANCE_DEG {
                        return Err(OrientationError::Disagree(off));
                    }
                }
                Ok(o)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> Result<Orientation, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    #[test]
    fn orientation_reads_either_form() {
        let o = Orientation::from_ypr(Ypr::from_degrees(30.0, -10.0, 5.0)).unwrap();
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("\"ypr_deg\":[30.0,-10.0,5.0]"), "{json}");
        assert_eq!(read(&json).unwrap(), o);
        let only_ypr = read(r#"{"ypr_deg": [30, -10, 5]}"#).unwrap();
        assert!(rotation_angle(only_ypr.quat(), o.quat()) < 1e-12);
        let q = o.quat().to_array();
        let only_quat = read(&format!("{{\"quat\": {q:?}}}")).unwrap();
        assert_eq!(only_quat, o);
    }

    #[test]
    fn orientation_refuses_disagreement_and_junk() {
        let e = read(r#"{"quat": [0, 0, 0, 1], "ypr_deg": [10, 0, 0]}"#).unwrap_err();
        assert!(e.contains("disagree by 10.0000°"), "{e}");
        assert!(
            read(r#"{"quat": [0, 0, 0, 0]}"#)
                .unwrap_err()
                .contains("near 0")
        );
        assert!(
            read(r#"{}"#)
                .unwrap_err()
                .contains("needs `quat` or `ypr_deg`")
        );
        assert!(read(r#"{"quat": [0, 0, 0, 1], "yaw": 3}"#).is_err());
    }

    #[test]
    fn orientation_normalises_once() {
        let o = Orientation::new(DQuat::from_xyzw(0.0, 0.0, 0.0, 2.0)).unwrap();
        assert_eq!(o.quat(), DQuat::IDENTITY);
        // A quaternion already unit length to rounding is kept bit for bit.
        let q = Ypr::from_degrees(12.3, 45.6, 78.9).to_quat();
        assert_eq!(Orientation::new(q).unwrap().quat().to_array(), q.to_array());
    }

    #[test]
    fn keys_write_what_they_hold() {
        let k = Key {
            id: KeyId(4),
            at: Time::from_secs(2),
            value: 75.0,
            ease: Ease::default(),
            spin: 0,
        };
        let json = serde_json::to_value(&k).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"id": 4, "at": {"exact": "2", "seconds": 2.0}, "value": 75.0,
                "ease": {"into": "smooth", "out": "smooth"}})
        );
        let aim: Orient = serde_json::from_str(r#"{"aim": {"target": 9}}"#).unwrap();
        assert_eq!(
            aim,
            Orient::Aim {
                target: TargetId(9),
                roll_deg: 0.0
            }
        );
    }
}
