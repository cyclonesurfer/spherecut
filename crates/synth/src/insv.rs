//! The synthetic camera's files (M3 plan 7.2): an Insta360-like trailer
//! appended to an MP4 synth writes, and the calibration strings printed
//! from a [`CalibTruth`].
//!
//! This is synth's own writer and printer, from `docs/formats/insta360.md`
//! sections 3 and 4 and the bytes of Pat's samples, sharing no code with
//! `spherecut_camera`'s reader and parser, which it is there to check
//! (4.1 rule 2): a bug a shared writer mirrored would go unseen. M3.2
//! writes the trailer and prints the strings; M3.5 adds `write_insv`, its
//! `InsvSpec` and the LRV, which call [`append_trailer`] once
//! `Mp4Writer` has written the tracks.
//!
//! The trailer is laid out as the X5's (§3.1 and §3.2): records in
//! descending id order, each its payload and a 6-byte footer `{u8 format,
//! u8 id, u32le size}`, every one but the metadata (1) and record 10
//! starting on a 128 KiB boundary of the file with zeros between; the
//! metadata last, ending at the index; the index of 31 10-byte slots
//! `{u8 id, u8 format, u32le size, u32le offset}` with its own footer
//! (id 0), so that it starts at EOF − 388; and the 72-byte tail: 32
//! zeros, the trailer's size E, the version 3 and the magic. The `inst`
//! box around it has size E + 8.

#![expect(
    unused_variables,
    reason = "the bodies are todo!() until M3.2's unit (d) writes them"
)]

use std::path::Path;

use spherecut_core::FrameRate;

use crate::lens::{CalibTruth, Planted};
use crate::media::SynthError;

/// What a synthetic `.insv` gets wrong on purpose (7.2). The trailer's
/// faults, [`InsvFault::in_trailer`], are planted by [`inst_box`] (M3.2);
/// the others are the MP4's, which `write_insv` plants (M3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InsvFault {
    /// The tail's size E is more than the file holds, as when a copy lost
    /// the trailer's start.
    TruncatedTrailer,
    /// One index slot's record runs past the trailer's end.
    IndexSlotPastEnd,
    /// The magic's last byte changed: no trailer at all, inside an `inst`
    /// box that says there is one.
    BadMagic,
    /// The V6 string with one token too few.
    WrongTokenCount,
    /// A varint in the metadata of eleven bytes.
    VarintOverflow,
    /// Lens 1's track one frame shorter than lens 0's.
    LensOneFrameShort,
    /// The tracks' first presentation times differ.
    StartPtsDiffer,
    /// The tracks' keyframes (`stss`) differ.
    StssDiffer,
    /// The tracks' frame rates differ.
    RateDiffer,
    /// The metadata without its calibration strings.
    NoCalibration,
}

impl InsvFault {
    /// Every fault, in 7.2's order.
    pub fn all() -> impl Iterator<Item = InsvFault> {
        [
            InsvFault::TruncatedTrailer,
            InsvFault::IndexSlotPastEnd,
            InsvFault::BadMagic,
            InsvFault::WrongTokenCount,
            InsvFault::VarintOverflow,
            InsvFault::LensOneFrameShort,
            InsvFault::StartPtsDiffer,
            InsvFault::StssDiffer,
            InsvFault::RateDiffer,
            InsvFault::NoCalibration,
        ]
        .into_iter()
    }

    /// Whether the trailer holds this fault, so that [`inst_box`] plants
    /// it: `TruncatedTrailer` to `VarintOverflow`, and `NoCalibration`.
    pub fn in_trailer(self) -> bool {
        todo!()
    }
}

/// A protobuf field as synth writes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub number: u32,
    pub value: Value,
}

/// A field's value, by the wire type it is written with.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Wire type 0.
    Varint(u64),
    /// Wire type 1, little-endian: a double's bits, or a fixed64.
    Fixed64(u64),
    /// Wire type 5, little-endian.
    Fixed32(u32),
    /// Wire type 2: a string or bytes.
    Bytes(Vec<u8>),
    /// Wire type 2: a message, written by [`encode`].
    Message(Vec<Field>),
}

impl Field {
    pub fn varint(number: u32, v: u64) -> Field {
        Field {
            number,
            value: Value::Varint(v),
        }
    }

    pub fn double(number: u32, v: f64) -> Field {
        Field {
            number,
            value: Value::Fixed64(v.to_bits()),
        }
    }

    pub fn string(number: u32, s: &str) -> Field {
        Field {
            number,
            value: Value::Bytes(s.as_bytes().to_vec()),
        }
    }

    pub fn message(number: u32, fields: Vec<Field>) -> Field {
        Field {
            number,
            value: Value::Message(fields),
        }
    }
}

/// `fields` in protobuf's wire format, in the order given, with synth's
/// own varints: each key `number << 3 | wire type`, and a length before
/// bytes and messages.
pub fn encode(fields: &[Field]) -> Vec<u8> {
    todo!()
}

/// An item of record 3 in the X5's raw form (field 62 = 1): 20 bytes,
/// `{u64 t_us, u16 × 6}`, accelerometer first (§3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImuItem {
    pub t_us: u64,
    pub accel: [u16; 3],
    pub gyro: [u16; 3],
}

/// An item of record 4: 16 bytes, `{u64 t_us, f64 seconds}`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExposureItem {
    pub t_us: u64,
    pub seconds: f64,
}

/// Record 2's picture: written after a 40-byte header of little-endian
/// u32s, 1, the record's size, 1, 0, the width, the height and four zeros,
/// as Pat's files have it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// `width × height` bytes of luma.
    pub y: Vec<u8>,
    /// `width × height / 2` bytes of Cb and Cr, interleaved.
    pub uv: Vec<u8>,
}

/// A record written as given, which camera lists by its size only (9,
/// 10, 11, 22, 28 and 29 on the X5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    pub id: u8,
    /// 0 binary, 1 protobuf, 2 JSON.
    pub format: u8,
    pub payload: Vec<u8>,
}

/// The trailer's records, as synth writes them (§3.3 and §3.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Records {
    /// Record 1, the metadata, in the order written.
    pub metadata: Vec<Field>,
    /// Record 2, or none, as the X5's 4K120 mode has none.
    pub thumbnail: Option<Thumbnail>,
    /// Record 3.
    pub imu: Vec<ImuItem>,
    /// Record 4, one item a frame.
    pub exposure: Vec<ExposureItem>,
    /// The other records.
    pub others: Vec<RawRecord>,
}

impl Records {
    /// An X5-like trailer's records for `frames` frames at `rate`: the
    /// serial `SYNTH00001` (never Pat's), the model `Insta360 X5`,
    /// firmware `v1.1.22_build1`, the window from `cals`' canvas and dst,
    /// fields 79, 80, 131 and 136 as Pat's masters have them, each of
    /// `cals` printed ([`print()`]) into its generation's two fields, an
    /// exposure item a frame and a 1 kHz IMU over the same span, and no
    /// thumbnail.
    pub fn x5_like(
        cals: &[CalibTruth],
        planted: &Planted,
        frames: u32,
        rate: FrameRate,
    ) -> Records {
        todo!()
    }
}

/// The alignment of the X5's records in the file: 128 KiB (§3.2).
pub const X5_ALIGN: u64 = 128 << 10;

/// Where [`inst_box`] put each part of a trailer, in the file's bytes,
/// for a test to hold a reader to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailerLayout {
    /// Where the trailer starts: the `inst` box's offset plus its 8-byte
    /// header.
    pub payload_start: u64,
    /// The trailer's size E, which the tail holds: from `payload_start`
    /// to the file's end.
    pub size: u64,
    /// The index's slots that list a record, in slot order.
    pub slots: Vec<Slot>,
}

/// One index slot as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub id: u8,
    pub format: u8,
    /// Where the record's payload starts in the file.
    pub offset: u64,
    /// Its payload's size, its footer not counted.
    pub size: u32,
}

/// The `inst` box that holds `records`' trailer, for a file whose bytes
/// before it end at offset `at`: its 8-byte header, then the trailer laid
/// out as the module says, with each record but 1 and 10 on an `align`
/// boundary of the file (`X5_ALIGN` for the X5's; 1 for none). A trailer
/// fault ([`InsvFault::in_trailer`]) is planted in it; the MP4's faults
/// leave it as it would be.
pub fn inst_box(
    records: &Records,
    at: u64,
    align: u64,
    fault: Option<InsvFault>,
) -> (Vec<u8>, TrailerLayout) {
    todo!()
}

/// Writes `mp4`'s bytes and then [`inst_box`] at the X5's alignment to
/// `out`, whole or not at all (`core::fs::TempFile`), so `out` exists
/// only once it is complete. `write_insv` (M3.5) writes the tracks with
/// `Mp4Writer` to a file of its own and calls
/// `append_trailer(&tracks, &dir.join("VID_…_00_001.insv"), &spec.records,
/// spec.fault)`, and the same for the LRV with its records.
pub fn append_trailer(
    mp4: &Path,
    out: &Path,
    records: &Records,
    fault: Option<InsvFault>,
) -> Result<TrailerLayout, SynthError> {
    todo!()
}

/// `cal` as its generation's calibration string (insta360.md §4.1): the
/// lens count 2, each block's tokens in the generation's order with its
/// canvas `w h` and lens type, then the packed word (the generation's
/// code in bits 16 and up, `cal.body` in bits 8 to 15). Each number is
/// written at the X5's precision ("Pat's X5 samples"): ξ and t to 6
/// decimals, focal, centre and angles to 3, the distortion terms to 8,
/// so that printing Pat's numbers gives Pat's strings.
///
/// `planted` names the conventions the strings are printed under. M3.2
/// prints `cal`'s stored numbers as they are, so no token depends on it
/// yet; the files M3.5 writes print a rig's truth through it (7.2).
pub fn print(cal: &CalibTruth, planted: &Planted) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    /// Printing Pat's V3 and V6 numbers ("Pat's X5 samples") gives
    /// Pat's strings, token for token.
    #[test]
    #[ignore = "M3.2 unit (d)"]
    fn print_gives_pats_strings_back() {
        todo!()
    }

    /// The footers, the 128 KiB boundaries, the 31-slot index at
    /// EOF − 388, the tail and `inst`'s size E + 8, read back by hand.
    #[test]
    #[ignore = "M3.2 unit (d)"]
    fn the_trailer_is_laid_out_as_the_x5s() {
        todo!()
    }
}
