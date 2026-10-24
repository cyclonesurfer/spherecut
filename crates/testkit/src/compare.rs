//! Image metrics for the render tests (M1 plan, section 6): PSNR over
//! RGB, SSIM on BT.709 luma, and a count of the pixels that differ by
//! more than a threshold.
//!
//! Every metric works in unit scale, samples in `[0, 1]`: 8-bit samples
//! count as `v / 255`, 16-bit ones as `v / 65535`, floats as they are. So
//! an 8-bit GPU image compares with an `f64` CPU reference directly, and
//! a PSNR between two 8-bit images is the usual one with a peak of 255.
//! Only the first three channels of a pixel (RGB) count; a fourth, alpha,
//! is skipped.

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
/// samples for their size.
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
    let mut total = 0.0;
    let mut count = 0usize;
    for y0 in 0..=a.height - 11 {
        for x0 in 0..=a.width - 11 {
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
            total += ((2.0 * ma * mb + c1) * (2.0 * cov + c2))
                / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            count += 1;
        }
    }
    Ok(total / count as f64)
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
    let mut s = DiffStats {
        max: 0.0,
        worst: None,
        count_over: 0,
        over: Vec::new(),
    };
    for y in 0..a.height {
        for x in 0..a.width {
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
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
