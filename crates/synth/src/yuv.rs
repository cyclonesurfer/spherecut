//! RGB ↔ planar Y'CbCr, 8- and 10-bit: BT.709 limited range 4:2:0 for
//! most clips, and other matrices, full range, 4:2:2 and 4:4:4 for the
//! colour and pixel-format tests.
//!
//! The synthetic patterns are display-referred values, written straight to
//! Y'CbCr the way a camera would; no linearisation happens here.

use spherecut_media::encode::{ColorTags, YuvPlanes};

use crate::{Rgb, RgbImage};

/// Chroma subsampling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Chroma {
    #[default]
    Yuv420,
    Yuv422,
    Yuv444,
}

impl Chroma {
    /// log2 of the horizontal and vertical subsampling.
    pub fn shift(self) -> (u32, u32) {
        match self {
            Chroma::Yuv420 => (1, 1),
            Chroma::Yuv422 => (1, 0),
            Chroma::Yuv444 => (0, 0),
        }
    }
}

/// How RGB becomes Y'CbCr.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct YuvFormat {
    /// The matrix's luma weights of red and blue.
    pub kr: f64,
    pub kb: f64,
    pub full_range: bool,
    pub chroma: Chroma,
}

impl YuvFormat {
    /// BT.709, limited range, 4:2:0.
    pub const BT709_420: YuvFormat = YuvFormat {
        kr: 0.2126,
        kb: 0.0722,
        full_range: false,
        chroma: Chroma::Yuv420,
    };

    /// The format a stream tagged `tags` says it has: BT.601 for matrix
    /// codes 5 and 6, BT.2020 for 9, BT.709 otherwise; full range for
    /// range code 2.
    pub fn tagged(tags: &ColorTags, chroma: Chroma) -> YuvFormat {
        let (kr, kb) = match tags.matrix {
            5 | 6 => (0.299, 0.114),
            9 => (0.2627, 0.0593),
            _ => (0.2126, 0.0722),
        };
        YuvFormat {
            kr,
            kb,
            full_range: tags.range == 2,
            chroma,
        }
    }

    fn rgb_to_ypbpr(&self, c: Rgb) -> (f64, f64, f64) {
        let [r, g, b] = c.map(|v| v.clamp(0.0, 1.0));
        let (kr, kb) = (self.kr, self.kb);
        let y = kr * r + (1.0 - kr - kb) * g + kb * b;
        let pb = (b - y) / (2.0 * (1.0 - kb));
        let pr = (r - y) / (2.0 * (1.0 - kr));
        (y, pb, pr)
    }

    fn ypbpr_to_rgb(&self, y: f64, pb: f64, pr: f64) -> Rgb {
        let (kr, kb) = (self.kr, self.kb);
        let r = y + 2.0 * (1.0 - kr) * pr;
        let b = y + 2.0 * (1.0 - kb) * pb;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b]
    }

    /// Luma and chroma at `bits` (8 or 10): the nominal ranges of H.273,
    /// 16 to 235 and 16 to 240 scaled up for limited range, and all of
    /// 0 to 2^bits − 1 for full.
    fn quant(&self, y: f64, pb: f64, pr: f64, bits: u32) -> (u16, u16, u16) {
        let max = f64::from((1u32 << bits) - 1);
        if self.full_range {
            let mid = f64::from(1u32 << (bits - 1));
            let q = |v: f64| v.round().clamp(0.0, max) as u16;
            return (q(max * y), q(mid + max * pb), q(mid + max * pr));
        }
        let s = f64::from(1u32 << (bits - 8));
        let q = |v: f64, lo: f64, hi: f64| (v * s).round().clamp(lo * s, hi * s) as u16;
        (
            q(16.0 + 219.0 * y, 16.0, 235.0),
            q(128.0 + 224.0 * pb, 16.0, 240.0),
            q(128.0 + 224.0 * pr, 16.0, 240.0),
        )
    }

    /// The inverse of `quant`.
    fn dequant(&self, y: f64, u: f64, v: f64, bits: u32) -> (f64, f64, f64) {
        if self.full_range {
            let max = f64::from((1u32 << bits) - 1);
            let mid = f64::from(1u32 << (bits - 1));
            return (y / max, (u - mid) / max, (v - mid) / max);
        }
        let s = f64::from(1u32 << (bits - 8));
        (
            (y / s - 16.0) / 219.0,
            (u / s - 128.0) / 224.0,
            (v / s - 128.0) / 224.0,
        )
    }
}

/// Convert an image with even dimensions to planar BT.709 limited-range
/// 4:2:0.
pub fn to_yuv420(img: &RgbImage, bits: u32) -> YuvPlanes {
    to_yuv(img, bits, &YuvFormat::BT709_420)
}

/// Convert an image with even dimensions to planar Y'CbCr in `format`.
/// 4:2:0 chroma is the average of each 2×2 block, so it is centre-sited,
/// which [`crate::media::CLIP_COLOR`] tags. 4:2:2 chroma is taken from
/// the left column of each pair, since H.264's and HEVC's 4:2:2 has no
/// chroma location field and is co-sited (left) by definition.
pub fn to_yuv(img: &RgbImage, bits: u32, format: &YuvFormat) -> YuvPlanes {
    let (w, h) = (img.width as usize, img.height as usize);
    let (sx, sy) = format.chroma.shift();
    let (bw, bh) = (1usize << sx, 1usize << sy);
    let (cw, ch) = (w / bw, h / bh);
    let mut yp = vec![0u16; w * h];
    let mut up = vec![0u16; cw * ch];
    let mut vp = vec![0u16; cw * ch];
    for y in 0..h {
        for x in 0..w {
            let (yy, _, _) = format.rgb_to_ypbpr(img.pixels[y * w + x]);
            yp[y * w + x] = format.quant(yy, 0.0, 0.0, bits).0;
        }
    }
    // The samples each chroma sample is made from: the whole block, or
    // its left column for 4:2:2.
    let (bw_used, bh_used) = match format.chroma {
        Chroma::Yuv422 => (1, bh),
        _ => (bw, bh),
    };
    let n = (bw_used * bh_used) as f64;
    for cy in 0..ch {
        for cx in 0..cw {
            let mut pb = 0.0;
            let mut pr = 0.0;
            for dy in 0..bh_used {
                for dx in 0..bw_used {
                    let px = img.pixels[(bh * cy + dy) * w + bw * cx + dx];
                    let (_, b, r) = format.rgb_to_ypbpr(px);
                    pb += b / n;
                    pr += r / n;
                }
            }
            let (_, u, v) = format.quant(0.0, pb, pr, bits);
            up[cy * cw + cx] = u;
            vp[cy * cw + cx] = v;
        }
    }
    if bits == 8 {
        YuvPlanes::U8([yp, up, vp].map(|p| p.into_iter().map(|v| v as u8).collect()))
    } else {
        YuvPlanes::U16([yp, up, vp])
    }
}

/// Inverse of [`to_yuv420`] with nearest chroma upsampling.
pub fn from_yuv420(planes: &YuvPlanes, width: u32, height: u32, bits: u32) -> RgbImage {
    from_yuv(planes, width, height, bits, &YuvFormat::BT709_420)
}

/// Inverse of [`to_yuv`] with nearest chroma upsampling.
pub fn from_yuv(
    planes: &YuvPlanes,
    width: u32,
    height: u32,
    bits: u32,
    format: &YuvFormat,
) -> RgbImage {
    let (w, h) = (width as usize, height as usize);
    let (sx, sy) = format.chroma.shift();
    let cw = w >> sx;
    let get = |p: usize, i: usize| -> f64 {
        match planes {
            YuvPlanes::U8(pl) => pl[p][i] as f64,
            YuvPlanes::U16(pl) => pl[p][i] as f64,
        }
    };
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let c = (y >> sy) * cw + (x >> sx);
            let (yy, pb, pr) = format.dequant(get(0, y * w + x), get(1, c), get(2, c), bits);
            pixels.push(format.ypbpr_to_rgb(yy, pb, pr).map(|v| v.clamp(0.0, 1.0)));
        }
    }
    RgbImage {
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pattern, render_equirect};

    #[test]
    fn round_trip_is_close() {
        // Constant 2x2 blocks, so chroma subsampling loses nothing.
        let small = render_equirect(Pattern::Seam, 32, 16, 0, 1);
        let img = RgbImage {
            width: 64,
            height: 32,
            pixels: (0..32 * 64)
                .map(|i| small.get((i % 64) / 2, (i / 64) / 2))
                .collect(),
        };
        for (bits, tol) in [(8, 3.0 / 219.0), (10, 3.0 / 876.0)] {
            let back = from_yuv420(&to_yuv420(&img, bits), 64, 32, bits);
            for (a, b) in img.pixels.iter().zip(&back.pixels) {
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < tol, "{bits}-bit: {a:?} vs {b:?}");
                }
            }
        }
    }

    #[test]
    fn other_formats_round_trip_too() {
        // Constant 2x2 blocks again.
        let small = render_equirect(Pattern::Grid, 32, 16, 0, 1);
        let img = RgbImage {
            width: 64,
            height: 32,
            pixels: (0..32 * 64)
                .map(|i| small.get((i % 64) / 2, (i / 64) / 2))
                .collect(),
        };
        let bt601_full = ColorTags {
            range: 2,
            matrix: 6,
            ..ColorTags::BT709_LIMITED
        };
        for chroma in [Chroma::Yuv420, Chroma::Yuv422, Chroma::Yuv444] {
            for tags in [ColorTags::BT709_LIMITED, bt601_full] {
                let f = YuvFormat::tagged(&tags, chroma);
                for (bits, tol) in [(8, 3.0 / 219.0), (10, 3.0 / 876.0)] {
                    let back = from_yuv(&to_yuv(&img, bits, &f), 64, 32, bits, &f);
                    for (a, b) in img.pixels.iter().zip(&back.pixels) {
                        for c in 0..3 {
                            assert!((a[c] - b[c]).abs() < tol, "{f:?} {bits}: {a:?} vs {b:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn full_range_white_and_black_are_the_extremes() {
        let img = RgbImage {
            width: 2,
            height: 2,
            pixels: vec![[1.0; 3], [1.0; 3], [0.0; 3], [0.0; 3]],
        };
        let f = YuvFormat {
            full_range: true,
            ..YuvFormat::BT709_420
        };
        match to_yuv(&img, 10, &f) {
            YuvPlanes::U16(p) => {
                assert_eq!(&p[0][..], &[1023, 1023, 0, 0]);
                assert_eq!((p[1][0], p[2][0]), (512, 512));
            }
            YuvPlanes::U8(_) => unreachable!(),
        }
    }

    #[test]
    fn white_and_black_hit_nominal_levels() {
        let img = RgbImage {
            width: 2,
            height: 2,
            pixels: vec![[1.0; 3], [1.0; 3], [0.0; 3], [0.0; 3]],
        };
        match to_yuv420(&img, 10) {
            YuvPlanes::U16(p) => assert_eq!(&p[0][..], &[940, 940, 64, 64]),
            YuvPlanes::U8(_) => unreachable!(),
        }
    }
}
