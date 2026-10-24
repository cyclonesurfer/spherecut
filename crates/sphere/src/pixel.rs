//! Output pixels and lens points (M1 plan, section 4.5).
//!
//! A `w` × `h` image has pixel `(i, j)` covering `[i, i + 1) × [j, j + 1)`
//! with its centre at `(i + 0.5, j + 0.5)`, `j = 0` at the top, which is
//! where a fragment shader's `@builtin(position)` puts it. A lens point
//! `p` (see [`crate::perspective`]) runs from −1 at the left edge to +1 at
//! the right edge, with `p.y` scaled by the same factor and +y up, so the
//! top and bottom edges are at `±h / w`.

use glam::DVec2;

/// Pixel coordinates `(x, y)` → lens point. `(x, y)` is a position in the
/// image, not an index: the centre of pixel `(i, j)` is
/// [`centre`]`(i, j)`.
pub fn to_lens(size: (u32, u32), x: f64, y: f64) -> DVec2 {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    DVec2::new(2.0 * x / w - 1.0, (1.0 - 2.0 * y / h) * h / w)
}

/// Lens point → pixel coordinates, the inverse of [`to_lens`].
pub fn from_lens(size: (u32, u32), p: DVec2) -> DVec2 {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    DVec2::new((p.x + 1.0) * w / 2.0, (h - p.y * w) / 2.0)
}

/// The centre of pixel `(i, j)`.
pub fn centre(i: u32, j: u32) -> DVec2 {
    DVec2::new(f64::from(i) + 0.5, f64::from(j) + 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn edges_and_centre_land_where_documented() {
        let size = (200, 100);
        assert_eq!(to_lens(size, 0.0, 50.0), DVec2::new(-1.0, 0.0));
        assert_eq!(to_lens(size, 200.0, 50.0), DVec2::new(1.0, 0.0));
        assert_eq!(to_lens(size, 100.0, 0.0), DVec2::new(0.0, 0.5));
        assert_eq!(to_lens(size, 100.0, 100.0), DVec2::new(0.0, -0.5));
        // An odd size has a pixel whose centre is the image's centre.
        assert_eq!(to_lens((3, 3), centre(1, 1).x, centre(1, 1).y), DVec2::ZERO);
    }

    proptest! {
        #[test]
        fn pixels_round_trip(w in 1u32..10_000, h in 1u32..10_000,
                             fx in 0.0f64..=1.0, fy in 0.0f64..=1.0) {
            let (x, y) = (fx * f64::from(w), fy * f64::from(h));
            let back = from_lens((w, h), to_lens((w, h), x, y));
            prop_assert!((back.x - x).abs() < 1e-9 * f64::from(w));
            prop_assert!((back.y - y).abs() < 1e-9 * f64::from(w.max(h)));
        }
    }
}
