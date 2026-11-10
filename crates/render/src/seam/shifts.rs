//! The shifts between the two lenses' strips, bin by bin, and the
//! signature fitted to them (M3 plan 8.2).
//!
//! A bin's **patch** is the part of lens 0's strip it measures from
//! ([`patch`]): the columns whose centres lie in the bin's φ, and the
//! rows that a search of ±`search_deg` can't carry off lens 1's strip.
//! The bin's shift is where lens 1's strip shows that patch.
//!
//! Both strips are first smoothed alike, luma and each channel: a 5 × 5
//! binomial blur, then a cubic B-spline with the blurred samples as its
//! coefficients, which makes each strip a surface that can be read
//! anywhere. Lens 0 is read at its pixels and lens 1 where the shift puts
//! them, both off surfaces made the same way, so that no interpolation
//! sits on one side of the comparison only, and what a 1° checker
//! aliases at the strip's scale is smoothed out of both.
//!
//! A sample is left out of both strips' smoothing, with those its
//! smoothing would reach, where its lens doesn't cover it or where a
//! channel is above 0.95: a clipped sample has lost its shape. A sample
//! below 0.01 keeps its place in the shift, where normalising for gain
//! and bias makes a dark patch as good as a bright one, and is left out
//! of the log ratio alone.

use std::collections::VecDeque;
use std::ops::Range;

use super::{BinShift, Signature, Strip, StripSpec, Strips};

/// How [`bin_shifts_by`] measures a bin. [`Method::PLAN`] is 8.2's, which
/// [`bin_shifts`] uses; each field switches one part off, for the control
/// of the test that holds that part to what it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method {
    /// How many Lucas–Kanade steps refine the coarse shift. With none,
    /// the shift is the coarse search's own: a whole number of strip
    /// pixels, or the neighbour's shift where a bin starts from that and
    /// matches there.
    pub lk_steps: u32,
    /// Whether the steps run on patches normalised for gain and bias.
    /// Off, they compare the strips' values as they stand.
    pub normalise: bool,
    /// Whether a bin with too few samples or too little texture, or one
    /// whose steps end unsettled or at no match, is marked invalid. Off,
    /// such a bin is passed off as measured: with a zero shift or, where
    /// it took its steps, with wherever they stopped.
    pub validity: bool,
    /// Whether a bin whose texture doesn't stand clear of its mismatch
    /// ([`NOISE_FLOOR`]: a flat patch or a ramp under noise) carries no
    /// information, or none the way it doesn't, and places no neighbour.
    /// Off, such a bin weighs in a fit by its whole normal matrix and
    /// hands its shift on, as any other.
    pub noise_floor: bool,
}

impl Method {
    /// 8.2's: three steps on normalised patches, a bin that measures
    /// nothing invalid, and one that measures only its noise without
    /// information.
    pub const PLAN: Method = Method {
        lk_steps: 3,
        normalise: true,
        validity: true,
        noise_floor: true,
    };
}

/// The samples of lens 0's strip that one bin measures from, in the
/// strip's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The columns whose centres lie in the bin's span of φ.
    pub columns: Range<u32>,
    /// The rows from which the search, the steps after it and the
    /// smoothing never reach past lens 1's strip: the band's middle, |ε|
    /// up to about `half_band_deg` − `search_deg` less four rows' share.
    /// Empty when the band is too narrow for the search.
    pub rows: Range<u32>,
}

/// How many bins `spec` divides the turn of φ into: 72 at 5°. Never
/// more than its strip has columns, and one where `bin_deg` isn't a
/// positive number of degrees.
pub fn bin_count(spec: &StripSpec) -> usize {
    if !(spec.bin_deg > 0.0 && spec.bin_deg.is_finite()) {
        return 1;
    }
    let columns = (spec.size().0 as usize).max(1);
    let bins = (360.0 / spec.bin_deg).round();
    if bins >= 1.0 {
        (bins as usize).min(columns)
    } else {
        1
    }
}

/// Bin `bin`'s patch on a strip of `spec`'s size. What predicts a bin's
/// shift (8.3) averages over the same patch.
pub fn patch(spec: &StripSpec, bin: usize) -> Patch {
    let (width, height) = spec.size();
    Layout::new(spec, width as usize, height as usize).patch(bin)
}

/// The shift between the two strips in each bin of `spec.bin_deg` in φ
/// (72 bins of 5°), in order from φ = 0, with [`BinShift`]'s signs. Each
/// bin finds a coarse shift by NCC over ±`spec.search_deg`, refines it
/// with three Lucas–Kanade steps on patches normalised for gain and bias,
/// and keeps the steps' 2 × 2 normal matrix as the shift's information.
/// A bin without texture is marked invalid, never given a zero shift.
///
/// The strips are `spec`'s, both of its size's shape; of strips that
/// differ in size, or too small to search, every bin is invalid. A bin's
/// `phi_deg` is the middle of its patch's columns ([`patch`]), which a
/// strip's whole pixels leave up to half a pixel from the bin's own. A
/// bin that has no column of the strip (an empty strip, or one of fewer
/// columns than bins) has the middle of its own span of φ, never NaN.
///
/// The search runs on luma, over whole pixels across and along. Where a
/// bin's texture repeats (a checker's cells) or runs along one line,
/// several shifts match alike. The bins with one match only, and no
/// sample left out, start their steps from it and, where they measure
/// with information both ways (below), hand it to their neighbours,
/// and so on round the seam: a bin takes its match, of those within
/// 0.05 of its best, nearest the shift it is handed. With none within
/// 0.5° of that, a bin with one match keeps it, if its search had an NCC
/// at every shift to hold the match against (a step in the field, as a
/// wall beside things further off leaves, which a sample left out
/// mustn't smooth over); any other bin starts its steps from the handed
/// shift itself, and measures only if they end at a match of its own. A
/// bin hands its own match on only where it measured with information
/// both ways; any other hands on the shift it was handed. When no bin
/// with one match and no sample left out measures so, each takes its
/// match nearest zero: a texture that repeats everywhere comes back at
/// the match nearest zero.
///
/// **Noise.** Under noise a flat patch, or a ramp, has a texture of its
/// own, the noise's, and a best match in it: such a bin settles and
/// comes back valid, with a shift of several pixels that means nothing.
/// A bin measures **with information** only where the texture of its
/// normalised patch, the greater eigenvalue of its normal matrix per
/// sample, is at least [`NOISE_FLOOR`] times its mismatch 1 − NCC.
/// Under that the bin stays valid, with its shift, its NCC, its
/// contrasts and its log gains, but its information is zero, no
/// neighbour takes a shift from it, and [`signature`] fits nothing to
/// it ([`BinShift::informed`]). It stays valid so that a seam whose bins
/// match nothing shows in the median NCC (8.2's gross failure), which
/// only a bin that claims a match can lower. The floor is held each way:
/// where only the lesser eigenvalue is under it (a horizon across a
/// noisy sky, whose match along the horizon is the noise's), the bin
/// keeps its information across that line and none along it, and places
/// no neighbour.
///
/// The floor doesn't see a bias. Where a patch's only texture lies at
/// its edge, beside a flat stretch under noise, the NCC is best a little
/// towards the texture, since more of it in lens 1's patch lifts the
/// patch clear of its noise: bins that hold three columns of a checker
/// beside a flat grey under 1.5 % noise came back 0.4 to 1.1 px that
/// way, with information and an NCC of 0.82 to 0.96 (`tests/seam.rs`).
///
/// **The floors.** The 0.95 and the 0.01 are 8.2's. Every other one (the
/// contrast of 0.2 %, the tie of 0.05, the 0.5°, the settled tenth of a
/// pixel, a patch's half, the noise floor) was set on synthetic strips,
/// without noise or with seeded white noise, and none has met real
/// footage: a fit that reads these bins weights each by its information
/// and gates it on its own NCC ([`BinShift`]).
///
/// An invalid bin's shift, NCC, contrasts and log gains are NaN and its
/// information zero. It is invalid when fewer than half its patch's
/// samples are usable on both lenses, when either lens's patch has a
/// contrast (its standard deviation over its mean) under 0.2 %, or when
/// the normalised patch's gradients carry no shift: a flat patch, or a
/// ramp, which a bias would explain as well. It is invalid too when its
/// steps haven't settled, the last of them moving the shift, as the
/// patch's best-informed direction counts it, by more than a tenth of a
/// strip pixel; and when steps from a handed shift end with an NCC more
/// than 0.05 under its best match's, at no match of the bin's own. Each
/// log gain is the median over the patch's samples of ln(lens 1 / lens 0)
/// at the measured shift, both read off the smoothed strips, a sample
/// below 0.01 on either lens left out, and NaN with fewer than 16
/// samples.
///
/// A shift past the search isn't detected. Such a bin comes back valid
/// at whatever matches best inside the search: on a texture that repeats
/// an alias of its shift, with an NCC as high as a true match's. Only
/// what predicts the field (8.3) knows which bins those are.
pub fn bin_shifts(strips: &Strips, spec: &StripSpec) -> Vec<BinShift> {
    bin_shifts_by(strips, spec, &Method::PLAN)
}

/// [`bin_shifts`] by `method`.
pub fn bin_shifts_by(strips: &Strips, spec: &StripSpec, method: &Method) -> Vec<BinShift> {
    let [lens0, lens1] = &strips.lenses;
    let (w, h) = (lens0.width as usize, lens0.height as usize);
    let layout = Layout::new(spec, w, h);
    let same = (lens1.width as usize, lens1.height as usize) == (w, h);
    let whole = |s: &Strip| s.rgb.len() == w * h && s.valid.len() == w * h;
    let patches: Vec<Patch> = (0..layout.bins).map(|k| layout.patch(k)).collect();
    let measurable = same && whole(lens0) && whole(lens1) && w >= 8 && !layout.rows.is_empty();
    let measured: Vec<Option<Measured>> = if measurable {
        let surfaces = [Surface::smoothed(lens0), Surface::smoothed(lens1)];
        measure(&surfaces, &layout, &patches, method)
    } else {
        vec![None; layout.bins]
    };
    let (sx, sy) = (layout.sx, layout.sy);
    measured
        .iter()
        .zip(&patches)
        .enumerate()
        .map(|(k, (m, patch))| {
            let phi_deg = if patch.columns.is_empty() {
                // No column of the strip is the bin's: its own middle.
                (k as f64 + 0.5) * 360.0 / layout.bins as f64
            } else {
                f64::from(patch.columns.start + patch.columns.end) / 2.0 / sx
            };
            match m {
                // The strip's y runs down, against ε.
                Some(m) => BinShift {
                    phi_deg,
                    across_deg: -m.shift[1] / sy,
                    along_deg: m.shift[0] / sx,
                    info: [
                        [sy * sy * m.normal[2], -sx * sy * m.normal[1]],
                        [-sx * sy * m.normal[1], sx * sx * m.normal[0]],
                    ],
                    ncc: m.ncc,
                    contrast: m.contrast,
                    log_gain: m.log_gain,
                    valid: true,
                },
                None if method.validity => BinShift {
                    phi_deg,
                    across_deg: f64::NAN,
                    along_deg: f64::NAN,
                    info: [[0.0; 2]; 2],
                    ncc: f64::NAN,
                    contrast: [f64::NAN; 2],
                    log_gain: [f64::NAN; 3],
                    valid: false,
                },
                // The control: nothing measured, passed off as a shift of
                // zero, as steps that found no gradient to move by leave
                // it.
                None => BinShift {
                    phi_deg,
                    across_deg: 0.0,
                    along_deg: 0.0,
                    info: [[0.0; 2]; 2],
                    ncc: 0.0,
                    contrast: [f64::NAN; 2],
                    log_gain: [f64::NAN; 3],
                    valid: true,
                },
            }
        })
        .collect()
}

/// A value above this on any channel is taken as clipped.
const CLIP: f64 = 0.95;
/// A value below this is left out of the log ratio.
const DARK: f64 = 0.01;
/// The least contrast, a patch's standard deviation over its mean, that
/// counts as texture.
const MIN_CONTRAST: f64 = 0.002;
/// The least mean square gradient of a normalised patch, per sample and
/// in strip pixels, that counts as texture.
const MIN_TEXTURE: f64 = 1e-4;
/// The least texture of a bin that measures with information, as a
/// multiple of its mismatch 1 − NCC: the texture is the greater
/// eigenvalue of the normalised patch's normal matrix per sample, in
/// strip pixels⁻². The lesser eigenvalue is held to the same, for the
/// information the other way.
///
/// The smoothing leaves white noise a texture of its own: a mean square
/// slope of 3/8 of its variance a pixel² (the blur and the spline spread
/// a sample over a variance of 4/3 px², and a slope's share is one over
/// twice that), whatever the noise's size. So a patch of noise alone,
/// whose best match in the search correlates by ρ, has 0.375 / (1 − ρ)
/// times its mismatch, and a true texture under noise has 0.375 more
/// than its own slopes are of the noise's.
///
/// The figures below are from synthetic strips with seeded white noise
/// on luma, independent on the two lenses and in proportion to each
/// sample's value; none is from footage. With the floor off, over 40
/// seeds of Tier 1's 72 bins, a flat grey under 1 % to 4 % noise comes
/// back valid in 1104 to 1146 bins of 2880, at 0.38 to 0.92 times its
/// mismatch and a median of 10 px from its shift; a ramp across the band
/// (0.35 to 0.65) in 2873 to 2876, at 0.23 to 0.95 times, a median of
/// 3 px off, with an NCC over 0.98. At Tier 2's 8 px/°, over 8 seeds,
/// the patches are larger and ρ is less: 0.31 to 0.51 times. So 3 is
/// three times the most that noise alone reached, in 18,000 such bins.
///
/// What it costs is the faintest texture. Waves a few pixels long, of a
/// fifth of the mean, have at the least 190, 48, 22 and 5.7 times their
/// mismatch under 1 %, 2 %, 3 % and 6 % noise at Tier 1 (their shifts
/// 0.05, 0.09, 0.14 and 0.31 px off at most), and 57, 15, 6.6 and 1.9
/// times at Tier 2, where the same waves are 2.1 times as long in
/// pixels: there, under 6 % noise, some bins fall under the floor with
/// shifts within 0.27 px. The same waves at a fiftieth of the contrast
/// under 0.2 % noise have 2.4 to 16 times at Tier 1, a median of 6.7,
/// and shifts up to 0.5 px off: a bin at the floor measures to about
/// that. On the attribution matrix's own strips (`tests/instrument.rs`),
/// which have two samplings' aliasing for noise, a true model's bins
/// have 12 times or more, a planted difference's 7 or more but for a
/// bin in one row at 2.96 (under the floor) and one in another at 3.6,
/// and another scene's 0.05 to 0.3.
pub const NOISE_FLOOR: f64 = 3.0;
/// The fewest samples a channel's log ratio is the median of.
const MIN_GAIN_SAMPLES: usize = 16;
/// Matches whose NCC lies within this of a bin's best are tied with it.
const TIE: f64 = 0.05;
/// Peaks no further apart than this, in strip pixels, are one match.
const ONE_MATCH: f64 = 1.5;
/// A bin with several matches takes one only within this many degrees of
/// the shift its neighbour hands it: more than a field that the search
/// can hold changes from one bin to the next, and less than half the way
/// to a 1° checker's next match.
const NEAR_DEG: f64 = 0.5;
/// The longest Lucas–Kanade step, in strip pixels, and how far past the
/// search the steps may carry a shift.
const MAX_STEP: f64 = 1.0;
/// The steps' damping, as a share of the normal matrix's trace: it holds
/// a step still along a line, where the matrix has nothing to say.
const DAMPING: f64 = 1e-6;
/// The longest last step of a bin that has settled, in strip pixels as
/// the patch's best-informed direction counts them ([`informed_length`]).
/// The last of three steps from a match is under 0.002 on the
/// instrument's own tests, and 0.04 on patches not normalised; steps
/// from a shift that isn't the bin's own are still moving by most of a
/// pixel.
const SETTLED: f64 = 0.1;
/// The rows at each edge of a strip that the smoothing doesn't reach: two
/// for the blur and one more for the spline.
const EDGE_ROWS: usize = 3;
/// The 5-tap binomial blur.
const BLUR: [f64; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
/// The cubic B-spline at a sample and at its two neighbours.
const SPLINE: [f64; 3] = [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0];

/// A strip pair's shape in pixels, and the search over it.
#[derive(Debug)]
struct Layout {
    w: usize,
    bins: usize,
    /// Pixels a degree along the strip and across it.
    sx: f64,
    sy: f64,
    /// How far the coarse search looks each way, in whole pixels.
    rx: usize,
    ry: usize,
    /// Every patch's rows.
    rows: Range<usize>,
}

impl Layout {
    fn new(spec: &StripSpec, w: usize, h: usize) -> Layout {
        let sx = w as f64 / 360.0;
        let sy = h as f64 / (2.0 * spec.half_band_deg);
        let reach = |scale: f64, most: usize| {
            let r = (spec.search_deg * scale).ceil();
            if r >= 0.0 { (r as usize).min(most) } else { 0 }
        };
        let (rx, ry) = (reach(sx, w), reach(sy, h));
        // A row j reads lens 1 at j + v for |v| up to ry + MAX_STEP, and
        // the spline there reaches one row up and two down, inside the
        // rows the blur made.
        let top = ry + EDGE_ROWS + 1;
        let bottom = h.saturating_sub(ry + EDGE_ROWS + 2);
        Layout {
            w,
            bins: bin_count(spec),
            sx,
            sy,
            rx,
            ry,
            rows: top..bottom.max(top),
        }
    }

    fn patch(&self, bin: usize) -> Patch {
        // Pixel i's centre is at i + ½.
        let edge = |k: usize| {
            let x = k as f64 * self.w as f64 / self.bins as f64 - 0.5;
            (x.ceil().max(0.0) as usize).min(self.w) as u32
        };
        Patch {
            columns: edge(bin)..edge(bin + 1),
            rows: self.rows.start as u32..self.rows.end as u32,
        }
    }
}

/// A strip, smoothed: the coefficients of a cubic B-spline surface, luma
/// first and then R, G and B, each the 5 × 5 binomial blur of the strip's
/// samples. The surface wraps around in x, as φ does.
#[derive(Debug)]
struct Surface {
    w: usize,
    h: usize,
    c: Vec<[f64; 4]>,
    /// Whether each coefficient was made of usable samples only.
    ok: Vec<bool>,
}

impl Surface {
    fn smoothed(strip: &Strip) -> Surface {
        let (w, h) = (strip.width as usize, strip.height as usize);
        let raw: Vec<([f64; 4], bool)> = (0..w * h)
            .map(|i| {
                let [r, g, b] = strip.rgb[i];
                if strip.valid[i] && [r, g, b].iter().all(|&v| v <= CLIP) {
                    ([0.2126 * r + 0.7152 * g + 0.0722 * b, r, g, b], true)
                } else {
                    ([0.0; 4], false)
                }
            })
            .collect();
        // Along each row, wrapping, then down each column.
        let mut rows = vec![([0.0; 4], false); w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0; 4];
                let mut all = true;
                for (t, weight) in BLUR.iter().enumerate() {
                    let (v, usable) = raw[y * w + (x + w + t - 2) % w];
                    all &= usable;
                    for (acc, v) in acc.iter_mut().zip(v) {
                        *acc += weight * v;
                    }
                }
                rows[y * w + x] = (acc, all);
            }
        }
        let mut c = vec![[0.0; 4]; w * h];
        let mut ok = vec![false; w * h];
        for y in 2..h.saturating_sub(2) {
            for x in 0..w {
                let mut acc = [0.0; 4];
                let mut all = true;
                for (t, weight) in BLUR.iter().enumerate() {
                    let (v, usable) = rows[(y + t - 2) * w + x];
                    all &= usable;
                    for (acc, v) in acc.iter_mut().zip(v) {
                        *acc += weight * v;
                    }
                }
                if all {
                    c[y * w + x] = acc;
                    ok[y * w + x] = true;
                }
            }
        }
        Surface { w, h, c, ok }
    }

    /// The surface at the sample (i, j): channel `k`, or `None` where a
    /// coefficient it reads isn't usable.
    fn at_sample(&self, i: usize, j: usize, k: usize) -> Option<f64> {
        if j < 1 || j + 1 >= self.h {
            return None;
        }
        let mut acc = 0.0;
        for (b, wy) in SPLINE.iter().enumerate() {
            for (a, wx) in SPLINE.iter().enumerate() {
                let at = (j + b - 1) * self.w + (i + self.w + a - 1) % self.w;
                if !self.ok[at] {
                    return None;
                }
                acc += wx * wy * self.c[at][k];
            }
        }
        Some(acc)
    }

    /// The 4 × 4 coefficients the surface reads at (x, y), in samples
    /// (sample i at x = i), with their weights and the weights of the
    /// surface's slope each way, or `None` where one isn't usable.
    fn taps(&self, x: f64, y: f64) -> Option<Taps> {
        let (fx, fy) = (x.floor(), y.floor());
        if !(fx.is_finite() && fy >= 1.0 && fy + 2.0 < self.h as f64) {
            return None;
        }
        let row = fy as usize - 1;
        let first = (fx as i64 - 1).rem_euclid(self.w as i64) as usize;
        let columns = [0, 1, 2, 3].map(|t| (first + t) % self.w);
        for b in 0..4 {
            if columns
                .iter()
                .any(|column| !self.ok[(row + b) * self.w + column])
            {
                return None;
            }
        }
        let (wx, dx) = spline_weights(x - fx);
        let (wy, dy) = spline_weights(y - fy);
        Some(Taps {
            columns,
            row,
            wx,
            dx,
            wy,
            dy,
        })
    }

    /// Luma at (x, y) and its slope there along x and along y, per
    /// sample.
    fn luma_at(&self, x: f64, y: f64) -> Option<[f64; 3]> {
        let taps = self.taps(x, y)?;
        let mut out = [0.0; 3];
        for (b, (wy, dy)) in taps.wy.iter().zip(taps.dy).enumerate() {
            let (mut value, mut slope) = (0.0, 0.0);
            for (column, (wx, dx)) in taps.columns.iter().zip(taps.wx.iter().zip(taps.dx)) {
                let c = self.c[(taps.row + b) * self.w + column][0];
                value += wx * c;
                slope += dx * c;
            }
            out[0] += wy * value;
            out[1] += wy * slope;
            out[2] += dy * value;
        }
        Some(out)
    }

    /// R, G and B at (x, y).
    fn rgb_at(&self, x: f64, y: f64) -> Option<[f64; 3]> {
        let taps = self.taps(x, y)?;
        let mut out = [0.0; 3];
        for (b, wy) in taps.wy.iter().enumerate() {
            for (column, wx) in taps.columns.iter().zip(taps.wx) {
                let c = &self.c[(taps.row + b) * self.w + column];
                for (out, c) in out.iter_mut().zip(&c[1..]) {
                    *out += wx * wy * c;
                }
            }
        }
        Some(out)
    }
}

/// The coefficients a surface reads at one point: four columns, wrapped,
/// of four rows from `row`, and their weights.
#[derive(Debug)]
struct Taps {
    columns: [usize; 4],
    row: usize,
    wx: [f64; 4],
    dx: [f64; 4],
    wy: [f64; 4],
    dy: [f64; 4],
}

/// The cubic B-spline's four weights a fraction `f` past a sample, for
/// the samples one before it to two after, and their derivatives in `f`.
fn spline_weights(f: f64) -> ([f64; 4], [f64; 4]) {
    let g = 1.0 - f;
    (
        [
            g * g * g / 6.0,
            (3.0 * f * f * f - 6.0 * f * f + 4.0) / 6.0,
            (-3.0 * f * f * f + 3.0 * f * f + 3.0 * f + 1.0) / 6.0,
            f * f * f / 6.0,
        ],
        [
            -g * g / 2.0,
            (3.0 * f * f - 4.0 * f) / 2.0,
            (-3.0 * f * f + 2.0 * f + 1.0) / 2.0,
            f * f / 2.0,
        ],
    )
}

/// Lens 1's luma surface at its samples, laid out for the coarse search:
/// `reach` columns wrapped on at each end, so that a patch moved by a
/// whole shift is a plain rectangle, and running sums, so that a
/// rectangle's count, sum and sum of squares each cost four reads.
#[derive(Debug)]
struct Searched {
    width: usize,
    value: Vec<f64>,
    usable: Vec<bool>,
    count: Vec<f64>,
    sum: Vec<f64>,
    squares: Vec<f64>,
}

impl Searched {
    fn new(surface: &Surface, reach: usize) -> Searched {
        let (w, h) = (surface.w, surface.h);
        let width = w + 2 * reach;
        let mut value = vec![0.0; width * h];
        let mut usable = vec![false; width * h];
        for j in 0..h {
            for i in 0..w {
                if let Some(v) = surface.at_sample(i, j, 0) {
                    // Every column that wraps onto column i.
                    let mut x = (i + reach) % w;
                    while x < width {
                        value[j * width + x] = v;
                        usable[j * width + x] = true;
                        x += w;
                    }
                }
            }
        }
        let stride = width + 1;
        let mut count = vec![0.0; stride * (h + 1)];
        let mut sum = vec![0.0; stride * (h + 1)];
        let mut squares = vec![0.0; stride * (h + 1)];
        for j in 0..h {
            let (mut n, mut s, mut q) = (0.0, 0.0, 0.0);
            for x in 0..width {
                let v = value[j * width + x];
                n += f64::from(u8::from(usable[j * width + x]));
                s += v;
                q += v * v;
                let (above, here) = (j * stride + x + 1, (j + 1) * stride + x + 1);
                count[here] = count[above] + n;
                sum[here] = sum[above] + s;
                squares[here] = squares[above] + q;
            }
        }
        Searched {
            width,
            value,
            usable,
            count,
            sum,
            squares,
        }
    }

    /// Whether every sample `template`'s search pairs one of its own with
    /// is usable.
    fn covers(&self, template: &Template, layout: &Layout) -> bool {
        // The patch's columns start `reach` further in here, so these
        // are the columns from `reach` before it to `reach` after.
        let x = template.columns.start..template.columns.end + 2 * layout.rx;
        let y = template.rows.start.saturating_sub(layout.ry)..template.rows.end + layout.ry;
        self.boxed(&self.count, &x, &y) == (x.len() * y.len()) as f64
    }

    /// A running sum's total over the columns `x` and the rows `y`.
    fn boxed(&self, sums: &[f64], x: &Range<usize>, y: &Range<usize>) -> f64 {
        let stride = self.width + 1;
        sums[y.end * stride + x.end]
            - sums[y.start * stride + x.end]
            - sums[y.end * stride + x.start]
            + sums[y.start * stride + x.start]
    }
}

/// A bin's patch of lens 0's luma surface, at its samples.
#[derive(Debug)]
struct Template {
    columns: Range<usize>,
    rows: Range<usize>,
    /// Row after row; 0 where a sample isn't usable.
    value: Vec<f64>,
    usable: Vec<bool>,
    count: usize,
    sum: f64,
    squares: f64,
}

impl Template {
    fn new(surface: &Surface, patch: &Patch) -> Template {
        let columns = patch.columns.start as usize..patch.columns.end as usize;
        let rows = patch.rows.start as usize..patch.rows.end as usize;
        let mut value = Vec::with_capacity(columns.len() * rows.len());
        let mut usable = Vec::with_capacity(columns.len() * rows.len());
        let (mut count, mut sum, mut squares) = (0, 0.0, 0.0);
        for j in rows.clone() {
            for i in columns.clone() {
                let v = surface.at_sample(i, j, 0);
                value.push(v.unwrap_or(0.0));
                usable.push(v.is_some());
                if let Some(v) = v {
                    count += 1;
                    sum += v;
                    squares += v * v;
                }
            }
        }
        Template {
            columns,
            rows,
            value,
            usable,
            count,
            sum,
            squares,
        }
    }

    fn len(&self) -> usize {
        self.columns.len() * self.rows.len()
    }

    /// The fewest pairs a match is measured from: half the patch.
    fn least(&self) -> usize {
        self.len().div_ceil(2).max(2)
    }
}

/// The sums an NCC is made of, over the pairs of a patch (a) and lens 1
/// (b).
#[derive(Debug, Clone, Copy, Default)]
struct Sums {
    n: f64,
    a: f64,
    b: f64,
    aa: f64,
    bb: f64,
    ab: f64,
}

impl Sums {
    /// The NCC, or `None` when either side has too little contrast to
    /// correlate.
    fn ncc(&self) -> Option<f64> {
        let (mean_a, mean_b) = (self.a / self.n, self.b / self.n);
        let var_a = self.aa / self.n - mean_a * mean_a;
        let var_b = self.bb / self.n - mean_b * mean_b;
        let textured = |var: f64, mean: f64| var > 0.0 && var.sqrt() >= MIN_CONTRAST * mean.abs();
        if !(textured(var_a, mean_a) && textured(var_b, mean_b)) {
            return None;
        }
        Some((self.ab / self.n - mean_a * mean_b) / (var_a * var_b).sqrt())
    }
}

/// The NCC of `template` with lens 1 moved by (u, v) whole pixels, or
/// `None` with fewer than half the patch's pairs or too little contrast.
fn ncc_at(template: &Template, lens1: &Searched, reach: usize, u: i64, v: i64) -> Option<f64> {
    let width = template.columns.len();
    let x0 = (template.columns.start + reach).checked_add_signed(u as isize)?;
    let y0 = template.rows.start.checked_add_signed(v as isize)?;
    let (x, y) = (x0..x0 + width, y0..y0 + template.rows.len());
    let pairs = lens1.boxed(&lens1.count, &x, &y);
    let mut sums = Sums::default();
    if template.count == template.len() && pairs == template.len() as f64 {
        // Every pair is there: only the products need the samples.
        for (j, a) in template.value.chunks_exact(width).enumerate() {
            let at = (y0 + j) * lens1.width + x0;
            let b = &lens1.value[at..at + width];
            sums.ab += a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
        }
        sums.n = pairs;
        sums.a = template.sum;
        sums.aa = template.squares;
        sums.b = lens1.boxed(&lens1.sum, &x, &y);
        sums.bb = lens1.boxed(&lens1.squares, &x, &y);
    } else {
        for j in 0..template.rows.len() {
            for i in 0..width {
                let at = (y0 + j) * lens1.width + x0 + i;
                if template.usable[j * width + i] && lens1.usable[at] {
                    let (a, b) = (template.value[j * width + i], lens1.value[at]);
                    sums.n += 1.0;
                    sums.a += a;
                    sums.b += b;
                    sums.aa += a * a;
                    sums.bb += b * b;
                    sums.ab += a * b;
                }
            }
        }
    }
    if sums.n < template.least() as f64 {
        return None;
    }
    sums.ncc()
}

/// One match of a bin's coarse search: a peak of its NCC over whole
/// pixels.
#[derive(Debug, Clone, Copy)]
struct Peak {
    /// The peak's shift, (u, v) in whole pixels.
    at: [i64; 2],
    /// The same with each way's parabola through the peak and its two
    /// neighbours, and the NCC at its top.
    fine: [f64; 2],
    ncc: f64,
}

/// The top of the parabola through a peak and its neighbours either side:
/// how far from the peak, and how far above it. Nothing where a neighbour
/// is missing.
fn vertex(before: f64, here: f64, after: f64) -> (f64, f64) {
    let bend = before - 2.0 * here + after;
    if bend < 0.0 {
        (
            0.5 * (before - after) / bend,
            -(after - before) * (after - before) / (8.0 * bend),
        )
    } else {
        (0.0, 0.0)
    }
}

/// The peaks of `template`'s NCC over the search that lie within [`TIE`]
/// of the best, the best first, and whether the search had an NCC at
/// every shift.
fn matches(template: &Template, lens1: &Searched, layout: &Layout) -> (Vec<Peak>, bool) {
    let (rx, ry) = (layout.rx as i64, layout.ry as i64);
    let across = 2 * layout.rx + 1;
    let mut grid = vec![f64::NAN; across * (2 * layout.ry + 1)];
    for v in -ry..=ry {
        for u in -rx..=rx {
            if let Some(ncc) = ncc_at(template, lens1, layout.rx, u, v) {
                grid[(v + ry) as usize * across + (u + rx) as usize] = ncc;
            }
        }
    }
    let at = |u: i64, v: i64| {
        if u.abs() > rx || v.abs() > ry {
            f64::NAN
        } else {
            grid[(v + ry) as usize * across + (u + rx) as usize]
        }
    };
    let mut peaks = Vec::new();
    for v in -ry..=ry {
        for u in -rx..=rx {
            let here = at(u, v);
            // A neighbour that wasn't measured is no rival.
            let beaten = (-1..=1).any(|dv| (-1..=1).any(|du| at(u + du, v + dv) > here));
            if !here.is_finite() || beaten {
                continue;
            }
            let (du, rise_u) = vertex(at(u - 1, v), here, at(u + 1, v));
            let (dv, rise_v) = vertex(at(u, v - 1), here, at(u, v + 1));
            peaks.push(Peak {
                at: [u, v],
                fine: [u as f64 + du, v as f64 + dv],
                ncc: here + rise_u + rise_v,
            });
        }
    }
    let far = |p: &Peak| p.fine[0].hypot(p.fine[1]);
    peaks.sort_by(|a, b| b.ncc.total_cmp(&a.ncc).then(far(a).total_cmp(&far(b))));
    if let Some(best) = peaks.first().map(|p| p.ncc) {
        peaks.retain(|p| p.ncc >= best - TIE);
    }
    (peaks, grid.iter().all(|ncc| ncc.is_finite()))
}

/// What one bin measured, in strip pixels: the shift (u, v) by which lens
/// 1 shows the patch, the steps' normal matrix there (uu, uv, vv), less
/// what of it doesn't stand clear of the bin's mismatch
/// ([`NOISE_FLOOR`]), the NCC, each lens's contrast, and the log gains.
#[derive(Debug, Clone, Copy)]
struct Measured {
    shift: [f64; 2],
    normal: [f64; 3],
    /// Whether the whole of the normal matrix stands clear, both ways:
    /// the bin's match is fit to place its neighbours by.
    sure: bool,
    ncc: f64,
    contrast: [f64; 2],
    log_gain: [f64; 3],
}

/// Every bin's measurement, or `None` for a bin that has none.
fn measure(
    surfaces: &[Surface; 2],
    layout: &Layout,
    patches: &[Patch],
    method: &Method,
) -> Vec<Option<Measured>> {
    let searched = Searched::new(&surfaces[1], layout.rx);
    let templates: Vec<Template> = patches
        .iter()
        .map(|patch| Template::new(&surfaces[0], patch))
        .collect();
    let found: Vec<Found> = templates
        .iter()
        .map(|template| {
            let (peaks, everywhere) = if template.count >= template.least() {
                matches(template, &searched, layout)
            } else {
                (Vec::new(), false)
            };
            Found {
                peaks,
                everywhere,
                whole: template.count == template.len() && searched.covers(template, layout),
            }
        })
        .collect();
    let near = NEAR_DEG * layout.sx.max(layout.sy);
    settle_ties(&found, near, method.noise_floor, |bin, start| {
        refine(&templates[bin], surfaces, layout, start, method)
    })
}

/// What one bin's coarse search found.
#[derive(Debug)]
struct Found {
    /// Its matches, the best first.
    peaks: Vec<Peak>,
    /// Whether the search had an NCC at every shift. Where it hadn't, a
    /// better match may lie at a shift it couldn't try.
    everywhere: bool,
    /// Whether every sample of the patch, and every sample of lens 1 the
    /// search pairs one with, is usable: only then does every shift's NCC
    /// compare the same pairs.
    whole: bool,
}

impl Found {
    /// The bin's one match: its best, when every peak tied with it lies
    /// within [`ONE_MATCH`] of it.
    fn sole(&self) -> Option<Peak> {
        let best = *self.peaks.first()?;
        self.peaks
            .iter()
            .all(|p| apart(p.fine, best.fine) <= ONE_MATCH)
            .then_some(best)
    }
}

/// How far apart two shifts lie.
fn apart(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Where a bin's steps start.
#[derive(Debug, Clone, Copy)]
struct Start {
    /// The shift, (u, v) in strip pixels.
    shift: [f64; 2],
    /// The least NCC the steps may end with. A start at one of the bin's
    /// matches has none to keep. A start from a handed shift has [`TIE`]
    /// under the bin's best: its steps end at a match of the bin's own,
    /// or the bin measures nothing.
    floor: f64,
}

impl Start {
    /// At a match of the bin's own, in whole pixels.
    fn at(peak: Peak) -> Start {
        Start {
            shift: [peak.at[0] as f64, peak.at[1] as f64],
            floor: f64::NEG_INFINITY,
        }
    }
}

/// Each bin's measurement: where its steps start, out of the matches
/// tied for it, and what `refine` makes of that start. A whole bin with
/// one match starts there, and hands its match to its neighbours either
/// way round the ring. A bin that is handed a shift takes its match
/// nearest to it and hands that on. With no match within `near` strip
/// pixels, a bin with one match that its search tried every shift
/// against keeps it and hands it on: the field steps there, and the
/// bin's own match says so, whatever samples it is short of. Any other
/// bin starts from the shift it was handed and hands the same on; its
/// steps then have to end at a match of its own, or it measures nothing
/// ([`refine`]). A bin with no match at all has no start, and hands on
/// what it was handed. With no bin to start from, each takes its match
/// nearest zero.
///
/// Under `noise_floor` a bin's own match counts for its neighbours only
/// where the bin measured from it with information both ways
/// ([`NOISE_FLOOR`]): no other bin starts the ring, and one that is
/// handed a shift hands the same on. So a bin of noise alone, whose one match lies anywhere in
/// the search, can't carry its neighbours to it, nor a stretch of them
/// let a shift drift from bin to bin.
fn settle_ties(
    found: &[Found],
    near: f64,
    noise_floor: bool,
    refine: impl Fn(usize, &Start) -> Option<Measured>,
) -> Vec<Option<Measured>> {
    let bins = found.len();
    let nearest = |peaks: &[Peak], to: [f64; 2]| {
        peaks
            .iter()
            .copied()
            .min_by(|a, b| apart(a.fine, to).total_cmp(&apart(b.fine, to)))
    };
    let places = |m: &Option<Measured>| !noise_floor || m.is_some_and(|m| m.sure);
    let mut measured = vec![None; bins];
    let mut handed: Vec<Option<[f64; 2]>> = vec![None; bins];
    let mut queue = VecDeque::new();
    for (k, bin) in found.iter().enumerate() {
        if bin.whole
            && let Some(best) = bin.sole()
        {
            let own = refine(k, &Start::at(best));
            if places(&own) {
                measured[k] = own;
                handed[k] = Some(best.fine);
                queue.push_back(k);
            }
        }
    }
    if queue.is_empty() {
        return found
            .iter()
            .enumerate()
            .map(|(k, bin)| refine(k, &Start::at(nearest(&bin.peaks, [0.0; 2])?)))
            .collect();
    }
    while let Some(k) = queue.pop_front() {
        let Some(from) = handed[k] else { continue };
        for next in [(k + 1) % bins, (k + bins - 1) % bins] {
            if handed[next].is_some() {
                continue;
            }
            let bin = &found[next];
            // Where the bin starts, and the match it would hand on.
            let (start, own) = match (bin.peaks.first(), nearest(&bin.peaks, from)) {
                (Some(_), Some(p)) if apart(p.fine, from) <= near => (Some(Start::at(p)), p.fine),
                (Some(best), Some(_)) => match bin.sole().filter(|_| bin.everywhere) {
                    Some(own) => (Some(Start::at(own)), own.fine),
                    None => {
                        let start = Start {
                            shift: from,
                            floor: best.ncc - TIE,
                        };
                        (Some(start), from)
                    }
                },
                _ => (None, from),
            };
            measured[next] = start.and_then(|start| refine(next, &start));
            handed[next] = Some(if places(&measured[next]) { own } else { from });
            queue.push_back(next);
        }
    }
    measured
}

/// A patch against lens 1 at one shift: what a Lucas–Kanade step is made
/// of.
#[derive(Debug, Clone, Copy)]
struct Reading {
    pairs: usize,
    /// The normal matrix (uu, uv, vv) and right-hand side of the step on
    /// the normalised patches.
    normal: [f64; 3],
    rhs: [f64; 2],
    /// The same on the values as they stand.
    plain_normal: [f64; 3],
    plain_rhs: [f64; 2],
    ncc: f64,
    /// Each side's standard deviation over its mean, over the pairs: the
    /// patch's, then lens 1's.
    contrast: [f64; 2],
}

/// `template` against lens 1's surface at `shift`, or `None` with fewer
/// than half the patch's pairs or a contrast under [`MIN_CONTRAST`] on
/// either side. `pairs` is working room.
///
/// With each patch less its mean and over its standard deviation, T̂ and
/// Î, the step lessens Σ (T̂ − Î)². Moving lens 1's patch moves its mean
/// and deviation too, so Î's slope in the shift is ((g − ḡ) − Î · mean(Î
/// (g − ḡ))) / σ for the luma slope g: the slope's mean and its share
/// along Î itself tell nothing, which leaves a ramp without information.
fn read(
    template: &Template,
    lens1: &Surface,
    shift: [f64; 2],
    pairs: &mut Vec<[f64; 4]>,
) -> Option<Reading> {
    pairs.clear();
    let width = template.columns.len();
    for (j, row) in template.rows.clone().enumerate() {
        for (i, column) in template.columns.clone().enumerate() {
            if !template.usable[j * width + i] {
                continue;
            }
            let (x, y) = (column as f64 + shift[0], row as f64 + shift[1]);
            if let Some([value, gx, gy]) = lens1.luma_at(x, y) {
                pairs.push([template.value[j * width + i], value, gx, gy]);
            }
        }
    }
    if pairs.len() < template.least() {
        return None;
    }
    let n = pairs.len() as f64;
    let mean = |k: usize| pairs.iter().map(|p| p[k]).sum::<f64>() / n;
    let (mean_t, mean_i, mean_gx, mean_gy) = (mean(0), mean(1), mean(2), mean(3));
    let deviation = |k: usize, mean: f64| {
        (pairs
            .iter()
            .map(|p| (p[k] - mean) * (p[k] - mean))
            .sum::<f64>()
            / n)
            .sqrt()
    };
    let (sigma_t, sigma_i) = (deviation(0, mean_t), deviation(1, mean_i));
    let textured = |sigma: f64, mean: f64| sigma > 0.0 && sigma >= MIN_CONTRAST * mean.abs();
    if !(textured(sigma_t, mean_t) && textured(sigma_i, mean_i)) {
        return None;
    }
    let mut along = [0.0; 2];
    for p in pairs.iter() {
        let i_hat = (p[1] - mean_i) / sigma_i;
        along[0] += i_hat * (p[2] - mean_gx) / n;
        along[1] += i_hat * (p[3] - mean_gy) / n;
    }
    let mut reading = Reading {
        pairs: pairs.len(),
        normal: [0.0; 3],
        rhs: [0.0; 2],
        plain_normal: [0.0; 3],
        plain_rhs: [0.0; 2],
        ncc: 0.0,
        contrast: [sigma_t / mean_t.abs(), sigma_i / mean_i.abs()],
    };
    for p in pairs.iter() {
        let t_hat = (p[0] - mean_t) / sigma_t;
        let i_hat = (p[1] - mean_i) / sigma_i;
        let jx = ((p[2] - mean_gx) - i_hat * along[0]) / sigma_i;
        let jy = ((p[3] - mean_gy) - i_hat * along[1]) / sigma_i;
        let left = t_hat - i_hat;
        reading.normal[0] += jx * jx;
        reading.normal[1] += jx * jy;
        reading.normal[2] += jy * jy;
        reading.rhs[0] += jx * left;
        reading.rhs[1] += jy * left;
        let plain = p[0] - p[1];
        reading.plain_normal[0] += p[2] * p[2];
        reading.plain_normal[1] += p[2] * p[3];
        reading.plain_normal[2] += p[3] * p[3];
        reading.plain_rhs[0] += p[2] * plain;
        reading.plain_rhs[1] += p[3] * plain;
        reading.ncc += t_hat * i_hat / n;
    }
    Some(reading)
}

/// The shift `start`, refined by `method`'s steps, or `None` for a bin
/// that measures nothing. That is a bin without the samples or the
/// texture; one whose last step was longer than [`SETTLED`], which three
/// steps from a match never leave and steps from a shift far from the
/// bin's own do; and one whose steps end under `start`'s floor, settled
/// on something that is no match of the bin's. Of a bin that measures,
/// the normal matrix is kept along each direction where its eigenvalue
/// per sample is [`NOISE_FLOOR`] times the mismatch or more
/// ([`above_floor`]), and whole where `method` has that floor off.
fn refine(
    template: &Template,
    surfaces: &[Surface; 2],
    layout: &Layout,
    start: &Start,
    method: &Method,
) -> Option<Measured> {
    let mut shift = start.shift;
    let mut last = 0.0;
    let most = [layout.rx as f64 + MAX_STEP, layout.ry as f64 + MAX_STEP];
    let mut pairs = Vec::with_capacity(template.len());
    for _ in 0..method.lk_steps {
        let reading = read(template, &surfaces[1], shift, &mut pairs)?;
        let (normal, rhs) = if method.normalise {
            (reading.normal, reading.rhs)
        } else {
            (reading.plain_normal, reading.plain_rhs)
        };
        let mut step = damped_solve(normal, rhs);
        let length = step[0].hypot(step[1]);
        if length > MAX_STEP {
            step = step.map(|s| s * MAX_STEP / length);
        }
        last = informed_length(normal, step);
        for ((shift, step), most) in shift.iter_mut().zip(step).zip(most) {
            *shift = (*shift + step).clamp(-most, most);
        }
    }
    let reading = read(template, &surfaces[1], shift, &mut pairs)?;
    let texture = greater_eigenvalue(reading.normal) / reading.pairs as f64;
    if texture.is_nan() || texture < MIN_TEXTURE {
        return None;
    }
    // With the rule off, wherever the steps stopped passes for the shift.
    if method.validity && !(last <= SETTLED && reading.ncc >= start.floor) {
        return None;
    }
    let floor = NOISE_FLOOR * (1.0 - reading.ncc) * reading.pairs as f64;
    let (normal, directions) = if method.noise_floor {
        above_floor(reading.normal, floor)
    } else {
        (reading.normal, 2)
    };
    Some(Measured {
        shift,
        normal,
        sure: directions == 2,
        ncc: reading.ncc,
        contrast: reading.contrast,
        log_gain: log_gains(template, surfaces, shift),
    })
}

/// A normal matrix (uu, uv, vv) less what of it lies under `floor`, and
/// how many of its two directions are left: itself where both its
/// eigenvalues reach the floor; its part along its best-informed
/// direction where only the greater does, as a line's is; nothing where
/// neither does.
fn above_floor(normal: [f64; 3], floor: f64) -> ([f64; 3], usize) {
    let [uu, uv, vv] = normal;
    let greater = greater_eigenvalue(normal);
    // A NaN is under any floor.
    if greater.is_nan() || greater < floor {
        return ([0.0; 3], 0);
    }
    // A match with no mismatch has no floor: its lesser eigenvalue is
    // not to be held to a zero that rounding would decide.
    if floor <= 0.0 || uu + vv - greater >= floor {
        return (normal, 2);
    }
    // The greater eigenvector, by whichever row of N − λ is the longer.
    let e = if (greater - uu).abs() >= (greater - vv).abs() {
        [uv, greater - uu]
    } else {
        [greater - vv, uv]
    };
    let length = e[0] * e[0] + e[1] * e[1];
    if length > 0.0 {
        let along = [e[0] * e[0], e[0] * e[1], e[1] * e[1]];
        (along.map(|a| greater * a / length), 1)
    } else {
        (normal, 2)
    }
}

/// The greater eigenvalue of a normal matrix (uu, uv, vv): what the
/// patch knows of a shift along its best-informed direction.
fn greater_eigenvalue([uu, uv, vv]: [f64; 3]) -> f64 {
    0.5 * (uu + vv + (uu - vv).hypot(2.0 * uv))
}

/// How long a step is as the normal matrix N that made it counts it:
/// √(δᵀ N δ / λ), for N's greater eigenvalue λ. Along the patch's
/// best-informed direction that is the step's length in strip pixels;
/// along a line, where N has nothing to say and a step settles nothing,
/// it is next to none.
fn informed_length(normal: [f64; 3], step: [f64; 2]) -> f64 {
    let [uu, uv, vv] = normal;
    let [u, v] = step;
    let greatest = greater_eigenvalue(normal);
    if greatest > 0.0 {
        ((u * (uu * u + uv * v) + v * (uv * u + vv * v)).max(0.0) / greatest).sqrt()
    } else {
        0.0
    }
}

/// The step (N + [`DAMPING`] · tr N) δ = r of a normal matrix (uu, uv,
/// vv), and no step of a matrix with nothing in it.
fn damped_solve(normal: [f64; 3], rhs: [f64; 2]) -> [f64; 2] {
    let damping = DAMPING * (normal[0] + normal[2]);
    let (uu, uv, vv) = (normal[0] + damping, normal[1], normal[2] + damping);
    let det = uu * vv - uv * uv;
    if damping > 0.0 && det > 0.0 {
        [
            (vv * rhs[0] - uv * rhs[1]) / det,
            (uu * rhs[1] - uv * rhs[0]) / det,
        ]
    } else {
        [0.0; 2]
    }
}

/// The median, channel by channel, of ln(lens 1 / lens 0) over the
/// patch's samples at `shift`, both off the smoothed strips; NaN with
/// fewer than [`MIN_GAIN_SAMPLES`] of them.
fn log_gains(template: &Template, surfaces: &[Surface; 2], shift: [f64; 2]) -> [f64; 3] {
    let mut ratios: [Vec<f64>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for row in template.rows.clone() {
        for column in template.columns.clone() {
            let (x, y) = (column as f64 + shift[0], row as f64 + shift[1]);
            let Some(lens1) = surfaces[1].rgb_at(x, y) else {
                continue;
            };
            for (k, (ratios, lens1)) in ratios.iter_mut().zip(lens1).enumerate() {
                if let Some(lens0) = surfaces[0].at_sample(column, row, k + 1)
                    && lens0 >= DARK
                    && lens1 >= DARK
                {
                    ratios.push((lens1 / lens0).ln());
                }
            }
        }
    }
    ratios.map(|mut ratios| {
        if ratios.len() >= MIN_GAIN_SAMPLES {
            median(&mut ratios)
        } else {
            f64::NAN
        }
    })
}

/// The median of `values`, the mean of the middle two of an even number;
/// NaN of none.
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    match n {
        0 => f64::NAN,
        _ if n % 2 == 1 => values[n / 2],
        _ => 0.5 * (values[n / 2 - 1] + values[n / 2]),
    }
}

/// How many terms a signature fits: a0, a1, b1, a2, b2, c0, c1 and d1.
pub const TERMS: usize = 8;

/// The signature of one frame's bins (8.2): the fit of across(φ) and
/// along(φ) to the shifts of the bins with information
/// ([`BinShift::informed`]), each weighted by its information, with each
/// term's standard error, the rms of what is left, the numbers of valid
/// bins and of those with information, the valid bins' median NCC and
/// the photometric log gains.
///
/// The fit is the least squares that makes Σ rᵀ W r least, over the
/// bins with information, for a bin's leftover shift r = (across, along)
/// less the fit at its φ and its information W. So a bin that a lone
/// line informs across the line only pulls the fit across that line and
/// not along it, and a valid bin without information (a flat patch
/// under noise, [`NOISE_FLOOR`]) pulls it nowhere.
///
/// **What NaN marks, and what it doesn't.** A term is NaN, never a zero,
/// where the normal equations are short of rank for it: its pivot is at
/// most 10⁻¹⁰ of the greatest diagonal entry (no bin with information,
/// no information along the seam anywhere, fewer bins than the terms
/// across take), or it can't be told from such a term. That is
/// rank alone, not conditioning. Bins on one side of the ring leave
/// every term finite and some of them worthless: by the equations
/// themselves, for bins informed alike both ways, a term's standard
/// error is 0.12 to 0.17 of one bin's noise over the whole ring of 72;
/// at most 1.6 times it over two opposite sides of 12 bins each (8.6's
/// usable take); 1.6 (a0) and 2.5 (b1) times over half the ring; and
/// over one 90° arc of 18 bins 47 times for a0, 6 to 66 for a1 and b1,
/// and 2.4 for c0. Over 30° (6 bins) a0 is at 11,000 times, still
/// finite. `valid_bins` and `informed_bins` don't tell these apart:
/// `term_se_deg` does.
///
/// `term_se_deg` is each term's standard error: √(the term's diagonal
/// entry of the normal matrix's inverse × Σ rᵀ W r / ν), for ν the
/// degrees of freedom, the directions the bins inform (two a bin, one
/// for a line's) less the terms fitted. It doesn't depend on the
/// weights' scale. It is NaN where the term is, and where ν is 0, with
/// nothing left over to tell the noise from. It takes the bins'
/// information for their inverse covariances up to one common factor
/// (8.4), which bins of unlike contrast aren't ([`BinShift::info`]), and
/// what the fit leaves for noise: depth that varies with azimuth widens
/// it, and a bias common to the bins (a texture that aliases) doesn't
/// show in it. The parameters' own covariance is the fit's (8.4's stage
/// F), over its own Jacobian.
///
/// `rest_rms_deg` is √(Σ rᵀ W r / ½ Σ tr W): for bins informed alike
/// both ways, the rms of the leftover shifts' lengths. `median_ncc` and
/// each `log_gain` are medians over the valid bins that have one, with
/// information or without: a bin without it still says how well its
/// patches match, and a flat patch's log gain is as good as any.
///
/// 8.2 also asks, on a near scene, for a q per 15° sector, which tells
/// depth that varies with azimuth from the terms above. That is M3.6's,
/// with its fit (8.4), and is not fitted here: such depth shows in
/// `rest_rms_deg`.
pub fn signature(bins: &[BinShift]) -> Signature {
    signature_of(bins, [true; TERMS])
}

/// [`signature`] with only the terms `free` names fitted, in the order
/// a0, a1, b1, a2, b2, c0, c1, d1: a term that isn't free is held at 0,
/// with a standard error of 0. What a fit short of a term makes of a
/// field that has it is the control for fitting that term.
pub fn signature_of(bins: &[BinShift], free: [bool; TERMS]) -> Signature {
    let finite = |b: &BinShift| {
        let numbers = [b.phi_deg, b.across_deg, b.along_deg];
        numbers
            .iter()
            .chain(b.info.iter().flatten())
            .all(|v| v.is_finite())
    };
    let used: Vec<Used> = bins
        .iter()
        .filter(|b| b.informed() && finite(b))
        .map(|b| Used {
            shift: [b.across_deg, b.along_deg],
            rows: basis(b.phi_deg),
            w: weights(b),
        })
        .collect();
    // The normal equations, the right-hand side as a ninth column, and
    // an identity after it, which the elimination turns into the normal
    // matrix's inverse.
    let mut normal = [[0.0; WIDTH]; TERMS];
    for (i, equation) in normal.iter_mut().enumerate() {
        equation[TERMS + 1 + i] = 1.0;
    }
    for Used { shift, rows, w } in &used {
        for (i, equation) in normal.iter_mut().enumerate() {
            for a in 0..2 {
                for b in 0..2 {
                    let left = rows[a][i] * w[a][b];
                    for (sum, right) in equation.iter_mut().zip(rows[b]) {
                        *sum += left * right;
                    }
                    equation[TERMS] += left * shift[b];
                }
            }
        }
    }
    let solved = solve(normal, free);
    let fitted = solved.terms.map(|t| t.unwrap_or(0.0));
    let (mut left, mut trace) = (0.0, 0.0);
    for Used { shift, rows, w } in &used {
        let fit = |row: &[f64; TERMS]| row.iter().zip(fitted).map(|(r, t)| r * t).sum::<f64>();
        let r = [shift[0] - fit(&rows[0]), shift[1] - fit(&rows[1])];
        left += r[0] * (w[0][0] * r[0] + w[0][1] * r[1]) + r[1] * (w[1][0] * r[0] + w[1][1] * r[1]);
        trace += w[0][0] + w[1][1];
    }
    // What a unit of weight came to, in degrees²: the leftover over its
    // degrees of freedom.
    let informed: usize = used.iter().map(|u| rank(&u.w)).sum();
    let unit = if informed > solved.reached {
        left.max(0.0) / (informed - solved.reached) as f64
    } else {
        f64::NAN
    };
    let term_se_deg = std::array::from_fn(|i| match solved.terms[i] {
        Some(_) if !free[i] => 0.0,
        Some(_) => (solved.inverse[i].max(0.0) * unit).sqrt(),
        None => f64::NAN,
    });
    let valid = || bins.iter().filter(|b| b.valid);
    let median_of = |value: &dyn Fn(&BinShift) -> f64| {
        let mut values: Vec<f64> = valid().map(value).filter(|v| v.is_finite()).collect();
        median(&mut values)
    };
    let [a0, a1, b1, a2, b2, c0, c1, d1] = solved.terms.map(|t| t.unwrap_or(f64::NAN));
    Signature {
        a0,
        a1,
        b1,
        a2,
        b2,
        c0,
        c1,
        d1,
        term_se_deg,
        rest_rms_deg: if trace > 0.0 {
            (left.max(0.0) / (0.5 * trace)).sqrt()
        } else {
            f64::NAN
        },
        valid_bins: valid().count(),
        informed_bins: used.len(),
        median_ncc: median_of(&|b| b.ncc),
        log_gain: [0, 1, 2].map(|k| median_of(&|b| b.log_gain[k])),
    }
}

/// A bin the fit uses: its shift (across, along), what each term adds to
/// each at its φ, and its weight.
struct Used {
    shift: [f64; 2],
    rows: [[f64; TERMS]; 2],
    w: [[f64; 2]; 2],
}

/// What each term adds to across (the first row) and to along (the
/// second) at `phi_deg`.
fn basis(phi_deg: f64) -> [[f64; TERMS]; 2] {
    let (sin, cos) = phi_deg.to_radians().sin_cos();
    let (sin2, cos2) = (2.0 * phi_deg).to_radians().sin_cos();
    [
        [1.0, cos, sin, cos2, sin2, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, cos, sin],
    ]
}

/// A bin's information as the fit's weight, made symmetric.
fn weights(bin: &BinShift) -> [[f64; 2]; 2] {
    let cross = 0.5 * (bin.info[0][1] + bin.info[1][0]);
    [[bin.info[0][0], cross], [cross, bin.info[1][1]]]
}

/// How many directions a weight informs: two, or one for a line's, whose
/// lesser eigenvalue is no information beside its greater
/// ([`NO_INFORMATION`]).
fn rank(w: &[[f64; 2]; 2]) -> usize {
    let (sum, spread) = (w[0][0] + w[1][1], (w[0][0] - w[1][1]).hypot(2.0 * w[0][1]));
    let (greater, lesser) = (0.5 * (sum + spread), 0.5 * (sum - spread));
    usize::from(greater > 0.0) + usize::from(lesser > NO_INFORMATION * greater)
}

/// A pivot at most this share of the greatest diagonal entry is no
/// information about its term.
const NO_INFORMATION: f64 = 1e-10;
/// A solved term that depends by more than this on one with no
/// information has none either.
const LEANS: f64 = 1e-6;

/// The columns of the equations [`solve`] takes: the normal matrix, the
/// right-hand side, and an identity.
const WIDTH: usize = 2 * TERMS + 1;

/// What [`solve`] makes of the normal equations.
struct Solved {
    /// Each term, `None` where the equations don't determine it.
    terms: [Option<f64>; TERMS],
    /// The diagonal of the normal matrix's inverse, over the terms the
    /// elimination reached: each term's variance under unit weights.
    inverse: [f64; TERMS],
    /// How many terms the elimination reached.
    reached: usize,
}

/// The terms of the normal equations `m` (the right-hand side its ninth
/// column, an identity after it) by Gauss–Jordan, each pivot the greatest
/// diagonal entry left; the identity ends as the inverse of the normal
/// matrix over the terms reached. A term that isn't free is 0. A free
/// term is `None` when the equations say nothing about it, or tie it to
/// one they say nothing about.
fn solve(mut m: [[f64; WIDTH]; TERMS], free: [bool; TERMS]) -> Solved {
    for held in (0..TERMS).filter(|&i| !free[i]) {
        m[held] = [0.0; WIDTH];
        for row in &mut m {
            row[held] = 0.0;
        }
    }
    let scale = (0..TERMS).map(|i| m[i][i]).fold(0.0, f64::max);
    let mut pivoted = [false; TERMS];
    while let Some(p) = (0..TERMS)
        .filter(|&i| free[i] && !pivoted[i])
        .max_by(|&a, &b| m[a][a].total_cmp(&m[b][b]))
    {
        let pivot = m[p][p];
        if pivot.is_nan() || pivot <= NO_INFORMATION * scale {
            break;
        }
        let row = m[p].map(|v| v / pivot);
        m[p] = row;
        for (i, other) in m.iter_mut().enumerate() {
            let factor = other[p];
            if i != p && factor != 0.0 {
                for (v, r) in other.iter_mut().zip(row) {
                    *v -= factor * r;
                }
            }
        }
        pivoted[p] = true;
    }
    Solved {
        terms: std::array::from_fn(|i| {
            if !free[i] {
                return Some(0.0);
            }
            let leans = (0..TERMS).any(|j| free[j] && !pivoted[j] && m[i][j].abs() > LEANS);
            (pivoted[i] && !leans).then_some(m[i][TERMS])
        }),
        inverse: std::array::from_fn(|i| m[i][TERMS + 1 + i]),
        reached: pivoted.iter().filter(|&&p| p).count(),
    }
}
