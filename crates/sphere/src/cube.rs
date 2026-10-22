//! Cube maps, both plain (gnomonic) and equi-angular (EAC).
//!
//! This module only maps directions to a face and a face-local coordinate.
//! How faces are packed into pixels (YouTube EAC, GoPro's two-stream layout)
//! is a per-format concern handled where those formats are decoded.

use glam::{DVec2, DVec3};
use std::f64::consts::FRAC_PI_4;

/// The six faces, named by the axis that points out through their centre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    /// −Z, the forward direction.
    Front,
    /// +X.
    Right,
    /// +Z.
    Back,
    /// −X.
    Left,
    /// +Y.
    Up,
    /// −Y.
    Down,
}

impl Face {
    pub const ALL: [Face; 6] = [
        Face::Front,
        Face::Right,
        Face::Back,
        Face::Left,
        Face::Up,
        Face::Down,
    ];

    /// `(normal, right, up)` for the face as seen from the cube centre.
    /// A face coordinate `(s, t)` in `[-1, 1]²` (t up) is the point
    /// `normal + s·right + t·up` on the unit cube.
    pub fn basis(self) -> (DVec3, DVec3, DVec3) {
        match self {
            Face::Front => (DVec3::NEG_Z, DVec3::X, DVec3::Y),
            Face::Right => (DVec3::X, DVec3::Z, DVec3::Y),
            Face::Back => (DVec3::Z, DVec3::NEG_X, DVec3::Y),
            Face::Left => (DVec3::NEG_X, DVec3::NEG_Z, DVec3::Y),
            // Looking up, with the forward direction at the bottom edge.
            Face::Up => (DVec3::Y, DVec3::X, DVec3::Z),
            // Looking down, with the forward direction at the top edge.
            Face::Down => (DVec3::NEG_Y, DVec3::X, DVec3::NEG_Z),
        }
    }

    /// The face a direction exits the cube through.
    pub fn of_dir(d: DVec3) -> Face {
        let a = d.abs();
        if a.x >= a.y && a.x >= a.z {
            if d.x > 0.0 { Face::Right } else { Face::Left }
        } else if a.y >= a.z {
            if d.y > 0.0 { Face::Up } else { Face::Down }
        } else if d.z > 0.0 {
            Face::Back
        } else {
            Face::Front
        }
    }
}

/// How positions on a face are spaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spacing {
    /// Plain cube map: face coordinate is the tangent-plane position.
    Gnomonic,
    /// Equi-angular: face coordinate is proportional to angle, which spreads
    /// resolution evenly (`s = atan(x) · 4/π`).
    EquiAngular,
}

/// Direction → `(face, st)` with `st` in `[-1, 1]²`, t pointing up the face.
pub fn from_dir(d: DVec3, spacing: Spacing) -> (Face, DVec2) {
    let face = Face::of_dir(d);
    (face, from_dir_on(face, d, spacing))
}

/// Face coordinate of `d` on a given face, which may lie outside `[-1, 1]`
/// when `d` belongs to a neighbouring face. Sampling across face edges
/// (overlap bands, blending) needs this. `d` must point into the half-space
/// of the face.
pub fn from_dir_on(face: Face, d: DVec3, spacing: Spacing) -> DVec2 {
    let (n, r, u) = face.basis();
    let depth = d.dot(n);
    let p = DVec2::new(d.dot(r) / depth, d.dot(u) / depth);
    match spacing {
        Spacing::Gnomonic => p,
        Spacing::EquiAngular => DVec2::new(p.x.atan(), p.y.atan()) / FRAC_PI_4,
    }
}

/// `(face, st)` → unit direction.
pub fn to_dir(face: Face, st: DVec2, spacing: Spacing) -> DVec3 {
    let p = match spacing {
        Spacing::Gnomonic => st,
        Spacing::EquiAngular => DVec2::new((st.x * FRAC_PI_4).tan(), (st.y * FRAC_PI_4).tan()),
    };
    let (n, r, u) = face.basis();
    (n + p.x * r + p.y * u).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::angle_between;
    use crate::testutil::unit_dir;
    use proptest::prelude::*;

    #[test]
    fn bases_are_right_handed_and_orthonormal() {
        for f in Face::ALL {
            let (n, r, u) = f.basis();
            assert!((n.length() - 1.0).abs() < 1e-12);
            assert!(n.dot(r).abs() < 1e-12 && n.dot(u).abs() < 1e-12 && r.dot(u).abs() < 1e-12);
            // Viewed from inside, right × up points back towards the viewer.
            assert!((r.cross(u) + n).length() < 1e-12, "{f:?}");
            assert_eq!(Face::of_dir(n), f);
        }
    }

    #[test]
    fn face_corners_meet() {
        // The top-right corner of Front is the bottom-right corner of Up.
        let a = to_dir(Face::Front, DVec2::new(1.0, 1.0), Spacing::EquiAngular);
        let b = to_dir(Face::Up, DVec2::new(1.0, -1.0), Spacing::EquiAngular);
        assert!(angle_between(a, b) < 1e-12);
    }

    proptest! {
        #[test]
        fn round_trip(d in unit_dir(), eac in any::<bool>()) {
            let sp = if eac { Spacing::EquiAngular } else { Spacing::Gnomonic };
            let (f, st) = from_dir(d, sp);
            prop_assert!(st.x.abs() <= 1.0 + 1e-12 && st.y.abs() <= 1.0 + 1e-12);
            prop_assert!(angle_between(to_dir(f, st, sp), d) < 1e-9);
        }

        #[test]
        fn equi_angular_steps_are_equal_angles(s in -0.95f64..0.95) {
            // Along the face's horizontal centre line, equal steps in s are
            // equal steps in angle.
            let h = 0.05;
            let a = to_dir(Face::Front, DVec2::new(s, 0.0), Spacing::EquiAngular);
            let b = to_dir(Face::Front, DVec2::new(s + h, 0.0), Spacing::EquiAngular);
            prop_assert!((angle_between(a, b) - h * FRAC_PI_4).abs() < 1e-9);
        }
    }
}
