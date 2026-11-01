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
//! optical axis, which [`F`] = diag(1, −1, −1) maps the frame above to
//! (det +1). [`Unified`] sits beside [`Model`] rather than in it, because
//! Mei distorts the plane point, not θ. Every reading of a camera's terms
//! maps onto one [`Canonical`] distortion, the only form the shader
//! evaluates, and [`V6Reading`] is the generic space V6's d5 to d12 are
//! read through, named after no source.

use std::f64::consts::PI;

use glam::{DQuat, DVec2, DVec3};

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

/// F = diag(1, −1, −1) (M3 plan 6.1) as a rotation: the 180° turn about
/// +x (det +1) that takes a direction in [`Lens`]'s frame, looking down
/// −Z with +y up, to the same direction in [`Unified`]'s and
/// [`KbLens`]'s, +z along the axis and +y down. It is its own inverse, so
/// it takes the second frame back to the first as well.
pub const F: DQuat = DQuat::from_xyzw(1.0, 0.0, 0.0, 0.0);

/// The step of the scans for where a lens's radius stops rising
/// ([`Unified::limit_theta`], [`KbLens::limit_theta`]): 0.05°, in
/// radians (6.1).
const SCAN_STEP: f64 = 0.05 * PI / 180.0;

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

/// Where [`Unified::invert`] left a pixel: the plane point its last pass
/// reached, how many passes it ran, and the full forward's residual
/// there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Inversion {
    /// The undistorted plane point, (x, y)/(z + ξ) of the direction.
    pub m: DVec2,
    /// The passes run.
    pub passes: u32,
    /// How far [`Canonical::distort`] of `m` lands from the pixel's
    /// distorted plane point, in normalised plane units.
    pub residual: f64,
}

impl Unified {
    /// The most passes [`Unified::unproject`] runs (6.1).
    pub const PASSES: u32 = 20;

    /// The full forward's residual, in normalised plane units, under which
    /// the inverse stops (6.1): about 4e-9 px at the X5's focal.
    pub const RESIDUAL: f64 = 1e-12;

    /// Lens-frame direction to pixel, in closed form. `None` for a zero
    /// or non-finite `p`, and past the model's own end: the fold at
    /// θ = acos(−1/ξ) for ξ > 1, or where the denominator z + ξ vanishes
    /// for ξ ≤ 1. The polynomial can fold earlier, at
    /// [`Unified::limit_theta`], which this doesn't scan for: past that
    /// fold the pixel is still the formula's, no longer one to one, and
    /// `Rig::build` keeps θ_max inside it.
    pub fn project(&self, p: DVec3) -> Option<DVec2> {
        let d = p.normalize_or_zero();
        if d == DVec3::ZERO || !self.images(d.z) {
            return None;
        }
        let m = DVec2::new(d.x, d.y) / (d.z + self.xi);
        let md = self.dist.distort(m);
        Some(DVec2::new(
            self.fx * md.x + self.cx,
            self.fy * md.y + self.cy,
        ))
    }

    /// Pixel to lens-frame unit direction, on the CPU only (6.1): one pass
    /// strips the tangential and prism terms at the current estimate, then
    /// a Newton on m R(m²) = r_d bracketed on [0, m(`limit_theta`)], and
    /// the passes repeat until the full forward's residual is under 1e-12
    /// in plane units, at most 20 times; then the closed-form lift.
    /// `None` past the limit, where the bracket holds no plane point that
    /// meets the residual, and whenever 20 passes leave the residual above
    /// 1e-12. Pat's V3 lenses take at most 6 passes up to 119°, and V6's 7
    /// read through every term, or 9 through the reading that is slowest.
    ///
    /// Each call finds [`Unified::limit_theta`] afresh. Pat's lenses at
    /// ξ = 2 skip its scan, but others (ξ ≤ 1 with a negative term, or
    /// Pat's at a ξ a few hundredths under 2) scan up to 3600 steps a
    /// call, so a caller inverting many pixels of one lens finds the limit
    /// once and calls [`Unified::unproject_within`].
    pub fn unproject(&self, px: DVec2) -> Option<DVec3> {
        self.unproject_within(px, self.limit_theta())
    }

    /// [`Unified::unproject`] with the limit given, found once for the
    /// lens: `limit` is [`Unified::limit_theta`]'s value or any smaller θ
    /// in radians, and the inverse is `None` past it. Past `limit_theta`
    /// the radial curve no longer rises, so a larger `limit` gives the
    /// bracket no single answer. A `limit` past the model's own end, +∞
    /// among them, is the end. `None` for a NaN or negative `limit`.
    pub fn unproject_within(&self, px: DVec2, limit: f64) -> Option<DVec3> {
        let inv = self.invert_within(px, Self::PASSES, limit)?;
        (inv.residual < Self::RESIDUAL).then(|| self.lift(inv.m))
    }

    /// The inverse's plane point for the pixel `px` after at most
    /// `passes` passes, stopping early once the full forward's residual is
    /// under [`Unified::RESIDUAL`]: what [`Unified::unproject`] lifts, with
    /// the passes it took and the residual it left. `None` for a
    /// non-finite pixel. A pass whose radial target lies past
    /// r(`limit_theta`), where its bracket fails, takes the bracket's top,
    /// so a pixel past the limit ends with its residual above
    /// [`Unified::RESIDUAL`].
    pub fn invert(&self, px: DVec2, passes: u32) -> Option<Inversion> {
        self.invert_within(px, passes, self.limit_theta())
    }

    /// [`Unified::invert`] with its bracket ending at θ = `limit`, as
    /// [`Unified::unproject_within`] takes it.
    fn invert_within(&self, px: DVec2, passes: u32, limit: f64) -> Option<Inversion> {
        // The method, stripping the tangential terms at the estimate and
        // solving the radial curve by a bracketed Newton, follows Gyroflow's
        // `src/core/stabilization/distortion_models/insta360.rs`
        // (GPL-3.0-or-later, as Spherecut is), written here from the M3
        // plan's description (6.1). Where Gyroflow stops after three
        // passes, these repeat until the residual is under 1e-12: three
        // leave 4e-7 to 5e-6 rad on Pat's X5 at θ up to 100°.
        let target = DVec2::new((px.x - self.cx) / self.fx, (px.y - self.cy) / self.fy);
        if !target.is_finite() || limit.is_nan() || limit < 0.0 {
            return None;
        }
        // Past the end the plane radius turns back for ξ > 1, and the sine
        // of ∞ is NaN, either of which would leave the bracket's top wrong.
        let limit = limit.min(self.end_theta());
        let top = self.plane_at(limit);
        let reach = if top.is_finite() {
            self.dist.radial(top)
        } else {
            f64::INFINITY
        };
        let curve = |rho: f64| (self.dist.radial(rho), self.dist.slope(rho * rho));
        let mut m = DVec2::ZERO;
        let mut inv = Inversion {
            m,
            passes: 0,
            residual: (self.dist.distort(m) - target).length(),
        };
        for pass in 1..=passes {
            let want = target - self.dist.rest(m);
            let rd = want.length();
            if rd.is_nan() {
                return None;
            }
            // A pass whose radial target lies past the curve's reach takes
            // the bracket's top, and the passes go on: near the fold an
            // estimate a hair off overshoots, while a pixel truly past the
            // limit never meets the residual.
            let rho = if rd >= reach {
                top
            } else {
                let hi = if top.is_finite() {
                    top
                } else {
                    past(curve, rd)
                };
                solve_rising(curve, rd, 0.0, hi, m.length())
            };
            m = if rd > 0.0 {
                want * (rho / rd)
            } else {
                DVec2::ZERO
            };
            inv = Inversion {
                m,
                passes: pass,
                residual: (self.dist.distort(m) - target).length(),
            };
            if inv.residual < Self::RESIDUAL {
                break;
            }
        }
        Some(inv)
    }

    /// The unit direction whose undistorted plane point is `m`, in closed
    /// form: η = (ξ + √(1 + (1 − ξ²)ρ))/(ρ + 1) with ρ = |m|², and the
    /// direction (η mx, η my, η − ξ), on the near side of the fold.
    pub fn lift(&self, m: DVec2) -> DVec3 {
        let rho = m.length_squared();
        let root = (1.0 + (1.0 - self.xi * self.xi) * rho).max(0.0).sqrt();
        let eta = (self.xi + root) / (rho + 1.0);
        DVec3::new(eta * m.x, eta * m.y, eta - self.xi).normalize()
    }

    /// The largest θ, in radians, at which r(θ) still rises: acos(−1/ξ)
    /// for ξ > 1 (120° at ξ = 2) and acos(−ξ) for ξ ≤ 1 (90° at ξ = 0),
    /// or earlier where the polynomial folds, found by a scan of dr/dθ in
    /// 0.05° steps and bisected within the step where it stops rising.
    /// The scan looks at its last step too, from its last sample to the
    /// end, which ends a hair (1e-12 of it) inside the end for ξ ≤ 1,
    /// where m has no bound. r(θ) is the radial curve m R(m²), which the
    /// tangential and prism terms don't change. `Rig::build` refuses a
    /// θ_max beyond it.
    pub fn limit_theta(&self) -> f64 {
        let end = self.end_theta();
        let xi = self.xi;
        // The plane radius at the end is 1/√(ξ² − 1) for ξ > 1 and
        // unbounded otherwise. When the slope can't reach 0 below it, even
        // with every positive term dropped, there is nothing to scan for:
        // so for Pat's V3 and V6 at ξ = 2.
        let r2_end = if xi > 1.0 {
            1.0 / (xi * xi - 1.0)
        } else {
            f64::INFINITY
        };
        if self.dist.slope_floor(r2_end) > 0.0 {
            return end;
        }
        // Below the end, dm/dθ = (1 + ξ cos θ)/(cos θ + ξ)² > 0, so r(θ)
        // rises where the radial curve's slope in m does.
        let rises = |theta: f64| {
            let (s, c) = theta.sin_cos();
            let m = s / (c + xi);
            self.dist.slope(m * m) > 0.0
        };
        // Each step's sine and cosine come from the last by the angle-sum
        // rule, which drifts by far less than a step over the scan, so
        // that the scan costs no trigonometry; the step where the curve
        // stops rising is bisected with the functions themselves.
        let (sd, cd) = SCAN_STEP.sin_cos();
        let (mut s, mut c) = (0.0, 1.0);
        // The scan runs up to the end, or a hair inside it for ξ ≤ 1,
        // where z + ξ vanishes.
        let tail = if xi > 1.0 { end } else { end * (1.0 - 1e-12) };
        let steps = (tail / SCAN_STEP).ceil() as u32;
        for k in 1..steps {
            (s, c) = (s * cd + c * sd, c * cd - s * sd);
            let m = s / (c + xi);
            if self.dist.slope(m * m) <= 0.0 {
                return last_rising(
                    rises,
                    f64::from(k - 1) * SCAN_STEP,
                    f64::from(k) * SCAN_STEP,
                );
            }
        }
        // The last step, from the loop's last sample to the tail, a whole
        // step or less. None is left when the tail is a whole number of
        // steps to the last bit, so that the loop's last sample was the tail
        // itself.
        let last = f64::from(steps - 1) * SCAN_STEP;
        if last < tail && !rises(tail) {
            return last_rising(rises, last, tail);
        }
        end
    }

    /// The model's own end, in radians off the axis: the fold at
    /// acos(−1/ξ) for ξ > 1, or acos(−ξ), where z + ξ vanishes, for
    /// ξ ≤ 1.
    fn end_theta(&self) -> f64 {
        if self.xi > 1.0 {
            (-1.0 / self.xi).acos()
        } else {
            (-self.xi).clamp(-1.0, 1.0).acos()
        }
    }

    /// Whether the model images a unit direction whose z is `z`, up to
    /// its own end.
    fn images(&self, z: f64) -> bool {
        if self.xi > 1.0 {
            z >= -1.0 / self.xi
        } else {
            z + self.xi > 0.0
        }
    }

    /// The plane radius at θ = `limit`, the top of the inverse's bracket:
    /// infinite when the limit is the model's own end at ξ ≤ 1, where the
    /// denominator vanishes (when the polynomial has no fold before it).
    fn plane_at(&self, limit: f64) -> f64 {
        if self.xi <= 1.0 && limit >= self.end_theta() {
            return f64::INFINITY;
        }
        let (s, c) = limit.sin_cos();
        s / (c + self.xi)
    }
}

/// The last angle in [lo, hi] at which `rises` holds, given that it holds
/// at `lo` and not at `hi`, by bisection to the last bit.
fn last_rising(rises: impl Fn(f64) -> bool, mut lo: f64, mut hi: f64) -> f64 {
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if mid <= lo || mid >= hi {
            break;
        }
        if rises(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The t in [lo, hi] at which the rising curve `h` reaches `target`, by
/// Newton's method from `guess`, falling back to bisection whenever a
/// step would leave the bracket or fails to halve the step before last.
/// `h` gives its value and slope at t, and h(lo) ≤ target ≤ h(hi); it is
/// a polynomial here, with no pole for a tiny step beside one to pass
/// for convergence.
fn solve_rising(
    h: impl Fn(f64) -> (f64, f64),
    target: f64,
    mut lo: f64,
    mut hi: f64,
    guess: f64,
) -> f64 {
    let mut t = guess.max(lo).min(hi);
    let mut step = hi - lo;
    let mut step_before = step;
    for _ in 0..200 {
        let (v, slope) = h(t);
        let f = v - target;
        if f == 0.0 {
            return t;
        }
        if f < 0.0 {
            lo = t;
        } else {
            hi = t;
        }
        let newton = t - f / slope;
        let next = if newton > lo && newton < hi && (2.0 * f).abs() <= (step_before * slope).abs() {
            newton
        } else {
            0.5 * (lo + hi)
        };
        step_before = step;
        step = (next - t).abs();
        if step <= f64::EPSILON * next.abs() {
            return next;
        }
        t = next;
    }
    t
}

/// A t at which the rising, unbounded curve `h` has passed `target`, by
/// doubling from 1: the top of a bracket the curve gives no end for.
fn past(h: impl Fn(f64) -> (f64, f64), target: f64) -> f64 {
    let mut t = target.max(1.0);
    for _ in 0..1100 {
        if h(t).0 >= target {
            break;
        }
        t *= 2.0;
    }
    t
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
    pub fn v3(k: [f64; 3], p: [f64; 2]) -> Canonical {
        Canonical {
            k: [k[0], k[1], k[2], 0.0, 0.0],
            p,
            ..Canonical::NONE
        }
    }

    /// V6's d0 to d12 read through `reading`, one of the 21
    /// ([`V6Reading::is_valid`]): d0 to d4 radial, d5 to d12 as the
    /// reading says, and 0 in every field it doesn't read. A reading that
    /// isn't valid reads d7 and d8 twice, as its fields say.
    pub fn v6(d: [f64; 13], reading: V6Reading) -> Canonical {
        let mut c = Canonical {
            k: [d[0], d[1], d[2], d[3], d[4]],
            ..Canonical::NONE
        };
        if let Some(t) = reading.tangential {
            let (p, pe) = match t.pair {
                PairOrder::D5D6 => ([d[5], d[6]], [d[7], d[8]]),
                PairOrder::D6D5 => ([d[6], d[5]], [d[8], d[7]]),
            };
            c.p = p;
            if t.extension {
                c.pe = pe;
            }
        }
        if let Some(prism) = reading.prism {
            let at = match prism.at {
                PrismAt::D7 => 7,
                PrismAt::D9 => 9,
            };
            let [first, second, third, fourth] = [d[at], d[at + 1], d[at + 2], d[at + 3]];
            c.s = match prism.order {
                PrismOrder::ByAxis => [first, second, third, fourth],
                PrismOrder::Interleaved => [first, third, second, fourth],
            };
        }
        c
    }

    /// The distorted plane point (xd, yd) of the plane point `m`.
    pub fn distort(&self, m: DVec2) -> DVec2 {
        m * self.radial_factor(m.length_squared()) + self.rest(m)
    }

    /// R = 1 + k1 r² + k2 r⁴ + k3 r⁶ + k4 r⁸ + k5 r¹⁰, of r² = `r2`.
    fn radial_factor(&self, r2: f64) -> f64 {
        let [k1, k2, k3, k4, k5] = self.k;
        1.0 + r2 * (k1 + r2 * (k2 + r2 * (k3 + r2 * (k4 + r2 * k5))))
    }

    /// What [`Canonical::distort`] adds to m R at `m`: the tangential and
    /// prism terms.
    fn rest(&self, m: DVec2) -> DVec2 {
        let (x, y) = (m.x, m.y);
        let r2 = x * x + y * y;
        let p1 = self.p[0] + self.pe[0] * r2;
        let p2 = self.p[1] + self.pe[1] * r2;
        let [s1, s2, s3, s4] = self.s;
        DVec2::new(
            2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x) + r2 * (s1 + s2 * r2),
            2.0 * p2 * x * y + p1 * (r2 + 2.0 * y * y) + r2 * (s3 + s4 * r2),
        )
    }

    /// The radial curve ρ R(ρ²) of the plane radius `rho`.
    fn radial(&self, rho: f64) -> f64 {
        rho * self.radial_factor(rho * rho)
    }

    /// The radial curve's slope in ρ, of ρ² = `r2`: 1 + 3 k1 r² + 5 k2 r⁴
    /// + 7 k3 r⁶ + 9 k4 r⁸ + 11 k5 r¹⁰.
    fn slope(&self, r2: f64) -> f64 {
        let [k1, k2, k3, k4, k5] = self.k;
        1.0 + r2 * (3.0 * k1 + r2 * (5.0 * k2 + r2 * (7.0 * k3 + r2 * (9.0 * k4 + r2 * 11.0 * k5))))
    }

    /// A floor under [`Canonical::slope`] for every r² up to `r2_max`,
    /// which may be infinite: 1 plus each negative term at `r2_max`.
    fn slope_floor(&self, r2_max: f64) -> f64 {
        let mut floor = 1.0;
        let mut power = 1.0;
        for (n, k) in (3u32..).step_by(2).zip(self.k) {
            power *= r2_max;
            let c = f64::from(n) * k;
            if c < 0.0 {
                floor += c * power;
            }
        }
        floor
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
///
/// For the shader's copy (10.3): θ is atan2(|(x, y)|, z), never acos(z).
/// In f32 the largest z under 1 is 1 − 2⁻²⁴, whose acos is already
/// √(2 · 2⁻²⁴), about 3.5e-4 rad, so acos quantises θ that coarsely near
/// the axis, where atan2 keeps its digits. (cos φ, sin φ) is
/// (x, y)/|(x, y)|, and on the axis, where that has no value, m is 0.
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
    /// Lens-frame direction to pixel. `None` for a zero or non-finite
    /// `p`; on the back axis, θ = 180°, which has no azimuth, so that the
    /// directions about it land all round the rim and none of them is its
    /// image (as [`Unified::project`] is `None` there at any ξ); and where
    /// θ_d leaves the base's image: below 0, past 90° for an orthographic
    /// base, past 180° for an equisolid one, and from 180° on for a
    /// stereographic one, whose radius is infinite there. As with
    /// [`Unified::project`], the polynomial's own fold is
    /// [`KbLens::limit_theta`]'s to find.
    pub fn project(&self, p: DVec3) -> Option<DVec2> {
        let d = p.normalize_or_zero();
        if d == DVec3::ZERO {
            return None;
        }
        let rho = d.x.hypot(d.y);
        if rho == 0.0 && d.z < 0.0 {
            return None;
        }
        let theta_d = self.theta_d(rho.atan2(d.z));
        let images = theta_d >= 0.0
            && match self.base {
                Model::Stereographic => theta_d < PI,
                _ => theta_d <= self.base_end(),
            };
        if !images {
            return None;
        }
        let r = self.base_radius(theta_d);
        let m = if rho > 0.0 {
            DVec2::new(d.x, d.y) * (r / rho)
        } else {
            DVec2::ZERO
        };
        let md = self.terms().distort(m);
        Some(DVec2::new(
            self.fx * md.x + self.cx,
            self.fy * md.y + self.cy,
        ))
    }

    /// Pixel to lens-frame unit direction. The plane point comes first, by
    /// [`Unified`]'s passes with no radial term to solve: each strips the
    /// tangential and prism terms at the estimate, until the residual is
    /// under [`Unified::RESIDUAL`], at most [`Unified::PASSES`] times;
    /// then θ_d, by the base's own inverse of |m|, and θ, by a Newton on
    /// the polynomial θ_d(θ) bracketed on [0, `limit_theta`]. `None` past
    /// the limit or where the passes don't converge. Each call scans for
    /// [`KbLens::limit_theta`] afresh, up to 3600 steps, so a caller
    /// inverting many pixels of one lens finds the limit once and calls
    /// [`KbLens::unproject_within`].
    pub fn unproject(&self, px: DVec2) -> Option<DVec3> {
        self.unproject_within(px, self.limit_theta())
    }

    /// [`KbLens::unproject`] with the limit given, found once for the
    /// lens: `limit` is [`KbLens::limit_theta`]'s value or any smaller θ
    /// in radians, and the inverse is `None` past it, or for a NaN or
    /// negative `limit`. Past `limit_theta` the polynomial no longer
    /// rises, so a larger `limit` gives the Newton no single answer. A
    /// `limit` past the back axis, +∞ among them, is the back axis, where
    /// a larger θ would turn the direction half way round. The base's own
    /// end needs no such care: its closed-form inverse gives no θ_d past
    /// it.
    pub fn unproject_within(&self, px: DVec2, limit: f64) -> Option<DVec3> {
        let target = DVec2::new((px.x - self.cx) / self.fx, (px.y - self.cy) / self.fy);
        if !target.is_finite() || limit.is_nan() || limit < 0.0 {
            return None;
        }
        let limit = limit.min(PI);
        let terms = self.terms();
        let mut m = DVec2::ZERO;
        let mut converged = false;
        for _ in 0..Unified::PASSES {
            m = target - terms.rest(m);
            if (terms.distort(m) - target).length() < Unified::RESIDUAL {
                converged = true;
                break;
            }
        }
        if !converged {
            return None;
        }
        // The base is undone in closed form, so that the Newton runs on the
        // polynomial alone, which has no pole (a stereographic base's tan
        // has one at θ_d = 180°).
        let r = m.length();
        let theta_d = match self.base {
            Model::Equidistant => r,
            Model::Equisolid => 2.0 * (r / 2.0).asin(),
            Model::Stereographic => 2.0 * (r / 2.0).atan(),
            Model::Orthographic => r.asin(),
        };
        // Past the base's image (r > 1 on an orthographic base, r > 2 on an
        // equisolid one) θ_d is NaN.
        if theta_d.is_nan() || theta_d > self.theta_d(limit) {
            return None;
        }
        let curve = |theta: f64| (self.theta_d(theta), self.theta_d_slope(theta));
        let theta = solve_rising(curve, theta_d, 0.0, limit, theta_d);
        let (st, ct) = theta.sin_cos();
        let across = if r > 0.0 { m * (st / r) } else { DVec2::ZERO };
        Some(DVec3::new(across.x, across.y, ct))
    }

    /// The largest θ, in radians, at which the radius still rises, the
    /// base's own end included (an orthographic base folds at θ_d = 90°,
    /// an equisolid one at 180°, where a stereographic one's radius is
    /// infinite): a scan in 0.05° steps up to 180°, the back axis itself
    /// the last sample, bisected within the step where the radius stops
    /// rising, as [`Unified::limit_theta`]'s is.
    pub fn limit_theta(&self) -> f64 {
        let end = self.base_end();
        let rises = |theta: f64| self.theta_d_slope(theta) > 0.0 && self.theta_d(theta) < end;
        let steps = (PI / SCAN_STEP).round() as u32;
        for k in 1..steps {
            let theta = f64::from(k) * SCAN_STEP;
            if !rises(theta) {
                return last_rising(rises, f64::from(k - 1) * SCAN_STEP, theta);
            }
        }
        // The last step, up to the back axis, where θ_d and its slope are
        // as finite as anywhere.
        let last = f64::from(steps - 1) * SCAN_STEP;
        if !rises(PI) {
            return last_rising(rises, last, PI);
        }
        PI
    }

    /// The tangential and prism terms, as a [`Canonical`] with no radial
    /// ones.
    fn terms(&self) -> Canonical {
        Canonical {
            k: [0.0; 5],
            p: self.p,
            pe: self.pe,
            s: self.s,
        }
    }

    /// θ_d = θ (1 + k1 θ² + k2 θ⁴ + k3 θ⁶ + k4 θ⁸ + k5 θ¹⁰).
    fn theta_d(&self, theta: f64) -> f64 {
        let t2 = theta * theta;
        let [k1, k2, k3, k4, k5] = self.k;
        theta * (1.0 + t2 * (k1 + t2 * (k2 + t2 * (k3 + t2 * (k4 + t2 * k5)))))
    }

    /// dθ_d/dθ.
    fn theta_d_slope(&self, theta: f64) -> f64 {
        let t2 = theta * theta;
        let [k1, k2, k3, k4, k5] = self.k;
        1.0 + t2 * (3.0 * k1 + t2 * (5.0 * k2 + t2 * (7.0 * k3 + t2 * (9.0 * k4 + t2 * 11.0 * k5))))
    }

    /// The base's plane radius at θ_d, as [`Lens::radius`] gives it.
    fn base_radius(&self, theta_d: f64) -> f64 {
        match self.base {
            Model::Equidistant => theta_d,
            Model::Equisolid => 2.0 * (theta_d / 2.0).sin(),
            Model::Stereographic => 2.0 * (theta_d / 2.0).tan(),
            Model::Orthographic => theta_d.sin(),
        }
    }

    /// The θ_d at which the base stops rising.
    fn base_end(&self) -> f64 {
        match self.base {
            Model::Equidistant => f64::INFINITY,
            Model::Equisolid | Model::Stereographic => PI,
            Model::Orthographic => PI / 2.0,
        }
    }
}

/// A lens of a stitched pair (6.1): Mei, or Kannala–Brandt on a base. The
/// shader carries which one per rig, not per lens (10.3's `Rig.model`),
/// so a rig that pairs a Mei lens with a KB one renders on the CPU only;
/// whether `Rig::build` refuses such a pair is the rig's to decide.
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

    /// Pixel to lens-frame unit direction, with the limit found once:
    /// [`LensModel::limit_theta`]'s value or any smaller θ, past which it
    /// is `None`.
    pub fn unproject_within(&self, px: DVec2, limit: f64) -> Option<DVec3> {
        match self {
            LensModel::Mei(m) => m.unproject_within(px, limit),
            LensModel::Kb(k) => k.unproject_within(px, limit),
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
