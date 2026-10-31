//! Spherical projection math, on the CPU, in `f64`.
//!
//! The render crate's WGSL shaders implement the same functions in `f32`,
//! and its CPU reference renderer (`spherecut_render::reference`) checks
//! them against this crate. The synthetic test-media generator uses it to
//! draw known scenes.
//!
//! # Conventions
//!
//! Directions are unit vectors in a right-handed frame with **+X right,
//! +Y up, −Z forward**. "Forward" is the centre of an equirectangular frame.
//!
//! Longitude is `atan2(x, −z)`: 0 forward, +π/2 right, ±π behind.
//! Latitude is `asin(y)`: +π/2 straight up.
//!
//! Normalised image coordinates are `(u, v)` in `[0, 1]²` with `v = 0` at the
//! top row, so they map to texels as `(u · width, v · height)` with texel
//! centres at half-integers.

#![forbid(unsafe_code)]

use glam::DVec3;

pub mod cube;
pub mod fisheye;
pub mod grab;
pub mod orient;
pub mod perspective;
pub mod pixel;
pub mod spline;

pub use glam::{DQuat, DVec2};

use std::f64::consts::{FRAC_PI_2, PI, TAU};

/// Longitude and latitude in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LonLat {
    pub lon: f64,
    pub lat: f64,
}

impl LonLat {
    pub fn from_dir(d: DVec3) -> Self {
        let d = d.normalize();
        LonLat {
            lon: d.x.atan2(-d.z),
            lat: d.y.clamp(-1.0, 1.0).asin(),
        }
    }

    pub fn to_dir(self) -> DVec3 {
        let (sl, cl) = self.lat.sin_cos();
        let (so, co) = self.lon.sin_cos();
        DVec3::new(cl * so, sl, -cl * co)
    }
}

/// Equirectangular (plate carrée) projection of the full sphere.
pub mod equirect {
    use super::*;

    /// Direction → normalised `(u, v)`. `u` is in `[0, 1)`; the seam at
    /// `u = 0 ≡ 1` is directly behind.
    pub fn from_dir(d: DVec3) -> DVec2 {
        let ll = LonLat::from_dir(d);
        let u = (0.5 + ll.lon / TAU).rem_euclid(1.0);
        let v = 0.5 - ll.lat / PI;
        DVec2::new(u, v)
    }

    /// Normalised `(u, v)` → unit direction.
    pub fn to_dir(uv: DVec2) -> DVec3 {
        LonLat {
            lon: (uv.x - 0.5) * TAU,
            lat: (0.5 - uv.y) * PI,
        }
        .to_dir()
    }

    /// How `(u, v)` changes for a small change `dd` of the unit direction
    /// `d`: the Jacobian of [`from_dir`] at `d` applied to `dd`. Unlike a
    /// difference of two `from_dir` results it has no jump at the seam,
    /// where `u` wraps from 1 to 0, so a renderer can take the footprint of
    /// a pixel from it (the render crate's analytic LOD, DECISIONS D31).
    ///
    /// Towards a pole `u`'s rate grows without bound. It is clamped to
    /// ±0.5, half the ring, the most a step can move along it. A step
    /// whose sideways extent reaches the polar axis (half of it at least
    /// `d`'s distance from the axis) spans the whole ring and gets ±0.5,
    /// even at the pole itself, where the Jacobian says 0; `v`'s rate then
    /// uses that half-step as the distance, so it stays finite.
    pub fn uv_gradient(d: DVec3, dd: DVec3) -> DVec2 {
        let r2 = d.x * d.x + d.z * d.z;
        // The step's eastward part, times the distance from the axis.
        let east = -d.z * dd.x + d.x * dd.z;
        let side2 = dd.x * dd.x + dd.z * dd.z;
        let du = if side2 >= 4.0 * r2 {
            0.5f64.copysign(east)
        } else {
            (east / (r2 * TAU)).clamp(-0.5, 0.5)
        };
        let r = r2.max(side2 / 4.0).max(1e-24).sqrt();
        DVec2::new(du, -dd.y / (PI * r))
    }

    /// Solid angle covered by a texel row at latitude `lat`, relative to a
    /// texel on the equator. Useful for area-weighted image metrics.
    pub fn row_weight(lat: f64) -> f64 {
        lat.clamp(-FRAC_PI_2, FRAC_PI_2).cos()
    }
}

/// Angle between two directions in radians, robust near 0 and π.
pub fn angle_between(a: DVec3, b: DVec3) -> f64 {
    let a = a.normalize();
    let b = b.normalize();
    a.cross(b).length().atan2(a.dot(b))
}

/// Spherical linear interpolation along the shortest arc.
pub fn slerp(a: DQuat, b: DQuat, t: f64) -> DQuat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    a.slerp(b, t)
}

/// Angular distance between two rotations in radians (0..=π).
pub fn rotation_angle(a: DQuat, b: DQuat) -> f64 {
    // atan2 form stays accurate for tiny angles, where acos(dot) does not.
    let rel = a.conjugate() * b;
    2.0 * rel.xyz().length().atan2(rel.w.abs())
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use proptest::prelude::*;

    /// Uniformly distributed unit vectors.
    pub fn unit_dir() -> impl Strategy<Value = DVec3> {
        (-1.0f64..=1.0, 0.0f64..TAU).prop_map(|(z, phi)| {
            let r = (1.0 - z * z).max(0.0).sqrt();
            DVec3::new(r * phi.cos(), r * phi.sin(), z)
        })
    }

    pub fn unit_quat() -> impl Strategy<Value = DQuat> {
        (unit_dir(), -PI..PI).prop_map(|(axis, angle)| DQuat::from_axis_angle(axis, angle))
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn axes_land_where_documented() {
        let c = |d: DVec3| equirect::from_dir(d);
        assert!((c(DVec3::NEG_Z) - DVec2::new(0.5, 0.5)).length() < 1e-12);
        assert!((c(DVec3::X) - DVec2::new(0.75, 0.5)).length() < 1e-12);
        assert!((c(-DVec3::X) - DVec2::new(0.25, 0.5)).length() < 1e-12);
        assert!((c(DVec3::Y).y).abs() < 1e-12);
        assert!((c(-DVec3::Y).y - 1.0).abs() < 1e-12);
        let behind = c(DVec3::Z).x;
        assert!(behind.min(1.0 - behind) < 1e-12);
    }

    /// The change of `(u, v)` between `a` and `b`, with `u`'s wrap at the
    /// seam undone.
    fn uv_step(a: DVec3, b: DVec3) -> DVec2 {
        let d = equirect::from_dir(b) - equirect::from_dir(a);
        DVec2::new((d.x + 0.5).rem_euclid(1.0) - 0.5, d.y)
    }

    /// A central difference of `from_dir` along the unit tangent `t` at
    /// `d`, against `uv_gradient` for the same step.
    fn gradient_error(d: DVec3, t: DVec3, h: f64) -> (DVec2, DVec2) {
        let a = (d - t * h).normalize();
        let b = (d + t * h).normalize();
        (uv_step(a, b), equirect::uv_gradient(d, b - a))
    }

    #[test]
    fn uv_gradient_is_the_derivative_across_the_seam() {
        // Right behind, a step east crosses u = 1 → 0; from_dir jumps by
        // one, the gradient doesn't.
        for lat in [-60.0f64, -10.0, 0.0, 25.0, 70.0] {
            for lon in [179.9999f64, 180.0, -179.9999] {
                let ll = LonLat {
                    lon: lon.to_radians(),
                    lat: lat.to_radians(),
                };
                let d = ll.to_dir();
                let east = DVec3::Y.cross(d).normalize() * -1.0;
                let (fd, g) = gradient_error(d, east, 1e-5);
                assert!(
                    (fd - g).length() < 1e-9 * fd.length(),
                    "{lat} {lon}: {fd} vs {g}"
                );
                // A step east moves u up by its angle over 2π cos(lat).
                let want = 2e-5 / (TAU * ll.lat.cos());
                assert!((g.x - want).abs() < 1e-9 * want, "{lat} {lon}: {g}");
                assert!(g.y.abs() < 1e-12);
            }
        }
    }

    #[test]
    fn uv_gradient_holds_near_the_poles_and_is_bounded_at_them() {
        for lat in [89.0f64, 89.9, 89.99, -89.99, -89.9] {
            let d = LonLat {
                lon: 0.3,
                lat: lat.to_radians(),
            }
            .to_dir();
            let north = (DVec3::Y - d * d.y).normalize();
            let side = north.cross(d).normalize();
            for t in [north, side, (north + side).normalize()] {
                let h = 1e-4 * (90.0 - lat.abs()).to_radians();
                let (fd, g) = gradient_error(d, t, h);
                assert!(
                    (fd - g).length() < 1e-6 * fd.length(),
                    "{lat} {t}: {fd} vs {g}"
                );
            }
        }
        // A step across a pole spans the whole ring, at the pole too,
        // where the Jacobian alone would say u doesn't change; never
        // infinite or NaN.
        for pole in [DVec3::Y, DVec3::NEG_Y] {
            let g = equirect::uv_gradient(pole, DVec3::new(1e-3, -1e-7, 1e-3));
            assert!(g.x.abs() == 0.5 && g.is_finite(), "{g}");
            let near = (pole + DVec3::new(1e-4, 0.0, 0.0)).normalize();
            let g = equirect::uv_gradient(near, DVec3::new(0.0, 0.0, 1e-3));
            assert!(g.x.abs() == 0.5 && g.is_finite(), "{g}");
        }
    }

    proptest! {
        #[test]
        fn uv_gradient_matches_finite_differences(d in unit_dir(), s in unit_dir()) {
            prop_assume!(d.y.abs() < 0.999);
            let t = (s - d * s.dot(d)).normalize();
            prop_assume!(t.is_finite());
            let (fd, g) = gradient_error(d, t, 1e-6);
            prop_assert!((fd - g).length() < 1e-6 * fd.length().max(1e-6), "{fd} vs {g}");
        }

        #[test]
        fn equirect_round_trip(d in unit_dir()) {
            let uv = equirect::from_dir(d);
            prop_assert!((0.0..1.0).contains(&uv.x));
            prop_assert!((0.0..=1.0).contains(&uv.y));
            let back = equirect::to_dir(uv);
            prop_assert!(angle_between(d, back) < 1e-9);
        }

        #[test]
        fn equirect_uv_round_trip(u in 0.0f64..1.0, v in 0.001f64..0.999) {
            let uv = DVec2::new(u, v);
            let back = equirect::from_dir(equirect::to_dir(uv));
            // u wraps at the seam.
            let du = (back.x - u).abs();
            prop_assert!(du.min(1.0 - du) < 1e-9);
            prop_assert!((back.y - v).abs() < 1e-9);
        }

        #[test]
        fn slerp_is_continuous_and_shortest(a in unit_quat(), b in unit_quat()) {
            let total = rotation_angle(a, b);
            let steps = 64;
            let mut prev = a;
            for i in 1..=steps {
                let q = slerp(a, b, i as f64 / steps as f64);
                // Equal angular steps along the shortest arc.
                prop_assert!((rotation_angle(prev, q) - total / steps as f64).abs() < 1e-6);
                prev = q;
            }
            prop_assert!(rotation_angle(prev, b) < 1e-9);
        }
    }
}
