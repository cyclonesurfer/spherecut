//! Synthetic test media.
//!
//! Every scene is a function of direction on the unit sphere, so the same
//! scene can be rendered into any projection (equirect now; dual fisheye
//! and EAC for the stitching round trips in M3) and compared against itself.
//! Rendering is pure CPU `f64` math with fixed supersampling and no
//! randomness or clock (rows are split across threads, which changes no
//! byte), so a given build renders the same bytes on every run. Other
//! machines should agree but aren't guaranteed to: the trigonometry (in
//! `LonLat` and the marker and frame-code geometry) comes from the
//! platform's libm, which may round the last bit differently and so flip
//! a sample at an edge. Agreement across machines has not been
//! checked, so tests compare against frames rendered by the same build,
//! not stored ones.

#![forbid(unsafe_code)]

use glam::DVec3;
use spherecut_sphere::{LonLat, equirect};

pub mod audio;
pub mod fake;
pub mod lens;
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
    /// The stitching round trip's scene (M3 plan 4.2): tilted great
    /// circles across both of a back-to-back rig's side seams (the great
    /// circle through ±X and ±Y, where lenses looking along ∓Z meet), a
    /// 1° checker within ±15° of that seam, and a luminance ramp for the
    /// exposure checks.
    SeamLines,
}

impl Pattern {
    pub const ALL: [Pattern; 4] = [
        Pattern::Grid,
        Pattern::HighFrequency,
        Pattern::Seam,
        Pattern::SeamLines,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Pattern::Grid => "grid",
            Pattern::HighFrequency => "hifreq",
            Pattern::Seam => "seam",
            Pattern::SeamLines => "seamlines",
        }
    }
}

/// Display-referred RGB in `[0, 1]`.
pub type Rgb = [f64; 3];

/// Axis marker colors, used by orientation tests. Each disc also carries
/// its letter from [`AXIS_LABELS`]; the centre of the disc stays its color.
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

/// The letter in each axis disc, in [`AXIS_MARKERS`] order.
pub const AXIS_LABELS: [char; 6] = ['F', 'R', 'B', 'L', 'U', 'D'];

/// Which way is up in each label, seen from the centre of the sphere. The
/// side labels are upright; up and down read as they do after tilting the
/// head from the front, so their tops point back and front.
pub const AXIS_LABEL_UP: [DVec3; 6] = [
    DVec3::Y,
    DVec3::Y,
    DVec3::Y,
    DVec3::Y,
    DVec3::Z,
    DVec3::NEG_Z,
];

/// Side of a label cell, as the tangent of its angle at the sphere's centre
/// (about 1.6°). A 5×7-cell glyph then fits inside its disc.
pub const AXIS_LABEL_CELL: f64 = 0.028;

/// 5×7 glyphs for [`AXIS_LABELS`], row by row from the top, side by side.
/// The centre cell (row 3, column 2) is empty in all six, so a marker's
/// centre direction keeps the marker color.
const AXIS_GLYPHS: [[&str; 6]; 7] = [
    //  F        R        B        L        U        D
    ["#####", "####.", "####.", "#....", "#...#", "###.."],
    ["#....", "#...#", "#...#", "#....", "#...#", "#..#."],
    ["####.", "####.", "####.", "#....", "#...#", "#...#"],
    ["#....", "##...", "#...#", "#....", "#...#", "#...#"],
    ["#....", "#.#..", "#...#", "#....", "#...#", "#...#"],
    ["#....", "#..#.", "#...#", "#....", "#...#", "#..#."],
    ["#....", "#...#", "####.", "#####", ".###.", "###.."],
];

/// The ink of a label: black on light discs, white on dark ones.
pub fn axis_label_ink(disc: Rgb) -> Rgb {
    let luma = 0.2126 * disc[0] + 0.7152 * disc[1] + 0.0722 * disc[2];
    if luma > 0.5 { [0.0; 3] } else { [1.0; 3] }
}

/// The label cell `(column, row)` that `d` falls in for marker `i`, given
/// `along = d·axis > 0`. Gnomonic coordinates: plain arithmetic, so the
/// glyphs render the same everywhere.
pub fn axis_label_cell(i: usize, d: DVec3, along: f64) -> Option<(usize, usize)> {
    let (axis, _) = AXIS_MARKERS.get(i)?;
    let up = AXIS_LABEL_UP[i];
    let right = axis.cross(up);
    let x = d.dot(right) / along / AXIS_LABEL_CELL + 2.5;
    let y = 3.5 - d.dot(up) / along / AXIS_LABEL_CELL;
    ((0.0..5.0).contains(&x) && (0.0..7.0).contains(&y)).then_some((x as usize, y as usize))
}

/// Whether cell `(column, row)` of marker `i`'s glyph is inked.
pub fn axis_label_inked(i: usize, (col, row): (usize, usize)) -> bool {
    AXIS_GLYPHS
        .get(row)
        .and_then(|r| r.get(i))
        .and_then(|g| g.as_bytes().get(col))
        == Some(&b'#')
}

/// Frame counter: bit `i` is a square centred at latitude −20°,
/// longitude `FRAME_CODE_LON0 + i·FRAME_CODE_STEP` degrees. White = 1.
pub const FRAME_CODE_BITS: u32 = 16;
pub const FRAME_CODE_LAT: f64 = -20.0;
pub const FRAME_CODE_LON0: f64 = -45.0;
pub const FRAME_CODE_STEP: f64 = 6.0;
const FRAME_CODE_HALF: f64 = 2.2;

/// `SeamLines`' great circles, as `(α, τ)` in degrees. Each crosses the
/// seam, the great circle z = 0, at azimuth α (atan2(y, x), from +X
/// towards +Y) and again at α + 180°, so every one crosses both side
/// seams, near +X and near −X. At α it runs at τ to the seam, rising
/// towards +Z as α grows for τ > 0. Neighbours tilt opposite ways, so
/// that a misregistration shows along the seam as well as across it, and
/// are far enough apart that no two meet within the checker band.
pub const SEAM_LINES: [(f64, f64); 5] = [
    (-48.0, 55.0),
    (-24.0, -55.0),
    (0.0, 55.0),
    (24.0, -55.0),
    (48.0, 55.0),
];
/// Half the width of a `SeamLines` line, in degrees.
pub const SEAM_LINE_HALF_WIDTH: f64 = 0.3;
/// The colors of `SeamLines`' lines, taken in turn. No channel passes
/// [`SEAM_LINES_PEAK`].
pub const SEAM_LINE_COLORS: [Rgb; 2] = [[0.8, 0.1, 0.1], [0.1, 0.4, 0.8]];
/// How far `SeamLines`' 1° checker reaches each side of the seam, in
/// degrees.
pub const SEAM_CHECKER_BAND: f64 = 15.0;
/// How far a `SeamLines` checker cell lies above or below the ramp.
pub const SEAM_CHECKER_CONTRAST: f64 = 0.15;

/// The brightest value `SeamLines` shows on any channel: the brightest
/// line channel, and a bright checker cell at −X. 4.2's exposure control
/// puts lens 1 at +0.5 EV and +5 % red, which takes a display value above
/// 0.840 past white (in linear light 1/(√2 · 1.05) = 0.673), so the scene
/// stays below that, and lens 1 can be matched back to lens 0 everywhere
/// on it. Its darkest value, a dark cell at +X, is 0.05, above the 0.01
/// that section 16's estimator leaves out.
pub const SEAM_LINES_PEAK: f64 = 0.8;

/// `SeamLines`' luminance ramp at azimuth `along` degrees: 0.2 at +X up
/// to 0.65 at −X, the same either way round, so it has no step. With the
/// checker's contrast it reaches [`SEAM_LINES_PEAK`].
pub fn seam_ramp(along: f64) -> f64 {
    0.2 + 0.45 * along.abs() / 180.0
}

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
            for (i, (axis, c)) in AXIS_MARKERS.into_iter().enumerate() {
                let along = d.dot(axis);
                if along > cos_axis {
                    return match axis_label_cell(i, d, along) {
                        Some(cell) if axis_label_inked(i, cell) => axis_label_ink(c),
                        _ => c,
                    };
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
        Pattern::SeamLines => {
            // A line's plane has the normal cos τ ẑ − sin τ e, where e =
            // (−sin α, cos α, 0) runs along the seam at its crossing.
            let width = SEAM_LINE_HALF_WIDTH.to_radians().sin();
            for (k, (along, tilt)) in SEAM_LINES.into_iter().enumerate() {
                let (sa, ca) = along.to_radians().sin_cos();
                let (st, ct) = tilt.to_radians().sin_cos();
                if d.dot(DVec3::new(st * sa, -st * ca, ct)).abs() < width {
                    return SEAM_LINE_COLORS[k % SEAM_LINE_COLORS.len()];
                }
            }
            // Azimuth along the seam and elevation off it, in degrees.
            let along = deg(d.y.atan2(d.x));
            let off = deg(d.z.clamp(-1.0, 1.0).asin());
            let level = seam_ramp(along);
            if off.abs() < SEAM_CHECKER_BAND {
                let cell = (along.floor() as i64 + off.floor() as i64).rem_euclid(2);
                let sign = if cell == 0 { 1.0 } else { -1.0 };
                [level + sign * SEAM_CHECKER_CONTRAST; 3]
            } else {
                [level; 3]
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

/// The supersampled analytic truth of a view (M1 plan section 6): each
/// of the `size` output pixels is the mean of `n × n` samples of
/// `pattern` itself at frame 0, not of an equirect texture of it, at
/// the points `(i + (a + ½) / n, j + (b + ½) / n)` for `a, b < n`.
/// `view_fn` maps such a point, in pixels from the top-left corner (a
/// pixel's centre at i + 0.5), to the direction seen there, or to `None`
/// where the view shows nothing (a fisheye past θ = π), which counts as
/// black. The render goldens hold minified views to it: what a renderer
/// that doesn't alias would show. Rows are dealt to threads in turn, so
/// a view whose costly rows bunch together still spreads; the result
/// doesn't depend on how many threads there are.
pub fn render_view_ideal(
    pattern: Pattern,
    view_fn: impl Fn(f64, f64) -> Option<DVec3> + Sync,
    size: (u32, u32),
    n: u32,
) -> RgbImage {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(16);
    ideal_on_threads(pattern, view_fn, size, n, threads)
}

/// [`render_view_ideal`] on `threads` threads.
fn ideal_on_threads(
    pattern: Pattern,
    view_fn: impl Fn(f64, f64) -> Option<DVec3> + Sync,
    size: (u32, u32),
    n: u32,
    threads: usize,
) -> RgbImage {
    let (width, height) = size;
    let n = n.max(1);
    let threads = threads.max(1);
    let inv = 1.0 / f64::from(n * n);
    let mut pixels = vec![[0.0; 3]; width as usize * height as usize];
    let mut dealt: Vec<Vec<(usize, &mut [Rgb])>> = (0..threads).map(|_| Vec::new()).collect();
    for (j, row) in pixels.chunks_mut(width.max(1) as usize).enumerate() {
        dealt[j % threads].push((j, row));
    }
    let view_fn = &view_fn;
    std::thread::scope(|s| {
        for rows in dealt {
            s.spawn(move || {
                for (j, row) in rows {
                    for (i, px) in row.iter_mut().enumerate() {
                        let mut acc = [0.0; 3];
                        for b in 0..n {
                            for a in 0..n {
                                let x = i as f64 + (f64::from(a) + 0.5) / f64::from(n);
                                let y = j as f64 + (f64::from(b) + 0.5) / f64::from(n);
                                if let Some(d) = view_fn(x, y) {
                                    let c = color(pattern, d, 0);
                                    for (acc, c) in acc.iter_mut().zip(c) {
                                        *acc += c;
                                    }
                                }
                            }
                        }
                        *px = acc.map(|v| v * inv);
                    }
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
    use spherecut_testkit::control::must_fail;

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
    fn axis_labels_are_drawn_in_their_discs() {
        for (i, (axis, c)) in AXIS_MARKERS.into_iter().enumerate() {
            let up = AXIS_LABEL_UP[i];
            let right = axis.cross(up);
            let mut glyph = String::new();
            for row in 0..7 {
                for col in 0..5 {
                    // Cell centre, in the disc's tangent plane.
                    let x = (col as f64 - 2.0) * AXIS_LABEL_CELL;
                    let y = (3.0 - row as f64) * AXIS_LABEL_CELL;
                    let d = (axis + right * x + up * y).normalize();
                    let got = color(Pattern::Grid, d, 0);
                    glyph.push(if got == c {
                        '.'
                    } else {
                        assert_eq!(got, axis_label_ink(c), "{} ({col}, {row})", AXIS_LABELS[i]);
                        '#'
                    });
                }
                glyph.push('\n');
            }
            let want: String = AXIS_GLYPHS.iter().map(|r| format!("{}\n", r[i])).collect();
            assert_eq!(glyph, want, "{}", AXIS_LABELS[i]);
            // Inked somewhere, and never at the centre.
            assert!(glyph.contains('#'));
            assert_eq!(&glyph[3 * 6 + 2..3 * 6 + 3], ".");
            assert_ne!(axis_label_ink(c), c);
        }
    }

    #[test]
    fn frame_code_round_trips() {
        for n in [0u32, 1, 2, 5, 255, 1234, 65535] {
            let img = render_equirect(Pattern::Grid, 720, 360, n, 1);
            assert_eq!(decode_frame_code(&img), Some(n));
        }
    }

    /// A view of the 1° checker in which pixel column `i` spans longitude
    /// `[i + shift, i + 1 + shift)` degrees and every row lies inside one
    /// cell's latitudes, (10°, 11°): a pixel per cell, offset by `shift`.
    fn checker_strip(shift: f64) -> impl Fn(f64, f64) -> Option<DVec3> + Sync {
        move |x: f64, y: f64| {
            Some(
                LonLat {
                    lon: (x + shift).to_radians(),
                    lat: (10.0 + y.fract().clamp(0.01, 0.99)).to_radians(),
                }
                .to_dir(),
            )
        }
    }

    #[test]
    fn the_ideal_is_the_mean_of_the_pattern_over_each_pixel() {
        // Aligned with the cells, each pixel is one cell: 0.9 and 0.1 in
        // turn. Half a cell off, each pixel is half of two: their mean,
        // 0.5, for any even n. n = 1 samples the centre only, one cell.
        let aligned = render_view_ideal(Pattern::HighFrequency, checker_strip(0.0), (8, 2), 4);
        for x in 0..8 {
            let want = if x % 2 == 0 { 0.9 } else { 0.1 };
            for y in 0..2 {
                for c in aligned.get(x, y) {
                    assert!((c - want).abs() < 1e-12, "({x}, {y}): {c}");
                }
            }
        }
        let halves = render_view_ideal(Pattern::HighFrequency, checker_strip(0.5), (8, 2), 4);
        assert!(
            halves
                .pixels
                .iter()
                .flatten()
                .all(|c| (c - 0.5).abs() < 1e-12)
        );
        let centre = render_view_ideal(Pattern::HighFrequency, checker_strip(0.25), (8, 1), 1);
        assert_eq!(centre.get(0, 0), [0.9; 3]);
        assert_eq!(centre.get(1, 0), [0.1; 3]);
        // A third of a cell off with 3 × 3 samples: two thirds of one
        // cell and one third of the next.
        let thirds = render_view_ideal(Pattern::HighFrequency, checker_strip(1.0 / 3.0), (2, 1), 3);
        assert!((thirds.get(0, 0)[0] - (2.0 * 0.9 + 0.1) / 3.0).abs() < 1e-12);
        assert!((thirds.get(1, 0)[0] - (2.0 * 0.1 + 0.9) / 3.0).abs() < 1e-12);
    }

    #[test]
    fn the_ideal_shows_black_where_the_view_shows_nothing() {
        // Only the left half of each pixel shows the front marker (red).
        let half = |x: f64, _y: f64| (x.fract() < 0.5).then_some(DVec3::NEG_Z);
        let img = render_view_ideal(Pattern::Grid, half, (3, 2), 4);
        for p in &img.pixels {
            assert_eq!(*p, [0.5, 0.0, 0.0]);
        }
        let none = render_view_ideal(Pattern::Grid, |_, _| None, (2, 2), 2);
        assert!(none.pixels.iter().flatten().all(|&c| c == 0.0));
        assert_eq!(
            render_view_ideal(Pattern::Grid, |_, _| None, (0, 3), 2)
                .pixels
                .len(),
            0
        );
    }

    #[test]
    fn the_ideal_is_the_same_on_every_run() {
        // Rows are dealt to threads in turn and each pixel is worked out
        // on its own, so neither the run nor the thread count changes a
        // byte, and a shorter picture agrees on the rows it shares.
        let view = |x: f64, y: f64| {
            Some(
                LonLat {
                    lon: (x * 3.7).to_radians(),
                    lat: (y * 2.9 - 40.0).to_radians(),
                }
                .to_dir(),
            )
        };
        let a = render_view_ideal(Pattern::Grid, view, (24, 37), 3);
        let b = render_view_ideal(Pattern::Grid, view, (24, 37), 3);
        assert_eq!(a.pixels, b.pixels);
        for threads in [1, 3, 16, 40] {
            let c = ideal_on_threads(Pattern::Grid, view, (24, 37), 3, threads);
            assert_eq!(a.pixels, c.pixels, "{threads} threads");
        }
        let top = render_view_ideal(Pattern::Grid, view, (24, 2), 3);
        assert_eq!(top.pixels[..], a.pixels[..48]);
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

    /// The direction at azimuth `along` (from +X towards +Y) and elevation
    /// `off` off the seam z = 0, both in degrees.
    fn seam_dir(along: f64, off: f64) -> DVec3 {
        let (sa, ca) = along.to_radians().sin_cos();
        let (so, co) = off.to_radians().sin_cos();
        DVec3::new(co * ca, co * sa, so)
    }

    /// Whether `SeamLines` shows line `k`'s color at `along` and `off`.
    fn on_line(k: usize, along: f64, off: f64) -> bool {
        color(Pattern::SeamLines, seam_dir(along, off), 0) == SEAM_LINE_COLORS[k % 2]
    }

    /// `SeamLines` draws tilted great circles that cross both side seams
    /// (the great circle z = 0).
    #[test]
    fn seam_lines_cross_both_side_seams() {
        for (k, (along, tilt)) in SEAM_LINES.into_iter().enumerate() {
            // Within 50° of +X, so the far crossing is within 50° of −X.
            assert!(along.abs() <= 50.0, "line {k}");
            for far in [false, true] {
                let at = if far { along + 180.0 } else { along };
                assert!(on_line(k, at, 0.0), "line {k} at {at}°");
                // At elevation β the line lies at azimuth α + δ, where
                // its plane's normal (sin τ sin α, −sin τ cos α, cos τ)
                // meets cos β (cos(α + δ), sin(α + δ), 0) + sin β ẑ at a
                // right angle: cos τ sin β = sin τ cos β sin δ, so sin δ
                // = tan β / tan τ. On the far side sin(180° + δ) = −sin δ,
                // so the line leans the other way.
                for off in [-10.0f64, -5.0, 5.0, 10.0] {
                    let lean = (off.to_radians().tan() / tilt.to_radians().tan())
                        .asin()
                        .to_degrees();
                    let shift = if far { -lean } else { lean };
                    assert!(on_line(k, at + shift, off), "line {k} at {at}°, {off}°");
                    // Control: a line square to the seam would pass here.
                    must_fail("a line square to the seam", on_line(k, at, off));
                }
            }
            // Control: half a degree along the seam is 0.5° · sin 55° =
            // 0.41° from the line, beyond its half-width of 0.3°.
            must_fail("half a degree off the line", on_line(k, along + 0.5, 0.0));
        }
    }

    /// Whether `pattern` shows `SeamLines`' checker at `along` and `off`:
    /// gray, the ramp plus or minus the contrast, with the same sign
    /// across the cell and the other a cell along and a cell across.
    fn checker_at(pattern: Pattern, along: f64, off: f64) -> bool {
        let at = |a: f64, o: f64| color(pattern, seam_dir(a, o), 0);
        let term = |a: f64, o: f64| at(a, o)[0] - seam_ramp(a);
        let c = at(along, off);
        let across = if off + 1.0 < SEAM_CHECKER_BAND {
            off + 1.0
        } else {
            off - 1.0
        };
        c[0] == c[1]
            && c[1] == c[2]
            && (term(along, off).abs() - SEAM_CHECKER_CONTRAST).abs() < 1e-12
            && (term(along + 0.3, off + 0.3) - term(along, off)).abs() < 1e-12
            && (term(along + 1.0, off) + term(along, off)).abs() < 1e-12
            && (term(along, across) + term(along, off)).abs() < 1e-12
    }

    /// `SeamLines` holds a 1° checker within ±15° of the seam and a
    /// luminance ramp beyond it.
    #[test]
    fn seam_lines_has_a_checker_near_the_seam_and_a_ramp() {
        // No line comes within 15° of the seam between 62° and 118° of
        // azimuth either way round: a line at τ = 55° reaches 15° off the
        // seam asin(tan 15° / tan 55°) = 10.8° along it, so the lines
        // crossing at 48° and 132° (48° + 180°) stop at 58.8° and 121.2°.
        for along in [62.5, 80.5, 100.5, 116.5, -63.5, -99.5] {
            for off in [-14.5, -7.5, -0.5, 0.5, 7.5, 14.5] {
                assert!(
                    checker_at(Pattern::SeamLines, along, off),
                    "{along}°, {off}°"
                );
            }
            // Beyond the band, the ramp alone.
            for off in [15.5, 16.5, 40.0, -15.5, -60.0] {
                // The azimuth comes back from the direction within a few
                // ulps, so the level does too.
                let c = color(Pattern::SeamLines, seam_dir(along, off), 0);
                assert!(
                    c.iter().all(|v| (v - seam_ramp(along)).abs() < 1e-12),
                    "{along}°, {off}°: {c:?}"
                );
                // Control: no checker there.
                must_fail(
                    "a checker beyond the band",
                    checker_at(Pattern::SeamLines, along, off),
                );
            }
        }
        // The ramp runs from 0.2 at +X to 0.65 at −X, through 0.425 at
        // ±Y, the same either way round, so it has no step. Its bright
        // cells at −X reach 0.65 + 0.15, the peak.
        assert_eq!(seam_ramp(0.0), 0.2);
        assert!((seam_ramp(90.0) - 0.425).abs() < 1e-15);
        assert_eq!(seam_ramp(-90.0), seam_ramp(90.0));
        assert!((seam_ramp(180.0) - 0.65).abs() < 1e-15);
        assert_eq!(seam_ramp(-180.0), seam_ramp(180.0));
        assert!((seam_ramp(180.0) + SEAM_CHECKER_CONTRAST - SEAM_LINES_PEAK).abs() < 1e-15);
        let far_off = |along: f64| color(Pattern::SeamLines, seam_dir(along, 30.0), 0)[0];
        assert!(far_off(100.0) > far_off(80.0));
        assert!((far_off(100.0) - far_off(80.0) - 0.45 * 20.0 / 180.0).abs() < 1e-12);
        // Control: the Seam pattern has no checker there.
        must_fail(
            "the Seam pattern's checker",
            checker_at(Pattern::Seam, 80.5, 0.5),
        );
    }
}
