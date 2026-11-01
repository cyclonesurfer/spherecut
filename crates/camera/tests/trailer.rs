//! The trailer reader (M3 plan 5.1 and 5.5), M3.2's unit (a): a trailer
//! cut at every point is an error, arbitrary bytes never panic, each
//! limit is an error at its byte, and the readers refuse the UI thread.
//! The properties run 100,000 cases at `PROPTEST_CASES=100000`.
//!
//! The trailers here are built byte by byte as insta360.md §3 lays out
//! the X5's, by this file's own few lines; synth's writer, which shares
//! nothing with the reader either, holds it to its layout again in
//! `synth_insv.rs` (unit (d)).
//!
//! Budget (M3 plan 4.1 rule 5, 19.6): camera's suite ≤ 1 s at the default
//! 256 cases, which each test prints its time against; the properties
//! run at `PROPTEST_CASES=100000` apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::OnceLock;
use std::time::Instant;

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::{Index, subsequence};
use spherecut_camera::insta360::limits::{MAX_METADATA, MAX_SLOTS, MAX_TRAILER};
use spherecut_camera::insta360::{
    Exposure, ImuSpan, MAGIC, RecordRef, Trailer, imu_span, parse_exposure, parse_metadata,
    read_record, read_trailer, thumbnail,
};
use spherecut_camera::{CameraError, Part};
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with};
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

/// The tail: 32 zeros, the trailer's size, its version and the magic.
const TAIL: usize = 72;
/// From the index's footer to the end: the footer and the tail.
const AFTER_INDEX: usize = 6 + TAIL;
/// An index slot.
const SLOT: usize = 10;

/// A record to lay out.
#[derive(Debug, Clone)]
struct Rec {
    id: u8,
    format: u8,
    payload: Vec<u8>,
}

impl Rec {
    fn new(id: u8, format: u8, payload: Vec<u8>) -> Rec {
        Rec {
            id,
            format,
            payload,
        }
    }
}

/// How a trailer is laid out.
#[derive(Debug, Clone)]
struct Layout {
    /// The bytes before the trailer, an MP4's in a real file.
    prefix: Vec<u8>,
    /// Whether the trailer is inside an `inst` box, as the X5 writes it,
    /// or appended bare.
    inst: bool,
    /// The alignment of the records in the file, the X5's 128 KiB scaled
    /// down; 1 for none. Records 1 and 10 aren't aligned, as on the X5.
    align: usize,
    /// The index's slots: 31 on the X5. Record k goes in slot k.
    slots: usize,
    /// The tail's version.
    version: u32,
}

impl Layout {
    fn x5() -> Layout {
        Layout {
            prefix: Vec::new(),
            inst: true,
            align: 64,
            slots: 31,
            version: 3,
        }
    }
}

/// A file with a trailer, and what the reader should find in it.
#[derive(Debug, Clone)]
struct Built {
    bytes: Vec<u8>,
    /// The trailer as `read_trailer` should give it.
    trailer: Trailer,
    /// The trailer's size E, from its start to the end.
    size: u64,
}

impl Built {
    fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read(&self) -> Result<Option<Trailer>, CameraError> {
        read_trailer(&mut Cursor::new(&self.bytes), self.len())
    }

    /// Record `id` as built.
    fn record(&self, id: u8) -> RecordRef {
        *self.trailer.records.iter().find(|r| r.id == id).unwrap()
    }

    fn payload(&self, r: &RecordRef) -> &[u8] {
        &self.bytes[r.offset as usize..][..r.size as usize]
    }

    /// The same file with its first `k` bytes lost, as the reader should
    /// find it when `k` is no more than the trailer's start.
    fn without_first(&self, k: usize) -> (Vec<u8>, Trailer) {
        let k64 = k as u64;
        let moved = |r: &RecordRef| RecordRef {
            offset: r.offset - k64,
            slot: r.slot - k64,
            ..*r
        };
        let t = Trailer {
            payload_start: self.trailer.payload_start - k64,
            records: self.trailer.records.iter().map(moved).collect(),
            skipped: self.trailer.skipped.iter().map(moved).collect(),
            ..self.trailer.clone()
        };
        (self.bytes[k..].to_vec(), t)
    }

    /// Its trailer alone, the bytes before it lost: a whole trailer.
    fn whole(&self) -> Built {
        let (bytes, trailer) = self.without_first(self.trailer.payload_start as usize);
        Built {
            bytes,
            trailer,
            size: self.size,
        }
    }
}

/// Whether reading came to no trailer, or to an error: what every cut of
/// a whole trailer must come to.
fn no_trailer(read: &Result<Option<Trailer>, CameraError>) -> bool {
    !matches!(read, Ok(Some(_)))
}

/// `recs` laid out as `l` says, as insta360.md §3.1 and §3.2 have it: the
/// records in the order given, each its payload and a footer `{format,
/// id, u32le size}`; the index of `l.slots` slots `{id, format, u32le
/// size, u32le offset from the trailer's start}` with its footer (id 0);
/// and the tail.
fn build(recs: &[Rec], l: &Layout) -> Built {
    let mut b = l.prefix.clone();
    if l.inst {
        b.extend([0; 8]);
    }
    let payload_start = b.len();
    let mut slots = vec![[0u8; SLOT]; l.slots];
    let mut at = Vec::new();
    for r in recs {
        if !matches!(r.id, 1 | 10) {
            b.resize(b.len().next_multiple_of(l.align), 0);
        }
        at.push((r, b.len()));
        let size = (r.payload.len() as u32).to_le_bytes();
        let offset = ((b.len() - payload_start) as u32).to_le_bytes();
        slots[usize::from(r.id)] = [
            r.id, r.format, size[0], size[1], size[2], size[3], offset[0], offset[1], offset[2],
            offset[3],
        ];
        b.extend(&r.payload);
        b.extend([r.format, r.id]);
        b.extend(size);
    }
    let index_at = b.len();
    b.extend(slots.concat());
    b.extend([0, 0]);
    b.extend(((l.slots * SLOT) as u32).to_le_bytes());
    let size = b.len() + TAIL - payload_start;
    b.extend([0; 32]);
    b.extend((size as u32).to_le_bytes());
    b.extend(l.version.to_le_bytes());
    b.extend(MAGIC);
    if l.inst {
        b[payload_start - 8..payload_start]
            .copy_from_slice(&[&((size + 8) as u32).to_be_bytes()[..], b"inst"].concat());
    }
    at.sort_by_key(|(r, _)| r.id);
    let records = at
        .iter()
        .map(|&(r, at)| RecordRef {
            id: r.id,
            format: r.format,
            offset: at as u64,
            size: r.payload.len() as u32,
            slot: (index_at + SLOT * usize::from(r.id)) as u64,
        })
        .collect();
    Built {
        bytes: b,
        trailer: Trailer {
            version: l.version,
            payload_start: payload_start as u64,
            records,
            skipped: Vec::new(),
        },
        size: size as u64,
    }
}

/// An X5-like trailer: Pat's files' ten records, in the X5's order,
/// with small payloads, the exposure two items, the IMU three and the
/// thumbnail 8 × 4.
fn x5_recs() -> Vec<Rec> {
    let fill = |n: usize, seed: u8| -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect()
    };
    let exposure: Vec<u8> = [(1000u64, 0.004f64), (34_367, 0.0035)]
        .iter()
        .flat_map(|&(t, s)| [t.to_le_bytes(), s.to_bits().to_le_bytes()].concat())
        .collect();
    let imu: Vec<u8> = [1003u64, 2004, 3008]
        .iter()
        .flat_map(|&t| [&t.to_le_bytes()[..], &[0x80; 12]].concat())
        .collect();
    let thumb = [
        [1u32, 88, 1, 0, 8, 4, 0, 0, 0, 0]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<u8>>(),
        fill(48, 9),
    ]
    .concat();
    vec![
        Rec::new(29, 0, fill(7, 29)),
        Rec::new(28, 0, fill(13, 28)),
        Rec::new(22, 0, fill(50, 22)),
        Rec::new(11, 0, fill(31, 11)),
        Rec::new(10, 0, fill(35, 10)),
        Rec::new(9, 0, fill(20, 9)),
        Rec::new(4, 0, exposure),
        Rec::new(3, 0, imu),
        Rec::new(2, 0, thumb),
        Rec::new(1, 1, fill(60, 1)),
    ]
}

fn x5_like() -> Built {
    build(&x5_recs(), &Layout::x5())
}

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

/// What a trailer cut at `k` reads as, every way a trailer can be cut,
/// for `whole`, a trailer that is the whole of its file:
///
/// - the file's end lost (the bytes before `k` kept, and the length
///   theirs): no trailer, since the magic went with the end;
/// - its start lost (the bytes from `k` on kept): the tail says more
///   bytes than the file holds, or the file is too short for a tail;
/// - `cut` bytes from `k` lost, the tail kept: the tail says more bytes
///   than the file holds;
/// - the bytes stopping at `k` while the length says they go on, as when
///   a file shrinks after its length was taken: cut short where they
///   stop, or where the tail starts.
fn check_cuts(whole: &Built, k: usize, cut: usize) {
    let b = &whole.bytes;
    let n = b.len();
    assert_eq!(whole.trailer.payload_start, 0, "a whole trailer");
    assert!(k < n);
    let end_lost = read_trailer(&mut Cursor::new(&b[..k]), k as u64);
    assert!(
        matches!(end_lost, Ok(None)),
        "end lost at {k}: {end_lost:?}"
    );

    let too_large = |len: usize, got: Result<Option<Trailer>, CameraError>| {
        if len < TAIL {
            assert!(matches!(got, Ok(None)), "{len} bytes left: {got:?}");
        } else {
            match got {
                Err(CameraError::TooLarge {
                    part: Part::Tail,
                    size,
                    limit,
                    offset,
                }) => assert_eq!(
                    (size, limit, offset),
                    (whole.size, len as u64, (len - 40) as u64)
                ),
                got => panic!("{len} bytes left of {n}: {got:?}"),
            }
        }
    };
    if k > 0 {
        let rest = &b[k..];
        too_large(
            rest.len(),
            read_trailer(&mut Cursor::new(rest), rest.len() as u64),
        );
    }
    let cut = cut.clamp(1, n);
    if k + cut <= n - TAIL {
        let rest = [&b[..k], &b[k + cut..]].concat();
        too_large(
            rest.len(),
            read_trailer(&mut Cursor::new(&rest), rest.len() as u64),
        );
    }
    match read_trailer(&mut Cursor::new(&b[..k]), n as u64) {
        Err(CameraError::Truncated {
            part: Part::Tail,
            offset,
        }) => assert_eq!(offset, k.max(n - TAIL) as u64),
        got => panic!("bytes stopping at {k} of {n}: {got:?}"),
    }
}

/// Where the parts of `whole` start and end, and a byte either side:
/// the cuts most likely to find an off-by-one.
fn boundaries(whole: &Built) -> Vec<usize> {
    let n = whole.bytes.len();
    let mut at = vec![0, n - AFTER_INDEX, n - TAIL, n - 40, n - 36, n - 32, n - 1];
    if let Some(first) = whole.trailer.records.iter().map(|r| r.slot).min() {
        at.push(first as usize);
    }
    for r in &whole.trailer.records {
        let footer = (r.offset + u64::from(r.size)) as usize;
        at.extend([r.offset as usize, footer, footer + 6]);
    }
    at.iter()
        .flat_map(|&b| [b.saturating_sub(1), b, b + 1])
        .filter(|&b| b < n)
        .collect()
}

/// Checks `got`, a file's trailer as read, against the file's bytes
/// themselves: every slot listed says what the trailer has, every record
/// kept ends before the index, its footer repeats its slot, and every
/// record skipped doesn't; and then reads each record, and one more id,
/// through `read_record` with `max`.
fn holds(bytes: &[u8], got: &Trailer, max: usize, also: u8) {
    let le32 = |at: u64| u32::from_le_bytes(bytes[at as usize..][..4].try_into().unwrap());
    let len = bytes.len() as u64;
    assert_eq!(got.version, 3);
    assert!(got.payload_start <= len);
    let index_at = len - AFTER_INDEX as u64 - u64::from(le32(len - AFTER_INDEX as u64 + 2));
    assert!(index_at >= got.payload_start);
    for (r, kept) in got
        .records
        .iter()
        .map(|r| (r, true))
        .chain(got.skipped.iter().map(|r| (r, false)))
    {
        assert!(r.slot >= index_at && (r.slot - index_at).is_multiple_of(SLOT as u64));
        assert!(r.slot + SLOT as u64 <= len - AFTER_INDEX as u64);
        let s = &bytes[r.slot as usize..][..SLOT];
        assert_eq!((s[0], s[1], le32(r.slot + 2)), (r.id, r.format, r.size));
        assert_eq!(
            r.offset,
            got.payload_start + u64::from(le32(r.slot + 6)),
            "{r:?}"
        );
        let end = r.offset + u64::from(r.size) + 6;
        assert_eq!(
            end <= index_at,
            kept,
            "{r:?} against the index at {index_at}"
        );
        if kept {
            let footer = &bytes[(end - 6) as usize..][..6];
            assert_eq!(
                (footer[0], footer[1], le32(end - 4)),
                (r.format, r.id, r.size)
            );
        }
    }
    let ids = got.records.iter().chain(&got.skipped).map(|r| r.id);
    for id in ids.chain([also]).collect::<Vec<_>>() {
        let read = read_record(&mut Cursor::new(bytes), got, id, max);
        let kept = got.records.iter().find(|r| r.id == id);
        let skipped = got.skipped.iter().find(|r| r.id == id);
        match (kept, skipped, read) {
            (
                Some(r),
                _,
                Err(CameraError::TooLarge {
                    part,
                    size,
                    limit,
                    offset,
                }),
            ) => {
                assert!(u64::from(r.size) > max as u64);
                assert_eq!(
                    (part, size, limit, offset),
                    (Part::Record(id), u64::from(r.size), max as u64, r.slot + 2)
                );
            }
            (Some(r), _, Ok(Some(p))) => {
                assert!(r.size as usize <= max);
                assert_eq!(p, &bytes[r.offset as usize..][..r.size as usize]);
            }
            (None, Some(r), Err(CameraError::SlotPastEnd { id: i, offset })) => {
                assert_eq!((i, offset), (id, r.slot));
            }
            (None, None, Ok(None)) => {}
            (kept, skipped, read) => {
                panic!("record {id}: kept {kept:?}, skipped {skipped:?}, read {read:?}")
            }
        }
    }
}

/// Up to eight records, each id once below `slots`, in any order, with
/// any format and payload.
fn records(slots: usize) -> impl Strategy<Value = Vec<Rec>> {
    let ids: Vec<u8> = (1..slots).map(|id| u8::try_from(id).unwrap()).collect();
    subsequence(ids, 0..8.min(slots))
        .prop_shuffle()
        .prop_flat_map(|ids| {
            let n = ids.len();
            (Just(ids), vec((0u8..3, vec(any::<u8>(), 0..40)), n))
        })
        .prop_map(|(ids, bodies)| {
            ids.into_iter()
                .zip(bodies)
                .map(|(id, (format, payload))| Rec::new(id, format, payload))
                .collect()
        })
}

/// Any trailer: its records, and a layout with or without an `inst` box,
/// bytes before it and records aligned.
fn trailer() -> impl Strategy<Value = (Vec<Rec>, Layout)> {
    (
        prop_oneof![Just(31usize), 2usize..=MAX_SLOTS],
        vec(any::<u8>(), 0..40),
        any::<bool>(),
        prop_oneof![Just(1usize), Just(4), Just(16), Just(64)],
    )
        .prop_flat_map(|(slots, prefix, inst, align)| {
            let layout = Layout {
                prefix,
                inst,
                align,
                slots,
                version: 3,
            };
            (records(slots), Just(layout))
        })
}

/// Bytes ending in a tail with the magic, the size `size` and the version
/// `version`, sometimes after an index footer saying `index` bytes.
fn tailed(body: Vec<u8>, index: Option<u32>, size: u32, version: u32) -> Vec<u8> {
    let mut b = body;
    if let Some(index) = index {
        b.extend([0, 0]);
        b.extend(index.to_le_bytes());
    }
    b.extend([0; 32]);
    b.extend(size.to_le_bytes());
    b.extend(version.to_le_bytes());
    b.extend(MAGIC);
    b
}

/// Arbitrary bytes, most with the magic at their end: anything; anything
/// before a tail; slots of anything before an index footer and a tail;
/// and built trailers with bytes changed or a slot overwritten.
fn arbitrary() -> impl Strategy<Value = Vec<u8>> {
    let size = || prop_oneof![0u32..1000, any::<u32>()];
    let version = || prop_oneof![4 => Just(3u32), 1 => any::<u32>()];
    // Whole slots up to 70, and either side of MAX_SLOTS.
    let most = MAX_SLOTS as u32;
    let index = prop_oneof![
        (0u32..=70).prop_map(|n| n * 10),
        (most - 4..=most + 4).prop_map(|n| n * 10),
        0u32..1000,
        any::<u32>()
    ];
    prop_oneof![
        vec(any::<u8>(), 0..400),
        (vec(any::<u8>(), 0..400), size(), version())
            .prop_map(|(body, size, version)| tailed(body, None, size, version)),
        (vec(any::<u8>(), 0..800), index, size(), version()).prop_map(
            |(body, index, size, version)| {
                let n = body.len() as u32;
                // A size the file can hold, mostly.
                let size = if size < 1000 { n.min(size) + 78 } else { size };
                tailed(body, Some(index), size, version)
            }
        ),
        (trailer(), vec((any::<Index>(), any::<u8>()), 1..8)).prop_map(|((recs, l), flips)| {
            let mut b = build(&recs, &l).bytes;
            for (i, x) in flips {
                let i = i.index(b.len());
                b[i] ^= x;
            }
            b
        }),
        (trailer(), any::<Index>(), any::<[u8; SLOT]>()).prop_map(|((recs, l), k, slot)| {
            let built = build(&recs, &l);
            let mut b = built.bytes;
            let index_at = b.len() - AFTER_INDEX - l.slots * SLOT;
            let at = index_at + SLOT * k.index(l.slots);
            b[at..at + SLOT].copy_from_slice(&slot);
            b
        }),
    ]
}

// The properties, each run whole by a test below that times it.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    fn a_trailer_cut_at_every_point_is_an_error_cases(
        (recs, layout) in trailer(),
        picks in vec((any::<Index>(), 1usize..64), 4),
        early in any::<Index>(),
    ) {
        let file = build(&recs, &layout);
        // As built, it reads back as built, payload for payload.
        prop_assert_eq!(file.read().unwrap(), Some(file.trailer.clone()));
        for r in &file.trailer.records {
            let p = read_record(&mut Cursor::new(&file.bytes), &file.trailer, r.id, usize::MAX);
            prop_assert_eq!(p.unwrap(), Some(file.payload(r).to_vec()));
        }
        // A cut before the trailer leaves it whole, its offsets moved.
        let k = early.index(file.trailer.payload_start as usize + 1);
        let (rest, moved) = file.without_first(k);
        prop_assert_eq!(read_trailer(&mut Cursor::new(&rest), rest.len() as u64).unwrap(), Some(moved));

        let whole = file.whole();
        for k in boundaries(&whole) {
            check_cuts(&whole, k, 1);
        }
        for (k, cut) in picks {
            check_cuts(&whole, k.index(whole.bytes.len()), cut);
        }
    }

    fn arbitrary_bytes_never_panic_cases(
        bytes in arbitrary(),
        max in prop_oneof![0usize..64, Just(usize::MAX)],
        also in any::<u8>(),
    ) {
        let len = bytes.len() as u64;
        match read_trailer(&mut Cursor::new(&bytes), len) {
            Ok(Some(t)) => holds(&bytes, &t, max, also),
            Ok(None) => prop_assert!(len < TAIL as u64 || bytes[bytes.len() - 32..] != MAGIC[..]),
            Err(e) => prop_assert!(offset(&e) <= len, "{e:?} past {len}"),
        }
        // A length that isn't the bytes': the reader still returns.
        for wrong in [len / 2, len + 100, u64::MAX] {
            if let Err(e) = read_trailer(&mut Cursor::new(&bytes), wrong) {
                prop_assert!(offset(&e) <= wrong.max(len), "{e:?}");
            }
        }
    }
}

/// Property: every cut of a whole trailer, from its first byte to its
/// last, is an error or no trailer, never a panic or a trailer.
///
/// First every byte of an X5-like trailer, each way `check_cuts` cuts it;
/// then any trailer, at its parts' edges and four points of its own. Each
/// case also checks that the trailer reads back as built, and that losing
/// bytes before it only moves it.
#[test]
fn a_trailer_cut_at_every_point_is_an_error() {
    let _time = timed("a_trailer_cut_at_every_point_is_an_error");
    let file = x5_like();
    let whole = file.whole();
    assert_eq!(whole.read().unwrap(), Some(whole.trailer.clone()));
    assert_eq!(whole.trailer.records.len(), 10);
    for k in 0..whole.bytes.len() {
        check_cuts(&whole, k, 1);
        check_cuts(&whole, k, 37);
    }
    // The control: a cut of bytes before the trailer, three of the inst
    // box's header, is no cut of the trailer, and reads as one.
    let (rest, _) = file.without_first(3);
    must_fail(
        "three bytes of the inst box lost",
        no_trailer(&read_trailer(&mut Cursor::new(&rest), rest.len() as u64)),
    );
    a_trailer_cut_at_every_point_is_an_error_cases();
}

/// Property: `read_trailer` and `read_record` on arbitrary bytes, with
/// and without the tail's magic at their end, return and never panic.
/// Whatever trailer they give is the bytes': each slot listed, record
/// kept and footer checked as `holds` reads them from the bytes itself.
#[test]
fn arbitrary_bytes_never_panic() {
    let _time = timed("arbitrary_bytes_never_panic");
    // The control: `holds` is no rubber stamp. A trailer read from one
    // file doesn't hold for another's bytes.
    let a = x5_like();
    let mut other = x5_recs();
    other[1].payload.push(0);
    let b = build(&other, &Layout::x5());
    let check = std::panic::catch_unwind(|| holds(&b.bytes, &a.trailer, usize::MAX, 0));
    must_fail("one file's trailer held to another's bytes", check);
    arbitrary_bytes_never_panic_cases();
}

/// A reader of `len` bytes, all zero but the last, `tail`, so that a test
/// can read the end of a file larger than it can hold.
struct Sparse {
    len: u64,
    tail: Vec<u8>,
    at: u64,
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let tail_at = self.len - self.tail.len() as u64;
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.at)) as usize;
        for (i, b) in buf[..n].iter_mut().enumerate() {
            let at = self.at + i as u64;
            *b = at.checked_sub(tail_at).map_or(0, |i| self.tail[i as usize]);
        }
        self.at += n as u64;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.at = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::End(d) => self.len.checked_add_signed(d).unwrap(),
            SeekFrom::Current(d) => self.at.checked_add_signed(d).unwrap(),
        };
        Ok(self.at)
    }
}

/// Each limit of `insta360::limits`, and a record over `read_record`'s
/// `max`, is its error at the byte that says so. `MAX_STRING`, and
/// `MAX_METADATA` as `parse_metadata` holds to it, are unit (b)'s, in
/// `strings.rs`; here `MAX_METADATA` is the `max` that record 1 is read
/// with.
#[test]
fn every_limit_is_an_error_with_its_offset() {
    let _time = timed("every_limit_is_an_error_with_its_offset");
    // MAX_TRAILER, in a file larger than it: a trailer of 1 GiB reads,
    // one of a byte more doesn't. All of it is zeros but the index and
    // the tail, so an empty index.
    let len = MAX_TRAILER + 4096;
    let end = |size: u64| {
        let index = vec![0; 31 * SLOT];
        tailed(index, Some(31 * SLOT as u32), size as u32, 3)
    };
    let mut at_limit = Sparse {
        len,
        tail: end(MAX_TRAILER),
        at: 0,
    };
    assert_eq!(
        read_trailer(&mut at_limit, len).unwrap(),
        Some(Trailer {
            version: 3,
            payload_start: 4096,
            records: Vec::new(),
            skipped: Vec::new(),
        })
    );
    let mut over = Sparse {
        len,
        tail: end(MAX_TRAILER + 1),
        at: 0,
    };
    let e = must_fail("a trailer a byte over 1 GiB", read_trailer(&mut over, len));
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Tail, size, limit, offset }
            if (size, limit, offset) == (MAX_TRAILER + 1, MAX_TRAILER, len - 40)),
        "{e:?}"
    );
    // The size it names is the trailer's, which the tail gives.
    assert_eq!(
        e.to_string(),
        format!(
            "the trailer's size in its tail is {} bytes, over its limit of {MAX_TRAILER} (byte {})",
            MAX_TRAILER + 1,
            len - 40
        )
    );

    // The file's length: a trailer as large as its file reads, one a byte
    // larger doesn't.
    let whole = x5_like().whole().bytes;
    assert!(!no_trailer(&read_bytes(&whole)));
    let mut larger = whole.clone();
    let n = larger.len();
    larger[n - 40..n - 36].copy_from_slice(&(n as u32 + 1).to_le_bytes());
    let e = must_fail(
        "a trailer a byte larger than its file",
        read_trailer(&mut Cursor::new(&larger), n as u64),
    );
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Tail, size, limit, offset }
            if (size, limit, offset) == (n as u64 + 1, n as u64, n as u64 - 40)),
        "{e:?}"
    );
    assert_eq!(
        e.to_string(),
        format!(
            "the trailer's size in its tail is {} bytes, more than the file's length of {n} (byte {})",
            n + 1,
            n - 40
        )
    );
    // Control: the same numbers in another part read as that part's own
    // size, not the tail's words.
    let index = CameraError::TooLarge {
        part: Part::Index,
        size: n as u64 + 1,
        limit: n as u64,
        offset: n as u64 - 40,
    };
    must_fail(
        "the index's size in the tail's words",
        index.to_string() == e.to_string(),
    );

    // MAX_SLOTS: an index of 256 slots, one for each id a slot can name,
    // reads with record 255 in its last slot and a TimeMap's 128 among
    // them; one a byte larger doesn't, nor one of 257 slots.
    let recs = [Rec::new(128, 0, vec![9; 4]), Rec::new(255, 0, vec![7; 5])];
    let most = build(
        &recs,
        &Layout {
            slots: MAX_SLOTS,
            ..Layout::x5()
        },
    );
    assert_eq!(MAX_SLOTS, 256);
    assert_eq!(most.read().unwrap(), Some(most.trailer.clone()));
    let n = most.len() as usize;
    let footer_size = n as u64 - AFTER_INDEX as u64 + 2;
    let byte_over = patched(&most.bytes, AFTER_INDEX - 2, &2561u32.to_le_bytes());
    let e = must_fail("an index of 2561 bytes", read_bytes(&byte_over));
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Index, size: 2561, limit: 2560, offset }
            if offset == footer_size),
        "{e:?}"
    );
    let one_more = build(
        &recs,
        &Layout {
            slots: MAX_SLOTS + 1,
            ..Layout::x5()
        },
    );
    let e = must_fail("an index of 257 slots", one_more.read());
    let footer_size = one_more.len() - AFTER_INDEX as u64 + 2;
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Index, size: 2570, limit: 2560, offset }
            if offset == footer_size),
        "{e:?}"
    );

    // read_record's max: a record of 100 bytes reads with a max of 100,
    // not 99, which is an error at its slot's size.
    let file = build(&[Rec::new(7, 0, vec![3; 100])], &Layout::x5());
    let r = file.record(7);
    let read = |max| read_record(&mut Cursor::new(&file.bytes), &file.trailer, 7, max);
    assert_eq!(read(100).unwrap(), Some(vec![3; 100]));
    let e = must_fail("a 100-byte record read with a max of 99", read(99));
    assert!(
        matches!(e, CameraError::TooLarge { part: Part::Record(7), size: 100, limit: 99, offset }
            if offset == r.slot + 2),
        "{e:?}"
    );

    // MAX_METADATA as record 1's max: 1 MiB reads, a byte more doesn't.
    for (size, fits) in [(MAX_METADATA, true), (MAX_METADATA + 1, false)] {
        let file = build(&[Rec::new(1, 1, vec![0x0a; size])], &Layout::x5());
        let read = read_record(
            &mut Cursor::new(&file.bytes),
            &file.trailer,
            1,
            MAX_METADATA,
        );
        if fits {
            assert_eq!(read.unwrap().map(|p| p.len()), Some(size));
        } else {
            let e = must_fail("a metadata record a byte over 1 MiB", read);
            assert!(
                matches!(e, CameraError::TooLarge { part: Part::Record(1), size: s, limit, offset }
                    if (s, limit, offset) == (size as u64, MAX_METADATA as u64, file.record(1).slot + 2)),
                "{e:?}"
            );
        }
    }
}

/// A reader that fails every read of the byte at `at`, and every seek
/// to it when `seek` is set.
struct Failing<'a> {
    inner: Cursor<&'a [u8]>,
    at: u64,
    seek: bool,
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
        let at = self.inner.seek(to)?;
        if self.seek && at == self.at {
            return Err(io::Error::other("planted"));
        }
        Ok(at)
    }
}

/// `whole`'s bytes with `bytes` written at `at` from the end.
fn patched(whole: &[u8], from_end: usize, bytes: &[u8]) -> Vec<u8> {
    let mut b = whole.to_vec();
    let at = b.len() - from_end;
    b[at..at + bytes.len()].copy_from_slice(bytes);
    b
}

fn read_bytes(b: &[u8]) -> Result<Option<Trailer>, CameraError> {
    read_trailer(&mut Cursor::new(b), b.len() as u64)
}

/// Each error the readers give, with the part it names and the byte it
/// is at, against the same file whole, which reads.
#[test]
fn each_trailer_error_is_at_the_byte_that_says_so() {
    let _time = timed("each_trailer_error_is_at_the_byte_that_says_so");
    let file = x5_like();
    let b = &file.bytes;
    let n = file.len();
    assert_eq!(file.read().unwrap(), Some(file.trailer.clone()));

    // Version 4: at the version.
    let e = must_fail(
        "version 4",
        read_bytes(&patched(b, 36, &4u32.to_le_bytes())),
    );
    assert!(
        matches!(e, CameraError::Version { found: 4, offset } if offset == n - 36),
        "{e:?}"
    );

    // A footer that doesn't repeat its slot, by its format, its id or its
    // size: at the footer.
    let r = file.record(4);
    let footer = r.offset + u64::from(r.size);
    for (i, what) in [(0, "format"), (1, "id"), (2, "size")] {
        let mut changed = b.clone();
        changed[footer as usize + i] ^= 1;
        let e = must_fail(
            &format!("record 4's footer with another {what}"),
            read_bytes(&changed),
        );
        assert!(
            matches!(e, CameraError::FooterMismatch { id: 4, offset } if offset == footer),
            "{what}: {e:?}"
        );
        assert_eq!(
            e.to_string(),
            format!("record 4's footer at byte {footer} doesn't match its index slot")
        );
    }
    // A footer at EOF − 78 that isn't an index's: older cameras' records
    // walked from the end. At the footer, the index's own error, saying
    // the trailer has no index rather than naming a slot.
    let e = must_fail(
        "a metadata footer where the index's is",
        read_bytes(&patched(b, 78, &[1, 1])),
    );
    assert!(
        matches!(e, CameraError::IndexFooter { offset } if offset == n - 78),
        "{e:?}"
    );
    let no_index = |e: &CameraError| e.to_string().contains("the trailer has no index");
    assert!(no_index(&e), "{e}");
    must_fail(
        "record 4's footer said to be the index's",
        no_index(&CameraError::FooterMismatch {
            id: 4,
            offset: footer,
        }),
    );
    // A damaged slot that claims id 0, the index's own: here slot 0 with
    // record 29's format, size and offset. It is a record like any
    // other, and its footer, record 29's, doesn't match it: at that
    // footer, as record 0's, not as the index's footer, which is where
    // it belongs and is sound.
    let r29 = file.record(29);
    let footer29 = r29.offset + u64::from(r29.size);
    let index_at = b.len() - AFTER_INDEX - 31 * SLOT;
    let mut claims_0 = b.clone();
    claims_0[index_at..][..SLOT].copy_from_slice(&b[r29.slot as usize..][..SLOT]);
    claims_0[index_at] = 0;
    let e = must_fail("a slot claiming id 0", read_bytes(&claims_0));
    assert!(
        matches!(e, CameraError::FooterMismatch { id: 0, offset } if offset == footer29),
        "{e:?}"
    );
    let as_record_0 = |e: &CameraError| {
        e.to_string()
            == format!("record 0's footer at byte {footer29} doesn't match its index slot")
    };
    assert!(as_record_0(&e), "{e}");
    // The control: the index footer's error at that byte isn't record
    // 0's, as when id 0 alone chose the message. And with the slot's id
    // left 29, the same slot reads: record 29 listed twice.
    must_fail(
        "a slot claiming id 0 worded as the index's footer",
        as_record_0(&CameraError::IndexFooter { offset: footer29 }),
    );
    claims_0[index_at] = 29;
    let twice = read_bytes(&claims_0).unwrap().unwrap();
    assert_eq!(twice.records.iter().filter(|r| r.id == 29).count(), 2);

    // An index that isn't whole slots: at its footer's size.
    let e = must_fail(
        "an index of 305 bytes",
        read_bytes(&patched(b, 76, &305u32.to_le_bytes())),
    );
    assert!(
        matches!(e, CameraError::BadSize { id: 0, size: 305, offset } if offset == n - 76),
        "{e:?}"
    );

    // A trailer too small for its index's footer and the tail (78
    // bytes), or for its index too (388): at the tail's size, the byte
    // that says so, as a trailer too large is.
    // Each is worded by what it is too small for: the index's footer and
    // the tail, or the index, its footer included, and the tail.
    let sized = |size: u32| read_bytes(&patched(b, 40, &size.to_le_bytes()));
    let worded = |size: u32, least: u64, takes: &str| {
        format!(
            "the trailer's size in its tail is {size} bytes, less than the {least} {takes} take (byte {})",
            n - 40
        )
    };
    let (footer, index) = (
        "its index's footer and the tail",
        "its index, footer included, and the tail",
    );
    for (size, least, what, takes) in [
        (77u32, 78u64, "the footer", footer),
        (78 + 300, 388, "the index", index),
    ] {
        let e = must_fail(&format!("a trailer too small for {what}"), sized(size));
        assert!(
            matches!(e, CameraError::TooSmall { part: Part::Tail, size: s, least: l, offset }
                if (s, l, offset) == (u64::from(size), least, n - 40)),
            "{what}: {e:?}"
        );
        assert_eq!(e.to_string(), worded(size, least, takes));
    }
    let e = sized(77).unwrap_err().to_string();
    must_fail(
        "a trailer too small for the footer worded as too small for the index",
        e == worded(77, 78, index),
    );
    // The controls: at the least it isn't too small. A trailer of 388
    // bytes reads, every record's offset then past the index, so
    // skipped; one of 78 has room for the footer, and is too small only
    // for the index.
    let at_least = sized(388).unwrap().unwrap();
    assert_eq!((at_least.records.len(), at_least.skipped.len()), (0, 10));
    must_fail(
        "a trailer of 388 bytes too small",
        matches!(sized(388), Err(CameraError::TooSmall { .. })),
    );
    must_fail(
        "a trailer of 78 bytes too small for the footer",
        matches!(sized(78), Err(CameraError::TooSmall { least: 78, .. })),
    );

    // A slot whose record runs into the index, or past the file: listed
    // as skipped, not followed, and reading it is an error at its slot.
    let r = file.record(29);
    for (offset, what) in [
        (r.slot - file.trailer.payload_start - 3, "into the index"),
        (u32::MAX as u64, "past the file"),
    ] {
        let mut past = b.clone();
        past[r.slot as usize + 6..][..4].copy_from_slice(&(offset as u32).to_le_bytes());
        let t = read_bytes(&past).unwrap().unwrap();
        assert_eq!(t.records.len(), 9, "{what}");
        assert_eq!(
            t.skipped,
            [RecordRef {
                offset: t.payload_start + offset,
                ..r
            }],
            "{what}"
        );
        let e = must_fail(
            &format!("record 29 {what}"),
            read_record(&mut Cursor::new(&past), &t, 29, usize::MAX),
        );
        assert!(
            matches!(e, CameraError::SlotPastEnd { id: 29, offset } if offset == r.slot),
            "{what}: {e:?}"
        );
    }

    // Bytes that stop before the length given: cut short where they stop.
    let short = &b[..b.len() - 10];
    let e = must_fail(
        "a file ten bytes short of its length",
        read_trailer(&mut Cursor::new(short), n),
    );
    assert!(
        matches!(e, CameraError::Truncated { part: Part::Tail, offset } if offset == n - 10),
        "{e:?}"
    );
    // A record's bytes that stop inside its payload, after its trailer was
    // read: cut short in the record, where they stop. Read whole, the
    // same bytes up to its footer give the payload.
    let r = file.record(4);
    let cut = &b[..r.offset as usize + 5];
    let e = must_fail(
        "record 4 cut five bytes into its payload",
        read_record(&mut Cursor::new(cut), &file.trailer, 4, usize::MAX),
    );
    assert!(
        matches!(e, CameraError::Truncated { part: Part::Record(4), offset } if offset == r.offset + 5),
        "{e:?}"
    );
    let upto = &b[..(r.offset + u64::from(r.size)) as usize];
    assert_eq!(
        read_record(&mut Cursor::new(upto), &file.trailer, 4, usize::MAX).unwrap(),
        Some(file.payload(&r).to_vec())
    );

    // A read or a seek that fails: in the part being read, at the byte
    // it was for.
    let r = file.record(4);
    let footer = r.offset + u64::from(r.size);
    let index_at = n - AFTER_INDEX as u64 - 31 * SLOT as u64;
    let tail = (Part::Tail, "the trailer's tail");
    let index = (Part::Index, "the trailer's index");
    let record = (Part::Record(4), "record 4");
    let mut last = String::new();
    for (seek, at, (part, text), what) in [
        (false, n - 50, tail, "the tail"),
        (false, n - 76, index, "the index's footer"),
        (true, index_at, index, "the index"),
        (false, index_at + 15, index, "the index"),
        (false, footer + 2, record, "record 4's footer"),
        (true, footer, record, "record 4's footer"),
        (false, r.offset + 5, record, "record 4"),
    ] {
        let mut failing = Failing {
            inner: Cursor::new(b),
            at,
            seek,
        };
        let got = read_trailer(&mut failing, n)
            .and_then(|t| read_record(&mut failing, &t.unwrap(), 4, usize::MAX));
        let e = must_fail(
            &format!("a failed {} in {what}", if seek { "seek" } else { "read" }),
            got,
        );
        assert!(
            matches!(&e, CameraError::Io { part: p, offset, error }
                if (*p, *offset) == (part, at) && error.to_string() == "planted"),
            "{what}: {e:?}"
        );
        last = format!("I/O error reading {text} at byte {at}: planted");
        assert_eq!(e.to_string(), last, "{what}");
    }
    // The control: the same failure in the tail doesn't read as record
    // 4's.
    let in_tail = CameraError::Io {
        part: Part::Tail,
        offset: r.offset + 5,
        error: io::Error::other("planted"),
    };
    must_fail(
        "a failed read in record 4 worded as the tail's",
        in_tail.to_string() == last,
    );
}

/// `WireType`'s two cases are worded apart: a wire type the metadata
/// doesn't use at all, and one it uses, for another field than the one
/// whose key is there. Unit (b)'s error tables, at their bytes, are in
/// `strings.rs`; this is the wording, through `parse_metadata`.
#[test]
fn wire_type_messages_tell_a_field_from_the_walk() {
    let _time = timed("wire_type_messages_tell_a_field_from_the_walk");
    // Field 93 (a varint) as a length of 0: key (93 << 3) | 2.
    let e = must_fail(
        "field 93 in wire type 2",
        parse_metadata(&[0xea, 0x05, 0x00]),
    );
    assert!(
        matches!(
            e,
            CameraError::WireType {
                part: Part::Field(93),
                wire: 2,
                offset: 0
            }
        ),
        "{e:?}"
    );
    let field = e.to_string();
    assert_eq!(
        field,
        "the metadata's field 93 has wire type 2 at byte 0, not the field's own"
    );
    // Field 9, unknown, as a group: key (9 << 3) | 3.
    let e = must_fail("a group", parse_metadata(&[0x4b]));
    assert!(
        matches!(
            e,
            CameraError::WireType {
                part: Part::Record(1),
                wire: 3,
                offset: 0
            }
        ),
        "{e:?}"
    );
    let walk = e.to_string();
    assert_eq!(
        walk,
        "record 1 has wire type 3 at byte 0, which the metadata doesn't use"
    );
    // The control: the field's message doesn't say the metadata has no
    // use for wire type 2, as it did when one wording served both.
    let unused = |m: &str| m.ends_with("which the metadata doesn't use");
    assert!(unused(&walk));
    must_fail(
        "field 93 in wire type 2 worded as the walk's",
        unused(&field),
    );
}

/// The record parsers read what was written, and refuse a size their
/// layout can't have, at the byte where it goes wrong.
#[test]
fn the_records_parse_as_written() {
    let _time = timed("the_records_parse_as_written");
    let file = x5_like();
    let payload = |id| file.payload(&file.record(id)).to_vec();

    assert_eq!(
        parse_exposure(&payload(4)).unwrap(),
        [
            Exposure {
                t_us: 1000,
                seconds: 0.004
            },
            Exposure {
                t_us: 34_367,
                seconds: 0.0035
            }
        ]
    );
    assert_eq!(parse_exposure(&[]).unwrap(), []);
    let e = must_fail("an exposure record of 33 bytes", parse_exposure(&[0; 33]));
    assert!(
        matches!(
            e,
            CameraError::BadSize {
                id: 4,
                size: 33,
                offset: 32
            }
        ),
        "{e:?}"
    );

    assert_eq!(
        imu_span(&payload(3)).unwrap(),
        Some(ImuSpan {
            samples: 3,
            first_us: 1003,
            last_us: 3008
        })
    );
    assert_eq!(imu_span(&[]).unwrap(), None);
    let e = must_fail("an IMU record of 41 bytes", imu_span(&[0; 41]));
    assert!(
        matches!(
            e,
            CameraError::BadSize {
                id: 3,
                size: 41,
                offset: 40
            }
        ),
        "{e:?}"
    );

    let thumb = payload(2);
    let image = thumbnail(&thumb).unwrap();
    assert_eq!((image.width, image.height), (8, 4));
    assert_eq!(image.y, thumb[40..72]);
    assert_eq!(image.uv, thumb[72..]);
    let e = must_fail(
        "a thumbnail shorter than its header",
        thumbnail(&thumb[..39]),
    );
    assert!(
        matches!(
            e,
            CameraError::Truncated {
                part: Part::Record(2),
                offset: 39
            }
        ),
        "{e:?}"
    );
    let e = must_fail(
        "a thumbnail a byte short",
        thumbnail(&thumb[..thumb.len() - 1]),
    );
    assert!(
        matches!(
            e,
            CameraError::BadSize {
                id: 2,
                size: 87,
                offset: 16
            }
        ),
        "{e:?}"
    );
    // 7 × 4 with the same bytes: luma and chroma don't add up, and NV12
    // can't halve an odd width anyway.
    let mut odd = thumb.clone();
    odd[16] = 7;
    let e = must_fail("a thumbnail 7 wide", thumbnail(&odd));
    assert!(
        matches!(
            e,
            CameraError::BadSize {
                id: 2,
                size: 88,
                offset: 16
            }
        ),
        "{e:?}"
    );
    // A header whose width and height overflow 32 bits between them.
    let mut huge = thumb;
    huge[16..24].copy_from_slice(&[0xfe, 0xff, 0xff, 0xff, 0xfe, 0xff, 0xff, 0xff]);
    let e = must_fail("a thumbnail of 2³² × 2³²", thumbnail(&huge));
    assert!(matches!(e, CameraError::BadSize { id: 2, .. }), "{e:?}");
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

/// `read_trailer` calls `may_block` before anything else: on a thread
/// marked as the UI thread it panics, even for a file too short to have
/// a tail, which it would answer without reading.
#[test]
fn read_trailer_panics_on_the_ui_thread() {
    let _time = timed("read_trailer_panics_on_the_ui_thread");
    for bytes in [x5_like().bytes, vec![0; 10]] {
        let call = move || {
            let len = bytes.len() as u64;
            drop(read_trailer(&mut Cursor::new(bytes), len));
        };
        let m = panics_on(true, call.clone()).expect("read_trailer returned on the UI thread");
        assert!(
            m.starts_with("reading an Insta360 file's trailer would block the UI thread"),
            "{m}"
        );
        // The control: unmarked, the same call returns.
        must_fail(
            "read_trailer on a thread not marked",
            panics_on(false, call).is_some(),
        );
    }
}

/// `read_record` calls `may_block` before anything else: on a thread
/// marked as the UI thread it panics, even for a record the trailer
/// doesn't list, which it would answer without reading.
#[test]
fn read_record_panics_on_the_ui_thread() {
    let _time = timed("read_record_panics_on_the_ui_thread");
    let file = x5_like();
    for id in [4, 5] {
        let (bytes, trailer) = (file.bytes.clone(), file.trailer.clone());
        let call = move || {
            drop(read_record(
                &mut Cursor::new(bytes),
                &trailer,
                id,
                usize::MAX,
            ))
        };
        let m = panics_on(true, call.clone()).expect("read_record returned on the UI thread");
        assert!(
            m.starts_with(
                "reading a record of an Insta360 file's trailer would block the UI thread"
            ),
            "record {id}: {m}"
        );
        // The control: unmarked, the same call returns.
        must_fail(
            "read_record on a thread not marked",
            panics_on(false, call).is_some(),
        );
    }
}
