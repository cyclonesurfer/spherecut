//! The vocabulary of decoded pictures that every crate shares (M1 plan,
//! section 3.1): how a picture's planes are laid out, a trait that hands
//! them out without copying them, and the colour description a picture
//! is shown with.
//!
//! `spherecut-media` fills these in from FFmpeg (DECISIONS D30), the
//! renderer uploads planes through [`VideoPlanes`] without knowing where
//! they came from, and synthetic sources implement it too, so render tests
//! and M3's stitcher need no FFmpeg.

use serde::{Deserialize, Serialize};

/// Bytes per sample in a plane: one up to 8 bits, two for 9 to 16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleBytes {
    One,
    Two,
}

impl SampleBytes {
    pub fn get(self) -> usize {
        match self {
            SampleBytes::One => 1,
            SampleBytes::Two => 2,
        }
    }
}

/// One plane of a [`PixelLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaneDesc {
    /// Samples at each position: 1 (Y, U, V, G, B, R or gray) or 2 (U
    /// and V interleaved, as in the second plane of NV12 and P010).
    pub components: u8,
    pub bytes: SampleBytes,
    /// log2 of the plane's horizontal and vertical subsampling against
    /// the first plane: (1, 1) for 4:2:0 chroma, (1, 0) for 4:2:2 and
    /// (0, 0) at full resolution.
    pub log2_sub: (u8, u8),
}

impl PlaneDesc {
    /// The plane's size in sample positions for a `width` × `height`
    /// picture, rounding up as FFmpeg does: a 5-pixel-wide 4:2:0 picture
    /// has 3 chroma columns.
    pub fn size(&self, width: u32, height: u32) -> (u32, u32) {
        let (sx, sy) = (u32::from(self.log2_sub.0), u32::from(self.log2_sub.1));
        (
            width.div_ceil(1 << sx.min(31)),
            height.div_ceil(1 << sy.min(31)),
        )
    }

    /// Bytes that one row of `width` sample positions holds.
    pub fn row_bytes(&self, width: u32) -> usize {
        width as usize * usize::from(self.components) * self.bytes.get()
    }
}

/// What a picture's planes hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorFamily {
    /// Y, then U and V, or Y then interleaved UV.
    Yuv,
    /// Planar RGB in FFmpeg's order: G, B, R (`gbrp`). Its matrix is
    /// [`Matrix::Identity`].
    Rgb,
    /// Luma only.
    Gray,
}

/// How a decoded picture's samples are laid out in memory: planar or
/// semi-planar, 8 to 16 bits, little-endian. Alpha is never included; a
/// format with alpha is described without its alpha plane.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PixelLayout {
    /// FFmpeg's name for the format, such as `yuv420p10le` or `nv12`, for
    /// display only.
    pub name: String,
    /// 1 to 3 planes, in the format's own order ([`ColorFamily`]).
    pub planes: Vec<PlaneDesc>,
    pub family: ColorFamily,
    /// Significant bits per sample, 8 to 16.
    pub bit_depth: u8,
    /// The bits are the high ones of each 16-bit sample, as in P010 and
    /// P016, rather than the low ones, as in `yuv420p10le`.
    pub msb_aligned: bool,
}

impl PixelLayout {
    /// The chroma planes' subsampling, (0, 0) for one without chroma.
    pub fn chroma_sub(&self) -> (u8, u8) {
        match self.family {
            ColorFamily::Yuv => self.planes.get(1).map_or((0, 0), |p| p.log2_sub),
            ColorFamily::Rgb | ColorFamily::Gray => (0, 0),
        }
    }

    /// Whether the chroma samples share one plane, as in NV12.
    pub fn semi_planar(&self) -> bool {
        self.planes.iter().any(|p| p.components == 2)
    }
}

/// One plane of a picture, borrowed from wherever the picture lives.
///
/// Row `y` starts `y * stride` bytes into `bytes` and holds
/// [`PlaneDesc::row_bytes`]`(width)` bytes, all inside `bytes`. The last
/// row may have no padding after it, so `bytes` can be shorter than
/// `stride * height`.
#[derive(Debug, Clone, Copy)]
pub struct PlaneData<'a> {
    pub bytes: &'a [u8],
    pub stride: usize,
    /// In sample positions: for interleaved chroma, pairs of samples.
    pub width: u32,
    pub height: u32,
}

impl<'a> PlaneData<'a> {
    /// The first `row_bytes` bytes of row `y`, or `None` when they don't
    /// lie inside the plane.
    pub fn row(&self, y: u32, row_bytes: usize) -> Option<&'a [u8]> {
        if y >= self.height {
            return None;
        }
        let start = (y as usize).checked_mul(self.stride)?;
        self.bytes.get(start..start.checked_add(row_bytes)?)
    }
}

/// A decoded picture whose planes can be uploaded without being copied
/// first. Implementations are immutable once made, so one picture can be
/// shared between threads (the frame cache and the upload thread).
pub trait VideoPlanes: Send + Sync + std::fmt::Debug {
    fn layout(&self) -> &PixelLayout;
    /// The picture's size in pixels, after cropping.
    fn size(&self) -> (u32, u32);
    /// Plane `index`, `None` past the last one.
    fn plane(&self, index: usize) -> Option<PlaneData<'_>>;
    fn color(&self) -> &ColorInfo;
}

/// The colour fields as the file codes them, as FFmpeg's ints. FFmpeg's
/// primaries, transfer and matrix values are the code points of ITU-T
/// H.273 (2 is unspecified). `range` is FFmpeg's `AVColorRange` (0
/// unspecified, 1 limited, 2 full), and `chroma_location` its
/// `AVChromaLocation` (0 unspecified, 1 left, 2 center, 3 top left, 4
/// top, 5 bottom left, 6 bottom), which is H.273's
/// `chroma_sample_loc_type` plus 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorCodes {
    pub primaries: i32,
    pub transfer: i32,
    pub matrix: i32,
    pub range: i32,
    pub chroma_location: i32,
}

impl ColorCodes {
    /// A file that says nothing about its colour.
    pub const UNSPECIFIED: ColorCodes = ColorCodes {
        primaries: 2,
        transfer: 2,
        matrix: 2,
        range: 0,
        chroma_location: 0,
    };
}

/// The range of the coded values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    /// 16 to 235 for luma and 16 to 240 for chroma at 8 bits, scaled up
    /// for deeper samples ("tv", "mpeg").
    Limited,
    /// Every code ("pc", "jpeg").
    Full,
}

/// The matrix from RGB to Y'CbCr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Matrix {
    Bt709,
    /// BT.601, whether coded as BT.470 BG or SMPTE 170M, which are the
    /// same matrix.
    Bt601,
    Bt2020Ncl,
    Smpte240m,
    Fcc,
    /// No matrix: the planes are G, B and R.
    Identity,
}

impl Matrix {
    /// The luma weights of red and blue, Kr and Kb (green's is 1 − Kr −
    /// Kb), or `None` for [`Matrix::Identity`].
    pub fn kr_kb(self) -> Option<(f64, f64)> {
        match self {
            Matrix::Bt709 => Some((0.2126, 0.0722)),
            Matrix::Bt601 => Some((0.299, 0.114)),
            Matrix::Bt2020Ncl => Some((0.2627, 0.0593)),
            Matrix::Smpte240m => Some((0.212, 0.087)),
            Matrix::Fcc => Some((0.30, 0.11)),
            Matrix::Identity => None,
        }
    }
}

/// The transfer function, as far as the renderer tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    /// A gamma-like SDR curve (BT.709, BT.601, sRGB, BT.2020 SDR), all
    /// shown with the sRGB curve in M1.
    Sdr,
    /// SMPTE ST 2084, HDR10.
    Pq,
    /// ARIB STD-B67, hybrid log-gamma.
    Hlg,
    Linear,
}

impl Transfer {
    pub fn is_hdr(self) -> bool {
        matches!(self, Transfer::Pq | Transfer::Hlg)
    }
}

/// The colour primaries, as far as the renderer tells them apart. The
/// code itself is in [`ColorCodes::primaries`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primaries {
    Bt709,
    Bt2020,
    /// Any other primaries, shown as BT.709 until M7's colour pipeline.
    Other,
}

/// Where a chroma sample sits against the luma samples it covers
/// (H.273's chroma sample location, FFmpeg's `AVChromaLocation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromaSiting {
    /// With the left column, midway between the rows: MPEG-2, H.264,
    /// HEVC, AV1 and ProRes 4:2:0.
    Left,
    /// Midway both ways: JPEG and MPEG-1.
    Center,
    /// With the top-left sample: BT.2020's 4:2:0, common in HDR files.
    TopLeft,
    Top,
    BottomLeft,
    Bottom,
}

impl ChromaSiting {
    /// Where the chroma sample sits between the first and the last luma
    /// sample of its block, horizontally and vertically: 0 on the first,
    /// 1 on the last, ½ midway. A direction that isn't subsampled has
    /// one luma sample per block, so the offset doesn't apply to it.
    pub fn offset(self) -> (f64, f64) {
        match self {
            ChromaSiting::Left => (0.0, 0.5),
            ChromaSiting::Center => (0.5, 0.5),
            ChromaSiting::TopLeft => (0.0, 0.0),
            ChromaSiting::Top => (0.5, 0.0),
            ChromaSiting::BottomLeft => (0.0, 1.0),
            ChromaSiting::Bottom => (0.5, 1.0),
        }
    }
}

/// Which fields of a [`ColorInfo`] a default rule chose, because the
/// file left them unspecified or coded something Spherecut doesn't
/// render. The Info pane and the JSON show them, so that a person or an
/// agent can see a guess for what it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Assumed {
    pub range: bool,
    pub matrix: bool,
    pub transfer: bool,
    pub primaries: bool,
    pub siting: bool,
}

impl Assumed {
    pub fn any(&self) -> bool {
        self.range || self.matrix || self.transfer || self.primaries || self.siting
    }
}

/// The colour a picture is shown with: what the file codes, and what the
/// renderer uses, which a default rule fills in where the file says
/// nothing (`media::color::resolve`, DECISIONS D30).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ColorInfo {
    pub codes: ColorCodes,
    pub range: Range,
    pub matrix: Matrix,
    pub transfer: Transfer,
    pub primaries: Primaries,
    pub siting: ChromaSiting,
    pub assumed: Assumed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nv12() -> PixelLayout {
        PixelLayout {
            name: "nv12".into(),
            planes: vec![
                PlaneDesc {
                    components: 1,
                    bytes: SampleBytes::One,
                    log2_sub: (0, 0),
                },
                PlaneDesc {
                    components: 2,
                    bytes: SampleBytes::One,
                    log2_sub: (1, 1),
                },
            ],
            family: ColorFamily::Yuv,
            bit_depth: 8,
            msb_aligned: false,
        }
    }

    #[test]
    fn planes_are_sized_as_ffmpeg_rounds() {
        let l = nv12();
        assert_eq!(l.planes[0].size(5, 3), (5, 3));
        assert_eq!(l.planes[1].size(5, 3), (3, 2));
        assert_eq!(l.planes[1].row_bytes(3), 6);
        assert_eq!(l.chroma_sub(), (1, 1));
        assert!(l.semi_planar());
        let p010 = PlaneDesc {
            bytes: SampleBytes::Two,
            ..l.planes[1]
        };
        assert_eq!(p010.row_bytes(3), 12);
    }

    #[test]
    fn rows_stay_inside_the_plane() {
        // Two rows of 3 bytes, 4 apart, with no padding after the last.
        let bytes = [1, 2, 3, 0, 4, 5, 6];
        let p = PlaneData {
            bytes: &bytes,
            stride: 4,
            width: 3,
            height: 2,
        };
        assert_eq!(p.row(0, 3), Some(&[1, 2, 3][..]));
        assert_eq!(p.row(1, 3), Some(&[4, 5, 6][..]));
        assert_eq!(p.row(1, 4), None);
        assert_eq!(p.row(2, 3), None);
    }

    #[test]
    fn matrices_have_their_luma_weights() {
        assert_eq!(Matrix::Bt709.kr_kb(), Some((0.2126, 0.0722)));
        assert_eq!(Matrix::Bt601.kr_kb(), Some((0.299, 0.114)));
        assert_eq!(Matrix::Bt2020Ncl.kr_kb(), Some((0.2627, 0.0593)));
        assert_eq!(Matrix::Identity.kr_kb(), None);
        assert!(Transfer::Pq.is_hdr() && Transfer::Hlg.is_hdr() && !Transfer::Sdr.is_hdr());
        assert_eq!(ChromaSiting::Left.offset(), (0.0, 0.5));
        assert_eq!(ChromaSiting::Center.offset(), (0.5, 0.5));
    }

    #[test]
    fn color_info_reads_back_from_its_json() {
        let c = ColorInfo {
            codes: ColorCodes {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                range: 1,
                chroma_location: 3,
            },
            range: Range::Limited,
            matrix: Matrix::Bt2020Ncl,
            transfer: Transfer::Pq,
            primaries: Primaries::Bt2020,
            siting: ChromaSiting::TopLeft,
            assumed: Assumed {
                siting: true,
                ..Assumed::default()
            },
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(
            json,
            r#"{"codes":{"primaries":9,"transfer":16,"matrix":9,"range":1,"chroma_location":3},"range":"limited","matrix":"bt2020_ncl","transfer":"pq","primaries":"bt2020","siting":"top_left","assumed":{"range":false,"matrix":false,"transfer":false,"primaries":false,"siting":true}}"#
        );
        assert_eq!(serde_json::from_str::<ColorInfo>(&json).unwrap(), c);
        // And through ron, the project files' format.
        let ron = ron::to_string(&c).unwrap();
        assert_eq!(ron::from_str::<ColorInfo>(&ron).unwrap(), c);
        assert!(c.assumed.any());
        assert!(!Assumed::default().any());
    }
}
