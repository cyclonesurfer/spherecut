//! BT.709 limited-range RGB ↔ YUV 4:2:0, 8- and 10-bit.
//!
//! The synthetic patterns are display-referred values, written straight to
//! Y'CbCr the way a camera would; no linearisation happens here.

use spherecut_media::encode::YuvPlanes;

use crate::{Rgb, RgbImage};

const KR: f64 = 0.2126;
const KB: f64 = 0.0722;
const KG: f64 = 1.0 - KR - KB;

fn rgb_to_ypbpr(c: Rgb) -> (f64, f64, f64) {
    let [r, g, b] = c.map(|v| v.clamp(0.0, 1.0));
    let y = KR * r + KG * g + KB * b;
    let pb = (b - y) / (2.0 * (1.0 - KB));
    let pr = (r - y) / (2.0 * (1.0 - KR));
    (y, pb, pr)
}

fn ypbpr_to_rgb(y: f64, pb: f64, pr: f64) -> Rgb {
    let r = y + 2.0 * (1.0 - KR) * pr;
    let b = y + 2.0 * (1.0 - KB) * pb;
    let g = (y - KR * r - KB * b) / KG;
    [r, g, b]
}

/// Quantise to limited range at `bits` (8 or 10).
fn quant(y: f64, pb: f64, pr: f64, bits: u32) -> (u16, u16, u16) {
    let s = (1u32 << (bits - 8)) as f64;
    let q = |v: f64, lo: f64, hi: f64| (v * s).round().clamp(lo * s, hi * s) as u16;
    (
        q(16.0 + 219.0 * y, 16.0, 235.0),
        q(128.0 + 224.0 * pb, 16.0, 240.0),
        q(128.0 + 224.0 * pr, 16.0, 240.0),
    )
}

/// Convert an image with even dimensions to planar 4:2:0. Chroma is the
/// average of each 2×2 block (centre-sited, which is close enough for
/// tests that compare at block scale).
pub fn to_yuv420(img: &RgbImage, bits: u32) -> YuvPlanes {
    let (w, h) = (img.width as usize, img.height as usize);
    let (cw, ch) = (w / 2, h / 2);
    let mut yp = vec![0u16; w * h];
    let mut up = vec![0u16; cw * ch];
    let mut vp = vec![0u16; cw * ch];
    for y in 0..h {
        for x in 0..w {
            let (yy, _, _) = rgb_to_ypbpr(img.pixels[y * w + x]);
            yp[y * w + x] = quant(yy, 0.0, 0.0, bits).0;
        }
    }
    for cy in 0..ch {
        for cx in 0..cw {
            let mut pb = 0.0;
            let mut pr = 0.0;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (_, b, r) = rgb_to_ypbpr(img.pixels[(2 * cy + dy) * w + 2 * cx + dx]);
                pb += b / 4.0;
                pr += r / 4.0;
            }
            let (_, u, v) = quant(0.0, pb, pr, bits);
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
    let (w, h) = (width as usize, height as usize);
    let cw = w / 2;
    let get = |p: usize, i: usize| -> f64 {
        match planes {
            YuvPlanes::U8(pl) => pl[p][i] as f64,
            YuvPlanes::U16(pl) => pl[p][i] as f64,
        }
    };
    let s = (1u32 << (bits - 8)) as f64;
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let c = (y / 2) * cw + x / 2;
            let yy = (get(0, y * w + x) / s - 16.0) / 219.0;
            let pb = (get(1, c) / s - 128.0) / 224.0;
            let pr = (get(2, c) / s - 128.0) / 224.0;
            pixels.push(ypbpr_to_rgb(yy, pb, pr).map(|v| v.clamp(0.0, 1.0)));
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
