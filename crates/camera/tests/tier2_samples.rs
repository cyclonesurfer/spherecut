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
//! throwaway script read from the same bytes (2026-11-01).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use spherecut_camera::CameraError;
use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{
    Trailer, imu_span, parse_exposure, read_record, read_trailer, thumbnail,
};
use spherecut_testkit::control::{at_most, must_fail};
use spherecut_testkit::samples::{self, Role};
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

/// One file's trailer, checked as the test's doc says, its table printed.
/// `other` is another take, whose sizes this file's must not match.
fn check_file(path: &Path, role: &Role, take: &Take, other: &Take, lrv: bool) -> Trailer {
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

    // The payloads that M3 reads: the metadata within its limit (unit (b)
    // parses it), the thumbnail, the IMU's stamps and the exposure.
    let mut read = |id, max| {
        read_record(&mut f, &t, id, max)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .unwrap_or_else(|| panic!("{name}: no record {id}"))
    };
    assert_eq!(
        read(1, MAX_METADATA).len(),
        take.r1[usize::from(lrv)] as usize
    );
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
    assert!(exposure.windows(2).all(|w| w[0].t_us < w[1].t_us), "{name}");
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
    for e in &exposure {
        assert!(e.seconds > 0.0 && e.seconds < period, "{name}: {e:?}");
    }
    eprintln!(
        "  IMU {} items, {imu_step:.1} µs apart; exposure {} items, {:.3} ms apart, {:.5} to {:.5} s",
        imu.samples,
        exposure.len(),
        step * 1e3,
        exposure
            .iter()
            .map(|e| e.seconds)
            .fold(f64::INFINITY, f64::min),
        exposure.iter().map(|e| e.seconds).fold(0.0, f64::max),
    );

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
    let mut early = [0; 8];
    f.seek(SeekFrom::Start(t.payload_start - 9)).unwrap();
    f.read_exact(&mut early).unwrap();
    must_fail(&format!("{name}'s inst box a byte early"), inst(early));
    t
}

/// On all 14 files: the index (31 slots, the records' sizes and 128 KiB
/// boundaries) and records 1, 3 and 4 (unit a); the metadata, field 27,
/// the frame counts, the four generations' strings and their equality
/// across masters and LRVs (unit b).
///
/// Unit (a)'s part: each file's trailer reads with nothing skipped, in an
/// `inst` box of its size plus 8; its index at EOF − 388 lists record k
/// in slot k; its records, 22 and 28 among them, are the table's ids and
/// sizes, each footer checked; all but 1 and 10 sit on 128 KiB
/// boundaries; the thumbnail is 1280 × 640, the IMU about 1 kHz, and the
/// exposure one item a frame at the take's rate, each shorter than the
/// frame. A table per file says what the index holds.
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
            let t = check_file(&path, role, take, other, lrv);
            summary.push((path, t));
        }
    }
    assert_eq!(summary.len(), 14);
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

/// Every calibration string in the 14 files prints back byte for byte.
#[test]
#[ignore = "M3.2 unit (b)"]
fn samples_calib_print_is_identity() {
    todo!()
}
