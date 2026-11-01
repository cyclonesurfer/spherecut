//! The synthetic camera's files (M3 plan 7.2): an Insta360-like trailer
//! appended to an MP4 synth writes, and the calibration strings printed
//! from a [`CalibTruth`].
//!
//! This is synth's own writer and printer, from `docs/formats/insta360.md`
//! sections 3 and 4 and the bytes of Pat's samples, sharing no code with
//! `spherecut_camera`'s reader and parser, which it is there to check
//! (4.1 rule 2): a bug a shared writer mirrored would go unseen. M3.2
//! writes the trailer, gives the records of a master and of its LRV and
//! prints the strings; M3.5 adds `write_insv` and its `InsvSpec`, which
//! write both files and call [`append_trailer`] once `Mp4Writer` has
//! written the tracks.
//!
//! The trailer is laid out as the X5's (§3.1 and §3.2): records in
//! descending id order, each its payload and then six bytes giving its
//! format, id and payload length, every one but the metadata (1) and
//! record 10 starting on a 128 KiB boundary of the file; the metadata
//! last, ending at the index; the index, a table of 31 ten-byte slots
//! (the id before the format there, then the length and the offset from
//! the trailer's start) closed by six bytes of its own as record 0, so
//! that it starts at EOF − 388; and the file's last 72 bytes: 32 zeros,
//! the trailer's size E, version 3 and the magic. The `inst` box around
//! it has size E + 8. The X5 itself leaves more room between its records
//! than the next boundary (Pat's files skip a further 128 KiB after most
//! of them), and its gaps are mostly zeros, with stray bytes in every one
//! of Pat's 14 files, most of them in the gaps before records 9, 4 and 2;
//! a reader finds the records by the index either way, so synth packs
//! them to the next boundary and fills the gaps with zeros.

use std::cmp::Reverse;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use spherecut_core::FrameRate;
use spherecut_core::fs::TempFile;

use crate::lens::{CalibTruth, Generation, Planted};
use crate::media::SynthError;

/// What a synthetic `.insv` gets wrong on purpose (7.2). The trailer's
/// faults, [`InsvFault::in_trailer`], are planted by [`inst_box`] (M3.2);
/// the others are the MP4's, which `write_insv` plants (M3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InsvFault {
    /// The tail's size E is more than the file holds, as when a copy lost
    /// the trailer's start: the file's length and a byte.
    TruncatedTrailer,
    /// One index slot's record runs past the trailer's end: record 1's
    /// slot gives the trailer's size E as its offset, so that the record
    /// would start at the file's end.
    IndexSlotPastEnd,
    /// The magic's last byte changed: no trailer at all, inside an `inst`
    /// box that says there is one.
    BadMagic,
    /// Each calibration string with one token too few: its last, the
    /// packed word, dropped from the working string and the original of
    /// every generation the metadata holds, so that no reader finds a
    /// string of another generation to fall back on.
    WrongTokenCount,
    /// A varint in the metadata of eleven bytes: a field 7 put first,
    /// whose value is ten bytes with their top bit set and a zero.
    VarintOverflow,
    /// Lens 1's track one frame shorter than lens 0's.
    LensOneFrameShort,
    /// The tracks' first presentation times differ.
    StartPtsDiffer,
    /// The tracks' keyframes (`stss`) differ.
    StssDiffer,
    /// The tracks' frame rates differ.
    RateDiffer,
    /// The metadata without its calibration strings: every generation's
    /// working string and original left out.
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
        match self {
            InsvFault::TruncatedTrailer
            | InsvFault::IndexSlotPastEnd
            | InsvFault::BadMagic
            | InsvFault::WrongTokenCount
            | InsvFault::VarintOverflow
            | InsvFault::NoCalibration => true,
            InsvFault::LensOneFrameShort
            | InsvFault::StartPtsDiffer
            | InsvFault::StssDiffer
            | InsvFault::RateDiffer => false,
        }
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
/// bytes and messages. Every varint takes its fewest bytes. A number
/// protobuf doesn't allow (0, or past 2²⁹ − 1) is written as given, for
/// a test of a reader's refusal.
pub fn encode(fields: &[Field]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in fields {
        put_field(&mut out, f);
    }
    out
}

/// Appends `v` as a varint: 7 bits a byte, the lowest first, each byte
/// but the last with its top bit set.
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Appends field `f`: its key, then its value as its wire type writes it.
fn put_field(out: &mut Vec<u8>, f: &Field) {
    let wire = match f.value {
        Value::Varint(_) => 0,
        Value::Fixed64(_) => 1,
        Value::Bytes(_) | Value::Message(_) => 2,
        Value::Fixed32(_) => 5,
    };
    put_varint(out, (u64::from(f.number) << 3) | wire);
    match &f.value {
        Value::Varint(v) => put_varint(out, *v),
        Value::Fixed64(v) => out.extend(v.to_le_bytes()),
        Value::Fixed32(v) => out.extend(v.to_le_bytes()),
        Value::Bytes(b) => {
            put_varint(out, b.len() as u64);
            out.extend(b);
        }
        Value::Message(m) => {
            let b = encode(m);
            put_varint(out, b.len() as u64);
            out.extend(b);
        }
    }
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
    /// Record 3, written when it holds an item.
    pub imu: Vec<ImuItem>,
    /// Record 4, written when it holds an item: in [`Records::x5_like`]'s,
    /// an item a frame and a few more before and after them.
    pub exposure: Vec<ExposureItem>,
    /// The other records, written as given.
    pub others: Vec<RawRecord>,
}

/// The serial synth's files give (field 1), never Pat's.
const SERIAL: &str = "SYNTH00001";
/// The model and firmware they give (fields 2 and 3), Pat's X5's.
const MODEL: &str = "Insta360 X5";
const FIRMWARE: &str = "v1.1.22_build1";
/// The camera's clock at the first frame, in µs (field 24): a second
/// after it started. The exposure and the IMU are stamped on the same
/// clock, and both start before it, as the X5's do.
const FIRST_FRAME_US: u64 = 1_000_000;
/// Each frame's shutter time in record 4, in seconds.
const SHUTTER_S: f64 = 1.0 / 500.0;
/// The IMU's period: 1 kHz, about the X5's (§3.4).
const IMU_PERIOD_US: usize = 1000;
/// An IMU item at rest, offset-binary around 32768: 1 g on the third
/// axis at the X5's 32 g full scale (field 65), 1024 counts a g, and no
/// rotation.
const ACCEL_AT_REST: [u16; 3] = [32768, 32768, 32768 + 1024];
const GYRO_AT_REST: [u16; 3] = [32768; 3];
/// The metadata fields of the calibration strings, the working string and
/// the original of each generation (§3.5): V1's 5 and 17, V2's 53 and
/// 55, V3's 54 and 56 and V6's 111 and 112.
const CALIBRATION_FIELDS: [u32; 8] = [5, 17, 53, 55, 54, 56, 111, 112];
/// The exposure items the X5 writes before the first frame's and past
/// the last frame's, by its rate in whole frames a second, rounded up:
/// on Pat's files (insta360.md, "Pat's X5 samples", record 4 and the
/// IMU against the frames), 5 and 2 at 25 fps (take 005), 5 and 3 at
/// 29.97 (004 and 008 to 010), 6 and 4 at 59.94 (006) and 10 and 6 at
/// 119.88 (007). A rate between two rows takes the row above it, and
/// one past 120 the last.
const X5_EXPOSURE_MARGINS: [(u64, u64, u64); 4] =
    [(25, 5, 2), (30, 5, 3), (60, 6, 4), (120, 10, 6)];

/// The fields of a generation's working string and its original.
fn string_fields(g: Generation) -> [u32; 2] {
    match g {
        Generation::V3 => [54, 56],
        Generation::V6 => [111, 112],
    }
}

/// Field 136's value for a generation in use (§3.5): 3 for V3, 4 for V6.
fn offset_version(g: Generation) -> u64 {
    match g {
        Generation::V3 => 3,
        Generation::V6 => 4,
    }
}

/// [`X5_EXPOSURE_MARGINS`]' items before the first frame and past the
/// last at `rate`.
fn exposure_margins(rate: FrameRate) -> (u64, u64) {
    let r = rate.as_rational();
    let whole = |n: i64| u64::try_from(n).unwrap_or(1);
    let fps = whole(r.num()).div_ceil(whole(r.den()).max(1));
    let row = X5_EXPOSURE_MARGINS
        .iter()
        .find(|&&(up_to, ..)| fps <= up_to)
        .or(X5_EXPOSURE_MARGINS.last());
    row.map_or((0, 0), |&(_, before, after)| (before, after))
}

impl Records {
    /// An X5-like master's records for `frames` frames a track at `rate`,
    /// as Pat's masters have them ("Pat's X5 samples"), but for the
    /// serial, `SYNTH00001`, never Pat's.
    ///
    /// The metadata, in field order as the X5 writes it: the serial, the
    /// model `Insta360 X5` and firmware `v1.1.22_build1` (1 to 3); the
    /// first frame's time on the camera's clock (24); the window from
    /// `cals`' first canvas and dst (27); the IMU's raw form, its pts and
    /// its ranges (62, 64, 65); 79, 80, 131 and 132 as Pat's masters
    /// have them; field 98 with the first frame's time as item 1 and
    /// `frames + 1` as item 5, the count the X5 gives a master, and field
    /// 93 that count's length in whole ms, rounded down, ⌊(frames + 1) ×
    /// 1000 / rate⌋; each of `cals` printed ([`print()`]) into its
    /// generation's two fields; and field 136 naming the generation in
    /// use, the newest `cals` holds (4 for V6, else 3 for V3, else none).
    ///
    /// Record 4: an item a frame, a frame apart, with the items
    /// [`X5_EXPOSURE_MARGINS`] gives at `rate` before the first frame's,
    /// whose stamp is field 24's, and past the last frame's, as the X5
    /// writes it: so field 24's frame is item 5 at 29.97 fps, not item 0,
    /// and the record has 122 items for clip 1's 114 frames. Record 3: a
    /// 1 kHz IMU at rest over record 4's span, as the X5's runs from
    /// within 17 ms of record 4's first item (92 to 190 ms before field
    /// 24's on Pat's files, "Pat's X5 samples" again) to within 48 ms of
    /// its last. No thumbnail.
    ///
    /// `cals` holds one calibration of each generation at most, which a
    /// debug build asserts, or none. At a rate under 5 fps, which no
    /// camera records at, the items before the first frame's would start
    /// before the camera's clock does, and start at 0 instead.
    pub fn x5_like(
        cals: &[CalibTruth],
        planted: &Planted,
        frames: u32,
        rate: FrameRate,
    ) -> Records {
        debug_assert!(
            Generation::all().all(|g| cals.iter().filter(|c| c.generation() == g).count() <= 1),
            "x5_like takes one calibration of each generation at most"
        );
        let r = rate.as_rational();
        let (num, den) = (i128::from(r.num()), i128::from(r.den()));
        // `n` frames' time in µs, to the nearest.
        let us = |n: u64| {
            let t = (i128::from(n) * 1_000_000 * den + num / 2) / num;
            u64::try_from(t).unwrap_or(u64::MAX)
        };
        // The count field 98 gives a master, and its length in ms, rounded
        // down.
        let counted = u64::from(frames) + 1;
        let length_ms = u64::try_from(i128::from(counted) * 1000 * den / num).unwrap_or(u64::MAX);
        // Item `before` is the first frame's, at field 24's time.
        let (before, after) = exposure_margins(rate);
        let origin = FIRST_FRAME_US.saturating_sub(us(before));
        let exposure: Vec<ExposureItem> = (0..before + u64::from(frames) + after)
            .map(|i| ExposureItem {
                t_us: origin.saturating_add(us(i)),
                seconds: SHUTTER_S,
            })
            .collect();
        let imu = match (exposure.first(), exposure.last()) {
            (Some(first), Some(last)) => (first.t_us..=last.t_us)
                .step_by(IMU_PERIOD_US)
                .map(|t_us| ImuItem {
                    t_us,
                    accel: ACCEL_AT_REST,
                    gyro: GYRO_AT_REST,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mut metadata = vec![
            Field::string(1, SERIAL),
            Field::string(2, MODEL),
            Field::string(3, FIRMWARE),
            Field::varint(24, FIRST_FRAME_US),
            Field::varint(62, 1),
            Field::varint(64, 2),
            Field::message(65, vec![Field::varint(1, 32), Field::varint(2, 2000)]),
            Field::varint(79, 2),
            Field::varint(80, 1),
            Field::varint(93, length_ms),
            Field::message(
                98,
                vec![Field::varint(1, FIRST_FRAME_US), Field::varint(5, counted)],
            ),
            Field::varint(131, 4),
            Field::varint(132, 2),
        ];
        let in_use = [Generation::V6, Generation::V3]
            .into_iter()
            .find(|&g| cals.iter().any(|c| c.generation() == g));
        if let Some(g) = in_use {
            metadata.push(Field::varint(136, offset_version(g)));
        }
        if let Some(cal) = cals.first() {
            let (src, dst) = (cal.canvas[1], cal.dst);
            let window = [src, src, dst, dst, 0, 0]
                .into_iter()
                .zip(1..)
                .map(|(v, n)| Field::varint(n, u64::from(v)))
                .collect();
            metadata.push(Field::message(27, window));
        }
        for cal in cals {
            let s = print(cal, planted);
            for n in string_fields(cal.generation()) {
                metadata.push(Field::string(n, &s));
            }
        }
        metadata.sort_by_key(|f| f.number);
        Records {
            metadata,
            thumbnail: None,
            imu,
            exposure,
            others: Vec::new(),
        }
    }

    /// The records of the LRV beside a master whose records are `master`,
    /// for an LRV of `frames` frames, as Pat's X5 writes them ("Pat's X5
    /// samples"): every record but the metadata is the master's, the
    /// exposure record (4) among them whatever the LRV's length (the
    /// 5.7K60 take's LRV has 95 of its master's 190 frames and its 200
    /// exposure items), and the metadata is the master's with the LRV's
    /// own fields where the master has them: no 79 or 80, 131 and 132 1
    /// (one stream, H.264), and item 5 of field 98 the LRV's `frames`.
    /// Fields 93 and 98's item 1 stay the master's, as on the X5. Of the
    /// LRV's other fields of its own, 26's index, 44 and 135, `x5_like`
    /// writes none.
    pub fn x5_lrv(master: &Records, frames: u32) -> Records {
        let mut lrv = master.clone();
        lrv.metadata.retain(|f| !matches!(f.number, 79 | 80));
        for f in &mut lrv.metadata {
            match (f.number, &mut f.value) {
                (131 | 132, v) => *v = Value::Varint(1),
                (98, Value::Message(items)) => {
                    for item in items.iter_mut().filter(|item| item.number == 5) {
                        item.value = Value::Varint(u64::from(frames));
                    }
                }
                _ => {}
            }
        }
        lrv
    }

    /// Prints `original` into its generation's original field (56 or 112)
    /// in place of what is there, so that the original copy differs from
    /// the working string (54 or 111), as an accessory mode might make it
    /// (insta360.md Q9) and as no file seen has it: the format notes' two
    /// and Pat's 14 hold the two equal (§4.2). With no original of that
    /// generation, it is added where field order puts it. For a test that
    /// tells the two apart.
    pub fn plant_original(&mut self, original: &CalibTruth, planted: &Planted) {
        let [_, field] = string_fields(original.generation());
        let value = Field::string(field, &print(original, planted));
        match self.metadata.iter_mut().find(|f| f.number == field) {
            Some(f) => *f = value,
            None => {
                let at = self.metadata.partition_point(|f| f.number < field);
                self.metadata.insert(at, value);
            }
        }
    }
}

/// The alignment of the X5's records in the file: 128 KiB (§3.2).
pub const X5_ALIGN: u64 = 128 << 10;

/// The 32 ASCII bytes the trailer ends with (insta360.md §3.1).
const MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";
/// The version at EOF − 36, 3 on every camera the format notes describe
/// (§3.1).
const VERSION: u32 = 3;
/// The `inst` box's header: its size, big-endian, and its type.
const INST_HEADER: usize = 8;
/// The bytes after each record's payload (§3.2): the format (0 binary, 1
/// protobuf, 2 JSON), then the id, then the payload's length as a
/// little-endian u32.
const FOOTER: usize = 6;
/// One slot of the index (§3.2), ten bytes: the id, then the format (the
/// footer's two the other way round), then the payload's length and its
/// offset from the trailer's start, each a little-endian u32.
const SLOT: usize = 10;
/// The X5's index: 31 slots, slot k for record k.
const X5_SLOTS: usize = 31;
/// The file's last 72 bytes (§3.1): zeros from EOF − 72, the trailer's
/// size E at EOF − 40, the version at EOF − 36 and the magic from
/// EOF − 32.
const TAIL: usize = 72;
/// Record 2's header before its planes.
const THUMBNAIL_HEADER: usize = 40;

/// Where [`inst_box`] put each part of a trailer, in the file's bytes,
/// for a test to hold a reader to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailerLayout {
    /// Where the trailer starts: the `inst` box's offset plus its 8-byte
    /// header.
    pub payload_start: u64,
    /// The trailer's size E: from `payload_start` to the file's end. The
    /// tail holds it, but under [`InsvFault::TruncatedTrailer`], which has
    /// the tail say more.
    pub size: u64,
    /// Where the index starts: EOF − 388 for the X5's 31 slots.
    pub index: u64,
    /// The index's slots that list a record, in slot order, as the records
    /// are in the file: [`InsvFault::IndexSlotPastEnd`] changes record 1's
    /// slot in the file, not here.
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

/// `n` as the trailer's u32s hold it: past their range, which no
/// synthetic file comes near, it saturates.
fn u32_of(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// `records` as the trailer holds them, each its id, format and payload,
/// in the order the X5 writes them: by id, highest first, two of one id
/// in the order given. The metadata's faults are planted here.
fn laid_out(records: &Records, fault: Option<InsvFault>) -> Vec<RawRecord> {
    let calibration = |f: &Field| CALIBRATION_FIELDS.contains(&f.number);
    let mut metadata = records.metadata.clone();
    match fault {
        Some(InsvFault::WrongTokenCount) => {
            for f in metadata.iter_mut().filter(|f| calibration(f)) {
                if let Value::Bytes(s) = &mut f.value
                    && let Some(last) = s.iter().rposition(|&b| b == b'_')
                {
                    s.truncate(last);
                }
            }
        }
        Some(InsvFault::NoCalibration) => metadata.retain(|f| !calibration(f)),
        _ => {}
    }
    let mut meta = Vec::new();
    if fault == Some(InsvFault::VarintOverflow) {
        // Field 7, a varint, whose value runs to eleven bytes.
        put_varint(&mut meta, 7 << 3);
        meta.extend([0x80; 10]);
        meta.push(0);
    }
    meta.extend(encode(&metadata));

    let mut out = vec![RawRecord {
        id: 1,
        format: 1,
        payload: meta,
    }];
    if let Some(t) = &records.thumbnail {
        let size = (THUMBNAIL_HEADER + t.y.len() + t.uv.len()) as u64;
        let header = [1, u32_of(size), 1, 0, t.width, t.height, 0, 0, 0, 0];
        let mut payload: Vec<u8> = header.iter().flat_map(|w| w.to_le_bytes()).collect();
        payload.extend(&t.y);
        payload.extend(&t.uv);
        out.push(RawRecord {
            id: 2,
            format: 0,
            payload,
        });
    }
    if !records.imu.is_empty() {
        let payload = records
            .imu
            .iter()
            .flat_map(|item| {
                let values = item.accel.iter().chain(&item.gyro);
                let mut b = item.t_us.to_le_bytes().to_vec();
                b.extend(values.flat_map(|v| v.to_le_bytes()));
                b
            })
            .collect();
        out.push(RawRecord {
            id: 3,
            format: 0,
            payload,
        });
    }
    if !records.exposure.is_empty() {
        let payload = records
            .exposure
            .iter()
            .flat_map(|item| {
                [
                    item.t_us.to_le_bytes(),
                    item.seconds.to_bits().to_le_bytes(),
                ]
            })
            .flatten()
            .collect();
        out.push(RawRecord {
            id: 4,
            format: 0,
            payload,
        });
    }
    out.extend(records.others.iter().cloned());
    out.sort_by_key(|r| Reverse(r.id));
    out
}

/// The `inst` box that holds `records`' trailer, for a file whose bytes
/// before it end at offset `at`: its 8-byte header, then the trailer laid
/// out as the module says, with each record but 1 and 10 on an `align`
/// boundary of the file (`X5_ALIGN` for the X5's; 1 for none) and 1 and
/// 10 right after the record before them. Record 1 is always written,
/// 2 when there is a thumbnail, 3 and 4 when they hold an item, and each
/// of `records.others` as given. The index has 31 slots, or one more than
/// the largest id when that is 31 or more; a slot lists the last record
/// written with its id.
///
/// A trailer fault ([`InsvFault::in_trailer`]) is planted in it, as each
/// fault's doc says; the MP4's faults leave it as it would be. The tail's
/// sizes are u32s, so a trailer or a file of 4 GiB or more, which no
/// synthetic file comes near, is written with its sizes saturated.
pub fn inst_box(
    records: &Records,
    at: u64,
    align: u64,
    fault: Option<InsvFault>,
) -> (Vec<u8>, TrailerLayout) {
    let align = align.max(1);
    let payload_start = at + INST_HEADER as u64;
    let mut b = vec![0; INST_HEADER];
    let mut slots: Vec<Slot> = Vec::new();
    for r in laid_out(records, fault) {
        if !matches!(r.id, 1 | 10) {
            let here = at + b.len() as u64;
            let pad = here.next_multiple_of(align) - here;
            b.resize(b.len() + pad as usize, 0);
        }
        let size = u32_of(r.payload.len() as u64);
        slots.retain(|s| s.id != r.id);
        slots.push(Slot {
            id: r.id,
            format: r.format,
            offset: at + b.len() as u64,
            size,
        });
        b.extend(&r.payload);
        b.extend([r.format, r.id]);
        b.extend(size.to_le_bytes());
    }
    slots.sort_by_key(|s| s.id);

    let count = slots
        .last()
        .map_or(0, |s| usize::from(s.id) + 1)
        .max(X5_SLOTS);
    let index = at + b.len() as u64;
    let size = (b.len() - INST_HEADER + count * SLOT + FOOTER + TAIL) as u64;
    let mut table = vec![0; count * SLOT];
    for s in &slots {
        let offset = match fault {
            Some(InsvFault::IndexSlotPastEnd) if s.id == 1 => size,
            _ => s.offset - payload_start,
        };
        let slot = &mut table[usize::from(s.id) * SLOT..][..SLOT];
        slot[0] = s.id;
        slot[1] = s.format;
        slot[2..6].copy_from_slice(&s.size.to_le_bytes());
        slot[6..].copy_from_slice(&u32_of(offset).to_le_bytes());
    }
    b.extend(table);
    // The index's footer: format 0, id 0 and its size.
    b.extend([0, 0]);
    b.extend(u32_of((count * SLOT) as u64).to_le_bytes());

    let told = match fault {
        Some(InsvFault::TruncatedTrailer) => payload_start + size + 1,
        _ => size,
    };
    b.extend([0; 32]);
    b.extend(u32_of(told).to_le_bytes());
    b.extend(VERSION.to_le_bytes());
    b.extend(MAGIC);
    if fault == Some(InsvFault::BadMagic)
        && let Some(last) = b.last_mut()
    {
        *last ^= 1;
    }
    b[..4].copy_from_slice(&u32_of(size + INST_HEADER as u64).to_be_bytes());
    b[4..INST_HEADER].copy_from_slice(b"inst");
    let layout = TrailerLayout {
        payload_start,
        size,
        index,
        slots,
    };
    (b, layout)
}

/// Writes `mp4`'s bytes and then [`inst_box`] at the X5's alignment to
/// `out`, whole or not at all (`core::fs::TempFile`), so `out` exists
/// only once it is complete; a file at `out` is replaced, even `mp4`
/// itself. `write_insv` (M3.5) writes the tracks with `Mp4Writer` to a
/// file of its own and calls `append_trailer(&tracks,
/// &dir.join("VID_…_00_001.insv"), &spec.records, spec.fault)`, and the
/// same for the LRV with its records.
pub fn append_trailer(
    mp4: &Path,
    out: &Path,
    records: &Records,
    fault: Option<InsvFault>,
) -> Result<TrailerLayout, SynthError> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |error| SynthError::Io { path, error }
    };
    let mut src = File::open(mp4).map_err(io_err(mp4))?;
    let mut tmp = TempFile::create(out).map_err(io_err(out))?;
    let at = io::copy(&mut src, tmp.file_mut()).map_err(io_err(out))?;
    let (inst, layout) = inst_box(records, at, X5_ALIGN, fault);
    tmp.file_mut().write_all(&inst).map_err(io_err(out))?;
    tmp.persist_replace().map_err(io_err(out))?;
    Ok(layout)
}

/// The lens count every string synth prints starts with.
const LENSES: usize = 2;
/// The decimals the X5 prints each kind of number to ("Pat's X5
/// samples"): ξ and the translation to 6, the focal lengths, the centre
/// and the angles to 3, the distortion terms to 8.
const XI_DECIMALS: usize = 6;
const T_DECIMALS: usize = 6;
const PIXEL_DECIMALS: usize = 3;
const ANGLE_DECIMALS: usize = 3;
const TERM_DECIMALS: usize = 8;

/// `v` to `decimals` places, as C's `printf("%.*f")` writes it: rounded
/// to the nearest, ties to even, with a minus for any negative value,
/// even one that rounds to zero.
fn fixed(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}")
}

/// `cal` as its generation's calibration string (insta360.md §4.1): the
/// lens count 2, each block's tokens in the generation's order with its
/// canvas `w h` and lens type, then the packed word (the generation's
/// code in bits 16 and up, `cal.body` in bits 8 to 15). Each number is
/// written at the X5's precision ("Pat's X5 samples"): ξ and t to 6
/// decimals, focal, centre and angles to 3, the distortion terms to 8,
/// so that printing Pat's numbers gives Pat's strings. The numbers must
/// be finite: a NaN or an infinity prints as Rust writes it, which no
/// camera does.
///
/// `planted` names the conventions the strings are printed under. M3.2
/// prints `cal`'s stored numbers as they are, so no token depends on it
/// yet; the files M3.5 writes print a rig's truth through it (7.2).
pub fn print(cal: &CalibTruth, planted: &Planted) -> String {
    // The stored numbers are printed as they are, whatever is planted.
    let _ = planted;
    let mut tokens = vec![LENSES.to_string()];
    for (b, block) in cal.blocks.iter().enumerate() {
        tokens.push(fixed(block.xi, XI_DECIMALS));
        let pixels = [block.fx, block.fy, block.cx, block.cy];
        tokens.extend(pixels.map(|v| fixed(v, PIXEL_DECIMALS)));
        tokens.extend(block.ypr_deg.map(|v| fixed(v, ANGLE_DECIMALS)));
        tokens.extend(block.t_m.map(|v| fixed(v, T_DECIMALS)));
        let terms = cal.dist.terms(b);
        tokens.extend(terms.into_iter().map(|v| fixed(v, TERM_DECIMALS)));
        let integers = [cal.canvas[0], cal.canvas[1], cal.lens_type];
        tokens.extend(integers.map(|n| n.to_string()));
    }
    let code: u32 = match cal.generation() {
        Generation::V3 => 3,
        Generation::V6 => 6,
    };
    let word = (code << 16) | (u32::from(cal.body) << 8);
    tokens.push(word.to_string());
    tokens.join("_")
}

#[cfg(test)]
mod tests {
    use spherecut_testkit::control::must_fail;

    use super::*;
    use crate::lens::{
        Angle, Axis, BlockTruth, QuarterTurn, Sign, StoredDist, TruthBinding, TruthCanvas,
        TruthEuler, TruthTurn, TruthTz, TruthV6,
    };

    // Pat's V3 and V6 strings, byte-identical in all 14 of Pat's files
    // (insta360.md, "Pat's X5 samples").
    const PATS_V3: &str = "2_2.000000_4284.620_4284.280_2701.130_2692.130_-0.198_0.211_90.280_0.000000_0.000000_0.000000_0.18632051_2.04984355_-3.24446917_-0.00143576_-0.00339240_10752_5376_113_2.000000_4291.030_4291.030_8069.000_2684.950_0.239_0.113_89.107_-0.004460_0.000017_-0.031920_0.18940836_2.07073069_-3.24103332_0.00013831_-0.00299873_10752_5376_113_197632";
    const PATS_V6: &str = "2_2.000000_4277.520_4277.270_2713.050_2689.210_-0.310_-0.271_90.255_0.000000_0.000000_0.000000_0.28032997_0.89045405_2.04927588_-8.05179787_0.00000000_0.00120200_-0.00259487_0.02324706_0.00533920_-0.01560208_0.00019357_-0.04580523_0.00197187_10752_5376_113_2.000000_4284.400_4284.760_8075.730_2680.350_0.120_-0.173_89.112_0.000637_0.000001_-0.031525_0.27665332_0.98522973_1.83605266_-7.99374485_0.00000000_0.00021056_-0.00147626_0.01940425_0.00331838_-0.01330665_0.00074511_-0.02859299_0.00888746_10752_5376_113_394240";

    /// Pat's V3 calibration, number for number as the format notes lay it
    /// out.
    fn pats_v3() -> CalibTruth {
        let blocks = [
            BlockTruth {
                xi: 2.0,
                fx: 4284.62,
                fy: 4284.28,
                cx: 2701.13,
                cy: 2692.13,
                ypr_deg: [-0.198, 0.211, 90.28],
                t_m: [0.0; 3],
            },
            BlockTruth {
                xi: 2.0,
                fx: 4291.03,
                fy: 4291.03,
                cx: 8069.0,
                cy: 2684.95,
                ypr_deg: [0.239, 0.113, 89.107],
                t_m: [-0.00446, 0.000017, -0.03192],
            },
        ];
        let dist = StoredDist::V3 {
            k: [
                [0.18632051, 2.04984355, -3.24446917],
                [0.18940836, 2.07073069, -3.24103332],
            ],
            p: [[-0.00143576, -0.0033924], [0.00013831, -0.00299873]],
        };
        CalibTruth {
            blocks,
            dist,
            canvas: [10752, 5376],
            dst: 5312,
            lens_type: 113,
            body: 4,
        }
    }

    /// Pat's V6 calibration, a separate one from V3's.
    fn pats_v6() -> CalibTruth {
        let blocks = [
            BlockTruth {
                xi: 2.0,
                fx: 4277.52,
                fy: 4277.27,
                cx: 2713.05,
                cy: 2689.21,
                ypr_deg: [-0.31, -0.271, 90.255],
                t_m: [0.0; 3],
            },
            BlockTruth {
                xi: 2.0,
                fx: 4284.4,
                fy: 4284.76,
                cx: 8075.73,
                cy: 2680.35,
                ypr_deg: [0.12, -0.173, 89.112],
                t_m: [0.000637, 0.000001, -0.031525],
            },
        ];
        let d = [
            [
                0.28032997,
                0.89045405,
                2.04927588,
                -8.05179787,
                0.0,
                0.001202,
                -0.00259487,
                0.02324706,
                0.0053392,
                -0.01560208,
                0.00019357,
                -0.04580523,
                0.00197187,
            ],
            [
                0.27665332,
                0.98522973,
                1.83605266,
                -7.99374485,
                0.0,
                0.00021056,
                -0.00147626,
                0.01940425,
                0.00331838,
                -0.01330665,
                0.00074511,
                -0.02859299,
                0.00888746,
            ],
        ];
        CalibTruth {
            dist: StoredDist::V6 { d },
            blocks,
            ..pats_v3()
        }
    }

    /// One value of each convention; `print` reads none of them yet.
    fn planted() -> Planted {
        Planted {
            binding: TruthBinding::Direct,
            canvas: TruthCanvas::A,
            euler: TruthEuler {
                axes: [Axis::Z, Axis::Y, Axis::X],
                order: [Angle::Yaw, Angle::Pitch, Angle::Roll],
                signs: [Sign::Plus; 3],
                roll_offset: QuarterTurn::Minus90,
            },
            turn: TruthTurn::Y,
            upright: QuarterTurn::Zero,
            tz: TruthTz::AsStored,
            v6: TruthV6::RADIAL_ONLY,
        }
    }

    /// The first token at which `a` and `b` differ, by its index: one
    /// string's end counts as a token the other doesn't have.
    fn same_tokens(a: &str, b: &str) -> Result<(), usize> {
        let (a, b): (Vec<&str>, Vec<&str>) = (a.split('_').collect(), b.split('_').collect());
        match (0..a.len().max(b.len())).find(|&k| a.get(k) != b.get(k)) {
            Some(k) => Err(k),
            None => Ok(()),
        }
    }

    /// Printing Pat's V3 and V6 numbers ("Pat's X5 samples") gives
    /// Pat's strings, token for token.
    #[test]
    fn print_gives_pats_strings_back() {
        let p = planted();
        assert_eq!(same_tokens(&print(&pats_v3(), &p), PATS_V3), Ok(()));
        assert_eq!(same_tokens(&print(&pats_v6(), &p), PATS_V6), Ok(()));
        assert_eq!(print(&pats_v3(), &p), PATS_V3);
        assert_eq!(print(&pats_v6(), &p), PATS_V6);
        // No token depends on what is planted yet: under every value of
        // the other conventions, the same strings.
        let others = TruthBinding::all().flat_map(|binding| {
            TruthTurn::all().flat_map(move |turn| {
                TruthTz::all().map(move |tz| Planted {
                    binding,
                    turn,
                    tz,
                    canvas: TruthCanvas::C,
                    upright: QuarterTurn::Half,
                    euler: TruthEuler::all().last().unwrap(),
                    v6: TruthV6::all().last().unwrap(),
                })
            })
        });
        for q in others {
            assert_eq!(
                same_tokens(&print(&pats_v6(), &q), PATS_V6),
                Ok(()),
                "{q:?}"
            );
        }

        // Controls: the blocks swapped, which first differ in the focal
        // length (token 2), and block 1's tz a unit off in its last place
        // (token 38, the 11th of block 1, which starts at token 28).
        let mut swapped = pats_v3();
        swapped.blocks.swap(0, 1);
        let at = must_fail(
            "Pat's V3 blocks swapped",
            same_tokens(&print(&swapped, &p), PATS_V3),
        );
        assert_eq!(at, 2);
        let mut off = pats_v6();
        off.blocks[1].t_m[2] -= 0.000001;
        let at = must_fail(
            "block 1's tz a unit off",
            same_tokens(&print(&off, &p), PATS_V6),
        );
        assert_eq!(at, 38);
    }

    /// The little-endian u32 at byte `at` of `b`.
    fn le32(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// The `inst` box `b`, at byte `at` of its file, read by hand as
    /// insta360.md §3 lays out the X5's, with every rule of its layout
    /// checked; or the first rule it breaks. The rules: the tail's zeros,
    /// size E (the box less its header), version 3 and magic; the box's
    /// size E + 8 and type; the index's footer at EOF − 78 and its 31
    /// slots at EOF − 388, slot k listing record k; each record's footer
    /// repeating its slot; each record but 1 and 10 on a 128 KiB boundary
    /// of the file; the records in descending id order with only zeros
    /// between them (synth's padding: the X5's has stray bytes in some
    /// gaps of every file), 1 and 10 right after the record before; and
    /// record 1's footer ending where the index starts.
    fn read_by_hand(b: &[u8], at: u64) -> Result<TrailerLayout, String> {
        let n = b.len();
        if b[n - 72..n - 40] != [0; 32] {
            return Err("the tail's 32 bytes aren't zeros".into());
        }
        let size = le32(b, n - 40) as usize;
        if size != n - 8 {
            return Err(format!("the tail's size is {size}, not {}", n - 8));
        }
        if le32(b, n - 36) != 3 {
            return Err("the tail's version isn't 3".into());
        }
        if &b[n - 32..] != b"8db42d694ccc418790edff439fe026bf" {
            return Err("the tail's magic".into());
        }
        if u32::from_be_bytes(b[..4].try_into().unwrap()) as usize != size + 8
            || &b[4..8] != b"inst"
        {
            return Err("the inst box's header".into());
        }
        if b[n - 78..n - 76] != [0, 0] || le32(b, n - 76) != 310 {
            return Err("the index's footer at EOF − 78".into());
        }
        let index = n - 388;
        let mut slots = Vec::new();
        for k in 0..31 {
            let s = &b[index + 10 * k..][..10];
            if s == [0; 10] {
                continue;
            }
            let (id, format) = (s[0], s[1]);
            let (size, offset) = (le32(s, 2), le32(s, 6) as usize);
            if usize::from(id) != k {
                return Err(format!("slot {k} lists record {id}"));
            }
            let footer = 8 + offset + size as usize;
            let want = [&[format, id][..], &size.to_le_bytes()].concat();
            if b[footer..footer + 6] != want[..] {
                return Err(format!("record {id}'s footer doesn't repeat its slot"));
            }
            let start = at + 8 + offset as u64;
            if !matches!(id, 1 | 10) && !start.is_multiple_of(128 << 10) {
                return Err(format!(
                    "record {id} at byte {start} isn't on a 128 KiB boundary"
                ));
            }
            slots.push(Slot {
                id,
                format,
                offset: start,
                size,
            });
        }
        let mut in_file = slots.clone();
        in_file.sort_by_key(|s| s.offset);
        let mut end = 8;
        for (i, s) in in_file.iter().enumerate() {
            if i > 0 && in_file[i - 1].id <= s.id {
                return Err(format!("record {} comes after a lower id", s.id));
            }
            let start = (s.offset - at) as usize;
            if b[end..start].iter().any(|&x| x != 0) {
                return Err(format!("the bytes before record {} aren't zeros", s.id));
            }
            if matches!(s.id, 1 | 10) && start != end {
                return Err(format!("record {} doesn't follow the record before", s.id));
            }
            end = start + s.size as usize + 6;
        }
        if in_file.last().map(|s| s.id) != Some(1) || end != index {
            return Err("record 1 doesn't end at the index".into());
        }
        Ok(TrailerLayout {
            payload_start: at + 8,
            size: size as u64,
            index: at + index as u64,
            slots,
        })
    }

    /// The footers, the 128 KiB boundaries, the 31-slot index at
    /// EOF − 388, the tail and `inst`'s size E + 8, read back by hand.
    #[test]
    fn the_trailer_is_laid_out_as_the_x5s() {
        let mut records = Records::x5_like(
            &[pats_v3(), pats_v6()],
            &planted(),
            30,
            FrameRate::FPS_29_97,
        );
        records.thumbnail = Some(Thumbnail {
            width: 8,
            height: 4,
            y: vec![16; 32],
            uv: vec![128; 16],
        });
        // The X5's other records, small.
        records.others = [29, 28, 22, 11, 10, 9]
            .map(|id| RawRecord {
                id,
                format: 0,
                payload: vec![id; 3 * usize::from(id)],
            })
            .to_vec();
        // An MP4 whose end is on no boundary.
        let at = 3 * X5_ALIGN + 12_345;
        let (b, layout) = inst_box(&records, at, X5_ALIGN, None);
        assert_eq!(read_by_hand(&b, at), Ok(layout.clone()));
        let ids: Vec<u8> = layout.slots.iter().map(|s| s.id).collect();
        assert_eq!(ids, [1, 2, 3, 4, 9, 10, 11, 22, 28, 29]);
        assert_eq!(layout.payload_start + layout.size, at + b.len() as u64);
        assert_eq!(layout.index, at + b.len() as u64 - 388);
        // The metadata starts with the serial, field 1: key 0x0a, its
        // length and its bytes, synth's and never Pat's.
        let meta = layout.slots[0];
        let start = (meta.offset - at) as usize;
        assert_eq!(&b[start..start + 12], b"\x0a\x0aSYNTH00001");
        // 30 frames at 29.97: 38 exposure items, 5 before the first
        // frame's and 3 past the last's, and an IMU item a ms over their
        // 37 frames, 1234.567 ms.
        assert_eq!((layout.slots[3].id, layout.slots[3].size), (4, 38 * 16));
        assert_eq!((layout.slots[2].id, layout.slots[2].size), (3, 1235 * 20));

        // Controls: the same box read as if one byte later in the file,
        // so that no record is on a boundary; a footer with another id;
        // and a byte between two records that isn't zero.
        let e = must_fail("the box a byte later", read_by_hand(&b, at + 1));
        assert!(e.contains("isn't on a 128 KiB boundary"), "{e}");
        let r4 = layout.slots[3];
        let footer = (r4.offset - at) as usize + r4.size as usize;
        let mut changed = b.clone();
        changed[footer + 1] = 5;
        let e = must_fail("record 4's footer with id 5", read_by_hand(&changed, at));
        assert_eq!(e, "record 4's footer doesn't repeat its slot");
        let mut dirty = b.clone();
        dirty[footer + 6] = 1;
        let e = must_fail(
            "a byte after record 4 that isn't zero",
            read_by_hand(&dirty, at),
        );
        assert_eq!(e, "the bytes before record 3 aren't zeros");
    }

    /// `encode` writes protobuf's wire format, checked against bytes
    /// worked out by hand from the format's rules.
    #[test]
    fn encode_writes_the_wire_format() {
        let fields = [
            // Key 1 << 3 | 0, then 300 in two bytes: 0b10_0101100.
            Field::varint(1, 300),
            // Key 150 << 3 | 0 = 1200 in two bytes, then 0.
            Field::varint(150, 0),
            // Key 2 << 3 | 1, then 1.5's bits, little-endian.
            Field::double(2, 1.5),
            // Key 3 << 3 | 5, then 7 in four bytes.
            Field {
                number: 3,
                value: Value::Fixed32(7),
            },
            // Key 4 << 3 | 2, the length, the bytes.
            Field::string(4, "ab"),
            // Key 5 << 3 | 2, the length, then field 1's varint u64::MAX
            // in ten bytes.
            Field::message(5, vec![Field::varint(1, u64::MAX)]),
        ];
        let mut want = vec![0x08, 0xac, 0x02, 0xb0, 0x09, 0x00, 0x11];
        want.extend(1.5f64.to_bits().to_le_bytes());
        want.extend([0x1d, 7, 0, 0, 0, 0x22, 2, b'a', b'b', 0x2a, 11, 0x08]);
        want.extend([0xff; 9]);
        want.push(0x01);
        assert_eq!(encode(&fields), want);
        // Control: a field's number changed changes its key.
        let mut other = fields.to_vec();
        other[0].number = 2;
        must_fail("field 1 written as 2", encode(&other) == want);
    }

    /// Each trailer fault is planted where its doc says, and the MP4's
    /// faults leave the box as it is without them.
    #[test]
    fn each_fault_is_planted_where_it_says() {
        let records = Records::x5_like(&[pats_v3(), pats_v6()], &planted(), 4, FrameRate::FPS_25);
        let at = 1000;
        let (clean, layout) = inst_box(&records, at, 64, None);
        let n = clean.len();
        let meta = layout.slots[0];
        // How many bytes of `b` aren't the clean box's, its length apart.
        let differ = |b: &[u8]| (0..n.min(b.len())).filter(|&i| b[i] != clean[i]).count();
        for fault in InsvFault::all() {
            let (b, l) = inst_box(&records, at, 64, Some(fault));
            if !fault.in_trailer() {
                assert_eq!((&b, &l), (&clean, &layout), "{fault:?}");
                continue;
            }
            assert!(differ(&b) > 0, "{fault:?}");
            match fault {
                // The file's length and a byte, in the tail's size alone.
                InsvFault::TruncatedTrailer => {
                    assert_eq!(b.len(), n);
                    assert_eq!(le32(&b, n - 40) as usize, 1000 + n + 1);
                    assert!(differ(&b) <= 4);
                }
                // Record 1's slot, the second, gives E as its offset.
                InsvFault::IndexSlotPastEnd => {
                    assert_eq!(u64::from(le32(&b, n - 388 + 10 + 6)), layout.size);
                    assert!(differ(&b) <= 4);
                }
                InsvFault::BadMagic => {
                    assert_eq!(b[n - 1], b'g');
                    assert_eq!(differ(&b), 1);
                }
                // A key of field 7, wire type 0, then eleven bytes.
                InsvFault::VarintOverflow => {
                    let start = (l.slots[0].offset - at) as usize;
                    assert_eq!(b[start], 0x38);
                    assert_eq!(&b[start + 1..start + 12], [&[0x80; 10][..], &[0]].concat());
                    assert_eq!(l.slots[0].size, meta.size + 12);
                }
                // Each of the four strings 7 bytes shorter ("_197632" or
                // "_394240" dropped), or gone with its key and length,
                // two bytes each.
                InsvFault::WrongTokenCount | InsvFault::NoCalibration => {
                    let less = match fault {
                        InsvFault::WrongTokenCount => 4 * 7,
                        _ => 2 * (2 + 2 + PATS_V3.len()) + 2 * (2 + 2 + PATS_V6.len()),
                    };
                    assert_eq!(
                        l.slots[0].size as usize,
                        meta.size as usize - less,
                        "{fault:?}"
                    );
                }
                _ => unreachable!("{fault:?}"),
            }
        }
        // Control: the clean box differs from itself nowhere.
        must_fail("the clean box against itself", differ(&clean) > 0);
    }

    /// One of Pat's takes as its master's trailer has it, read from the
    /// bytes: the tracks' frames and rate, record 4's items and the item
    /// at field 24's time (as tier2_samples.rs's table has them), field
    /// 98's count, field 93, and the IMU's first stamp less field 24's, in
    /// µs (a throwaway script's read of all 14 files, 2026-11-01, whose
    /// figures insta360.md's "Pat's X5 samples" lists).
    struct Take {
        file: &'static str,
        frames: u32,
        rate: FrameRate,
        items: usize,
        first: usize,
        counted: u64,
        length_ms: u64,
        imu_from_us: i64,
    }

    /// Pat's seven takes.
    fn takes() -> [Take; 7] {
        let (f25, f30, f60) = (
            FrameRate::FPS_25,
            FrameRate::FPS_29_97,
            FrameRate::FPS_59_94,
        );
        let f120 = FrameRate::new(spherecut_core::Rational::new(120000, 1001).unwrap()).unwrap();
        let take = |file, frames, rate, items, first, counted, length_ms, imu_from_us| Take {
            file,
            frames,
            rate,
            items,
            first,
            counted,
            length_ms,
            imu_from_us,
        };
        [
            take("004", 114, f30, 122, 5, 115, 3837, -156_387),
            take("005", 116, f25, 123, 5, 117, 4680, -190_207),
            take("006", 190, f60, 200, 6, 191, 3186, -112_408),
            take("007", 586, f120, 602, 10, 587, 4896, -91_611),
            take("008", 966, f30, 974, 5, 967, 32265, -150_526),
            take("009", 1904, f30, 1912, 5, 1905, 63563, -155_919),
            take("010", 968, f30, 976, 5, 969, 32332, -151_490),
        ]
    }

    /// Field `n` of `records`' metadata, or of its message `n` the item
    /// `item`, as a varint.
    fn varint(records: &Records, n: u32, item: Option<u32>) -> Option<u64> {
        let f = records.metadata.iter().find(|f| f.number == n)?;
        let value = match (&f.value, item) {
            (v, None) => v,
            (Value::Message(items), Some(k)) => &items.iter().find(|i| i.number == k)?.value,
            _ => return None,
        };
        match value {
            Value::Varint(v) => Some(*v),
            _ => None,
        }
    }

    /// `records`, x5_like's for take `t`'s frames and rate, held to the
    /// take's master: record 4's items; field 24's time that of item
    /// `t.first` alone, and every item a frame apart, to the µs synth
    /// rounds to; field 98's item 1 at field 24's time and its count;
    /// field 93; and the IMU from within 17 ms of the take's first stamp
    /// (where the X5's starts, within 17 ms of record 4's first item) to
    /// within a ms of record 4's last item. Or the first that isn't.
    fn as_take(records: &Records, t: &Take) -> Result<(), String> {
        let e = &records.exposure;
        if e.len() != t.items {
            return Err(format!("{} exposure items, not {}", e.len(), t.items));
        }
        let first_us = varint(records, 24, None).ok_or("no field 24")?;
        let at: Vec<usize> = (0..e.len()).filter(|&i| e[i].t_us == first_us).collect();
        if at != [t.first] {
            return Err(format!("field 24 is exposure item {at:?}, not {}", t.first));
        }
        let r = t.rate.as_rational();
        let period_us = 1e6 * r.den() as f64 / r.num() as f64;
        if let Some(w) = e
            .windows(2)
            .find(|w| ((w[1].t_us - w[0].t_us) as f64 - period_us).abs() > 1.0)
        {
            return Err(format!(
                "items {} and {} µs aren't a frame apart",
                w[0].t_us, w[1].t_us
            ));
        }
        if varint(records, 98, Some(1)) != Some(first_us) {
            return Err("field 98's item 1 isn't field 24's time".into());
        }
        let counted = varint(records, 98, Some(5));
        if counted != Some(t.counted) {
            return Err(format!("field 98 counts {counted:?}, not {}", t.counted));
        }
        let length = varint(records, 93, None);
        if length != Some(t.length_ms) {
            return Err(format!("field 93 is {length:?}, not {}", t.length_ms));
        }
        let (Some(imu_first), Some(imu_last)) = (records.imu.first(), records.imu.last()) else {
            return Err("no IMU".into());
        };
        let from = imu_first.t_us as i64 - first_us as i64;
        if (from - t.imu_from_us).abs() > 17_000 {
            return Err(format!(
                "the IMU starts {from} µs from field 24, not about {}",
                t.imu_from_us
            ));
        }
        let last = e[e.len() - 1].t_us;
        if !(last.saturating_sub(1000)..=last).contains(&imu_last.t_us) {
            return Err(format!(
                "the IMU ends at {}, not by record 4's last item at {last}",
                imu_last.t_us
            ));
        }
        Ok(())
    }

    /// `x5_like` for the frames and rate of each of Pat's takes gives what
    /// the take's master holds: record 4's item count, with field 24's
    /// frame among earlier items (5 at 25 and 29.97 fps, 6 at 59.94, 10 at
    /// 119.88), field 98's count of frames + 1, field 93 rounded down and
    /// an IMU that starts about where the camera's does.
    ///
    /// Controls, each failing a check of its own: take 006's records held
    /// to take 007's; field 24 at the first item, as synth had it before;
    /// one item a frame late; field 98's item 1 a µs off; field 98
    /// counting the tracks' frames; field 93 to the nearest ms (3187 on
    /// 006, where the X5 writes 3186); the IMU from field 24's time; and
    /// the IMU stopped at the last frame's item.
    #[test]
    fn x5_like_holds_what_pats_masters_hold() {
        let p = planted();
        let cals = [pats_v3(), pats_v6()];
        let all = takes();
        for t in &all {
            let records = Records::x5_like(&cals, &p, t.frames, t.rate);
            assert_eq!(as_take(&records, t), Ok(()), "take {}", t.file);
        }

        let (t006, t007) = (&all[2], &all[3]);
        let r = Records::x5_like(&cals, &p, t006.frames, t006.rate);
        let set = |n: u32, item: Option<u32>, v: u64| {
            let mut r = r.clone();
            let f = r.metadata.iter_mut().find(|f| f.number == n).unwrap();
            match (&mut f.value, item) {
                (Value::Message(items), Some(k)) => {
                    items.iter_mut().find(|i| i.number == k).unwrap().value = Value::Varint(v)
                }
                (value, _) => *value = Value::Varint(v),
            }
            r
        };
        let e = must_fail("take 006's records held to 007's", as_take(&r, t007));
        assert_eq!(e, "200 exposure items, not 602");
        let e = must_fail(
            "field 24 at the first exposure item",
            as_take(&set(24, None, r.exposure[0].t_us), t006),
        );
        assert_eq!(e, "field 24 is exposure item [0], not 6");
        let mut late = r.clone();
        late.exposure[100].t_us += 16_683;
        let e = must_fail("an exposure item a frame late", as_take(&late, t006));
        assert!(e.ends_with("aren't a frame apart"), "{e}");
        let e = must_fail(
            "field 98's item 1 a µs off",
            as_take(&set(98, Some(1), 1_000_001), t006),
        );
        assert_eq!(e, "field 98's item 1 isn't field 24's time");
        let e = must_fail(
            "field 98 counting the tracks' frames",
            as_take(&set(98, Some(5), 190), t006),
        );
        assert_eq!(e, "field 98 counts Some(190), not 191");
        let e = must_fail(
            "field 93 to the nearest ms",
            as_take(&set(93, None, (191 * 1000 * 1001 + 30_000) / 60_000), t006),
        );
        assert_eq!(e, "field 93 is Some(3187), not 3186");
        let mut late_imu = r.clone();
        late_imu.imu.retain(|i| i.t_us >= FIRST_FRAME_US);
        let e = must_fail("the IMU from field 24's time", as_take(&late_imu, t006));
        assert!(e.starts_with("the IMU starts 900 µs from field 24"), "{e}");
        let last_frame = r.exposure[t006.first + t006.frames as usize - 1].t_us;
        let mut short_imu = r.clone();
        short_imu.imu.retain(|i| i.t_us <= last_frame);
        let e = must_fail(
            "the IMU stopped at the last frame",
            as_take(&short_imu, t006),
        );
        assert!(e.starts_with("the IMU ends at"), "{e}");
    }

    /// Field 136, the generation in use, is the newest `x5_like` is
    /// given: 4 with V6, 3 with V3 alone, none with neither. A debug
    /// build refuses two calibrations of one generation.
    ///
    /// Controls: V3 alone said to be in use as V6, as synth wrote it
    /// before; and one calibration of each generation, refused.
    #[test]
    fn field_136_names_the_newest_generation_given() {
        let in_use = |cals: &[CalibTruth]| {
            let r = Records::x5_like(cals, &planted(), 4, FrameRate::FPS_25);
            varint(&r, 136, None)
        };
        let (v3, v6) = (pats_v3(), pats_v6());
        assert_eq!(in_use(&[v3, v6]), Some(4));
        assert_eq!(in_use(&[v6, v3]), Some(4));
        assert_eq!(in_use(&[v6]), Some(4));
        assert_eq!(in_use(&[v3]), Some(3));
        assert_eq!(in_use(&[]), None);
        must_fail("V3 alone in use as V6", in_use(&[v3]) == Some(4));
        if cfg!(debug_assertions) {
            let twice = std::panic::catch_unwind(|| {
                Records::x5_like(&[pats_v3(), pats_v3()], &planted(), 4, FrameRate::FPS_25)
            });
            assert!(twice.is_err(), "two V3 calibrations taken");
            let once = std::panic::catch_unwind(|| {
                Records::x5_like(&[pats_v3(), pats_v6()], &planted(), 4, FrameRate::FPS_25)
            });
            must_fail("one calibration of each generation, refused", once.is_err());
        }
    }

    /// Whether `lrv` is the LRV of `master` for `frames` frames as
    /// x5_lrv's doc says: every record but the metadata the master's;
    /// the metadata's fields the master's, in its order, but 79 and 80,
    /// which it lacks, 131 and 132, which are 1, and field 98's item 5,
    /// which is `frames`. Or the first that isn't.
    fn lrv_of(lrv: &Records, master: &Records, frames: u64) -> Result<(), String> {
        if lrv.exposure != master.exposure {
            return Err("its exposure record isn't the master's".into());
        }
        if (&lrv.thumbnail, &lrv.imu, &lrv.others)
            != (&master.thumbnail, &master.imu, &master.others)
        {
            return Err("a record other than 1 and 4 isn't the master's".into());
        }
        let kept: Vec<&Field> = master
            .metadata
            .iter()
            .filter(|f| !matches!(f.number, 79 | 80))
            .collect();
        let numbers = |fields: &[&Field]| fields.iter().map(|f| f.number).collect::<Vec<_>>();
        let lrv_fields: Vec<&Field> = lrv.metadata.iter().collect();
        if numbers(&lrv_fields) != numbers(&kept) {
            return Err(format!("its fields {:?}", numbers(&lrv_fields)));
        }
        for (l, m) in lrv_fields.iter().zip(kept) {
            let same = match l.number {
                131 | 132 => l.value == Value::Varint(1),
                98 => {
                    varint(lrv, 98, Some(5)) == Some(frames)
                        && varint(lrv, 98, Some(1)) == varint(master, 98, Some(1))
                }
                _ => l == &m,
            };
            if !same {
                return Err(format!("its field {} is {:?}", l.number, l.value));
            }
        }
        Ok(())
    }

    /// `x5_lrv` keeps the master's records and its exposure record
    /// whatever the LRV's length, as the 5.7K60 take's LRV of 95 frames
    /// keeps its master's 200 items, and gives the metadata the LRV's own
    /// fields.
    ///
    /// Controls: the LRV x5_like writes for its own 95 frames, whose
    /// exposure record isn't the master's; and the master held to be its
    /// own LRV, which has fields 79 and 80.
    #[test]
    fn x5_lrv_keeps_the_masters_records() {
        let cals = [pats_v3(), pats_v6()];
        let master = Records::x5_like(&cals, &planted(), 190, FrameRate::FPS_59_94);
        let lrv = Records::x5_lrv(&master, 95);
        assert_eq!(lrv_of(&lrv, &master, 95), Ok(()));
        assert_eq!(lrv.exposure.len(), 200);

        let own = Records::x5_like(&cals, &planted(), 95, FrameRate::FPS_59_94);
        let e = must_fail("an LRV of its own 95 frames", lrv_of(&own, &master, 95));
        assert_eq!(e, "its exposure record isn't the master's");
        let e = must_fail("the master as its own LRV", lrv_of(&master, &master, 191));
        assert!(
            e.starts_with("its fields [1, 2, 3, 24, 27, 54, 56, 62, 64, 65, 79,"),
            "{e}"
        );
    }

    /// `plant_original` changes the original copy alone, and adds one in
    /// field order where there is none.
    ///
    /// Control: the working string held to the planted original.
    #[test]
    fn plant_original_changes_the_original_alone() {
        let p = planted();
        let clean = Records::x5_like(&[pats_v3(), pats_v6()], &p, 4, FrameRate::FPS_25);
        let mut other = pats_v6();
        other.blocks[0].fx += 1.0;
        let mut planted_one = clean.clone();
        planted_one.plant_original(&other, &p);
        let changed: Vec<u32> = clean
            .metadata
            .iter()
            .zip(&planted_one.metadata)
            .filter(|(a, b)| a != b)
            .map(|(a, _)| a.number)
            .collect();
        assert_eq!(changed, [112]);
        let text = |r: &Records, n: u32| {
            r.metadata
                .iter()
                .find(|f| f.number == n)
                .map(|f| f.value.clone())
        };
        assert_eq!(
            text(&planted_one, 112),
            Some(Value::Bytes(print(&other, &p).into_bytes()))
        );
        assert_eq!(
            text(&planted_one, 111),
            Some(Value::Bytes(PATS_V6.as_bytes().to_vec()))
        );
        must_fail(
            "the working string, the planted original",
            text(&planted_one, 111) == text(&planted_one, 112),
        );

        // V3 alone, given V6's original: field 112 after 56, in field
        // order.
        let mut v3 = Records::x5_like(&[pats_v3()], &p, 4, FrameRate::FPS_25);
        v3.plant_original(&pats_v6(), &p);
        let numbers: Vec<u32> = v3.metadata.iter().map(|f| f.number).collect();
        assert!(numbers.is_sorted(), "{numbers:?}");
        assert!(
            numbers.contains(&112) && !numbers.contains(&111),
            "{numbers:?}"
        );
    }
}
