//! Fisheye lens models.
//!
//! A lens looks down its own −Z axis. A direction at angle `θ` from the
//! optical axis and azimuth `φ` around it lands at radius `r(θ)` in the
//! normalised image plane, at `(r cos φ, r sin φ)` with +y up. The focal
//! scale is 1; per-lens pixel centre, radius and rotation are applied by the
//! caller.
//!
//! Distortion follows the Kannala–Brandt / OpenCV fisheye form on top of the
//! base model: `θ_d = θ (1 + k1 θ² + k2 θ⁴ + k3 θ⁶ + k4 θ⁸)`, then
//! `r = base(θ_d)`.

use glam::{DVec2, DVec3};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// `r = θ`. The usual model for 360 camera lenses.
    Equidistant,
    /// `r = 2 sin(θ/2)`.
    Equisolid,
    /// `r = 2 tan(θ/2)`.
    Stereographic,
    /// `r = sin θ`. Only covers θ ≤ 90°.
    Orthographic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lens {
    pub model: Model,
    pub k: [f64; 4],
}

impl Lens {
    pub const fn ideal(model: Model) -> Self {
        Lens { model, k: [0.0; 4] }
    }

    fn distort(&self, theta: f64) -> f64 {
        let t2 = theta * theta;
        let [k1, k2, k3, k4] = self.k;
        theta * (1.0 + t2 * (k1 + t2 * (k2 + t2 * (k3 + t2 * k4))))
    }

    fn distort_deriv(&self, theta: f64) -> f64 {
        let t2 = theta * theta;
        let [k1, k2, k3, k4] = self.k;
        1.0 + t2 * (3.0 * k1 + t2 * (5.0 * k2 + t2 * (7.0 * k3 + t2 * 9.0 * k4)))
    }

    /// Radius in the image plane for a ray `theta` radians off axis.
    pub fn radius(&self, theta: f64) -> f64 {
        let td = self.distort(theta);
        match self.model {
            Model::Equidistant => td,
            Model::Equisolid => 2.0 * (td / 2.0).sin(),
            Model::Stereographic => 2.0 * (td / 2.0).tan(),
            Model::Orthographic => td.sin(),
        }
    }

    /// Inverse of [`Lens::radius`]. `None` when `r` is outside the lens's
    /// image circle or the distortion polynomial can't be inverted there.
    pub fn theta(&self, r: f64) -> Option<f64> {
        let td = match self.model {
            Model::Equidistant => r,
            Model::Equisolid => {
                if r > 2.0 {
                    return None;
                }
                2.0 * (r / 2.0).asin()
            }
            Model::Stereographic => 2.0 * (r / 2.0).atan(),
            Model::Orthographic => {
                if r > 1.0 {
                    return None;
                }
                r.asin()
            }
        };
        if self.k == [0.0; 4] {
            return Some(td);
        }
        // Newton's method on distort(θ) = td, from the undistorted guess.
        let mut theta = td;
        for _ in 0..20 {
            let f = self.distort(theta) - td;
            let df = self.distort_deriv(theta);
            if df.abs() < 1e-12 {
                return None;
            }
            let step = f / df;
            theta -= step;
            if step.abs() < 1e-14 {
                break;
            }
        }
        ((self.distort(theta) - td).abs() < 1e-9 && theta >= 0.0).then_some(theta)
    }

    /// Direction in lens space → normalised image point.
    pub fn project(&self, d: DVec3) -> DVec2 {
        let d = d.normalize();
        let theta = DVec2::new(d.x, d.y).length().atan2(-d.z);
        let phi = d.y.atan2(d.x);
        let r = self.radius(theta);
        DVec2::new(r * phi.cos(), r * phi.sin())
    }

    /// Normalised image point → direction in lens space.
    pub fn unproject(&self, p: DVec2) -> Option<DVec3> {
        let theta = self.theta(p.length())?;
        let phi = p.y.atan2(p.x);
        let (st, ct) = theta.sin_cos();
        Some(DVec3::new(st * phi.cos(), st * phi.sin(), -ct))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::angle_between;
    use crate::testutil::unit_dir;
    use proptest::prelude::*;

    fn model() -> impl Strategy<Value = Model> {
        prop_oneof![
            Just(Model::Equidistant),
            Just(Model::Equisolid),
            Just(Model::Stereographic),
            Just(Model::Orthographic),
        ]
    }

    #[test]
    fn axis_maps_to_centre() {
        let l = Lens::ideal(Model::Equidistant);
        assert!(l.project(DVec3::NEG_Z).length() < 1e-12);
        let p = l.project(DVec3::X);
        assert!((p - DVec2::new(std::f64::consts::FRAC_PI_2, 0.0)).length() < 1e-12);
    }

    proptest! {
        #[test]
        fn round_trip(d in unit_dir(), m in model(),
                      k1 in -0.05f64..0.05, k2 in -0.01f64..0.01) {
            let lens = Lens { model: m, k: [k1, k2, 0.0, 0.0] };
            let theta = DVec2::new(d.x, d.y).length().atan2(-d.z);
            // Real 360 lenses cover a little over 90°, i.e. θ up to ~100°.
            let max = match m { Model::Orthographic => 1.4, _ => 1.8 };
            prop_assume!(theta < max);
            let back = lens.unproject(lens.project(d));
            prop_assert!(back.is_some());
            prop_assert!(angle_between(back.unwrap_or_default(), d) < 1e-7);
        }
    }
}
