//! Grabbing the picture and zooming about the cursor (M1 plan section
//! 7.4): the viewer turns the camera so that the direction the pointer
//! pressed on stays under the pointer.
//!
//! Points are lens points ([`crate::pixel::to_lens`]) and orientations
//! are `world = q · view`, as in [`crate::orient`]. Level mode keeps the
//! roll, so a level horizon stays level; free mode (Ctrl held) takes the
//! smallest rotation and may roll the view.

use glam::{DQuat, DVec2, DVec3};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use crate::orient::Ypr;
use crate::perspective::Lens;

/// Where a grab started: the world direction under the pointer, the roll
/// level mode keeps, and the yaw and pitch its next solve starts from
/// (the pitch nearest wins, so a drag moves continuously, and the yaw is
/// kept where any yaw would do). After each move the viewer sets them to
/// the yaw and pitch it took.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrabStart {
    pub world: DVec3,
    pub roll: f64,
    pub yaw: f64,
    pub pitch: f64,
}

/// Whether a solve put the grabbed direction exactly under the point,
/// or, where no orientation with the kept roll can (near a pole), as
/// close as one can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabFit {
    Exact,
    Clamped,
}

/// A grab at lens point `p0` of a view `q` through `lens`.
pub fn start(q: DQuat, lens: &Lens, p0: DVec2) -> GrabStart {
    let ypr = Ypr::from_quat(q);
    GrabStart {
        world: (q * lens.unproject(p0)).normalize(),
        roll: ypr.roll,
        yaw: ypr.yaw,
        pitch: ypr.pitch,
    }
}

/// Level mode: the orientation with `start.roll` that puts `start.world`
/// under lens point `p`, its pitch in [−π/2, π/2] and the root nearest
/// `start.pitch`. `Clamped` when no such pitch exists: then the pitch in
/// range that brings the direction closest.
pub fn solve_level(start: &GrabStart, lens: &Lens, p: DVec2) -> (DQuat, GrabFit) {
    let w = start.world.normalize();
    // v' = R_z(−roll) · unproject(p); R_y(−yaw) · R_x(pitch) · v' = w.
    let v = DQuat::from_rotation_z(-start.roll) * lens.unproject(p).normalize();
    // R_y keeps y, so (R_x(pitch) · v').y = v'.y cos p − v'.z sin p
    // = R cos(p + φ) must equal w.y.
    let r = v.y.hypot(v.z);
    let phi = v.z.atan2(v.y);
    let nearest = roots(r, phi, w.y)
        .into_iter()
        .flatten()
        .filter_map(in_range)
        .min_by(|a, b| (a - start.pitch).abs().total_cmp(&(b - start.pitch).abs()));
    let (pitch, fit) = match nearest {
        Some(p) => (p, GrabFit::Exact),
        None => (closest(r, phi, w.y), GrabFit::Clamped),
    };
    // With the pitch fixed, the yaw turns u's azimuth onto w's: R_y(−yaw)
    // adds yaw to an azimuth atan2(x, −z).
    let u = DQuat::from_rotation_x(pitch) * v;
    let yaw = if u.x.hypot(u.z) < 1e-12 || w.x.hypot(w.z) < 1e-12 {
        // Straight up or down: any yaw does, so the grab's is kept.
        start.yaw
    } else {
        w.x.atan2(-w.z) - u.x.atan2(-u.z)
    };
    let q = DQuat::from_rotation_y(-yaw)
        * DQuat::from_rotation_x(pitch)
        * DQuat::from_rotation_z(-start.roll);
    (q.normalize(), fit)
}

/// Free mode: the smallest rotation of `q` that puts `start.world`
/// under lens point `p`, `from_rotation_arc(q · unproject(p), world) · q`.
pub fn solve_free(q: DQuat, start: &GrabStart, lens: &Lens, p: DVec2) -> DQuat {
    let now = (q * lens.unproject(p)).normalize();
    (DQuat::from_rotation_arc(now, start.world.normalize()) * q).normalize()
}

/// Zoom about the cursor: the orientation that keeps the direction under
/// lens point `p` there when the lens changes from `old` to `new`, with
/// the roll kept (level mode's solve; near a pole, as close as it gets).
pub fn zoom_about(q: DQuat, old: &Lens, new: &Lens, p: DVec2) -> DQuat {
    let grab = start(q, old, p);
    solve_level(&grab, new, p).0
}

/// The pitches p with R cos(p + φ) = y, or `None` when |y| > R.
fn roots(r: f64, phi: f64, y: f64) -> Option<[f64; 2]> {
    if r < 1e-15 {
        return None;
    }
    let c = y / r;
    if c.abs() > 1.0 + 1e-12 {
        return None;
    }
    let a = c.clamp(-1.0, 1.0).acos();
    Some([a - phi, -a - phi])
}

/// `p` moved by whole turns into [−π/2, π/2], if it lands there (within
/// rounding, then clamped onto the edge).
fn in_range(p: f64) -> Option<f64> {
    let p = (p + PI).rem_euclid(TAU) - PI;
    const EDGE: f64 = 1e-12;
    (p.abs() <= FRAC_PI_2 + EDGE).then(|| p.clamp(-FRAC_PI_2, FRAC_PI_2))
}

/// The pitch in [−π/2, π/2] whose y, R cos(p + φ), is nearest `y`: an
/// end of the range, or an extreme of the sinusoid inside it (where
/// p + φ is a multiple of π).
fn closest(r: f64, phi: f64, y: f64) -> f64 {
    let gap = |p: f64| (r * (p + phi).cos() - y).abs();
    let ends = [-FRAC_PI_2, FRAC_PI_2];
    let extremes = (-2..=2).filter_map(|k| in_range(f64::from(k) * PI - phi));
    ends.into_iter()
        .chain(extremes)
        .min_by(|a, b| gap(*a).total_cmp(&gap(*b)))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::angle_between;
    use crate::perspective::Projection;
    use proptest::prelude::*;

    fn lens(p: Projection, to: Projection, morph: f64, hfov_deg: f64) -> Lens {
        Lens {
            from: p,
            to,
            morph,
            hfov: hfov_deg.to_radians(),
        }
    }

    fn projection() -> impl Strategy<Value = Projection> {
        prop_oneof![
            Just(Projection::Rectilinear),
            Just(Projection::Fisheye),
            Just(Projection::Stereographic)
        ]
    }

    #[test]
    fn a_drag_across_the_centre_turns_the_yaw_and_pitch_it_should() {
        let l = Lens::new(Projection::Rectilinear, 90f64.to_radians());
        let q = Ypr::new(0.0, 0.0, 0.0).to_quat();
        // Pressing on the right edge's centre and dragging to the centre
        // turns right by half the field of view.
        let g = start(q, &l, DVec2::new(1.0, 0.0));
        let (q1, fit) = solve_level(&g, &l, DVec2::ZERO);
        assert_eq!(fit, GrabFit::Exact);
        let ypr = Ypr::from_quat(q1);
        assert!((ypr.yaw - 45f64.to_radians()).abs() < 1e-12, "{ypr:?}");
        assert!(ypr.pitch.abs() < 1e-12 && ypr.roll.abs() < 1e-12, "{ypr:?}");
        // And pressing above the centre and dragging down to it looks up.
        let g = start(q, &l, DVec2::new(0.0, 0.5));
        let (q1, _) = solve_level(&g, &l, DVec2::ZERO);
        let ypr = Ypr::from_quat(q1);
        assert!((ypr.pitch - 0.5f64.atan()).abs() < 1e-12, "{ypr:?}");
        assert!(ypr.yaw.abs() < 1e-12, "{ypr:?}");
    }

    #[test]
    fn a_pole_dragged_past_the_centre_is_clamped_at_ninety_degrees() {
        let l = Lens::new(Projection::Rectilinear, 90f64.to_radians());
        let g = GrabStart {
            world: DVec3::Y,
            roll: 0.0,
            yaw: 0.0,
            pitch: 80f64.to_radians(),
        };
        // The zenith under a point below the centre would need a pitch
        // past 90°.
        let (q1, fit) = solve_level(&g, &l, DVec2::new(0.0, -0.3));
        assert_eq!(fit, GrabFit::Clamped);
        let ypr = Ypr::from_quat(q1);
        assert!((ypr.pitch - FRAC_PI_2).abs() < 1e-9, "{ypr:?}");
    }

    #[test]
    fn free_mode_takes_the_direction_exactly_and_may_roll() {
        let l = Lens::new(Projection::Fisheye, 180f64.to_radians());
        let q = Ypr::from_degrees(30.0, 20.0, 0.0).to_quat();
        let g = start(q, &l, DVec2::new(0.4, 0.3));
        let p = DVec2::new(-0.2, -0.1);
        let q1 = solve_free(q, &g, &l, p);
        assert!(angle_between(q1 * l.unproject(p), g.world) < 1e-12);
    }

    #[test]
    fn zooming_keeps_the_direction_under_the_cursor() {
        let old = Lens::new(Projection::Rectilinear, 100f64.to_radians());
        let new = Lens::new(Projection::Rectilinear, 60f64.to_radians());
        let q = Ypr::from_degrees(-40.0, 10.0, 5.0).to_quat();
        let p = DVec2::new(0.6, -0.2);
        let w = q * old.unproject(p);
        let q1 = zoom_about(q, &old, &new, p);
        assert!(angle_between(q1 * new.unproject(p), w) < 1e-9);
        assert!((Ypr::from_quat(q1).roll - Ypr::from_quat(q).roll).abs() < 1e-12);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(10_000)
        ))]

        /// Level mode puts the grabbed direction under the point to 1e-9
        /// rad when it says Exact (and project_morph gives the point back
        /// to 1e-6), keeps the roll to 1e-12, and when it says Clamped no
        /// pitch on a 0.01° grid about its own gets closer.
        #[test]
        fn level_grabs_land_or_come_as_close_as_any_pitch(
            yaw in -PI..PI,
            pitch in -FRAC_PI_2..FRAC_PI_2,
            roll in -PI..PI,
            from in projection(),
            to in projection(),
            morph in 0.0f64..1.0,
            hfov_deg in 20.0f64..200.0,
            p0 in (-1.0f64..1.0, -0.6f64..0.6),
            p in (-1.0f64..1.0, -0.6f64..0.6),
        ) {
            let l = lens(from, to, morph, hfov_deg);
            let q = Ypr::new(yaw, pitch, roll).to_quat();
            let (p0, p) = (DVec2::new(p0.0, p0.1), DVec2::new(p.0, p.1));
            let g = start(q, &l, p0);
            let (q1, fit) = solve_level(&g, &l, p);
            let ypr1 = Ypr::from_quat(q1);
            let got = q1 * l.unproject(p);
            let miss = angle_between(got, g.world);
            // Roll is kept, except looking straight up or down, where
            // from_quat folds it into the yaw.
            if ypr1.pitch.abs() < FRAC_PI_2 - 1e-4 {
                let d = (ypr1.roll - g.roll + PI).rem_euclid(TAU) - PI;
                prop_assert!(d.abs() < 1e-9, "roll {} against {}", ypr1.roll, g.roll);
            }
            match fit {
                GrabFit::Exact => {
                    prop_assert!(miss < 1e-9, "missed by {miss} rad");
                    if let Some(back) = l.project_morph(q1.conjugate() * g.world) {
                        prop_assert!((back - p).length() < 1e-6, "{back} against {p}");
                    }
                }
                GrabFit::Clamped => {
                    let v = DQuat::from_rotation_z(-g.roll) * l.unproject(p);
                    let step = 0.01f64.to_radians();
                    for k in -100..=100 {
                        let pp = (ypr1.pitch + f64::from(k) * step).clamp(-FRAC_PI_2, FRAC_PI_2);
                        let u = DQuat::from_rotation_x(pp) * v;
                        // The best yaw for that pitch lines the azimuths
                        // up, leaving the gap in latitude.
                        let gap = (u.y.clamp(-1.0, 1.0).asin() - g.world.y.clamp(-1.0, 1.0).asin()).abs();
                        prop_assert!(gap >= miss - 1e-9, "pitch {pp} gets within {gap}, against {miss}");
                    }
                }
            }
        }
    }
}
