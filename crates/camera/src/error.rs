//! What can be wrong with a camera's bytes, and where.

use std::fmt;
use std::io;

use thiserror::Error;

use crate::insta360::calib::Generation;
use crate::insta360::limits::{MAX_PROFILES, MAX_STRING, MAX_TRAILER};

/// The part of a file an error is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    /// The 72 bytes an Insta360 file ends with: zeros, the trailer's size,
    /// its version and the magic (insta360.md §3.1).
    Tail,
    /// The trailer's index, the record whose footer is at EOF − 78
    /// (§3.2).
    Index,
    /// A record, by id (§3.3).
    Record(u8),
    /// A top-level field of the metadata record, by number (§3.5),
    /// whatever message inside it the error is in.
    Field(u32),
}

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Part::Tail => write!(f, "the trailer's tail"),
            Part::Index => write!(f, "the trailer's index"),
            Part::Record(id) => write!(f, "record {id}"),
            Part::Field(n) => write!(f, "the metadata's field {n}"),
        }
    }
}

/// Why a camera's bytes couldn't be read.
///
/// Every error carries the byte it is at: in the file for the readers
/// (`read_trailer`, `read_record` and `verify_companion`), in the
/// record's payload for the parsers (`parse_metadata`, `parse_exposure`,
/// `thumbnail`, `imu_span` and [`crate::pb::Walker`]), and in the string
/// for `calib::parse`.
#[derive(Debug, Error)]
pub enum CameraError {
    /// The bytes end before what they say comes next.
    #[error("{part} is cut short at byte {offset}")]
    Truncated { part: Part, offset: u64 },
    /// An index slot whose record would run into the index or past the
    /// file. `read_trailer` lists such a slot in `Trailer::skipped` and
    /// never follows it; this is what reading its record gives. `offset`
    /// is the slot's.
    #[error("record {id}'s index slot at byte {offset} runs into the index or past the file")]
    SlotPastEnd { id: u8, offset: u64 },
    /// A record's footer that doesn't say what its index slot does: its
    /// id, format or size differ. `id` is the slot's, whatever it is: a
    /// damaged slot can claim id 0, the index's own. `offset` is the
    /// footer's.
    #[error("record {id}'s footer at byte {offset} doesn't match its index slot")]
    FooterMismatch { id: u8, offset: u64 },
    /// The footer at EOF − 78, where the index's belongs, when it isn't
    /// an index's (id 0, format 0): a trailer with no index, as older
    /// cameras write theirs, or a damaged one. `offset` is the footer's.
    #[error(
        "the footer at byte {offset}, where the trailer's index's is, isn't an index's: the trailer has no index (older cameras write none) or is damaged"
    )]
    IndexFooter { offset: u64 },
    /// A varint of more than ten bytes, or past 64 bits; a field's key
    /// past 32 bits or with field number 0, which protobuf doesn't allow;
    /// or a varint past 32 bits where the metadata holds a u32. `offset`
    /// is the varint's first byte.
    #[error("{part} has a bad varint at byte {offset}")]
    BadVarint { part: Part, offset: u64 },
    /// A protobuf wire type the metadata doesn't use: a group's (3 or 4),
    /// or 6 or 7, which protobuf doesn't define; or, for a field
    /// `parse_metadata` reads, a wire type other than the field's own,
    /// which is then one the metadata uses (0, 1, 2 or 5) and is worded
    /// so. `offset` is the field's key.
    #[error("{}", wire_type(.part, .wire, .offset))]
    WireType { part: Part, wire: u8, offset: u64 },
    /// More bytes than a limit allows (`insta360::limits`), or than the
    /// caller asked for. Nothing was allocated for them. In
    /// [`Part::Tail`] the size is the trailer's, which the tail gives, and
    /// the limit the file's length or `MAX_TRAILER`, whichever is less.
    /// In `Part::Field(145)` with `MAX_PROFILES` for its limit, it is
    /// more lens profiles of a kind than that: `size` counts them as far
    /// as the first one past it, at whose key `offset` is.
    #[error("{}", too_large(.part, .size, .limit, .offset))]
    TooLarge {
        part: Part,
        size: u64,
        limit: u64,
        offset: u64,
    },
    /// Fewer bytes than what they must hold. In [`Part::Tail`] the size
    /// is the trailer's, which the tail gives, and `least` what the
    /// index's footer and the tail take (78 bytes), or with the index
    /// that footer gives, 78 more than its size: a trailer smaller than
    /// that would have its index start before it does. `offset` is the
    /// byte that gives the size.
    #[error("{}", too_small(.part, .size, .least, .offset))]
    TooSmall {
        part: Part,
        size: u64,
        least: u64,
        offset: u64,
    },
    /// A trailer version other than 3, the only one documented.
    #[error("the trailer's version is {found}, not 3 (byte {offset})")]
    Version { found: u32, offset: u64 },
    /// A record whose size its layout can't have: an index (id 0) that
    /// isn't whole 10-byte slots, an IMU record (3) that isn't whole
    /// 20-byte items, an exposure record (4) that isn't whole 16-byte
    /// items, or a thumbnail (2) that isn't its header and the planes its
    /// header gives.
    #[error("record {id} is {size} bytes, which its layout doesn't allow (byte {offset})")]
    BadSize { id: u8, size: u64, offset: u64 },
    /// A string field that isn't UTF-8.
    #[error("{part} isn't UTF-8 (byte {offset})")]
    NotUtf8 { part: Part, offset: u64 },
    /// A calibration string with a token count its generation's grammar
    /// doesn't allow (insta360.md §4.1: 6N + 4, 16N + 2, 19N + 2 or
    /// 27N + 2 for N lenses). `offset` is where the count went wrong: the
    /// string's end when tokens are missing, the first extra token's start
    /// when there are too many.
    #[error(
        "the {generation} calibration string has {found} tokens, which its grammar doesn't allow (byte {offset})"
    )]
    TokenCount {
        generation: Generation,
        found: usize,
        offset: u64,
    },
    /// A calibration token that isn't what the grammar has in its place:
    /// not a decimal number, an integer (the lens count, the canvas, the
    /// lens type or the word) that isn't a u32 as it prints, a lens count
    /// other than 2, a packed word of another generation, or a block whose
    /// canvas or lens type isn't the other's. `why` says which, and
    /// `offset` is where the token starts.
    #[error("the {generation} calibration string's token at byte {offset} {why}")]
    BadToken {
        generation: Generation,
        why: &'static str,
        offset: u64,
    },
    /// Reading the file failed, in `part`, at the byte the seek or read
    /// was for.
    #[error("I/O error reading {part} at byte {offset}: {error}")]
    Io {
        part: Part,
        offset: u64,
        error: io::Error,
    },
}

/// `TooLarge`'s message. In the tail the size is the trailer's, not the
/// tail's own 72 bytes: a limit under `MAX_TRAILER` is the file's length.
/// In field 145 a limit of `MAX_PROFILES` counts lens profiles, which
/// tells it from a string's `MAX_STRING` there.
fn too_large(part: &Part, size: &u64, limit: &u64, offset: &u64) -> String {
    const _: () = assert!(MAX_PROFILES != MAX_STRING);
    match part {
        Part::Tail if *limit < MAX_TRAILER => format!(
            "the trailer's size in its tail is {size} bytes, more than the file's length of {limit} (byte {offset})"
        ),
        Part::Tail => format!(
            "the trailer's size in its tail is {size} bytes, over its limit of {limit} (byte {offset})"
        ),
        Part::Field(145) if *limit == MAX_PROFILES as u64 => format!(
            "{part} has more than {limit} lens profiles of a kind, polynomials or fields of view (byte {offset})"
        ),
        part => format!("{part} is {size} bytes, over its limit of {limit} (byte {offset})"),
    }
}

/// `TooSmall`'s message. In the tail the size is the trailer's, as in
/// `TooLarge`'s.
fn too_small(part: &Part, size: &u64, least: &u64, offset: &u64) -> String {
    match part {
        Part::Tail => format!(
            "the trailer's size in its tail is {size} bytes, less than the {least} its index and the tail take (byte {offset})"
        ),
        part => {
            format!("{part} is {size} bytes, less than the {least} it must hold (byte {offset})")
        }
    }
}

/// `WireType`'s message. A wire type the metadata uses anywhere (0, 1,
/// 2 or 5) is only wrong for the field whose key is at `offset`.
fn wire_type(part: &Part, wire: &u8, offset: &u64) -> String {
    match wire {
        0 | 1 | 2 | 5 => {
            format!("{part} has wire type {wire} at byte {offset}, not the field's own")
        }
        wire => {
            format!("{part} has wire type {wire} at byte {offset}, which the metadata doesn't use")
        }
    }
}
