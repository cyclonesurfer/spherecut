//! A protobuf message's fields, one at a time and with no schema: the
//! hand-written varint walker the metadata record is read with (the M3
//! plan's D62).
//! M3.2's unit (b).
//!
//! The wire format is protobuf's own: each field a varint key (its number
//! times 8, plus its wire type), then a varint (type 0), 8 bytes (1), a
//! varint length and that many bytes (2), or 4 bytes (5). The walker
//! reads exactly that and allocates nothing; a message inside a field is
//! walked by a walker of its own over the field's bytes, so the caller,
//! which knows the fields, bounds how deep it goes.
//!
//! A varint is little-endian in groups of 7 bits, each byte but the last
//! with its top bit set, in at most ten bytes; the tenth may hold only
//! bit 63. Padded varints (`0x80 0x00` for 0) read as their value, as
//! protobuf reads them. A key is a 32-bit varint whose field number isn't
//! 0, as protobuf's are.

use crate::{CameraError, Part};

/// The most bytes a varint takes: ten, for 64 bits in groups of 7.
const MAX_VARINT: usize = 10;

/// One field's value, as its wire type gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    /// Wire type 0: an integer, a bool or an enum.
    Varint(u64),
    /// Wire type 1: 8 bytes, little-endian (a double, or a fixed64).
    I64(u64),
    /// Wire type 2: a string, bytes or a message.
    Len(&'a [u8]),
    /// Wire type 5: 4 bytes, little-endian (a float, or a fixed32).
    I32(u32),
}

/// One field of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field<'a> {
    /// Its number, from 1.
    pub number: u32,
    pub value: Value<'a>,
    /// Where its key starts, counted as the walker's offsets are.
    pub offset: u64,
    /// Where its value starts: after the key and, for [`Value::Len`], the
    /// length. A message inside the field is walked from here.
    pub value_offset: u64,
}

/// A message's fields in the order written. Each item is a field, or the
/// error that ends the walk: [`CameraError::BadVarint`] at the start of a
/// varint over ten bytes or past 64 bits, or of a key past 32 bits or
/// with field number 0; [`CameraError::WireType`] at the key of a field
/// whose wire type the metadata doesn't use (a group's, or 6 or 7); or
/// [`CameraError::Truncated`] at the end of the bytes, when a key or a
/// value runs past it. After an error it yields nothing more.
#[derive(Debug, Clone)]
pub struct Walker<'a> {
    bytes: &'a [u8],
    /// The next byte to read, in `bytes`.
    at: usize,
    /// What `bytes[0]` is at, so that offsets count from the record's
    /// start whatever message is walked.
    base: u64,
    /// What errors say they are in.
    part: Part,
    /// Set once an error has been yielded.
    done: bool,
}

impl<'a> Walker<'a> {
    /// A walker over `bytes`, a message that is `part` (`Part::Record(1)`
    /// for the metadata, `Part::Field(n)` for a message inside its field
    /// n), whose first byte is at `base`.
    pub fn new(bytes: &'a [u8], part: Part, base: u64) -> Walker<'a> {
        Walker {
            bytes,
            at: 0,
            base,
            part,
            done: false,
        }
    }

    /// Where byte `at` of the bytes walked is, as offsets count.
    fn offset(&self, at: usize) -> u64 {
        self.base.saturating_add(at as u64)
    }

    /// The bytes ending before what they say comes next: an error at the
    /// first byte missing, the end of the bytes walked.
    fn cut(&self) -> CameraError {
        CameraError::Truncated {
            part: self.part,
            offset: self.offset(self.bytes.len()),
        }
    }

    /// The varint starting at byte `at`, and the byte after it.
    fn varint(&self, at: usize) -> Result<(u64, usize), CameraError> {
        let mut value = 0;
        for i in 0..MAX_VARINT {
            let Some(&b) = self.bytes.get(at + i) else {
                return Err(self.cut());
            };
            // The tenth byte holds bit 63 alone, and ends the varint.
            if i == MAX_VARINT - 1 && b > 1 {
                break;
            }
            value |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok((value, at + i + 1));
            }
        }
        Err(CameraError::BadVarint {
            part: self.part,
            offset: self.offset(at),
        })
    }

    /// The `N` bytes starting at byte `at`.
    fn fixed<const N: usize>(&self, at: usize) -> Result<[u8; N], CameraError> {
        self.bytes
            .get(at..)
            .and_then(|b| b.first_chunk::<N>())
            .copied()
            .ok_or_else(|| self.cut())
    }

    /// The field whose key is at byte `at`, and the byte after it.
    fn field(&self, at: usize) -> Result<(Field<'a>, usize), CameraError> {
        let (key, value_at) = self.varint(at)?;
        let number = u32::try_from(key)
            .ok()
            .map(|key| key >> 3)
            .filter(|&n| n != 0)
            .ok_or(CameraError::BadVarint {
                part: self.part,
                offset: self.offset(at),
            })?;
        let (value, value_at, next) = match key & 7 {
            0 => {
                let (v, next) = self.varint(value_at)?;
                (Value::Varint(v), value_at, next)
            }
            1 => {
                let v = u64::from_le_bytes(self.fixed(value_at)?);
                (Value::I64(v), value_at, value_at + 8)
            }
            2 => {
                let (len, from) = self.varint(value_at)?;
                let bytes = usize::try_from(len)
                    .ok()
                    .and_then(|len| self.bytes.get(from..)?.get(..len))
                    .ok_or_else(|| self.cut())?;
                (Value::Len(bytes), from, from + bytes.len())
            }
            5 => {
                let v = u32::from_le_bytes(self.fixed(value_at)?);
                (Value::I32(v), value_at, value_at + 4)
            }
            wire => {
                return Err(CameraError::WireType {
                    part: self.part,
                    wire: wire as u8,
                    offset: self.offset(at),
                });
            }
        };
        let field = Field {
            number,
            value,
            offset: self.offset(at),
            value_offset: self.offset(value_at),
        };
        Ok((field, next))
    }
}

impl<'a> Iterator for Walker<'a> {
    type Item = Result<Field<'a>, CameraError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.at >= self.bytes.len() {
            return None;
        }
        Some(match self.field(self.at) {
            Ok((field, next)) => {
                self.at = next;
                Ok(field)
            }
            Err(e) => {
                self.done = true;
                Err(e)
            }
        })
    }
}
