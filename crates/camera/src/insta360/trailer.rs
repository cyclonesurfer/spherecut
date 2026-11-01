//! The trailer's tail and index, and one record's payload (insta360.md
//! §3.1 and §3.2; M3 plan 5.1). M3.2's unit (a).
//!
//! Everything is found from the file's end, so a trailer inside an `inst`
//! box (the X5's) and one appended bare read the same. The index is the
//! record whose footer sits just before the tail: a table of 10-byte
//! slots, slot k for record type k (the X5 writes 31, so the index starts
//! at EOF − 388). Each record listed is checked by its own footer, which
//! must repeat its slot's id, format and size; a record that would run
//! into the index or past the file is listed as skipped and never read.
//! The reader reads the tail, the index and the footers, a few hundred
//! bytes whatever the file's size, and allocates nothing for a size it
//! hasn't checked.

use std::io::{self, Read, Seek, SeekFrom};

use spherecut_core::may_block;

use super::limits::{MAX_SLOTS, MAX_TRAILER};
use super::{MAGIC, le32};
use crate::{CameraError, Part};

/// The tail: 32 zeros, the trailer's size, its version and the magic.
const TAIL: u64 = 72;
/// Where in the tail the trailer's size is, then its version, then the
/// magic.
const SIZE_AT: usize = 32;
const VERSION_AT: usize = 36;
const MAGIC_AT: usize = 40;
/// A record's footer: `{u8 format, u8 id, u32le size}`.
const FOOTER: u64 = 6;
/// An index slot: `{u8 id, u8 format, u32le size, u32le offset}`.
const SLOT: u64 = 10;
/// Where a slot's size is, and where a footer's.
const SLOT_SIZE_AT: u64 = 2;
const FOOTER_SIZE_AT: u64 = 2;
/// The only trailer version documented.
const VERSION: u32 = 3;
/// The index's id, which its footer gives.
const INDEX_ID: u8 = 0;

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
    /// The slots whose record would run into the index or past the file:
    /// listed with what they say, counted, and never followed (5.1).
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
    /// Where its slot is in the file: the index's start plus 10 bytes for
    /// each slot before it. On the X5, slot k lists record k, so record
    /// k's slot is at EOF − 388 + 10k. The errors about a record's slot
    /// are at this byte.
    pub slot: u64,
}

impl RecordRef {
    /// Where its footer is in the file.
    fn footer(&self) -> u64 {
        self.offset.saturating_add(u64::from(self.size))
    }

    /// The footer its slot says it has.
    fn footer_bytes(&self) -> [u8; FOOTER as usize] {
        let [s0, s1, s2, s3] = self.size.to_le_bytes();
        [self.format, self.id, s0, s1, s2, s3]
    }
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
/// byte that says so. A slot whose record would run into the index or
/// past the file is listed in [`Trailer::skipped`], not followed.
///
/// The other errors: [`CameraError::FooterMismatch`] for a record's
/// footer that doesn't repeat its slot, at the footer, and for a footer at
/// EOF − 78 that isn't an index's (id 0, format 0: older cameras walk
/// their records from the end with no index, which M3 doesn't read);
/// [`CameraError::BadSize`] for an index that isn't whole slots, at its
/// footer's size; [`CameraError::Truncated`] in [`Part::Index`] for an
/// index that would start before the trailer does, at the trailer's
/// start; and `Truncated` or [`CameraError::Io`] where reading `r` stops
/// short of `len` or fails. The tail's 32 zeros aren't checked.
///
/// It calls [`spherecut_core::may_block`] first, so on the UI thread it
/// panics in a debug build.
pub fn read_trailer<R: Read + Seek>(r: &mut R, len: u64) -> Result<Option<Trailer>, CameraError> {
    may_block("reading an Insta360 file's trailer");
    let Some(tail_at) = len.checked_sub(TAIL) else {
        return Ok(None);
    };
    let mut tail = [0; TAIL as usize];
    read_at(r, tail_at, &mut tail, Part::Tail)?;
    if tail.get(MAGIC_AT..) != Some(&MAGIC[..]) {
        return Ok(None);
    }
    let version = le32(&tail, VERSION_AT);
    if version != VERSION {
        return Err(CameraError::Version {
            found: version,
            offset: tail_at + VERSION_AT as u64,
        });
    }
    let size = u64::from(le32(&tail, SIZE_AT));
    let limit = len.min(MAX_TRAILER);
    if size > limit {
        return Err(CameraError::TooLarge {
            part: Part::Tail,
            size,
            limit,
            offset: tail_at + SIZE_AT as u64,
        });
    }
    let payload_start = len - size;
    // Nothing of the trailer may lie before its start: an index that
    // would is cut short there.
    let cut = CameraError::Truncated {
        part: Part::Index,
        offset: payload_start,
    };
    let footer_at = match tail_at.checked_sub(FOOTER) {
        Some(at) if at >= payload_start => at,
        _ => return Err(cut),
    };
    let mut footer = [0; FOOTER as usize];
    read_at(r, footer_at, &mut footer, Part::Index)?;
    let [format, id, ..] = footer;
    if (format, id) != (0, INDEX_ID) {
        return Err(CameraError::FooterMismatch {
            id: INDEX_ID,
            offset: footer_at,
        });
    }
    let index_size = u64::from(le32(&footer, FOOTER_SIZE_AT as usize));
    let index_limit = MAX_SLOTS as u64 * SLOT;
    if index_size > index_limit {
        return Err(CameraError::TooLarge {
            part: Part::Index,
            size: index_size,
            limit: index_limit,
            offset: footer_at + FOOTER_SIZE_AT,
        });
    }
    if index_size % SLOT != 0 {
        return Err(CameraError::BadSize {
            id: INDEX_ID,
            size: index_size,
            offset: footer_at + FOOTER_SIZE_AT,
        });
    }
    let index_at = match footer_at.checked_sub(index_size) {
        Some(at) if at >= payload_start => at,
        _ => return Err(cut),
    };
    // At most MAX_SLOTS slots, checked above: no allocation for them.
    let mut buf = [0; MAX_SLOTS * SLOT as usize];
    let index = buf.get_mut(..index_size as usize).unwrap_or_default();
    read_at(r, index_at, index, Part::Index)?;

    let mut records = Vec::new();
    let mut skipped = Vec::new();
    let (slots, _) = index.as_chunks::<{ SLOT as usize }>();
    for (k, s) in (0u64..).zip(slots) {
        if s.iter().all(|&b| b == 0) {
            continue;
        }
        let [id, format, z0, z1, z2, z3, o0, o1, o2, o3] = *s;
        let offset = u64::from(u32::from_le_bytes([o0, o1, o2, o3]));
        let rec = RecordRef {
            id,
            format,
            offset: payload_start.saturating_add(offset),
            size: u32::from_le_bytes([z0, z1, z2, z3]),
            slot: index_at + k * SLOT,
        };
        // Its payload and footer must end before the index starts.
        if rec.footer().saturating_add(FOOTER) > index_at {
            skipped.push(rec);
            continue;
        }
        let mut footer = [0; FOOTER as usize];
        read_at(r, rec.footer(), &mut footer, Part::Record(id))?;
        if footer != rec.footer_bytes() {
            return Err(CameraError::FooterMismatch {
                id,
                offset: rec.footer(),
            });
        }
        records.push(rec);
    }
    Ok(Some(Trailer {
        version,
        payload_start,
        records,
        skipped,
    }))
}

/// Reads record `id`'s payload, its footer left off: `Ok(None)` when the
/// trailer `t` lists no record `id`.
///
/// A record over `max` bytes is [`CameraError::TooLarge`] before anything
/// is allocated for it, at its slot's size, and one whose slot runs into
/// the index or past the file ([`Trailer::skipped`]) is
/// [`CameraError::SlotPastEnd`], at its slot. Bytes that end inside the
/// payload are [`CameraError::Truncated`] in [`Part::Record`] at the first
/// byte missing, and a failed seek or read is [`CameraError::Io`] at the
/// byte it was for. When `t` lists `id` twice, the first is read.
///
/// It calls [`spherecut_core::may_block`] first, so on the UI thread it
/// panics in a debug build.
pub fn read_record<R: Read + Seek>(
    r: &mut R,
    t: &Trailer,
    id: u8,
    max: usize,
) -> Result<Option<Vec<u8>>, CameraError> {
    may_block("reading a record of an Insta360 file's trailer");
    let Some(rec) = t.records.iter().find(|rec| rec.id == id) else {
        return match t.skipped.iter().find(|rec| rec.id == id) {
            Some(rec) => Err(CameraError::SlotPastEnd {
                id,
                offset: rec.slot,
            }),
            None => Ok(None),
        };
    };
    let size = u64::from(rec.size);
    let limit = u64::try_from(max).unwrap_or(u64::MAX);
    if size > limit {
        return Err(CameraError::TooLarge {
            part: Part::Record(id),
            size,
            limit,
            offset: rec.slot.saturating_add(SLOT_SIZE_AT),
        });
    }
    // At most `max` bytes, checked above.
    let mut payload = vec![0; rec.size as usize];
    read_at(r, rec.offset, &mut payload, Part::Record(id))?;
    Ok(Some(payload))
}

/// Fills `buf` from byte `at` of `r`. Bytes that end first are
/// [`CameraError::Truncated`] in `part` at the first byte missing; a
/// failed seek or read is [`CameraError::Io`] at the byte it was for.
fn read_at<R: Read + Seek>(
    r: &mut R,
    at: u64,
    buf: &mut [u8],
    part: Part,
) -> Result<(), CameraError> {
    r.seek(SeekFrom::Start(at))
        .map_err(|error| CameraError::Io { offset: at, error })?;
    let mut got = 0;
    while let Some(rest) = buf.get_mut(got..).filter(|rest| !rest.is_empty()) {
        let offset = at.saturating_add(got as u64);
        match r.read(rest) {
            Ok(0) => return Err(CameraError::Truncated { part, offset }),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(CameraError::Io { offset, error }),
        }
    }
    Ok(())
}
