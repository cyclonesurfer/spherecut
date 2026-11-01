//! `mp4::track_timing`: the timing tables P-TRK compares between a
//! camera's two lenses (M3 plan 4.3) and the frame lock compares over
//! their common prefix (section 9).
//!
//! Files built box by box are read back as written, and every check the
//! reader makes has a file that fails it, named by the box. The
//! comparisons over the common prefix, and the first frame's time, are
//! checked against counting sample by sample. Files that FFmpeg's own
//! muxer wrote (`Mp4Writer`) are timed as FFmpeg's demuxer reads them
//! (`PacketReader`), an instrument that shares no code with the box
//! reader. No bytes, and no cut of a file, make it panic.
//!
//! Budget (M3 plan 4.1 rule 5): 3 s for the suite at the default 256
//! cases; the property tests run at `PROPTEST_CASES=100000` apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;
use spherecut_core::FrameRate;
use spherecut_media::encode::{
    AudioTrack, ColorTags, Mp4Writer, MuxOptions, PixelLayout, VideoSettings, YuvPlanes,
};
use spherecut_media::mp4::{self, Edit, EditList, Mp4Box, Mp4Error, TrackTiming};
use spherecut_media::packets::{NextPacket, PacketInfo, PacketReader};
use spherecut_testkit::control::must_fail;
use spherecut_testkit::fnv64;

/// Rate 1.0 in an edit: 16.16 fixed point.
const RATE_1: u32 = 0x0001_0000;

/// Why a file with no `moov` box has no track.
const NO_MOOV: &str = "the file has no `moov` box, so no index of the media";

fn bx(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(t);
    v.extend_from_slice(payload);
    v
}

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_be_bytes()).collect()
}

/// A full box's version and flags (none).
fn version(v: u8) -> Vec<u8> {
    (u32::from(v) << 24).to_be_bytes().to_vec()
}

fn mdhd(timescale: u32) -> Vec<u8> {
    // Version 0: creation and modification times, the timescale, the
    // duration, and the language.
    words(&[0, 0, 0, timescale, 0, 0x55c4_0000])
}

fn mvhd(timescale: u32) -> Vec<u8> {
    [words(&[0, 0, 0, timescale, 0]), vec![0; 80]].concat()
}

/// Sample sizes for `count` samples of a byte each.
fn stsz(count: u32) -> Vec<u8> {
    words(&[0, 1, count])
}

/// A sample table of version 0 that claims `entries` entries and holds
/// the words `w`.
fn table(entries: usize, w: &[u32]) -> Vec<u8> {
    [words(&[0, entries as u32]), words(w)].concat()
}

fn ctts(v: u8, runs: &[(u32, i32)]) -> Vec<u8> {
    let w: Vec<u32> = runs.iter().flat_map(|&(n, o)| [n, o as u32]).collect();
    [version(v), words(&[runs.len() as u32]), words(&w)].concat()
}

/// An edit as (duration, media time, rate).
type EditSpec = (u64, i64, u32);

/// An `elst` of `edits`, with 32-bit times in version 0 and 64-bit ones
/// in version 1.
fn elst(v: u8, edits: &[EditSpec]) -> Vec<u8> {
    let mut p = [version(v), words(&[edits.len() as u32])].concat();
    for &(d, t, rate) in edits {
        if v == 1 {
            p.extend(d.to_be_bytes());
            p.extend(t.to_be_bytes());
        } else {
            p.extend((d as u32).to_be_bytes());
            p.extend((t as i32).to_be_bytes());
        }
        p.extend(rate.to_be_bytes());
    }
    p
}

/// The payloads of the boxes a track's timing is read from, version and
/// flags included, so that a test can put any bytes in any of them.
#[derive(Debug, Clone)]
struct Boxes {
    handler: [u8; 4],
    /// The type of its one sample entry.
    entry: [u8; 4],
    mdhd: Vec<u8>,
    stts: Option<Vec<u8>>,
    stsz: Option<Vec<u8>>,
    ctts: Option<Vec<u8>>,
    stss: Option<Vec<u8>>,
    elst: Option<Vec<u8>>,
}

/// A video track's tables, as they should read back.
#[derive(Debug, Clone)]
struct Spec {
    timescale: u32,
    stts: Vec<(u32, u32)>,
    /// `ctts`'s version and its runs of (samples, offset).
    ctts: Option<(u8, Vec<(u32, i32)>)>,
    stss: Option<Vec<u32>>,
    /// `elst`'s version and its edits.
    elst: Option<(u8, Vec<EditSpec>)>,
}

impl Spec {
    fn new(timescale: u32, stts: &[(u32, u32)]) -> Spec {
        Spec {
            timescale,
            stts: stts.to_vec(),
            ctts: None,
            stss: None,
            elst: None,
        }
    }

    fn count(&self) -> u32 {
        self.stts.iter().map(|r| r.0).sum()
    }

    fn boxes(&self) -> Boxes {
        let stts: Vec<u32> = self.stts.iter().flat_map(|&(n, d)| [n, d]).collect();
        Boxes {
            handler: *b"vide",
            entry: *b"hvc1",
            mdhd: mdhd(self.timescale),
            stts: Some(table(self.stts.len(), &stts)),
            stsz: Some(stsz(self.count())),
            ctts: self.ctts.as_ref().map(|(v, runs)| ctts(*v, runs)),
            stss: self.stss.as_ref().map(|s| table(s.len(), s)),
            elst: self.elst.as_ref().map(|(v, edits)| elst(*v, edits)),
        }
    }
}

/// An audio track of 48 kHz frames of 1024 samples.
fn audio(frames: u32) -> Boxes {
    Boxes {
        handler: *b"soun",
        entry: *b"mp4a",
        ..Spec::new(48000, &[(frames, 1024)]).boxes()
    }
}

fn trak(id: u32, b: &Boxes) -> Vec<u8> {
    let tkhd = bx(b"tkhd", &[words(&[0, 0, 0, id]), vec![0; 80]].concat());
    let hdlr = bx(
        b"hdlr",
        &[words(&[0, 0]), b.handler.to_vec(), vec![0; 13]].concat(),
    );
    // The fixed fields of a video or an audio sample entry.
    let fixed = if b.entry == *b"hvc1" { 78 } else { 28 };
    let stsd = bx(
        b"stsd",
        &[words(&[0, 1]), bx(&b.entry, &vec![0; fixed])].concat(),
    );
    let mut tables = stsd;
    for (t, p) in [
        (b"stts", &b.stts),
        (b"stsz", &b.stsz),
        (b"ctts", &b.ctts),
        (b"stss", &b.stss),
    ] {
        if let Some(p) = p {
            tables.extend(bx(t, p));
        }
    }
    let minf = bx(b"minf", &bx(b"stbl", &tables));
    let mdia = bx(b"mdia", &[bx(b"mdhd", &b.mdhd), hdlr, minf].concat());
    let edts = b
        .elst
        .as_ref()
        .map(|e| bx(b"edts", &bx(b"elst", e)))
        .unwrap_or_default();
    bx(b"trak", &[tkhd, edts, mdia].concat())
}

/// `ftyp`, a `moov` of `children`, and an empty `mdat`.
fn file_of(children: &[Vec<u8>]) -> Vec<u8> {
    [
        bx(b"ftyp", b"isom\0\0\x02\0isom"),
        bx(b"moov", &children.concat()),
        bx(b"mdat", &[]),
    ]
    .concat()
}

/// A file of the tracks, in a movie at `movie_timescale`.
fn file(movie_timescale: u32, tracks: &[Boxes]) -> Vec<u8> {
    let mut children = vec![bx(b"mvhd", &mvhd(movie_timescale))];
    children.extend(tracks.iter().zip(1..).map(|(t, id)| trak(id, t)));
    file_of(&children)
}

fn timing(f: &[u8], stream: usize) -> Result<TrackTiming, Mp4Error> {
    let tree = mp4::read(&mut io::Cursor::new(f))?;
    mp4::track_timing_in(&tree, stream)
}

/// The box a `Malformed` error names, and why.
fn named(e: Mp4Error) -> (String, String) {
    match e {
        Mp4Error::Malformed { fourcc, why, .. } => (fourcc, why),
        e => panic!("not an error naming a box: {e}"),
    }
}

/// Why there is no video track, for a `NoTrack` error.
fn no_track(e: Mp4Error) -> String {
    match e {
        Mp4Error::NoTrack { why, .. } => why,
        e => panic!("not a missing track: {e}"),
    }
}

/// Whether two tracks' decode times agree over their common prefix, or
/// the first sample where they don't.
fn same_times(a: &TrackTiming, b: &TrackTiming) -> Result<(), u64> {
    a.stts_mismatch(b).map_or(Ok(()), Err)
}

/// One of the X5 master's lens tracks, as its 8K30 masters time them:
/// 114 frames at 1/30000, 1001 ticks apart, a keyframe every 60, and one
/// edit that shows them all.
fn x5_lens() -> Spec {
    Spec {
        ctts: Some((0, vec![(114, 0)])),
        stss: Some(vec![1, 61]),
        elst: Some((0, vec![(114_114, 0, RATE_1)])),
        ..Spec::new(30000, &[(114, 1001)])
    }
}

#[test]
fn the_tables_of_a_known_file() {
    // Lens 1's runs are split where lens 0's aren't: the same times.
    let lens1 = Spec {
        stts: vec![(60, 1001), (54, 1001)],
        ..x5_lens()
    };
    let f = file(30000, &[x5_lens().boxes(), lens1.boxes(), audio(177)]);
    let t0 = timing(&f, 0).unwrap();
    assert_eq!(
        t0,
        TrackTiming {
            timescale: 30000,
            count: 114,
            first_pts: 0,
            edit: Some(EditList {
                movie_timescale: 30000,
                edits: vec![Edit {
                    duration: 114_114,
                    media_time: 0,
                    rate: RATE_1
                }],
            }),
            stts: vec![(114, 1001)],
            // testkit's FNV-1a, of the entry as stored.
            ctts_hash: Some(fnv64(&words(&[114, 0]))),
            stss: Some(vec![1, 61]),
        }
    );
    let t1 = timing(&f, 1).unwrap();
    assert_eq!(t1.stts, [(60, 1001), (54, 1001)]);
    assert_eq!(same_times(&t0, &t1), Ok(()));
    assert_eq!(t0.stss_mismatch(&t1), None);

    // The audio is stream 2, and there is no stream 3.
    assert_eq!(
        no_track(timing(&f, 2).unwrap_err()),
        "it is a `soun` track, not video"
    );
    assert_eq!(
        no_track(timing(&f, 3).unwrap_err()),
        "the file has 3 tracks, so none is stream 3"
    );

    // Control: a lens at 25 fps, as the 8K25 master's, is out by the
    // second frame.
    let k25 = Spec::new(25000, &[(116, 1000)]);
    let t25 = timing(&file(30000, &[k25.boxes()]), 0).unwrap();
    assert_eq!(must_fail("29.97 against 25 fps", same_times(&t0, &t25)), 1);
}

#[test]
fn an_edit_list_moves_the_first_frame() {
    // 90 frames at 1/15360, 512 ticks apart, in a movie at 1/1000.
    let at = |v: u8, edits: &[EditSpec], movie: u32| {
        let s = Spec {
            elst: Some((v, edits.to_vec())),
            ..Spec::new(15360, &[(90, 512)])
        };
        timing(&file(movie, &[s.boxes()]), 0)
    };
    let without = timing(&file(1000, &[Spec::new(15360, &[(90, 512)]).boxes()]), 0).unwrap();
    assert_eq!((without.first_pts, without.edit), (0, None));
    for v in [0, 1] {
        let first = |edits: &[EditSpec], movie| at(v, edits, movie).unwrap().first_pts;
        // Two frames of lead-in hidden: the third is shown first, at 0.
        assert_eq!(first(&[(3000, 1024, RATE_1)], 1000), 0);
        // An empty edit of half a second first: 7680 ticks.
        assert_eq!(first(&[(500, -1, RATE_1), (3000, 0, RATE_1)], 1000), 7680);
        // Two empty edits add up, each rounded to the nearest tick as
        // FFmpeg rounds them: 1 ms is 15.36 ticks and 2 ms 30.72, so
        // 15 + 31.
        assert_eq!(
            first(&[(1, -1, RATE_1), (2, -1, RATE_1), (3000, 0, RATE_1)], 1000),
            46
        );
        // Half a tick rounds away from zero, as FFmpeg's av_rescale does,
        // so two halves make 2.
        assert_eq!(first(&[(1, -1, RATE_1), (3000, 0, RATE_1)], 30720), 1);
        let halves = first(
            &[(1, -1, RATE_1), (1, -1, RATE_1), (3000, 0, RATE_1)],
            30720,
        );
        assert_eq!(halves, 2);
        // Control: the two added before rounding make one tick.
        let once = (2 * 15360 + 30720 / 2) / 30720;
        must_fail("the empty edits rounded once", halves == once);
        // An edit that starts between frames shows the next one first,
        // 24 ticks after the edit's start. Its rate doesn't move it.
        assert_eq!(first(&[(3000, 1000, RATE_1)], 1000), 24);
        assert_eq!(first(&[(3000, 1000, 0x8000)], 1000), 24);
        // Only the first edit that shows media counts.
        assert_eq!(first(&[(1000, 0, RATE_1), (1000, 20480, RATE_1)], 1000), 0);
    }
    // The list is read as stored, with the movie's timescale.
    let t = at(1, &[(500, -1, RATE_1), (3000, 1024, RATE_1)], 1000).unwrap();
    let edit = t.edit.unwrap();
    assert_eq!(edit.movie_timescale, 1000);
    assert_eq!(
        edit.edits,
        [
            Edit {
                duration: 500,
                media_time: -1,
                rate: RATE_1
            },
            Edit {
                duration: 3000,
                media_time: 1024,
                rate: RATE_1
            }
        ]
    );
    // A list of no edits, which FFmpeg ignores, moves nothing.
    let none = at(0, &[], 1000).unwrap();
    assert_eq!((none.first_pts, none.edit.unwrap().edits.len()), (0, 0));

    // Version 1's times past 32 bits: an empty edit of 2^32 + 1000 ms,
    // then the media from 2^33 + 2^30 ticks, where the fourth frame is
    // decoded after three of 0xC000_0000 ticks.
    let wide_edits = vec![
        ((1 << 32) + 1000, -1, RATE_1),
        (3000, 0x2_4000_0000, RATE_1),
    ];
    let wide = Spec {
        elst: Some((1, wide_edits.clone())),
        ..Spec::new(15360, &[(3, 0xC000_0000), (4, 512)])
    };
    let t = timing(&file(1000, &[wide.boxes()]), 0).unwrap();
    // 2^32 + 1000 ms is 65970713026.56 ticks.
    assert_eq!(t.first_pts, 65_970_713_027);
    let read: Vec<EditSpec> = t
        .edit
        .unwrap()
        .edits
        .iter()
        .map(|e| (e.duration, e.media_time, e.rate))
        .collect();
    assert_eq!(read, wide_edits);
    // Control: the edits cut to their low words, as a reader of 32 bits
    // would take them, show another frame first, at another time.
    let low = Spec {
        elst: Some((1, vec![(1000, -1, RATE_1), (3000, 0x4000_0000, RATE_1)])),
        ..wide.clone()
    };
    let low = timing(&file(1000, &[low.boxes()]), 0).unwrap();
    must_fail("the low words", low.first_pts == t.first_pts);

    // Controls: lists that can't say when the first frame is shown.
    for (edits, why) in [
        (
            &[(500, -1, RATE_1)][..],
            "every edit is empty, so it shows none of the media",
        ),
        (
            &[(3000, -2, RATE_1)][..],
            "edit 0 starts at media time -2, but only an empty edit's, -1, is below 0",
        ),
        (
            &[(3000, 46081, RATE_1)][..],
            "its first edit that shows media starts at media time 46081, after every sample",
        ),
    ] {
        let e = must_fail(why, at(0, edits, 1000));
        assert_eq!(named(e), ("elst".to_string(), why.to_string()));
    }
    let short = Boxes {
        elst: Some([version(0), words(&[2])].concat()),
        ..Spec::new(15360, &[(90, 512)]).boxes()
    };
    let e = must_fail(
        "an elst short of its edits",
        timing(&file(1000, &[short]), 0),
    );
    assert_eq!(
        named(e),
        (
            "elst".to_string(),
            "`elst` claims 2 entries but has room for 0".to_string()
        )
    );
    // Without an `mvhd`, an edit's duration has no timescale.
    let s = Spec {
        elst: Some((0, vec![(3000, 0, RATE_1)])),
        ..Spec::new(15360, &[(90, 512)])
    };
    let e = must_fail("no mvhd", timing(&file_of(&[trak(1, &s.boxes())]), 0));
    assert_eq!(named(e).0, "moov");
}

#[test]
fn composition_offsets_set_the_first_frame() {
    // I0 P3 B1 B2 in decode order, shown a frame later than decoded: at
    // 512, 2048, 1024 and 1536, so the first is shown at 512, and at 0
    // once the edit list skips the delay, as FFmpeg's muxer writes it.
    let b_frames = vec![(1, 512), (1, 1536), (2, 0)];
    let s = Spec {
        ctts: Some((0, b_frames.clone())),
        ..Spec::new(15360, &[(4, 512)])
    };
    let t = timing(&file(1000, &[s.boxes()]), 0).unwrap();
    assert_eq!(t.first_pts, 512);
    assert_eq!(t.ctts_hash, Some(fnv64(&words(&[1, 512, 1, 1536, 2, 0]))));
    let edited = Spec {
        elst: Some((0, vec![(133, 512, RATE_1)])),
        ..s.clone()
    };
    assert_eq!(
        timing(&file(1000, &[edited.boxes()]), 0).unwrap().first_pts,
        0
    );
    // Negative offsets (version 1), as `+negative_cts_offsets` writes
    // them: the I frame is shown at 0.
    let negative = Spec {
        ctts: Some((1, vec![(1, 0), (1, 1024), (2, -512)])),
        ..Spec::new(15360, &[(4, 512)])
    };
    let n = timing(&file(1000, &[negative.boxes()]), 0).unwrap();
    assert_eq!(n.first_pts, 0);
    // Different offsets hash apart; without a `ctts` there's no hash.
    assert_ne!(n.ctts_hash, t.ctts_hash);
    let plain = timing(&file(1000, &[Spec::new(15360, &[(4, 512)]).boxes()]), 0).unwrap();
    assert_eq!((plain.first_pts, plain.ctts_hash), (0, None));

    // Control: offsets for 3 of the 4 samples.
    let short = Spec {
        ctts: Some((0, vec![(1, 512), (2, 0)])),
        ..Spec::new(15360, &[(4, 512)])
    };
    let e = must_fail("ctts for 3 of 4", timing(&file(1000, &[short.boxes()]), 0));
    assert_eq!(
        named(e),
        (
            "ctts".to_string(),
            "it gives offsets for 3 samples, but the track has 4".to_string()
        )
    );
}

#[test]
fn a_track_without_stss_has_every_sample_sync() {
    let all = timing(&file(1000, &[Spec::new(15360, &[(10, 512)]).boxes()]), 0).unwrap();
    assert_eq!(all.stss, None);
    let listed = |stss: &[u32], frames: u32| {
        let s = Spec {
            stss: Some(stss.to_vec()),
            ..Spec::new(15360, &[(frames, 512)])
        };
        timing(&file(1000, &[s.boxes()]), 0).unwrap()
    };
    // Every sample listed is the same as no list.
    let every: Vec<u32> = (1..=10).collect();
    assert_eq!(all.stss_mismatch(&listed(&every, 10)), None);
    // Sync samples past the shorter track don't count.
    let gop5 = listed(&[1, 6, 11], 12);
    assert_eq!(listed(&[1, 6], 10).stss_mismatch(&gop5), None);
    // Controls: sample 2 not sync; GOPs of 5 and 4.
    let missing = listed(&[1, 3, 4, 5, 6, 7, 8, 9, 10], 10);
    assert_eq!(
        must_fail(
            "sample 2 not sync",
            all.stss_mismatch(&missing).map_or(Ok(()), Err)
        ),
        2
    );
    let gop4 = listed(&[1, 5, 9], 12);
    assert_eq!(
        must_fail(
            "GOPs of 5 and 4",
            gop5.stss_mismatch(&gop4).map_or(Ok(()), Err)
        ),
        5
    );
}

#[test]
fn bad_tables_are_errors_naming_their_box() {
    let good = Spec {
        ctts: Some((0, vec![(90, 0)])),
        stss: Some(vec![1, 31, 61]),
        ..Spec::new(15360, &[(90, 512)])
    };
    assert!(timing(&file(1000, &[good.boxes()]), 0).is_ok());
    let with = |f: &dyn Fn(&mut Boxes)| {
        let mut b = good.boxes();
        f(&mut b);
        timing(&file(1000, &[b]), 0)
    };
    type Case<'a> = (&'a dyn Fn(&mut Boxes), &'a str, &'a str);
    let cases: [Case<'_>; 12] = [
        (
            &|b| b.stss = Some(table(2, &[0, 31])),
            "stss",
            "entry 0 is sample 0, but samples count from 1",
        ),
        (
            &|b| b.stss = Some(table(3, &[1, 31, 31])),
            "stss",
            "entry 2 is sample 31, but the entry before it is sample 31",
        ),
        (
            &|b| b.stss = Some(table(2, &[1, 91])),
            "stss",
            "entry 1 is sample 91, but the track has 90",
        ),
        (
            &|b| b.stss = Some(table(3, &[1])),
            "stss",
            "`stss` claims 3 entries but has room for 1",
        ),
        (
            &|b| b.stts = Some(table(1, &[89, 512])),
            "stts",
            "it times 89 samples, but `stsz` sizes 90",
        ),
        (
            &|b| b.stts = Some(table(1, &[])),
            "stts",
            "`stts` claims 1 entries but has room for 0",
        ),
        (&|b| b.stts = None, "stbl", "it has no `stts` box"),
        (
            &|b| b.stsz = None,
            "stbl",
            "it has no sample sizes (`stsz` or `stz2`)",
        ),
        (
            &|b| b.mdhd = mdhd(0),
            "mdhd",
            "its timescale is 0, which FFmpeg can't use",
        ),
        (
            &|b| b.mdhd = mdhd(1 << 31),
            "mdhd",
            "its timescale is 2147483648, which FFmpeg can't use",
        ),
        (
            &|b| b.mdhd = words(&[0, 0]),
            "mdhd",
            "it is too short to hold its timescale",
        ),
        (
            &|b| {
                b.stts = Some(table(0, &[]));
                b.stsz = Some(stsz(0));
                b.ctts = None;
                b.stss = None;
            },
            "stts",
            "it times no samples, so the track shows none",
        ),
    ];
    for (bad, fourcc, why) in cases {
        let e = must_fail(why, with(bad));
        assert_eq!(named(e), (fourcc.to_string(), why.to_string()));
    }
}

/// `f` with the `cut` bytes at offset `at` replaced by `bytes`, and each
/// box of `around`, which all start before `at`, hold it and have 32-bit
/// sizes, resized to fit.
fn splice(f: &[u8], at: usize, cut: usize, bytes: &[u8], around: &[&Mp4Box]) -> Vec<u8> {
    let mut out = [&f[..at], bytes, &f[at + cut..]].concat();
    for b in around {
        assert_eq!(b.header_len, 8, "{b:?}");
        let o = b.offset as usize;
        let size = (b.size + bytes.len() as u64)
            .checked_sub(cut as u64)
            .and_then(|s| u32::try_from(s).ok())
            .unwrap();
        out[o..o + 4].copy_from_slice(&size.to_be_bytes());
    }
    out
}

#[test]
fn a_box_the_reader_loses_is_an_error_naming_its_parent() {
    // `stss` last in `stbl`, as on the X5, and two tracks.
    let s = Spec {
        stss: Some(vec![1, 31, 61]),
        elst: Some((0, vec![(3000, 0, RATE_1)])),
        ..Spec::new(15360, &[(90, 512)])
    };
    let f = file(1000, &[s.boxes(), s.boxes()]);
    let whole = timing(&f, 0).unwrap();
    assert_eq!(whole.stss, Some(vec![1, 31, 61]));
    let tree = mp4::read(&mut io::Cursor::new(&f)).unwrap();
    let moov = tree.moov().unwrap();
    let trak = moov.child(b"trak").unwrap();
    let (mdia, edts) = (trak.child(b"mdia").unwrap(), trak.child(b"edts").unwrap());
    let minf = mdia.child(b"minf").unwrap();
    let stbl = minf.child(b"stbl").unwrap();

    // Fewer than 8 bytes after a container's last box are not a box,
    // and FFmpeg passes over them too.
    let end = (stbl.offset + stbl.size) as usize;
    let padded = splice(&f, end, 0, &[0; 7], &[moov, trak, mdia, minf, stbl]);
    assert_eq!(timing(&padded, 0).unwrap(), whole);

    // Controls: each box made a byte longer than its parent holds, which
    // stops the reader there, as its warnings say. A lost `stss` would
    // read as every sample a sync sample.
    let stss = stbl.child(b"stss").unwrap();
    let elst = edts.child(b"elst").unwrap();
    for (b, parent) in [
        (stss, "stbl"),
        (elst, "edts"),
        (stbl, "minf"),
        (minf, "mdia"),
        (mdia, "trak"),
    ] {
        let at = b.offset as usize;
        let mut bad = f.clone();
        bad[at..at + 4].copy_from_slice(&(b.size as u32 + 1).to_be_bytes());
        let tree = mp4::read(&mut io::Cursor::new(&bad)).unwrap();
        let warning = format!("child box `{}` at {at} overruns its parent", b.fourcc);
        assert_eq!(tree.warnings, [warning]);
        let (fourcc, why) = named(must_fail(parent, mp4::track_timing_in(&tree, 0)));
        assert_eq!(fourcc, parent);
        assert!(
            why.starts_with(&format!(
                "its boxes stop at offset {at}, {} bytes before its end",
                b.size
            )),
            "{why}"
        );
        // The other track is whole.
        assert!(mp4::track_timing_in(&tree, 1).is_ok());
    }
    // A track lost from `moov` says why it isn't there.
    let second = moov.children_of(b"trak").nth(1).unwrap();
    let at = second.offset as usize;
    let mut bad = f.clone();
    bad[at..at + 4].copy_from_slice(&(second.size as u32 + 1).to_be_bytes());
    let why = no_track(must_fail("a lost trak", timing(&bad, 1)));
    assert!(
        why.starts_with("the file has 1 tracks, so none is stream 1, but box `moov`"),
        "{why}"
    );
    assert_eq!(timing(&bad, 0).unwrap(), whole);
}

#[test]
fn stts_agree_over_the_common_prefix() {
    let t = |timescale: u32, stts: &[(u32, u32)]| {
        timing(&file(1000, &[Spec::new(timescale, stts).boxes()]), 0).unwrap()
    };
    let base = t(15360, &[(90, 512)]);
    // The same times however the runs are split, with an empty run, and
    // for a track a frame short.
    for agrees in [
        t(15360, &[(30, 512), (60, 512)]),
        t(15360, &[(0, 7), (90, 512)]),
        t(15360, &[(89, 512)]),
        // The last frame's duration is a muxer's guess, and a delta tells
        // when the frame after it is.
        t(15360, &[(89, 512), (1, 300)]),
        // 1/30 s is 512 ticks at 15360 and 1000 at 30000.
        t(30000, &[(90, 1000)]),
    ] {
        assert_eq!(same_times(&base, &agrees), Ok(()), "{agrees:?}");
        assert_eq!(same_times(&agrees, &base), Ok(()), "{agrees:?}");
    }
    // A track a frame short whose last delta is a guess agrees too, but
    // not one whose guess times a frame both have.
    let short = t(15360, &[(59, 512), (1, 100)]);
    assert_eq!(same_times(&t(15360, &[(60, 512)]), &short), Ok(()));
    let longer = t(15360, &[(59, 512), (1, 100), (1, 512)]);
    assert_eq!(
        must_fail(
            "a guess inside the common prefix",
            same_times(&t(15360, &[(61, 512)]), &longer)
        ),
        60
    );
    // Controls: frame 5 lasting longer moves frame 6; 29.97 against 30.
    let late = t(15360, &[(5, 512), (1, 600), (84, 512)]);
    assert_eq!(must_fail("frame 5 longer", same_times(&base, &late)), 6);
    assert_eq!(
        must_fail(
            "29.97 against 30",
            same_times(&base, &t(30000, &[(90, 1001)]))
        ),
        1
    );
}

#[test]
fn streams_are_numbered_as_ffmpeg_numbers_them() {
    let video = Spec::new(15360, &[(10, 512)]).boxes();
    let mvhd_box = bx(b"mvhd", &mvhd(1000));
    // iTunes-style metadata: an encoder tag, and cover art.
    let tag = bx(b"\xa9too", &bx(b"data", b"\0\0\0\x01\0\0\0\0Lavf"));
    let cover = bx(b"covr", &bx(b"data", b"\0\0\0\x0e\0\0\0\0png"));
    let udta = |items: &[Vec<u8>]| {
        let hdlr = bx(b"hdlr", b"\0\0\0\0\0\0\0\0mdirappl\0\0\0\0\0\0\0\0\0");
        bx(
            b"udta",
            &bx(
                b"meta",
                &[words(&[0]), hdlr, bx(b"ilst", &items.concat())].concat(),
            ),
        )
    };
    let ok = |f: &[u8], stream| timing(f, stream).map(|t| t.count);

    // Metadata after the tracks, as muxers write it, numbers nothing.
    let after = file_of(&[
        mvhd_box.clone(),
        trak(1, &video),
        udta(&[tag.clone(), cover.clone()]),
    ]);
    assert_eq!(ok(&after, 0).unwrap(), 10);
    let tagged = file_of(&[
        mvhd_box.clone(),
        udta(std::slice::from_ref(&tag)),
        trak(1, &video),
    ]);
    assert_eq!(ok(&tagged, 0).unwrap(), 10);

    // FFmpeg makes a stream of each cover picture where it reads it, and
    // of an IAMF track's further substreams.
    let before = file_of(&[mvhd_box.clone(), udta(&[tag, cover]), trak(1, &video)]);
    let why = no_track(must_fail("cover art first", ok(&before, 0)));
    assert!(
        why.starts_with("FFmpeg makes streams of the `covr` at offset"),
        "{why}"
    );
    let iamf = Boxes {
        handler: *b"soun",
        entry: *b"iamf",
        ..audio(10)
    };
    let f = file_of(&[mvhd_box.clone(), trak(1, &iamf), trak(2, &video)]);
    let why = no_track(must_fail("an IAMF track first", ok(&f, 1)));
    assert!(why.contains("`iamf`"), "{why}");
    assert!(
        why.ends_with("before the file's track #1, so which stream that track is isn't known"),
        "{why}"
    );
    // And of HEIF items, which a top-level `meta` lists.
    let heif = bx(
        b"meta",
        &[words(&[0]), bx(b"iinf", &words(&[0, 0]))].concat(),
    );
    let mut f = bx(b"ftyp", b"isom\0\0\x02\0isom");
    f.extend(heif);
    f.extend_from_slice(&file(1000, &[video])[20..]);
    let why = no_track(must_fail("HEIF items first", ok(&f, 0)));
    assert!(why.contains("`iinf`"), "{why}");
}

#[test]
fn a_cut_or_missing_moov_is_an_error_naming_it() {
    let f = file(30000, &[x5_lens().boxes(), x5_lens().boxes()]);
    let whole = timing(&f, 1).unwrap();
    let moov = f.windows(4).position(|w| w == b"moov").unwrap() - 4;
    let mdat = f.windows(4).position(|w| w == b"mdat").unwrap() - 4;
    for len in 0..f.len() {
        let cut = &f[..len];
        let Ok(tree) = mp4::read(&mut io::Cursor::new(cut)) else {
            continue;
        };
        match mp4::track_timing_in(&tree, 1) {
            Ok(t) => {
                assert!(len >= mdat, "{len} bytes of {}", f.len());
                assert_eq!(t, whole);
            }
            Err(e) => {
                assert!(len < mdat, "{len} bytes of {}: {e}", f.len());
                let why = no_track(e);
                if len == moov {
                    assert_eq!(why, NO_MOOV);
                } else {
                    // Once its type is in the file, the box cut short is named.
                    let (start, named) = if len < moov {
                        (0, "`ftyp`")
                    } else {
                        (moov, "`moov`")
                    };
                    assert!(why.ends_with("it was cut short"), "{len}: {why}");
                    assert!(len - start < 8 || why.contains(named), "{len}: {why}");
                }
            }
        }
    }
    let no_moov = [bx(b"ftyp", b"isom\0\0\x02\0isom"), bx(b"mdat", &[1; 8])].concat();
    assert_eq!(no_track(timing(&no_moov, 0).unwrap_err()), NO_MOOV);
}

/// Twelve 64x64 frames of H.264 at 30 fps, as FFmpeg's MP4 muxer writes
/// them (time base 1/30, which it stores at 1/15360), with the encoder's
/// `options`, `edit_start` frames hidden by the edit list, and `mux`.
fn written(
    dir: &Path,
    name: &str,
    options: &[(&str, &str)],
    edit_start: u32,
    mux: &MuxOptions,
) -> PathBuf {
    let settings = VideoSettings {
        encoder: "libx264".into(),
        width: 64,
        height: 64,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: options.iter().map(|&(k, v)| (k.into(), v.into())).collect(),
        color: ColorTags::BT709_LIMITED,
        edit_start,
        time_base: None,
        crop_left: 0,
        device: None,
    };
    let path = dir.join(format!("{name}.mp4"));
    let mut w = Mp4Writer::create_with(&path, &settings, None::<AudioTrack<'_>>, mux).unwrap();
    for i in 0..12 + edit_start {
        let chroma = vec![128; 32 * 32];
        w.push_video(&YuvPlanes::U8([
            vec![16 + 8 * i as u8; 64 * 64],
            chroma.clone(),
            chroma,
        ]))
        .unwrap();
    }
    w.finish().unwrap()
}

/// The packets of stream 0 that FFmpeg's demuxer shows, and its time
/// base's denominator.
fn demuxed(path: &Path) -> (i64, Vec<PacketInfo>) {
    let mut r = PacketReader::open(path, 0, Arc::new(AtomicBool::new(false))).unwrap();
    let tb = r.time_base();
    assert_eq!(tb.num(), 1);
    let mut packets = Vec::new();
    loop {
        match r.next_packet() {
            NextPacket::Packet(p) => packets.push(p),
            NextPacket::End { damaged } => {
                assert!(!damaged);
                break;
            }
            NextPacket::Stopped => panic!("stopped"),
        }
    }
    (tb.den(), packets)
}

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn temp_dir(name: &str) -> TempDir {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

#[test]
fn ffmpeg_s_files_time_as_its_demuxer_reads_them() {
    let dir = temp_dir("track-timing-ffmpeg");
    let plain = MuxOptions::default();
    let negative = MuxOptions {
        negative_cts_offsets: true,
        ..MuxOptions::default()
    };
    // (name, encoder options, frames hidden, muxer options)
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], u32, &'a MuxOptions);
    let cases: [Case<'_>; 5] = [
        ("gop6", &[("g", "6"), ("bf", "0")], 0, &plain),
        ("b_frames", &[("g", "12"), ("bf", "2")], 0, &plain),
        (
            "b_frames_negative",
            &[("g", "12"), ("bf", "2")],
            0,
            &negative,
        ),
        ("lead_in", &[("g", "6"), ("bf", "0")], 2, &plain),
        ("intra", &[("g", "1"), ("bf", "0")], 0, &plain),
    ];
    for (name, options, hidden, mux) in cases {
        let path = written(&dir.0, name, options, hidden, mux);
        let t = mp4::track_timing(&path, 0).unwrap();
        let (den, packets) = demuxed(&path);
        // FFmpeg's time base is the track's timescale, and it shows every
        // sample the edit list doesn't hide.
        assert_eq!(i64::from(t.timescale), den, "{name}");
        assert_eq!(t.count, packets.len() as u64 + u64::from(hidden), "{name}");
        // Its first frame is shown at the first pts.
        let first = packets.iter().filter_map(|p| p.pts).min().unwrap();
        assert_eq!(t.first_pts, first, "{name}");
        // Its keyframes are the sync samples, and its decode times are
        // `stts`'s.
        let shown = (u64::from(hidden) + 1..).zip(&packets);
        let keys: Vec<u64> = shown
            .clone()
            .filter(|(_, p)| p.key)
            .map(|(k, _)| k)
            .collect();
        let sync: Vec<u64> = match &t.stss {
            Some(s) => s
                .iter()
                .map(|&k| u64::from(k))
                .filter(|&k| k > u64::from(hidden))
                .collect(),
            None => (u64::from(hidden) + 1..=t.count).collect(),
        };
        assert_eq!(keys, sync, "{name}");
        let deltas: Vec<i64> = packets
            .windows(2)
            .map(|w| w[1].dts.unwrap() - w[0].dts.unwrap())
            .collect();
        let stts: Vec<i64> = t
            .stts
            .iter()
            .flat_map(|&(n, d)| std::iter::repeat_n(i64::from(d), n as usize))
            .skip(hidden as usize)
            .take(deltas.len())
            .collect();
        assert_eq!(deltas, stts, "{name}");
        eprintln!("{name}: {t:?}");
    }
    // What FFmpeg's muxer wrote, as the cases mean it to.
    let at = |name: &str| mp4::track_timing(&dir.0.join(format!("{name}.mp4")), 0).unwrap();
    let gop6 = at("gop6");
    assert_eq!(
        (gop6.stts.clone(), gop6.stss.clone(), gop6.ctts_hash),
        (vec![(12, 512)], Some(vec![1, 7]), None)
    );
    assert!(at("b_frames").ctts_hash.is_some());
    assert_eq!(at("intra").stss, None);
    let lead_in = at("lead_in").edit.unwrap();
    assert_eq!(lead_in.edits[0].media_time, 1024);

    // Control: the box reader's first frame without the edit list, which
    // FFmpeg's demuxer does apply.
    let b = at("b_frames");
    let shift = b.edit.as_ref().unwrap().edits[0].media_time;
    assert!(shift > 0, "{b:?}");
    let ignored = TrackTiming {
        first_pts: b.first_pts + shift,
        ..b.clone()
    };
    let (_, packets) = demuxed(&dir.0.join("b_frames.mp4"));
    let first = packets.iter().filter_map(|p| p.pts).min().unwrap();
    must_fail("the edit list ignored", ignored.first_pts == first);
}

#[test]
fn edits_put_in_by_hand_time_as_ffmpeg_s_demuxer_reads_them() {
    let dir = temp_dir("track-timing-edits");
    // A movie at 30720, whose tick is half the track's.
    let mux = MuxOptions {
        movie_timescale: Some(30720),
        ..MuxOptions::default()
    };
    let plain = written(&dir.0, "plain", &[("g", "6"), ("bf", "0")], 0, &mux);
    let f = std::fs::read(&plain).unwrap();
    // The file with its track's edit list replaced by `edits`, or one put
    // in after `tkhd`. FFmpeg's muxer writes `moov` last, so no chunk
    // moves.
    let put = |name: &str, edits: &[EditSpec]| {
        let tree = mp4::read(&mut io::Cursor::new(&f)).unwrap();
        assert_eq!(tree.top.last().unwrap().fourcc.0, *b"moov");
        let moov = tree.moov().unwrap();
        let trak = moov.child(b"trak").unwrap();
        let (at, cut) = match trak.child(b"edts") {
            Some(e) => (e.offset, e.size),
            None => {
                let tkhd = trak.child(b"tkhd").unwrap();
                (tkhd.offset + tkhd.size, 0)
            }
        };
        let edts = bx(b"edts", &bx(b"elst", &elst(0, edits)));
        let path = dir.0.join(format!("{name}.mp4"));
        let bytes = splice(&f, at as usize, cut as usize, &edts, &[moov, trak]);
        std::fs::write(&path, bytes).unwrap();
        let first = |p: &[PacketInfo]| p.iter().filter_map(|p| p.pts).min().unwrap();
        let (den, packets) = demuxed(&path);
        let t = mp4::track_timing(&path, 0).unwrap();
        assert_eq!((den, t.timescale), (15360, 15360), "{name}");
        (t, first(&packets))
    };
    // Two empty edits of half a track tick each, then the twelve frames,
    // 0.4 s: FFmpeg rounds each edit to a tick and adds them up.
    let (t, first) = put(
        "halves",
        &[(1, -1, RATE_1), (1, -1, RATE_1), (12288, 0, RATE_1)],
    );
    assert_eq!((t.first_pts, first), (2, 2));
    // Control: the two added before rounding make one tick.
    must_fail("the empty edits rounded once", first == 1);
    // An edit that starts 300 ticks into the first frame: FFmpeg
    // discards that frame and shows the next at the empty edits' time,
    // 0, where the tables give the next frame's offset from the edit's
    // start.
    let (t, first) = put("between", &[(12288, 300, RATE_1)]);
    assert_eq!((t.first_pts, first), (512 - 300, 0));
}

#[test]
fn a_moov_cut_short_is_an_error_naming_it() {
    // FFmpeg's muxer writes `moov` last, as cameras do.
    let dir = temp_dir("track-timing-cut");
    let path = written(&dir.0, "whole", &[("g", "6")], 0, &MuxOptions::default());
    let bytes = std::fs::read(&path).unwrap();
    let cut = dir.0.join("cut.mp4");
    std::fs::write(&cut, &bytes[..bytes.len() - 10]).unwrap();
    let why = no_track(must_fail("a cut moov", mp4::track_timing(&cut, 0)));
    assert!(
        why.contains("`moov` box") && why.ends_with("it was cut short"),
        "{why}"
    );
    assert!(mp4::track_timing(&path, 0).is_ok());
}

/// Decode times plus composition offsets, sample by sample.
fn presentation_times(s: &Spec) -> Vec<i128> {
    let offsets: Vec<i128> = match &s.ctts {
        None => vec![0; s.count() as usize],
        Some((_, runs)) => runs
            .iter()
            .flat_map(|&(n, o)| std::iter::repeat_n(i128::from(o), n as usize))
            .collect(),
    };
    let deltas = s
        .stts
        .iter()
        .flat_map(|&(n, d)| std::iter::repeat_n(i128::from(d), n as usize));
    deltas
        .zip(offsets)
        .scan(0, |dts, (d, o)| {
            let t = *dts + o;
            *dts += d;
            Some(t)
        })
        .collect()
}

/// When the first frame of `s` is shown, sample by sample, in a movie
/// at `movie`: `None` when its edits show none.
fn first_pts_by_sample(s: &Spec, movie: u32) -> Option<i128> {
    let (from, delay) = match &s.elst {
        Some((_, edits)) if !edits.is_empty() => {
            let shown = edits.iter().position(|e| e.1 != -1)?;
            let (ts, movie) = (i128::from(s.timescale), i128::from(movie));
            // Each to the nearest tick, halves away from zero, then added.
            let empty: i128 = edits[..shown]
                .iter()
                .map(|e| (2 * i128::from(e.0) * ts + movie) / (2 * movie))
                .sum();
            (Some(i128::from(edits[shown].1)), empty)
        }
        _ => (None, 0),
    };
    let first = presentation_times(s)
        .into_iter()
        .filter(|&t| from.is_none_or(|m| t >= m))
        .min()?;
    Some(delay + first - from.unwrap_or(0))
}

/// [`TrackTiming::stts_mismatch`], sample by sample.
fn stts_mismatch_by_sample(a: &TrackTiming, b: &TrackTiming) -> Option<u64> {
    let n = a.count.min(b.count) as usize;
    let dts = |t: &TrackTiming| -> Vec<u128> {
        t.stts
            .iter()
            .flat_map(|&(k, d)| std::iter::repeat_n(u128::from(d), k as usize))
            .scan(0, |at, d| {
                let t = *at;
                *at += d;
                Some(t)
            })
            .take(n)
            .collect()
    };
    let (da, db) = (dts(a), dts(b));
    let (ta, tb) = (u128::from(a.timescale), u128::from(b.timescale));
    (0..n).find(|&i| da[i] * tb != db[i] * ta).map(|i| i as u64)
}

/// [`TrackTiming::stss_mismatch`], sample by sample.
fn stss_mismatch_by_sample(a: &TrackTiming, b: &TrackTiming) -> Option<u64> {
    let sync = |t: &TrackTiming, k: u64| t.stss.as_ref().is_none_or(|s| s.contains(&(k as u32)));
    (1..=a.count.min(b.count)).find(|&k| sync(a, k) != sync(b, k))
}

/// Runs of equal values, as (count, value).
fn runs<T: PartialEq + Copy>(values: &[T]) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for &v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

/// A video track with any tables that agree with each other, and the
/// movie's timescale. Edits may show none of the media, or start after
/// every sample.
fn spec() -> impl Strategy<Value = (Spec, u32)> {
    let stts = vec((0u32..6, prop_oneof![Just(512u32), 0u32..2000]), 1..6)
        .prop_filter("a sample", |r| r.iter().any(|&(n, _)| n > 0));
    (1u32..200_000, 1u32..100_000, stts)
        .prop_flat_map(|(timescale, movie, stts)| {
            let count = stts.iter().map(|r| r.0).sum::<u32>() as usize;
            let span = stts
                .iter()
                .map(|&(n, d)| i64::from(n) * i64::from(d))
                .sum::<i64>();
            let offsets = option::of((
                0u8..2,
                vec(prop_oneof![Just(0i32), Just(512), -3000i32..3000], count),
            ));
            let sync = option::of(vec(any::<bool>(), count));
            // Empty edits often, so that lists start with two or more.
            let media_time = prop_oneof![Just(-1i64), -1i64..span + 3000];
            let edit = (0u64..5000, media_time, 0u32..0x2_0000);
            let edits = option::of((0u8..2, vec(edit, 0..4)));
            (
                Just(timescale),
                Just(movie),
                Just(stts),
                offsets,
                sync,
                edits,
            )
        })
        .prop_map(|(timescale, movie, stts, offsets, sync, edits)| {
            let spec = Spec {
                ctts: offsets.map(|(v, o)| (v, runs(&o))),
                stss: sync.map(|s| (1..).zip(s).filter(|(_, k)| *k).map(|(n, _)| n).collect()),
                elst: edits,
                ..Spec::new(timescale, &stts)
            };
            (spec, movie)
        })
}

/// A table word: usually small, sometimes at the limits of its 32 bits.
fn field() -> impl Strategy<Value = u32> {
    prop_oneof![
        6 => 0u32..8,
        1 => Just(u32::MAX),
        1 => Just(1 << 31),
        1 => Just(RATE_1),
        1 => any::<u32>(),
    ]
}

/// Any payload: bytes, or words, which put counts and sample numbers at
/// their limits more often.
fn payload() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        vec(any::<u8>(), 0..48),
        vec(field(), 0..12).prop_map(|w| words(&w))
    ]
}

/// What a track's timing must be, whatever the bytes it was read from.
fn holds(t: &TrackTiming) -> Result<(), TestCaseError> {
    prop_assert!(t.timescale > 0 && t.timescale <= i32::MAX as u32);
    prop_assert_eq!(t.stts.iter().map(|r| u64::from(r.0)).sum::<u64>(), t.count);
    if let Some(s) = &t.stss {
        prop_assert!(s.windows(2).all(|w| w[0] < w[1]), "{:?}", s);
        prop_assert!(s.iter().all(|&k| k >= 1 && u64::from(k) <= t.count));
    }
    if let Some(e) = &t.edit {
        prop_assert!(e.movie_timescale > 0);
    }
    prop_assert_eq!(t.stts_mismatch(t), None);
    prop_assert_eq!(t.stss_mismatch(t), None);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn tables_read_back_as_written((s, movie) in spec()) {
        let f = file(movie, &[s.boxes(), audio(3)]);
        match (timing(&f, 0), first_pts_by_sample(&s, movie)) {
            (Ok(t), Some(first)) => {
                holds(&t)?;
                prop_assert_eq!(i128::from(t.first_pts), first);
                prop_assert_eq!(&t.stts, &s.stts);
                prop_assert_eq!(t.count, u64::from(s.count()));
                prop_assert_eq!(&t.stss, &s.stss);
                let entries = s.ctts.as_ref().map(|(_, r)| {
                    words(&r.iter().flat_map(|&(n, o)| [n, o as u32]).collect::<Vec<_>>())
                });
                prop_assert_eq!(t.ctts_hash, entries.map(|e| fnv64(&e)));
                let edits = s.elst.as_ref().map(|(_, e)| {
                    e.iter().map(|&(duration, media_time, rate)| Edit { duration, media_time, rate }).collect()
                });
                prop_assert_eq!(t.edit.map(|e| e.edits), edits);
            }
            (Err(e), None) => {
                let (fourcc, _) = named(e);
                prop_assert_eq!(fourcc, "elst");
            }
            (t, first) => prop_assert!(false, "read {:?}, by sample {:?}", t, first),
        }
    }

    #[test]
    fn mismatches_are_the_first_samples_that_differ(
        (a, _) in spec(),
        (b, _) in spec(),
        same_rate in any::<bool>(),
    ) {
        // Often the same deltas and sync samples at the same timescale, so
        // that the two agree for a while.
        let b = if same_rate {
            let head = &a.stts[..a.stts.len() / 2];
            let n: u32 = head.iter().map(|r| r.0).sum();
            let sync = |s: &Spec, upto: u32| match &s.stss {
                Some(v) => v.iter().copied().filter(|&k| k <= upto).collect(),
                None => (1..=upto).collect::<Vec<_>>(),
            };
            let tail = sync(&b, b.count()).into_iter().map(|k| k + n);
            let stss = (a.stss.is_some() || b.stss.is_some()).then(|| sync(&a, n).into_iter().chain(tail).collect());
            Spec { timescale: a.timescale, stts: [head, &b.stts].concat(), ctts: None, stss, ..b }
        } else {
            b
        };
        let ta = timing(&file(1000, &[Spec { elst: None, ..a }.boxes()]), 0).unwrap();
        let tb = timing(&file(1000, &[Spec { elst: None, ..b }.boxes()]), 0).unwrap();
        prop_assert_eq!(ta.stts_mismatch(&tb), stts_mismatch_by_sample(&ta, &tb));
        prop_assert_eq!(tb.stts_mismatch(&ta), stts_mismatch_by_sample(&tb, &ta));
        prop_assert_eq!(ta.stss_mismatch(&tb), stss_mismatch_by_sample(&ta, &tb));
    }

    #[test]
    fn any_table_bytes_are_read_or_refused(
        (s, movie) in spec(),
        bad in vec((0usize..10, payload()), 1..4),
        mvhd_bytes in option::of(payload()),
    ) {
        let mut b = s.boxes();
        for (which, p) in bad {
            match which {
                0 => b.mdhd = p,
                1 => b.stts = Some(p),
                2 => b.stsz = Some(p),
                3 => b.ctts = Some(p),
                4 => b.stss = Some(p),
                5 => b.elst = Some(p),
                6 => b.ctts = None,
                7 => b.stss = None,
                8 => b.elst = None,
                _ => b.stts = None,
            }
        }
        let mut children = vec![bx(b"mvhd", &mvhd_bytes.unwrap_or_else(|| mvhd(movie)))];
        children.extend([trak(1, &b), trak(2, &s.boxes())]);
        let f = file_of(&children);
        for stream in 0..3 {
            if let Ok(t) = timing(&f, stream) {
                holds(&t)?;
            }
        }
    }

    #[test]
    fn cut_and_damaged_files_are_read_or_refused(
        (s, movie) in spec(),
        cut in any::<prop::sample::Index>(),
        flips in vec((any::<prop::sample::Index>(), any::<u8>()), 0..6),
    ) {
        let mut f = file(movie, &[s.boxes(), s.boxes()]);
        for (i, x) in &flips {
            let i = i.index(f.len());
            f[i] ^= x;
        }
        let f = &f[..cut.index(f.len() + 1)];
        for stream in 0..2 {
            if let Ok(t) = timing(f, stream) {
                holds(&t)?;
            }
        }
    }

    #[test]
    fn any_bytes_are_read_or_refused(
        bytes in vec(any::<u8>(), 0..300),
        in_moov in any::<bool>(),
    ) {
        let rest = if in_moov { bx(b"moov", &bytes) } else { bytes };
        let f = [bx(b"ftyp", b"isom\0\0\x02\0isom"), rest].concat();
        for stream in 0..2 {
            if let Ok(t) = timing(&f, stream) {
                holds(&t)?;
            }
        }
    }
}
