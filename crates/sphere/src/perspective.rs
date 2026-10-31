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

/// A lens's projection without its field of view: one projection, or a
/// morph part way from one to another (D33's slider). A projection key
/// holds one. Written as the projection's name, or as
/// `{"from": "rectilinear", "to": "fisheye", "morph": 0.25}`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(
    untagged,
    expecting = "a projection name, or a morph {\"from\", \"to\", \"morph\"}"
)]
pub enum LensShape {
    Pure(Projection),
    Morph(Morph),
}

/// A morph from one projection to another: 0 is `from`, 1 is `to`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Morph {
    pub from: Projection,
    pub to: Projection,
    pub morph: f64,
}

impl LensShape {
    /// The length of [`LensShape::perimeter`]'s loop.
    pub const PERIMETER: f64 = 3.0;

    /// Where the shape is on a loop through the three projections, each
    /// next to the other two: rectilinear at 0, fisheye at 1,
    /// stereographic at 2, and back to rectilinear at 3, a morph between
    /// two of them on the edge between. A projection track is a scalar
    /// track of this, the shortest way round, so keys of any shapes
    /// interpolate through morphs of two projections at a time, as the
    /// renderer draws them.
    pub fn perimeter(self) -> f64 {
        let at = |p: Projection| match p {
            Projection::Rectilinear => 0.0,
            Projection::Fisheye => 1.0,
            Projection::Stereographic => 2.0,
        };
        match self {
            LensShape::Pure(p) => at(p),
            LensShape::Morph(m) => {
                let (a, b) = (at(m.from), at(m.to));
                let m = m.morph.clamp(0.0, 1.0);
                // From a to b the short way round, which is one edge.
                let step = crate::spline::wrap_period(b - a, Self::PERIMETER);
                (a + step * m).rem_euclid(Self::PERIMETER)
            }
        }
    }

    /// The shape at `c` on [`LensShape::perimeter`]'s loop.
    pub fn from_perimeter(c: f64) -> LensShape {
        const ORDER: [Projection; 3] = [
            Projection::Rectilinear,
            Projection::Fisheye,
            Projection::Stereographic,
        ];
        let c = c.rem_euclid(Self::PERIMETER);
        let i = (c.floor() as usize).min(2);
        let morph = c - i as f64;
        if morph <= 0.0 {
            return LensShape::Pure(ORDER[i]);
        }
        LensShape::Morph(Morph {
            from: ORDER[i],
            to: ORDER[(i + 1) % 3],
            morph,
        })
    }

    /// A lens of this shape.
    pub fn lens(self, hfov: f64) -> Lens {
        match self {
            LensShape::Pure(p) => Lens::new(p, hfov),
            LensShape::Morph(m) => Lens {
                from: m.from,
                to: m.to,
                morph: m.morph,
                hfov,
            },
        }
    }
}

impl Lens {
    /// The lens's shape, as a projection key holds it: a morph at 0 or 1,
    /// or between a projection and itself, is that projection.
    pub fn shape(&self) -> LensShape {
        if self.from == self.to || self.morph <= 0.0 {
            LensShape::Pure(self.from)
        } else if self.morph >= 1.0 {
            LensShape::Pure(self.to)
        } else {
            LensShape::Morph(Morph {
                from: self.from,
                to: self.to,
                morph: self.morph,
            })
        }
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

    #[test]
    fn shapes_sit_on_a_loop_of_the_three_projections() {
        use Projection::*;
        let morph = |from, to, morph| LensShape::Morph(Morph { from, to, morph });
        assert_eq!(LensShape::Pure(Rectilinear).perimeter(), 0.0);
        assert_eq!(LensShape::Pure(Stereographic).perimeter(), 2.0);
        assert_eq!(morph(Rectilinear, Fisheye, 0.25).perimeter(), 0.25);
        assert_eq!(morph(Fisheye, Rectilinear, 0.25).perimeter(), 0.75);
        // Stereographic and rectilinear are next to each other too.
        assert_eq!(morph(Rectilinear, Stereographic, 0.25).perimeter(), 2.75);
        assert_eq!(morph(Stereographic, Rectilinear, 0.25).perimeter(), 2.25);
        assert_eq!(
            LensShape::from_perimeter(2.25),
            morph(Stereographic, Rectilinear, 0.25)
        );
        assert_eq!(LensShape::from_perimeter(3.0), LensShape::Pure(Rectilinear));
        assert_eq!(
            LensShape::from_perimeter(-1.0),
            LensShape::Pure(Stereographic)
        );
    }

    #[test]
    fn a_shape_reads_as_a_name_or_a_morph() {
        let pure: LensShape = serde_json::from_str("\"fisheye\"").unwrap();
        assert_eq!(pure, LensShape::Pure(Projection::Fisheye));
        let m: LensShape =
            serde_json::from_str(r#"{"from": "fisheye", "to": "stereographic", "morph": 0.5}"#)
                .unwrap();
        assert_eq!(m.perimeter(), 1.5);
        assert_eq!(serde_json::to_string(&pure).unwrap(), "\"fisheye\"");
        let e = serde_json::from_str::<LensShape>(r#"{"from": "fisheye", "too": 1}"#).unwrap_err();
        assert!(
            e.to_string().contains("a projection name, or a morph"),
            "{e}"
        );
    }

    proptest! {
        #[test]
        fn shapes_and_lenses_round_trip(from in proj(), to in proj(), morph in 0.0f64..1.0) {
            let lens = Lens { from, to, morph, hfov: 1.0 };
            let shape = lens.shape();
            let back = LensShape::from_perimeter(shape.perimeter()).lens(1.0);
            // The same blend of projections, however it is written.
            for rho in [0.1, 0.5, 0.9] {
                prop_assert!((back.theta(rho) - lens.theta(rho)).abs() < 1e-12);
            }
        }
    }
}
