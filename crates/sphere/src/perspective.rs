//! Output projections for a virtual camera looking down −Z.
//!
//! A point on the output image is described by `p`, with `p.x` running from
//! −1 at the left edge to +1 at the right edge and `p.y` scaled by the same
//! factor (so `p.y = ±1/aspect` at the top and bottom edges, +y up). Its
//! radius `ρ = |p|` maps to `θ`, the angle off the view axis. The horizontal
//! field of view fixes the scale: `θ(1) = hfov / 2`.
//!
//! Every projection is radially symmetric, so morphing between two of them
//! is a blend of their `θ(ρ)` curves at the same field of view. Because each
//! curve is increasing in `ρ`, so is any blend, and the morph is smooth.

use glam::{DVec2, DVec3};
use std::f64::consts::PI;

/// The narrowest horizontal field of view a view may have, in degrees:
/// the viewer's zoom stops here, and a project's FOV keys may not go
/// below it.
pub const MIN_HFOV_DEG: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    /// Pinhole camera; straight lines stay straight. hfov < 180°.
    Rectilinear,
    /// Equidistant fisheye, `θ ∝ ρ`. Any hfov up to 360°.
    Fisheye,
    /// Stereographic ("tiny planet" when looking straight down with a wide
    /// field of view). hfov < 360°.
    Stereographic,
}

impl Projection {
    /// Largest usable horizontal field of view, in radians.
    pub fn max_hfov(self) -> f64 {
        match self {
            Projection::Rectilinear => PI * (170.0 / 180.0),
            Projection::Fisheye => 2.0 * PI,
            Projection::Stereographic => PI * (340.0 / 180.0),
        }
    }

    /// Angle off axis for normalised radius `rho`, given the half field of
    /// view `half`. Callers clamp `half` to `max_hfov() / 2`.
    pub fn theta(self, rho: f64, half: f64) -> f64 {
        match self {
            Projection::Rectilinear => (rho * half.tan()).atan(),
            Projection::Fisheye => rho * half,
            Projection::Stereographic => 2.0 * (rho * (half / 2.0).tan()).atan(),
        }
    }

    /// Inverse of [`Projection::theta`]. `None` when `theta` is not visible
    /// in this projection (behind a rectilinear camera, or at the antipode).
    pub fn rho(self, theta: f64, half: f64) -> Option<f64> {
        match self {
            Projection::Rectilinear => (theta < PI / 2.0).then(|| theta.tan() / half.tan()),
            Projection::Fisheye => Some(theta / half),
            Projection::Stereographic => {
                (theta < PI - 1e-9).then(|| (theta / 2.0).tan() / (half / 2.0).tan())
            }
        }
    }
}

/// A virtual camera's lens: a projection, optionally morphing towards a
/// second one, and a horizontal field of view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lens {
    pub from: Projection,
    pub to: Projection,
    /// 0 = `from`, 1 = `to`.
    pub morph: f64,
    /// Horizontal field of view in radians.
    pub hfov: f64,
}

impl Lens {
    pub fn new(p: Projection, hfov: f64) -> Self {
        Lens {
            from: p,
            to: p,
            morph: 0.0,
            hfov,
        }
    }

    /// Half of the field of view `p` is drawn with: `hfov`, capped at
    /// `p`'s [`Projection::max_hfov`]. The renderer passes these to its
    /// shader.
    pub fn half(&self, p: Projection) -> f64 {
        self.hfov.min(p.max_hfov()) / 2.0
    }

    /// Angle off axis at normalised radius `rho`.
    pub fn theta(&self, rho: f64) -> f64 {
        let a = self.from.theta(rho, self.half(self.from));
        if self.morph <= 0.0 || self.from == self.to {
            return a;
        }
        let b = self.to.theta(rho, self.half(self.to));
        a + (b - a) * self.morph.clamp(0.0, 1.0)
    }

    /// Output image point → view-space direction.
    pub fn unproject(&self, p: DVec2) -> DVec3 {
        let rho = p.length();
        if rho < 1e-15 {
            return DVec3::NEG_Z;
        }
        let theta = self.theta(rho);
        let (st, ct) = theta.sin_cos();
        let a = p / rho;
        DVec3::new(st * a.x, st * a.y, -ct)
    }

    /// Normalised radius at which this lens shows the angle `theta` off
    /// axis, for any morph: the blended θ(ρ) increases with ρ, so
    /// bisection finds it. `None` where no radius reaches `theta` (behind
    /// a rectilinear lens, or past where a blend's curve levels off).
    pub fn rho(&self, theta: f64) -> Option<f64> {
        if theta <= 0.0 {
            return Some(0.0);
        }
        let mut hi = 1.0;
        while self.theta(hi) < theta {
            hi *= 2.0;
            if hi > 1e12 {
                return None;
            }
        }
        let mut lo = 0.0;
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if self.theta(mid) < theta {
                lo = mid;
            } else {
                hi = mid;
            }
            if hi - lo <= f64::EPSILON * hi {
                break;
            }
        }
        Some(0.5 * (lo + hi))
    }

    /// View-space direction → output image point for any morph, the
    /// inverse of [`Lens::unproject`] (by [`Lens::rho`]). `None` where the
    /// lens can't show `d`.
    pub fn project_morph(&self, d: DVec3) -> Option<DVec2> {
        let d = d.normalize();
        let lateral = DVec2::new(d.x, d.y);
        let len = lateral.length();
        let rho = self.rho(len.atan2(-d.z))?;
        Some(if len < 1e-15 {
            DVec2::ZERO
        } else {
            lateral / len * rho
        })
    }

    /// View-space direction → output image point, for pure (non-morphing)
    /// lenses. Used to draw overlays and to test round trips.
    pub fn project(&self, d: DVec3) -> Option<DVec2> {
        let d = d.normalize();
        let lateral = DVec2::new(d.x, d.y);
        let theta = lateral.length().atan2(-d.z);
        let rho = self.from.rho(theta, self.half(self.from))?;
        let len = lateral.length();
        Some(if len < 1e-15 {
            DVec2::ZERO
        } else {
            lateral / len * rho
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::angle_between;
    use proptest::prelude::*;

    fn proj() -> impl Strategy<Value = Projection> {
        prop_oneof![
            Just(Projection::Rectilinear),
            Just(Projection::Fisheye),
            Just(Projection::Stereographic),
        ]
    }

    #[test]
    fn edge_of_frame_is_half_fov() {
        for p in [
            Projection::Rectilinear,
            Projection::Fisheye,
            Projection::Stereographic,
        ] {
            let lens = Lens::new(p, 100f64.to_radians());
            let d = lens.unproject(DVec2::new(1.0, 0.0));
            assert!((angle_between(d, DVec3::NEG_Z) - 50f64.to_radians()).abs() < 1e-12);
            assert!(d.x > 0.0);
        }
    }

    proptest! {
        #[test]
        fn round_trip(p in proj(), fov_deg in 10.0f64..330.0,
                      x in -1.0f64..1.0, y in -1.0f64..1.0) {
            let lens = Lens::new(p, fov_deg.to_radians());
            let pt = DVec2::new(x, y);
            // Past the antipode a wide fisheye wraps and can't be inverted.
            prop_assume!(lens.theta(pt.length()) < PI - 1e-6);
            let d = lens.unproject(pt);
            let back = lens.project(d);
            prop_assert!(back.is_some());
            prop_assert!((back.unwrap_or_default() - pt).length() < 1e-7);
        }

        #[test]
        fn morphs_round_trip(a in proj(), b in proj(), t in 0.0f64..=1.0,
                             fov_deg in 10.0f64..330.0, x in -1.0f64..1.0, y in -1.0f64..1.0) {
            let lens = Lens { from: a, to: b, morph: t, hfov: fov_deg.to_radians() };
            let pt = DVec2::new(x, y);
            prop_assume!(lens.theta(pt.length()) < PI - 1e-6);
            let back = lens.project_morph(lens.unproject(pt));
            prop_assert!(back.is_some());
            prop_assert!((back.unwrap_or_default() - pt).length() < 1e-7, "{back:?}");
        }

        #[test]
        fn project_morph_is_project_for_a_pure_lens(p in proj(), fov_deg in 10.0f64..330.0,
                                                    d in crate::testutil::unit_dir()) {
            let lens = Lens::new(p, fov_deg.to_radians());
            match (lens.project(d), lens.project_morph(d)) {
                (Some(a), Some(b)) => prop_assert!((a - b).length() < 1e-9 * a.length().max(1.0)),
                // Only where the closed form says no, or right at its limit.
                (None, None) => {}
                (a, b) => {
                    let theta = DVec2::new(d.x, d.y).length().atan2(-d.z);
                    prop_assert!(theta > PI / 2.0 - 1e-6, "{a:?} vs {b:?} at {theta}");
                }
            }
        }

        #[test]
        fn morph_is_monotonic_and_bounded(a in proj(), b in proj(), t in 0.0f64..=1.0,
                                          fov_deg in 20.0f64..160.0) {
            let lens = Lens { from: a, to: b, morph: t, hfov: fov_deg.to_radians() };
            let mut prev = 0.0;
            for i in 1..=100 {
                let th = lens.theta(i as f64 / 50.0);
                prop_assert!(th > prev);
                prev = th;
            }
            let edge = lens.theta(1.0);
            prop_assert!((edge - fov_deg.to_radians() / 2.0).abs() < 1e-9);
        }
    }
}
