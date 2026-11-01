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

use std::fmt;
use std::str::Split;

use crate::CameraError;

/// The lens count every string read gives, token 0: Calib holds two
/// blocks.
const LENSES: usize = 2;

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
        match self {
            Generation::V1 => [5, 17],
            Generation::V2 => [53, 55],
            Generation::V3 => [54, 56],
            Generation::V6 => [111, 112],
        }
    }

    /// The generation the packed word's bits 16 and up give: 0 V1, 2 V2,
    /// 3 V3, 6 V6; None for any other.
    pub fn of_word(word: u32) -> Option<Generation> {
        Generation::ALL
            .into_iter()
            .find(|g| u32::from(g.code()) == word >> 16)
    }

    /// The code bits 16 and up of its packed word hold.
    fn code(self) -> u8 {
        match self {
            Generation::V1 => 0,
            Generation::V2 => 2,
            Generation::V3 => 3,
            Generation::V6 => 6,
        }
    }

    /// How many tokens a string of `lenses` lenses has: 6N + 4 (V1),
    /// 16N + 2 (V2), 19N + 2 (V3) or 27N + 2 (V6).
    pub fn tokens(self, lenses: usize) -> usize {
        let (block, more): (usize, usize) = match self {
            Generation::V1 => (6, 4),
            Generation::V2 => (16, 2),
            Generation::V3 => (19, 2),
            Generation::V6 => (27, 2),
        };
        block.saturating_mul(lenses).saturating_add(more)
    }

    /// How many distortion terms a block has: none in V1, V2's c0 to c3,
    /// V3's k1 k2 k3 p1 p2, V6's d0 to d12.
    fn coefficients(self) -> usize {
        match self {
            Generation::V1 => 0,
            Generation::V2 => 4,
            Generation::V3 => 5,
            Generation::V6 => 13,
        }
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
    /// exponent, a plus, a space, `inf` or `nan`, or digits too many for
    /// an f64 to hold as a finite number.
    pub fn new(text: &str) -> Option<Token> {
        let unsigned = text.strip_prefix('-').unwrap_or(text);
        let decimal = match unsigned.split_once('.') {
            Some((whole, fraction)) => digits(whole) && digits(fraction),
            None => digits(unsigned),
        };
        if !decimal {
            return None;
        }
        let value = text.parse::<f64>().ok().filter(|v| v.is_finite())?;
        Some(Token {
            text: text.to_owned(),
            value,
        })
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
///
/// The count is checked first, so that a token lost or added is that
/// error, not the token that lands in a place it doesn't fit. The empty
/// string has no tokens. The integer tokens (the lens count, the canvas,
/// the lens type and the word) must be decimal digits with no leading
/// zero, as a u32 prints; nothing allocated depends on `s`'s length until
/// the count is right.
pub fn parse(generation: Generation, s: &str) -> Result<Calib, CameraError> {
    count(generation, s)?;
    let mut t = Tokens {
        generation,
        split: s.split('_'),
        at: 0,
        len: s.len(),
    };
    let (n_at, n) = t.whole()?;
    if n as usize != LENSES {
        return Err(t.bad(n_at, "isn't a lens count of 2, the only one read"));
    }
    let (lens0, tail0) = t.block()?;
    let (lens1, tail1) = t.block()?;
    let (canvas, lens_type) = match (tail0, tail1) {
        (Some(a), Some(b)) => {
            if (b.w.1, b.h.1) != (a.w.1, a.h.1) {
                let at = if b.w.1 != a.w.1 { b.w.0 } else { b.h.0 };
                return Err(t.bad(at, "gives a canvas other than block 0's"));
            }
            if b.lens_type.1 != a.lens_type.1 {
                return Err(t.bad(b.lens_type.0, "gives a lens type other than block 0's"));
            }
            ((a.w.1, a.h.1), Some(a.lens_type.1))
        }
        // V1 gives the canvas once, after the blocks, and its lens type in
        // the word.
        _ => ((t.whole()?.1, t.whole()?.1), None),
    };
    let (word_at, word) = t.whole()?;
    if Generation::of_word(word) != Some(generation) {
        return Err(t.bad(word_at, "is a packed word of another generation"));
    }
    Ok(Calib {
        generation,
        lenses: [lens0, lens1],
        canvas,
        word,
        lens_type: lens_type.unwrap_or(word & 0xff),
    })
}

/// Prints `c` as the camera writes it: its tokens' own text, joined by
/// `_`.
///
/// What [`parse`] gives prints back as it was parsed. A `Calib` it can't
/// give, such as a V1 block with a translation or a V1 lens type that
/// isn't its word's low byte, prints as its tokens say, which `parse`
/// then refuses or reads otherwise.
pub fn print(c: &Calib) -> String {
    let mut out = c.lenses.len().to_string();
    let mut push = |text: &str| {
        out.push('_');
        out.push_str(text);
    };
    for lens in &c.lenses {
        match &lens.focal {
            Focal::Radius(r) => push(r.text()),
            Focal::Mei { xi, fx, fy } => [xi, fx, fy].iter().for_each(|t| push(t.text())),
        }
        let t = lens.t.iter().flatten();
        for token in [&lens.cx, &lens.cy].into_iter().chain(&lens.ypr).chain(t) {
            push(token.text());
        }
        lens.coefficients.iter().for_each(|k| push(k.text()));
        if c.generation != Generation::V1 {
            for n in [c.canvas.0, c.canvas.1, c.lens_type] {
                push(&n.to_string());
            }
        }
    }
    if c.generation == Generation::V1 {
        push(&c.canvas.0.to_string());
        push(&c.canvas.1.to_string());
    }
    push(&c.word.to_string());
    out
}

/// [`CameraError::TokenCount`] unless `s` has as many tokens as two lenses
/// of `generation`'s take, or [`CameraError::BadToken`] when its count is
/// another number of lenses' and token 0 says so.
fn count(generation: Generation, s: &str) -> Result<(), CameraError> {
    let found = if s.is_empty() {
        0
    } else {
        s.split('_').count()
    };
    let expected = generation.tokens(LENSES);
    if found == expected {
        return Ok(());
    }
    // A string of 3 lenses, say, whose count is right for 3: its lens
    // count is what is wrong.
    let lenses = s.split('_').next().and_then(whole);
    if lenses.is_some_and(|n| generation.tokens(n as usize) == found) {
        return Err(CameraError::BadToken {
            generation,
            why: "isn't a lens count of 2, the only one read",
            offset: 0,
        });
    }
    // Where it went wrong: the end, or the first token too many.
    let offset = match s.match_indices('_').nth(expected - 1) {
        Some((at, _)) if found > expected => at + 1,
        _ => s.len(),
    };
    Err(CameraError::TokenCount {
        generation,
        found,
        offset: offset as u64,
    })
}

/// Whether `s` is one or more decimal digits.
fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `text` as a u32, if it is one as a u32 prints: digits with no leading
/// zero.
fn whole(text: &str) -> Option<u32> {
    let canonical = text == "0" || !text.starts_with('0');
    if !(digits(text) && canonical) {
        return None;
    }
    text.parse().ok()
}

/// The integers a block of V2, V3 or V6 ends with, each with its byte.
#[derive(Debug, Clone, Copy)]
struct Tail {
    w: (usize, u32),
    h: (usize, u32),
    lens_type: (usize, u32),
}

/// A string's tokens, read in order, each with the byte it starts at.
struct Tokens<'s> {
    generation: Generation,
    split: Split<'s, char>,
    /// Where the next token starts.
    at: usize,
    /// The string's length, where a token past the last would start.
    len: usize,
}

impl<'s> Tokens<'s> {
    /// [`CameraError::BadToken`] at byte `at`.
    fn bad(&self, at: usize, why: &'static str) -> CameraError {
        CameraError::BadToken {
            generation: self.generation,
            why,
            offset: at as u64,
        }
    }

    /// The next token's text and where it starts. [`count`] has checked
    /// that there is one; were there none, it is a count error at the
    /// end.
    fn next(&mut self) -> Result<(usize, &'s str), CameraError> {
        let at = self.at;
        let text = self.split.next().ok_or(CameraError::TokenCount {
            generation: self.generation,
            found: self.generation.tokens(LENSES),
            offset: self.len as u64,
        })?;
        self.at += text.len() + 1;
        Ok((at, text))
    }

    /// The next token, a decimal number.
    fn number(&mut self) -> Result<Token, CameraError> {
        let (at, text) = self.next()?;
        Token::new(text).ok_or_else(|| self.bad(at, "isn't a decimal number"))
    }

    /// The next token, an integer as a u32 prints, and where it starts.
    fn whole(&mut self) -> Result<(usize, u32), CameraError> {
        let (at, text) = self.next()?;
        whole(text)
            .map(|n| (at, n))
            .ok_or_else(|| self.bad(at, "isn't a whole number as a u32 prints"))
    }

    /// The next block, and for V2, V3 and V6 the canvas and lens type it
    /// ends with.
    fn block(&mut self) -> Result<(LensTokens, Option<Tail>), CameraError> {
        let g = self.generation;
        let focal = match g {
            Generation::V1 | Generation::V2 => Focal::Radius(self.number()?),
            Generation::V3 | Generation::V6 => Focal::Mei {
                xi: self.number()?,
                fx: self.number()?,
                fy: self.number()?,
            },
        };
        let (cx, cy) = (self.number()?, self.number()?);
        let ypr = [self.number()?, self.number()?, self.number()?];
        let t = match g {
            Generation::V1 => None,
            _ => Some([self.number()?, self.number()?, self.number()?]),
        };
        let coefficients = (0..g.coefficients())
            .map(|_| self.number())
            .collect::<Result<_, _>>()?;
        let tail = match g {
            Generation::V1 => None,
            _ => Some(Tail {
                w: self.whole()?,
                h: self.whole()?,
                lens_type: self.whole()?,
            }),
        };
        let lens = LensTokens {
            focal,
            cx,
            cy,
            ypr,
            t,
            coefficients,
        };
        Ok((lens, tail))
    }
}
