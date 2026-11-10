//! Image metrics (M1 plan, section 6): PSNR over RGB, SSIM on BT.709
//! luma, and a count of the pixels that differ by more than a threshold,
//! for the render tests; and PSNR and SSIM of one plane, for the
//! export's parity tests (M2 plan section 11); and the first three with a
//! weight for each pixel, for the stitching round trip's masked
//! comparisons (M3 plan 8.1). Here rather than in testkit so that agent
//! can measure an export too (A2, amendment 2 item 32).
//!
//! Every metric works in unit scale, samples in `[0, 1]`: 8-bit samples
//! count as `v / 255`, 16-bit ones as `v / 65535`, floats as they are. So
//! an 8-bit GPU image compares with an `f64` CPU reference directly, and
//! a PSNR between two 8-bit images is the usual one with a peak of 255.
//! Only the first three channels of a pixel (RGB) count; a fourth, alpha,
//! is skipped.

use std::f64::consts::{FRAC_PI_2, PI};
use std::fmt;

/// A sample type an [`View`] can hold.
pub trait Sample: Copy {
    /// The sample in `[0, 1]`.
    fn unit(self) -> f64;
}

impl Sample for u8 {
    fn unit(self) -> f64 {
        f64::from(self) / 255.0
    }
}

impl Sample for u16 {
    fn unit(self) -> f64 {
        f64::from(self) / 65535.0
    }
}

impl Sample for f32 {
    fn unit(self) -> f64 {
        f64::from(self)
    }
}

impl Sample for f64 {
    fn unit(self) -> f64 {
        self
    }
}

/// A borrowed image: `width` × `height` pixels of `channels` samples
/// each (3 or 4, RGB first), row after row with no padding.
#[derive(Debug, Clone, Copy)]
pub struct View<'a, T> {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: &'a [T],
}

impl<'a, T: Sample> View<'a, T> {
    pub fn new(width: usize, height: usize, channels: usize, data: &'a [T]) -> Self {
        View {
            width,
            height,
            channels,
            data,
        }
    }

    /// Pixel `(x, y)`'s RGB in unit scale.
    pub fn rgb(&self, x: usize, y: usize) -> [f64; 3] {
        let i = (y * self.width + x) * self.channels;
        [
            self.data[i].unit(),
            self.data[i + 1].unit(),
            self.data[i + 2].unit(),
        ]
    }

    /// BT.709 luma of pixel `(x, y)`, in unit scale.
    fn luma(&self, x: usize, y: usize) -> f64 {
        let [r, g, b] = self.rgb(x, y);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    fn check(&self) -> Result<(), Mismatch> {
        let need = self.width * self.height * self.channels;
        if self.channels < 3 || self.data.len() < need {
            return Err(Mismatch(format!(
                "{}x{} with {} channels needs {need} samples, has {}",
                self.width,
                self.height,
                self.channels,
                self.data.len()
            )));
        }
        Ok(())
    }
}

/// Two images that can't be compared: of different sizes, or with too few
/// samples for their size; or weights that don't fit them ([`Weights`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch(pub String);

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Mismatch {}

fn same_size<A: Sample, B: Sample>(a: &View<'_, A>, b: &View<'_, B>) -> Result<(), Mismatch> {
    a.check()?;
    b.check()?;
    if (a.width, a.height) != (b.width, b.height) {
        return Err(Mismatch(format!(
            "{}x{} against {}x{}",
            a.width, a.height, b.width, b.height
        )));
    }
    Ok(())
}

/// The mean squared error over RGB, in unit scale.
pub fn mse<A: Sample, B: Sample>(a: View<'_, A>, b: View<'_, B>) -> Result<f64, Mismatch> {
    same_size(&a, &b)?;
    let mut sum = 0.0;
    for y in 0..a.height {
        for x in 0..a.width {
            let (p, q) = (a.rgb(x, y), b.rgb(x, y));
            for c in 0..3 {
                sum += (p[c] - q[c]).powi(2);
            }
        }
    }
    Ok(sum / (3 * a.width * a.height).max(1) as f64)
}

/// Peak signal-to-noise ratio over RGB in dB, with a peak of 1 in unit
/// scale (255 for 8-bit samples): `10 log10(1 / MSE)`, infinite for equal
/// images. 48.13 dB is an RMS error of one 8-bit step.
pub fn psnr<A: Sample, B: Sample>(a: View<'_, A>, b: View<'_, B>) -> Result<f64, Mismatch> {
    let mse = mse(a, b)?;
    Ok(if mse == 0.0 {
        f64::INFINITY
    } else {
        -10.0 * mse.log10()
    })
}

/// The 11×11 Gaussian window of SSIM, σ = 1.5, summing to 1.
fn gaussian_window() -> [[f64; 11]; 11] {
    let mut w = [[0.0; 11]; 11];
    let mut sum = 0.0;
    for (y, row) in w.iter_mut().enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            let (dx, dy) = (x as f64 - 5.0, y as f64 - 5.0);
            *v = (-(dx * dx + dy * dy) / (2.0 * 1.5 * 1.5)).exp();
            sum += *v;
        }
    }
    for v in w.iter_mut().flatten() {
        *v /= sum;
    }
    w
}

/// Structural similarity (Wang, Bovik, Sheikh and Simoncelli, 2004) of
/// the BT.709 luma of two images, with an 11×11 Gaussian window (σ 1.5)
/// over every position where it fits, K1 = 0.01, K2 = 0.03 and a range
/// of 1: 1.0 for equal images. Both must be at least 11×11.
pub fn ssim<A: Sample, B: Sample>(a: View<'_, A>, b: View<'_, B>) -> Result<f64, Mismatch> {
    same_size(&a, &b)?;
    let (total, count) = ssim_sums(&a, &b, |_| 1.0)?;
    Ok(total / count)
}

/// The sums [`ssim`] and [`ssim_weighted`] divide, for two images of one
/// size: the SSIM map's value at each position of the window, times
/// `weight` of the index of the pixel at the window's centre, and those
/// weights. A position of weight 0 is left out, its window not computed;
/// a pixel of weight 0 under a counted window is in that window's value
/// all the same. A weight of 1 throughout gives the map's plain sum and
/// the number of positions.
fn ssim_sums<A: Sample, B: Sample>(
    a: &View<'_, A>,
    b: &View<'_, B>,
    weight: impl Fn(usize) -> f64,
) -> Result<(f64, f64), Mismatch> {
    if a.width < 11 || a.height < 11 {
        return Err(Mismatch(format!(
            "SSIM needs 11x11 at least, not {}x{}",
            a.width, a.height
        )));
    }
    let (c1, c2) = (0.01f64.powi(2), 0.03f64.powi(2));
    let w = gaussian_window();
    let la: Vec<f64> = (0..a.height)
        .flat_map(|y| (0..a.width).map(move |x| (x, y)))
        .map(|(x, y)| a.luma(x, y))
        .collect();
    let lb: Vec<f64> = (0..b.height)
        .flat_map(|y| (0..b.width).map(move |x| (x, y)))
        .map(|(x, y)| b.luma(x, y))
        .collect();
    let (mut total, mut weights) = (0.0, 0.0);
    for y0 in 0..=a.height - 11 {
        for x0 in 0..=a.width - 11 {
            let at = weight((y0 + 5) * a.width + x0 + 5);
            if at == 0.0 {
                continue;
            }
            let (mut ma, mut mb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for (dy, row) in w.iter().enumerate() {
                for (dx, &k) in row.iter().enumerate() {
                    let i = (y0 + dy) * a.width + x0 + dx;
                    let (p, q) = (la[i], lb[i]);
                    ma += k * p;
                    mb += k * q;
                    saa += k * p * p;
                    sbb += k * q * q;
                    sab += k * p * q;
                }
            }
            let (va, vb, cov) = (saa - ma * ma, sbb - mb * mb, sab - ma * mb);
            total += at
                * (((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                    / ((ma * ma + mb * mb + c1) * (va + vb + c2)));
            weights += at;
        }
    }
    Ok((total, weights))
}

/// How two images differ pixel by pixel ([`diff_stats`]).
#[derive(Debug, Clone, PartialEq)]
pub struct DiffStats {
    /// The largest difference of any channel, in unit scale.
    pub max: f64,
    /// Where it is, `(x, y)`; `None` for equal images.
    pub worst: Option<(usize, usize)>,
    /// Pixels with a channel that differs by more than the threshold.
    pub count_over: usize,
    /// The first of them in row order, up to [`DiffStats::LISTED`], for
    /// failure messages.
    pub over: Vec<(usize, usize)>,
}

impl DiffStats {
    pub const LISTED: usize = 16;
}

/// Compare `a` and `b` pixel by pixel: the largest difference and the
/// pixels with a channel more than `threshold` apart (unit scale, so
/// `2.0 / 255.0` for two 8-bit steps). For seams and poles, where a line
/// of wrong pixels barely moves PSNR.
pub fn diff_stats<A: Sample, B: Sample>(
    a: View<'_, A>,
    b: View<'_, B>,
    threshold: f64,
) -> Result<DiffStats, Mismatch> {
    same_size(&a, &b)?;
    Ok(diff_stats_where(&a, &b, threshold, |_| true))
}

/// [`diff_stats`] of two images of one size, over the pixels whose index
/// `counts` keeps.
fn diff_stats_where<A: Sample, B: Sample>(
    a: &View<'_, A>,
    b: &View<'_, B>,
    threshold: f64,
    counts: impl Fn(usize) -> bool,
) -> DiffStats {
    let mut s = DiffStats {
        max: 0.0,
        worst: None,
        count_over: 0,
        over: Vec::new(),
    };
    for y in 0..a.height {
        for x in 0..a.width {
            if !counts(y * a.width + x) {
                continue;
            }
            let (p, q) = (a.rgb(x, y), b.rgb(x, y));
            let d = (0..3).map(|c| (p[c] - q[c]).abs()).fold(0.0, f64::max);
            if d > s.max {
                s.max = d;
                s.worst = Some((x, y));
            }
            if d > threshold {
                s.count_over += 1;
                if s.over.len() < DiffStats::LISTED {
                    s.over.push((x, y));
                }
            }
        }
    }
    s
}

/// A weight for each pixel of an image, for the weighted metrics (M3 plan
/// 8.1): `width` × `height` of them, row after row, none negative. A mask
/// of 0s and 1s ([`crate::seam::band_weights`],
/// [`crate::seam::lens_coverage`]), a density ([`equirect_coslat`]), or
/// the product of the two.
///
/// A metric refuses weights that aren't of the images' size, that hold a
/// value below 0 or one that isn't finite, or that sum to 0: a mask that
/// came out empty measures nothing, which must not read as a perfect
/// score.
#[derive(Debug, Clone, Copy)]
pub struct Weights<'a> {
    pub width: usize,
    pub height: usize,
    pub w: &'a [f64],
}

impl Weights<'_> {
    /// The sum of the weights, once they are known to fit images of
    /// `width` × `height` pixels.
    fn check(&self, width: usize, height: usize) -> Result<f64, Mismatch> {
        let need = self.width * self.height;
        if self.w.len() != need {
            return Err(Mismatch(format!(
                "{}x{} weights need {need} values, have {}",
                self.width,
                self.height,
                self.w.len()
            )));
        }
        if (self.width, self.height) != (width, height) {
            return Err(Mismatch(format!(
                "{}x{} weights against {width}x{height} images",
                self.width, self.height
            )));
        }
        if let Some(i) = self.w.iter().position(|v| !(v.is_finite() && *v >= 0.0)) {
            return Err(Mismatch(format!(
                "the weight at ({}, {}) is {}: a weight is finite and 0 or more",
                i % width,
                i / width,
                self.w[i]
            )));
        }
        let sum: f64 = self.w.iter().sum();
        if sum == 0.0 {
            return Err(Mismatch(format!(
                "the {width}x{height} weights sum to 0: they leave nothing to measure"
            )));
        }
        if !sum.is_finite() {
            return Err(Mismatch(format!(
                "the {width}x{height} weights sum to {sum}"
            )));
        }
        Ok(sum)
    }
}

/// [`psnr`] with each pixel's squared error counted by its weight:
/// `10 log10(1 / MSE)`, where the MSE is the sum over the pixels of the
/// weight times the pixel's squared error over RGB, over three times the
/// sum of the weights. Weights of 1 everywhere give [`psnr`] exactly, and
/// a pixel of weight 0 doesn't count, whatever it holds. The weights must
/// be of the images' size.
pub fn psnr_weighted<A: Sample, B: Sample>(
    a: View<'_, A>,
    b: View<'_, B>,
    w: &Weights<'_>,
) -> Result<f64, Mismatch> {
    same_size(&a, &b)?;
    let total = w.check(a.width, a.height)?;
    // Summed as `mse` sums, so that weights of 1 give its bits.
    let mut sum = 0.0;
    for y in 0..a.height {
        for x in 0..a.width {
            let k = w.w[y * a.width + x];
            if k == 0.0 {
                continue;
            }
            let (p, q) = (a.rgb(x, y), b.rgb(x, y));
            for c in 0..3 {
                sum += k * (p[c] - q[c]).powi(2);
            }
        }
    }
    let mse = sum / (3.0 * total);
    Ok(if mse == 0.0 {
        f64::INFINITY
    } else {
        -10.0 * mse.log10()
    })
}

/// [`ssim`] with its map averaged by weight: each position of the window
/// counts with the weight of the pixel at the window's centre. Weights of
/// 1 everywhere give [`ssim`] exactly. The weights must be of the images'
/// size.
///
/// A weight is its window's, not its pixel's: an error at a pixel of
/// weight 0 still counts through each window that covers it and whose
/// centre has a weight, so a mask's edge reaches 5 pixels further here
/// than in [`psnr_weighted`]. The 5 pixels along each edge of the image
/// are no window's centre, so their weights count for nothing, and
/// weights that are 0 at every centre are refused.
pub fn ssim_weighted<A: Sample, B: Sample>(
    a: View<'_, A>,
    b: View<'_, B>,
    w: &Weights<'_>,
) -> Result<f64, Mismatch> {
    same_size(&a, &b)?;
    w.check(a.width, a.height)?;
    let (total, weights) = ssim_sums(&a, &b, |i| w.w[i])?;
    if weights == 0.0 {
        return Err(Mismatch(format!(
            "the weights are 0 at every centre of SSIM's window, which stays 5 pixels inside \
             the {}x{} image",
            a.width, a.height
        )));
    }
    Ok(total / weights)
}

/// [`diff_stats`] over the pixels whose weight is above 0: here the
/// weights are a mask, since the largest difference and a count of pixels
/// have no weighted form. `threshold` is in unit scale, as
/// [`diff_stats`]'s. The weights must be of the images' size.
pub fn diff_stats_weighted<A: Sample, B: Sample>(
    a: View<'_, A>,
    b: View<'_, B>,
    w: &Weights<'_>,
    threshold: f64,
) -> Result<DiffStats, Mismatch> {
    same_size(&a, &b)?;
    w.check(a.width, a.height)?;
    Ok(diff_stats_where(&a, &b, threshold, |i| w.w[i] > 0.0))
}

/// The weights of a `width` × `height` equirect picture's pixels by the
/// solid angle each covers, `width` × `height` of them, row after row:
/// the cosine of the latitude at the centre of the pixel's row,
/// π/2 − (y + ½)/height · π for row y (as [`crate::OutKind::Equirect`]
/// places the rows), the same all along the row. A pixel's share of their
/// sum is its share of the sphere.
pub fn equirect_coslat(width: usize, height: usize) -> Vec<f64> {
    let mut w = Vec::with_capacity(width * height);
    for y in 0..height {
        let latitude = FRAC_PI_2 - (y as f64 + 0.5) / height as f64 * PI;
        w.resize(w.len() + width, latitude.cos());
    }
    w
}

/// One plane of a picture, its samples in unit scale, row after row:
/// a luma plane, say, for comparing a decoded export with what it was
/// rendered from in the encoder's own codes.
#[derive(Debug, Clone, Copy)]
pub struct Plane<'a> {
    pub width: usize,
    pub height: usize,
    pub data: &'a [f64],
}

impl Plane<'_> {
    fn check(&self) -> Result<(), Mismatch> {
        if self.data.len() != self.width * self.height {
            return Err(Mismatch(format!(
                "a {}x{} plane needs {} samples, has {}",
                self.width,
                self.height,
                self.width * self.height,
                self.data.len()
            )));
        }
        Ok(())
    }
}

fn same_plane(a: &Plane<'_>, b: &Plane<'_>) -> Result<(), Mismatch> {
    a.check()?;
    b.check()?;
    if (a.width, a.height) != (b.width, b.height) {
        return Err(Mismatch(format!(
            "{}x{} against {}x{}",
            a.width, a.height, b.width, b.height
        )));
    }
    Ok(())
}

/// [`psnr`] of one plane: `10 log10(1 / MSE)` in unit scale.
pub fn psnr_plane(a: Plane<'_>, b: Plane<'_>) -> Result<f64, Mismatch> {
    same_plane(&a, &b)?;
    let sum: f64 = a
        .data
        .iter()
        .zip(b.data)
        .map(|(p, q)| (p - q).powi(2))
        .sum();
    let mse = sum / a.data.len().max(1) as f64;
    Ok(if mse == 0.0 {
        f64::INFINITY
    } else {
        -10.0 * mse.log10()
    })
}

/// [`ssim`] of one plane, as it is rather than as luma: the same window
/// and constants, with the Gaussian applied as two passes of its 1D
/// factor, so a 4K plane takes a fraction of a second.
pub fn ssim_plane(a: Plane<'_>, b: Plane<'_>) -> Result<f64, Mismatch> {
    same_plane(&a, &b)?;
    let (w, h) = (a.width, a.height);
    if w < 11 || h < 11 {
        return Err(Mismatch(format!("SSIM needs 11x11 at least, not {w}x{h}")));
    }
    // The 2D window is the outer product of this, normalized.
    let g: Vec<f64> = {
        let g: Vec<f64> = (0..11)
            .map(|i| (-((i as f64 - 5.0).powi(2)) / (2.0 * 1.5 * 1.5)).exp())
            .collect();
        let s: f64 = g.iter().sum();
        g.into_iter().map(|v| v / s).collect()
    };
    let (ow, oh) = (w - 10, h - 10);
    // Each of the five moments, filtered along rows, then along columns.
    let fields: [&dyn Fn(f64, f64) -> f64; 5] = [
        &|p, _| p,
        &|_, q| q,
        &|p, _| p * p,
        &|_, q| q * q,
        &|p, q| p * q,
    ];
    let filtered: Vec<Vec<f64>> = fields
        .iter()
        .map(|f| {
            let mut rows = vec![0.0; ow * h];
            for y in 0..h {
                let (ra, rb) = (&a.data[y * w..(y + 1) * w], &b.data[y * w..(y + 1) * w]);
                for x0 in 0..ow {
                    rows[y * ow + x0] = (0..11).map(|d| g[d] * f(ra[x0 + d], rb[x0 + d])).sum();
                }
            }
            let mut out = vec![0.0; ow * oh];
            for y0 in 0..oh {
                for x0 in 0..ow {
                    out[y0 * ow + x0] = (0..11).map(|d| g[d] * rows[(y0 + d) * ow + x0]).sum();
                }
            }
            out
        })
        .collect();
    let (c1, c2) = (0.01f64.powi(2), 0.03f64.powi(2));
    let total: f64 = (0..ow * oh)
        .map(|i| {
            let (ma, mb) = (filtered[0][i], filtered[1][i]);
            let (va, vb) = (filtered[2][i] - ma * ma, filtered[3][i] - mb * mb);
            let cov = filtered[4][i] - ma * mb;
            ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2))
        })
        .sum();
    Ok(total / (ow * oh) as f64)
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::sync::OnceLock;
    use std::time::Instant;

    use proptest::prelude::{Just, Strategy};
    use spherecut_testkit::control::{at_least, at_most, must_fail};

    use super::*;

    #[test]
    fn a_plane_measures_as_the_gray_image_it_would_be() {
        let (w, h) = (40, 23);
        let mut rng = Rng(7);
        let a: Vec<f64> = (0..w * h)
            .map(|i| 0.2 + 0.6 * ((i % w) as f64 / w as f64))
            .collect();
        let b: Vec<f64> = a.iter().map(|v| v + 0.02 * rng.gauss()).collect();
        let gray = |p: &[f64]| p.iter().flat_map(|&v| [v, v, v]).collect::<Vec<f64>>();
        let (ga, gb) = (gray(&a), gray(&b));
        let (va, vb) = (View::new(w, h, 3, &ga), View::new(w, h, 3, &gb));
        let (pa, pb) = (
            Plane {
                width: w,
                height: h,
                data: &a,
            },
            Plane {
                width: w,
                height: h,
                data: &b,
            },
        );
        let (p, q) = (psnr(va, vb).unwrap(), psnr_plane(pa, pb).unwrap());
        assert!((p - q).abs() < 1e-9, "{p} {q}");
        let (s, t) = (ssim(va, vb).unwrap(), ssim_plane(pa, pb).unwrap());
        assert!((s - t).abs() < 1e-9, "{s} {t}");
        assert!(t < 0.99, "{t}");
        assert_eq!(ssim_plane(pa, pa).unwrap(), 1.0);
        assert_eq!(psnr_plane(pa, pa).unwrap(), f64::INFINITY);
        let short = Plane {
            width: w,
            height: h,
            data: &a[1..],
        };
        assert!(psnr_plane(pa, short).is_err());
    }

    /// A seeded generator for test images: xorshift64*, and Box–Muller
    /// for Gaussian noise.
    struct Rng(u64);

    impl Rng {
        fn unit(&mut self) -> f64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
        }

        fn gauss(&mut self) -> f64 {
            let (u, v) = (self.unit().max(1e-300), self.unit());
            (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
        }
    }

    fn scene(w: usize, h: usize) -> Vec<f64> {
        (0..w * h)
            .flat_map(|i| {
                let (x, y) = ((i % w) as f64 / w as f64, (i / w) as f64 / h as f64);
                [
                    0.3 + 0.4 * x,
                    0.5 + 0.2 * (6.0 * y).sin(),
                    0.6 - 0.3 * x * y,
                ]
            })
            .collect()
    }

    #[test]
    fn equal_images_are_infinite_psnr_and_unit_ssim() {
        let a = scene(32, 24);
        let v = View::new(32, 24, 3, &a);
        assert_eq!(psnr(v, v), Ok(f64::INFINITY));
        assert!((ssim(v, v).unwrap() - 1.0).abs() < 1e-12);
        let d = diff_stats(v, v, 0.0).unwrap();
        assert_eq!((d.max, d.worst, d.count_over), (0.0, None, 0));
    }

    #[test]
    fn gaussian_noise_gives_its_psnr() {
        // σ = 0.01 is an MSE of 1e-4, 40 dB; 0.001 is 60 dB. 196 608
        // samples estimate the MSE within about 0.3%, 0.014 dB.
        let (w, h) = (256, 256);
        let a = scene(w, h);
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for (sigma, want) in [(0.01, 40.0), (0.001, 60.0)] {
            let b: Vec<f64> = a.iter().map(|&v| v + sigma * rng.gauss()).collect();
            let p = psnr(View::new(w, h, 3, &a), View::new(w, h, 3, &b)).unwrap();
            assert!((p - want).abs() < 0.1, "σ {sigma}: {p} dB");
        }
    }

    #[test]
    fn one_step_everywhere_is_48_db_in_8_bits() {
        let a = vec![100u8; 16 * 16 * 4];
        let b: Vec<u8> = a
            .iter()
            .enumerate()
            .map(|(i, &v)| v + u8::from(i % 4 != 3))
            .collect();
        let p = psnr(View::new(16, 16, 4, &a), View::new(16, 16, 4, &b)).unwrap();
        // 20 log10(255): alpha, which differs nowhere here, doesn't count.
        assert!((p - 48.1308).abs() < 1e-3, "{p}");
        // Samples of different types compare in unit scale.
        let f: Vec<f64> = a.iter().map(|&v| f64::from(v) / 255.0).collect();
        assert_eq!(
            psnr(View::new(16, 16, 4, &a), View::new(16, 16, 4, &f)),
            Ok(f64::INFINITY)
        );
        let w: Vec<u16> = a.iter().map(|&v| u16::from(v) * 257).collect();
        assert_eq!(
            psnr(View::new(16, 16, 4, &a), View::new(16, 16, 4, &w)),
            Ok(f64::INFINITY)
        );
    }

    #[test]
    fn ssim_has_its_known_values() {
        // Flat fields: only the luminance term is left, (2ab + C1) /
        // (a² + b² + C1).
        let (a, b) = (vec![0.5f64; 16 * 16 * 3], vec![0.6f64; 16 * 16 * 3]);
        let s = ssim(View::new(16, 16, 3, &a), View::new(16, 16, 3, &b)).unwrap();
        let want = (2.0 * 0.5 * 0.6 + 1e-4) / (0.25 + 0.36 + 1e-4);
        assert!((s - want).abs() < 1e-12, "{s} vs {want}");
        // A pattern against its negative is anticorrelated.
        let p = scene(32, 32);
        let n: Vec<f64> = p.iter().map(|v| 1.0 - v).collect();
        let s = ssim(View::new(32, 32, 3, &p), View::new(32, 32, 3, &n)).unwrap();
        assert!(s < 0.0, "{s}");
        // Noise lowers it from 1.
        let mut rng = Rng(7);
        let q: Vec<f64> = p.iter().map(|&v| v + 0.05 * rng.gauss()).collect();
        let s = ssim(View::new(32, 32, 3, &p), View::new(32, 32, 3, &q)).unwrap();
        assert!(s > 0.0 && s < 0.9, "{s}");
    }

    #[test]
    fn diff_stats_counts_and_places_the_pixels_over() {
        let a = vec![0u8; 8 * 4 * 3];
        let mut b = a.clone();
        b[(2 * 8 + 5) * 3 + 1] = 3; // (5, 2): 3 steps in green
        b[(3 * 8 + 1) * 3] = 2; // (1, 3): 2 steps in red
        let d = diff_stats(View::new(8, 4, 3, &a), View::new(8, 4, 3, &b), 2.0 / 255.0).unwrap();
        assert_eq!(d.count_over, 1);
        assert_eq!(d.over, vec![(5, 2)]);
        assert_eq!(d.worst, Some((5, 2)));
        assert!((d.max - 3.0 / 255.0).abs() < 1e-15);
        let d = diff_stats(View::new(8, 4, 3, &a), View::new(8, 4, 3, &b), 1.0 / 255.0).unwrap();
        assert_eq!(d.over, vec![(5, 2), (1, 3)]);
    }

    #[test]
    fn images_that_dont_match_are_refused() {
        let a = vec![0u8; 8 * 8 * 3];
        assert!(psnr(View::new(8, 8, 3, &a), View::new(8, 4, 3, &a[..96])).is_err());
        assert!(psnr(View::new(8, 8, 3, &a), View::new(8, 8, 4, &a)).is_err());
        assert!(ssim(View::new(8, 8, 3, &a), View::new(8, 8, 3, &a)).is_err());
    }

    // The weighted metrics (M3 plan 8.1).

    /// The weighted metrics' share, in seconds, of the 15 s that 19.6
    /// gives M3.4's suites (M3 plan 4.1 rule 5), at the default number of
    /// cases.
    const WEIGHTED_BUDGET_S: f64 = 0.5;

    /// When the first of the weighted metrics' tests began.
    static BEGAN: OnceLock<Instant> = OnceLock::new();

    /// A test's clock, which prints as the test ends how long it took and
    /// how long the weighted metrics' tests have run, against their
    /// budget.
    struct Timed {
        name: &'static str,
        began: Instant,
    }

    fn timed(name: &'static str) -> Timed {
        BEGAN.get_or_init(Instant::now);
        Timed {
            name,
            began: Instant::now(),
        }
    }

    impl Drop for Timed {
        fn drop(&mut self) {
            let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
            eprintln!(
                "{}: {:.3} s; the weighted metrics {suite:.3} s so far, of their \
                 {WEIGHTED_BUDGET_S} s of M3.4's budget at the default number of cases",
                self.name,
                self.began.elapsed().as_secs_f64()
            );
        }
    }

    /// `a`, a `w`-wide RGB image, with Gaussian noise added whose σ grows
    /// from 0.01 at the left edge to 0.05 at the right, so that where a
    /// weight lies matters.
    fn noisy(a: &[f64], w: usize, seed: u64) -> Vec<f64> {
        let mut rng = Rng(seed);
        a.iter()
            .enumerate()
            .map(|(i, &v)| v + (0.01 + 0.04 * (i / 3 % w) as f64 / w as f64) * rng.gauss())
            .collect()
    }

    fn weights(width: usize, height: usize, w: &[f64]) -> Weights<'_> {
        Weights { width, height, w }
    }

    /// The SSIM map of two `w` × `h` RGB images, from `ssim` itself on
    /// each position's 11×11 crop: position `(x0, y0)` is at
    /// `y0 * (w - 10) + x0`.
    fn ssim_map(a: &[f64], b: &[f64], w: usize, h: usize) -> Vec<f64> {
        let crop = |p: &[f64], x0: usize, y0: usize| -> Vec<f64> {
            (y0..y0 + 11)
                .flat_map(|y| p[(y * w + x0) * 3..(y * w + x0 + 11) * 3].iter().copied())
                .collect()
        };
        (0..h - 10)
            .flat_map(|y0| (0..w - 10).map(move |x0| (x0, y0)))
            .map(|(x0, y0)| {
                let (ca, cb) = (crop(a, x0, y0), crop(b, x0, y0));
                ssim(View::new(11, 11, 3, &ca), View::new(11, 11, 3, &cb)).unwrap()
            })
            .collect()
    }

    /// The mean of an SSIM map with each position's value counted by
    /// `weight` of the position.
    fn map_mean(map: &[f64], w: usize, weight: impl Fn(usize, usize) -> f64) -> f64 {
        let (mut total, mut sum) = (0.0, 0.0);
        for (m, s) in map.iter().enumerate() {
            let k = weight(m % (w - 10), m / (w - 10));
            total += k * s;
            sum += k;
        }
        total / sum
    }

    /// Weights of 1 everywhere reproduce `psnr`, `ssim` and `diff_stats`
    /// exactly (M3 plan 8.1).
    #[test]
    fn unit_weights_give_the_plain_metrics() {
        let _t = timed("unit_weights_give_the_plain_metrics");
        let threshold = 0.05;
        for (w, h, seed) in [(40, 23, 7), (11, 11, 3), (33, 57, 5)] {
            let a = scene(w, h);
            let b = noisy(&a, w, seed);
            // 8-bit codes too, as a GPU render against the f64 reference.
            let codes: Vec<u8> = b
                .iter()
                .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect();
            let ones = vec![1.0; w * h];
            let unit = weights(w, h, &ones);
            let (va, vb, vc) = (
                View::new(w, h, 3, &a),
                View::new(w, h, 3, &b),
                View::new(w, h, 3, &codes),
            );
            let p = psnr(va, vb).unwrap();
            let s = ssim(va, vb).unwrap();
            let d = diff_stats(va, vb, threshold).unwrap();
            assert!(p.is_finite() && s < 0.999, "{w}x{h}: {p} dB, {s}");
            assert!(d.count_over > 0 && d.count_over < w * h, "{w}x{h}: {d:?}");
            assert_eq!(psnr_weighted(va, vb, &unit).unwrap().to_bits(), p.to_bits());
            assert_eq!(ssim_weighted(va, vb, &unit).unwrap().to_bits(), s.to_bits());
            assert_eq!(diff_stats_weighted(va, vb, &unit, threshold).unwrap(), d);
            assert_eq!(
                psnr_weighted(va, vc, &unit).unwrap().to_bits(),
                psnr(va, vc).unwrap().to_bits()
            );
            assert_eq!(
                ssim_weighted(vc, va, &unit).unwrap().to_bits(),
                ssim(vc, va).unwrap().to_bits()
            );
            assert_eq!(
                diff_stats_weighted(va, vc, &unit, threshold).unwrap(),
                diff_stats(va, vc, threshold).unwrap()
            );
            // Equal images, as the plain metrics have them.
            assert_eq!(psnr_weighted(va, va, &unit), Ok(f64::INFINITY));
            assert_eq!(
                ssim_weighted(va, va, &unit).unwrap().to_bits(),
                ssim(va, va).unwrap().to_bits()
            );
            eprintln!(
                "{w}x{h}: {p:.3} dB, SSIM {s:.5} and {} pixels over, alike",
                d.count_over
            );
        }

        // Controls: weights that aren't 1 everywhere don't. The left half
        // at ½ moves the PSNR and the SSIM towards the noisier right
        // half's.
        let (w, h) = (40, 23);
        let a = scene(w, h);
        let b = noisy(&a, w, 7);
        let (va, vb) = (View::new(w, h, 3, &a), View::new(w, h, 3, &b));
        let halved: Vec<f64> = (0..w * h)
            .map(|i| if i % w < w / 2 { 0.5 } else { 1.0 })
            .collect();
        let k = weights(w, h, &halved);
        let p = psnr(va, vb).unwrap();
        let off = must_fail(
            "the left half's weights halved, PSNR",
            at_most((psnr_weighted(va, vb, &k).unwrap() - p).abs(), 0.0),
        );
        assert!(off > 0.3, "{off} dB");
        let s = ssim(va, vb).unwrap();
        let off_s = must_fail(
            "the left half's weights halved, SSIM",
            at_most((ssim_weighted(va, vb, &k).unwrap() - s).abs(), 0.0),
        );
        assert!(off_s > 0.01, "{off_s}");
        // A mask has no halves: diff_stats counts a pixel of weight ½ as
        // any other, and loses the left half's only once its weights are
        // 0.
        let d = diff_stats(va, vb, threshold).unwrap();
        assert_eq!(diff_stats_weighted(va, vb, &k, threshold).unwrap(), d);
        let right: Vec<f64> = halved.iter().map(|&v| f64::from(v == 1.0)).collect();
        let masked = diff_stats_weighted(va, vb, &weights(w, h, &right), threshold).unwrap();
        must_fail("the left half masked out, diff_stats", masked == d);
        assert!(masked.count_over < d.count_over, "{masked:?} {d:?}");
        assert!(masked.over.iter().all(|&(x, _)| x >= w / 2), "{masked:?}");
        eprintln!(
            "controls: the left half at ½ is {off:.3} dB and {off_s:.5} of SSIM off; masked \
             out, {} of {} pixels over are left",
            masked.count_over, d.count_over
        );
    }

    /// An error planted where the weight is 0 changes no weighted metric.
    #[test]
    fn an_error_at_weight_zero_changes_nothing() {
        let _t = timed("an_error_at_weight_zero_changes_nothing");
        let (w, h) = (40, 30);
        let threshold = 0.05;
        let a = scene(w, h);
        let b = noisy(&a, w, 21);
        // Nothing from column 20 on, and a ramp down the rows before it.
        let k: Vec<f64> = (0..w * h)
            .map(|i| {
                if i % w >= 20 {
                    0.0
                } else {
                    0.25 + 0.75 * (i / w) as f64 / h as f64
                }
            })
            .collect();
        let k = weights(w, h, &k);
        let (va, vb) = (View::new(w, h, 3, &a), View::new(w, h, 3, &b));
        let before = (
            psnr_weighted(va, vb, &k).unwrap(),
            ssim_weighted(va, vb, &k).unwrap(),
            diff_stats_weighted(va, vb, &k, threshold).unwrap(),
        );
        assert!(before.0.is_finite() && before.1 < 0.999, "{before:?}");
        // `b` with `steps` added to the RGB of the 3×3 pixels about
        // `(x, y)`.
        let planted = |x: usize, y: usize, steps: [f64; 3]| {
            let mut c = b.clone();
            for (px, py) in (y - 1..=y + 1).flat_map(|py| (x - 1..=x + 1).map(move |px| (px, py))) {
                assert_eq!(k.w[py * w + px], 0.0);
                for (ch, step) in steps.iter().enumerate() {
                    c[(py * w + px) * 3 + ch] += step;
                }
            }
            c
        };

        // Deep in the masked part, where no window with a weight reaches:
        // the nearest pixel with one is 10 columns away, and a window
        // covers 5 either side of its centre.
        let deep = planted(30, 15, [0.7, -0.4, 0.0]);
        let vd = View::new(w, h, 3, &deep);
        assert_eq!(
            psnr_weighted(va, vd, &k).unwrap().to_bits(),
            before.0.to_bits()
        );
        assert_eq!(
            ssim_weighted(va, vd, &k).unwrap().to_bits(),
            before.1.to_bits()
        );
        assert_eq!(
            diff_stats_weighted(va, vd, &k, threshold).unwrap(),
            before.2
        );
        // Controls: the plain metrics see it.
        let (p, s, d) = (
            psnr(va, vb).unwrap(),
            ssim(va, vb).unwrap(),
            diff_stats(va, vb, threshold).unwrap(),
        );
        let lost = must_fail(
            "the plain PSNR on the error at weight 0",
            at_least(psnr(va, vd).unwrap(), p),
        );
        assert!(p - lost > 3.0, "{p} to {lost} dB");
        let lost_s = must_fail(
            "the plain SSIM on the error at weight 0",
            at_least(ssim(va, vd).unwrap(), s),
        );
        assert!(s - lost_s > 0.01, "{s} to {lost_s}");
        let seen = diff_stats(va, vd, threshold).unwrap();
        let max = must_fail(
            "the plain diff_stats on the error at weight 0",
            at_most(seen.max, d.max),
        );
        assert!(max > 0.6 && seen.count_over > d.count_over, "{seen:?}");

        // Whatever the pixel holds: a NaN there poisons the plain PSNR
        // and no weighted metric.
        let nan = planted(30, 15, [f64::NAN; 3]);
        let vn = View::new(w, h, 3, &nan);
        assert!(psnr(va, vn).unwrap().is_nan());
        assert_eq!(
            psnr_weighted(va, vn, &k).unwrap().to_bits(),
            before.0.to_bits()
        );
        assert_eq!(
            ssim_weighted(va, vn, &k).unwrap().to_bits(),
            before.1.to_bits()
        );
        assert_eq!(
            diff_stats_weighted(va, vn, &k, threshold).unwrap(),
            before.2
        );

        // Beside the weighted part, the PSNR and diff_stats still see
        // nothing: theirs is each pixel's own weight. SSIM's is its
        // window's (`weighted_ssim_weighs_window_centres`).
        let beside = planted(21, 15, [0.7, -0.4, 0.0]);
        let ve = View::new(w, h, 3, &beside);
        assert_eq!(
            psnr_weighted(va, ve, &k).unwrap().to_bits(),
            before.0.to_bits()
        );
        assert_eq!(
            diff_stats_weighted(va, ve, &k, threshold).unwrap(),
            before.2
        );
        eprintln!(
            "an error at weight 0: {:.3} dB, SSIM {:.5} and {} pixels over, before and after; \
             the plain metrics go from {p:.3} to {lost:.3} dB, from {s:.5} to {lost_s:.5} and \
             from {} to {} pixels over",
            before.0, before.1, before.2.count_over, d.count_over, seen.count_over
        );
    }

    /// An error planted where the weight is 1 lowers the weighted PSNR by
    /// the amount worked out from the error and the weights.
    #[test]
    fn an_error_at_weight_one_costs_what_it_adds() {
        let _t = timed("an_error_at_weight_one_costs_what_it_adds");
        let (w, h) = (40, 23);
        let a = scene(w, h);
        let b = noisy(&a, w, 33);
        // Nothing in the first 10 columns, 1 in the rows above 12 and ½
        // below, so the weights' sum is far from the number of pixels.
        let k: Vec<f64> = (0..w * h)
            .map(|i| match (i % w, i / w) {
                (x, _) if x < 10 => 0.0,
                (_, y) if y < 12 => 1.0,
                _ => 0.5,
            })
            .collect();
        let sum: f64 = k.iter().sum();
        assert_eq!(sum, 525.0);
        let k = weights(w, h, &k);
        let (va, vb) = (View::new(w, h, 3, &a), View::new(w, h, 3, &b));
        let before = psnr_weighted(va, vb, &k).unwrap();
        let mse = 10f64.powf(-before / 10.0);
        let db = |mse: f64| -10.0 * mse.log10();
        let holds = |got: f64, want: f64| at_most((got - want).abs(), 1e-9);
        for (x, y, weight) in [(25, 5, 1.0), (25, 18, 0.5)] {
            assert_eq!(k.w[y * w + x], weight);
            let i = (y * w + x) * 3;
            let mut c = b.clone();
            let mut added = 0.0;
            for (ch, step) in [0.8, -0.6, 0.7].into_iter().enumerate() {
                c[i + ch] += step;
                added += (a[i + ch] - c[i + ch]).powi(2) - (a[i + ch] - b[i + ch]).powi(2);
            }
            let after = psnr_weighted(va, View::new(w, h, 3, &c), &k).unwrap();
            let want = db(mse + weight * added / (3.0 * sum));
            assert_eq!(
                holds(after, want).passes(),
                Some(true),
                "{after} dB, not {want}"
            );
            assert!(before - after > 1.0, "{before} to {after} dB");
            // Controls: the same error counted as the plain PSNR would,
            // over every pixel rather than over the weights' sum; and at
            // weight ½, counted in full.
            let plain = db(mse + weight * added / (3 * w * h) as f64);
            let off = must_fail(
                "the pixels counted in place of their weights",
                holds(after, plain),
            );
            assert!(off > 0.3, "{off} dB");
            if weight < 1.0 {
                let full = db(mse + added / (3.0 * sum));
                let off = must_fail("an error at weight ½ counted in full", holds(after, full));
                assert!(off > 0.3, "{off} dB");
            }
            eprintln!(
                "an error at weight {weight}: {before:.3} to {after:.3} dB, {:.1e} dB from the \
                 amount worked out; over every pixel it would be {plain:.3} dB",
                (after - want).abs()
            );
        }
    }

    /// The weighted SSIM is the SSIM map's mean over the window centres,
    /// each with its pixel's weight.
    #[test]
    fn weighted_ssim_weighs_window_centres() {
        let _t = timed("weighted_ssim_weighs_window_centres");
        let (w, h) = (36, 28);
        let a = scene(w, h);
        let b = noisy(&a, w, 44);
        let (va, vb) = (View::new(w, h, 3, &a), View::new(w, h, 3, &b));
        let map = ssim_map(&a, &b, w, h);

        // One pixel with a weight: its window's value alone, bit for
        // bit, at the corners of the centres' range and inside it.
        for (cx, cy) in [(5, 5), (w - 6, h - 6), (17, 12)] {
            let mut k = vec![0.0; w * h];
            k[cy * w + cx] = 0.25;
            let got = ssim_weighted(va, vb, &weights(w, h, &k)).unwrap();
            assert_eq!(got.to_bits(), map[(cy - 5) * (w - 10) + cx - 5].to_bits());
        }

        // Any weights: the map's mean by the weight at each centre. A
        // third of these are 0.
        let mut rng = Rng(45);
        let k: Vec<f64> = (0..w * h)
            .map(|_| (1.5 * rng.unit() - 0.5).max(0.0))
            .collect();
        let got = ssim_weighted(va, vb, &weights(w, h, &k)).unwrap();
        let want = map_mean(&map, w, |x0, y0| k[(y0 + 5) * w + x0 + 5]);
        let any = (got - want).abs();
        assert!(any < 1e-12, "{got} against {want}");
        let plain = ssim(va, vb).unwrap();
        assert!(
            (got - plain).abs() > 1e-4,
            "{got} against the plain {plain}"
        );

        // A mask of the left half, and an error planted just past its
        // edge, at weight 0: the windows centred up to 5 columns inside
        // the mask cover it, so the weighted SSIM falls, where the
        // weighted PSNR sees nothing.
        let mask: Vec<f64> = (0..w * h).map(|i| f64::from(i % w < 18)).collect();
        let k = weights(w, h, &mask);
        let mut c = b.clone();
        for (x, y) in (12..=16).flat_map(|y| (18..=20).map(move |x| (x, y))) {
            assert_eq!(mask[y * w + x], 0.0);
            c[(y * w + x) * 3] += 0.5;
            c[(y * w + x) * 3 + 1] -= 0.3;
        }
        let vc = View::new(w, h, 3, &c);
        assert_eq!(
            psnr_weighted(va, vc, &k).unwrap().to_bits(),
            psnr_weighted(va, vb, &k).unwrap().to_bits()
        );
        let (clean, got) = (
            ssim_weighted(va, vb, &k).unwrap(),
            ssim_weighted(va, vc, &k).unwrap(),
        );
        let hurt = ssim_map(&a, &c, w, h);
        let holds = |want: f64| at_most((got - want).abs(), 1e-12);
        let centres = map_mean(&hurt, w, |x0, y0| mask[(y0 + 5) * w + x0 + 5]);
        assert_eq!(
            holds(centres).passes(),
            Some(true),
            "{got} against {centres}"
        );
        // Controls, the two ways of weighing the pixels under a window
        // rather than its centre. With each pixel's own weight on its
        // error, as the PSNR has it, the error past the edge is in no
        // window and the SSIM stays what it was.
        let blind = must_fail(
            "an error at weight 0 kept out of every window",
            holds(clean),
        );
        assert!(blind > 0.005, "{clean} to {got}");
        // And with each window weighed by the mean of the weights under
        // it, the windows centred past the edge count too, which the
        // error fills.
        let under = |x0: usize, y0: usize| {
            let rows = mask.chunks_exact(w).skip(y0).take(11);
            rows.flat_map(|row| &row[x0..x0 + 11]).sum::<f64>() / 121.0
        };
        let pixels = map_mean(&hurt, w, under);
        let off = must_fail("each window weighed by the pixels under it", holds(pixels));
        assert!(off > 0.005, "{got} against {pixels}");

        // Whatever a pixel of weight 0 holds, it is in each counted
        // window over it: the last centre with a weight is column 17,
        // whose window ends at column 22, so a NaN there poisons the
        // weighted SSIM, where the weighted PSNR never reads it.
        let nan_at = |x: usize| {
            let mut n = b.clone();
            assert_eq!(mask[14 * w + x], 0.0);
            n[(14 * w + x) * 3] = f64::NAN;
            n
        };
        let under_a_window = nan_at(22);
        let vn = View::new(w, h, 3, &under_a_window);
        assert!(ssim_weighted(va, vn, &k).unwrap().is_nan());
        assert_eq!(
            psnr_weighted(va, vn, &k).unwrap().to_bits(),
            psnr_weighted(va, vb, &k).unwrap().to_bits()
        );
        // Control: a column further, no counted window reaches it.
        let past_them = nan_at(23);
        let past = ssim_weighted(va, View::new(w, h, 3, &past_them), &k).unwrap();
        must_fail("a NaN a column past the last counted window", past.is_nan());
        assert_eq!(past.to_bits(), clean.to_bits());
        eprintln!(
            "the weighted SSIM by centres: {any:.1e} from the map's mean on any weights; an \
             error past a mask's edge takes it from {clean:.5} to {got:.5}, the map's mean by \
             centres ({:.1e} off), and by the pixels under each window it would be {pixels:.5}",
            (got - centres).abs()
        );
    }

    /// `equirect_coslat` gives each row the cosine of its centre's
    /// latitude.
    #[test]
    fn equirect_coslat_is_each_rows_latitude() {
        let _t = timed("equirect_coslat_is_each_rows_latitude");
        // An odd height, so that the middle row lies on the equator.
        let (w, h) = (16, 9);
        let k = equirect_coslat(w, h);
        assert_eq!(k.len(), w * h);
        for y in 0..h {
            // cos(π/2 − t) is sin t.
            let want = ((y as f64 + 0.5) / h as f64 * PI).sin();
            for x in 0..w {
                assert!((k[y * w + x] - want).abs() < 1e-15, "({x}, {y})");
            }
        }
        assert_eq!(k[4 * w], 1.0);
        assert!(equirect_coslat(0, 5).is_empty() && equirect_coslat(5, 0).is_empty());

        // As the renderer lays an equirect picture's rows: each pixel's
        // share of the weights is its share of the sphere, the solid
        // angle between its corners' directions (2π/w of longitude times
        // the difference of the sines of its edges' latitudes) over 4π.
        let view = crate::ViewParams::equirect(glam::DQuat::IDENTITY);
        let size = (w as u32, h as u32);
        let solid: Vec<f64> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                let (top, bottom) = (view.dir(size, x, y), view.dir(size, x, y + 1.0));
                TAU / w as f64 * (top.y - bottom.y)
            })
            .collect();
        let sphere: f64 = solid.iter().sum();
        assert!((sphere - 2.0 * TAU).abs() < 1e-12, "{sphere}");
        // The largest error of a pixel's share of `k`, as a part of the
        // share it should have.
        let shares = |k: &[f64]| {
            let sum: f64 = k.iter().sum();
            let worst = (0..w * h)
                .map(|i| (k[i] / sum / (solid[i] / sphere) - 1.0).abs())
                .fold(0.0, f64::max);
            at_most(worst, 1e-12)
        };
        assert_eq!(shares(&k).passes(), Some(true), "{}", shares(&k));
        // Controls: the latitude of each row's top edge rather than its
        // centre's, and no weights at all.
        let edges: Vec<f64> = (0..w * h)
            .map(|i| (FRAC_PI_2 - (i / w) as f64 / h as f64 * PI).cos())
            .collect();
        let off = must_fail("the latitudes of the rows' top edges", shares(&edges));
        assert!(off > 0.1, "{off}");
        let flat = must_fail("uniform weights' shares", shares(&vec![1.0; w * h]));
        assert!(flat > 0.1, "{flat}");

        // An error that grows towards the poles, 0.2 sin(latitude) on
        // every channel: over the sphere its mean square is 0.2²/3, which
        // the weighted PSNR gives and, as its control, the plain one
        // doesn't (0.2²/2 over the picture, 1.76 dB less).
        let (w, h) = (64, 32);
        let a = vec![0.5; w * h * 3];
        let b: Vec<f64> = (0..w * h * 3)
            .map(|i| {
                let latitude = FRAC_PI_2 - ((i / 3 / w) as f64 + 0.5) / h as f64 * PI;
                0.5 + 0.2 * latitude.sin()
            })
            .collect();
        let (va, vb) = (View::new(w, h, 3, &a), View::new(w, h, 3, &b));
        let sphere = -10.0 * (0.2f64.powi(2) / 3.0).log10();
        let holds = |k: &[f64]| {
            let got = psnr_weighted(va, vb, &weights(w, h, k)).unwrap();
            at_most((got - sphere).abs(), 0.01)
        };
        let k = equirect_coslat(w, h);
        assert_eq!(holds(&k).passes(), Some(true), "{}", holds(&k));
        let uniform = must_fail(
            "uniform weights on an error at the poles",
            holds(&vec![1.0; w * h]),
        );
        assert!(uniform > 1.5, "{uniform} dB");
        eprintln!(
            "coslat: a pixel's share is its solid angle's within 1e-12 (the rows' top edges \
             {off:.3} off, uniform weights {flat:.3}); an error of 0.2 sin(latitude) is \
             {:.4} dB, {sphere:.4} dB over the sphere, and {uniform:.3} dB off unweighted",
            psnr_weighted(va, vb, &weights(w, h, &k)).unwrap()
        );
    }

    /// Weights that don't fit the images, or that leave nothing to
    /// measure, are refused by every weighted metric.
    #[test]
    fn weights_that_dont_fit_are_refused() {
        let _t = timed("weights_that_dont_fit_are_refused");
        let (w, h) = (16, 12);
        let a = scene(w, h);
        let b = noisy(&a, w, 61);
        let (va, vb) = (View::new(w, h, 3, &a), View::new(w, h, 3, &b));
        let ones = vec![1.0; w * h];
        let all = |k: &Weights<'_>| {
            (
                psnr_weighted(va, vb, k),
                ssim_weighted(va, vb, k),
                diff_stats_weighted(va, vb, k, 0.05),
            )
        };
        let (p, s, d) = all(&weights(w, h, &ones));
        assert!(p.is_ok() && s.is_ok() && d.is_ok(), "{p:?} {s:?} {d:?}");
        // Each of these is a control of that: the three metrics refuse
        // it, in the same words, which name what is wrong.
        let refused = |name: &str, k: &Weights<'_>, words: &str| {
            let (p, s, d) = all(k);
            let said = [
                must_fail(&format!("{name}, PSNR"), p),
                must_fail(&format!("{name}, SSIM"), s),
                must_fail(&format!("{name}, diff_stats"), d),
            ];
            assert!(said[0].0.contains(words), "{name}: {}", said[0]);
            assert!(said[1] == said[0] && said[2] == said[0], "{name}: {said:?}");
        };
        refused(
            "a value short",
            &weights(w, h, &ones[1..]),
            "need 192 values, have 191",
        );
        let long = vec![1.0; w * h + 1];
        refused(
            "a value too many",
            &weights(w, h, &long),
            "need 192 values, have 193",
        );
        refused(
            "another picture's size",
            &weights(h, w, &ones),
            "12x16 weights against 16x12 images",
        );
        let with = |x: usize, y: usize, v: f64| {
            let mut k = ones.clone();
            k[y * w + x] = v;
            k
        };
        refused(
            "a weight below 0",
            &weights(w, h, &with(3, 2, -1e-9)),
            "the weight at (3, 2) is -0.000000001",
        );
        refused(
            "a weight that is no number",
            &weights(w, h, &with(15, 11, f64::NAN)),
            "the weight at (15, 11) is NaN",
        );
        refused(
            "a weight without end",
            &weights(w, h, &with(0, 0, f64::INFINITY)),
            "the weight at (0, 0) is inf",
        );
        refused(
            "weights of 0 everywhere",
            &weights(w, h, &vec![0.0; w * h]),
            "sum to 0",
        );
        let huge = vec![f64::MAX; w * h];
        refused(
            "weights past f64 in sum",
            &weights(w, h, &huge),
            "sum to inf",
        );
        // Weights only where no window is centred: the PSNR and
        // diff_stats have pixels to measure, and SSIM has no window.
        let edge: Vec<f64> = (0..w * h)
            .map(|i| f64::from(i % w < 5 || i / w >= h - 5))
            .collect();
        let (p, s, d) = all(&weights(w, h, &edge));
        assert!(p.is_ok() && d.is_ok(), "{p:?} {d:?}");
        let said = must_fail("weights at no window's centre, SSIM", s);
        assert!(said.0.contains("0 at every centre"), "{said}");
        // The images are still checked first.
        let said = must_fail(
            "images of two sizes",
            psnr_weighted(va, View::new(h, w, 3, &b), &weights(w, h, &ones)),
        );
        assert_eq!(said.0, "16x12 against 12x16");
    }

    /// What `weighted_metrics_are_the_plain_ones_of_the_weighted_pixels`
    /// finds wrong on `a` and `b`, `w` × `h` RGB images, when the weighted
    /// metrics are given the weights `given` and the pictures the plain
    /// ones measure are made from `counted`, whole numbers both: nothing,
    /// when the two are the same.
    fn against_plain(
        w: usize,
        h: usize,
        a: &[f64],
        b: &[f64],
        given: &[f64],
        counted: &[f64],
        threshold: f64,
    ) -> Result<(), String> {
        let (va, vb) = (View::new(w, h, 3, a), View::new(w, h, 3, b));
        let mask = |k: &[f64]| -> Vec<f64> { k.iter().map(|&v| f64::from(v > 0.0)).collect() };
        let (given_mask, counted_mask) = (mask(given), mask(counted));
        // `p`'s pixels in a row, each as many times as `times` says, and
        // how many that makes.
        let repeated = |p: &[f64], times: &[f64]| -> (usize, Vec<f64>) {
            let row: Vec<f64> = p
                .chunks_exact(3)
                .zip(times)
                .flat_map(|(px, &t)| std::iter::repeat_n(px, t as usize).flatten().copied())
                .collect();
            (row.len() / 3, row)
        };
        let ((n, ra), (_, rb)) = (repeated(a, counted), repeated(b, counted));
        let ((m, ma), (_, mb)) = (repeated(a, &counted_mask), repeated(b, &counted_mask));
        let mut wrong = Vec::new();
        let psnr_w = psnr_weighted(va, vb, &weights(w, h, given));
        let psnr_m = psnr_weighted(va, vb, &weights(w, h, &given_mask));
        let diff = diff_stats_weighted(va, vb, &weights(w, h, given), threshold);
        if n == 0 {
            if psnr_w.is_ok() || psnr_m.is_ok() || diff.is_ok() {
                wrong.push(format!(
                    "PSNR and diff_stats of nothing: {psnr_w:?} {psnr_m:?} {diff:?}"
                ));
            }
        } else {
            let (got, want) = (
                psnr_w.map_err(|e| e.0)?,
                psnr(View::new(n, 1, 3, &ra), View::new(n, 1, 3, &rb)).map_err(|e| e.0)?,
            );
            // Equal images are infinite on both sides.
            if got != want && at_most((got - want).abs(), 1e-9).passes() != Some(true) {
                wrong.push(format!("PSNR {got} dB, the repeated pixels' {want}"));
            }
            let kept = (View::new(m, 1, 3, &ma), View::new(m, 1, 3, &mb));
            let (got, want) = (
                psnr_m.map_err(|e| e.0)?,
                psnr(kept.0, kept.1).map_err(|e| e.0)?,
            );
            if got.to_bits() != want.to_bits() {
                wrong.push(format!("masked PSNR {got} dB, the kept pixels' {want}"));
            }
            let (got, want) = (
                diff.map_err(|e| e.0)?,
                diff_stats(kept.0, kept.1, threshold).map_err(|e| e.0)?,
            );
            let places = got.over.iter().chain(&got.worst);
            if (got.max, got.count_over) != (want.max, want.count_over)
                || !places.clone().all(|&(x, y)| counted[y * w + x] > 0.0)
            {
                wrong.push(format!("diff_stats {got:?}, the kept pixels' {want:?}"));
            }
        }
        let centre = |x0: usize, y0: usize| counted[(y0 + 5) * w + x0 + 5];
        let got = ssim_weighted(va, vb, &weights(w, h, given));
        if (0..h - 10).all(|y0| (0..w - 10).all(|x0| centre(x0, y0) == 0.0)) {
            if got.is_ok() {
                wrong.push(format!("SSIM of no window: {got:?}"));
            }
        } else {
            let (got, want) = (
                got.map_err(|e| e.0)?,
                map_mean(&ssim_map(a, b, w, h), w, centre),
            );
            if at_most((got - want).abs(), 1e-12).passes() != Some(true) {
                wrong.push(format!("SSIM {got}, the crops' mean {want}"));
            }
        }
        if wrong.is_empty() {
            Ok(())
        } else {
            Err(wrong.join("; "))
        }
    }

    /// On any small images and any weights, each weighted metric is the
    /// plain one of another picture: the PSNR that of a row with each
    /// pixel repeated as many times as its weight (whole here, 0 to 3),
    /// within 1e-9 dB, and under a mask that of the pixels kept, bit for
    /// bit; `diff_stats` that of the pixels kept; the SSIM the mean of
    /// `ssim` of each window's own crop, by the weight at its centre.
    #[test]
    fn weighted_metrics_are_the_plain_ones_of_the_weighted_pixels() {
        let _t = timed("weighted_metrics_are_the_plain_ones_of_the_weighted_pixels");
        // Control: a pixel that the metrics leave out, a window's centre
        // and the worst of the picture, counted among the plain ones'.
        let (w, h) = (13, 12);
        let a = scene(w, h);
        let mut b = noisy(&a, w, 71);
        let given: Vec<f64> = (0..w * h).map(|i| (i % 4) as f64).collect();
        let at = 6 * w + 6;
        assert_eq!(given[at], 0.0);
        b[at * 3] += 0.6;
        assert_eq!(against_plain(w, h, &a, &b, &given, &given, 0.05), Ok(()));
        let mut counted = given.clone();
        counted[at] = 1.0;
        let why = must_fail(
            "a pixel of weight 0 counted",
            against_plain(w, h, &a, &b, &given, &counted, 0.05),
        );
        let parts: Vec<&str> = why.split("; ").collect();
        let metrics = ["PSNR ", "masked PSNR ", "diff_stats ", "SSIM "];
        assert_eq!(parts.len(), metrics.len(), "{why}");
        for (part, metric) in parts.iter().zip(metrics) {
            assert!(part.starts_with(metric), "{metric}: {why}");
        }

        let images = (11usize..=13, 11usize..=13).prop_flat_map(|(w, h)| {
            let n = w * h;
            (
                Just(w),
                Just(h),
                proptest::collection::vec(0.0..=1.0f64, 3 * n),
                proptest::collection::vec(0.0..=1.0f64, 3 * n),
                proptest::collection::vec(0u8..=3, n),
            )
        });
        proptest::proptest!(|((w, h, a, b, k) in images, threshold in 0.0..=1.0f64)| {
            let k: Vec<f64> = k.into_iter().map(f64::from).collect();
            let found = against_plain(w, h, &a, &b, &k, &k, threshold);
            proptest::prop_assert!(found.is_ok(), "{}", found.unwrap_err());
        });
    }
}
