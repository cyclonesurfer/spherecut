//! Yaw, pitch and roll for a view orientation (the M1 plan's view
//! conventions, DECISIONS D33).
//!
//! Stored orientations are quaternions; yaw, pitch and roll are how the
//! UI, `spherecut frame` and M2's keyframe editor spell them:
//!
//! * yaw > 0 turns right, about the world's up axis;
//! * pitch > 0 looks up, about the camera's own right axis after the yaw;
//! * roll > 0 rolls the camera clockwise about its view axis, as seen from
//!   behind it: its up turns towards its right, so the scene turns
//!   anticlockwise in the picture and the world's up moves to its left.
//!
//! `q = R_y(−yaw) · R_x(pitch) · R_z(−roll)`, and as everywhere else
//! `world = q · view` ([`crate`]'s conventions: +X right, +Y up, −Z
//! forward).

use glam::{DQuat, DVec3};

use crate::spline::wrap;

/// An orientation as yaw, pitch and roll, in radians.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Ypr {
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
}

impl Ypr {
    pub fn new(yaw: f64, pitch: f64, roll: f64) -> Ypr {
        Ypr { yaw, pitch, roll }
    }

    pub fn from_degrees(yaw: f64, pitch: f64, roll: f64) -> Ypr {
        Ypr::new(yaw.to_radians(), pitch.to_radians(), roll.to_radians())
    }

    /// The view orientation, `R_y(−yaw) · R_x(pitch) · R_z(−roll)`.
    pub fn to_quat(self) -> DQuat {
        DQuat::from_rotation_y(-self.yaw)
            * DQuat::from_rotation_x(self.pitch)
            * DQuat::from_rotation_z(-self.roll)
    }

    /// The yaw, pitch and roll of `q` (which need not be normalised), with
    /// yaw and roll in (−π, π] and pitch in [−π/2, π/2]. Looking straight
    /// up or down, yaw and roll turn about the same axis; the roll is then
    /// 0 and the yaw says it all.
    pub fn from_quat(q: DQuat) -> Ypr {
        let q = q.normalize();
        let f = q * DVec3::NEG_Z;
        let level = f.x.hypot(f.z);
        let pitch = f.y.atan2(level);
        if level < 1e-9 {
            let r = q * DVec3::X;
            return Ypr::new(wrap(r.z.atan2(r.x)), pitch, 0.0);
        }
        let yaw = f.x.atan2(-f.z);
        // What is left once yaw and pitch are undone is R_z(−roll).
        let rest = (DQuat::from_rotation_y(-yaw) * DQuat::from_rotation_x(pitch)).conjugate() * q;
        let roll = -2.0 * rest.z.atan2(rest.w);
        Ypr::new(wrap(yaw), pitch, wrap(roll))
    }

    /// The world-frame angular velocity of this orientation while its
    /// angles change at `rate` (radians per second each): yaw turns about
    /// the world's down axis, pitch about the camera's right after the
    /// yaw, and roll about its forward.
    pub fn omega(self, rate: Ypr) -> DVec3 {
        let q = self.to_quat();
        let right = DQuat::from_rotation_y(-self.yaw) * DVec3::X;
        DVec3::NEG_Y * rate.yaw + right * rate.pitch + (q * DVec3::NEG_Z) * rate.roll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::unit_quat;
    use crate::{angle_between, rotation_angle};
    use proptest::prelude::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    fn close(a: DVec3, b: DVec3) -> bool {
        angle_between(a, b) < 1e-12
    }

    #[test]
    fn each_angle_turns_the_way_documented() {
        // Yaw 90 turns right: forward goes to +X, and up stays up.
        let q = Ypr::from_degrees(90.0, 0.0, 0.0).to_quat();
        assert!(close(q * DVec3::NEG_Z, DVec3::X));
        assert!(close(q * DVec3::Y, DVec3::Y));
        // Pitch 90 looks up.
        let q = Ypr::from_degrees(0.0, 90.0, 0.0).to_quat();
        assert!(close(q * DVec3::NEG_Z, DVec3::Y));
        // Roll 90 turns the camera's up to its right (clockwise from
        // behind), so the world's up is on the picture's left.
        let q = Ypr::from_degrees(0.0, 0.0, 90.0).to_quat();
        assert!(close(q * DVec3::Y, DVec3::X));
        assert!(close(q * DVec3::NEG_Z, DVec3::NEG_Z));
        assert!(close(q.conjugate() * DVec3::Y, DVec3::NEG_X));
        // Pitch turns about the camera's own right axis after the yaw.
        let q = Ypr::from_degrees(90.0, 45.0, 0.0).to_quat();
        assert!(close(q * DVec3::NEG_Z, DVec3::new(1.0, 1.0, 0.0)));
        // Yaw 180 looks back, where the equirect seam is.
        let q = Ypr::from_degrees(180.0, 0.0, 0.0).to_quat();
        assert!(close(q * DVec3::NEG_Z, DVec3::Z));
    }

    #[test]
    fn straight_up_and_down_keep_roll_at_zero() {
        for pitch in [90.0, -90.0] {
            let q = Ypr::from_degrees(30.0, pitch, 20.0).to_quat();
            let ypr = Ypr::from_quat(q);
            assert_eq!(ypr.roll, 0.0);
            assert!((ypr.pitch.abs() - FRAC_PI_2).abs() < 1e-12);
            assert!(rotation_angle(ypr.to_quat(), q) < 1e-9, "{pitch}: {ypr:?}");
        }
    }

    proptest! {
        #[test]
        fn quaternions_round_trip(q in unit_quat()) {
            let ypr = Ypr::from_quat(q);
            prop_assert!(ypr.pitch.abs() <= FRAC_PI_2);
            prop_assert!(ypr.yaw > -PI && ypr.yaw <= PI);
            prop_assert!(ypr.roll > -PI && ypr.roll <= PI);
            prop_assert!(rotation_angle(ypr.to_quat(), q) < 1e-9);
        }

        #[test]
        fn omega_is_the_rate_of_turn(
            yaw in -PI..PI, pitch in -1.5f64..1.5, roll in -PI..PI,
            dy in -3.0f64..3.0, dp in -3.0f64..3.0, dr in -3.0f64..3.0,
        ) {
            let at = |t: f64| Ypr::new(yaw + dy * t, pitch + dp * t, roll + dr * t).to_quat();
            let h = 1e-6;
            let fd = crate::spline::log(at(h) * at(-h).conjugate()) / (2.0 * h);
            let w = Ypr::new(yaw, pitch, roll).omega(Ypr::new(dy, dp, dr));
            prop_assert!((fd - w).length() < 1e-6, "{fd} {w}");
        }

        #[test]
        fn angles_round_trip(yaw in -PI..PI, pitch in -1.56f64..1.56, roll in -PI..PI) {
            let back = Ypr::from_quat(Ypr::new(yaw, pitch, roll).to_quat());
            let diff = |a: f64, b: f64| wrap(a - b).abs();
            prop_assert!(diff(back.yaw, yaw) < 1e-9, "{back:?}");
            prop_assert!((back.pitch - pitch).abs() < 1e-9, "{back:?}");
            prop_assert!(diff(back.roll, roll) < 1e-9, "{back:?}");
        }
    }
}
