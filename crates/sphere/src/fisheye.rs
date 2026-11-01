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
//!
//! The stitching models, [`Unified`] and [`KbLens`] (M3 plan 6.1,
//! DECISIONS D63), work in another frame and in pixels: Mei's and
//! OpenCV's lens frame, +x right in the image, +y down and +z along the
//! optical axis, which F = diag(1, −1, −1) maps the frame above to (det
//! +1). [`Unified`] sits beside [`Model`] rather than in it, because Mei
//! distorts the plane point, not θ. Every reading of a camera's terms
//! maps onto one [`Canonical`] distortion, the only form the shader
//! evaluates, and [`V6Reading`] is the generic space V6's d5 to d12 are
//! read through, named after no source.

use glam::{DVec2, DVec3};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// `r = θ`. The usual model for 360 camera lenses.
    Equidistant,
    /// `r = 2 sin(θ/2)`.
    Equisolid,
    /// `r = 2 tan(θ/2)`.
    Stereographic,
    /// `r = sin θ`. Only covers θ_d ≤ 90°: past that, `sin` folds back and
    /// [`Lens::theta`] gives the mirrored angle.
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

/// The unified (Mei) model with one canonical distortion (M3 plan 6.1).
/// A unit direction (x, y, z) in the lens frame (+z along the axis, +y
/// down) lands on the plane at m = (x, y)/(z + ξ), is distorted by
/// [`Canonical::distort`] to (xd, yd), and reaches the pixel
/// (fx xd + cx, fy yd + cy), in whatever pixels the focal and centre are
/// given in: a slot's, or the calibration canvas's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Unified {
    /// Mei's ξ, how far behind the unit sphere's centre the projection
    /// centre sits (2 on the X5).
    pub xi: f64,
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    pub dist: Canonical,
}

impl Unified {
    /// Lens-frame direction to pixel, in closed form. `None` past
    /// [`Unified::limit_theta`], where the model folds or its denominator
    /// vanishes.
    #[expect(unused_variables, reason = "M3.3 unit (a) writes the body")]
    pub fn project(&self, p: DVec3) -> Option<DVec2> {
        todo!()
    }

    /// Pixel to lens-frame unit direction, on the CPU only (6.1): one pass
    /// strips the tangential and prism terms at the current estimate, then
    /// a Newton on m R(m²) = r_d bracketed on [0, m(`limit_theta`)], and
    /// the passes repeat until the full forward's residual is under 1e-12
    /// in plane units, at most 20 times; then the closed-form lift.
    /// `None` past the limit, when the bracket fails, or when 20 passes
    /// leave the residual above 1e-12.
    #[expect(unused_variables, reason = "M3.3 unit (a) writes the body")]
    pub fn unproject(&self, px: DVec2) -> Option<DVec3> {
        todo!()
    }

    /// The largest θ, in radians, at which r(θ) still rises: acos(−1/ξ)
    /// for ξ > 1 (120° at ξ = 2) and acos(−ξ) for ξ ≤ 1 (90° at ξ = 0),
    /// or earlier where the polynomial folds, found by a scan of dr/dθ in
    /// 0.05° steps. `Rig::build` refuses a θ_max beyond it.
    pub fn limit_theta(&self) -> f64 {
        todo!()
    }
}

/// The one distortion form that every reading of the camera's terms maps
/// onto (6.1). Of the plane point m, with r² = |m|²:
///
/// ```text
/// R  = 1 + k1 r² + k2 r⁴ + k3 r⁶ + k4 r⁸ + k5 r¹⁰
/// P1 = p1 + p1' r²,  P2 = p2 + p2' r²
/// xd = mx R + 2 P1 mx my + P2 (r² + 2mx²) + s1 r² + s2 r⁴
/// yd = my R + 2 P2 mx my + P1 (r² + 2my²) + s3 r² + s4 r⁴
/// ```
///
/// The shader evaluates only this form (10.3), so every reading the
/// Stitch pane offers renders on the GPU as the CPU and the fit see it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Canonical {
    /// Radial: the coefficients of r², r⁴, r⁶, r⁸ and r¹⁰.
    pub k: [f64; 5],
    /// Brown's tangential p1 and p2.
    pub p: [f64; 2],
    /// Their r² extensions, p1' and p2'.
    pub pe: [f64; 2],
    /// Thin prism: x gains s1 r² + s2 r⁴ and y gains s3 r² + s4 r⁴.
    pub s: [f64; 4],
}

impl Canonical {
    /// No distortion.
    pub const NONE: Canonical = Canonical {
        k: [0.0; 5],
        p: [0.0; 2],
        pe: [0.0; 2],
        s: [0.0; 4],
    };

    /// V3's terms in token order, k1 k2 k3 and p1 p2: radial k1 to k3 and
    /// Brown's pair, with k4, k5, the extensions and the prism 0.
    #[expect(unused_variables, reason = "M3.3 unit (a) writes the body")]
    pub fn v3(k: [f64; 3], p: [f64; 2]) -> Canonical {
        todo!()
    }

    /// V6's d0 to d12 read through `reading`, one of the 21
    /// ([`V6Reading::is_valid`]): d0 to d4 radial, d5 to d12 as the
    /// reading says, and 0 in every field it doesn't read.
    #[expect(unused_variables, reason = "M3.3 unit (a) writes the body")]
    pub fn v6(d: [f64; 13], reading: V6Reading) -> Canonical {
        todo!()
    }

    /// The distorted plane point (xd, yd) of the plane point `m`.
    #[expect(unused_variables, reason = "M3.3 unit (a) writes the body")]
    pub fn distort(&self, m: DVec2) -> DVec2 {
        todo!()
    }
}

/// What V6's d5 to d12 mean (6.1): a small generic space of 21 values,
/// as the Euler angles are read (`rig::EulerConv`), so that no source's
/// reading is copied or favoured and none is named after one. d0 to d4
/// are always the radial terms. Both fields `None` is
/// [`V6Reading::RADIAL_ONLY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct V6Reading {
    pub tangential: Option<Tangential>,
    pub prism: Option<Prism>,
}

impl V6Reading {
    /// d0 to d4 alone.
    pub const RADIAL_ONLY: V6Reading = V6Reading {
        tangential: None,
        prism: None,
    };

    /// The 21 values: 5 tangential readings (none, or either pair order
    /// with or without the extension) by 5 prism readings (none, or either
    /// start in either order), less the 4 that would read d7 and d8 twice.
    pub fn all() -> impl Iterator<Item = V6Reading> {
        let tangential = std::iter::once(None).chain(
            PairOrder::all()
                .flat_map(|pair| [false, true].map(|extension| Tangential { pair, extension }))
                .map(Some),
        );
        tangential
            .flat_map(|tangential| {
                std::iter::once(None)
                    .chain(Prism::all().map(Some))
                    .map(move |prism| V6Reading { tangential, prism })
            })
            .filter(V6Reading::is_valid)
    }

    /// Whether no term is read twice, as an extension (d7 and d8) and a
    /// prism at d7 would.
    pub fn is_valid(&self) -> bool {
        !matches!(
            (self.tangential, self.prism),
            (
                Some(Tangential {
                    extension: true,
                    ..
                }),
                Some(Prism {
                    at: PrismAt::D7,
                    ..
                })
            )
        )
    }
}

/// V6's tangential pair, and whether it has an r² extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tangential {
    pub pair: PairOrder,
    /// p1' and p2' from d7 and d8, in the pair's order: (d7, d8) after
    /// `D5D6`, (d8, d7) after `D6D5`.
    pub extension: bool,
}

/// Which of d5 and d6 is p1.
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

/// V6's four thin-prism terms: where they start and how they give s1 to
/// s4.
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

/// Where the four prism terms start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrismAt {
    /// d7 to d10, only without a tangential extension, which reads d7
    /// and d8.
    D7,
    /// d9 to d12.
    D9,
}

impl PrismAt {
    pub fn all() -> impl Iterator<Item = PrismAt> {
        [PrismAt::D7, PrismAt::D9].into_iter()
    }
}

/// How the four prism terms, first to fourth, give s1 to s4 (x gains
/// s1 r² + s2 r⁴, y gains s3 r² + s4 r⁴).
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

/// A Kannala–Brandt lens on one of [`Model`]'s bases (6.1), for a camera
/// that isn't Mei (DJI, dji.md §3.3) and the no-calibration fallback. In
/// [`Unified`]'s lens frame and pixels: a direction θ off the +z axis and
/// at azimuth φ has θ_d = θ (1 + k1 θ² + k2 θ⁴ + k3 θ⁶ + k4 θ⁸ + k5 θ¹⁰),
/// then the plane point m = base(θ_d) (cos φ, sin φ), as [`Lens::radius`]
/// applies the base after the polynomial; the tangential and prism terms
/// act on m as [`Canonical`]'s do, and the pixel is (fx xd + cx,
/// fy yd + cy).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KbLens {
    pub base: Model,
    /// The polynomial in θ: k1 to k5 (DJI needs k5).
    pub k: [f64; 5],
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    /// Brown's tangential p1 and p2, on the plane point.
    pub p: [f64; 2],
    /// Their r² extensions, p1' and p2'.
    pub pe: [f64; 2],
    /// Thin prism, s1 to s4, as [`Canonical::s`].
    pub s: [f64; 4],
}

impl KbLens {
    /// Lens-frame direction to pixel. `None` past
    /// [`KbLens::limit_theta`].
    #[expect(unused_variables, reason = "M3.3 unit (a) writes the body")]
    pub fn project(&self, p: DVec3) -> Option<DVec2> {
        todo!()
    }

    /// Pixel to lens-frame unit direction. `None` past the limit or where
    /// the inverse doesn't converge.
    #[expect(unused_variables, reason = "M3.3 unit (a) writes the body")]
    pub fn unproject(&self, px: DVec2) -> Option<DVec3> {
        todo!()
    }

    /// The largest θ, in radians, at which the radius still rises, the
    /// base's own fold included (an orthographic base folds at θ_d = 90°).
    pub fn limit_theta(&self) -> f64 {
        todo!()
    }
}

/// A lens of a stitched pair (6.1): Mei, or Kannala–Brandt on a base. The
/// shader carries which one per rig (10.3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LensModel {
    Mei(Unified),
    Kb(KbLens),
}

impl LensModel {
    /// Lens-frame direction to pixel.
    pub fn project(&self, p: DVec3) -> Option<DVec2> {
        match self {
            LensModel::Mei(m) => m.project(p),
            LensModel::Kb(k) => k.project(p),
        }
    }

    /// Pixel to lens-frame unit direction.
    pub fn unproject(&self, px: DVec2) -> Option<DVec3> {
        match self {
            LensModel::Mei(m) => m.unproject(px),
            LensModel::Kb(k) => k.unproject(px),
        }
    }

    /// The largest θ at which the radius still rises.
    pub fn limit_theta(&self) -> f64 {
        match self {
            LensModel::Mei(m) => m.limit_theta(),
            LensModel::Kb(k) => k.limit_theta(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    use crate::angle_between;
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
        fn round_trip(m in model(), along in 0.0f64..1.0, phi in -PI..PI,
                      k1 in -0.05f64..0.05, k2 in -0.01f64..0.01) {
            let lens = Lens { model: m, k: [k1, k2, 0.0, 0.0] };
            // Real 360 lenses cover a little over 90°, i.e. θ up to ~100°.
            // The angle is drawn inside that range rather than filtered, so
            // high case counts don't run into proptest's reject limit.
            let max = match m { Model::Orthographic => 1.4, _ => 1.8 };
            let theta = along * max;
            // An orthographic lens is one-to-one only up to θ_d = 90°, which
            // distortion can reach before θ does.
            prop_assume!(m != Model::Orthographic || lens.distort(theta) <= FRAC_PI_2);
            let (st, ct) = theta.sin_cos();
            let d = DVec3::new(st * phi.cos(), st * phi.sin(), -ct);
            let back = lens.unproject(lens.project(d));
            prop_assert!(back.is_some());
            prop_assert!(angle_between(back.unwrap_or_default(), d) < 1e-7);
        }
    }
}
