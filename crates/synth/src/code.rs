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
//! 1, over the other 17, then XORed with [`CHECK_XOR`] so that a block
//! all black or all white is no code). Any one, two or three cells read
//! wrong fail the checksum. All four corners hold the same code. A corner
//! is read from the luma of each cell's middle, so that a lossy encode's
//! ringing at the cells' edges doesn't count, and the picture's code is
//! the one every corner whose checksum holds agrees on.
//!
//! A block must stay clear of the image circle, or it would cover what
//! the lens shows: [`check_clear`] holds a rendered lens to 7.1's bound,
//! b ≤ s/2 − |centre offset| − (r(θ_max) + 2)/√2. It is for M3.3's
//! `render_lens`, which asserts it for every truth it renders; the perf
//! clips, whose picture is noise, have no circle to keep clear of.

use spherecut_core::video::{ColorFamily, SampleBytes, VideoPlanes};
use thiserror::Error;

use crate::RgbImage;
use crate::yuv::YuvPlanes;

/// Cells along each side of a block.
pub const GRID: u32 = 5;

/// Bits of frame number a code holds: frames count modulo 2¹⁶.
pub const FRAME_BITS: u32 = 16;

/// The lenses a code tells apart: indices 0 and 1.
pub const LENSES: usize = 2;

/// The codes' layout, raised whenever a code would be painted otherwise:
/// its grid, its bits, its checksum, its blocks' size or where they lie.
/// A picture coded in one layout doesn't read in another, so a perf
/// pair's stamp holds this and hw-test makes the pair again when it
/// changes; `tests/code.rs` pins the pixels of the layout it names.
pub const FORMAT: u32 = 1;

/// The bits a code's checksum covers: the frame's and the lens's.
const DATA_BITS: u32 = FRAME_BITS + 1;

/// The bits of a whole code: one a cell.
const WORD_BITS: u32 = GRID * GRID;

/// CRC-8's generator, x⁸ + x² + x + 1 without its x⁸ term. It has x + 1
/// as a factor, so it catches every odd number of wrong bits, and over 17
/// bits no two codes are closer than 4 bits.
const CRC_POLY: u8 = 0x07;

/// What the checksum is XORed with, so that neither an all-black block
/// (every bit 0) nor an all-white one (every bit 1) holds a code.
pub const CHECK_XOR: u8 = 0xa5;

/// A cell reads as white above this fraction of the samples' range and
/// as black below [`BLACK_BELOW`]; between the two it reads as neither.
const WHITE_ABOVE: f64 = 5.0 / 8.0;
const BLACK_BELOW: f64 = 3.0 / 8.0;

/// What a corner code says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CornerCode {
    /// The lens, 0 or 1.
    pub lens: usize,
    /// The frame number's low [`FRAME_BITS`] bits.
    pub frame: u16,
}

impl CornerCode {
    /// The code of frame `frame` (its low 16 bits) of lens `lens`.
    pub fn of(lens: usize, frame: u32) -> CornerCode {
        CornerCode {
            lens,
            frame: (frame & 0xffff) as u16,
        }
    }
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
    #[error("a picture {side} px square has blocks of {block} px, fewer than its {GRID} cells")]
    TooSmall { side: u32, block: u32 },
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
    f64::from(side) / 2.0 - offset.abs() - (radius + 2.0) / std::f64::consts::SQRT_2
}

/// Whether [`block_side`]'s blocks stay clear of a rendered lens's image
/// circle by 7.1's bound ([`block_bound`]). M3.3's `render_lens` asserts
/// it for every truth it renders, with the circle's radius at the truth's
/// `theta_max` and its centre's offset from the square's.
pub fn check_clear(side: u32, offset: f64, radius: f64) -> Result<(), CodeError> {
    let block = block_side(side);
    let bound = block_bound(side, offset, radius);
    // A NaN bound is no bound: it fails as one that is too small.
    if f64::from(block) <= bound {
        Ok(())
    } else {
        Err(CodeError::ReachesCircle {
            side,
            block,
            offset,
            radius,
            bound,
        })
    }
}

/// CRC-8 of the low `bits` bits of `data`, the highest first, from 0.
fn crc8(data: u32, bits: u32) -> u8 {
    let mut crc = 0u8;
    for i in (0..bits).rev() {
        let bit = ((data >> i) & 1) as u8;
        let top = crc >> 7;
        crc <<= 1;
        if top ^ bit == 1 {
            crc ^= CRC_POLY;
        }
    }
    crc
}

/// The 25 bits that hold `code`: its frame in bits 0 to 15, its lens in
/// bit 16 and the checksum in bits 17 to 24. Cell k of a block holds bit
/// k, row by row from the top left.
pub fn code_word(code: CornerCode) -> Result<u32, CodeError> {
    if code.lens >= LENSES {
        return Err(CodeError::Lens(code.lens));
    }
    let data = u32::from(code.frame) | (code.lens as u32) << FRAME_BITS;
    let check = crc8(data, DATA_BITS) ^ CHECK_XOR;
    Ok(data | u32::from(check) << DATA_BITS)
}

/// The code that `word`'s 25 bits hold, or `None` when its checksum
/// doesn't hold or it has bits above them.
pub fn read_word(word: u32) -> Option<CornerCode> {
    if word >> WORD_BITS != 0 {
        return None;
    }
    let data = word & ((1 << DATA_BITS) - 1);
    let check = (word >> DATA_BITS) as u8;
    (crc8(data, DATA_BITS) ^ CHECK_XOR == check).then_some(CornerCode {
        lens: (data >> FRAME_BITS) as usize,
        frame: (data & 0xffff) as u16,
    })
}

/// The side of a square picture whose blocks can hold a code.
fn coded_side(width: u32, height: u32) -> Result<u32, CodeError> {
    if width != height {
        return Err(CodeError::NotSquare { width, height });
    }
    let block = block_side(width);
    if block < GRID {
        return Err(CodeError::TooSmall { side: width, block });
    }
    Ok(width)
}

/// The top left corner of each block of a picture `side` px square, in
/// [`read_corners`]' order.
fn origins(side: u32) -> [(u32, u32); 4] {
    let far = side - block_side(side);
    [(0, 0), (far, 0), (0, far), (far, far)]
}

/// The columns (or rows) of cell `i` along a block of side `b` from
/// `origin`: the cells split the block as evenly as whole pixels allow.
fn cell_span(origin: u32, b: u32, i: u32) -> (u32, u32) {
    (origin + i * b / GRID, origin + (i + 1) * b / GRID)
}

/// Paint the corner code of frame `frame` (its low 16 bits) of lens
/// `lens` into the four corners of `img`, a square lens image: each cell
/// black or white, so that its chroma is neutral in any matrix.
pub fn corner_code(img: &mut RgbImage, lens: usize, frame: u32) -> Result<(), CodeError> {
    let side = coded_side(img.width, img.height)?;
    let word = code_word(CornerCode::of(lens, frame))?;
    let b = block_side(side);
    for (x0, y0) in origins(side) {
        for k in 0..WORD_BITS {
            let ink = if word >> k & 1 == 1 { 1.0 } else { 0.0 };
            let (xa, xb) = cell_span(x0, b, k % GRID);
            let (ya, yb) = cell_span(y0, b, k / GRID);
            for y in ya..yb {
                let row = (y * side) as usize;
                for px in &mut img.pixels[row + xa as usize..row + xb as usize] {
                    *px = [ink; 3];
                }
            }
        }
    }
    Ok(())
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

impl Samples<'_> {
    /// How many samples it holds.
    fn len(&self) -> usize {
        match self {
            Samples::U8(s) => s.len(),
            Samples::U16(s) => s.len(),
            Samples::Le16 { bytes, .. } => bytes.len() / 2,
        }
    }
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
        let samples = match planes {
            YuvPlanes::U8(p) => Samples::U8(&p[0]),
            YuvPlanes::U16(p) => Samples::U16(&p[0]),
        };
        Luma {
            samples,
            width,
            height,
            stride: width as usize,
            bits,
        }
    }

    /// The luma of a decoded picture in system memory: its first plane,
    /// as its layout says it is stored.
    pub fn of_picture(picture: &'a dyn VideoPlanes) -> Result<Luma<'a>, CodeError> {
        let layout = picture.layout();
        if layout.family == ColorFamily::Rgb {
            return Err(CodeError::Plane(format!("{} has no luma", layout.name)));
        }
        let plane = picture.plane(0).ok_or_else(|| {
            CodeError::Plane(
                "the picture is still on its device, so its planes can't be read".into(),
            )
        })?;
        let desc = layout
            .planes
            .first()
            .ok_or_else(|| CodeError::Plane(format!("{} has no planes", layout.name)))?;
        let (samples, stride) = match desc.bytes {
            SampleBytes::One => (Samples::U8(plane.bytes), plane.stride),
            SampleBytes::Two => (
                Samples::Le16 {
                    bytes: plane.bytes,
                    msb_aligned: layout.msb_aligned,
                },
                plane.stride / 2,
            ),
        };
        Ok(Luma {
            samples,
            width: plane.width,
            height: plane.height,
            stride,
            bits: u32::from(layout.bit_depth),
        })
    }

    /// Whether every row lies inside the samples, or why not.
    fn check(&self) -> Result<(), CodeError> {
        if !(8..=16).contains(&self.bits) {
            return Err(CodeError::Plane(format!(
                "{} bits a sample, not 8 to 16",
                self.bits
            )));
        }
        let width = self.width as usize;
        let end = (self.height as usize)
            .checked_sub(1)
            .and_then(|r| r.checked_mul(self.stride))
            .and_then(|s| s.checked_add(width));
        match end {
            _ if self.stride < width => Err(CodeError::Plane(format!(
                "rows of {width} samples are {} apart",
                self.stride
            ))),
            Some(end) if end <= self.samples.len() => Ok(()),
            _ => Err(CodeError::Plane(format!(
                "{} rows of {width} samples, {} apart, don't fit {} samples",
                self.height,
                self.stride,
                self.samples.len()
            ))),
        }
    }

    /// The sample at (`x`, `y`), inside the plane ([`Luma::check`]).
    fn at(&self, x: u32, y: u32) -> u32 {
        let i = y as usize * self.stride + x as usize;
        match self.samples {
            Samples::U8(s) => u32::from(s[i]),
            Samples::U16(s) => u32::from(s[i]),
            Samples::Le16 { bytes, msb_aligned } => {
                let v = u32::from(u16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]));
                if msb_aligned {
                    v >> (16 - self.bits)
                } else {
                    v
                }
            }
        }
    }
}

/// The level of each cell of the block at (`x0`, `y0`), of side `b`, as
/// a fraction of the samples' range: the mean of the cell's middle, a
/// quarter of the cell in from each edge and at least its middle pixel.
fn block_levels(luma: &Luma<'_>, (x0, y0): (u32, u32), b: u32) -> [f64; WORD_BITS as usize] {
    let max = (1u32 << luma.bits) - 1;
    let mut levels = [0.0; WORD_BITS as usize];
    for (k, level) in (0..WORD_BITS).zip(&mut levels) {
        let (xa, xb) = cell_span(x0, b, k % GRID);
        let (ya, yb) = cell_span(y0, b, k / GRID);
        let (mx, my) = ((xb - xa) / 4, (yb - ya) / 4);
        let (mut sum, mut n) = (0u64, 0u64);
        for y in ya + my..yb - my {
            for x in xa + mx..xb - mx {
                sum += u64::from(luma.at(x, y).min(max));
                n += 1;
            }
        }
        *level = sum as f64 / n as f64 / f64::from(max);
    }
    levels
}

/// What a block whose cells have `levels` reads as.
fn read_levels(levels: &[f64; WORD_BITS as usize]) -> Corner {
    let mut word = 0;
    for (k, &level) in (0..WORD_BITS).zip(levels) {
        if level > WHITE_ABOVE {
            word |= 1 << k;
        } else if level >= BLACK_BELOW {
            return Corner::Unclear { cell: k };
        }
    }
    match read_word(word) {
        Some(code) => Corner::Code(code),
        None => Corner::Checksum { word },
    }
}

/// Read the corner code of a picture's luma: each corner's block on its
/// own ([`read_corners`]), then the code every corner whose checksum
/// holds agrees on. One corner lost to the picture or the coding still
/// leaves the others; two corners whose codes differ are refused.
pub fn read_corner_code(luma: &Luma<'_>) -> Result<CornerCode, CodeError> {
    let corners = read_corners(luma)?;
    let mut codes = corners.iter().filter_map(|c| match c {
        Corner::Code(code) => Some(*code),
        _ => None,
    });
    let Some(first) = codes.next() else {
        return Err(CodeError::NoCode(corners));
    };
    if codes.all(|c| c == first) {
        Ok(first)
    } else {
        Err(CodeError::Disagree(corners))
    }
}

/// What each corner's block reads as, top left, top right, bottom left,
/// then bottom right.
pub fn read_corners(luma: &Luma<'_>) -> Result<[Corner; 4], CodeError> {
    luma.check()?;
    let side = coded_side(luma.width, luma.height)?;
    let b = block_side(side);
    Ok(origins(side).map(|o| read_levels(&block_levels(luma, o, b))))
}

/// How clearly a picture's blocks read: the least distance of any cell's
/// level from the band between black and white that reads as neither, as
/// a fraction of the samples' range, over all four corners. A lossless
/// picture is clear by 3/8 (or by what its range leaves of it); a coding
/// whose clearance nears 0 is close to losing a code, and a negative one
/// has a cell that reads as neither.
pub fn clearance(luma: &Luma<'_>) -> Result<f64, CodeError> {
    luma.check()?;
    let side = coded_side(luma.width, luma.height)?;
    let b = block_side(side);
    Ok(origins(side)
        .iter()
        .flat_map(|&o| block_levels(luma, o, b))
        .map(|l| (l - WHITE_ABOVE).max(BLACK_BELOW - l))
        .fold(f64::INFINITY, f64::min))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_crc_8_of_the_frame_and_lens() {
        // CRC-8 (x⁸ + x² + x + 1, from 0, unreflected: SMBus's, whose
        // check value over "123456789" is 0xf4) of "1" and of "123", by a
        // byte-wise table form of it.
        assert_eq!(crc8(u32::from(b'1'), 8), 0x97);
        assert_eq!(crc8(0x31_32_33, 24), 0xc0);
        assert_eq!(crc8(0, DATA_BITS), 0);
        // Frame 0 of lens 0 is all-black data with the XOR as its check.
        assert_eq!(
            code_word(CornerCode::of(0, 0)),
            Ok(u32::from(CHECK_XOR) << DATA_BITS)
        );
        assert_eq!(
            CornerCode::of(1, 0x1_0005),
            CornerCode { lens: 1, frame: 5 }
        );
    }

    #[test]
    fn cells_split_a_block_into_whole_pixels() {
        for b in [5, 21, 32, 240, 320] {
            let spans: Vec<(u32, u32)> = (0..GRID).map(|i| cell_span(10, b, i)).collect();
            assert_eq!(spans[0].0, 10);
            assert_eq!(spans[4].1, 10 + b);
            assert!(spans.windows(2).all(|w| w[0].1 == w[1].0), "{spans:?}");
            assert!(spans.iter().all(|(a, z)| z > a), "{spans:?}");
        }
    }
}
