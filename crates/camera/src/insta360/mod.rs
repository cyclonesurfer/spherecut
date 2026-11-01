//! Insta360's `.insv` and `.lrv` files (`docs/formats/insta360.md`; M3
//! plan 5.1 to 5.4): the trailer and its records, the metadata, the
//! calibration strings, and which files make one recording.
//!
//! An X5 file is an MP4 (`ftyp wide mdat moov`) followed by an `inst` box
//! that holds the trailer (insta360.md §3): the records, each ending in a
//! 6-byte footer, then the index, a record listing the others by id,
//! offset and size, whose footer is at EOF − 78, and last the 72-byte
//! tail with the trailer's size, its version and [`MAGIC`].
//! [`read_trailer`] reads the tail, the index and each record's footer,
//! and [`read_record`] one record's payload. The parsers read a payload
//! already in memory: [`parse_metadata`] record 1, [`thumbnail`] record 2,
//! [`imu_span`] record 3 (its length and stamps only; M4 parses the IMU)
//! and [`parse_exposure`] record 4. [`calib`] parses the calibration
//! strings the metadata holds and prints them back. [`companions`] names
//! a recording's other files from a file's name alone, and
//! [`verify_companion`] checks a master and its LRV by their bytes.
//!
//! Every function that reads calls [`spherecut_core::may_block`] first, so
//! one called on the UI thread panics in a debug build (D34);
//! [`companions`] and the parsers read nothing. The [`limits`] bound what
//! a file can make them read or keep, in D15's style.

pub mod calib;
mod companions;
mod metadata;
mod records;
mod trailer;

pub use companions::{
    CompanionError, Companions, InsvName, NameRole, Pairing, Recording, Side, companions,
    verify_companion,
};
pub use metadata::{
    CalibStrings, Calibrations, FileGroup, FrameSpan, LensProfiles, Metadata, Window,
    parse_metadata,
};
pub use records::{Exposure, ImuSpan, Nv12Image, imu_span, parse_exposure, thumbnail};
pub use trailer::{RecordRef, Trailer, read_record, read_trailer};

/// The 32 ASCII bytes an Insta360 trailer ends with (insta360.md §3.1).
pub const MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";

/// What a file can make the readers read or keep (M3 plan 5.1, in D15's
/// style). A size over its limit is an error before anything is
/// allocated for it.
pub mod limits {
    /// The largest trailer read, whatever its tail says: 1 GiB, and never
    /// more than the file holds. Pat's X5 files have 10 MB to 13 MB, the
    /// IMU record growing with the length.
    pub const MAX_TRAILER: u64 = 1 << 30;
    /// The largest metadata record parsed: 1 MiB. The X5's is about 4 KB.
    pub const MAX_METADATA: usize = 1 << 20;
    /// The longest string the metadata keeps: 4 KiB. The X5's longest,
    /// its V6 calibration string, is 518 bytes.
    pub const MAX_STRING: usize = 4 << 10;
    /// The most lens profiles the metadata keeps of each kind, field
    /// 145's polynomials (its item 1) and fields of view (item 2): 64.
    /// The X5 lists 6 of each, one for each accessory. An item can be 2
    /// bytes where what it is kept as takes 48, so without the cap a
    /// 1 MiB field would keep half a million.
    pub const MAX_PROFILES: usize = 64;
    /// The most index slots read: 256, one for each id a slot's byte can
    /// name, so that no trailer is refused for the id of a record it
    /// lists (slot k lists record k: an app-trimmed file's TimeMap, 128,
    /// would need 129). The X5 has 31, the X6 59. The reader holds them
    /// on the stack, 2.5 KiB.
    pub const MAX_SLOTS: usize = 256;
}

/// The little-endian u32 at byte `at` of `b`, or 0 past its end, which
/// its callers' lengths rule out.
fn le32(b: &[u8], at: usize) -> u32 {
    b.get(at..)
        .and_then(|b| b.first_chunk::<4>())
        .map_or(0, |w| u32::from_le_bytes(*w))
}

/// The little-endian u64 at byte `at` of `b`, or 0 past its end, which
/// its callers' lengths rule out.
fn le64(b: &[u8], at: usize) -> u64 {
    b.get(at..)
        .and_then(|b| b.first_chunk::<8>())
        .map_or(0, |w| u64::from_le_bytes(*w))
}
