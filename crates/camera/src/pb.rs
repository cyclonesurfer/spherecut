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

use crate::{CameraError, Part};

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
/// error that ends the walk: a bad varint, a key with field number 0, a
/// wire type the metadata doesn't use (a group's, or 6 or 7), or a value
/// cut short. After an error it yields nothing more.
#[derive(Debug, Clone)]
#[expect(
    dead_code,
    reason = "the walk is todo!() until M3.2's unit (b) writes it"
)]
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
}

impl<'a> Iterator for Walker<'a> {
    type Item = Result<Field<'a>, CameraError>;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}
