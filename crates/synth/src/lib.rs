//! Synthetic test media.
//!
//! Every scene is a function of direction on the unit sphere, so the same
//! scene can be rendered into any projection (equirect now; dual fisheye
//! and EAC for the stitching round trips in M3) and compared against itself.
//! Rendering is pure CPU `f64` math with fixed supersampling, so pattern
//! frames are byte-identical on every machine.

use glam::DVec3;
use spherecut_sphere::{LonLat, equirect};

pub mod audio;
pub mod media;
pub mod yuv;

pub use glam::DVec2;

/// The test scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// Smooth direction-coded colors, a 15° graticule, bright equator and
    /// prime meridian, labelled axis discs, a marker moving along the
    /// equator, and a binary frame counter. The general-purpose scene.
    Grid,
    /// A 1°-cell checkerboard in longitude/latitude: maximal aliasing in
    /// wide views and around the poles.
    HighFrequency,
    /// Thin stripes straddling the ±180° seam plus a horizontal gradient,
    /// so a seam line or a half-texel wrap error is obvious.
    Seam,
}

impl Pattern {
    pub const ALL: [Pattern; 3] = [Pattern::Grid, Pattern::HighFrequency, Pattern::Seam];

    pub fn name(self) -> &'static str {
        match self {
            Pattern::Grid => "grid",
            Pattern::HighFrequency => "hifreq",
            Pattern::Seam => "seam",
        }
    }
}

/// Display-referred RGB in `[0, 1]`.
pub type Rgb = [f64; 3];

/// Axis marker colors, used by orientation tests.
pub const AXIS_MARKERS: [(DVec3, Rgb); 6] = [
    (DVec3::NEG_Z, [1.0, 0.0, 0.0]), // front: red
    (DVec3::X, [0.0, 1.0, 0.0]),     // right: green
    (DVec3::Z, [0.0, 0.0, 1.0]),     // back: blue
    (DVec3::NEG_X, [1.0, 1.0, 0.0]), // left: yellow
    (DVec3::Y, [1.0, 1.0, 1.0]),     // up: white
    (DVec3::NEG_Y, [0.0, 0.0, 0.0]), // down: black
];
/// Angular radius of the axis discs.
pub const AXIS_MARKER_RADIUS: f64 = 8.0 * std::f64::consts::PI / 180.0;

/// Frame counter: bit `i` is a square centred at latitude −20°,
/// longitude `FRAME_CODE_LON0 + i·FRAME_CODE_STEP` degrees. White = 1.
pub const FRAME_CODE_BITS: u32 = 16;
pub const FRAME_CODE_LAT: f64 = -20.0;
pub const FRAME_CODE_LON0: f64 = -45.0;
pub const FRAME_CODE_STEP: f64 = 6.0;
const FRAME_CODE_HALF: f64 = 2.2;

/// Longitude (degrees) of the moving equator marker at frame `n`.
pub fn moving_marker_lon(frame: u32) -> f64 {
    (frame as f64 * 3.0 + 180.0).rem_euclid(360.0) - 180.0
}

fn deg(r: f64) -> f64 {
    r.to_degrees()
}

/// Distance in degrees from `x` to the nearest multiple of `step`.
fn dist_to_multiple(x: f64, step: f64) -> f64 {
    let r = x.rem_euclid(step);
    r.min(step - r)
}

/// The color of `pattern` in direction `d` at frame `frame`.
pub fn color(pattern: Pattern, d: DVec3, frame: u32) -> Rgb {
    let d = d.normalize();
    let ll = LonLat::from_dir(d);
    let (lon, lat) = (deg(ll.lon), deg(ll.lat));
    match pattern {
        Pattern::Grid => {
            // Dot-product tests: cheaper than angles, and this is the
            // innermost loop of every synthetic frame.
            let cos_axis = AXIS_MARKER_RADIUS.cos();
            for (axis, c) in AXIS_MARKERS {
                if d.dot(axis) > cos_axis {
                    return c;
                }
            }
            // Frame code.
            if (lat - FRAME_CODE_LAT).abs() < FRAME_CODE_HALF {
                for bit in 0..FRAME_CODE_BITS {
                    let c = FRAME_CODE_LON0 + bit as f64 * FRAME_CODE_STEP;
                    if (lon - c).abs() < FRAME_CODE_HALF {
                        let on = (frame >> bit) & 1 == 1;
                        return if on { [1.0; 3] } else { [0.0; 3] };
                    }
                }
            }
            // Moving marker on the equator.
            let m = LonLat {
                lon: moving_marker_lon(frame).to_radians(),
                lat: 0.0,
            }
            .to_dir();
            if d.dot(m) > 3f64.to_radians().cos() {
                return [1.0, 0.0, 1.0];
            }
            // Equator and prime meridian.
            if lat.abs() < 0.6 || (lon.abs() < 0.6 && lat.abs() < 80.0) {
                return [1.0; 3];
            }
            // 15° graticule (meridians stop short of the poles).
            let on_parallel = dist_to_multiple(lat, 15.0) < 0.35;
            let on_meridian = dist_to_multiple(lon, 15.0) < 0.35 && lat.abs() < 75.0;
            let base = [0.5 + 0.35 * d.x, 0.5 + 0.35 * d.y, 0.5 + 0.35 * d.z];
            if on_parallel || on_meridian {
                base.map(|v| v * 0.35)
            } else {
                base
            }
        }
        Pattern::HighFrequency => {
            let a = (lon.floor() as i64 + 1000) % 2;
            let b = (lat.floor() as i64 + 1000) % 2;
            if a == b { [0.9; 3] } else { [0.1; 3] }
        }
        Pattern::Seam => {
            let behind = 180.0 - lon.abs(); // 0 at the seam
            if behind < 3.0 {
                // 0.5°-wide stripes: 6 on each side of the seam.
                if (behind / 0.5).floor() as i64 % 2 == 0 {
                    [1.0, 0.2, 0.2]
                } else {
                    [0.2, 0.2, 1.0]
                }
            } else {
                let t = (lon + 180.0) / 360.0;
                [t, 0.5 + 0.4 * (lat / 90.0), 1.0 - t]
            }
        }
    }
}

/// An RGB image, row-major, `[r, g, b]` per pixel in `[0, 1]`.
#[derive(Debug, Clone)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgb>,
}

impl RgbImage {
    pub fn get(&self, x: u32, y: u32) -> Rgb {
        self.pixels[(y * self.width + x) as usize]
    }

    /// Nearest-texel lookup at a direction, for equirect images.
    pub fn sample_equirect(&self, d: DVec3) -> Rgb {
        let uv = equirect::from_dir(d);
        let x = ((uv.x * self.width as f64) as u32).min(self.width - 1);
        let y = ((uv.y * self.height as f64) as u32).min(self.height - 1);
        self.get(x, y)
    }
}

/// Render an equirect frame with `ss × ss` supersampling per pixel, split
/// across threads by rows. The result does not depend on the thread count.
pub fn render_equirect(pattern: Pattern, width: u32, height: u32, frame: u32, ss: u32) -> RgbImage {
    let ss = ss.max(1);
    let inv = 1.0 / (ss * ss) as f64;
    let mut pixels = vec![[0.0; 3]; (width * height) as usize];
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(16);
    let rows_per = (height as usize).div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (chunk_i, chunk) in pixels.chunks_mut(rows_per * width as usize).enumerate() {
            s.spawn(move || {
                for (i, px) in chunk.iter_mut().enumerate() {
                    let x = (i % width as usize) as u32;
                    let y = (chunk_i * rows_per + i / width as usize) as u32;
                    let mut acc = [0.0; 3];
                    for sy in 0..ss {
                        for sx in 0..ss {
                            let u = (x as f64 + (sx as f64 + 0.5) / ss as f64) / width as f64;
                            let v = (y as f64 + (sy as f64 + 0.5) / ss as f64) / height as f64;
                            let c = color(pattern, equirect::to_dir(DVec2::new(u, v)), frame);
                            for k in 0..3 {
                                acc[k] += c[k];
                            }
                        }
                    }
                    *px = acc.map(|v| v * inv);
                }
            });
        }
    });
    RgbImage {
        width,
        height,
        pixels,
    }
}

/// Read the frame counter back from an equirect image (e.g. a decoded
/// frame). `None` if any bit is ambiguous.
pub fn decode_frame_code(img: &RgbImage) -> Option<u32> {
    let mut n = 0;
    for bit in 0..FRAME_CODE_BITS {
        let d = LonLat {
            lon: (FRAME_CODE_LON0 + bit as f64 * FRAME_CODE_STEP).to_radians(),
            lat: FRAME_CODE_LAT.to_radians(),
        }
        .to_dir();
        let c = img.sample_equirect(d);
        let l = (c[0] + c[1] + c[2]) / 3.0;
        if l > 0.75 {
            n |= 1 << bit;
        } else if l > 0.25 {
            return None;
        }
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_is_deterministic() {
        let a = render_equirect(Pattern::Grid, 64, 32, 7, 2);
        let b = render_equirect(Pattern::Grid, 64, 32, 7, 2);
        assert_eq!(a.pixels, b.pixels);
    }

    #[test]
    fn axis_markers_are_where_documented() {
        let img = render_equirect(Pattern::Grid, 360, 180, 0, 1);
        for (axis, c) in AXIS_MARKERS {
            assert_eq!(img.sample_equirect(axis), c, "{axis:?}");
        }
    }

    #[test]
    fn frame_code_round_trips() {
        for n in [0u32, 1, 2, 5, 255, 1234, 65535] {
            let img = render_equirect(Pattern::Grid, 720, 360, n, 1);
            assert_eq!(decode_frame_code(&img), Some(n));
        }
    }

    #[test]
    fn seam_stripes_meet_across_the_seam() {
        // Directly behind, the two sides of the seam are the same stripe.
        let l = color(
            Pattern::Seam,
            LonLat {
                lon: (179.9f64).to_radians(),
                lat: 0.0,
            }
            .to_dir(),
            0,
        );
        let r = color(
            Pattern::Seam,
            LonLat {
                lon: (-179.9f64).to_radians(),
                lat: 0.0,
            }
            .to_dir(),
            0,
        );
        assert_eq!(l, r);
    }
}
