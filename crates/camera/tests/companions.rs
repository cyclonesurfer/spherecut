//! A recording's files (M3 plan 5.4), M3.2's unit (c): `companions`
//! from names alone, and `verify_companion` by record 4.
//!
//! The trailers are synth's (`insv::inst_box`), which shares no code with
//! camera's reader. The names are the X5's (insta360.md §2.3), and the
//! agent's `ops/clips.rs` tests, which now go through `companions`, hold
//! the rest of its naming rule.
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): camera's suite ≤ 1 s, which each
//! test prints its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use spherecut_camera::insta360::{
    CompanionError, Companions, InsvName, NameRole, Pairing, Recording, Side, Trailer, companions,
    read_trailer, verify_companion,
};
use spherecut_camera::{CameraError, Part};
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with};
use spherecut_synth::insv::{
    self, ExposureItem, Field, ImuItem, RawRecord, Records, TrailerLayout, X5_ALIGN,
};
use spherecut_testkit::control::must_fail;

/// The suite's budget in seconds: all of camera's (19.6).
const BUDGET_S: f64 = 1.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the budget.
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

// Names.

/// A recording as the X5 names it: 22 October 2026 at 10:15:00, number
/// `seq`.
fn recording(pro: bool, seq: &str) -> Recording {
    Recording {
        pro,
        date: "20261022".into(),
        time: "101500".into(),
        seq: seq.into(),
    }
}

/// What `companions` gives for a file of `role` in `dir`, with `master`
/// and `lrv` its recording's files there.
fn named(r: Recording, role: NameRole, dir: &Path, master: &str, lrv: &str) -> Companions {
    Companions {
        name: Some(InsvName { recording: r, role }),
        master: Some(dir.join(master)),
        lrv: Some(dir.join(lrv)),
    }
}

/// From a master to its LRV and back: each names the other, and itself
/// in its own place. The folder is one that isn't there, as `companions`
/// opens nothing.
#[test]
fn companions_pair_a_master_and_its_lrv_both_ways() {
    let _time = timed("companions_pair_a_master_and_its_lrv_both_ways");
    let dir = Path::new("/nowhere/DCIM/Camera01");
    // (the master, its LRV, its recording, and the name of a second lens
    // that would be the master's on a two-file camera)
    let recordings = [
        (
            "VID_20261022_101500_00_007.insv",
            "LRV_20261022_101500_01_007.lrv",
            recording(false, "007"),
            "VID_20261022_101500_10_007.insv",
        ),
        (
            "PRO_VID_20261022_101500_00_008.insv",
            "PRO_LRV_20261022_101500_01_008.lrv",
            recording(true, "008"),
            "PRO_VID_20261022_101500_10_008.insv",
        ),
        (
            "VID_20261022_101500_00_009-trim.insv",
            "LRV_20261022_101500_01_009-trim.lrv",
            recording(false, "009-trim"),
            "VID_20261022_101500_10_009-trim.insv",
        ),
    ];
    for (master, lrv, r, lens1) in recordings {
        let from_master = companions(&dir.join(master));
        assert_eq!(
            from_master,
            named(r.clone(), NameRole::Lens0, dir, master, lrv),
            "{master}"
        );
        let from_lrv = companions(&dir.join(lrv));
        assert_eq!(
            from_lrv,
            named(r.clone(), NameRole::Proxy, dir, master, lrv),
            "{lrv}"
        );
        // Lens 1's file of a two-file camera names the same two.
        assert_eq!(
            companions(&dir.join(lens1)),
            named(r, NameRole::Lens1, dir, master, lrv),
            "{lens1}"
        );
        // Both ways: the LRV's master is the master, and the master's LRV
        // the LRV.
        assert_eq!(from_lrv.master.as_deref(), Some(dir.join(master).as_path()));
        assert_eq!(from_master.lrv.as_deref(), Some(dir.join(lrv).as_path()));
    }

    // A proxy by its K of 1 or its extension alone is one too: the ONE
    // X2's `_11_` in an `.insv`, and a `VID_` name in an `.lrv`.
    for proxy in [
        "LRV_20261022_101500_11_010.insv",
        "VID_20261022_101500_01_010.insv",
        "VID_20261022_101500_00_010.LRV",
    ] {
        let c = companions(&dir.join(proxy));
        assert_eq!(
            c.name.as_ref().map(|n| n.role),
            Some(NameRole::Proxy),
            "{proxy}"
        );
        assert_eq!(c.lrv.as_deref(), Some(dir.join(proxy).as_path()));
        assert_eq!(
            c.master.as_deref(),
            Some(dir.join("VID_20261022_101500_00_010.insv").as_path()),
            "{proxy}"
        );
    }
    // A name alone, with no folder, names files beside it.
    assert_eq!(
        companions(Path::new("VID_20261022_101500_00_011.mp4")).lrv,
        Some(PathBuf::from("LRV_20261022_101500_01_011.lrv"))
    );

    // The control: another recording's LRV doesn't name this master, by
    // its time or its number.
    let master = dir.join("VID_20261022_101500_00_007.insv");
    for other in [
        "LRV_20261022_101501_01_007.lrv",
        "LRV_20261022_101500_01_017.lrv",
    ] {
        must_fail(
            "another recording's LRV naming this master",
            companions(&dir.join(other)).master.as_ref() == Some(&master),
        );
    }
}

/// Names that aren't an Insta360 camera's, or only look like one, name
/// no companions; `companions` opens nothing.
#[test]
fn companions_of_other_names_are_none() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let _time = timed("companions_of_other_names_are_none");
    let none = Companions {
        name: None,
        master: None,
        lrv: None,
    };
    let dir = Path::new("/nowhere");
    for other in [
        "GS010013.360",
        "clip.mp4",
        "VID_20261022_101500_00_007",
        "VID_20261022_101500_00_007.mov",
        "IMG_20261022_101500_00_007.insv",
        "VID_2026102_101500_00_007.insv",
        "VID_20261022_10150_00_007.insv",
        "VID_20261022_101500_0_007.insv",
        "VID_20261022_101500_0a_007.insv",
        "VID_20261022_101500_00_.insv",
        "VID_20261022_101500_00_-trim.insv",
        "VID_20261022_101500_00_x07.insv",
        "PRO_IMG_20261022_101500_00_007.insv",
        "vid_20261022_101500_00_007.insv",
    ] {
        assert_eq!(companions(&dir.join(other)), none, "{other}");
    }
    // A name that isn't UTF-8, and a path with no name at all.
    let latin1 = OsStr::from_bytes(b"VID_20261022_101500_00_007\xe9.insv");
    assert_eq!(companions(&dir.join(latin1)), none);
    assert_eq!(companions(Path::new("/")), none);
    assert_eq!(companions(Path::new("")), none);

    // The control: the X5's own name, in a folder that isn't there either,
    // names its files.
    must_fail(
        "the X5's own name naming nothing",
        companions(&dir.join("VID_20261022_101500_00_007.insv")) == none,
    );
}

// Bytes.

/// The serial synth's files give, never Pat's.
const SERIAL: &str = "SYNTH00001";
/// The camera's clock at the first frame, in µs.
const FIRST_FRAME_US: u64 = 1_000_000;

/// `frames` of record 4, a frame each 1/30 s from the first frame's time,
/// at 1/500 s.
fn exposure(frames: u64) -> Vec<ExposureItem> {
    (0..frames)
        .map(|i| ExposureItem {
            t_us: FIRST_FRAME_US + i * 1_000_000 / 30,
            seconds: 1.0 / 500.0,
        })
        .collect()
}

/// A master's records: metadata, a few IMU items, and `exposure`.
fn master_records(exposure: Vec<ExposureItem>) -> Records {
    let imu = (0..8)
        .map(|i| ImuItem {
            t_us: FIRST_FRAME_US + i * 1000,
            accel: [32768, 32768, 32768 + 1024],
            gyro: [32768; 3],
        })
        .collect();
    Records {
        metadata: vec![
            Field::string(1, SERIAL),
            Field::string(2, "Insta360 X5"),
            Field::varint(24, FIRST_FRAME_US),
        ],
        thumbnail: None,
        imu,
        exposure,
        others: Vec::new(),
    }
}

/// An LRV's records: other metadata, no IMU, and `exposure`.
fn lrv_records(exposure: Vec<ExposureItem>) -> Records {
    Records {
        metadata: vec![Field::string(1, SERIAL), Field::varint(93, 4000)],
        thumbnail: None,
        imu: Vec::new(),
        exposure,
        others: Vec::new(),
    }
}

/// A file of `at` bytes of a pattern and then `records`' trailer at
/// `align`, with where synth put each part.
struct File {
    bytes: Vec<u8>,
    layout: TrailerLayout,
}

fn file(records: &Records, at: usize, align: u64) -> File {
    let (inst, layout) = insv::inst_box(records, at as u64, align, None);
    let mut bytes: Vec<u8> = (0..at).map(|i| (i % 251) as u8).collect();
    bytes.extend(inst);
    File { bytes, layout }
}

impl File {
    /// Record 4's slot as synth wrote it.
    fn exposure(&self) -> insv::Slot {
        *self.layout.slots.iter().find(|s| s.id == 4).unwrap()
    }

    /// The trailer camera reads from the bytes.
    fn trailer(&self) -> Trailer {
        read_trailer(&mut Cursor::new(&self.bytes), self.bytes.len() as u64)
            .unwrap()
            .unwrap()
    }
}

/// A master of 4,200 frames and its LRV: record 4 of 67,200 bytes, more
/// than one of `verify_companion`'s 64 KiB chunks, at the X5's alignment
/// in the master and at none in the LRV, so that the two records are at
/// different offsets.
fn pair(frames: u64) -> (File, File) {
    (
        file(&master_records(exposure(frames)), 300_000, X5_ALIGN),
        file(&lrv_records(exposure(frames)), 1_000, 1),
    )
}

/// How much of each record 4 the tests compare: media's 64 MiB, which
/// probe passes.
const MAX: usize = 64 << 20;

/// `verify_companion` on the two files' bytes, each with the trailer
/// camera reads from it, comparing at most [`MAX`] bytes.
fn verify(master: &File, lrv: &File) -> Result<Pairing, CompanionError> {
    verify_at_most(master, lrv, MAX)
}

/// `verify_companion` comparing at most `max` bytes.
fn verify_at_most(master: &File, lrv: &File, max: usize) -> Result<Pairing, CompanionError> {
    let (mt, lt) = (master.trailer(), lrv.trailer());
    verify_companion(
        (&mut Cursor::new(&master.bytes), &mt),
        (&mut Cursor::new(&lrv.bytes), &lt),
        max,
    )
}

/// The check: the pair is `Verified`, or what it came to instead, a
/// `NameOnly`'s why or the error, in words.
fn verified(p: Result<Pairing, CompanionError>) -> Result<(), String> {
    match p {
        Ok(Pairing::Verified) => Ok(()),
        Ok(Pairing::NameOnly { why }) => Err(why),
        Err(e) => Err(format!("an error: {e}")),
    }
}

/// A reader that fails every read of the byte at `at`.
struct Failing<'a> {
    inner: Cursor<&'a [u8]>,
    at: u64,
}

impl Read for Failing<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.inner.position();
        if (pos..pos + buf.len() as u64).contains(&self.at) {
            // Up to the byte, then the error on the next read.
            let n = (self.at - pos) as usize;
            if n == 0 {
                return Err(io::Error::other("planted"));
            }
            return self.inner.read(&mut buf[..n]);
        }
        self.inner.read(buf)
    }
}

impl Seek for Failing<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// Equal exposure records are `Verified`; one byte changed, or the
/// record missing from either file, is `NameOnly`.
#[test]
fn verify_companion_compares_record_4() {
    let _time = timed("verify_companion_compares_record_4");
    let (master, lrv) = pair(4200);
    let (m4, l4) = (master.exposure(), lrv.exposure());
    assert_eq!(m4.size, 4200 * 16);
    assert_ne!(m4.offset, l4.offset);
    assert_eq!(verify(&master, &lrv).unwrap(), Pairing::Verified);
    verified(verify(&master, &lrv)).unwrap();

    // A byte changed in the first chunk and in the second, in either
    // file, is found at its byte of the record.
    for at in [0, 5, 65_535, 65_536, 67_199] {
        let mut changed = File {
            bytes: lrv.bytes.clone(),
            layout: lrv.layout.clone(),
        };
        changed.bytes[l4.offset as usize + at] ^= 0x10;
        let p = verified(verify(&master, &changed));
        assert_eq!(
            must_fail("an LRV with a byte changed", p),
            format!("their exposure records (4) differ from byte {at} of the record")
        );
        let mut changed = File {
            bytes: master.bytes.clone(),
            layout: master.layout.clone(),
        };
        changed.bytes[m4.offset as usize + at] ^= 0x01;
        assert_eq!(
            must_fail(
                "a master with a byte changed",
                verified(verify(&changed, &lrv))
            ),
            format!("their exposure records (4) differ from byte {at} of the record")
        );
    }

    // A frame fewer in the LRV: the sizes differ.
    let (_, short) = pair(4199);
    assert_eq!(
        must_fail("an LRV a frame short", verified(verify(&master, &short))),
        "their exposure records (4) differ in size: 67200 bytes in the master, 67184 in the LRV"
    );

    // Record 4 missing from either file, or both.
    let bare_master = file(&master_records(Vec::new()), 300_000, X5_ALIGN);
    let bare_lrv = file(&lrv_records(Vec::new()), 1_000, 1);
    assert!(bare_master.layout.slots.iter().all(|s| s.id != 4));
    for (m, l, said) in [
        (
            &bare_master,
            &lrv,
            "the master has no exposure record (4) to compare with its LRV's",
        ),
        (
            &master,
            &bare_lrv,
            "the LRV has no exposure record (4) to compare with its master's",
        ),
        (
            &bare_master,
            &bare_lrv,
            "neither file has an exposure record (4) to compare",
        ),
    ] {
        assert_eq!(must_fail(said, verified(verify(m, l))), said);
    }

    // A slot for record 4 that runs past the file: `read_trailer` skips
    // it, and comparing it is the error at its slot, as `read_record`
    // gives.
    let mut past = File {
        bytes: lrv.bytes.clone(),
        layout: lrv.layout.clone(),
    };
    let slot = (lrv.layout.index + 4 * 10) as usize;
    let size = u32::try_from(lrv.layout.size).unwrap();
    past.bytes[slot + 6..slot + 10].copy_from_slice(&size.to_le_bytes());
    let t = past.trailer();
    assert!(t.records.iter().all(|r| r.id != 4) && t.skipped.iter().any(|r| r.id == 4));
    let e = must_fail("record 4's slot past the LRV's end", verify(&master, &past));
    assert_eq!(e.side, Side::Lrv, "{e}");
    match e.error {
        CameraError::SlotPastEnd { id: 4, offset } => assert_eq!(offset, slot as u64),
        e => panic!("{e:?}"),
    }
    assert_eq!(
        e.to_string(),
        format!(
            "in the LRV, record 4's index slot at byte {slot} runs into the index or past the file"
        )
    );

    // Bytes that end inside the master's record 4, with the trailer read
    // from the whole file: Truncated at the first byte missing.
    let (mt, lt) = (master.trailer(), lrv.trailer());
    let cut = m4.offset as usize + 66_000;
    let e = must_fail(
        "a master cut inside record 4",
        verify_companion(
            (&mut Cursor::new(&master.bytes[..cut]), &mt),
            (&mut Cursor::new(&lrv.bytes), &lt),
            MAX,
        ),
    );
    // Both files' record 4 is `Part::Record(4)`: the side says which.
    assert_eq!(e.side, Side::Master, "{e}");
    assert_eq!(
        e.to_string(),
        format!("in the master, record 4 is cut short at byte {cut}")
    );
    match e.error {
        CameraError::Truncated {
            part: Part::Record(4),
            offset,
        } => assert_eq!(offset, cut as u64),
        e => panic!("{e:?}"),
    }

    // A read that fails inside the LRV's record: Io at the LRV's byte.
    let bad = l4.offset + 66_000;
    let e = must_fail(
        "an LRV whose read fails inside record 4",
        verify_companion(
            (&mut Cursor::new(&master.bytes), &mt),
            (
                &mut Failing {
                    inner: Cursor::new(&lrv.bytes),
                    at: bad,
                },
                &lt,
            ),
            MAX,
        ),
    );
    assert_eq!(e.side, Side::Lrv, "{e}");
    assert!(e.to_string().starts_with("in the LRV, I/O error"), "{e}");
    match e.error {
        CameraError::Io { offset, .. } => assert_eq!(offset, bad),
        e => panic!("{e:?}"),
    }
}

/// Record 4 of more than `max` bytes isn't compared: paired by name only,
/// saying so, at a byte over and not at the limit itself.
#[test]
fn verify_companion_compares_at_most_max_bytes() {
    let _time = timed("verify_companion_compares_at_most_max_bytes");
    let (master, lrv) = pair(4200);
    let size = master.exposure().size as usize;
    assert_eq!(
        verified(verify_at_most(&master, &lrv, size)),
        Ok(()),
        "at the limit"
    );
    assert_eq!(
        must_fail(
            "a record a byte over the limit, verified",
            verified(verify_at_most(&master, &lrv, size - 1))
        ),
        format!(
            "their exposure records (4) are {size} bytes each, over the {} compared",
            size - 1
        )
    );
    // Over the limit, nothing is read: not even a file cut inside it is
    // an error.
    let (mt, lt) = (master.trailer(), lrv.trailer());
    let cut = master.exposure().offset as usize + 10;
    assert!(matches!(
        verify_companion(
            (&mut Cursor::new(&master.bytes[..cut]), &mt),
            (&mut Cursor::new(&lrv.bytes), &lt),
            size - 1,
        ),
        Ok(Pairing::NameOnly { .. })
    ));
}

/// Two empty exposure records show nothing: paired by name only.
#[test]
fn empty_exposure_records_are_name_only() {
    let _time = timed("empty_exposure_records_are_name_only");
    // synth writes record 4 only with an item; an empty one is a raw
    // record of its id.
    let empty = |mut r: Records| {
        r.others.push(RawRecord {
            id: 4,
            format: 0,
            payload: Vec::new(),
        });
        r
    };
    let master = file(&empty(master_records(Vec::new())), 300_000, X5_ALIGN);
    let lrv = file(&empty(lrv_records(Vec::new())), 1_000, 1);
    assert_eq!(master.exposure().size, 0);
    assert_eq!(lrv.exposure().size, 0);
    assert_eq!(
        must_fail(
            "empty exposure records, verified",
            verified(verify(&master, &lrv))
        ),
        "their exposure records (4) are empty"
    );
    // An empty one against a full one differs in size.
    let (full, _) = pair(2);
    assert_eq!(
        verified(verify(&full, &lrv)),
        Err(
            "their exposure records (4) differ in size: 32 bytes in the master, 0 in the LRV"
                .into()
        )
    );
}

/// What `f` panicked with, run on a thread of its own, marked as the UI
/// thread when `marked`; `None` if it returned.
fn panics_on(marked: bool, f: impl FnOnce() + Send + 'static) -> Option<String> {
    std::thread::spawn(move || {
        if marked {
            mark_ui_thread_with(OnBlock::Panic);
        }
        f();
    })
    .join()
    .err()
    .map(|e| e.downcast_ref::<String>().cloned().unwrap_or_default())
}

/// `verify_companion` calls `may_block` before anything else: on a
/// thread marked as the UI thread it panics, even for files with no
/// record 4, which it would answer without reading.
#[test]
fn verify_companion_panics_on_the_ui_thread() {
    let _time = timed("verify_companion_panics_on_the_ui_thread");
    let (master, lrv) = pair(30);
    let bare = (
        file(&master_records(Vec::new()), 0, 1),
        file(&lrv_records(Vec::new()), 0, 1),
    );
    for (m, l) in [(master, lrv), bare] {
        // The trailers are read here, as `read_trailer` would panic on the
        // UI thread too.
        let (mt, lt) = (m.trailer(), l.trailer());
        let (m, l) = (m.bytes, l.bytes);
        let call = move || {
            drop(verify_companion(
                (&mut Cursor::new(&m), &mt),
                (&mut Cursor::new(&l), &lt),
                MAX,
            ));
        };
        let said =
            panics_on(true, call.clone()).expect("verify_companion returned on the UI thread");
        assert!(
            said.starts_with(
                "comparing an Insta360 master's exposure record with its LRV's would block the UI thread"
            ),
            "{said}"
        );
        // The control: unmarked, the same call returns.
        must_fail(
            "verify_companion on a thread not marked",
            panics_on(false, call).is_some(),
        );
    }
}
