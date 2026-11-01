//! Frame codes in the corners of a lens image (M3 plan 7.1), so that a
//! pair can be checked at the pixel: each frame of each lens says which
//! frame of which lens it is, wherever it has been decoded, uploaded or
//! read back.
//!
//! A lens's image circle leaves the corners of its square free. Each
//! corner holds a block of side b = s/12 (5 px at 64², 32 px at 384², 240
//! px at 2880² and 320 px at 3840²), split into 5 × 5 cells, each black
//! or white: 25 bits, of which 16 are the frame number's low bits, one is
//! the lens index and 8 are a checksum (CRC-8, polynomial x⁸ + x² + x +
//! 1, over the other 17, then XORed with a constant so that a block all
//! black or all white is no code). All four corners hold the same code.
//! A corner is read from the luma of each cell's middle, so that a lossy
//! encode's ringing at the cells' edges doesn't count, and the picture's
//! code is the one every corner whose checksum holds agrees on.
//!
//! A block must stay clear of the image circle, or it would cover what
//! the lens shows: [`check_clear`] holds a rendered lens to 7.1's bound,
//! b ≤ s/2 − |centre offset| − (r(θ_max) + 2)/√2. It is for M3.3's
//! `render_lens`, which asserts it for every truth it renders; the perf
//! clips, whose picture is noise, have no circle to keep clear of.

use spherecut_core::video::{SampleBytes, VideoPlanes};
use thiserror::Error;

use crate::RgbImage;
use crate::yuv::YuvPlanes;

/// Cells along each side of a block.
pub const GRID: u32 = 5;

/// Bits of frame number a code holds: frames count modulo 2¹⁶.
pub const FRAME_BITS: u32 = 16;

/// The lenses a code tells apart: indices 0 and 1.
pub const LENSES: usize = 2;

/// What a corner code says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CornerCode {
    /// The lens, 0 or 1.
    pub lens: usize,
    /// The frame number's low [`FRAME_BITS`] bits.
    pub frame: u16,
}

/// What one corner's block reads as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    /// Every cell read, and the checksum holds.
    Code(CornerCode),
    /// Every cell read, and the checksum doesn't hold: the 25 bits as
    /// read, cell 0 (top left) in bit 0.
    Checksum { word: u32 },
    /// A cell's luma lies too near the middle of the range to read as
    /// black or white: the first such cell, counted row by row from the
    /// top left.
    Unclear { cell: u32 },
}

/// Why a picture's corner code couldn't be written or read.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum CodeError {
    #[error("a {width}x{height} picture isn't square, so it has no corner code")]
    NotSquare { width: u32, height: u32 },
    #[error("a picture {side} px square has blocks of {} px, fewer than its {GRID} cells", block_side(*side))]
    TooSmall { side: u32 },
    #[error("lens {0}: a corner code tells only lenses 0 and 1 apart")]
    Lens(usize),
    #[error("no corner holds a code whose checksum holds: {0:?}")]
    NoCode([Corner; 4]),
    #[error("the corners hold different codes: {0:?}")]
    Disagree([Corner; 4]),
    #[error("the luma plane: {0}")]
    Plane(String),
    #[error(
        "a block of {block} px at {side}² reaches the image circle: with its centre {offset} px \
         off and a radius of {radius} px, a block may be {bound:.2} px at most"
    )]
    ReachesCircle {
        side: u32,
        block: u32,
        offset: f64,
        radius: f64,
        bound: f64,
    },
}

/// The side of each corner's block in a picture `side` px square: s/12,
/// rounded down.
pub fn block_side(side: u32) -> u32 {
    side / 12
}

/// The largest block 7.1's bound allows in a lens image `side` px square
/// whose image circle has radius `radius` px about a centre `offset` px
/// from the square's: s/2 − |offset| − (radius + 2)/√2. Its inner corner
/// then lies at least 2 px outside the circle, wherever the offset
/// points.
pub fn block_bound(side: u32, offset: f64, radius: f64) -> f64 {
    let _ = (side, offset, radius);
    todo!("M3.1 unit (a)")
}

/// Whether [`block_side`]'s blocks stay clear of a rendered lens's image
/// circle by 7.1's bound ([`block_bound`]). M3.3's `render_lens` asserts
/// it for every truth it renders, with the circle's radius at the truth's
/// `theta_max` and its centre's offset from the square's.
pub fn check_clear(side: u32, offset: f64, radius: f64) -> Result<(), CodeError> {
    let _ = (side, offset, radius);
    todo!("M3.1 unit (a)")
}

/// The 25 bits that hold `code`: its frame in bits 0 to 15, its lens in
/// bit 16 and the checksum in bits 17 to 24. Cell k of a block holds bit
/// k, row by row from the top left.
pub fn code_word(code: CornerCode) -> u32 {
    let _ = code;
    todo!("M3.1 unit (a)")
}

/// The code that `word`'s 25 bits hold, or `None` when its checksum
/// doesn't hold or it has bits above them.
pub fn read_word(word: u32) -> Option<CornerCode> {
    let _ = word;
    todo!("M3.1 unit (a)")
}

/// Paint the corner code of frame `frame` (its low 16 bits) of lens
/// `lens` into the four corners of `img`, a square lens image: each cell
/// black or white, so that its chroma is neutral in any matrix.
pub fn corner_code(img: &mut RgbImage, lens: usize, frame: u32) -> Result<(), CodeError> {
    let _ = (img, lens, frame);
    todo!("M3.1 unit (a)")
}

/// How a luma plane's samples are stored.
#[derive(Debug, Clone, Copy)]
pub enum Samples<'a> {
    /// A byte each: 8 bits.
    U8(&'a [u8]),
    /// A `u16` each, the value in the low bits: synth's own planes and
    /// `DecodedFrame::planes_u16`'s.
    U16(&'a [u16]),
    /// Two bytes each, little-endian, as FFmpeg's frames and a GPU
    /// readback hold them: the value in the low bits (`yuv420p10le`), or
    /// in the high ones when `msb_aligned` (P010).
    Le16 { bytes: &'a [u8], msb_aligned: bool },
}

/// A picture's luma plane, as a decoder hands it out or a readback holds
/// it: `width` × `height` samples of `bits` bits, rows `stride` samples
/// apart.
#[derive(Debug, Clone, Copy)]
pub struct Luma<'a> {
    pub samples: Samples<'a>,
    pub width: u32,
    pub height: u32,
    /// Samples, not bytes, from the start of one row to the next.
    pub stride: usize,
    /// Significant bits a sample: 8 to 16.
    pub bits: u32,
}

impl<'a> Luma<'a> {
    /// The luma of `planes`, a `width` × `height` picture of `bits` bits
    /// as synth writes it.
    pub fn of_yuv(planes: &'a YuvPlanes, width: u32, height: u32, bits: u32) -> Luma<'a> {
        let _ = (planes, width, height, bits);
        todo!("M3.1 unit (a)")
    }

    /// The luma of a decoded picture in system memory: its first plane,
    /// as its layout says it is stored.
    pub fn of_picture(picture: &'a dyn VideoPlanes) -> Result<Luma<'a>, CodeError> {
        let _ = (picture, SampleBytes::One);
        todo!("M3.1 unit (a)")
    }
}

/// Read the corner code of a picture's luma: each corner's block on its
/// own ([`read_corners`]), then the code every corner whose checksum
/// holds agrees on. One corner lost to the picture or the coding still
/// leaves the others.
pub fn read_corner_code(luma: &Luma<'_>) -> Result<CornerCode, CodeError> {
    let _ = luma;
    todo!("M3.1 unit (a)")
}

/// What each corner's block reads as, top left, top right, bottom left,
/// then bottom right.
pub fn read_corners(luma: &Luma<'_>) -> Result<[Corner; 4], CodeError> {
    let _ = luma;
    todo!("M3.1 unit (a)")
}
