//! `synth::insv::write_insv`'s files (M3 plan 7.2; M3.5's unit (a)): a
//! master and its LRV as the X5 writes them, the planes a lossless file
//! decodes to, each `InsvFault` planted where it belongs, and Pat's
//! strings printed unchanged (the project review's F141, amendment 7).
//!
//! The files are read back with instruments of their own: the top-level
//! boxes and the strings in record 1 by hand, the tracks' tables through
//! `media::mp4::track_timing` and FFmpeg's demuxer, the pictures through
//! FFmpeg's software decoder and the corner codes. Each file is 2 × 128²
//! but where the picture is what is checked.
//!
//! Budget (M3 plan 4.1 rule 5): 2 s for the suite, which each test
//! prints its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use spherecut_boundary::pats;
use spherecut_core::video::{ColorCodes, VideoPlanes};
use spherecut_core::{FrameRate, Rational, StopToken};
use spherecut_media::backend::{
    FfmpegBackend, MediaBackend, NextFrame, VideoFrame, VideoRequest, VideoStreamInfo,
};
use spherecut_media::encode::EncodeError;
use spherecut_media::mp4::{self, TrackTiming};
use spherecut_media::probe::{self, StreamKind};
use spherecut_synth::Pattern;
use spherecut_synth::code::{CornerCode, Luma, Samples, read_corner_code};
use spherecut_synth::insv::{
    InsvFault, InsvSpec, LrvSpec, Quality, Records, TrailerLayout, X5_ALIGN, inst_box, print,
    write_insv,
};
use spherecut_synth::lens::{CalibTruth, Generation, compose, x5_like, x5_like_planted};
use spherecut_synth::media::{SynthError, pairs_64, write_pair};
use spherecut_synth::yuv::YuvPlanes;
use spherecut_testkit::Fnv64;
use spherecut_testkit::control::must_fail;
use spherecut_testkit::pats as testkit_pats;

/// The suite's budget in seconds.
const BUDGET_S: f64 = 2.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the suite's budget. The tests run
/// side by side, so none of them can time the rest: the last line
/// printed is the suite's time.
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
            "{}: {:.3} s; the suite {suite:.3} s so far, of its {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn temp_dir(name: &str) -> TempDir {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("insv-file-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// A master's name, as the X5 names take `take`.
fn master_name(take: u32) -> String {
    format!("VID_20250101_120000_00_{take:03}.insv")
}

/// An X5-like spec of `cal` at 2 × `side`², `frames` frames a track at
/// `rate`: composed under `x5_like_planted` at front track 0, seeing
/// `SeamLines`, lossless at ss 1, with `Records::x5_like` of `cal`, the
/// X5's GOP, no fault and no LRV.
fn truth_spec(take: u32, cal: &CalibTruth, side: u32, frames: u32, rate: FrameRate) -> InsvSpec {
    let planted = x5_like_planted();
    InsvSpec {
        name: master_name(take),
        rig: compose(cal, &planted, 0, [side; 2]),
        pattern: Pattern::SeamLines,
        frames,
        rate,
        quality: Quality::Lossless,
        calib: planted,
        records: Records::x5_like(std::slice::from_ref(cal), &planted, frames, rate),
        fault: None,
        lrv: None,
        keyint: None,
        ss: 1,
    }
}

/// [`truth_spec`] of `x5_like`.
fn spec(take: u32, side: u32, frames: u32, rate: FrameRate) -> InsvSpec {
    truth_spec(take, &x5_like(), side, frames, rate)
}

/// 119.88 frames a second, the X5's 4K120.
fn fps_119_88() -> FrameRate {
    FrameRate::new(Rational::new(120_000, 1001).unwrap()).unwrap()
}

/// The little-endian u32 at byte `at` of `b`.
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// The top-level boxes of a file's bytes, each its type, offset and
/// size, read by ISO BMFF's rules: a 32-bit size, or a 64-bit one after
/// the type when it is 1, or to the file's end when it is 0.
fn top_boxes(b: &[u8]) -> Result<Vec<([u8; 4], u64, u64)>, String> {
    let mut boxes = Vec::new();
    let mut at = 0usize;
    while at < b.len() {
        let head = b
            .get(at..at + 8)
            .ok_or(format!("a box header cut at {at}"))?;
        let kind: [u8; 4] = head[4..8].try_into().unwrap();
        let size = match u32::from_be_bytes(head[..4].try_into().unwrap()) {
            0 => (b.len() - at) as u64,
            1 => {
                let large = b.get(at + 8..at + 16).ok_or("a large size cut")?;
                u64::from_be_bytes(large.try_into().unwrap())
            }
            n => u64::from(n),
        };
        if size < 8 || at as u64 + size > b.len() as u64 {
            return Err(format!("the box at {at} has size {size}"));
        }
        boxes.push((kind, at as u64, size));
        at += size as usize;
    }
    Ok(boxes)
}

/// Each track's handler and sample entry types, in file order.
fn track_kinds(path: &Path) -> Vec<([u8; 4], [u8; 4])> {
    let tree = mp4::read_file(path).unwrap();
    tree.tracks()
        .map(|trak| {
            let handler = mp4::handler_type(trak).unwrap().0;
            let stsd = trak.path(&[b"mdia", b"minf", b"stbl", b"stsd"]).unwrap();
            (handler, stsd.children[0].fourcc.0)
        })
        .collect()
}

/// Whether `path` is laid out as the X5's master, or the first way it
/// isn't: `ftyp wide mdat moov inst` at the top, and two `hvc1` video
/// tracks then an `mp4a` sound track.
fn x5_container(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).unwrap();
    let kinds: Vec<[u8; 4]> = top_boxes(&bytes)?.iter().map(|b| b.0).collect();
    if kinds != [*b"ftyp", *b"wide", *b"mdat", *b"moov", *b"inst"] {
        let names: Vec<String> = kinds
            .iter()
            .map(|k| String::from_utf8_lossy(k).into_owned())
            .collect();
        return Err(format!("top-level boxes {names:?}"));
    }
    let tracks = track_kinds(path);
    let want = [
        (*b"vide", *b"hvc1"),
        (*b"vide", *b"hvc1"),
        (*b"soun", *b"mp4a"),
    ];
    if tracks != want {
        return Err(format!("tracks {tracks:?}"));
    }
    Ok(())
}

/// Every frame of video stream `stream` of `path`, decoded in software,
/// each as `f` makes it, and the stream's info.
fn each_frame<T>(
    path: &Path,
    stream: usize,
    mut f: impl FnMut(&VideoFrame) -> T,
) -> (VideoStreamInfo, Vec<T>) {
    let never = StopToken::never();
    let req = VideoRequest {
        stream: Some(stream),
        ..VideoRequest::default()
    };
    let mut dec = FfmpegBackend::default().open_video(path, &req).unwrap();
    let info = dec.info().clone();
    let mut out = Vec::new();
    loop {
        match dec.next_frame(&never).unwrap() {
            NextFrame::Frame(frame) => out.push(f(&frame)),
            NextFrame::End => return (info, out),
            NextFrame::Stopped => panic!("stopped"),
        }
    }
}

/// FNV-1a of a decoded 8-bit frame's three planes, row by row, padding
/// left out.
fn frame_hash(frame: &VideoFrame) -> u64 {
    let mut h = Fnv64::new();
    for i in 0..3 {
        let plane = frame.plane(i).unwrap();
        for y in 0..plane.height {
            h.write(plane.row(y, plane.width as usize).unwrap());
        }
    }
    h.finish()
}

/// FNV-1a of synth's 8-bit planes, which hold their rows without
/// padding.
fn planes_hash(planes: &YuvPlanes) -> u64 {
    let YuvPlanes::U8(p) = planes else {
        panic!("not 8-bit");
    };
    let mut h = Fnv64::new();
    for plane in p {
        h.write(plane);
    }
    h.finish()
}

/// The X5's colour (3.1): full range, BT.709 throughout, chroma sited
/// left.
const X5_COLOR: ColorCodes = ColorCodes {
    primaries: 1,
    transfer: 1,
    matrix: 1,
    range: 2,
    chroma_location: 1,
};

/// The master is the X5's container (3.1, 7.2): two square `hvc1`
/// tracks coded as the X5 codes them (full range, BT.709, chroma left,
/// an IDR every `keyint` frames, no B-frames; the X5's own GOP is
/// [`write_insv_codes_the_x5s_gop`]'s), one silent AAC track,
/// `ftyp wide mdat moov`, and the `inst` box holding synth's trailer
/// where `InsvFiles::layout` says, whose serial probe reads. Both
/// tracks' tables are the same, read by the box reader and by the
/// backend the frame lock asks, and their edits are exact in the
/// movie's timescale. The control is `write_pair`'s file, which has no
/// `wide`, no audio and no trailer, and FFmpeg's movie timescale.
#[test]
fn write_insv_writes_the_x5s_container() {
    let _t = timed("write_insv_writes_the_x5s_container");
    let dir = temp_dir("container");
    let s = InsvSpec {
        keyint: Some(3),
        ..spec(101, 128, 8, FrameRate::FPS_29_97)
    };
    let files = write_insv(&s, &dir.0).unwrap();
    assert_eq!(files.master, dir.0.join(&s.name));
    assert_eq!(files.lrv, None);
    let left: Vec<String> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        std::slice::from_ref(&s.name),
        "only the master is left"
    );

    x5_container(&files.master).unwrap();
    let bytes = std::fs::read(&files.master).unwrap();
    let boxes = top_boxes(&bytes).unwrap();
    let (_, wide_at, wide_size) = boxes[1];
    assert_eq!((wide_at, wide_size), (boxes[0].2, 8));
    let (_, inst_at, inst_size) = boxes[4];
    let layout = &files.layout;
    assert_eq!(inst_at + 8, layout.payload_start);
    assert_eq!(inst_size, layout.size + 8);
    assert_eq!(inst_at + inst_size, bytes.len() as u64);
    assert_eq!(u64::from(le32(&bytes, bytes.len() - 40)), layout.size);

    // Both tracks' tables, by the box reader and by the backend.
    let backend = FfmpegBackend::default();
    let mut timings = Vec::new();
    for k in 0..2 {
        let t = mp4::track_timing(&files.master, k).unwrap();
        let asked = backend.track_timing(&files.master, k).unwrap();
        assert_eq!(asked.as_ref(), Some(&t), "track {k}");
        assert_eq!(t.timescale, 30_000);
        assert_eq!(t.count, 8);
        assert_eq!(t.first_pts, 0);
        assert_eq!(t.stts, [(8, 1001)]);
        assert_eq!(t.ctts_hash, None, "no B-frames, so no ctts");
        assert_eq!(t.stss.as_deref(), Some(&[1, 4, 7][..]), "an IDR every 3");
        let edit = t.edit.as_ref().unwrap();
        assert_eq!(edit.edits.len(), 1);
        assert_eq!(edit.edits[0].media_time, 0);
        // The movie's timescale is the least common multiple of the
        // tracks' (30000 and the audio's 48000) on every FFmpeg, so that
        // the edit is the track's 8 frames exactly.
        assert_eq!(edit.movie_timescale, 240_000);
        assert_eq!(edit.edits[0].duration, 8 * 1001 * 8);
        timings.push(t);

        let (info, keys) = each_frame(&files.master, k, |f| f.key);
        assert_eq!(info.codec, "hevc");
        assert_eq!(info.size, (128, 128));
        assert_eq!(info.color.codes, X5_COLOR, "track {k}");
        assert_eq!(info.frame_rate(), Some(FrameRate::FPS_29_97));
        let idr: Vec<usize> = (0..keys.len()).filter(|&i| keys[i]).collect();
        assert_eq!(idr, [0, 3, 6]);
    }
    assert_eq!(timings[0], timings[1]);

    // The silent AAC track, and what probe makes of the file.
    let report = probe::probe(&files.master).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(report.damage.is_empty(), "{:?}", report.damage);
    let camera = report.camera.as_ref().unwrap();
    assert_eq!(camera.serial.as_deref(), Some("SYNTH00001"));
    let audio: Vec<(Option<&str>, u32, u32)> = report
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .map(|s| {
            let a = s.audio.as_ref().unwrap();
            (s.codec.as_deref(), a.sample_rate, a.channels)
        })
        .collect();
    assert_eq!(audio, [(Some("aac"), 48_000, 2)]);

    // Its control: a pair file, which has no wide box, no audio and no
    // trailer.
    let pair = write_pair(&pairs_64()[0], &dir.0).unwrap();
    let why = must_fail("write_pair's file as the X5's", x5_container(&pair));
    assert!(why.starts_with("top-level boxes"), "{why}");
    // Its movie timescale is FFmpeg's own, 1000 before 9 and its video
    // tracks' least common multiple from 9, not the master's.
    let pair_timing = mp4::track_timing(&pair, 0).unwrap();
    must_fail(
        "write_pair's movie timescale as the master's",
        pair_timing.edit.unwrap().movie_timescale == 240_000,
    );
}

/// Whether `write_insv` refuses `spec` as invalid with exactly
/// `"<name>: <why>"` and leaves `dir` unmade, or what it did instead.
fn refused_as(spec: &InsvSpec, dir: &Path, why: &str) -> Result<(), String> {
    let want = format!("{}: {why}", spec.name);
    match write_insv(spec, dir) {
        Err(SynthError::Encode(EncodeError::Invalid(said))) if said == want => {}
        Err(e) => return Err(format!("refused: {e}")),
        Ok(files) => return Err(format!("wrote {}", files.master.display())),
    }
    match dir.exists() {
        true => Err(format!("made {}", dir.display())),
        false => Ok(()),
    }
}

/// `write_insv` refuses each spec its doc refuses for its frames, IDRs,
/// fault, rate or LRV, as invalid, naming the spec and why, before it
/// makes `dir`: no frames, an IDR every 0 frames, `LensOneFrameShort` or
/// `StssDiffer` on one frame, `RateDiffer` at a rate neither n/1001 nor
/// whole (12.5), an LRV of a master not named as the X5 names it, and an
/// LRV at a rate it takes no whole number of frames of (24 fps, against
/// the LRV's 29.97). The doc's last refusal, a corner code reaching the
/// image circle, is `check_clear`'s, which `code.rs` tests. The controls
/// hold each refusal to the next one's message.
#[test]
fn write_insv_refuses_the_specs_it_documents() {
    let _t = timed("write_insv_refuses_the_specs_it_documents");
    let dir = temp_dir("refusals");
    let base = || spec(110, 128, 2, FrameRate::FPS_29_97);
    let fault = |fault| InsvSpec {
        fault: Some(fault),
        frames: 1,
        ..base()
    };
    let lrv = Some(LrvSpec {
        side: 64,
        quality: Quality::Lossless,
    });
    let cases: Vec<(InsvSpec, &str)> = vec![
        (
            InsvSpec {
                frames: 0,
                ..base()
            },
            "an .insv needs a frame",
        ),
        (
            InsvSpec {
                keyint: Some(0),
                ..base()
            },
            "an IDR every 0 frames",
        ),
        (
            fault(InsvFault::LensOneFrameShort),
            "LensOneFrameShort needs 2 frames",
        ),
        (fault(InsvFault::StssDiffer), "StssDiffer needs 2 frames"),
        (
            InsvSpec {
                fault: Some(InsvFault::RateDiffer),
                ..spec(
                    110,
                    128,
                    2,
                    FrameRate::new(Rational::new(25, 2).unwrap()).unwrap(),
                )
            },
            "RateDiffer needs a rate of n/1001 or a whole one",
        ),
        (
            InsvSpec {
                name: "clip.insv".into(),
                lrv,
                ..base()
            },
            "an LRV needs a master named VID_<date>_<time>_00_<NNN>.insv",
        ),
        (
            InsvSpec {
                lrv,
                ..spec(
                    110,
                    128,
                    2,
                    FrameRate::new(Rational::new(24, 1).unwrap()).unwrap(),
                )
            },
            "its LRV takes no whole number of its frames",
        ),
    ];
    for (i, (s, why)) in cases.iter().enumerate() {
        let at = dir.0.join(format!("case-{i}"));
        assert_eq!(refused_as(s, &at, why), Ok(()), "{why}");
        // Its control: the next refusal's message.
        let other = cases[(i + 1) % cases.len()].1;
        must_fail(&format!("{why} as {other}"), refused_as(s, &at, other));
    }
}

/// Whether track `track` of `path` holds `frames` frames with an IDR
/// every `gop` from its first, by its `stss` and by the decoder's key
/// flags, or the first way it doesn't. The count comes first: a track
/// too short to reach its second IDR would pass for one coded at any
/// longer GOP.
fn idr_every(path: &Path, track: usize, frames: u64, gop: u64) -> Result<(), String> {
    let t = mp4::track_timing(path, track).unwrap();
    if t.count != frames {
        return Err(format!(
            "track {track} holds {} frames, not {frames}",
            t.count
        ));
    }
    let want: Vec<u64> = (1..=frames).step_by(gop as usize).collect();
    let stss: Option<Vec<u64>> = t.stss.map(|s| s.into_iter().map(u64::from).collect());
    if stss.as_ref() != Some(&want) {
        return Err(format!("track {track}'s stss is {stss:?}, not {want:?}"));
    }
    let (_, keys) = each_frame(path, track, |f| f.key);
    let got: Vec<u64> = (1..).zip(&keys).filter(|k| *k.1).map(|k| k.0).collect();
    if got != want {
        return Err(format!("track {track}'s key frames are {got:?}"));
    }
    Ok(())
}

/// With no `keyint`, the tracks are coded at the X5's GOP (3.1), shown
/// in a file long enough to hold each track's second IDR: 61 frames at
/// 29.97, the master's tracks an IDR every 60 frames (2 s, frames 1 and
/// 61) and the LRV's every 30 (1 s, frames 1, 31 and 61), by `stss` and
/// by the decoder's key flags, each track holding all 61 frames. The
/// controls hold each track to the other's GOP (the LRV, coded at
/// another GOP, to the master's, and the master's tracks to the LRV's),
/// and the master to a frame fewer than it holds.
#[test]
fn write_insv_codes_the_x5s_gop() {
    let _t = timed("write_insv_codes_the_x5s_gop");
    let dir = temp_dir("gop");
    let frames = 61;
    let s = InsvSpec {
        lrv: Some(LrvSpec {
            side: 64,
            quality: Quality::Lossless,
        }),
        ..spec(102, 128, frames, FrameRate::FPS_29_97)
    };
    assert_eq!(s.keyint, None);
    let files = write_insv(&s, &dir.0).unwrap();
    let lrv = files.lrv.unwrap();
    let n = u64::from(frames);
    for k in 0..2 {
        assert_eq!(idr_every(&files.master, k, n, 60), Ok(()));
        must_fail(
            &format!("the master's track {k} at the LRV's GOP"),
            idr_every(&files.master, k, n, 30),
        );
    }
    let why = must_fail(
        "the master held to a frame fewer",
        idr_every(&files.master, 0, n - 1, 60),
    );
    assert_eq!(why, "track 0 holds 61 frames, not 60");
    assert_eq!(idr_every(&lrv, 0, n, 30), Ok(()));
    let why = must_fail("the LRV at the master's GOP", idr_every(&lrv, 0, n, 60));
    assert!(why.contains("[1, 31, 61]"), "{why}");
}

/// A `Quality::Lossless` file's decoded planes hash equal to
/// `InsvSpec::planes`, track by track and frame by frame, corner codes
/// and all (4.2's rung 3), at `InsvSpec::small`'s 2 × 384². The controls:
/// a `CameraLike` file's planes don't, and each frame's don't hash as the
/// next frame's planes.
#[test]
fn write_insv_lossless_planes_hash_equal() {
    let _t = timed("write_insv_lossless_planes_hash_equal");
    let dir = temp_dir("lossless");
    let small = InsvSpec::small();
    assert_eq!(small.rig.lenses[0].side, 384);
    let frames = 3;
    let s = InsvSpec {
        frames,
        records: Records::x5_like(&[x5_like()], &small.calib, frames, small.rate),
        ..small
    };
    let files = write_insv(&s, &dir.0).unwrap();
    let mut wanted = Vec::new();
    for k in 0..2 {
        let (_, got) = each_frame(&files.master, k, frame_hash);
        let want: Vec<u64> = (0..frames)
            .map(|i| planes_hash(&s.planes(k, i).unwrap()))
            .collect();
        assert_eq!(got, want, "track {k}");
        // Control: each frame against the next frame's planes, which
        // differ by their corner codes.
        let next: Vec<u64> = (1..=frames)
            .map(|i| planes_hash(&s.planes(k, i).unwrap()))
            .collect();
        must_fail(
            &format!("track {k}'s frames held to the next ones' planes"),
            got == next,
        );
        wanted.push(want);
    }

    // Control: the same at a camera's loss.
    let lossy = InsvSpec {
        name: master_name(103),
        frames: 1,
        records: Records::x5_like(&[x5_like()], &s.calib, 1, s.rate),
        quality: Quality::CameraLike { crf: 28 },
        ..s.clone()
    };
    let lossy = write_insv(&lossy, &dir.0).unwrap();
    let (_, got) = each_frame(&lossy.master, 0, frame_hash);
    must_fail(
        "a CameraLike file's planes hashed as the lossless planes",
        got[..] == wanted[0][..1],
    );
}

/// The parts of two tracks' tables the frame lock compares, track 0's
/// then track 1's: timescale, first pts, the first `stts` and `stss`
/// mismatch, and count, each `None` where the two agree.
#[derive(Debug, Default, PartialEq, Eq)]
struct Differ {
    timescale: Option<(u32, u32)>,
    first_pts: Option<(i64, i64)>,
    stts: Option<u64>,
    stss: Option<u64>,
    count: Option<(u64, u64)>,
}

fn differ(a: &TrackTiming, b: &TrackTiming) -> Differ {
    fn pair<T: PartialEq>(x: T, y: T) -> Option<(T, T)> {
        (x != y).then_some((x, y))
    }
    Differ {
        timescale: pair(a.timescale, b.timescale),
        first_pts: pair(a.first_pts, b.first_pts),
        stts: a.stts_mismatch(b),
        stss: a.stss_mismatch(b),
        count: pair(a.count, b.count),
    }
}

/// Each edit's duration in seconds, as a fraction (ticks, movie
/// timescale) in lowest terms, and its media time: what holds whatever
/// timescale the muxer gives the movie (amendment 7's FFmpeg 9 note).
fn edits_in_seconds(t: &TrackTiming) -> Vec<((u64, u64), i64)> {
    let list = t.edit.as_ref().unwrap();
    let movie = u64::from(list.movie_timescale);
    list.edits
        .iter()
        .map(|e| {
            let (mut a, mut b) = (e.duration, movie);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            ((e.duration / a, movie / a), e.media_time)
        })
        .collect()
}

/// Whether `t` starts exactly `ticks` of its timescale late: its edit
/// list's first edit an empty one of that time in seconds, whatever the
/// movie's timescale, and its first pts `ticks`.
fn starts_late_by(t: &TrackTiming, ticks: u32) -> Result<(), String> {
    let list = t.edit.as_ref().ok_or("no edit list")?;
    let Some(empty) = list.edits.first().filter(|e| e.media_time == -1) else {
        return Err(format!("no empty edit first: {list:?}"));
    };
    // d/movie s is ticks/timescale s when d·timescale = ticks·movie.
    if u128::from(empty.duration) * u128::from(t.timescale)
        != u128::from(ticks) * u128::from(list.movie_timescale)
    {
        return Err(format!(
            "the empty edit isn't {ticks}/{} s: {list:?}",
            t.timescale
        ));
    }
    if t.first_pts != i64::from(ticks) {
        return Err(format!("the first pts is {}, not {ticks}", t.first_pts));
    }
    Ok(())
}

/// Each of `InsvFault`'s MP4 faults (`LensOneFrameShort`,
/// `StartPtsDiffer`, `StssDiffer`, `RateDiffer`) is in the tracks as its
/// name says, as `track_timing` reads them: of the timescale, the first
/// pts, the first `stts` and `stss` mismatch and the count, only its own
/// differs between the tracks, and track 0 is the unfaulted file's. The
/// control is the unfaulted file, where none does.
#[test]
fn write_insv_plants_each_mp4_fault() {
    let _t = timed("write_insv_plants_each_mp4_fault");
    let dir = temp_dir("mp4-faults");
    let frames = 10;
    let faults: Vec<InsvFault> = InsvFault::all().filter(|f| !f.in_trailer()).collect();
    assert_eq!(faults.len(), 4);
    let write = |take: u32, fault: Option<InsvFault>| {
        let mut s = spec(take, 128, frames, FrameRate::FPS_29_97);
        s.fault = fault;
        let master = write_insv(&s, &dir.0).unwrap().master;
        let t: Vec<TrackTiming> = (0..2)
            .map(|k| mp4::track_timing(&master, k).unwrap())
            .collect();
        (master, t)
    };
    let rate = |path: &Path, k| each_frame(path, k, |_| ()).0.frame_rate();
    let (clean, unfaulted) = write(110, None);
    // Its control: the unfaulted file's tracks agree on all of them.
    must_fail(
        "the unfaulted file's tracks differ",
        differ(&unfaulted[0], &unfaulted[1]) != Differ::default(),
    );
    assert_eq!(rate(&clean, 0), rate(&clean, 1));

    for (fault, take) in faults.iter().zip(111..) {
        let (master, t) = write(take, Some(*fault));
        let (a, b) = (&t[0], &t[1]);
        let got = differ(a, b);
        assert_eq!(a, &unfaulted[0], "{fault:?}: track 0");
        match fault {
            InsvFault::LensOneFrameShort => {
                let short = Differ {
                    count: Some((10, 9)),
                    ..Differ::default()
                };
                assert_eq!(got, short);
                assert_eq!(
                    (&a.stts[..], &b.stts[..]),
                    (&[(10, 1001)][..], &[(9, 1001)][..])
                );
                assert_ne!(edits_in_seconds(a), edits_in_seconds(b));
            }
            InsvFault::StartPtsDiffer => {
                let first_pts = got.first_pts.filter(|p| p.0 == 0);
                assert!(first_pts.is_some(), "{got:?}");
                assert_eq!(
                    got,
                    Differ {
                        first_pts,
                        ..Differ::default()
                    }
                );
                // Track 1 starts exactly a frame late, 1001 ticks of
                // 1/30000 s: an empty edit of a frame before the same
                // media edit as track 0's.
                starts_late_by(b, 1001).unwrap();
                let (ea, eb) = (edits_in_seconds(a), edits_in_seconds(b));
                assert_eq!(eb.len(), 2, "{eb:?}");
                assert_eq!(eb[1..], ea[..]);
                assert_eq!(a.stts, b.stts);
                // Its control: track 1 as a movie timescale of 1000,
                // FFmpeg's own before 9, has it: each edit rounded down
                // to the millisecond, so that it starts 990 ticks late.
                let mut rounded = b.clone();
                let list = rounded.edit.as_mut().unwrap();
                let movie = u64::from(list.movie_timescale);
                for e in &mut list.edits {
                    e.duration = e.duration * 1000 / movie;
                }
                list.movie_timescale = 1000;
                rounded.first_pts = 990;
                must_fail(
                    "track 1 a frame late in a movie timescale of 1000",
                    starts_late_by(&rounded, 1001),
                );
            }
            InsvFault::StssDiffer => {
                // Track 0 has the X5's one IDR in 10 frames; track 1 a
                // second one on its last frame.
                let stss = Differ {
                    stss: Some(9),
                    ..Differ::default()
                };
                assert_eq!(got, stss);
                assert_eq!(a.stss.as_deref(), Some(&[1][..]));
                assert_eq!(b.stss.as_deref(), Some(&[1, 10][..]));
                assert_eq!(a.edit, b.edit);
                assert_eq!(a.stts, b.stts);
            }
            InsvFault::RateDiffer => {
                let rated = Differ {
                    stts: Some(1),
                    ..Differ::default()
                };
                assert_eq!(got, rated);
                assert_eq!(
                    (&a.stts[..], &b.stts[..]),
                    (&[(10, 1001)][..], &[(10, 1200)][..])
                );
                assert_ne!(edits_in_seconds(a), edits_in_seconds(b));
                assert_eq!(rate(&master, 0), Some(FrameRate::FPS_29_97));
                assert_eq!(rate(&master, 1), Some(FrameRate::FPS_25));
            }
            other => panic!("{other:?} is the trailer's"),
        }
    }
}

/// The bytes of `path` and where its `inst` box starts, by the tail's
/// size E: the box is E + 8 bytes and ends the file.
fn split_at_trailer(path: &Path) -> (Vec<u8>, usize) {
    let bytes = std::fs::read(path).unwrap();
    let e = le32(&bytes, bytes.len() - 40) as usize;
    let at = bytes.len() - e - 8;
    (bytes, at)
}

/// Each trailer fault (`TruncatedTrailer` to `VarintOverflow`, and
/// `NoCalibration`) is planted in the master's trailer: the MP4 before it
/// is the unfaulted file's byte for byte, and the `inst` box is the one
/// `inst_box` lays out for that fault, which M3.2's tests hold a reader
/// to. The control is the unfaulted file's trailer, which is none of the
/// faulted ones.
#[test]
fn write_insv_plants_each_trailer_fault() {
    let _t = timed("write_insv_plants_each_trailer_fault");
    let dir = temp_dir("trailer-faults");
    let faults: Vec<InsvFault> = InsvFault::all().filter(|f| f.in_trailer()).collect();
    assert_eq!(faults.len(), 6);
    let base = spec(120, 128, 2, FrameRate::FPS_29_97);
    let clean = write_insv(&base, &dir.0).unwrap();
    let (unfaulted, at) = split_at_trailer(&clean.master);
    let (inst, layout) = inst_box(&base.records, at as u64, X5_ALIGN, None);
    assert_eq!(unfaulted[at..], inst[..]);
    assert_eq!(layout, clean.layout);
    for (fault, take) in faults.iter().zip(121..) {
        let s = InsvSpec {
            name: master_name(take),
            fault: Some(*fault),
            ..base.clone()
        };
        let files = write_insv(&s, &dir.0).unwrap();
        let bytes = std::fs::read(&files.master).unwrap();
        assert_eq!(bytes[..at], unfaulted[..at], "{fault:?}: the MP4");
        let (planted, layout) = inst_box(&s.records, at as u64, X5_ALIGN, Some(*fault));
        assert_eq!(bytes[at..], planted[..], "{fault:?}: the trailer");
        assert_eq!(layout, files.layout, "{fault:?}");
        // Its control: the unfaulted trailer isn't this one.
        must_fail(
            &format!("the unfaulted trailer as {fault:?}'s"),
            unfaulted[at..] == bytes[at..],
        );
    }
}

/// The payload of record `id` in a file's bytes, by a layout's slot.
fn record(bytes: &[u8], layout: &TrailerLayout, id: u8) -> Vec<u8> {
    let slot = layout.slots.iter().find(|s| s.id == id).unwrap();
    bytes[slot.offset as usize..][..slot.size as usize].to_vec()
}

/// An LRV's bytes, and its trailer as `inst_box` lays out `records` where
/// the file's own tail says it starts, which must be the file's tail byte
/// for byte.
fn lrv_layout(path: &Path, records: &Records) -> (Vec<u8>, TrailerLayout) {
    let (bytes, at) = split_at_trailer(path);
    let (inst, layout) = inst_box(records, at as u64, X5_ALIGN, None);
    assert_eq!(bytes[at..], inst[..], "the LRV's trailer");
    (bytes, layout)
}

/// How many frames an LRV of halves `side`² has, when every one shows
/// master frame `step` × its index, track 1 on the left and track 0 on
/// the right, by each half's corner code; or the first frame that
/// doesn't. With `swapped`, the halves are read the other way round.
fn lrv_halves(path: &Path, side: u32, step: u32, swapped: bool) -> Result<u32, String> {
    let (_, codes) = each_frame(path, 0, |f| {
        let plane = f.plane(0).unwrap();
        let half = |x0: usize| {
            let luma = Luma {
                samples: Samples::U8(&plane.bytes[x0..]),
                width: side,
                height: side,
                stride: plane.stride,
                bits: 8,
            };
            read_corner_code(&luma).ok()
        };
        (half(0), half(side as usize))
    });
    let (left, right) = if swapped { (0, 1) } else { (1, 0) };
    for (j, (l, r)) in (0..).zip(&codes) {
        let at = |lens| Some(CornerCode::of(lens, j * step));
        if *l != at(left) || *r != at(right) {
            return Err(format!("frame {j}: left {l:?}, right {r:?}"));
        }
    }
    Ok(codes.len() as u32)
}

/// The LRV follows the X5's rules (3.1): one H.264 track of 2 · side ×
/// side with track 1 on the left, and an AAC track; 25 frames a second
/// for a 25 fps master and 29.97 otherwise; the master's frame count at
/// 25 and 29.97, half at 59.94 and a quarter, rounded up, at 119.88; named
/// `LRV_…_01_NNN.lrv` beside the master, with `Records::x5_lrv`'s
/// records, records 3 and 4 the master's byte for byte. The control reads
/// the halves the other way round.
#[test]
fn write_insv_lrv_follows_the_x5s_rules() {
    let _t = timed("write_insv_lrv_follows_the_x5s_rules");
    let dir = temp_dir("lrv");
    let frames = 6;
    let rows = [
        (FrameRate::FPS_25, FrameRate::FPS_25, 1, 6),
        (FrameRate::FPS_29_97, FrameRate::FPS_29_97, 1, 6),
        (FrameRate::FPS_59_94, FrameRate::FPS_29_97, 2, 3),
        (fps_119_88(), FrameRate::FPS_29_97, 4, 2),
    ];
    for ((rate, lrv_rate, step, lrv_frames), take) in rows.into_iter().zip(130..) {
        let mut s = spec(take, 128, frames, rate);
        s.lrv = Some(LrvSpec {
            side: 64,
            quality: Quality::Lossless,
        });
        let files = write_insv(&s, &dir.0).unwrap();
        let lrv = files.lrv.clone().unwrap();
        let name = format!("LRV_20250101_120000_01_{take:03}.lrv");
        assert_eq!(lrv, dir.0.join(name));
        assert_eq!(
            track_kinds(&lrv),
            [(*b"vide", *b"avc1"), (*b"soun", *b"mp4a")]
        );
        let (info, _) = each_frame(&lrv, 0, |_| ());
        assert_eq!(info.codec, "h264");
        assert_eq!(info.size, (128, 64));
        assert_eq!(info.color.codes, X5_COLOR);
        assert_eq!(info.frame_rate(), Some(lrv_rate), "{rate:?}");
        assert_eq!(
            lrv_halves(&lrv, 64, step, false),
            Ok(lrv_frames),
            "{rate:?}"
        );
        // Its control: the halves read the other way round.
        must_fail("the LRV's halves swapped", lrv_halves(&lrv, 64, step, true));

        let records = Records::x5_lrv(&s.records, lrv_frames);
        let (lrv_bytes, layout) = lrv_layout(&lrv, &records);
        let master = std::fs::read(&files.master).unwrap();
        let exposure = record(&master, &files.layout, 4);
        assert!(!exposure.is_empty());
        assert_eq!(record(&lrv_bytes, &layout, 4), exposure, "{rate:?}");
        assert_eq!(
            record(&lrv_bytes, &layout, 3),
            record(&master, &files.layout, 3)
        );
    }
}

/// The fields of a protobuf message numbered `number` and of wire type 2,
/// in order: a reader of record 1 of this test's own.
fn strings(message: &[u8], number: u32) -> Vec<Vec<u8>> {
    fn varint(message: &[u8], at: &mut usize) -> u64 {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = message[*at];
            *at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return v;
            }
            shift += 7;
        }
    }
    let mut at = 0;
    let mut out = Vec::new();
    while at < message.len() {
        let key = varint(message, &mut at);
        match key & 7 {
            0 => {
                varint(message, &mut at);
            }
            1 => at += 8,
            5 => at += 4,
            2 => {
                let n = varint(message, &mut at) as usize;
                if key >> 3 == u64::from(number) {
                    out.push(message[at..at + n].to_vec());
                }
                at += n;
            }
            w => panic!("wire type {w}"),
        }
    }
    out
}

/// The working string and the original of generation `g` in record 1 of
/// a file's bytes.
fn calibration_strings(bytes: &[u8], layout: &TrailerLayout, g: Generation) -> Vec<Vec<u8>> {
    let metadata = record(bytes, layout, 1);
    let fields = match g {
        Generation::V3 => [54, 56],
        Generation::V6 => [111, 112],
    };
    fields.iter().flat_map(|&n| strings(&metadata, n)).collect()
}

/// The strings in a written file's metadata are `print`'s of the stored
/// calibration, byte for byte whatever the tracks' size, in the master
/// and in its LRV: `x5_like`'s, and Pat's truths
/// (`spherecut_boundary::pats`), which give `testkit::pats`'s strings
/// (F141). The control prints the truth with its blocks scaled to the
/// tracks, as `compose` scales them at 768², which gives other strings.
#[test]
fn write_insv_prints_pats_strings_unchanged() {
    let _t = timed("write_insv_prints_pats_strings_unchanged");
    let dir = temp_dir("strings");
    let planted = x5_like_planted();
    let truths = [
        (x5_like(), None),
        (pats(Generation::V3), Some(testkit_pats::V3)),
        (pats(Generation::V6), Some(testkit_pats::V6)),
    ];
    for ((cal, pat), take) in truths.into_iter().zip(140..) {
        let g = cal.generation();
        let printed = print(&cal, &planted);
        if let Some(pat) = pat {
            assert_eq!(printed, pat, "{g:?}");
        }
        let mut s = truth_spec(take, &cal, 128, 1, FrameRate::FPS_29_97);
        s.lrv = Some(LrvSpec {
            side: 64,
            quality: Quality::Lossless,
        });
        let files = write_insv(&s, &dir.0).unwrap();
        let master = std::fs::read(&files.master).unwrap();
        let both = vec![printed.as_bytes().to_vec(); 2];
        assert_eq!(
            calibration_strings(&master, &files.layout, g),
            both,
            "{g:?}"
        );
        let lrv = files.lrv.unwrap();
        let (lrv_bytes, layout) = lrv_layout(&lrv, &Records::x5_lrv(&s.records, 1));
        assert_eq!(calibration_strings(&lrv_bytes, &layout, g), both, "{g:?}");

        // Its control: the blocks scaled to a 768² track, as 7.2 had it
        // before amendment 7, print other strings.
        let mut scaled = cal;
        let k = 768.0 / f64::from(cal.canvas[1]);
        for b in &mut scaled.blocks {
            (b.fx, b.fy, b.cx, b.cy) = (b.fx * k, b.fy * k, b.cx * k, b.cy * k);
        }
        must_fail(
            &format!("{g:?}'s blocks scaled to the track"),
            print(&scaled, &planted).as_bytes() == both[0],
        );
    }
}
