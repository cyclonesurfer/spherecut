//! The trailer's tail and index, and one record's payload (insta360.md
//! §3.1 and §3.2; M3 plan 5.1). M3.2's unit (a).

#![expect(
    unused_variables,
    reason = "the bodies are todo!() until M3.2's unit (a) writes them"
)]

use std::io::{Read, Seek};

use crate::CameraError;

/// An Insta360 file's trailer, as its tail and index give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    /// The tail's version: 3 on every camera documented.
    pub version: u32,
    /// Where the trailer starts in the file: the file's length less the
    /// trailer's size E from the tail (§3.1). On the X5 the `inst` box's
    /// 8-byte header ends there.
    pub payload_start: u64,
    /// The records the index lists, in slot order, each inside the trailer
    /// and its footer checked against its slot.
    pub records: Vec<RecordRef>,
    /// The slots whose record would run past the trailer: listed with
    /// what they say, counted, and never followed (5.1).
    pub skipped: Vec<RecordRef>,
}

/// One record as its index slot gives it (a slot is `{u8 id, u8 format,
/// u32le size, u32le offset}`, §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordRef {
    /// The record's type (§3.3): 1 the metadata, 2 the thumbnail, 3 the
    /// IMU, 4 the exposure, and so on.
    pub id: u8,
    /// How its payload is written: 0 binary, 1 protobuf, 2 JSON.
    pub format: u8,
    /// Where its payload starts in the file: [`Trailer::payload_start`]
    /// plus the slot's offset.
    pub offset: u64,
    /// Its payload's size, the 6-byte footer after it not counted.
    pub size: u32,
}

/// Reads the trailer of the `len`-byte file that `r` holds: the 72-byte
/// tail, then the index whose footer is at EOF − 78, then each listed
/// record's footer, which must say what its slot does. It reads no
/// payload but the index's, and nothing before the trailer.
///
/// `Ok(None)` when the file has no trailer: it is shorter than the tail,
/// or its last 32 bytes aren't [`super::MAGIC`]. A trailer whose size is
/// over `min(len, MAX_TRAILER)` ([`super::limits`]), whose index has more
/// than `MAX_SLOTS` slots, or whose version isn't 3 is an error at the
/// byte that says so. A slot whose record would run past the trailer is
/// listed in [`Trailer::skipped`], not followed.
///
/// It calls [`spherecut_core::may_block`] first, so on the UI thread it
/// panics in a debug build.
pub fn read_trailer<R: Read + Seek>(r: &mut R, len: u64) -> Result<Option<Trailer>, CameraError> {
    todo!()
}

/// Reads record `id`'s payload, its footer left off: `Ok(None)` when the
/// trailer `t` lists no record `id`.
///
/// A record over `max` bytes is [`CameraError::TooLarge`] before anything
/// is allocated for it, and one whose slot runs past the trailer
/// ([`Trailer::skipped`]) is [`CameraError::SlotPastEnd`].
///
/// It calls [`spherecut_core::may_block`] first, so on the UI thread it
/// panics in a debug build.
pub fn read_record<R: Read + Seek>(
    r: &mut R,
    t: &Trailer,
    id: u8,
    max: usize,
) -> Result<Option<Vec<u8>>, CameraError> {
    todo!()
}
