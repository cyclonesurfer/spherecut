//! Tier 2c: the trailer, metadata and strings of Pat's 14 X5 files (M3
//! plan 5.5, 19.4). They need `SPHERECUT_SAMPLES` and the CPU only:
//!
//! ```text
//! SPHERECUT_SAMPLES=/home/claude/spherecut-samples/insta360-x5 \
//!   cargo test -p spherecut-camera --test tier2_samples -- --ignored
//! ```
//!
//! There is no other parser to compare with (D62): the records' own
//! footers, the index's place at EOF − 388 and the `inst` box around the
//! trailer check the walk, and the records' sizes are held to a table a
//! throwaway script read from the same bytes (2026-11-01). The metadata
//! is held to the format report's facts, which the same scripts and
//! ffprobe read (M3's understanding pass, and insta360.md's "Pat's X5
//! samples"): the tracks' frame counts, field 98's count beside them,
//! field 27's window and the four generations' strings, byte-identical
//! in all 14 files, whose tokens are insta360.md's. Pat's serial is
//! checked for its form and its sameness and never printed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use spherecut_camera::insta360::calib::{self, Calib, Focal, Generation, Token};
use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{
    Exposure, FileGroup, Metadata, Trailer, Window, imu_span, parse_exposure, parse_metadata,
    read_record, read_trailer, thumbnail,
};
use spherecut_camera::pb::{Value, Walker};
use spherecut_camera::{CameraError, Part};
use spherecut_testkit::control::{at_most, must_fail};
use spherecut_testkit::samples::{self, Mode, Role};
use spherecut_testkit::tier2;

/// The X5's records' alignment in the file (insta360.md §3.2).
const ALIGN: u64 = 128 << 10;
/// From the start of an index of `slots` slots to the file's end: the
/// slots, the index's footer and the tail.
const fn index_from_end(slots: u64) -> u64 {
    slots * 10 + 6 + 72
}
/// The X5's: 31 slots.
const INDEX_FROM_END: u64 = index_from_end(31);
/// The shortest shutter a camera's exposure record can hold, in
/// seconds: 1/100,000 s, past any shutter's speed. An integer word read
/// as a double's bits is a denormal far below it (3248 is about
/// 1.6e-320).
const MIN_SHUTTER: f64 = 1e-5;
/// Records 2, 10, 22 and 28, whose sizes are the same in every file.
const THUMBNAIL: u32 = 1_228_840;
const R10: u32 = 35;
const R22: u32 = 8_306_836;
const R28: u32 = 12_793;

/// A take's records as its files' index lists them, read from the bytes.
/// Only the metadata (1) differs in size between a master and its LRV.
struct Take {
    /// The camera's number for the take's files.
    file: &'static str,
    /// Record 1, the metadata: the master's size, then the LRV's.
    r1: [u32; 2],
    /// Whether record 2, the thumbnail, is there: the 4K120 take wrote a
    /// sidecar instead (insta360.md, "Pat's X5 samples").
    r2: bool,
    /// Records 3 (the IMU), 4 (the exposure), 9, 11 and 29.
    r3: u32,
    r4: u32,
    r9: u32,
    r11: u32,
    r29: u32,
    /// Each lens track's frames, and the LRV's: the master's at 25 and
    /// 29.97 fps, every second at 59.94 and a quarter rounded up at
    /// 119.88 (ffprobe's counts in the format report).
    frames: u32,
    lrv_frames: u32,
    /// Field 26's kind: 6 on the take in the camera's HDR video mode.
    kind: u64,
    /// Field 135 on the master, in Mb/s.
    bitrate: u64,
    /// Field 25, the rolling shutter in ms, to the bit: an f32 widened
    /// to a double (21.244, 9.181875 and 5.53125 ms as the format report
    /// rounds them).
    shutter_ms: f64,
    /// The exposure item whose stamp is field 24's, the first frame's.
    first_entry: usize,
}

const TAKES: [Take; 7] = [
    Take {
        file: "004",
        r1: [3965, 3959],
        r2: true,
        r3: 81_280,
        r4: 1952,
        r9: 5520,
        r11: 30_705,
        r29: 189,
        frames: 114,
        lrv_frames: 114,
        kind: 0,
        bitrate: 120,
        shutter_ms: 21.244001388549805,
        first_entry: 5,
    },
    Take {
        file: "005",
        r1: [3965, 3959],
        r2: true,
        r3: 98_880,
        r4: 1968,
        r9: 5616,
        r11: 31_239,
        r29: 225,
        frames: 116,
        lrv_frames: 116,
        kind: 0,
        bitrate: 120,
        shutter_ms: 21.244001388549805,
        first_entry: 5,
    },
    Take {
        file: "006",
        r1: [3967, 3959],
        r2: true,
        r3: 67_200,
        r4: 3200,
        r9: 9168,
        r11: 50_997,
        r29: 153,
        frames: 190,
        lrv_frames: 95,
        kind: 6,
        bitrate: 150,
        shutter_ms: 9.181875228881836,
        first_entry: 6,
    },
    Take {
        file: "007",
        r1: [3966, 3960],
        r2: false,
        r3: 100_800,
        r4: 9632,
        r9: 28_176,
        r11: 156_729,
        r29: 234,
        frames: 586,
        lrv_frames: 147,
        kind: 0,
        bitrate: 120,
        shutter_ms: 5.53125,
        first_entry: 10,
    },
    Take {
        file: "008",
        r1: [3971, 3964],
        r2: true,
        r3: 652_480,
        r4: 15_584,
        r9: 46_416,
        r11: 258_189,
        r29: 1467,
        frames: 966,
        lrv_frames: 966,
        kind: 0,
        bitrate: 120,
        shutter_ms: 21.244001388549805,
        first_entry: 5,
    },
    Take {
        file: "009",
        r1: [3973, 3966],
        r2: true,
        r3: 1_281_600,
        r4: 30_592,
        r9: 91_440,
        r11: 508_635,
        r29: 2880,
        frames: 1904,
        lrv_frames: 1904,
        kind: 0,
        bitrate: 120,
        shutter_ms: 21.244001388549805,
        first_entry: 5,
    },
    Take {
        file: "010",
        r1: [3973, 3966],
        r2: true,
        r3: 653_760,
        r4: 15_616,
        r9: 46_512,
        r11: 258_723,
        r29: 1476,
        frames: 968,
        lrv_frames: 968,
        kind: 0,
        bitrate: 120,
        shutter_ms: 21.244001388549805,
        first_entry: 5,
    },
];

impl Take {
    /// The records' ids and sizes in slot order, in the master or the
    /// LRV.
    fn records(&self, lrv: bool) -> Vec<(u8, u32)> {
        let mut v = vec![(1, self.r1[usize::from(lrv)])];
        if self.r2 {
            v.push((2, THUMBNAIL));
        }
        v.extend([
            (3, self.r3),
            (4, self.r4),
            (9, self.r9),
            (10, R10),
            (11, self.r11),
            (22, R22),
            (28, R28),
            (29, self.r29),
        ]);
        v
    }
}

/// `t`'s records' ids and sizes, in slot order.
fn records(t: &Trailer) -> Vec<(u8, u32)> {
    t.records.iter().map(|r| (r.id, r.size)).collect()
}

/// A reader of `inner` with the byte at `at` changed, as a control's
/// damage: planted in the bytes as they are read, the file left as it is.
struct Flipped<R> {
    inner: R,
    at: u64,
}

impl<R: Read + Seek> Read for Flipped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.inner.stream_position()?;
        let n = self.inner.read(buf)?;
        if let Some(i) = self.at.checked_sub(pos).filter(|&i| i < n as u64) {
            buf[i as usize] ^= 0x80;
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for Flipped<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

fn aligned(offset: u64) -> bool {
    offset.is_multiple_of(ALIGN)
}

/// Pat's X5's V3 and V6 calibrations as insta360.md's "Pat's X5 samples"
/// lays them out: each lens's tokens in the order written, the canvas and
/// lens type aside (ξ, fx, fy, cx, cy, yaw, pitch, roll, tx, ty, tz and
/// the terms).
const PAT_V3: [[&str; 16]; 2] = [
    [
        "2.000000",
        "4284.620",
        "4284.280",
        "2701.130",
        "2692.130",
        "-0.198",
        "0.211",
        "90.280",
        "0.000000",
        "0.000000",
        "0.000000",
        "0.18632051",
        "2.04984355",
        "-3.24446917",
        "-0.00143576",
        "-0.00339240",
    ],
    [
        "2.000000",
        "4291.030",
        "4291.030",
        "8069.000",
        "2684.950",
        "0.239",
        "0.113",
        "89.107",
        "-0.004460",
        "0.000017",
        "-0.031920",
        "0.18940836",
        "2.07073069",
        "-3.24103332",
        "0.00013831",
        "-0.00299873",
    ],
];
const PAT_V6: [[&str; 24]; 2] = [
    [
        "2.000000",
        "4277.520",
        "4277.270",
        "2713.050",
        "2689.210",
        "-0.310",
        "-0.271",
        "90.255",
        "0.000000",
        "0.000000",
        "0.000000",
        "0.28032997",
        "0.89045405",
        "2.04927588",
        "-8.05179787",
        "0.00000000",
        "0.00120200",
        "-0.00259487",
        "0.02324706",
        "0.00533920",
        "-0.01560208",
        "0.00019357",
        "-0.04580523",
        "0.00197187",
    ],
    [
        "2.000000",
        "4284.400",
        "4284.760",
        "8075.730",
        "2680.350",
        "0.120",
        "-0.173",
        "89.112",
        "0.000637",
        "0.000001",
        "-0.031525",
        "0.27665332",
        "0.98522973",
        "1.83605266",
        "-7.99374485",
        "0.00000000",
        "0.00021056",
        "-0.00147626",
        "0.01940425",
        "0.00331838",
        "-0.01330665",
        "0.00074511",
        "-0.02859299",
        "0.00888746",
    ],
];
/// The packed words, V1 to V6: each generation's code, body code 4, and
/// in V1 the lens type 113 (`0x71`).
const WORDS: [u32; 4] = [0x471, 0x20400, 0x30400, 0x60400];
/// Field 27 on the X5: a 5312² window of each lens's 5376² square, at
/// 0, 0.
const X5_WINDOW: Window = Window {
    src: [5376; 2],
    dst: [5312; 2],
    offset: [0; 2],
};
/// The top-level fields of the X5's metadata that `parse_metadata`
/// doesn't read, in every file.
const UNKNOWN: u32 = 67;

/// Lens `lens`'s tokens in the order written, the canvas and lens type
/// aside.
fn tokens(c: &Calib, lens: usize) -> Vec<&Token> {
    let l = &c.lenses[lens];
    let mut v = match &l.focal {
        Focal::Radius(r) => vec![r],
        Focal::Mei { xi, fx, fy } => vec![xi, fx, fy],
    };
    v.extend([&l.cx, &l.cy]);
    v.extend(&l.ypr);
    v.extend(l.t.iter().flatten());
    v.extend(&l.coefficients);
    v
}

/// Lens `lens`'s tokens' text in the order written.
fn texts(c: &Calib, lens: usize) -> Vec<&str> {
    tokens(c, lens).into_iter().map(Token::text).collect()
}

/// Whether `a` and `b` are the same calibration, their tokens' text and
/// values the same to the bit.
fn same_bits(a: &Calib, b: &Calib) -> bool {
    let bits = |c| {
        (0..2)
            .flat_map(|lens| tokens(c, lens))
            .map(|t| t.value().to_bits())
            .collect::<Vec<_>>()
    };
    a == b && bits(a) == bits(b)
}

/// The four generations' strings in `m`, parsed and checked: each there,
/// its original copy the same, of its generation, on the X5's canvas
/// with its lens type and word; V3's and V6's tokens Pat's; V1 and V2
/// with V3's centres and angles, and V2 with its translations and c0 = 1.
fn check_strings(name: &str, m: &Metadata) -> Vec<Calib> {
    let parsed: Vec<Calib> = Generation::ALL
        .iter()
        .zip(WORDS)
        .map(|(&g, word)| {
            let strings = m.calib.get(g);
            assert_eq!(strings.original, strings.working, "{name}: {g}");
            let s = strings
                .working
                .as_deref()
                .unwrap_or_else(|| panic!("{name} has no {g} string"));
            let c = calib::parse(g, s).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(c.generation, g, "{name}");
            assert_eq!(
                (c.canvas, c.lens_type, c.word),
                ((10752, 5376), 113, word),
                "{name}: {g}"
            );
            c
        })
        .collect();
    let [v1, v2, v3, v6] = [&parsed[0], &parsed[1], &parsed[2], &parsed[3]];
    for lens in 0..2 {
        assert_eq!(texts(v3, lens), PAT_V3[lens], "{name}: V3 lens {lens}");
        assert_eq!(texts(v6, lens), PAT_V6[lens], "{name}: V6 lens {lens}");
        // Centres and angles: V1 and V2 after their radius, V3 after ξ, fx
        // and fy.
        let shared = |c: &Calib, from: usize| texts(c, lens)[from..from + 5].join("_");
        assert_eq!(shared(v1, 1), shared(v3, 3), "{name}: V1 lens {lens}");
        assert_eq!(shared(v2, 1), shared(v3, 3), "{name}: V2 lens {lens}");
        assert_eq!(v2.lenses[lens].t, v3.lenses[lens].t, "{name}: lens {lens}");
        assert_eq!(v2.lenses[lens].coefficients[0].text(), "1.00000000");
    }
    // The controls: lens 1's tokens aren't lens 0's, nor is the V3
    // string a V6 one.
    must_fail(
        &format!("{name}'s V3 lens 1 held to lens 0's tokens"),
        texts(v3, 1) == PAT_V3[0],
    );
    let v3_string = m.calib.v3.working.as_deref().unwrap_or_default();
    let e = must_fail(
        &format!("{name}'s V3 string read as V6"),
        calib::parse(Generation::V6, v3_string),
    );
    assert!(
        matches!(
            e,
            CameraError::TokenCount {
                generation: Generation::V6,
                found: 40,
                ..
            }
        ),
        "{e:?}"
    );
    parsed
}

/// `bytes`, a metadata record, with the first or last byte of top-level
/// field `n`'s value changed by `x`.
fn changed(bytes: &[u8], n: u32, last: bool, x: u8) -> Vec<u8> {
    let f = Walker::new(bytes, Part::Record(1), 0)
        .map(Result::unwrap)
        .find(|f| f.number == n)
        .unwrap();
    let Value::Len(value) = f.value else {
        panic!("field {n} isn't bytes")
    };
    let at = f.value_offset as usize + if last { value.len() - 1 } else { 0 };
    let mut b = bytes.to_vec();
    b[at] ^= x;
    b
}

/// The facts a master's metadata and its LRV's differ in, or that differ
/// by take, as `take`'s row and the format report give them, or what of
/// them `m` doesn't hold: the video's size (44), the layout and track
/// order (79 and 80, which the LRV lacks), the streams, codec and bit
/// rate (131, 132, 135), V6 in use (136), the rolling shutter (25), the
/// file group (26, whose path names the master on both), field 98's
/// frame count (the tracks' frames and one more on the master, the LRV's
/// own frames on the LRV) and field 93's length from the master's count;
/// field 98's first frame at field 24's time, and its Unix times as many
/// frames apart at the nominal rate.
fn file_facts(
    m: &Metadata,
    take: &Take,
    mode: Mode,
    master: &str,
    lrv: bool,
) -> Result<(), String> {
    let mut wrong = Vec::new();
    let mut want = |what: &str, holds: bool| {
        if !holds {
            wrong.push(what.to_owned());
        }
    };
    let side = mode.side();
    let (size, tracks, streams) = if lrv {
        ([1664, 832], (None, None), (Some(1), Some(1), Some(7)))
    } else {
        (
            [side, side],
            (Some(2), Some(1)),
            (Some(4), Some(2), Some(take.bitrate)),
        )
    };
    want("44, the video's size", m.video_size == Some(size));
    want(
        "79 and 80, the layout and track order",
        (m.layout, m.track_order) == tracks,
    );
    want(
        "131, 132 and 135, the streams, codec and bit rate",
        (m.stream_type, m.codec, m.bitrate_mbps) == streams,
    );
    want("136, V6 in use", m.offset_version == Some(4));
    want(
        "25, the rolling shutter",
        m.rolling_shutter_ms.map(f64::to_bits) == Some(take.shutter_ms.to_bits()),
    );
    let group = FileGroup {
        kind: take.kind,
        index: u64::from(lrv),
        identify: Some(format!("/DCIM/Camera01/{master}")),
        total: 0,
    };
    want("26, the file group", m.file_group.as_ref() == Some(&group));
    let n = u64::from(if lrv {
        take.lrv_frames
    } else {
        take.frames + 1
    });
    let on_master = u64::from(take.frames + 1);
    let span = m.frames.unwrap_or_default();
    want("98's frame count", span.frames == n);
    let (num, den) = mode.rate();
    let ms = on_master * 1000 * u64::from(den) / u64::from(num);
    want("93, the length", m.duration_ms == Some(ms));
    want(
        "98's first frame, at field 24's time",
        m.first_frame_us.map(|t| t as u64) == Some(span.first_us),
    );
    let apart = on_master * 1000 / u64::from(num.div_ceil(den));
    want(
        "98's Unix times of the first and last frames",
        span.last_unix_ms.checked_sub(span.first_unix_ms) == Some(apart),
    );
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "fields {} aren't as the take's row says",
            wrong.join("; ")
        ))
    }
}

/// One file's metadata, checked as samples_insv_trailer_parses' doc says,
/// with a line printed that leaves the serial out. `other` is another
/// take, whose row this file's metadata must not match.
fn check_metadata(
    name: &str,
    bytes: &[u8],
    role: &Role,
    take: &Take,
    other: &Take,
    lrv: bool,
    exposure: &[Exposure],
) -> Metadata {
    let m = parse_metadata(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    let serial = |m: &Metadata| {
        m.serial.as_deref().is_some_and(|s| {
            s.len() == 14
                && s.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
    };
    let x5 = |m: &Metadata| m.model.as_deref() == Some("Insta360 X5");
    let firmware = |m: &Metadata| m.firmware.as_deref() == Some("v1.1.22_build1");
    let window = |m: &Metadata| m.window == Some(X5_WINDOW);
    assert!(
        serial(&m),
        "{name}: the serial isn't 14 capitals and digits"
    );
    assert!(x5(&m), "{name}: {:?}", m.model);
    assert!(firmware(&m), "{name}: {:?}", m.firmware);
    assert!(window(&m), "{name}: {:?}", m.window);
    assert_eq!(m.unknown, UNKNOWN, "{name}");
    check_strings(name, &m);
    let master = samples::role(role.name);
    let master = master.file_name().unwrap().to_string_lossy();
    if let Err(e) = file_facts(&m, take, role.mode, &master, lrv) {
        panic!("{name}: {e}");
    }
    // Field 24 is the first frame's exposure stamp, and no other's.
    let first_us = m.first_frame_us.unwrap() as u64;
    let at = exposure.iter().position(|e| e.t_us == first_us);
    assert_eq!(at, Some(take.first_entry), "{name}");
    let span = m.frames.unwrap();
    eprintln!(
        "  metadata: {}, {}, the serial 14 capitals and digits; {} fields unread; window {}² to {}² at {}, {}; field 98 {} frames (the tracks {}), field 93 {} ms; field 24 exposure item {}; V1, V2, V3 and V6, each original its working string",
        m.model.as_deref().unwrap_or("?"),
        m.firmware.as_deref().unwrap_or("?"),
        m.unknown,
        X5_WINDOW.src[0],
        X5_WINDOW.dst[0],
        X5_WINDOW.offset[0],
        X5_WINDOW.offset[1],
        span.frames,
        if lrv { take.lrv_frames } else { take.frames },
        m.duration_ms.unwrap_or(0),
        take.first_entry,
    );

    // The controls, each check made on bytes where it must fail: the
    // serial with a letter made small (or a digit made a control
    // character), the model an X4, the firmware build0 and the window
    // a pixel lower.
    let reparsed = |n, last, x| parse_metadata(&changed(bytes, n, last, x)).unwrap();
    must_fail(
        &format!("{name}'s serial with its first byte changed"),
        serial(&reparsed(1, false, 0x20)),
    );
    must_fail(&format!("{name}'s model an X4"), x5(&reparsed(2, true, 1)));
    must_fail(
        &format!("{name}'s firmware build0"),
        firmware(&reparsed(3, true, 1)),
    );
    must_fail(
        &format!("{name}'s window a pixel lower"),
        window(&reparsed(27, true, 1)),
    );
    // The facts held to the other kind of file's, and to another take's.
    let kind = if lrv { "a master's" } else { "an LRV's" };
    must_fail(
        &format!("{name}'s metadata held to {kind}"),
        file_facts(&m, take, role.mode, &master, !lrv),
    );
    must_fail(
        &format!("{name}'s metadata held to take {}'s", other.file),
        file_facts(&m, other, role.mode, &master, lrv),
    );
    must_fail(
        &format!("{name}'s field 24 at the next exposure item"),
        exposure[take.first_entry + 1].t_us == first_us,
    );
    // The record cut before its last field, which isn't one it reads.
    let last = Walker::new(bytes, Part::Record(1), 0)
        .map(Result::unwrap)
        .last()
        .unwrap();
    let cut = parse_metadata(&bytes[..last.offset as usize]).unwrap();
    must_fail(
        &format!("{name}'s metadata without its last field"),
        cut.unknown == UNKNOWN,
    );
    m
}

/// One file's trailer and metadata, checked as the test's doc says, its
/// table printed, and the metadata's payload. `other` is another take,
/// whose sizes and facts this file's must not match.
fn check_file(
    path: &Path,
    role: &Role,
    take: &Take,
    other: &Take,
    lrv: bool,
) -> (Trailer, Metadata, Vec<u8>) {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let mut f = File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let t = read_trailer(&mut f, len)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap_or_else(|| panic!("{name} has no trailer"));
    let size = len - t.payload_start;
    eprintln!(
        "{name} ({}, {}): {len} bytes; trailer version {}, {size} bytes from {}",
        role.name,
        role.mode.name(),
        t.version,
        t.payload_start
    );
    eprintln!("  id  format       size      offset  slot from EOF  128 KiB");
    for r in &t.records {
        eprintln!(
            "  {:>2}  {:>6}  {:>9}  {:>10}  {:>13}  {}",
            r.id,
            r.format,
            r.size,
            r.offset,
            len - r.slot,
            if aligned(r.offset) { "yes" } else { "no" }
        );
    }
    assert_eq!(t.version, 3, "{name}");
    assert!(t.skipped.is_empty(), "{name}: {:?}", t.skipped);

    // The `inst` box ends where the trailer starts, and holds it: its
    // size is the trailer's plus its 8-byte header.
    let mut header = [0; 8];
    f.seek(SeekFrom::Start(t.payload_start - 8)).unwrap();
    f.read_exact(&mut header).unwrap();
    let inst = |h: [u8; 8]| {
        h[4..] == *b"inst" && u64::from(u32::from_be_bytes(h[..4].try_into().unwrap())) == size + 8
    };
    assert!(inst(header), "{name}: {header:?}");

    // The index: 31 slots at EOF − 388, slot k listing record k. The
    // first record whose slot isn't where an index of `slots` puts it.
    let misplaced = |slots: u64| {
        t.records
            .iter()
            .find(|r| len - r.slot != index_from_end(slots) - 10 * u64::from(r.id))
            .copied()
    };
    assert_eq!(misplaced(31), None, "{name}");

    // The records and their sizes, 22 and 28 among them, each footer
    // checked by read_trailer. The metadata is protobuf (1), the rest
    // binary (0): the first record that isn't, with record `protobuf`
    // the one in protobuf.
    assert_eq!(records(&t), take.records(lrv), "{name}");
    let wrong_format = |protobuf: u8| {
        t.records
            .iter()
            .find(|r| r.format != u8::from(r.id == protobuf))
            .copied()
    };
    assert_eq!(wrong_format(1), None, "{name}");

    // Every record on a 128 KiB boundary of the file but 1 and 10: the
    // metadata ends where the index starts, and record 10 starts where
    // record 11 ends.
    let rec = |id| *t.records.iter().find(|r| r.id == id).unwrap();
    let end = |id| {
        let r = rec(id);
        r.offset + u64::from(r.size) + 6
    };
    for r in t.records.iter().filter(|r| !matches!(r.id, 1 | 10)) {
        assert!(aligned(r.offset), "{name}: {r:?}");
    }
    assert_eq!(end(1), len - INDEX_FROM_END, "{name}");
    assert_eq!(rec(10).offset, end(11), "{name}");

    // The payloads that M3 reads: the metadata within its limit, the
    // thumbnail, the IMU's stamps and the exposure.
    let mut read = |id, max| {
        read_record(&mut f, &t, id, max)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .unwrap_or_else(|| panic!("{name}: no record {id}"))
    };
    let metadata = read(1, MAX_METADATA);
    assert_eq!(metadata.len(), take.r1[usize::from(lrv)] as usize);
    // The thumbnail is 1280 × 640; turned to 640 × 1280 in its header,
    // which still parses, it isn't.
    let is_1280_by_640 = |bytes: &[u8]| {
        let image = thumbnail(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        (image.width, image.height) == (1280, 640)
    };
    if take.r2 {
        let thumb = read(2, THUMBNAIL as usize);
        assert!(is_1280_by_640(&thumb), "{name}: {:?}", &thumb[..40]);
        let mut portrait = thumb;
        portrait[16..24].rotate_left(4);
        must_fail(
            &format!("{name}'s thumbnail turned 640 × 1280"),
            is_1280_by_640(&portrait),
        );
    }
    let imu = imu_span(&read(3, take.r3 as usize)).unwrap().unwrap();
    assert_eq!(imu.samples, u64::from(take.r3 / 20), "{name}");
    let imu_step = (imu.last_us - imu.first_us) as f64 / (imu.samples - 1) as f64;
    // About 1 kHz: the mean step within 5 % of `step` µs.
    let imu_at = |step: f64| at_most((imu_step / step - 1.0).abs(), 0.05);
    assert_eq!(imu_at(1000.0).passes(), Some(true), "{name}: {imu_step} µs");
    let exposure = parse_exposure(&read(4, take.r4 as usize)).unwrap();
    assert_eq!(exposure.len(), take.r4 as usize / 16, "{name}");
    // The stamps rise, item by item.
    let stamps_rise = |items: &[Exposure]| items.windows(2).all(|w| w[0].t_us < w[1].t_us);
    assert!(stamps_rise(&exposure), "{name}");
    let (num, den) = role.mode.rate();
    let period = f64::from(den) / f64::from(num);
    let step = (exposure[exposure.len() - 1].t_us - exposure[0].t_us) as f64
        / (exposure.len() - 1) as f64
        / 1e6;
    // One item a frame: their mean step within 1 % of the frame period.
    let within = |period: f64| at_most((step / period - 1.0).abs(), 0.01);
    assert_eq!(
        within(period).passes(),
        Some(true),
        "{name}: {step} s a frame, not {period}"
    );
    // Every shutter a real one, no shorter than MIN_SHUTTER, and shorter
    // than the frame: the first item that isn't.
    let odd_shutter = |items: &[Exposure], frame: f64| {
        items
            .iter()
            .find(|e| !(MIN_SHUTTER..frame).contains(&e.seconds))
            .copied()
    };
    assert_eq!(odd_shutter(&exposure, period), None, "{name}");
    eprintln!(
        "  IMU {} items, {imu_step:.1} µs apart; exposure {} items, {:.3} ms apart, shutters {:.1} to {:.1} µs",
        imu.samples,
        exposure.len(),
        step * 1e3,
        exposure
            .iter()
            .map(|e| e.seconds * 1e6)
            .fold(f64::INFINITY, f64::min),
        exposure.iter().map(|e| e.seconds * 1e6).fold(0.0, f64::max),
    );
    let m = check_metadata(&name, &metadata, role, take, other, lrv, &exposure);

    // The controls, each check made where it must fail. Record 4's
    // footer with its id changed is refused, at the footer.
    let footer = rec(4).offset + u64::from(rec(4).size);
    let mut flipped = Flipped {
        inner: File::open(path).unwrap(),
        at: footer + 1,
    };
    let e = must_fail(
        &format!("{name} with record 4's footer changed"),
        read_trailer(&mut flipped, len),
    );
    assert!(
        matches!(e, CameraError::FooterMismatch { id: 4, offset } if offset == footer),
        "{name}: {e:?}"
    );
    // The slots and the metadata's end aren't where a 32-slot index
    // puts them.
    must_fail(
        &format!("{name}'s slots against an index of 32"),
        misplaced(32).is_none(),
    );
    must_fail(
        &format!("{name}'s metadata ending at an index of 32"),
        end(1) == len - index_from_end(32),
    );
    // The metadata isn't on a boundary, nor does record 9 follow 10 as
    // 10 follows 11; another take's sizes aren't this file's; the
    // thumbnail isn't the record in protobuf; the IMU isn't 500 Hz; the
    // exposure isn't a frame at half the rate apart; and the `inst` box
    // read a byte early isn't one.
    must_fail(
        &format!("{name}'s metadata on a 128 KiB boundary"),
        aligned(rec(1).offset),
    );
    must_fail(
        &format!("{name}'s record 9 where record 10 ends"),
        rec(9).offset == end(10),
    );
    must_fail(
        &format!("{name}'s sizes against take {}'s", other.file),
        records(&t) == other.records(lrv),
    );
    must_fail(
        &format!("{name}'s formats with the thumbnail in protobuf"),
        wrong_format(2).is_none(),
    );
    must_fail(&format!("{name}'s IMU at 500 Hz"), imu_at(2000.0));
    must_fail(
        &format!("{name}'s exposure a frame apart at half the rate"),
        within(2.0 * period),
    );
    // The exposure's stamps reversed don't rise; its shutters aren't all
    // shorter than a thousandth of the frame; and stored as integer µs,
    // read as a double's bits as parse_exposure reads them, they are
    // denormals, above zero and shorter than the frame but under
    // MIN_SHUTTER.
    let reversed: Vec<Exposure> = exposure.iter().rev().copied().collect();
    must_fail(
        &format!("{name}'s exposure stamps reversed"),
        stamps_rise(&reversed),
    );
    must_fail(
        &format!("{name}'s shutters held to a thousandth of the frame"),
        odd_shutter(&exposure, period / 1000.0).is_none(),
    );
    let as_integers: Vec<Exposure> = exposure
        .iter()
        .map(|e| Exposure {
            seconds: f64::from_bits((e.seconds * 1e6).round() as u64),
            ..*e
        })
        .collect();
    assert!(
        as_integers
            .iter()
            .all(|e| e.seconds > 0.0 && e.seconds < period),
        "{name}"
    );
    must_fail(
        &format!("{name}'s shutters as integer µs read as a double's bits"),
        odd_shutter(&as_integers, period).is_none(),
    );
    let mut early = [0; 8];
    f.seek(SeekFrom::Start(t.payload_start - 9)).unwrap();
    f.read_exact(&mut early).unwrap();
    must_fail(&format!("{name}'s inst box a byte early"), inst(early));
    (t, m, metadata)
}

/// On all 14 files: the index (31 slots, the records' sizes and 128 KiB
/// boundaries) and records 1, 3 and 4 (unit a); the metadata, field 27,
/// the frame counts, the four generations' strings and their equality
/// across masters and LRVs (unit b).
///
/// Unit (b)'s part: each file's metadata parses, with 67 fields it
/// doesn't read; its serial is 14 capitals and digits, the same in all
/// 14 files and never printed; the model is the X5 and the firmware
/// v1.1.22_build1; field 27 is the 5312² window at 0, 0. Its V1, V2, V3
/// and V6 strings are there, each original equal to its working string,
/// parse as their generation on the 10752 × 5376 canvas with lens type
/// 113 and body code 4, V3's and V6's tokens are insta360.md's, and V1
/// and V2 share V3's centres and angles; all 14 files' strings are
/// byte-identical. The master's and the LRV's own facts (44, 79, 80,
/// 131, 132, 135, 26's index) are each kind's, and field 98 counts the
/// tracks' frames and one more on the master, the LRV's own frames on
/// the LRV, the frame count field 93's length is from; field 24 is the
/// first frame's exposure item, 5 at 25 and 29.97 fps, 6 at 59.94 and
/// 10 at 119.88.
///
/// Unit (a)'s part: each file's trailer reads with nothing skipped, in an
/// `inst` box of its size plus 8; its index at EOF − 388 lists record k
/// in slot k; its records, 22 and 28 among them, are the table's ids and
/// sizes, each footer checked; all but 1 and 10 sit on 128 KiB
/// boundaries; the thumbnail is 1280 × 640, the IMU about 1 kHz, and the
/// exposure one item a frame at the take's rate, its stamps rising and
/// each shutter between 1/100,000 s and the frame. A table per file says
/// what the index holds.
#[test]
#[ignore = "tier2: Pat's X5 samples (Tier 2c: SPHERECUT_SAMPLES and the CPU only)"]
fn samples_insv_trailer_parses() {
    let started = Instant::now();
    tier2::require_samples_cpu();
    let shot: Vec<&'static Role> = samples::roles()
        .iter()
        .filter(|r| r.file.is_some())
        .collect();
    assert_eq!(shot.len(), TAKES.len());
    let mut summary = Vec::new();
    let mut metadata = Vec::new();
    for (i, role) in shot.iter().enumerate() {
        let take = TAKES
            .iter()
            .find(|t| Some(t.file) == role.file)
            .unwrap_or_else(|| panic!("no table row for take {:?}", role.file));
        let other = &TAKES[(i + 1) % TAKES.len()];
        for (lrv, path) in [
            (false, samples::role(role.name)),
            (true, samples::lrv(role.name)),
        ] {
            let (t, m, bytes) = check_file(&path, role, take, other, lrv);
            metadata.push((path.clone(), m, bytes));
            summary.push((path, t));
        }
    }
    assert_eq!(summary.len(), 14);
    // The serial and the strings are the camera's: the same in all 14
    // files, masters and LRVs.
    let (first, first_m, first_bytes) = &metadata[0];
    for (path, m, _) in &metadata {
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            m.serial == first_m.serial,
            "{name}'s serial isn't the others'"
        );
        assert_eq!(m.calib, first_m.calib, "{name} against {}", first.display());
    }
    // The controls: a master's V6 word a unit higher in its last digit,
    // which still parses, and its serial with a byte changed, aren't its
    // LRV's.
    let (_, lrv_m, _) = &metadata[1];
    let word_changed = parse_metadata(&changed(first_bytes, 111, true, 1)).unwrap();
    assert!(
        calib::parse(
            Generation::V6,
            word_changed.calib.v6.working.as_deref().unwrap()
        )
        .is_ok()
    );
    must_fail(
        "a master's V6 string with its word changed, against its LRV's",
        word_changed.calib == lrv_m.calib,
    );
    let serial_changed = parse_metadata(&changed(first_bytes, 1, true, 1)).unwrap();
    must_fail(
        "a master's serial with a byte changed, against its LRV's",
        serial_changed.serial == lrv_m.serial,
    );
    eprintln!(
        "The 14 files' serials are one, and their strings byte-identical: V1 {} bytes, V2 {}, V3 {}, V6 {}",
        first_m.calib.v1.working.as_ref().map_or(0, String::len),
        first_m.calib.v2.working.as_ref().map_or(0, String::len),
        first_m.calib.v3.working.as_ref().map_or(0, String::len),
        first_m.calib.v6.working.as_ref().map_or(0, String::len),
    );
    eprintln!("The records of Pat's 14 files (id:size):");
    for (path, t) in &summary {
        let row: Vec<String> = records(t)
            .iter()
            .map(|(id, size)| format!("{id}:{size}"))
            .collect();
        eprintln!(
            "  {}  {}",
            path.file_name().unwrap().to_string_lossy(),
            row.join(" ")
        );
    }
    eprintln!(
        "samples_insv_trailer_parses: {} files in {:.3} s",
        summary.len(),
        started.elapsed().as_secs_f64()
    );
}

/// `path`'s metadata record, read through its trailer.
fn metadata_record(path: &Path) -> Vec<u8> {
    let name = path.file_name().unwrap().to_string_lossy();
    let mut f = File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let t = read_trailer(&mut f, len)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap_or_else(|| panic!("{name} has no trailer"));
    read_record(&mut f, &t, 1, MAX_METADATA)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap_or_else(|| panic!("{name} has no metadata"))
}

/// `s` printed from its tokens' values rather than their text, as
/// Rust's shortest form of each: what a printer that kept only the
/// numbers would give.
fn printed_from_values(s: &str) -> String {
    let values: Vec<String> = s
        .split('_')
        .map(|t| t.parse::<f64>().unwrap().to_string())
        .collect();
    values.join("_")
}

/// On all 14 files, both copies of each generation's string, working and
/// original: `print(&parse(g, s)?) == s` byte for byte, and what prints
/// parses back to the same calibration, its tokens' text and values the
/// same to the bit (M3 plan 5.2).
///
/// The controls, on every string: a printer from the values alone (`2`
/// for `2.000000`) doesn't give it back; the string with its first
/// block's first token written with one more zero parses to that token's
/// value to the bit, but neither to the same calibration nor printing
/// as the camera's string; the calibration with that token's last digit
/// changed isn't the same to the bit; and the string read as the next
/// generation is refused for its token count.
#[test]
#[ignore = "tier2: Pat's X5 samples (Tier 2c: SPHERECUT_SAMPLES and the CPU only)"]
fn samples_calib_print_is_identity() {
    let started = Instant::now();
    tier2::require_samples_cpu();
    let mut files = 0;
    let mut strings = 0;
    for role in samples::roles().iter().filter(|r| r.file.is_some()) {
        for path in [samples::role(role.name), samples::lrv(role.name)] {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let m =
                parse_metadata(&metadata_record(&path)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut line = Vec::new();
            for (i, g) in Generation::ALL.into_iter().enumerate() {
                let both = m.calib.get(g);
                for (copy, s) in [("working", &both.working), ("original", &both.original)] {
                    let what = format!("{name}'s {copy} {g} string");
                    let s = s.as_deref().unwrap_or_else(|| panic!("{what} isn't there"));
                    let c = calib::parse(g, s).unwrap_or_else(|e| panic!("{what}: {e}"));
                    let printed = calib::print(&c);
                    assert_eq!(printed, s, "{what}");
                    let again = calib::parse(g, &printed).unwrap_or_else(|e| panic!("{what}: {e}"));
                    assert!(same_bits(&again, &c), "{what}: {again:?}\nagainst {c:?}");
                    strings += 1;

                    // The controls.
                    must_fail(
                        &format!("{what} printed from its values"),
                        printed_from_values(s) == s,
                    );
                    let first = tokens(&c, 0)[0];
                    assert!(first.text().contains('.'), "{what}: {}", first.text());
                    let longer = s.replacen(first.text(), &format!("{}0", first.text()), 1);
                    let rewritten =
                        calib::parse(g, &longer).unwrap_or_else(|e| panic!("{what}: {e}"));
                    let token = tokens(&rewritten, 0)[0];
                    assert_eq!(token.value().to_bits(), first.value().to_bits(), "{what}");
                    must_fail(
                        &format!("{what} with a zero more, against its own calibration"),
                        same_bits(&rewritten, &c),
                    );
                    must_fail(
                        &format!("{what} with a zero more, printed against the camera's"),
                        calib::print(&rewritten) == s,
                    );
                    let mut higher = first.text().to_owned();
                    let last = higher.pop().unwrap();
                    higher.push(if last == '9' {
                        '8'
                    } else {
                        char::from(last as u8 + 1)
                    });
                    let mut nudged = c.clone();
                    match &mut nudged.lenses[0].focal {
                        Focal::Radius(r) => *r = Token::new(&higher).unwrap(),
                        Focal::Mei { xi, .. } => *xi = Token::new(&higher).unwrap(),
                    }
                    must_fail(
                        &format!("{what} with its first token's last digit changed"),
                        same_bits(&nudged, &c),
                    );
                    let next = Generation::ALL[(i + 1) % Generation::ALL.len()];
                    let e = must_fail(&format!("{what} read as {next}"), calib::parse(next, s));
                    assert!(
                        matches!(e, CameraError::TokenCount { generation, found, .. }
                            if generation == next && found == g.tokens(2)),
                        "{what}: {e:?}"
                    );
                }
                let s = both.working.as_deref().unwrap_or_default();
                line.push(format!("{g} {} tokens, {} bytes", g.tokens(2), s.len()));
            }
            eprintln!(
                "{name}: {}; each original its working string",
                line.join(", ")
            );
            files += 1;
        }
    }
    assert_eq!((files, strings), (14, 14 * 8));
    eprintln!(
        "samples_calib_print_is_identity: {strings} strings in {files} files in {:.3} s",
        started.elapsed().as_secs_f64()
    );
}
