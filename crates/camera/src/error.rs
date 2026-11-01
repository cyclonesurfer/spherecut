//! What can be wrong with a camera's bytes, and where.

use std::fmt;
use std::io;

use thiserror::Error;

use crate::insta360::calib::Generation;

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
    /// An index slot whose record runs past the trailer. `read_trailer`
    /// lists such a slot in `Trailer::skipped` and never follows it; this
    /// is what reading its record gives. `offset` is the slot's.
    #[error("record {id}'s index slot at byte {offset} runs past the trailer")]
    SlotPastEnd { id: u8, offset: u64 },
    /// A record's footer that doesn't say what its index slot does: its
    /// id, format or size differ. `offset` is the footer's.
    #[error("record {id}'s footer at byte {offset} doesn't match its index slot")]
    FooterMismatch { id: u8, offset: u64 },
    /// A varint of more than ten bytes, or past 64 bits.
    #[error("{part} has a bad varint at byte {offset}")]
    BadVarint { part: Part, offset: u64 },
    /// A protobuf wire type the metadata doesn't use: a group's (3 or 4),
    /// or 6 or 7, which protobuf doesn't define.
    #[error("{part} has wire type {wire} at byte {offset}, which the metadata doesn't use")]
    WireType { part: Part, wire: u8, offset: u64 },
    /// More bytes than a limit allows (`insta360::limits`), or than the
    /// caller asked for. Nothing was allocated for them.
    #[error("{part} is {size} bytes, over its limit of {limit} (byte {offset})")]
    TooLarge {
        part: Part,
        size: u64,
        limit: u64,
        offset: u64,
    },
    /// A trailer version other than 3, the only one documented.
    #[error("the trailer's version is {found}, not 3 (byte {offset})")]
    Version { found: u32, offset: u64 },
    /// A record whose size its layout can't have: an exposure record that
    /// isn't whole 16-byte items, or a thumbnail that isn't its header and
    /// the planes its header gives.
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
    /// not a decimal number, a lens count other than 2, a packed word of
    /// another generation, or a block whose canvas or lens type isn't the
    /// other's. `why` says which.
    #[error("the {generation} calibration string's token at byte {offset} {why}")]
    BadToken {
        generation: Generation,
        why: &'static str,
        offset: u64,
    },
    /// Reading the file failed.
    #[error("I/O error at byte {offset}: {error}")]
    Io { offset: u64, error: io::Error },
}
