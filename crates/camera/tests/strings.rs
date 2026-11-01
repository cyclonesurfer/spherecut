//! The protobuf walker and the calibration strings (M3 plan 5.1, 5.2 and
//! 5.5), M3.2's unit (b). The properties run 100,000 cases at
//! `PROPTEST_CASES=100000`.
//!
//! The messages here are encoded by this file's own few lines, and the
//! strings assembled from insta360.md §4.1's table of each block's
//! tokens; synth's writer and printer, which share nothing with the
//! parsers either, are held to them in `synth_insv.rs` (unit (d)). Pat's
//! four strings are the ones insta360.md's "Pat's X5 samples" gives,
//! byte-identical in all 14 of Pat's files.
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): camera's suite ≤ 1 s at the default
//! 256 cases, which each test prints its time against; the properties
//! run at `PROPTEST_CASES=100000` apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::OnceLock;
use std::time::Instant;

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;
use spherecut_camera::insta360::calib::{self, Calib, Focal, Generation, Token};
use spherecut_camera::insta360::limits::{MAX_METADATA, MAX_STRING};
use spherecut_camera::insta360::{
    CalibStrings, Calibrations, FileGroup, FrameSpan, LensProfiles, Metadata, Window,
    parse_metadata,
};
use spherecut_camera::pb::{Field, Value, Walker};
use spherecut_camera::{CameraError, Part};
use spherecut_testkit::control::must_fail;

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

// Pat's four strings, as the X5 writes them (insta360.md, "Pat's X5
// samples"), and the generations they are.
const V1: &str = "2_2653.545_2701.130_2692.130_-0.198_0.211_90.280_2661.203_8069.000_2684.950_0.239_0.113_89.107_10752_5376_1137";
const V2: &str = "2_2660.372_2701.130_2692.130_-0.198_0.211_90.280_0.000000_0.000000_0.000000_1.00000000_-0.18756898_0.27067676_-0.08894466_10752_5376_113_2670.937_8069.000_2684.950_0.239_0.113_89.107_-0.004460_0.000017_-0.031920_1.00000000_-0.18880324_0.27227741_-0.08904964_10752_5376_113_132096";
const V3: &str = "2_2.000000_4284.620_4284.280_2701.130_2692.130_-0.198_0.211_90.280_0.000000_0.000000_0.000000_0.18632051_2.04984355_-3.24446917_-0.00143576_-0.00339240_10752_5376_113_2.000000_4291.030_4291.030_8069.000_2684.950_0.239_0.113_89.107_-0.004460_0.000017_-0.031920_0.18940836_2.07073069_-3.24103332_0.00013831_-0.00299873_10752_5376_113_197632";
const V6: &str = "2_2.000000_4277.520_4277.270_2713.050_2689.210_-0.310_-0.271_90.255_0.000000_0.000000_0.000000_0.28032997_0.89045405_2.04927588_-8.05179787_0.00000000_0.00120200_-0.00259487_0.02324706_0.00533920_-0.01560208_0.00019357_-0.04580523_0.00197187_10752_5376_113_2.000000_4284.400_4284.760_8075.730_2680.350_0.120_-0.173_89.112_0.000637_0.000001_-0.031525_0.27665332_0.98522973_1.83605266_-7.99374485_0.00000000_0.00021056_-0.00147626_0.01940425_0.00331838_-0.01330665_0.00074511_-0.02859299_0.00888746_10752_5376_113_394240";
const PATS: [(Generation, &str); 4] = [
    (Generation::V1, V1),
    (Generation::V2, V2),
    (Generation::V3, V3),
    (Generation::V6, V6),
];

// A protobuf encoder of this file's own (the wire format of pb.rs's
// header, which is protobuf's).

/// A varint of `v` in its fewest bytes, and then `pad` more: the last
/// byte's top bit set and a 0x80 for each padding byte but the last,
/// which is 0, as protobuf reads them.
fn varint(v: u64, pad: usize) -> Vec<u8> {
    let mut b = Vec::new();
    let mut v = v;
    loop {
        let low = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            b.push(low);
            break;
        }
        b.push(low | 0x80);
    }
    for _ in 0..pad {
        *b.last_mut().unwrap() |= 0x80;
        b.push(0);
    }
    b
}

/// A field's value as it is written.
#[derive(Debug, Clone)]
enum Wire {
    Varint(u64),
    /// A varint padded by this many bytes, as far as ten bytes allow.
    Padded(u64, usize),
    I64(u64),
    Bytes(Vec<u8>),
    Message(Vec<(u32, Wire)>),
    I32(u32),
}

fn key(number: u32, wire: u8) -> Vec<u8> {
    varint(u64::from(number) << 3 | u64::from(wire), 0)
}

fn field(number: u32, w: &Wire) -> Vec<u8> {
    let len = |b: Vec<u8>| [key(number, 2), varint(b.len() as u64, 0), b].concat();
    match w {
        Wire::Varint(v) => [key(number, 0), varint(*v, 0)].concat(),
        Wire::Padded(v, pad) => {
            let fewest = varint(*v, 0).len();
            [key(number, 0), varint(*v, (*pad).min(10 - fewest))].concat()
        }
        Wire::I64(v) => [key(number, 1), v.to_le_bytes().to_vec()].concat(),
        Wire::Bytes(b) => len(b.clone()),
        Wire::Message(fields) => len(message(fields)),
        Wire::I32(v) => [key(number, 5), v.to_le_bytes().to_vec()].concat(),
    }
}

fn message(fields: &[(u32, Wire)]) -> Vec<u8> {
    fields.iter().flat_map(|(n, w)| field(*n, w)).collect()
}

fn text(s: &str) -> Wire {
    Wire::Bytes(s.as_bytes().to_vec())
}

fn double(v: f64) -> Wire {
    Wire::I64(v.to_bits())
}

/// The polynomial and field of view field 145 gives the bare lens on
/// Pat's X5 (insta360.md §3.5).
const BARE: [f64; 6] = [0.0, 0.03159, -8.415e-5, 2.201e-6, -1.284e-8, 0.0];

/// An X5-like metadata record, field by field: every field
/// `parse_metadata` reads, with values as on Pat's clip 1 but for the
/// serial and the path, which are made up, and some it skips (98's items
/// 2, 6 and 7, and 145's item 3) inside the messages it reads.
fn x5_fields() -> Vec<(u32, Wire)> {
    use Wire::{Message as M, Varint as V};
    let mut fields = vec![
        (1, text("TEST0123456789")),
        (2, text("Insta360 X5")),
        (3, text("v1.1.22_build1")),
        (24, V(32_753_371)),
        (25, double(21.244)),
        (
            26,
            M(vec![
                (1, V(0)),
                (2, V(0)),
                (3, text("/DCIM/Camera01/VID_20260101_000000_00_001.insv")),
                (4, V(0)),
            ]),
        ),
        (
            27,
            M(vec![
                (1, V(5376)),
                (2, V(5376)),
                (3, V(5312)),
                (4, V(5312)),
                (5, V(0)),
                (6, V(0)),
            ]),
        ),
        (44, M(vec![(1, V(3840)), (2, V(3840))])),
        (79, V(2)),
        (80, V(1)),
        (93, V(3837)),
        (
            98,
            M(vec![
                (1, V(32_753_371)),
                (2, V(36_586_704)),
                (3, V(1_792_713_290_839)),
                (4, V(1_792_713_294_672)),
                (5, V(115)),
                (6, V(0)),
                (7, V(0)),
            ]),
        ),
        (131, V(4)),
        (132, V(2)),
        (135, V(120)),
        (136, V(4)),
    ];
    let polynomial = [
        vec![(1, text("bare"))],
        BARE.map(|c| (2, double(c))).to_vec(),
    ]
    .concat();
    fields.push((
        145,
        M(vec![
            (1, M(polynomial)),
            (2, M(vec![(1, text("bare")), (2, double(200.0))])),
            (3, M(vec![(1, text("heat_bare")), (2, V(10))])),
        ]),
    ));
    for (g, s) in PATS {
        let [working, original] = g.fields();
        fields.extend([(working, text(s)), (original, text(s))]);
    }
    fields
}

/// What `parse_metadata` should make of `x5_fields`.
fn x5_metadata() -> Metadata {
    let strings = |s: &str| CalibStrings {
        working: Some(s.to_owned()),
        original: Some(s.to_owned()),
    };
    Metadata {
        serial: Some("TEST0123456789".into()),
        model: Some("Insta360 X5".into()),
        firmware: Some("v1.1.22_build1".into()),
        first_frame_us: Some(32_753_371),
        rolling_shutter_ms: Some(21.244),
        file_group: Some(FileGroup {
            kind: 0,
            index: 0,
            identify: Some("/DCIM/Camera01/VID_20260101_000000_00_001.insv".into()),
            total: 0,
        }),
        window: Some(Window {
            src: [5376; 2],
            dst: [5312; 2],
            offset: [0; 2],
        }),
        video_size: Some([3840; 2]),
        layout: Some(2),
        track_order: Some(1),
        duration_ms: Some(3837),
        frames: Some(FrameSpan {
            first_us: 32_753_371,
            first_unix_ms: 1_792_713_290_839,
            last_unix_ms: 1_792_713_294_672,
            frames: 115,
        }),
        stream_type: Some(4),
        codec: Some(2),
        bitrate_mbps: Some(120),
        offset_version: Some(4),
        lens_profiles: Some(LensProfiles {
            polynomials: vec![("bare".into(), BARE.to_vec())],
            fovs: vec![("bare".into(), 200.0)],
        }),
        calib: Calibrations {
            v1: strings(V1),
            v2: strings(V2),
            v3: strings(V3),
            v6: strings(V6),
        },
        unknown: 0,
    }
}

/// The top-level fields `parse_metadata` reads.
const KNOWN: [u32; 25] = [
    1, 2, 3, 5, 17, 24, 25, 26, 27, 44, 53, 54, 55, 56, 79, 80, 93, 98, 111, 112, 131, 132, 135,
    136, 145,
];

/// The byte an error is at, whichever it is.
fn offset(e: &CameraError) -> u64 {
    match e {
        CameraError::Truncated { offset, .. }
        | CameraError::SlotPastEnd { offset, .. }
        | CameraError::FooterMismatch { offset, .. }
        | CameraError::IndexFooter { offset }
        | CameraError::BadVarint { offset, .. }
        | CameraError::WireType { offset, .. }
        | CameraError::TooLarge { offset, .. }
        | CameraError::TooSmall { offset, .. }
        | CameraError::Version { offset, .. }
        | CameraError::BadSize { offset, .. }
        | CameraError::NotUtf8 { offset, .. }
        | CameraError::TokenCount { offset, .. }
        | CameraError::BadToken { offset, .. }
        | CameraError::Io { offset, .. } => *offset,
    }
}

/// Any value of any wire type, the varints padded or not.
fn wire() -> impl Strategy<Value = Wire> {
    prop_oneof![
        (any::<u64>(), 0usize..3).prop_map(|(v, pad)| Wire::Padded(v, pad)),
        any::<u64>().prop_map(Wire::I64),
        vec(any::<u8>(), 0..24).prop_map(Wire::Bytes),
        any::<u32>().prop_map(Wire::I32),
    ]
}

/// A value in a varint's every length, each 7 bits of it, the ends and
/// powers of two weighed up.
fn any_varint() -> impl Strategy<Value = u64> {
    prop_oneof![
        any::<u64>(),
        (0u32..64).prop_flat_map(|bits| 0..=(u64::MAX >> (63 - bits))),
        (0u32..64).prop_map(|bit| 1u64 << bit),
        Just(0),
        Just(u64::MAX),
    ]
}

/// Whether walking `bytes` ends in `BadVarint` at byte `at`, after the
/// fields before it.
fn bad_varint_at(bytes: &[u8], at: u64) -> bool {
    let items: Vec<_> = Walker::new(bytes, Part::Record(1), 0).collect();
    matches!(
        items.last(),
        Some(Err(CameraError::BadVarint { part: Part::Record(1), offset })) if *offset == at
    ) && items[..items.len() - 1].iter().all(Result::is_ok)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn varints_round_trip_cases(
        v in any_varint(),
        number in prop_oneof![1u32..=32, 1u32..(1 << 29)],
        pad in 0usize..10,
        before in vec((1u32..300, wire()), 0..4),
        base in prop_oneof![Just(0u64), any::<u64>().prop_map(|b| b >> 1)],
        cut in any::<Index>(),
        tenth in 2u8..0x80,
    ) {
        let prefix = message(&before);
        let k = key(number, 0);
        let fewest = varint(v, 0);
        let pad = pad.min(10 - fewest.len());
        let value = varint(v, pad);
        let bytes = [&prefix[..], &k, &value].concat();
        let at = prefix.len() as u64;

        // Walked from any base, it is the last field, at its offsets.
        let fields: Vec<Field> = Walker::new(&bytes, Part::Record(1), base)
            .collect::<Result<_, _>>()
            .unwrap();
        prop_assert_eq!(fields.len(), before.len() + 1);
        let last = fields[fields.len() - 1];
        prop_assert_eq!(last.number, number);
        prop_assert_eq!(last.value, Value::Varint(v));
        prop_assert_eq!(last.offset, base + at);
        prop_assert_eq!(last.value_offset, base + at + k.len() as u64);
        // As a length, it is that many bytes, or past the end.
        let as_len = [&prefix[..], &key(number, 2), &value].concat();
        let last = Walker::new(&as_len, Part::Record(1), 0).last().unwrap();
        prop_assert!(
            matches!(last, Err(CameraError::Truncated { part: Part::Record(1), offset })
                if offset == as_len.len() as u64)
                || v == 0 && matches!(last, Ok(Field { value: Value::Len(b), .. }) if b.is_empty()),
            "{:?}", last
        );

        // Cut inside it: cut short at the end of the bytes.
        let cut = prefix.len() + k.len() + cut.index(value.len());
        let item = Walker::new(&bytes[..cut], Part::Record(1), base).last().unwrap();
        prop_assert!(
            matches!(item, Err(CameraError::Truncated { part: Part::Record(1), offset })
                if offset == base + cut as u64),
            "cut at {}: {:?}", cut, item
        );

        // Ten bytes and an eleventh, or a tenth byte past bit 63: a bad
        // varint, at its start.
        let ten = varint(v, 10 - fewest.len());
        prop_assert_eq!(ten.len(), 10);
        let mut eleven = ten.clone();
        eleven[9] |= 0x80;
        eleven.push(0);
        let at_value = at + k.len() as u64;
        prop_assert!(bad_varint_at(&[&prefix[..], &k, &eleven].concat(), at_value));
        let mut past = ten.clone();
        past[9] = tenth;
        prop_assert!(bad_varint_at(&[&prefix[..], &k, &past].concat(), at_value));
        // And the key itself, as eleven bytes.
        prop_assert!(bad_varint_at(&[&prefix[..], &eleven].concat(), at));
    }
}

/// Property: any u64 written as a varint, in any field, walks back to
/// itself; an eleventh byte or a value past 64 bits is `BadVarint`.
///
/// Each value is written in its fewest bytes and padded (protobuf's
/// `0x80 0x00` for 0), after fields of every wire type, and walked from
/// any base: the field comes back with its number, value and offsets. As
/// a length, it is that many bytes or past the end; cut anywhere inside,
/// it is cut short at the end; and ten bytes with an eleventh, or a tenth
/// byte past bit 63, are a bad varint at its first byte, as a key and as
/// a value.
#[test]
fn varints_round_trip() {
    let _time = timed("varints_round_trip");
    // u64::MAX takes ten bytes, the last bit 63 alone, and walks back.
    let max = varint(u64::MAX, 0);
    assert_eq!(max.len(), 10);
    assert_eq!(*max.last().unwrap(), 1);
    let bytes = [&key(7, 0)[..], &max].concat();
    let fields: Vec<Field> = Walker::new(&bytes, Part::Record(1), 0)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(fields[0].value, Value::Varint(u64::MAX));
    // A padded 0 is 0.
    assert_eq!(varint(0, 2), [0x80, 0x80, 0x00]);
    let zero = [&key(7, 0)[..], &varint(0, 2)].concat();
    assert_eq!(
        Walker::new(&zero, Part::Record(1), 0)
            .next()
            .unwrap()
            .unwrap()
            .value,
        Value::Varint(0)
    );
    // The control: the check of a bad varint, held to u64::MAX's ten
    // bytes as written, fails.
    must_fail(
        "u64::MAX's ten bytes taken for a bad varint",
        bad_varint_at(&bytes, 1),
    );
    varints_round_trip_cases();
}

/// A field number `parse_metadata` doesn't read, small or as large as
/// protobuf allows.
fn unknown_number() -> impl Strategy<Value = u32> {
    prop_oneof![1u32..300, 1u32..(1 << 29)].prop_filter("a field it reads", |n| !KNOWN.contains(n))
}

/// Inserts `extra` into the messages among `fields`, each into the one
/// and at the place its indexes pick.
fn nest(fields: &mut [(u32, Wire)], extra: Vec<(Index, Index, u32, Wire)>) {
    let messages: Vec<usize> = (0..fields.len())
        .filter(|&i| matches!(fields[i].1, Wire::Message(_)))
        .collect();
    for (which, at, n, w) in extra {
        if let Wire::Message(inner) = &mut fields[messages[which.index(messages.len())]].1 {
            inner.insert(at.index(inner.len() + 1), (n, w));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn unknown_fields_are_skipped_cases(
        order in Just(x5_fields()).prop_shuffle(),
        top in vec((any::<Index>(), unknown_number(), wire()), 0..12),
        // Inside the messages it reads, numbers past any they use.
        nested in vec((any::<Index>(), any::<Index>(), 8u32..1000, wire()), 0..8),
    ) {
        let mut fields = order;
        nest(&mut fields, nested);
        let expected = Metadata { unknown: top.len() as u32, ..x5_metadata() };
        for (at, n, w) in top {
            let at = at.index(fields.len() + 1);
            fields.insert(at, (n, w));
        }
        prop_assert_eq!(parse_metadata(&message(&fields)).unwrap(), expected);
    }
}

/// Property: fields `parse_metadata` doesn't read, of every wire type,
/// anywhere among the ones it does, are skipped and counted, and change
/// nothing else.
///
/// The X5-like record's fields come in any order, with up to 12 unknown
/// top-level fields among them (varints padded or not, doubles, bytes
/// and fixed32s, numbers up to protobuf's largest) and up to 8 more
/// inside the messages it reads, which are skipped and not counted.
#[test]
fn unknown_fields_are_skipped() {
    let _time = timed("unknown_fields_are_skipped");
    let fields = x5_fields();
    let base = message(&fields);
    assert_eq!(parse_metadata(&base).unwrap(), x5_metadata());
    // The controls: a second field 2, which it reads, isn't skipped, and
    // keeps its last value; and a group isn't skipped but an error.
    let mut again = fields.clone();
    again.push((2, text("Insta360 X4")));
    let parsed = parse_metadata(&message(&again)).unwrap();
    assert_eq!(parsed.model.as_deref(), Some("Insta360 X4"));
    must_fail(
        "a second field 2 among the unknown",
        parsed == x5_metadata(),
    );
    let group = [&base[..], &key(9, 3)].concat();
    let e = must_fail("a group among the unknown", parse_metadata(&group));
    assert!(
        matches!(e, CameraError::WireType { part: Part::Record(1), wire: 3, offset }
            if offset == base.len() as u64),
        "{e:?}"
    );
    unknown_fields_are_skipped_cases();
}

/// The names of a block's tokens in the order written: insta360.md
/// §4.1's table.
fn block_names(g: Generation) -> Vec<String> {
    let focal: &[&str] = match g {
        Generation::V1 | Generation::V2 => &["radius"],
        Generation::V3 | Generation::V6 => &["xi", "fx", "fy"],
    };
    let mut names: Vec<String> = focal.iter().map(|s| s.to_string()).collect();
    names.extend(["cx", "cy", "yaw", "pitch", "roll"].map(String::from));
    if g != Generation::V1 {
        names.extend(["tx", "ty", "tz"].map(String::from));
    }
    names.extend(coefficient_names(g));
    if g != Generation::V1 {
        names.extend(["w", "h", "type"].map(String::from));
    }
    names
}

/// The distortion terms' names: V2's c0 to c3, V3's k1 k2 k3 p1 p2, V6's
/// d0 to d12.
fn coefficient_names(g: Generation) -> Vec<String> {
    match g {
        Generation::V1 => Vec::new(),
        Generation::V2 => (0..4).map(|i| format!("c{i}")).collect(),
        Generation::V3 => ["k1", "k2", "k3", "p1", "p2"].map(String::from).to_vec(),
        Generation::V6 => (0..13).map(|i| format!("d{i}")).collect(),
    }
}

/// The code a packed word's bits 16 and up give each generation.
fn code(g: Generation) -> u32 {
    match g {
        Generation::V1 => 0,
        Generation::V2 => 2,
        Generation::V3 => 3,
        Generation::V6 => 6,
    }
}

/// A string laid out as insta360.md §4.1 has it: each block's tokens by
/// name, the canvas, the lens type and the word.
#[derive(Debug, Clone)]
struct Laid {
    g: Generation,
    /// Each block's decimal tokens, by name.
    blocks: [Vec<(String, String)>; 2],
    canvas: (u32, u32),
    lens_type: u32,
    word: u32,
}

impl Laid {
    /// The string: the lens count, each block (in V2, V3 and V6 ending
    /// with the canvas and lens type), V1's canvas, the word.
    fn string(&self) -> String {
        let (w, h) = self.canvas;
        let mut tokens = vec!["2".to_string()];
        for block in &self.blocks {
            tokens.extend(block.iter().map(|(_, text)| text.clone()));
            if self.g != Generation::V1 {
                tokens.extend([w, h, self.lens_type].map(|n| n.to_string()));
            }
        }
        if self.g == Generation::V1 {
            tokens.extend([w, h].map(|n| n.to_string()));
        }
        tokens.push(self.word.to_string());
        tokens.join("_")
    }

    /// The calibration it is, built by hand from its named tokens.
    fn calib(&self) -> Calib {
        let lens = |block: &[(String, String)]| {
            let t = |name: &str| {
                let (_, text) = block.iter().find(|(n, _)| n == name).unwrap();
                Token::new(text).unwrap()
            };
            let focal = match self.g {
                Generation::V1 | Generation::V2 => Focal::Radius(t("radius")),
                Generation::V3 | Generation::V6 => Focal::Mei {
                    xi: t("xi"),
                    fx: t("fx"),
                    fy: t("fy"),
                },
            };
            calib::LensTokens {
                focal,
                cx: t("cx"),
                cy: t("cy"),
                ypr: [t("yaw"), t("pitch"), t("roll")],
                t: (self.g != Generation::V1).then(|| [t("tx"), t("ty"), t("tz")]),
                coefficients: coefficient_names(self.g).iter().map(|n| t(n)).collect(),
            }
        };
        Calib {
            generation: self.g,
            lenses: [lens(&self.blocks[0]), lens(&self.blocks[1])],
            canvas: self.canvas,
            word: self.word,
            lens_type: self.lens_type,
        }
    }
}

/// A decimal token as the strings write them: a minus or not, digits,
/// and a point and digits or not.
fn decimal() -> impl Strategy<Value = String> {
    let digits =
        || vec(0u8..10, 1..7).prop_map(|d| d.iter().map(|d| char::from(b'0' + d)).collect());
    (any::<bool>(), digits(), prop::option::of(digits())).prop_map(
        |(minus, whole, fraction): (bool, String, Option<String>)| {
            let sign = if minus { "-" } else { "" };
            match fraction {
                Some(f) => format!("{sign}{whole}.{f}"),
                None => format!("{sign}{whole}"),
            }
        },
    )
}

/// Any string of any generation: any decimal tokens, any canvas and lens
/// type, and a word of its generation with any body code. In V1 the lens
/// type is the word's low byte.
fn laid() -> impl Strategy<Value = Laid> {
    prop::sample::select(Generation::ALL.to_vec())
        .prop_flat_map(|g| {
            let names: Vec<String> = block_names(g)
                .into_iter()
                .filter(|n| !matches!(n.as_str(), "w" | "h" | "type"))
                .collect();
            let n = names.len();
            (
                Just(g),
                Just(names),
                vec(decimal(), 2 * n),
                any::<(u32, u32, u32)>(),
                any::<(u8, u8)>(),
            )
        })
        .prop_map(|(g, names, texts, (w, h, lens_type), (body, low))| {
            let block =
                |texts: &[String]| names.iter().cloned().zip(texts.iter().cloned()).collect();
            let word = code(g) << 16 | u32::from(body) << 8 | u32::from(low);
            Laid {
                g,
                blocks: [block(&texts[..names.len()]), block(&texts[names.len()..])],
                canvas: (w, h),
                lens_type: if g == Generation::V1 {
                    u32::from(low)
                } else {
                    lens_type
                },
                word,
            }
        })
}

/// Every token of `c`, in the order printed, the integers aside.
fn tokens(c: &Calib) -> Vec<&Token> {
    let mut v = Vec::new();
    for lens in &c.lenses {
        match &lens.focal {
            Focal::Radius(r) => v.push(r),
            Focal::Mei { xi, fx, fy } => v.extend([xi, fx, fy]),
        }
        v.extend([&lens.cx, &lens.cy]);
        v.extend(&lens.ypr);
        v.extend(lens.t.iter().flatten());
        v.extend(&lens.coefficients);
    }
    v
}

/// Whether two calibrations are equal bit for bit: equal, and every
/// token's value the same bits.
fn same_bits(a: &Calib, b: &Calib) -> bool {
    let bits = |c| {
        tokens(c)
            .iter()
            .map(|t| t.value().to_bits())
            .collect::<Vec<_>>()
    };
    a == b && bits(a) == bits(b)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn strings_round_trip_cases(l in laid()) {
        let s = l.string();
        let c = l.calib();
        // The calibration built by hand prints as laid out, and the
        // string parses to it, each token in its named place.
        prop_assert_eq!(calib::print(&c), s.clone());
        let parsed = calib::parse(l.g, &s).unwrap();
        prop_assert!(same_bits(&parsed, &c), "{:?}\nagainst {:?}", parsed, c);
        prop_assert_eq!(calib::print(&parsed), s);
        let again = calib::parse(l.g, &calib::print(&c)).unwrap();
        prop_assert!(same_bits(&again, &c));
    }
}

/// Property: `parse(g, &print(c)) == c` for calibrations of every
/// generation, and `print(&parse(g, s)?) == s` for every string printed.
///
/// First Pat's four strings, then any string of any generation (any
/// decimal tokens, canvas, lens type and body code), whose calibration,
/// built by hand from its named tokens, prints as it and parses back bit
/// for bit.
#[test]
fn strings_round_trip() {
    let _time = timed("strings_round_trip");
    for (g, s) in PATS {
        let c = calib::parse(g, s).unwrap_or_else(|e| panic!("{g}: {e}"));
        assert_eq!(c.generation, g);
        assert_eq!((c.canvas, c.lens_type), ((10752, 5376), 113), "{g}");
        assert_eq!(calib::print(&c), s, "{g}");
        assert!(same_bits(&calib::parse(g, &calib::print(&c)).unwrap(), &c));
        assert_eq!(
            tokens(&c).len(),
            2 * block_names(g).len() - if g == Generation::V1 { 0 } else { 6 }
        );
    }
    // A few of Pat's tokens by name (insta360.md, "Pat's X5 samples").
    let v3 = calib::parse(Generation::V3, V3).unwrap();
    assert_eq!(v3.lenses[1].cx.text(), "8069.000");
    assert_eq!(v3.lenses[1].t.as_ref().unwrap()[2].text(), "-0.031920");
    assert_eq!(v3.lenses[0].coefficients[3].text(), "-0.00143576");
    let v6 = calib::parse(Generation::V6, V6).unwrap();
    assert_eq!(v6.lenses[1].coefficients.len(), 13);
    assert_eq!(v6.lenses[1].coefficients[12].text(), "0.00888746");
    assert_eq!(v6.word, 0x60400);
    let v1 = calib::parse(Generation::V1, V1).unwrap();
    assert_eq!((v1.word, v1.lenses[1].ypr[2].text()), (0x471, "89.107"));
    // The control: printed from its values alone, V3 isn't itself
    // ("2.000000" prints as "2"): the identity is the tokens' text.
    let from_values = {
        let mut t: Vec<String> = vec!["2".into()];
        let names = block_names(Generation::V3).len() - 3;
        for (i, token) in tokens(&v3).iter().enumerate() {
            t.push(token.value().to_string());
            if i % names == names - 1 {
                t.extend(["10752", "5376", "113"].map(String::from));
            }
        }
        t.push(v3.word.to_string());
        t.join("_")
    };
    assert_eq!(from_values.split('_').count(), 40);
    must_fail("Pat's V3 string printed from its values", from_values == V3);
    // The controls on same_bits: V3 with lens 0's ξ written with a zero
    // more has the same values to the bit but other text, and with ξ's
    // last digit changed, another value.
    let longer = V3.replacen("2_2.000000_", "2_2.0000000_", 1);
    let rewritten = calib::parse(Generation::V3, &longer).unwrap();
    let (xi, was) = (tokens(&rewritten)[0], tokens(&v3)[0]);
    assert_eq!((xi.text(), was.text()), ("2.0000000", "2.000000"));
    assert_eq!(xi.value().to_bits(), was.value().to_bits());
    must_fail(
        "Pat's V3 with lens 0's xi written 2.0000000",
        same_bits(&rewritten, &v3),
    );
    let mut nudged = v3.clone();
    let Focal::Mei { xi, .. } = &mut nudged.lenses[0].focal else {
        panic!("V3's lens 0 has no xi: {:?}", nudged.lenses[0].focal);
    };
    *xi = Token::new("2.000001").unwrap();
    must_fail(
        "Pat's V3 with lens 0's xi written 2.000001",
        same_bits(&nudged, &v3),
    );
    strings_round_trip_cases();
}

/// Whether `r` is `TokenCount` of generation `g`, `found` tokens, at
/// byte `at`.
fn count_error(r: &Result<Calib, CameraError>, g: Generation, found: usize, at: usize) -> bool {
    matches!(r, Err(CameraError::TokenCount { generation, found: f, offset })
        if (*generation, *f, *offset) == (g, found, at as u64))
}

/// Where token `k` of `s` starts.
fn token_at(s: &str, k: usize) -> usize {
    s.split('_').take(k).map(|t| t.len() + 1).sum()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn a_wrong_token_count_is_an_error_cases(
        l in laid(),
        at in any::<Index>(),
        add in any::<bool>(),
        extra in decimal(),
        other in prop::sample::select(Generation::ALL.to_vec()),
    ) {
        let s = l.string();
        let mut t: Vec<&str> = s.split('_').collect();
        let n = t.len();
        prop_assert_eq!(n, l.g.tokens(2));
        if add {
            t.insert(at.index(n + 1), &extra);
        } else {
            t.remove(at.index(n));
        }
        let wrong = t.join("_");
        // Too many: at the first token past the count; too few: at the end.
        let (found, end) = if add { (n + 1, token_at(&wrong, n)) } else { (n - 1, wrong.len()) };
        let r = calib::parse(l.g, &wrong);
        prop_assert!(count_error(&r, l.g, found, end), "{:?}", r);
        // Read as another generation, the string is that one's wrong count.
        if other != l.g {
            let expected = other.tokens(2);
            let end = if n > expected { token_at(&s, expected) } else { s.len() };
            let r = calib::parse(other, &s);
            prop_assert!(count_error(&r, other, n, end), "{:?}", r);
        }
    }
}

/// Property: a string with a token dropped or added is `TokenCount`, with
/// its generation, the count found and the byte where it went wrong.
///
/// The byte is the string's end when a token is missing, and the start of
/// the first token past the count when there is one too many, wherever
/// the token went; a string of one generation read as another's is that
/// one's wrong count, at its byte.
#[test]
fn a_wrong_token_count_is_an_error() {
    let _time = timed("a_wrong_token_count_is_an_error");
    let r = calib::parse(Generation::V6, &V6[2..]);
    assert!(count_error(&r, Generation::V6, 55, V6.len() - 2), "{r:?}");
    let r = calib::parse(Generation::V3, "");
    assert!(count_error(&r, Generation::V3, 0, 0), "{r:?}");
    assert_eq!(
        r.unwrap_err().to_string(),
        "the V3 calibration string has 0 tokens, which its grammar doesn't allow (byte 0)"
    );
    // The control: Pat's V6 as it is isn't a wrong count.
    let as_is = calib::parse(Generation::V6, V6);
    must_fail(
        "Pat's V6 string taken for a wrong count",
        count_error(&as_is, Generation::V6, 56, V6.len()),
    );
    assert!(as_is.is_ok());
    a_wrong_token_count_is_an_error_cases();
}

/// The record with field `n`'s first value replaced by `w`, where that
/// field's key is, and where its value is when `w` is bytes or a
/// message.
fn with_field(fields: &[(u32, Wire)], n: u32, w: Wire) -> (Vec<u8>, u64, u64) {
    let i = fields.iter().position(|(m, _)| *m == n).unwrap();
    let at = message(&fields[..i]).len() as u64;
    let len = match &w {
        Wire::Bytes(b) => b.len(),
        Wire::Message(f) => message(f).len(),
        _ => 0,
    };
    let value = at + (key(n, 2).len() + varint(len as u64, 0).len()) as u64;
    let mut fields = fields.to_vec();
    fields[i].1 = w;
    (message(&fields), at, value)
}

/// Each error `parse_metadata` gives, with the part it names and the
/// byte it is at, against the X5-like record, which parses: its limits
/// (`MAX_METADATA` and `MAX_STRING`, which `trailer.rs` leaves to this
/// file), a string that isn't UTF-8, groups and wire types a field
/// doesn't have, a u32 past 32 bits, keys protobuf doesn't allow, and
/// bytes cut short, at the top and inside a message. A number or a
/// string read twice keeps its last value and a message read twice is
/// replaced by its last copy, field 24 is signed, and packed doubles
/// read as field 145's unpacked ones do.
#[test]
fn each_metadata_error_is_at_its_byte() {
    let _time = timed("each_metadata_error_is_at_its_byte");
    let fields = x5_fields();
    let base = message(&fields);
    assert_eq!(parse_metadata(&base).unwrap(), x5_metadata());

    // MAX_METADATA: the record padded by an unknown field to 1 MiB
    // parses, a byte more doesn't, at the first byte past the limit.
    let padded = |size: usize| {
        // The filler's length, whose varint takes `n` bytes.
        let fill = (1..=10)
            .map(|n| size - base.len() - key(9, 2).len() - n)
            .find(|&fill| varint(fill as u64, 0).len() == size - base.len() - 1 - fill)
            .unwrap();
        let b = [&base[..], &field(9, &Wire::Bytes(vec![0; fill]))].concat();
        assert_eq!(b.len(), size);
        b
    };
    let at_limit = parse_metadata(&padded(MAX_METADATA)).unwrap();
    assert_eq!(at_limit.unknown, 1);
    let e = must_fail(
        "a metadata record a byte over 1 MiB",
        parse_metadata(&padded(MAX_METADATA + 1)),
    );
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Record(1), size, limit, offset }
            if (size, limit, offset) == (MAX_METADATA as u64 + 1, MAX_METADATA as u64, MAX_METADATA as u64)),
        "{e:?}"
    );

    // MAX_STRING: a model, an identify inside field 26 and a calibration
    // string of 4 KiB parse; a byte more is an error at its field's key.
    let long = |n: usize| Wire::Bytes(vec![b'a'; n]);
    let (b, ..) = with_field(&fields, 2, long(MAX_STRING));
    assert_eq!(parse_metadata(&b).unwrap().model.unwrap().len(), MAX_STRING);
    let (b, at, _) = with_field(&fields, 2, long(MAX_STRING + 1));
    let e = must_fail("a model a byte over 4 KiB", parse_metadata(&b));
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Field(2), size, limit, offset }
            if (size, limit, offset) == (MAX_STRING as u64 + 1, MAX_STRING as u64, at)),
        "{e:?}"
    );
    let (b, at, _) = with_field(&fields, 111, long(MAX_STRING + 1));
    let e = must_fail("a V6 string a byte over 4 KiB", parse_metadata(&b));
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Field(111), offset, .. } if offset == at),
        "{e:?}"
    );
    let group = |identify: Wire| {
        Wire::Message(vec![
            (1, Wire::Varint(0)),
            (3, identify),
            (4, Wire::Varint(0)),
        ])
    };
    let (b, ..) = with_field(&fields, 26, group(long(MAX_STRING)));
    assert!(parse_metadata(&b).is_ok());
    let (b, _, value) = with_field(&fields, 26, group(long(MAX_STRING + 1)));
    let e = must_fail("an identify a byte over 4 KiB", parse_metadata(&b));
    // Item 1 (2 bytes) before item 3.
    let item3 = value + 2;
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Field(26), offset, .. } if offset == item3),
        "{e:?}"
    );

    // A string that isn't UTF-8: at its first bad byte.
    let (b, _, value) = with_field(&fields, 3, Wire::Bytes(b"v1.1\xff22".to_vec()));
    let e = must_fail("a firmware that isn't UTF-8", parse_metadata(&b));
    assert!(
        matches!(e, CameraError::NotUtf8 { part: Part::Field(3), offset } if offset == value + 4),
        "{e:?}"
    );

    // Groups and the wire types protobuf doesn't define, at the key.
    for wire in [3, 4, 6, 7] {
        let b = [&base[..], &key(9, wire), &[0; 8]].concat();
        let e = must_fail(&format!("wire type {wire}"), parse_metadata(&b));
        assert!(
            matches!(e, CameraError::WireType { part: Part::Record(1), wire: w, offset }
                if (w, offset) == (wire, base.len() as u64)),
            "{e:?}"
        );
    }
    // A field it reads, in a wire type not its own: at its key, in it.
    for (n, w, wire) in [
        (24, Wire::I64(5), 1),
        (2, Wire::Varint(5), 0),
        (25, Wire::I32(5), 5),
        (27, Wire::Varint(5), 0),
    ] {
        let (b, at, _) = with_field(&fields, n, w);
        let e = must_fail(
            &format!("field {n} in wire type {wire}"),
            parse_metadata(&b),
        );
        assert!(
            matches!(e, CameraError::WireType { part: Part::Field(p), wire: w, offset }
                if (p, w, offset) == (n, wire, at)),
            "{e:?}"
        );
    }
    let window = |src_w: Wire| {
        Wire::Message(vec![
            (1, src_w),
            (2, Wire::Varint(5376)),
            (3, Wire::Varint(5312)),
        ])
    };
    let (b, _, value) = with_field(&fields, 27, window(Wire::Bytes(vec![1])));
    let e = must_fail("field 27's item 1 as bytes", parse_metadata(&b));
    assert!(
        matches!(e, CameraError::WireType { part: Part::Field(27), wire: 2, offset } if offset == value),
        "{e:?}"
    );

    // A u32 past 32 bits, in the window: at its value.
    let (b, ..) = with_field(&fields, 27, window(Wire::Varint(u64::from(u32::MAX))));
    assert_eq!(parse_metadata(&b).unwrap().window.unwrap().src[0], u32::MAX);
    let (b, _, value) = with_field(&fields, 27, window(Wire::Varint(1 << 32)));
    let e = must_fail("a window 2^32 wide", parse_metadata(&b));
    assert!(
        matches!(e, CameraError::BadVarint { part: Part::Field(27), offset } if offset == value + 1),
        "{e:?}"
    );

    // Keys protobuf doesn't allow: field 0, and past 32 bits.
    for (k, what) in [
        (varint(0, 0), "field 0"),
        (varint(1 << 35, 0), "a key past 32 bits"),
    ] {
        let b = [&base[..], &k, &[0]].concat();
        let e = must_fail(what, parse_metadata(&b));
        assert!(
            matches!(e, CameraError::BadVarint { part: Part::Record(1), offset } if offset == base.len() as u64),
            "{what}: {e:?}"
        );
    }

    // Cut short: the record inside its last field, at the end; a field
    // inside field 26 that runs past it, at field 26's end.
    let e = must_fail(
        "the record a byte short",
        parse_metadata(&base[..base.len() - 1]),
    );
    assert!(
        matches!(e, CameraError::Truncated { part: Part::Record(1), offset } if offset == base.len() as u64 - 1),
        "{e:?}"
    );
    let (b, _, value) = with_field(
        &fields,
        26,
        Wire::Bytes([key(3, 2), vec![9, b'/']].concat()),
    );
    let e = must_fail("field 26's identify past its end", parse_metadata(&b));
    assert!(
        matches!(e, CameraError::Truncated { part: Part::Field(26), offset } if offset == value + 3),
        "{e:?}"
    );

    // Field 145's doubles, packed: as unpacked, and whole doubles only.
    let packed = |bytes: Vec<u8>| {
        Wire::Message(vec![(
            1,
            Wire::Message(vec![(1, text("bare")), (2, Wire::Bytes(bytes))]),
        )])
    };
    let doubles: Vec<u8> = BARE.iter().flat_map(|c| c.to_le_bytes()).collect();
    let (b, ..) = with_field(&fields, 145, packed(doubles.clone()));
    let profiles = parse_metadata(&b).unwrap().lens_profiles.unwrap();
    assert_eq!(
        profiles.polynomials,
        vec![("bare".to_string(), BARE.to_vec())]
    );
    let (b, _, value) = with_field(&fields, 145, packed(doubles[..47].to_vec()));
    let e = must_fail(
        "field 145's packed doubles a byte short",
        parse_metadata(&b),
    );
    // Item 1's key and length, its name (6 bytes), and the doubles' key
    // and length.
    let end = value + 2 + 6 + 2 + 47;
    assert!(
        matches!(e, CameraError::Truncated { part: Part::Field(145), offset } if offset == end),
        "{e:?}"
    );

    // Read twice, a field keeps its last value; field 24 is an int64.
    let twice = [&base[..], &field(3, &text("v1.1.23_build1"))].concat();
    assert_eq!(
        parse_metadata(&twice).unwrap().firmware.as_deref(),
        Some("v1.1.23_build1")
    );
    let (b, ..) = with_field(&fields, 24, Wire::Varint(-5i64 as u64));
    assert_eq!(parse_metadata(&b).unwrap().first_frame_us, Some(-5));
    // A message read twice is replaced by its last copy, not merged: a
    // second window with only its width leaves the rest at 0.
    let width_only = Wire::Message(vec![(1, Wire::Varint(7))]);
    let twice = [&base[..], &field(27, &width_only)].concat();
    let replaced = parse_metadata(&twice).unwrap().window;
    assert_eq!(
        replaced,
        Some(Window {
            src: [7, 0],
            dst: [0; 2],
            offset: [0; 2],
        })
    );
    // The control: merged, as protobuf would, it would keep the rest.
    let merged = Window {
        src: [7, 5376],
        dst: [5312; 2],
        offset: [0; 2],
    };
    must_fail("a window read twice, merged", replaced == Some(merged));
}

/// Each error `calib::parse` gives besides a wrong count, with its
/// reason and the byte it is at, against Pat's strings, which parse; and
/// `Token::new`'s and `Generation`'s rules.
#[test]
fn each_bad_token_is_an_error_at_its_byte() {
    let _time = timed("each_bad_token_is_an_error_at_its_byte");
    for text in ["2", "-0.198", "0.00000000", "007", "-0", "4294967296"] {
        let t = Token::new(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!((t.text(), t.value()), (text, text.parse::<f64>().unwrap()));
    }
    let huge = "9".repeat(400);
    for text in [
        "", "-", "+1", " 1", "1 ", "1.", ".5", "1.2.3", "1e5", "inf", "nan", "0x10", "1_2", &huge,
    ] {
        assert!(Token::new(text).is_none(), "{text:?}");
    }
    assert_eq!(
        Generation::ALL.map(|g| g.fields()),
        [[5, 17], [53, 55], [54, 56], [111, 112]]
    );
    assert_eq!(Generation::ALL.map(|g| g.tokens(2)), [16, 34, 40, 56]);
    assert_eq!(Generation::ALL.map(|g| g.tokens(1)), [10, 18, 21, 29]);
    for (word, g) in [
        (0x471, Some(Generation::V1)),
        (0x20400, Some(Generation::V2)),
        (0x30b00, Some(Generation::V3)),
        (0x60400, Some(Generation::V6)),
        (0x40400, None),
        (0x1060400, None),
    ] {
        assert_eq!(Generation::of_word(word), g, "{word:#x}");
    }
    // Field 136's values (§3.5): 4 is V6, as on the X5; 5 (V8) and 0 are
    // none of the four.
    for (v, g) in [
        (1, Some(Generation::V1)),
        (2, Some(Generation::V2)),
        (3, Some(Generation::V3)),
        (4, Some(Generation::V6)),
        (0, None),
        (5, None),
        (6, None),
        (u64::MAX, None),
    ] {
        assert_eq!(Generation::of_offset_version(v), g, "{v}");
    }

    // Pat's V3 with token k's text replaced by `text`, and where token
    // k is.
    let replaced = |s: &str, k: usize, text: &str| {
        let mut t: Vec<&str> = s.split('_').collect();
        t[k] = text;
        (t.join("_"), token_at(s, k))
    };
    let bad = |g: Generation, s: &str, why: &str, at: usize| {
        let e = must_fail(&format!("{g}: {s:.40}…"), calib::parse(g, s));
        assert!(
            matches!(&e, CameraError::BadToken { generation, why: w, offset }
                if (*generation, *w, *offset) == (g, why, at as u64)),
            "{e:?}"
        );
        e
    };
    let not_a_number = "isn't a decimal number";
    let not_whole = "isn't a whole number as a u32 prints";
    for text in ["1e5", "+2.0", "", "inf", "2.000.0"] {
        let (s, at) = replaced(V3, 2, text);
        bad(Generation::V3, &s, not_a_number, at);
    }
    // The canvas, the lens type and the word, as a u32 prints them.
    for (k, text) in [
        (17, "010752"),
        (18, "5376.0"),
        (19, "-113"),
        (39, "4294967296"),
        (17, ""),
    ] {
        let (s, at) = replaced(V3, k, text);
        bad(Generation::V3, &s, not_whole, at);
    }
    // The lens count: 3 with two lenses' tokens, or a string of three.
    let (s, _) = replaced(V3, 0, "3");
    bad(
        Generation::V3,
        &s,
        "isn't a lens count of 2, the only one read",
        0,
    );
    let three = {
        let t: Vec<&str> = V3.split('_').collect();
        [&["3"], &t[1..20], &t[1..20], &t[20..]].concat().join("_")
    };
    assert_eq!(three.split('_').count(), Generation::V3.tokens(3));
    let e = bad(
        Generation::V3,
        &three,
        "isn't a lens count of 2, the only one read",
        0,
    );
    assert_eq!(
        e.to_string(),
        "the V3 calibration string's token at byte 0 isn't a lens count of 2, the only one read"
    );
    // Block 1's canvas or lens type isn't block 0's: at block 1's.
    let canvas = "gives a canvas other than block 0's";
    for k in [36, 37] {
        let (s, at) = replaced(V3, k, "10753");
        bad(Generation::V3, &s, canvas, at);
    }
    let (s, at) = replaced(V3, 38, "114");
    bad(
        Generation::V3,
        &s,
        "gives a lens type other than block 0's",
        at,
    );
    let (s, at) = replaced(V6, 54, "193");
    bad(
        Generation::V6,
        &s,
        "gives a lens type other than block 0's",
        at,
    );
    // A word of another generation, or of none.
    let other = "is a packed word of another generation";
    for word in ["394240", "17040384"] {
        let (s, at) = replaced(V3, 39, word);
        bad(Generation::V3, &s, other, at);
    }
    let (s, at) = replaced(V1, 15, "132096");
    bad(Generation::V1, &s, other, at);
    // V1's lens type is its word's low byte.
    let (s, _) = replaced(V1, 15, "1136");
    assert_eq!(calib::parse(Generation::V1, &s).unwrap().lens_type, 112);
}

/// Arbitrary metadata: anything, the X5-like record with bytes changed,
/// or cut anywhere.
fn arbitrary_metadata() -> impl Strategy<Value = Vec<u8>> {
    let base = message(&x5_fields());
    let n = base.len();
    prop_oneof![
        vec(any::<u8>(), 0..300),
        (Just(base.clone()), vec((any::<Index>(), 1u8..=255), 1..6)).prop_map(|(mut b, flips)| {
            for (i, x) in flips {
                let i = i.index(b.len());
                b[i] ^= x;
            }
            b
        }),
        (Just(base), 0..n).prop_map(|(b, cut)| b[..cut].to_vec()),
    ]
}

/// Arbitrary strings: of the strings' own characters, or Pat's with
/// characters changed.
fn arbitrary_string() -> impl Strategy<Value = String> {
    let chars = || prop::sample::select(b"0123456789._-x ".to_vec());
    prop_oneof![
        vec(chars(), 0..80).prop_map(|c| String::from_utf8(c).unwrap()),
        (
            prop::sample::select(PATS.map(|(_, s)| s).to_vec()),
            vec((any::<Index>(), chars()), 1..4),
        )
            .prop_map(|(s, edits)| {
                let mut b = s.as_bytes().to_vec();
                for (i, c) in edits {
                    let i = i.index(b.len());
                    b[i] = c;
                }
                String::from_utf8(b).unwrap()
            }),
        ".{0,40}",
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn arbitrary_bytes_never_panic_the_parsers_cases(
        bytes in arbitrary_metadata(),
        base in prop_oneof![Just(0u64), any::<u64>().prop_map(|b| b >> 1)],
        s in arbitrary_string(),
        g in prop::sample::select(Generation::ALL.to_vec()),
    ) {
        let len = bytes.len() as u64;
        if let Err(e) = parse_metadata(&bytes) {
            prop_assert!(offset(&e) <= len, "{:?} past {}", e, len);
        }
        // The walk ends, every field inside the bytes, and an error last;
        // from a base so far on that its offsets saturate, it still ends.
        prop_assert!(Walker::new(&bytes, Part::Record(1), u64::MAX - 8).count() <= bytes.len());
        let items: Vec<_> = Walker::new(&bytes, Part::Record(1), base).collect();
        let end = base + len;
        for (i, item) in items.iter().enumerate() {
            match item {
                Ok(f) => prop_assert!(f.offset < end && f.value_offset <= end, "{:?}", f),
                Err(e) => {
                    prop_assert_eq!(i, items.len() - 1);
                    prop_assert!(offset(e) <= end, "{:?}", e);
                }
            }
        }
        // A string parses and prints back as itself, or is an error
        // inside it.
        match calib::parse(g, &s) {
            Ok(c) => prop_assert_eq!(calib::print(&c), s),
            Err(e) => prop_assert!(offset(&e) <= s.len() as u64, "{:?} past {}", e, s.len()),
        }
    }
}

/// Property: `parse_metadata`, `pb::Walker` and `calib::parse` on
/// arbitrary bytes and strings return and never panic, every error
/// inside what they read; and a string that parses, whatever it is,
/// prints back as itself.
#[test]
fn arbitrary_bytes_never_panic_the_parsers() {
    let _time = timed("arbitrary_bytes_never_panic_the_parsers");
    // The controls: the X5-like record cut inside its last field isn't
    // read as whole, nor is Pat's V6 with its word's last digit a letter.
    let base = message(&x5_fields());
    must_fail(
        "the X5-like record cut a byte short",
        parse_metadata(&base[..base.len() - 1]),
    );
    let lettered = format!("{}x", &V6[..V6.len() - 1]);
    must_fail(
        "Pat's V6 with a letter for a digit",
        calib::parse(Generation::V6, &lettered),
    );
    arbitrary_bytes_never_panic_the_parsers_cases();
}
