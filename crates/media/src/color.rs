//! The colour a decoded picture is shown with (DECISIONS D30, M1 plan
//! section 3.5): what the file codes, read as FFmpeg's ints, and a default
//! for each field it leaves unspecified or codes in a way Spherecut
//! doesn't render. Each default sets its flag in [`Assumed`], so the Info
//! pane and the JSON can show a guess for what it is.

use spherecut_core::video::{
    Assumed, ChromaSiting, ColorCodes, ColorFamily, ColorInfo, Matrix, Primaries, Range, Transfer,
};

/// What the default rules look at besides the codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorContext<'a> {
    /// The family of the decoded pixel format.
    pub family: ColorFamily,
    /// Its chroma subsampling ([`PixelLayout::chroma_sub`]): chroma
    /// siting means something only where it isn't (0, 0).
    ///
    /// [`PixelLayout::chroma_sub`]: spherecut_core::video::PixelLayout::chroma_sub
    pub chroma_sub: (u8, u8),
    /// FFmpeg's name for the decoded pixel format, before any conversion:
    /// a `yuvj` format means full range.
    pub pix_fmt: &'a str,
    /// FFmpeg's codec name, such as `h264` or `mjpeg`.
    pub codec: &'a str,
    /// The picture's size, which picks the matrix of an untagged file.
    pub width: u32,
    pub height: u32,
}

/// Resolve `codes` with the defaults of section 3.5:
///
/// | Field | Unspecified or not rendered |
/// |---|---|
/// | range | full for a `yuvj` format and for RGB, else limited |
/// | matrix | BT.709 at width ≥ 1280 or height > 576, else BT.601 (mpv's and libplacebo's rule; swscale assumes BT.601 and libavcodec doesn't guess); identity for RGB |
/// | transfer | SDR |
/// | primaries | BT.709 |
/// | siting | centre for MJPEG, else left (H.264, HEVC, AV1, ProRes); not flagged where chroma isn't subsampled (4:4:4, RGB, gray), since none matters there |
///
/// Codes Spherecut doesn't render are assumed too: the YCgCo,
/// constant-luminance and ICtCp matrices take the size rule, and transfers
/// other than SDR's, PQ, HLG and linear are shown as SDR. Primaries other
/// than BT.709 and BT.2020 are [`Primaries::Other`], kept as coded, and
/// shown as BT.709 until M7.
pub fn resolve(codes: ColorCodes, cx: &ColorContext<'_>) -> ColorInfo {
    let mut assumed = Assumed::default();
    let range = match codes.range {
        1 => Range::Limited,
        2 => Range::Full,
        _ => {
            assumed.range = true;
            if cx.pix_fmt.starts_with("yuvj") || cx.family == ColorFamily::Rgb {
                Range::Full
            } else {
                Range::Limited
            }
        }
    };
    let by_size = || {
        if cx.width >= 1280 || cx.height > 576 {
            Matrix::Bt709
        } else {
            Matrix::Bt601
        }
    };
    let coded_matrix = match codes.matrix {
        0 => Some(Matrix::Identity),
        1 => Some(Matrix::Bt709),
        4 => Some(Matrix::Fcc),
        5 | 6 => Some(Matrix::Bt601),
        7 => Some(Matrix::Smpte240m),
        9 => Some(Matrix::Bt2020Ncl),
        _ => None,
    };
    let matrix = match (cx.family, coded_matrix) {
        // RGB has no matrix, whatever the file says.
        (ColorFamily::Rgb, Some(Matrix::Identity)) => Matrix::Identity,
        (ColorFamily::Rgb, _) => {
            assumed.matrix = codes.matrix != 0;
            Matrix::Identity
        }
        (_, Some(m)) => m,
        (_, None) => {
            assumed.matrix = true;
            by_size()
        }
    };
    let transfer = match codes.transfer {
        // BT.709, BT.470 M and BG, SMPTE 170M and 240M, sRGB, BT.2020 10
        // and 12-bit: all SDR here.
        1 | 4 | 5 | 6 | 7 | 13 | 14 | 15 => Transfer::Sdr,
        8 => Transfer::Linear,
        16 => Transfer::Pq,
        18 => Transfer::Hlg,
        _ => {
            assumed.transfer = true;
            Transfer::Sdr
        }
    };
    let primaries = match codes.primaries {
        1 => Primaries::Bt709,
        9 => Primaries::Bt2020,
        // Unspecified and reserved.
        0 | 2 | 3 => {
            assumed.primaries = true;
            Primaries::Bt709
        }
        _ => Primaries::Other,
    };
    let siting = match codes.chroma_location {
        1 => ChromaSiting::Left,
        2 => ChromaSiting::Center,
        3 => ChromaSiting::TopLeft,
        4 => ChromaSiting::Top,
        5 => ChromaSiting::BottomLeft,
        6 => ChromaSiting::Bottom,
        _ => {
            assumed.siting = cx.chroma_sub != (0, 0);
            // By codec, not by the yuvj range flag.
            if cx.codec == "mjpeg" {
                ChromaSiting::Center
            } else {
                ChromaSiting::Left
            }
        }
    };
    ColorInfo {
        codes,
        range,
        matrix,
        transfer,
        primaries,
        siting,
        assumed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(
        family: ColorFamily,
        pix_fmt: &'static str,
        codec: &'static str,
        w: u32,
        h: u32,
    ) -> ColorContext<'static> {
        // The subsampling of the formats these tests name.
        let chroma_sub = match family {
            ColorFamily::Yuv if pix_fmt.contains("444") => (0, 0),
            ColorFamily::Yuv if pix_fmt.contains("422") => (1, 0),
            ColorFamily::Yuv => (1, 1),
            ColorFamily::Rgb | ColorFamily::Gray => (0, 0),
        };
        ColorContext {
            family,
            chroma_sub,
            pix_fmt,
            codec,
            width: w,
            height: h,
        }
    }

    const UNTAGGED: ColorCodes = ColorCodes::UNSPECIFIED;

    #[test]
    fn untagged_fields_follow_the_table_and_say_so() {
        let hd = resolve(
            UNTAGGED,
            &cx(ColorFamily::Yuv, "yuv420p", "h264", 1920, 1080),
        );
        assert_eq!(
            (hd.range, hd.matrix, hd.transfer, hd.primaries, hd.siting),
            (
                Range::Limited,
                Matrix::Bt709,
                Transfer::Sdr,
                Primaries::Bt709,
                ChromaSiting::Left
            )
        );
        assert_eq!(
            hd.assumed,
            Assumed {
                range: true,
                matrix: true,
                transfer: true,
                primaries: true,
                siting: true
            }
        );
        // The size rule: width ≥ 1280 or height > 576 is BT.709.
        let m = |w, h| resolve(UNTAGGED, &cx(ColorFamily::Yuv, "yuv420p", "h264", w, h)).matrix;
        assert_eq!(m(1280, 720), Matrix::Bt709);
        assert_eq!(m(1279, 576), Matrix::Bt601);
        assert_eq!(m(720, 577), Matrix::Bt709);
        assert_eq!(m(720, 576), Matrix::Bt601);
        // yuvj means full range; MJPEG is centre-sited, by codec.
        let j = resolve(
            UNTAGGED,
            &cx(ColorFamily::Yuv, "yuvj420p", "mjpeg", 640, 480),
        );
        assert_eq!((j.range, j.siting), (Range::Full, ChromaSiting::Center));
        let j264 = resolve(
            UNTAGGED,
            &cx(ColorFamily::Yuv, "yuvj420p", "h264", 640, 480),
        );
        assert_eq!((j264.range, j264.siting), (Range::Full, ChromaSiting::Left));
        // RGB is full range with no matrix.
        let rgb = resolve(UNTAGGED, &cx(ColorFamily::Rgb, "gbrp", "png", 640, 480));
        assert_eq!((rgb.range, rgb.matrix), (Range::Full, Matrix::Identity));
        assert!(rgb.assumed.range && rgb.assumed.matrix);
    }

    #[test]
    fn siting_is_assumed_only_where_chroma_is_subsampled() {
        let assumed = |family, pix_fmt| {
            resolve(UNTAGGED, &cx(family, pix_fmt, "hevc", 640, 480))
                .assumed
                .siting
        };
        assert!(assumed(ColorFamily::Yuv, "yuv420p"));
        assert!(assumed(ColorFamily::Yuv, "yuv422p10le"));
        assert!(!assumed(ColorFamily::Yuv, "yuv444p"));
        assert!(!assumed(ColorFamily::Rgb, "gbrp"));
        assert!(!assumed(ColorFamily::Gray, "gray"));
    }

    #[test]
    fn coded_fields_are_taken_as_coded() {
        let hdr = ColorCodes {
            primaries: 9,
            transfer: 16,
            matrix: 9,
            range: 2,
            chroma_location: 3,
        };
        let c = resolve(hdr, &cx(ColorFamily::Yuv, "yuv420p10le", "hevc", 640, 360));
        assert_eq!(
            (c.range, c.matrix, c.transfer, c.primaries, c.siting),
            (
                Range::Full,
                Matrix::Bt2020Ncl,
                Transfer::Pq,
                Primaries::Bt2020,
                ChromaSiting::TopLeft
            )
        );
        assert!(!c.assumed.any());
        assert_eq!(c.codes, hdr);
        let bt601 = ColorCodes {
            primaries: 6,
            transfer: 6,
            matrix: 6,
            range: 1,
            chroma_location: 1,
        };
        let c = resolve(bt601, &cx(ColorFamily::Yuv, "yuv420p", "h264", 1920, 1080));
        assert_eq!(c.matrix, Matrix::Bt601);
        assert_eq!(c.transfer, Transfer::Sdr);
        // SMPTE 170M primaries are kept as coded, not BT.709.
        assert_eq!(c.primaries, Primaries::Other);
        assert!(!c.assumed.any());
        let hlg = ColorCodes {
            transfer: 18,
            ..UNTAGGED
        };
        assert_eq!(
            resolve(hlg, &cx(ColorFamily::Yuv, "yuv420p", "hevc", 64, 64)).transfer,
            Transfer::Hlg
        );
        // An identity matrix on RGB is as coded.
        let gbr = ColorCodes {
            matrix: 0,
            range: 2,
            ..UNTAGGED
        };
        let c = resolve(gbr, &cx(ColorFamily::Rgb, "gbrp", "h264", 64, 64));
        assert_eq!(c.matrix, Matrix::Identity);
        assert!(!c.assumed.matrix && !c.assumed.range);
    }

    #[test]
    fn codes_not_rendered_take_the_defaults_and_say_so() {
        let odd = ColorCodes {
            primaries: 22,
            transfer: 11,
            matrix: 8,
            range: 0,
            chroma_location: 0,
        };
        let c = resolve(odd, &cx(ColorFamily::Yuv, "yuv420p", "h264", 640, 480));
        // YCgCo takes the size rule; IEC 61966-2-4 is shown as SDR.
        assert_eq!((c.matrix, c.transfer), (Matrix::Bt601, Transfer::Sdr));
        assert!(c.assumed.matrix && c.assumed.transfer);
        // EBU 3213 primaries are other primaries, coded, not assumed.
        assert_eq!(c.primaries, Primaries::Other);
        assert!(!c.assumed.primaries);
        // A YUV matrix tagged on RGB planes is ignored, and says so.
        let c = resolve(
            ColorCodes {
                matrix: 1,
                ..UNTAGGED
            },
            &cx(ColorFamily::Rgb, "gbrp", "h264", 64, 64),
        );
        assert_eq!(c.matrix, Matrix::Identity);
        assert!(c.assumed.matrix);
    }
}
