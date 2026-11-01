//! camera's reader and parser against synth's own trailer writer and
//! string printer, which share no code with them (M3 plan 4.1 rule 2,
//! 5.5, 7.2), M3.2's unit (d). These are integration tests: synth depends
//! on media, which depends on camera, so a unit test would hold a second
//! copy of camera's types. The properties run 100,000 cases at
//! `PROPTEST_CASES=100000`.
//!
//! What synth wrote is known from what it was given, not from its bytes:
//! the records, the metadata's fields and the calibration's numbers. The
//! few lines here that lay out the IMU's items (insta360.md §3.4) and read
//! the thumbnail's header (as the bytes of Pat's files have it) are this
//! file's own.
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): camera's suite ≤ 1 s at the default
//! 256 cases, which each test prints its time against; the properties
//! run at `PROPTEST_CASES=100000` apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use proptest::sample::{Index, subsequence};
use spherecut_camera::insta360::calib::{self, Focal, Generation, Token};
use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{
    CalibStrings, Calibrations, Exposure, FileGroup, FrameSpan, LensProfiles, Metadata, Nv12Image,
    Pairing, RecordRef, Trailer, Window, imu_span, parse_exposure, parse_metadata, read_record,
    read_trailer, thumbnail, verify_companion,
};
use spherecut_camera::pb::{Value, Walker};
use spherecut_camera::{CameraError, Part};
use spherecut_core::{FrameRate, Rational};
use spherecut_synth::insv::{
    self, ExposureItem, Field, ImuItem, InsvFault, RawRecord, Records, Thumbnail, TrailerLayout,
    X5_ALIGN,
};
use spherecut_synth::lens::{
    Angle, Axis, BlockTruth, CalibTruth, Planted, QuarterTurn, Sign, StoredDist, TruthBinding,
    TruthCanvas, TruthEuler, TruthTurn, TruthTz, TruthV6,
};
use spherecut_testkit::control::must_fail;
use spherecut_testkit::{Fnv64, fnv64};

/// The suite's budget in seconds, at the default number of cases: all of
/// camera's (19.6).
const BUDGET_S: f64 = 1.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the budget. The tests run side by
/// side, so the last line printed is the suite's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the suite {suite:.3} s so far, of camera's {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

/// One value of each convention synth plants; M3.2's `print` reads none.
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

/// Pat's V3 calibration, from insta360.md's "Pat's X5 samples".
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
    CalibTruth {
        blocks,
        dist: StoredDist::V3 {
            k: [
                [0.18632051, 2.04984355, -3.24446917],
                [0.18940836, 2.07073069, -3.24103332],
            ],
            p: [[-0.00143576, -0.0033924], [0.00013831, -0.00299873]],
        },
        canvas: [10752, 5376],
        dst: 5312,
        lens_type: 113,
        body: 4,
    }
}

/// Pat's V6 calibration, from the same table.
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
        blocks,
        dist: StoredDist::V6 { d },
        ..pats_v3()
    }
}

/// What `parse_metadata` reads from `Records::x5_like` of Pat's two
/// calibrations, as x5_like's doc lists its fields, where field 98 counts
/// `counted` frames (one more than the tracks hold, as the X5 counts them
/// on a master) and field 93 is `duration_ms`.
fn x5_like_metadata(counted: u64, duration_ms: u64) -> Metadata {
    let printed = |cal: &CalibTruth| {
        let s = insv::print(cal, &planted());
        CalibStrings {
            working: Some(s.clone()),
            original: Some(s),
        }
    };
    Metadata {
        serial: Some("SYNTH00001".into()),
        model: Some("Insta360 X5".into()),
        firmware: Some("v1.1.22_build1".into()),
        first_frame_us: Some(1_000_000),
        window: Some(Window {
            src: [5376; 2],
            dst: [5312; 2],
            offset: [0; 2],
        }),
        layout: Some(2),
        track_order: Some(1),
        duration_ms: Some(duration_ms),
        frames: Some(FrameSpan {
            first_us: 1_000_000,
            frames: counted,
            ..FrameSpan::default()
        }),
        stream_type: Some(4),
        codec: Some(2),
        offset_version: Some(4),
        calib: Calibrations {
            v3: printed(&pats_v3()),
            v6: printed(&pats_v6()),
            ..Calibrations::default()
        },
        // Fields 62, 64 and 65.
        unknown: 3,
        ..Metadata::default()
    }
}

// The trailer's round trip.

/// The top-level fields `parse_metadata` reads (M3 plan 5.1).
const KNOWN: [u32; 25] = [
    1, 2, 3, 5, 17, 24, 25, 26, 27, 44, 53, 54, 55, 56, 79, 80, 93, 98, 111, 112, 131, 132, 135,
    136, 145,
];

/// A file of `at` bytes before synth's `inst` box and the box's own
/// after it. The bytes before read as a pattern and are held nowhere, so
/// that a box can sit anywhere in a file, past 4 GiB too.
struct Placed {
    at: u64,
    inst: Vec<u8>,
    pos: u64,
}

impl Placed {
    fn len(&self) -> u64 {
        self.at + self.inst.len() as u64
    }
}

impl Read for Placed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(k) = self.pos.checked_sub(self.at) {
            let rest = self.inst.get(k as usize..).unwrap_or_default();
            let n = buf.len().min(rest.len());
            buf[..n].copy_from_slice(&rest[..n]);
            self.pos += n as u64;
            return Ok(n);
        }
        let n = (buf.len() as u64).min(self.at - self.pos) as usize;
        for (i, b) in buf[..n].iter_mut().enumerate() {
            *b = ((self.pos + i as u64) % 251) as u8;
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Placed {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let to = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(d) => self.len().checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = to.ok_or_else(|| io::Error::other("a seek before the start"))?;
        Ok(self.pos)
    }
}

/// Whether `bytes`, walked by camera's walker, are synth's `fields`, field
/// for field, and each message's fields the same again; or the first
/// field that isn't.
fn walks_as(bytes: &[u8], fields: &[Field]) -> Result<(), String> {
    let got: Vec<_> = Walker::new(bytes, Part::Record(1), 0)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("the walk: {e:?}"))?;
    if got.len() != fields.len() {
        return Err(format!("{} fields read of {}", got.len(), fields.len()));
    }
    for (g, f) in got.iter().zip(fields) {
        let same = match (&f.value, g.value) {
            (insv::Value::Message(m), Value::Len(b)) => {
                walks_as(b, m)?;
                true
            }
            (insv::Value::Varint(a), Value::Varint(b)) => *a == b,
            (insv::Value::Fixed64(a), Value::I64(b)) => *a == b,
            (insv::Value::Fixed32(a), Value::I32(b)) => *a == b,
            (insv::Value::Bytes(a), Value::Len(b)) => a[..] == *b,
            _ => false,
        };
        if g.number != f.number || !same {
            return Err(format!("wrote {f:?}, read {g:?}"));
        }
    }
    Ok(())
}

/// Record 3's bytes as insta360.md §3.4 lays the X5's raw items out:
/// `{u64 t_us, u16 × 6}`, little-endian, accelerometer first.
fn imu_bytes(items: &[ImuItem]) -> Vec<u8> {
    let mut b = Vec::new();
    for item in items {
        b.extend(item.t_us.to_le_bytes());
        for v in item.accel.iter().chain(&item.gyro) {
            b.extend(v.to_le_bytes());
        }
    }
    b
}

/// The ids synth writes for `records`: 1 always, 2 with a thumbnail, 3
/// and 4 when they hold an item, and the others', in slot order.
fn ids(records: &Records) -> Vec<u8> {
    let mut ids = vec![1];
    if records.thumbnail.is_some() {
        ids.push(2);
    }
    if !records.imu.is_empty() {
        ids.push(3);
    }
    if !records.exposure.is_empty() {
        ids.push(4);
    }
    ids.extend(records.others.iter().map(|r| r.id));
    ids.sort();
    ids
}

/// Synth's box in `file`, read back through camera's readers and
/// parsers and held to what synth was given: `layout` ending at the
/// file's end with its index where the X5's is, the trailer as `layout`
/// says, every record listed, each payload as `records` has it, and the
/// metadata walked field for field and parsed to `meta`. Or the first
/// thing that differs.
fn round_trips(
    file: &mut Placed,
    records: &Records,
    layout: &TrailerLayout,
    meta: &Metadata,
) -> Result<(), String> {
    let len = file.len();
    // 31 slots, or one more than the largest id.
    let slots = layout
        .slots
        .last()
        .map_or(31, |s| (u64::from(s.id) + 1).max(31));
    if layout.payload_start + layout.size != len || layout.index != len - 78 - 10 * slots {
        return Err(format!("{layout:?} in a file of {len} bytes"));
    }
    let t = match read_trailer(file, len) {
        Ok(Some(t)) => t,
        got => return Err(format!("read_trailer: {got:?}")),
    };
    let slot = |id: u8| layout.index + 10 * u64::from(id);
    let want = Trailer {
        version: 3,
        payload_start: layout.payload_start,
        records: layout
            .slots
            .iter()
            .map(|s| RecordRef {
                id: s.id,
                format: s.format,
                offset: s.offset,
                size: s.size,
                slot: slot(s.id),
            })
            .collect(),
        skipped: Vec::new(),
    };
    if t != want {
        return Err(format!("read {t:?}, synth laid out {want:?}"));
    }
    let listed: Vec<u8> = t.records.iter().map(|r| r.id).collect();
    if listed != ids(records) {
        return Err(format!("records {listed:?} listed of {:?}", ids(records)));
    }
    for r in &t.records {
        let id = r.id;
        let payload = match read_record(file, &t, id, MAX_METADATA.max(r.size as usize)) {
            Ok(Some(p)) => p,
            got => return Err(format!("record {id}: {got:?}")),
        };
        let fails = |what: &dyn std::fmt::Debug| format!("record {id}: {what:?}");
        match id {
            1 => {
                walks_as(&payload, &records.metadata)?;
                let m = parse_metadata(&payload).map_err(|e| fails(&e))?;
                if m != *meta {
                    return Err(format!("parsed {m:?}, synth wrote {meta:?}"));
                }
            }
            2 => {
                let Some(th) = &records.thumbnail else {
                    return Err("a thumbnail synth didn't write".into());
                };
                let header: Vec<u32> = payload[..40]
                    .chunks(4)
                    .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
                    .collect();
                let size = payload.len() as u32;
                if header != [1, size, 1, 0, th.width, th.height, 0, 0, 0, 0] {
                    return Err(fails(&header));
                }
                let want = Nv12Image {
                    width: th.width,
                    height: th.height,
                    y: th.y.clone(),
                    uv: th.uv.clone(),
                };
                let image = thumbnail(&payload).map_err(|e| fails(&e))?;
                if image != want {
                    return Err(fails(&image));
                }
            }
            3 => {
                let span = imu_span(&payload).map_err(|e| fails(&e))?;
                let (first, last) = (records.imu.first(), records.imu.last());
                let want = (
                    records.imu.len() as u64,
                    first.map(|i| i.t_us),
                    last.map(|i| i.t_us),
                );
                let got = span.map(|s| (s.samples, Some(s.first_us), Some(s.last_us)));
                if got != Some(want) || payload != imu_bytes(&records.imu) {
                    return Err(fails(&span));
                }
            }
            4 => {
                let items = parse_exposure(&payload).map_err(|e| fails(&e))?;
                let got: Vec<(u64, u64)> = items
                    .iter()
                    .map(|e| (e.t_us, e.seconds.to_bits()))
                    .collect();
                let want: Vec<(u64, u64)> = records
                    .exposure
                    .iter()
                    .map(|e| (e.t_us, e.seconds.to_bits()))
                    .collect();
                if got != want {
                    return Err(fails(&items));
                }
            }
            _ => {
                let raw = records.others.iter().find(|o| o.id == id);
                if raw.map(|o| (o.format, &o.payload[..])) != Some((r.format, &payload[..])) {
                    return Err(fails(&payload));
                }
            }
        }
    }
    Ok(())
}

/// Short text of any characters, controls among them, as a protobuf string may hold.
fn text() -> impl Strategy<Value = String> {
    vec(any::<char>(), 0..12).prop_map(String::from_iter)
}

/// Any double but a NaN, which no `PartialEq` holds equal to itself.
fn double() -> impl Strategy<Value = f64> {
    use proptest::num::f64::{INFINITE, NEGATIVE, NORMAL, POSITIVE, SUBNORMAL, ZERO};
    POSITIVE | NEGATIVE | NORMAL | SUBNORMAL | ZERO | INFINITE
}

/// A value of a field camera doesn't read: any wire type, and messages a
/// level deep.
fn unknown_value() -> impl Strategy<Value = insv::Value> {
    let leaf = || {
        prop_oneof![
            any::<u64>().prop_map(insv::Value::Varint),
            any::<u64>().prop_map(insv::Value::Fixed64),
            any::<u32>().prop_map(insv::Value::Fixed32),
            vec(any::<u8>(), 0..16).prop_map(insv::Value::Bytes),
        ]
    };
    prop_oneof![
        4 => leaf(),
        1 => vec((1u32..40, leaf()), 0..4).prop_map(|fields| {
            insv::Value::Message(
                fields
                    .into_iter()
                    .map(|(number, value)| Field { number, value })
                    .collect(),
            )
        }),
    ]
}

/// A metadata record's fields as synth writes them, in any order, and
/// what `parse_metadata` reads from them.
#[derive(Debug, Clone)]
struct Meta {
    fields: Vec<Field>,
    want: Metadata,
}

/// Each field `parse_metadata` reads, or not, with any value of its
/// type; and fields it doesn't read, which it counts.
fn meta() -> impl Strategy<Value = Meta> {
    let strings = [option::of(text()), option::of(text()), option::of(text())];
    let times = (option::of(any::<i64>()), option::of(double()));
    let group = (any::<[u64; 3]>(), option::of(text()));
    let profiles = (
        vec((text(), vec(double(), 0..6)), 0..3),
        vec((text(), double()), 0..3),
    );
    let messages = (
        option::of(group),
        option::of(any::<[u32; 6]>()),
        option::of(any::<[u32; 2]>()),
        option::of(any::<[u64; 4]>()),
        option::of(profiles),
    );
    let numbers: [_; 7] = std::array::from_fn(|_| option::of(any::<u64>()));
    let calibration: [_; 8] = std::array::from_fn(|_| option::of(text()));
    let unknown_number = prop_oneof![4u32..200, 200u32..(1 << 29)]
        .prop_filter("a field camera reads", |n| !KNOWN.contains(n));
    let unknown = vec((unknown_number, unknown_value()), 0..6);
    (strings, times, messages, numbers, calibration, unknown).prop_flat_map(
        |(strings, times, messages, numbers, calibration, unknown)| {
            let mut fields = Vec::new();
            let mut want = Metadata::default();
            let mut string = |n: u32, s: &Option<String>| {
                if let Some(s) = s {
                    fields.push(Field::string(n, s));
                }
                s.clone()
            };
            want.serial = string(1, &strings[0]);
            want.model = string(2, &strings[1]);
            want.firmware = string(3, &strings[2]);
            let cal = |s: &mut dyn FnMut(u32, &Option<String>) -> Option<String>, k: usize| {
                CalibStrings {
                    working: s(KNOWN_CALIBRATION[2 * k], &calibration[2 * k]),
                    original: s(KNOWN_CALIBRATION[2 * k + 1], &calibration[2 * k + 1]),
                }
            };
            want.calib = Calibrations {
                v1: cal(&mut string, 0),
                v2: cal(&mut string, 1),
                v3: cal(&mut string, 2),
                v6: cal(&mut string, 3),
            };
            let (first, shutter) = times;
            if let Some(us) = first {
                fields.push(Field::varint(24, us as u64));
                want.first_frame_us = Some(us);
            }
            if let Some(ms) = shutter {
                fields.push(Field::double(25, ms));
                want.rolling_shutter_ms = Some(ms);
            }
            let (group, window, size, span, profiles) = messages;
            if let Some(([kind, index, total], identify)) = group {
                let mut g = vec![Field::varint(1, kind), Field::varint(2, index)];
                if let Some(path) = &identify {
                    g.push(Field::string(3, path));
                }
                g.push(Field::varint(4, total));
                fields.push(Field::message(26, g));
                want.file_group = Some(FileGroup {
                    kind,
                    index,
                    identify,
                    total,
                });
            }
            if let Some(w) = window {
                let items = (1..).zip(w).map(|(n, v)| Field::varint(n, v.into()));
                fields.push(Field::message(27, items.collect()));
                want.window = Some(Window {
                    src: [w[0], w[1]],
                    dst: [w[2], w[3]],
                    offset: [w[4], w[5]],
                });
            }
            if let Some([w, h]) = size {
                let items = vec![Field::varint(1, w.into()), Field::varint(2, h.into())];
                fields.push(Field::message(44, items));
                want.video_size = Some([w, h]);
            }
            if let Some([first_us, first_unix_ms, last_unix_ms, frames]) = span {
                // Item 2, which camera doesn't keep, among them.
                let items = [first_us, first_us ^ 1, first_unix_ms, last_unix_ms, frames];
                let items = (1..).zip(items).map(|(n, v)| Field::varint(n, v));
                fields.push(Field::message(98, items.collect()));
                want.frames = Some(FrameSpan {
                    first_us,
                    first_unix_ms,
                    last_unix_ms,
                    frames,
                });
            }
            if let Some((polynomials, fovs)) = profiles {
                let mut items = Vec::new();
                for (name, cs) in &polynomials {
                    let mut item = vec![Field::string(1, name)];
                    item.extend(cs.iter().map(|&c| Field::double(2, c)));
                    items.push(Field::message(1, item));
                }
                for (name, deg) in &fovs {
                    let item = vec![Field::string(1, name), Field::double(2, *deg)];
                    items.push(Field::message(2, item));
                }
                fields.push(Field::message(145, items));
                want.lens_profiles = Some(LensProfiles { polynomials, fovs });
            }
            let slots = [
                &mut want.layout,
                &mut want.track_order,
                &mut want.duration_ms,
                &mut want.stream_type,
                &mut want.codec,
                &mut want.bitrate_mbps,
                &mut want.offset_version,
            ];
            for ((n, v), slot) in [79, 80, 93, 131, 132, 135, 136]
                .into_iter()
                .zip(numbers)
                .zip(slots)
            {
                if let Some(v) = v {
                    fields.push(Field::varint(n, v));
                    *slot = Some(v);
                }
            }
            want.unknown = unknown.len() as u32;
            fields.extend(
                unknown
                    .into_iter()
                    .map(|(number, value)| Field { number, value }),
            );
            Just(fields).prop_shuffle().prop_map(move |fields| Meta {
                fields,
                want: want.clone(),
            })
        },
    )
}

/// The calibration strings' fields, each generation's working string and
/// its original: V1, V2, V3 and V6.
const KNOWN_CALIBRATION: [u32; 8] = [5, 17, 53, 55, 54, 56, 111, 112];

/// A thumbnail of any even size up to 10 × 6, with any bytes.
fn thumbnails() -> impl Strategy<Value = Thumbnail> {
    (0u32..6, 0u32..4).prop_flat_map(|(w, h)| {
        let (width, height) = (2 * w, 2 * h);
        let luma = (width * height) as usize;
        (vec(any::<u8>(), luma), vec(any::<u8>(), luma / 2)).prop_map(move |(y, uv)| Thumbnail {
            width,
            height,
            y,
            uv,
        })
    })
}

/// Any records synth writes: any metadata, a thumbnail or none, any IMU
/// and exposure items, and up to five other records, each id once, from
/// 5 to 63, so that the index has from 31 to 64 slots; with what
/// `parse_metadata` reads.
fn records() -> impl Strategy<Value = (Records, Metadata)> {
    let imu = (any::<u64>(), any::<[u16; 3]>(), any::<[u16; 3]>())
        .prop_map(|(t_us, accel, gyro)| ImuItem { t_us, accel, gyro });
    let exposure =
        (any::<u64>(), any::<f64>()).prop_map(|(t_us, seconds)| ExposureItem { t_us, seconds });
    let others = subsequence((5u8..64).collect::<Vec<u8>>(), 0..6)
        .prop_shuffle()
        .prop_flat_map(|ids| {
            let n = ids.len();
            (Just(ids), vec((0u8..3, vec(any::<u8>(), 0..40)), n))
        })
        .prop_map(|(ids, bodies)| {
            ids.into_iter()
                .zip(bodies)
                .map(|(id, (format, payload))| RawRecord {
                    id,
                    format,
                    payload,
                })
                .collect::<Vec<_>>()
        });
    (
        meta(),
        option::of(thumbnails()),
        vec(imu, 0..12),
        vec(exposure, 0..12),
        others,
    )
        .prop_map(|(meta, thumbnail, imu, exposure, others)| {
            let records = Records {
                metadata: meta.fields,
                thumbnail,
                imu,
                exposure,
                others,
            };
            (records, meta.want)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn the_trailer_round_trips_through_synths_writer_cases(
        (records, want) in records(),
        at in prop_oneof![Just(0u64), 0u64..5000, 0u64..(1 << 40)],
        align in prop_oneof![8 => Just(1u64), 8 => Just(16), 4 => Just(4096), 1 => Just(X5_ALIGN)],
        footer in (any::<Index>(), 0usize..6, 1u8..=255),
    ) {
        let (inst, layout) = insv::inst_box(&records, at, align, None);
        let mut file = Placed { at, inst, pos: 0 };
        prop_assert_eq!(round_trips(&mut file, &records, &layout, &want), Ok(()));

        // The control: one of the footers synth wrote, a byte of it
        // changed, is refused by the reader at that footer.
        let (k, byte, x) = footer;
        let r = layout.slots[k.index(layout.slots.len())];
        let at_footer = r.offset + u64::from(r.size);
        let i = (at_footer - at) as usize + byte;
        file.inst[i] ^= x;
        let e = must_fail("a footer of synth's changed", round_trips(&mut file, &records, &layout, &want));
        prop_assert_eq!(
            e,
            format!("read_trailer: Err(FooterMismatch {{ id: {}, offset: {at_footer} }})", r.id)
        );
        file.inst[i] ^= x;
    }
}

/// A change to what a file is held to: its records, its metadata as
/// parsed, and its layout.
type Change = fn(&mut Records, &mut Metadata, &mut TrailerLayout);

/// Property: records synth writes into an `inst` box, at any offset and
/// alignment, read back through `read_trailer` and `read_record` as
/// synth's `TrailerLayout` says, payload for payload, and the metadata
/// and exposure parse to what synth wrote.
///
/// First an X5-like trailer at the X5's alignment, after an MP4 of
/// 100,003 bytes, with a thumbnail and two of the X5's other records;
/// then any records, after any bytes, at alignments of 1, 16, 4096 and
/// 128 KiB. The X5-like file's controls change what it is held to, once
/// for each of `round_trips`' checks, so that only that check sees the
/// change and fails. Each case's control changes a byte of one of
/// synth's footers, which the reader must refuse there.
#[test]
fn the_trailer_round_trips_through_synths_writer() {
    let _time = timed("the_trailer_round_trips_through_synths_writer");
    let mut records = Records::x5_like(
        &[pats_v3(), pats_v6()],
        &planted(),
        114,
        FrameRate::FPS_29_97,
    );
    records.thumbnail = Some(Thumbnail {
        width: 64,
        height: 32,
        y: vec![100; 64 * 32],
        uv: vec![128; 64 * 16],
    });
    // Record 10 first, right after the box's header, then 9 on a
    // boundary.
    records.others = vec![
        RawRecord {
            id: 10,
            format: 0,
            payload: vec![10; 30],
        },
        RawRecord {
            id: 9,
            format: 2,
            payload: br#"{"synth":1}"#.to_vec(),
        },
    ];
    let at = 100_003;
    let (inst, layout) = insv::inst_box(&records, at, X5_ALIGN, None);
    let mut file = Placed { at, inst, pos: 0 };
    // Clip 1's 114 frames a track at 29.97: field 98 counts 115, the
    // X5's count on a master, and field 93 is ⌊115 × 1001 / 30⌋ = 3837
    // ms, as clip 1's master has them (tier2_samples.rs).
    let meta = x5_like_metadata(115, 3837);
    assert_eq!(round_trips(&mut file, &records, &layout, &meta), Ok(()));

    // The controls, in the order of the checks they reach: each change,
    // and how the error it fails with starts.
    fn field(r: &mut Records, n: u32) -> &mut Field {
        r.metadata.iter_mut().find(|f| f.number == n).unwrap()
    }
    fn other(r: &mut Records, id: u8) -> &mut RawRecord {
        r.others.iter_mut().find(|o| o.id == id).unwrap()
    }
    let controls: [(&str, Change, &str); 12] = [
        (
            "the trailer a byte longer than synth laid it out",
            |_, _, l| l.size += 1,
            "TrailerLayout {",
        ),
        (
            "the index a slot later than synth wrote it",
            |_, _, l| l.index += 10,
            "TrailerLayout {",
        ),
        (
            "record 4 a byte later than synth wrote it",
            |_, _, l| l.slots[3].offset += 1,
            "read Trailer {",
        ),
        (
            "no thumbnail",
            |r, _, _| r.thumbnail = None,
            "records [1, 2, 3, 4, 9, 10] listed of [1, 3, 4, 9, 10]",
        ),
        (
            "field 79 written as 3",
            |r, _, _| field(r, 79).value = insv::Value::Varint(3),
            "wrote Field { number: 79, value: Varint(3) }, read Field { number: 79, value: Varint(2),",
        ),
        (
            "another serial parsed",
            |_, m, _| m.serial = Some("SYNTH00002".into()),
            "parsed Metadata {",
        ),
        (
            "the thumbnail's sides swapped",
            |r, _, _| {
                let t = r.thumbnail.as_mut().unwrap();
                (t.width, t.height) = (t.height, t.width);
            },
            "record 2: [1, 3112, 1, 0, 64, 32, 0, 0, 0, 0]",
        ),
        (
            "the thumbnail's last chroma byte",
            |r, _, _| *r.thumbnail.as_mut().unwrap().uv.last_mut().unwrap() ^= 1,
            "record 2: Nv12Image {",
        ),
        (
            "the last IMU item a microsecond later",
            |r, _, _| r.imu.last_mut().unwrap().t_us += 1,
            "record 3: Some(ImuSpan {",
        ),
        (
            "an IMU item's gyro",
            |r, _, _| r.imu[7].gyro[2] ^= 1,
            "record 3: Some(ImuSpan {",
        ),
        (
            "an exposure of 1/250 s",
            |r, _, _| r.exposure[5].seconds = 1.0 / 250.0,
            "record 4: [Exposure {",
        ),
        (
            "record 9's last byte",
            |r, _, _| *other(r, 9).payload.last_mut().unwrap() ^= 1,
            "record 9: [123, 34,",
        ),
    ];
    for (name, change, fails_with) in controls {
        let (mut r, mut m, mut l) = (records.clone(), meta.clone(), layout.clone());
        change(&mut r, &mut m, &mut l);
        let e = must_fail(name, round_trips(&mut file, &r, &l, &m));
        assert!(e.starts_with(fails_with), "{name}: {e}");
    }
    // And record 10's format, the other half of the others' check.
    let mut r = records.clone();
    other(&mut r, 10).format = 2;
    let e = must_fail(
        "record 10 said to be JSON",
        round_trips(&mut file, &r, &layout, &meta),
    );
    assert!(e.starts_with("record 10: [10, 10,"), "{e}");
    the_trailer_round_trips_through_synths_writer_cases();
}

// The first frame and the LRV.

/// Synth's file of `records`, its box at the file's start and its records
/// on the X5's boundaries, with its trailer and its metadata and exposure
/// as camera reads them.
fn read_back(records: &Records) -> (Placed, Trailer, Metadata, Vec<Exposure>) {
    let (inst, _) = insv::inst_box(records, 0, X5_ALIGN, None);
    let mut file = Placed {
        at: 0,
        inst,
        pos: 0,
    };
    let len = file.len();
    let t = read_trailer(&mut file, len).unwrap().unwrap();
    let mut record = |id| {
        read_record(&mut file, &t, id, MAX_METADATA)
            .unwrap()
            .unwrap()
    };
    let m = parse_metadata(&record(1)).unwrap();
    let exposure = parse_exposure(&record(4)).unwrap();
    (file, t, m, exposure)
}

/// The X5's 119.88 fps, which core names no constant for.
fn fps_119_88() -> FrameRate {
    FrameRate::new(Rational::new(120000, 1001).unwrap()).unwrap()
}

/// Synth's X5-like file at each of the X5's rates, read through camera,
/// has its first frame among earlier exposure items, where the X5's is:
/// the item whose stamp is field 24's is item 5 at 25 and 29.97 fps, 6 at
/// 59.94 and 10 at 119.88 (Tier 2c's `first_entry` on Pat's files, which
/// is synth's spec); field 98's item 1 is the same time and its count
/// the tracks' frames + 1; each frame's item from there is a frame on
/// from field 24's time, to the µs; and 2 to 6 items follow the last
/// frame's, as on the X5. The tracks' frames are Pat's takes' at each
/// rate.
///
/// Controls: the first frame taken as exposure item 0, whose stamp isn't
/// field 24's; field 24 held to the next item's stamp; and the frames
/// timed from item 0.
#[test]
fn the_first_frame_is_found_among_earlier_exposure_items() {
    let _time = timed("the_first_frame_is_found_among_earlier_exposure_items");
    let cals = [pats_v3(), pats_v6()];
    let rates = [
        (FrameRate::FPS_25, 116, 5),
        (FrameRate::FPS_29_97, 114, 5),
        (FrameRate::FPS_59_94, 190, 6),
        (fps_119_88(), 586, 10),
    ];
    for (rate, frames, first) in rates {
        let records = Records::x5_like(&cals, &planted(), frames, rate);
        let (_, _, m, exposure) = read_back(&records);
        let first_us = m.first_frame_us.unwrap() as u64;
        // The frames' items from `from`, each within a µs of its time
        // from field 24's: the first that isn't.
        let r = rate.as_rational();
        let off = |from: usize| {
            (0..frames as usize).find(|&k| {
                let want = first_us as f64 + k as f64 * 1e6 * r.den() as f64 / r.num() as f64;
                (exposure[from + k].t_us as f64 - want).abs() > 1.0
            })
        };
        let at = exposure.iter().position(|e| e.t_us == first_us);
        assert_eq!(at, Some(first), "{rate:?}");
        let span = m.frames.unwrap();
        assert_eq!(
            (span.first_us, span.frames),
            (first_us, u64::from(frames) + 1),
            "{rate:?}"
        );
        assert_eq!(off(first), None, "{rate:?}");
        let after = exposure.len() - first - frames as usize;
        assert!(
            (2..=6).contains(&after),
            "{rate:?}: {after} items after the last frame's"
        );

        must_fail(
            &format!("the first frame at {rate:?} taken as exposure item 0"),
            exposure[0].t_us == first_us,
        );
        must_fail(
            &format!("field 24 at {rate:?} at the next exposure item"),
            exposure[first + 1].t_us == first_us,
        );
        must_fail(
            &format!("the frames at {rate:?} timed from item 0"),
            off(0).is_none(),
        );
    }
}

/// An LRV whose records `Records::x5_lrv` copies from its master's
/// verifies against the master whatever its length, as the X5's 5.7K60
/// LRV of 95 frames verifies against its 190-frame master by its 200
/// exposure items; and camera reads the LRV's own count in field 98, the
/// master's length in field 93, no fields 79 and 80, and 131 and 132 1.
///
/// Control: the LRV `Records::x5_like` writes for its own 95 frames,
/// whose exposure record (4) has 105 items to the master's 200, is paired
/// by name only.
#[test]
fn an_lrv_with_its_masters_exposure_record_verifies() {
    let _time = timed("an_lrv_with_its_masters_exposure_record_verifies");
    let cals = [pats_v3(), pats_v6()];
    let master = Records::x5_like(&cals, &planted(), 190, FrameRate::FPS_59_94);
    let (mut m, mt, mm, _) = read_back(&master);
    let mut pair = |lrv: &Records| {
        let (mut l, lt, lm, _) = read_back(lrv);
        let p = verify_companion((&mut m, &mt), (&mut l, &lt), 1 << 20).unwrap();
        (p, lm)
    };
    let (p, lm) = pair(&Records::x5_lrv(&master, 95));
    assert_eq!(p, Pairing::Verified);
    assert_eq!(lm.frames.unwrap().frames, 95);
    assert_eq!(mm.frames.unwrap().frames, 191);
    assert_eq!(
        (
            lm.duration_ms,
            lm.layout,
            lm.track_order,
            lm.stream_type,
            lm.codec
        ),
        (Some(3186), None, None, Some(1), Some(1))
    );
    assert_eq!(lm.calib, mm.calib);

    let (p, _) = pair(&Records::x5_like(
        &cals,
        &planted(),
        95,
        FrameRate::FPS_59_94,
    ));
    must_fail(
        "an LRV of its own 95 frames, verified",
        p == Pairing::Verified,
    );
    assert_eq!(
        p,
        Pairing::NameOnly {
            why: "their exposure records (4) differ in size: 3200 bytes in the master, 1680 in the LRV".into()
        }
    );
}

// The cross parse.

/// What the cross parse found wrong.
#[derive(Debug)]
enum Miss {
    /// camera refused the string, or read it otherwise than as printed.
    Read(String),
    /// A number not printed to the X5's decimals.
    Decimals {
        block: usize,
        name: String,
        text: String,
    },
    /// A number read back further from the truth than half a unit in its
    /// last printed place.
    Off {
        block: usize,
        name: String,
        off: f64,
        bound: f64,
    },
}

/// The X5's decimals for each kind of number (insta360.md, "Pat's X5
/// samples"): ξ and t to 6, the focal lengths, the centre and the angles
/// to 3, the distortion terms to 8.
const XI_T: usize = 6;
const PIXELS_ANGLES: usize = 3;
const TERMS: usize = 8;

/// `s`, synth's string of `cal`, parsed by camera and held to `cal`'s
/// numbers: each token to the X5's decimals, and within half a unit in
/// its last place of the truth's number, the f64 rounding of reading it
/// apart; the canvas, lens type and packed word exactly. Or the first
/// thing that isn't.
fn cross_parses(cal: &CalibTruth, s: &str) -> Result<(), Miss> {
    let (generation, code) = generation(cal);
    let c = calib::parse(generation, s).map_err(|e| Miss::Read(e.to_string()))?;
    let word = (code << 16) | (u32::from(cal.body) << 8);
    let integers = (c.canvas, c.lens_type, c.word);
    let want = ((cal.canvas[0], cal.canvas[1]), cal.lens_type, word);
    if c.generation != generation || integers != want {
        return Err(Miss::Read(format!("{integers:?}, not {want:?}")));
    }
    for (b, (truth, lens)) in cal.blocks.iter().zip(&c.lenses).enumerate() {
        let Focal::Mei { xi, fx, fy } = &lens.focal else {
            return Err(Miss::Read(format!("block {b}'s focal {:?}", lens.focal)));
        };
        let Some(t) = &lens.t else {
            return Err(Miss::Read(format!("block {b} has no t")));
        };
        let terms = match cal.dist {
            StoredDist::V3 { k, p } => [&k[b][..], &p[b][..]].concat(),
            StoredDist::V6 { d } => d[b].to_vec(),
        };
        if lens.coefficients.len() != terms.len() {
            return Err(Miss::Read(format!("block {b}'s terms")));
        }
        let mut numbers: Vec<(String, &Token, f64, usize)> = vec![
            ("xi".into(), xi, truth.xi, XI_T),
            ("fx".into(), fx, truth.fx, PIXELS_ANGLES),
            ("fy".into(), fy, truth.fy, PIXELS_ANGLES),
            ("cx".into(), &lens.cx, truth.cx, PIXELS_ANGLES),
            ("cy".into(), &lens.cy, truth.cy, PIXELS_ANGLES),
        ];
        for (i, name) in ["yaw", "pitch", "roll"].into_iter().enumerate() {
            numbers.push((name.into(), &lens.ypr[i], truth.ypr_deg[i], PIXELS_ANGLES));
        }
        for (i, name) in ["tx", "ty", "tz"].into_iter().enumerate() {
            numbers.push((name.into(), &t[i], truth.t_m[i], XI_T));
        }
        for (i, (token, &v)) in lens.coefficients.iter().zip(&terms).enumerate() {
            numbers.push((format!("term {i}"), token, v, TERMS));
        }
        for (name, token, v, decimals) in numbers {
            let places = token.text().split_once('.').map(|(_, f)| f.len());
            if places != Some(decimals) {
                return Err(Miss::Decimals {
                    block: b,
                    name,
                    text: token.text().to_owned(),
                });
            }
            // Half a unit in the last place, and a few of the f64's own
            // at the number's size for reading the token.
            let bound =
                0.5 * 10f64.powi(-(decimals as i32)) + 4.0 * f64::EPSILON * v.abs().max(1.0);
            let off = (token.value() - v).abs();
            if off.is_nan() || off > bound {
                return Err(Miss::Off {
                    block: b,
                    name,
                    off,
                    bound,
                });
            }
        }
    }
    Ok(())
}

/// The generation of `cal`'s string, and its code in the packed word.
fn generation(cal: &CalibTruth) -> (Generation, u32) {
    match cal.dist {
        StoredDist::V3 { .. } => (Generation::V3, 3),
        StoredDist::V6 { .. } => (Generation::V6, 6),
    }
}

/// `s` parsed by camera as `cal`'s generation's string and printed back
/// by camera's printer: byte for byte `want`, synth's print of `cal`. Or
/// what camera printed instead, or why it refused `s`.
fn reprints(cal: &CalibTruth, s: &str, want: &str) -> Result<(), String> {
    let c = calib::parse(generation(cal).0, s).map_err(|e| e.to_string())?;
    let back = calib::print(&c);
    if back == want { Ok(()) } else { Err(back) }
}

/// A number around `centre`, mostly within `spread` of it; sometimes
/// exactly it, sometimes within 10⁻⁷ of zero (so that it prints as zero,
/// a negative one with its minus), sometimes anything within a million.
fn around(centre: f64, spread: f64) -> impl Strategy<Value = f64> {
    prop_oneof![
        12 => (centre - spread)..(centre + spread),
        1 => Just(centre),
        1 => -1e-7f64..1e-7,
        1 => -1e6f64..1e6,
    ]
}

/// A block around one of Pat's: ξ about 2, focal lengths about 4285,
/// the centre within 60 px of `cx`, angles within 3° (the roll about
/// 90°) and t within 10 mm (tz about −32 mm).
fn block(cx: f64) -> impl Strategy<Value = BlockTruth> {
    (
        around(2.0, 0.5),
        [around(4285.0, 300.0), around(4285.0, 300.0)],
        [around(cx, 60.0), around(2688.0, 60.0)],
        [around(0.0, 3.0), around(0.0, 3.0), around(90.0, 3.0)],
        [around(0.0, 0.01), around(0.0, 0.01), around(-0.032, 0.01)],
    )
        .prop_map(|(xi, [fx, fy], [cx, cy], ypr_deg, t_m)| BlockTruth {
            xi,
            fx,
            fy,
            cx,
            cy,
            ypr_deg,
            t_m,
        })
}

/// A calibration around Pat's, of either generation: its terms around
/// zero, within the range of Pat's (−8.1 to 2.1), d4 sometimes exactly 0;
/// the X5's canvas, lens type and body mostly, any otherwise.
fn calibrations() -> impl Strategy<Value = CalibTruth> {
    let term = || around(0.0, 9.0);
    let dist = prop_oneof![
        (vec(term(), 6), vec(term(), 4)).prop_map(|(k, p)| StoredDist::V3 {
            k: [[k[0], k[1], k[2]], [k[3], k[4], k[5]]],
            p: [[p[0], p[1]], [p[2], p[3]]],
        }),
        vec(term(), 26).prop_map(|d| StoredDist::V6 {
            d: [
                std::array::from_fn(|i| d[i]),
                std::array::from_fn(|i| d[13 + i]),
            ],
        }),
    ];
    (
        [block(2700.0), block(8070.0)],
        dist,
        prop_oneof![3 => Just([10752u32, 5376]), 1 => any::<[u32; 2]>()],
        any::<u32>(),
        prop_oneof![3 => Just(113u32), 1 => any::<u32>()],
        prop_oneof![3 => Just(4u8), 1 => any::<u8>()],
    )
        .prop_map(|(blocks, dist, canvas, dst, lens_type, body)| CalibTruth {
            blocks,
            dist,
            canvas,
            dst,
            lens_type,
            body,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn calib_parses_synths_print_cases(cal in calibrations()) {
        let s = insv::print(&cal, &planted());
        prop_assert!(cross_parses(&cal, &s).is_ok(), "{:?} for {s}", cross_parses(&cal, &s));
        // And camera prints it back as synth did.
        prop_assert_eq!(reprints(&cal, &s, &s), Ok(()));
    }
}

/// Property: `calib::parse(synth::insv::print(truth))` gives each of the
/// truth's numbers back within half a unit in its last printed place,
/// for V3 and V6 truths around Pat's numbers (amendment 3).
///
/// First Pat's two calibrations, then any around them; and camera's
/// printer gives synth's string back from what camera parsed. Its
/// controls, on Pat's V3: each block held to the other's truth; the focal
/// length a unit off in its last place, which is more than half a unit;
/// the focal length printed a decimal short; the string held to Pat's V6
/// truth, as another generation's, which camera refuses; the string held
/// to a truth of another lens type, and of another body; and the focal
/// length printed with a zero more, the same number, which camera prints
/// back as it was given rather than as synth printed it.
#[test]
fn calib_parses_synths_print() {
    let _time = timed("calib_parses_synths_print");
    let p = planted();
    for cal in [pats_v3(), pats_v6()] {
        let s = insv::print(&cal, &p);
        assert!(cross_parses(&cal, &s).is_ok(), "{s}");
        assert_eq!(reprints(&cal, &s, &s), Ok(()));
    }
    let v3 = pats_v3();
    let s = insv::print(&v3, &p);
    let mut swapped = v3;
    swapped.blocks.swap(0, 1);
    let e = must_fail(
        "each block held to the other's truth",
        cross_parses(&swapped, &s),
    );
    assert!(
        matches!(&e, Miss::Off { block: 0, name, off, .. } if name == "fx" && (off - 6.41).abs() < 1e-9),
        "{e:?}"
    );
    let unit_off = s.replacen("_4284.620_", "_4284.621_", 1);
    let e = must_fail("fx a unit off", cross_parses(&pats_v3(), &unit_off));
    assert!(
        matches!(&e, Miss::Off { block: 0, name, off, bound } if name == "fx" && *off > 1.9 * bound),
        "{e:?}"
    );
    let short = s.replacen("_4284.620_", "_4284.62_", 1);
    let e = must_fail("fx a decimal short", cross_parses(&pats_v3(), &short));
    assert!(
        matches!(&e, Miss::Decimals { block: 0, name, text } if name == "fx" && text == "4284.62"),
        "{e:?}"
    );
    let e = must_fail(
        "Pat's V6 truth held to the V3 string",
        cross_parses(&pats_v6(), &s),
    );
    assert!(
        matches!(&e, Miss::Read(why) if why.starts_with("the V6 calibration string has 40 tokens")),
        "{e:?}"
    );
    let mut lens = pats_v3();
    lens.lens_type = 114;
    let e = must_fail("Pat's V3 held to lens type 114", cross_parses(&lens, &s));
    assert!(
        matches!(&e, Miss::Read(why) if why == "((10752, 5376), 113, 197632), not ((10752, 5376), 114, 197632)"),
        "{e:?}"
    );
    let mut body = pats_v3();
    body.body = 5;
    let e = must_fail("Pat's V3 held to body 5", cross_parses(&body, &s));
    assert!(
        matches!(&e, Miss::Read(why) if why == "((10752, 5376), 113, 197632), not ((10752, 5376), 113, 197888)"),
        "{e:?}"
    );
    let padded = s.replacen("_4284.620_", "_4284.6200_", 1);
    let e = must_fail(
        "fx with a zero more, reprinted",
        reprints(&pats_v3(), &padded, &s),
    );
    assert_eq!(e, padded);
    calib_parses_synths_print_cases();
}

// The calibration's hash.

/// FNV-1a 64 of `parts`, each ended by a NUL byte, as 16 hex digits, by
/// testkit's hasher, which shares no code with core's.
fn nul_ended(parts: &[&str]) -> String {
    let mut h = Fnv64::new();
    for p in parts {
        h.write(p.as_bytes()).write(&[0]);
    }
    format!("{:016x}", h.finish())
}

/// `Calibrations::hash` (M3 plan 11.1) is over the working strings in
/// generation order, each ended by a NUL, with a generation the file
/// lacks as its NUL alone; the originals don't count.
#[test]
fn the_calibration_hash_is_over_the_working_strings() {
    let _time = timed("the_calibration_hash_is_over_the_working_strings");
    let v3 = insv::print(&pats_v3(), &planted());
    let v6 = insv::print(&pats_v6(), &planted());
    let calib = x5_like_metadata(30, 1000).calib;
    let want = nul_ended(&["", "", &v3, &v6]);
    assert_eq!(calib.hash(), want);
    assert_eq!(want.len(), 16);
    let mut originals = calib.clone();
    originals.v3.original = Some("2_other".into());
    originals.v6.original = None;
    assert_eq!(originals.hash(), want, "the originals don't count");

    // The controls: a byte of a working string changed, the strings in
    // each other's generations, V3's string held as V2's, and the strings
    // hashed with no NULs, each another hash.
    let mut changed = calib.clone();
    changed.v6.working = Some(v6.replacen("4277.52", "4277.53", 1));
    must_fail(
        "a working string with a byte changed",
        changed.hash() == want,
    );
    let swapped = Calibrations {
        v3: calib.v6.clone(),
        v6: calib.v3.clone(),
        ..calib.clone()
    };
    must_fail("V3's and V6's strings swapped", swapped.hash() == want);
    let moved = Calibrations {
        v2: calib.v3.clone(),
        v3: CalibStrings::default(),
        ..calib.clone()
    };
    must_fail("V3's string held as V2's", moved.hash() == want);
    let joined = format!("{:016x}", fnv64(format!("{v3}{v6}").as_bytes()));
    must_fail("the strings hashed with no NULs", joined == calib.hash());
}

/// A file whose original copies (56 and 112) differ from its working
/// strings (54 and 111), as synth's `Records::plant_original` writes
/// them, reads with both kept apart, and camera's `Calibrations::hash`
/// follows the working strings alone (M3 plan 11.1): the hash over V1's
/// and V2's absent strings and Pat's V3 and V6, as `x5_like` printed
/// them.
///
/// Control: the same hash computed over the original copies, each its
/// generation's truth with one number moved, isn't camera's.
#[test]
fn the_calibration_hash_follows_the_working_strings_in_a_file() {
    let _time = timed("the_calibration_hash_follows_the_working_strings_in_a_file");
    let p = planted();
    let mut records = Records::x5_like(&[pats_v3(), pats_v6()], &p, 30, FrameRate::FPS_29_97);
    let (mut v3, mut v6) = (pats_v3(), pats_v6());
    v3.blocks[1].cx += 1.0;
    v6.blocks[0].fx -= 1.0;
    records.plant_original(&v3, &p);
    records.plant_original(&v6, &p);
    let (_, _, m, _) = read_back(&records);

    let working = [&pats_v3(), &pats_v6()].map(|c| insv::print(c, &p));
    let originals = [&v3, &v6].map(|c| insv::print(c, &p));
    let strings = |g: usize| CalibStrings {
        working: Some(working[g].clone()),
        original: Some(originals[g].clone()),
    };
    assert_eq!((&m.calib.v3, &m.calib.v6), (&strings(0), &strings(1)));
    assert_eq!(
        m.calib.hash(),
        nul_ended(&["", "", &working[0], &working[1]])
    );
    must_fail(
        "the hash over the original copies",
        m.calib.hash() == nul_ended(&["", "", &originals[0], &originals[1]]),
    );
}

// The faults.

/// A test's own directory in the system's temp dir, removed with what it
/// holds when dropped, so also when the test fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let d = std::env::temp_dir().join(format!(
            "spherecut-camera-synth-insv-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The file at `path`, its length, and its trailer as `read_trailer`
/// reads it.
fn open(path: &Path) -> (std::fs::File, u64, Result<Option<Trailer>, CameraError>) {
    let mut f = std::fs::File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let t = read_trailer(&mut f, len);
    (f, len, t)
}

/// The metadata of the file at `path`, whose trailer and record 1 read,
/// parsed.
fn metadata(path: &Path) -> Result<Metadata, CameraError> {
    let (mut f, _, t) = open(path);
    let t = t.unwrap().unwrap();
    let payload = read_record(&mut f, &t, 1, MAX_METADATA).unwrap().unwrap();
    parse_metadata(&payload)
}

/// Whether the file at `path`, whose trailer synth laid out as
/// `layout`, reads as `fault` makes it, with the error that names its
/// record or part at the planted byte and says so in words; or how it
/// reads otherwise.
fn reads_as(path: &Path, layout: &TrailerLayout, fault: InsvFault) -> Result<(), String> {
    let (mut f, len, t) = open(path);
    let wrong = |what: &dyn std::fmt::Debug| Err(format!("{fault:?}: {what:?}"));
    match fault {
        // The trailer's size in the tail, more than the file holds: an
        // error in the tail, at the size, saying whose size it is.
        InsvFault::TruncatedTrailer => match t {
            Err(
                ref e @ CameraError::TooLarge {
                    part: Part::Tail,
                    size,
                    limit,
                    offset,
                },
            ) if (size, limit, offset) == (len + 1, len, len - 40) => {
                let words = format!(
                    "the trailer's size in its tail is {} bytes, more than the file's length of {len} (byte {})",
                    len + 1,
                    len - 40
                );
                if e.to_string() == words {
                    Ok(())
                } else {
                    wrong(e)
                }
            }
            t => wrong(&t),
        },
        // Record 1's slot past the end: listed as skipped, and reading it
        // an error at its slot, the index's second.
        InsvFault::IndexSlotPastEnd => {
            let Ok(Some(t)) = t else {
                return wrong(&t);
            };
            let meta = layout.slots[0];
            let skipped = RecordRef {
                id: 1,
                format: 1,
                offset: layout.payload_start + layout.size,
                size: meta.size,
                slot: layout.index + 10,
            };
            if t.skipped != [skipped] || t.records.len() + 1 != layout.slots.len() {
                return wrong(&t);
            }
            match read_record(&mut f, &t, 1, MAX_METADATA) {
                Err(ref e @ CameraError::SlotPastEnd { id: 1, offset })
                    if offset == skipped.slot =>
                {
                    let words = format!(
                        "record 1's index slot at byte {offset} runs into the index or past the file"
                    );
                    if e.to_string() == words {
                        Ok(())
                    } else {
                        wrong(e)
                    }
                }
                got => wrong(&got),
            }
        }
        // The magic changed: no trailer at all.
        InsvFault::BadMagic => match t {
            Ok(None) => Ok(()),
            t => wrong(&t),
        },
        // The metadata reads, and every string in it is a count error at
        // its end, one token short.
        InsvFault::WrongTokenCount => {
            let m = metadata(path).map_err(|e| format!("{fault:?}: {e:?}"))?;
            for g in [Generation::V3, Generation::V6] {
                let c = m.calib.get(g);
                for s in [&c.working, &c.original] {
                    let Some(s) = s else {
                        return wrong(&m.calib);
                    };
                    let found = g.tokens(2) - 1;
                    match calib::parse(g, s) {
                        Err(
                            ref e @ CameraError::TokenCount {
                                generation,
                                found: n,
                                offset,
                            },
                        ) if (generation, n, offset) == (g, found, s.len() as u64) => {
                            let words = format!(
                                "the {g} calibration string has {found} tokens, which its grammar doesn't allow (byte {})",
                                s.len()
                            );
                            if e.to_string() != words {
                                return wrong(e);
                            }
                        }
                        got => return wrong(&got),
                    }
                }
            }
            Ok(())
        }
        // The metadata's first field's value of eleven bytes: a bad varint
        // in record 1 at byte 1, after its key.
        InsvFault::VarintOverflow => match metadata(path) {
            Err(
                ref e @ CameraError::BadVarint {
                    part: Part::Record(1),
                    offset: 1,
                },
            ) => {
                if e.to_string() == "record 1 has a bad varint at byte 1" {
                    Ok(())
                } else {
                    wrong(e)
                }
            }
            got => wrong(&got),
        },
        // The metadata reads, with no calibration strings at all.
        InsvFault::NoCalibration => {
            let m = metadata(path).map_err(|e| format!("{fault:?}: {e:?}"))?;
            if m.calib == Calibrations::default() && m.serial.is_some() {
                Ok(())
            } else {
                wrong(&m.calib)
            }
        }
        _ => wrong(&"not a trailer fault"),
    }
}

/// Each trailer `InsvFault` reads as it was planted. Four are errors
/// naming their part or record at the planted byte, each with its
/// message: `TruncatedTrailer` is `TooLarge` in the tail (the trailer's
/// size, at EOF − 40, more than the file's length), `IndexSlotPastEnd`
/// record 1 skipped and `SlotPastEnd` at its slot, `WrongTokenCount` a
/// `TokenCount` at each string's end and `VarintOverflow` a `BadVarint`
/// in record 1.
///
/// The other two aren't camera's errors. `BadMagic` leaves no trailer at
/// all, and `read_trailer` answers `Ok(None)` as for any file without
/// one, by its design; its clear message is probe's `bad_trailer`
/// warning of an `inst` box without the magic, at the byte that differs
/// (cli's contract.rs, `probe_warns_of_an_inst_box_without_the_magic`),
/// and doctor `--file`'s "missing: a camera trailer that can be read".
/// `NoCalibration` is a file whose metadata reads with no strings, a
/// state rather than damage (M3 plan 5.4); doctor `--file` says it
/// "can be stitched: no", missing "a calibration: the trailer holds none
/// that parses" (cli's doctor.rs).
///
/// Each file is written by `append_trailer` after the same MP4's bytes,
/// at the X5's alignment; the MP4's faults leave the file as it is
/// without them. Each fault's control: the clean file, read as that
/// fault says, doesn't read so.
#[test]
fn each_trailer_fault_reads_as_planted() {
    let _time = timed("each_trailer_fault_reads_as_planted");
    let dir = TempDir::new("faults");
    // Not an MP4, which camera never reads: the bytes before the trailer.
    let mp4 = dir.0.join("tracks.mp4");
    let tracks: Vec<u8> = (0..100_003u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&mp4, &tracks).unwrap();
    let records = Records::x5_like(
        &[pats_v3(), pats_v6()],
        &planted(),
        30,
        FrameRate::FPS_29_97,
    );
    let clean = dir.0.join("VID_clean.insv");
    let layout = insv::append_trailer(&mp4, &clean, &records, None).unwrap();
    let bytes = std::fs::read(&clean).unwrap();
    assert_eq!(bytes[..tracks.len()], tracks[..]);
    assert_eq!(layout.payload_start, tracks.len() as u64 + 8);

    // The clean file reads whole: what x5_like says it holds.
    let (_, len, t) = open(&clean);
    let t = t.unwrap().unwrap();
    assert_eq!(
        t.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        [1, 3, 4]
    );
    assert_eq!(layout.payload_start + layout.size, len);
    // 30 frames a track at 29.97: field 98 counts 31, and field 93 is
    // ⌊31 × 1001 / 30⌋ = 1034 ms.
    let want = x5_like_metadata(31, 1034);
    let m = metadata(&clean).unwrap();
    assert_eq!(m, want);

    for fault in InsvFault::all() {
        let path = dir.0.join(format!("VID_{fault:?}.insv"));
        let laid = insv::append_trailer(&mp4, &path, &records, Some(fault)).unwrap();
        let faulted = std::fs::read(&path).unwrap();
        if !fault.in_trailer() {
            assert!(faulted == bytes && laid == layout, "{fault:?}");
            continue;
        }
        assert_eq!(reads_as(&path, &laid, fault), Ok(()));
        let e = must_fail(
            &format!("the clean file read as {fault:?}"),
            reads_as(&clean, &layout, fault),
        );
        assert!(e.starts_with(&format!("{fault:?}: ")), "{e}");
    }
    // The control of the MP4's faults' check: a trailer fault's file isn't
    // the clean one.
    let truncated = std::fs::read(dir.0.join("VID_TruncatedTrailer.insv")).unwrap();
    must_fail("a truncated trailer's file", truncated == bytes);
}
