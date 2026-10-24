//! A source's colour and geometry as the projection shader takes them
//! (M1 plan section 4.3): Y'CbCr to R'G'B' folded into one 3×4 matrix,
//! the chroma siting, and where the picture sits on the sphere.

use spherecut_core::video::{ChromaSiting, ColorInfo, Range, Transfer};

use crate::plane::{LayoutKind, SlotShape};

/// The 3×4 matrix that turns sampled values into R'G'B': row `i` dotted
/// with `(s0, s1, s2, 1)` is R', G' or B', where the samples are each
/// plane's code / (2^b − 1), which is what both an `R8Unorm` and a
/// canonical f16 texture hold, and `(s0, s1, s2)` is (Y', Cb, Cr) or, for
/// planar RGB, (G', B', R'). The range and the matrix's Kr and Kb are
/// folded in, by H.273's formulas:
/// - limited: Y' = (code − 16·2^(b−8)) / (219·2^(b−8)), Cb = (code −
///   128·2^(b−8)) / (224·2^(b−8));
/// - full: Y' = code / (2^b − 1), Cb = (code − 2^(b−1)) / (2^b − 1).
///
/// For BT.709 limited range it is synth's `from_yuv` exactly.
pub fn fold(color: &ColorInfo, kind: LayoutKind, bit_depth: u8) -> [[f64; 4]; 3] {
    let b = i32::from(bit_depth.clamp(8, 16));
    let max = f64::from((1u32 << b) - 1);
    // Y' = ay·s + by and Cb = ac·s + bc, with s = code / max.
    let (ay, by, ac, bc) = match color.range {
        Range::Limited => {
            let k = f64::from(1u32 << (b - 8));
            (
                max / (219.0 * k),
                -16.0 / 219.0,
                max / (224.0 * k),
                -128.0 / 224.0,
            )
        }
        Range::Full => (1.0, 0.0, 1.0, -f64::from(1u32 << (b - 1)) / max),
    };
    match (kind, color.matrix.kr_kb()) {
        (LayoutKind::Gray, _) => [[ay, 0.0, 0.0, by]; 3],
        // Planes G, B, R, each with the luma range.
        (LayoutKind::Gbr, _) | (_, None) => {
            [[0.0, 0.0, ay, by], [ay, 0.0, 0.0, by], [0.0, ay, 0.0, by]]
        }
        (_, Some((kr, kb))) => {
            let kg = 1.0 - kr - kb;
            let (rv, bu) = (2.0 * (1.0 - kr), 2.0 * (1.0 - kb));
            let (gv, gu) = (rv * kr / kg, bu * kb / kg);
            [
                [ay, 0.0, rv * ac, by + rv * bc],
                [ay, -gu * ac, -gv * ac, by - (gu + gv) * bc],
                [ay, bu * ac, 0.0, by + bu * bc],
            ]
        }
    }
}

/// Which part of a stereo frame is the eye shown. M1 shows the left eye
/// of a stereo source (with a notice), as Spherical Video V2's `st3d`
/// lays it out. Only that eye is uploaded ([`Placement::shown`]): the
/// slot holds the eye's own picture, so U wraps at its own ±180°, V
/// clamps at its own poles, and its mips never mix in the other eye.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stereo {
    #[default]
    Mono,
    /// The left eye on top.
    TopBottom,
    /// The left eye on the left.
    LeftRight,
}

/// Where a frame's picture sits on the sphere: Spherical Video V2's
/// equirect bounds (the fractions of the full sphere's width and height
/// cropped off each side, `sv3d`'s `equi` box) and the stereo layout.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Placement {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub stereo: Stereo,
}

impl Placement {
    /// The whole sphere, one eye.
    pub const FULL: Placement = Placement {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        stereo: Stereo::Mono,
    };

    /// The part of a `size` frame that is uploaded and shown, from its
    /// top-left corner: all of a mono frame, the top half of a
    /// top-bottom one and the left half of a left-right one (the left
    /// eye either way).
    pub fn shown(&self, (w, h): (u32, u32)) -> (u32, u32) {
        match self.stereo {
            Stereo::Mono => (w, h),
            Stereo::TopBottom => (w, (h / 2).max(1)),
            Stereo::LeftRight => ((w / 2).max(1), h),
        }
    }

    /// The sphere-uv rectangle the picture covers, `(u0, v0, u1, v1)`.
    pub fn cover(&self) -> [f64; 4] {
        [self.left, self.top, 1.0 - self.right, 1.0 - self.bottom]
    }

    /// Sphere uv to texture uv: `tex = suv · scale + offset`, the crop
    /// stretched over the picture the slot holds (the shown eye's).
    pub fn uv_transform(&self) -> ([f64; 2], [f64; 2]) {
        let [u0, v0, u1, v1] = self.cover();
        let (w, h) = ((u1 - u0).max(1e-9), (v1 - v0).max(1e-9));
        ([1.0 / w, 1.0 / h], [-u0 / w, -v0 / h])
    }

    /// Whether U may wrap at the texture's edges: only for a picture that
    /// covers all 360°, a stereo eye included, since the slot holds that
    /// eye alone. A crop clamps, so that its edge doesn't bleed in.
    pub fn wraps(&self) -> bool {
        self.left == 0.0 && self.right == 0.0
    }
}

/// The projection shader's `Source` uniform (`shaders/project.wgsl`; a
/// naga test checks every offset).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SourceUniform {
    pub m0: [f32; 4],
    pub m1: [f32; 4],
    pub m2: [f32; 4],
    pub cover: [f32; 4],
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
    pub chroma_scale: [f32; 2],
    pub chroma_offset: [f32; 2],
    pub size0: [f32; 2],
    pub size1: [f32; 2],
    pub levels: [f32; 2],
    pub kind: u32,
    pub transfer: u32,
}

/// Where the chroma texture is read for a point of the luma texture:
/// `chroma_uv = luma_uv · scale + offset`. A chroma sample sits at
/// `ChromaSiting::offset()` between the first and last luma samples of
/// its block, so for 2^s luma samples per chroma sample (the layout's
/// subsampling) and a luma plane `w` wide whose chroma plane is `wc`
/// wide, `scale = w / (2^s · wc)` (1 unless the size isn't a multiple of
/// 2^s) and `offset = (2^s − 1)(½ − t) / (2^s · wc)`: Left 4:2:0 reads
/// half a luma texel to the right, Center reads in place. A direction
/// that isn't subsampled has no offset.
pub fn chroma_transform(shape: &SlotShape, siting: ChromaSiting) -> ([f64; 2], [f64; 2]) {
    let Some(chroma) = shape.planes.get(1) else {
        return ([1.0, 1.0], [0.0, 0.0]);
    };
    let (tx, ty) = siting.offset();
    let axis = |luma: u32, c: u32, log2_sub: u8, t: f64| {
        let (luma, c) = (f64::from(luma), f64::from(c));
        let n = f64::from(1u32 << log2_sub.min(4));
        let scale = luma / (n * c);
        (scale, (n - 1.0) * (0.5 - t) / (n * c))
    };
    let (sx, ox) = axis(shape.size.0, chroma.size.0, chroma.log2_sub.0, tx);
    let (sy, oy) = axis(shape.size.1, chroma.size.1, chroma.log2_sub.1, ty);
    ([sx, sy], [ox, oy])
}

/// The `Source` uniform for a frame of `shape` and `color`, placed on the
/// sphere by `placement`.
pub fn source_uniform(
    shape: &SlotShape,
    color: &ColorInfo,
    placement: &Placement,
) -> SourceUniform {
    let m = fold(color, shape.kind, shape.bit_depth);
    let row = |r: [f64; 4]| r.map(|v| v as f32);
    let pair = |p: [f64; 2]| p.map(|v| v as f32);
    let (uv_scale, uv_offset) = placement.uv_transform();
    let subsampled = matches!(shape.kind, LayoutKind::SemiPlanar | LayoutKind::Planar);
    let (chroma_scale, chroma_offset) = if subsampled {
        chroma_transform(shape, color.siting)
    } else {
        ([1.0, 1.0], [0.0, 0.0])
    };
    let luma = shape.planes[0];
    let chroma = shape.planes.get(1).copied().unwrap_or(luma);
    let size = |p: crate::plane::PlaneShape| [p.size.0 as f32, p.size.1 as f32];
    SourceUniform {
        m0: row(m[0]),
        m1: row(m[1]),
        m2: row(m[2]),
        cover: placement.cover().map(|v| v as f32),
        uv_scale: pair(uv_scale),
        uv_offset: pair(uv_offset),
        chroma_scale: pair(chroma_scale),
        chroma_offset: pair(chroma_offset),
        size0: size(luma),
        size1: size(chroma),
        levels: [luma.levels as f32, chroma.levels as f32],
        kind: shape.layout_code(),
        transfer: match color.transfer {
            Transfer::Linear => 1,
            // M1 shows PQ and HLG as SDR, with a notice (M7 maps them).
            Transfer::Sdr | Transfer::Pq | Transfer::Hlg => 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spherecut_core::video::{Assumed, ColorCodes, Matrix, Primaries};

    fn color(range: Range, matrix: Matrix) -> ColorInfo {
        ColorInfo {
            codes: ColorCodes::UNSPECIFIED,
            range,
            matrix,
            transfer: Transfer::Sdr,
            primaries: Primaries::Bt709,
            siting: ChromaSiting::Left,
            assumed: Assumed::default(),
        }
    }

    fn apply(m: &[[f64; 4]; 3], codes: [f64; 3], bits: u8) -> [f64; 3] {
        let max = f64::from((1u32 << bits) - 1);
        let s = [codes[0] / max, codes[1] / max, codes[2] / max, 1.0];
        m.map(|r| (0..4).map(|i| r[i] * s[i]).sum())
    }

    #[test]
    fn limited_black_white_and_grey_land_on_their_levels() {
        for bits in [8u8, 10, 12, 16] {
            let k = f64::from(1u32 << (bits - 8));
            let m = fold(
                &color(Range::Limited, Matrix::Bt709),
                LayoutKind::Planar,
                bits,
            );
            for (code, want) in [(16.0, 0.0), (235.0, 1.0), (125.5, 0.5)] {
                let rgb = apply(&m, [code * k, 128.0 * k, 128.0 * k], bits);
                for c in rgb {
                    assert!((c - want).abs() < 1e-12, "{bits}: {rgb:?}");
                }
            }
        }
    }

    #[test]
    fn full_range_uses_every_code() {
        let m = fold(
            &color(Range::Full, Matrix::Bt601),
            LayoutKind::SemiPlanar,
            10,
        );
        let rgb = apply(&m, [1023.0, 512.0, 512.0], 10);
        assert!(rgb.iter().all(|c| (c - 1.0).abs() < 1e-12), "{rgb:?}");
        let rgb = apply(&m, [0.0, 512.0, 512.0], 10);
        assert!(rgb.iter().all(|c| c.abs() < 1e-12), "{rgb:?}");
    }

    #[test]
    fn matrices_invert_their_own_encoding() {
        // Encode pure colours by each matrix's definition, then decode.
        for matrix in [
            Matrix::Bt709,
            Matrix::Bt601,
            Matrix::Bt2020Ncl,
            Matrix::Smpte240m,
            Matrix::Fcc,
        ] {
            let (kr, kb) = matrix.kr_kb().unwrap_or((0.0, 0.0));
            let m = fold(&color(Range::Full, matrix), LayoutKind::Planar, 16);
            for rgb in [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.3, 0.6, 0.9],
            ] {
                let y = kr * rgb[0] + (1.0 - kr - kb) * rgb[1] + kb * rgb[2];
                let cb = (rgb[2] - y) / (2.0 * (1.0 - kb));
                let cr = (rgb[0] - y) / (2.0 * (1.0 - kr));
                let max = 65535.0;
                let mid = 32768.0;
                let got = apply(&m, [y * max, mid + cb * max, mid + cr * max], 16);
                for c in 0..3 {
                    assert!(
                        (got[c] - rgb[c]).abs() < 1e-9,
                        "{matrix:?} {rgb:?}: {got:?}"
                    );
                }
            }
        }
    }

    proptest::proptest! {
        /// Any colour, coded by H.273's own formulas at any depth and in
        /// either range, decodes to itself.
        #[test]
        fn the_fold_inverts_h273(
            r in 0.0..=1.0f64, g in 0.0..=1.0f64, b in 0.0..=1.0f64,
            bits in 8u8..=16, full in proptest::bool::ANY,
            m in proptest::sample::select(vec![Matrix::Bt709, Matrix::Bt601, Matrix::Bt2020Ncl, Matrix::Smpte240m, Matrix::Fcc]),
        ) {
            let (kr, kb) = m.kr_kb().unwrap_or((0.0, 0.0));
            let y = kr * r + (1.0 - kr - kb) * g + kb * b;
            let (pb, pr) = ((b - y) / (2.0 * (1.0 - kb)), (r - y) / (2.0 * (1.0 - kr)));
            let k = f64::from(1u32 << (bits - 8));
            let max = f64::from((1u32 << bits) - 1);
            let codes = if full {
                let mid = f64::from(1u32 << (bits - 1));
                [y * max, mid + pb * max, mid + pr * max]
            } else {
                [(16.0 + 219.0 * y) * k, (128.0 + 224.0 * pb) * k, (128.0 + 224.0 * pr) * k]
            };
            let range = if full { Range::Full } else { Range::Limited };
            let got = apply(&fold(&color(range, m), LayoutKind::Planar, bits), codes, bits);
            for (c, want) in got.iter().zip([r, g, b]) {
                proptest::prop_assert!((c - want).abs() < 1e-9, "{got:?} for {:?}", [r, g, b]);
            }
        }
    }

    #[test]
    fn gbr_and_gray_ignore_chroma() {
        let m = fold(&color(Range::Full, Matrix::Identity), LayoutKind::Gbr, 8);
        assert_eq!(apply(&m, [255.0, 0.0, 51.0], 8), [0.2, 1.0, 0.0]);
        let m = fold(&color(Range::Limited, Matrix::Bt709), LayoutKind::Gray, 8);
        let rgb = apply(&m, [235.0, 0.0, 255.0], 8);
        assert!(rgb.iter().all(|c| (c - 1.0).abs() < 1e-12), "{rgb:?}");
    }

    fn shape(name: &str, size: (u32, u32), sub: (u8, u8)) -> SlotShape {
        use spherecut_core::video::{ColorFamily, PixelLayout, PlaneDesc, SampleBytes};
        let plane = |log2_sub| PlaneDesc {
            components: 1,
            bytes: SampleBytes::One,
            log2_sub,
        };
        let layout = PixelLayout {
            name: name.into(),
            planes: vec![plane((0, 0)), plane(sub), plane(sub)],
            family: ColorFamily::Yuv,
            bit_depth: 8,
            msb_aligned: false,
        };
        SlotShape::of(&layout, size).unwrap()
    }

    #[test]
    fn chroma_sits_where_its_siting_says() {
        // Values worked out by hand from the definition: chroma sample j
        // of a 2^s block sits at luma position 2^s·j + (2^s − 1)·t, in
        // samples, so luma uv (that + ½) / w reads chroma uv (j + ½) / wc.
        let check = |s: &SlotShape, siting, want: ([f64; 2], [f64; 2])| {
            let got = chroma_transform(s, siting);
            for a in 0..2 {
                assert!(
                    (got.0[a] - want.0[a]).abs() < 1e-15 && (got.1[a] - want.1[a]).abs() < 1e-15,
                    "{} {siting:?}: {got:?}, not {want:?}",
                    s.describe()
                );
            }
        };
        let i420 = shape("yuv420p", (64, 32), (1, 1));
        // Left: half a luma texel right (¼ of a chroma texel), centred
        // down. Center: in place. TopLeft: half a texel right and down.
        check(&i420, ChromaSiting::Left, ([1.0, 1.0], [0.25 / 32.0, 0.0]));
        check(&i420, ChromaSiting::Center, ([1.0, 1.0], [0.0, 0.0]));
        check(
            &i420,
            ChromaSiting::TopLeft,
            ([1.0, 1.0], [0.25 / 32.0, 0.25 / 16.0]),
        );
        check(
            &i420,
            ChromaSiting::BottomLeft,
            ([1.0, 1.0], [0.25 / 32.0, -0.25 / 16.0]),
        );
        // 4:2:2 isn't subsampled down, so only Left's offset across.
        let i422 = shape("yuv422p", (64, 32), (1, 0));
        check(
            &i422,
            ChromaSiting::TopLeft,
            ([1.0, 1.0], [0.25 / 32.0, 0.0]),
        );
        // An odd width: 5 luma samples, 3 chroma ones covering 6.
        let odd = shape("yuv420p", (5, 4), (1, 1));
        check(
            &odd,
            ChromaSiting::Left,
            ([5.0 / 6.0, 1.0], [0.5 / 6.0, 0.0]),
        );
        // 4:1:1 at a width that isn't a multiple of 4: 6 luma samples, 2
        // chroma ones of 4 each. The factor is the layout's 4, not 6 / 2.
        let i411 = shape("yuv411p", (6, 2), (2, 0));
        assert_eq!(i411.planes[1].size, (2, 2));
        check(
            &i411,
            ChromaSiting::Left,
            ([0.75, 1.0], [3.0 * 0.5 / 8.0, 0.0]),
        );
        check(&i411, ChromaSiting::Center, ([0.75, 1.0], [0.0, 0.0]));
    }

    #[test]
    fn placement_maps_the_crop_and_the_eye() {
        let half = Placement {
            left: 0.25,
            right: 0.25,
            ..Placement::FULL
        };
        let (s, o) = half.uv_transform();
        assert_eq!((s[0] * 0.25 + o[0], s[0] * 0.75 + o[0]), (0.0, 1.0));
        assert!(!half.wraps());
        assert!(Placement::FULL.wraps());
        assert_eq!(Placement::FULL.shown((512, 256)), (512, 256));
        // A stereo eye is uploaded alone, so it fills the texture and its
        // U wraps.
        for (stereo, shown) in [
            (Stereo::TopBottom, (512, 128)),
            (Stereo::LeftRight, (256, 256)),
        ] {
            let eye = Placement {
                stereo,
                ..Placement::FULL
            };
            assert_eq!(eye.shown((512, 256)), shown);
            assert_eq!(eye.uv_transform(), ([1.0, 1.0], [0.0, 0.0]));
            assert!(eye.wraps());
        }
    }
}
