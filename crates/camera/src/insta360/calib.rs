//! The calibration strings (insta360.md §4.1; M3 plan 5.2): parsed into
//! named tokens, and printed back byte for byte. M3.2's unit (b).
//!
//! A string is decimal tokens joined by `_`: the lens count N, N blocks,
//! for V1 the canvas's width and height, and last a packed word whose
//! bits 16 and up give the generation, bits 8 to 15 the body code (4 on
//! the X5) and, in V1, bits 0 to 7 the lens type. A block is
//!
//! - V1 (field 5): `radius cx cy yaw pitch roll`;
//! - V2 (field 53): `radius cx cy yaw pitch roll tx ty tz c0 c1 c2 c3 w h
//!   type`;
//! - V3 (field 54): `xi fx fy cx cy yaw pitch roll tx ty tz k1 k2 k3 p1 p2
//!   w h type`;
//! - V6 (field 111): `xi fx fy cx cy yaw pitch roll tx ty tz d0 … d12 w h
//!   type`,
//!
//! so two lenses take 16, 34, 40 or 56 tokens. Numbers are in the
//! canvas's pixels (block 1's `cx` includes block 0's width), degrees and
//! metres. Each value keeps its token's text, so that `parse(g,
//! &print(c)) == c` bit for bit, and `print(&parse(g, s)?) == s` for every
//! string in Pat's files.
//!
//! Which track a block describes, how its numbers scale to a track and
//! how its angles compose are the rig's questions (M3 plan 6.2), not
//! the strings'; M3.5's `to_calibration` hands these tokens to it.

#![expect(
    unused_variables,
    reason = "the bodies are todo!() until M3.2's unit (b) writes them"
)]

use std::fmt;

use crate::CameraError;

/// A calibration string's generation (insta360.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Generation {
    /// A radius, a centre and three angles a lens (fields 5 and 17).
    V1,
    /// V1's block with a translation, four terms and the canvas (fields
    /// 53 and 55).
    V2,
    /// Mei's ξ and focal lengths with k1 k2 k3 p1 p2 (fields 54 and 56).
    V3,
    /// Mei's ξ and focal lengths with 13 terms, d0 to d12 (fields 111 and
    /// 112).
    V6,
}

impl Generation {
    /// All four, oldest first.
    pub const ALL: [Generation; 4] = [
        Generation::V1,
        Generation::V2,
        Generation::V3,
        Generation::V6,
    ];

    /// The metadata fields of this generation's working string and of its
    /// original copy: 5 and 17, 53 and 55, 54 and 56, or 111 and 112.
    pub fn fields(self) -> [u32; 2] {
        todo!()
    }

    /// The generation the packed word's bits 16 and up give: 0 V1, 2 V2,
    /// 3 V3, 6 V6; None for any other.
    pub fn of_word(word: u32) -> Option<Generation> {
        todo!()
    }

    /// How many tokens a string of `lenses` lenses has: 6N + 4 (V1),
    /// 16N + 2 (V2), 19N + 2 (V3) or 27N + 2 (V6).
    pub fn tokens(self, lenses: usize) -> usize {
        todo!()
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Generation::V1 => "V1",
            Generation::V2 => "V2",
            Generation::V3 => "V3",
            Generation::V6 => "V6",
        })
    }
}

/// A number as a string holds it: the text, which printing gives back,
/// and its value.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    text: String,
    value: f64,
}

impl Token {
    /// `text` as a token, if it is a decimal number as the strings write
    /// them: an optional minus, digits, and optionally a point and more
    /// digits (`2`, `-0.198`, `0.00000000`). None for anything else: an
    /// exponent, a plus, a space, `inf` or `nan`.
    pub fn new(text: &str) -> Option<Token> {
        todo!()
    }

    /// The token as written.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The number it writes, as `str::parse::<f64>` reads it.
    pub fn value(&self) -> f64 {
        self.value
    }
}

/// How a block gives its lens's scale.
#[derive(Debug, Clone, PartialEq)]
pub enum Focal {
    /// V1 and V2: the image circle's radius, in canvas pixels.
    Radius(Token),
    /// V3 and V6: Mei's ξ, and the focal lengths in canvas pixels.
    Mei { xi: Token, fx: Token, fy: Token },
}

/// One block's tokens, named, in canvas pixels, degrees and metres.
#[derive(Debug, Clone, PartialEq)]
pub struct LensTokens {
    pub focal: Focal,
    /// The image centre on the whole canvas: block 1's includes block 0's
    /// width.
    pub cx: Token,
    pub cy: Token,
    /// The stored yaw, pitch and roll, in degrees.
    pub ypr: [Token; 3],
    /// The stored translation tx, ty and tz, in metres; V1 has none.
    pub t: Option<[Token; 3]>,
    /// The distortion terms in token order: V2's c0 to c3, V3's k1 k2 k3
    /// p1 p2, V6's d0 to d12; V1 has none. Their meaning is the lens
    /// model's (M3 plan 6.1), not the string's.
    pub coefficients: Vec<Token>,
}

/// One calibration string, parsed (M3 plan 5.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Calib {
    pub generation: Generation,
    /// Block 0, the canvas's left square, then block 1.
    pub lenses: [LensTokens; 2],
    /// The whole side-by-side canvas, width and height: V1's global pair,
    /// or the `w h` both blocks end with in the later generations (10752
    /// × 5376 on the X5).
    pub canvas: (u32, u32),
    /// The packed word, the last token.
    pub word: u32,
    /// The lens type: V1's word's low byte, or the `type` both blocks end
    /// with (113 on the X5's bare lens).
    pub lens_type: u32,
}

/// Parses `s` as a string of `generation`'s.
///
/// A token count the grammar doesn't allow is [`CameraError::TokenCount`];
/// a token that isn't a number, a lens count other than 2, a packed word
/// of another generation, blocks that disagree on the canvas or the lens
/// type, or an integer token that wouldn't print back as written is
/// [`CameraError::BadToken`]. Each error's offset is a byte of `s`.
pub fn parse(generation: Generation, s: &str) -> Result<Calib, CameraError> {
    todo!()
}

/// Prints `c` as the camera writes it: its tokens' own text, joined by
/// `_`.
pub fn print(c: &Calib) -> String {
    todo!()
}
