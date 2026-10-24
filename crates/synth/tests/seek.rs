//! The seek oracle (M1 plan section 12.1, DECISIONS D29): on every clip of
//! the set, a seek to any time returns exactly the frame a sequential
//! decode from the start shows then, by pts and by a hash of its planes,
//! and the frames after it follow on.
//!
//! Each test writes its clips into its own directory under
//! `CARGO_TARGET_TMPDIR`, removed when it ends, so each clip is made once
//! per run.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use spherecut_core::video::{ColorFamily, Matrix, Range, VideoPlanes};
use spherecut_core::{FrameRate, Rational, StopSource, StopToken, Time};
use spherecut_media::backend::{
    DecoderEvent, Exactness, FfmpegBackend, MediaBackend, NextFrame, SeekCost, SeekMode,
    SeekOutcome, Timing, VideoDecode, VideoFrame, VideoRequest,
};
use spherecut_media::encode::ColorTags;
use spherecut_media::ff;
use spherecut_media::seek::{self, FfmpegVideo, FrameAddress};
use spherecut_synth::media::{self, CLIP_COLOR, Codec, Gop, Spec};
use spherecut_synth::yuv::Chroma;

/// A test's own directory, removed with what it holds when dropped, so
/// also when the test fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("seek-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

const OPEN_GOP: Option<Gop> = Some(Gop {
    keyint: 8,
    bframes: 3,
    open: true,
});

/// 256x128 clips of 30 frames at 30 fps, in open GOPs of 8 frames with 3
/// B-frames, unless said otherwise.
fn spec(name: &str, codec: Codec) -> Spec {
    Spec {
        name: name.into(),
        codec,
        gop: OPEN_GOP,
        frames: 30,
        ..Spec::default()
    }
}

fn write(spec: &Spec, dir: &Path) -> PathBuf {
    media::write(spec, dir).unwrap()
}

/// FNV-1a over every row of every plane, without padding.
fn hash(f: &VideoFrame) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let layout = f.layout().clone();
    for (i, desc) in layout.planes.iter().enumerate() {
        let p = f.plane(i).unwrap();
        let row = desc.row_bytes(p.width);
        for y in 0..p.height {
            for &b in p.row(y, row).unwrap() {
                h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    h
}

/// A frame as the oracle knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seen {
    pts: i64,
    hash: u64,
}

impl Seen {
    fn of(f: &VideoFrame) -> Self {
        Seen {
            pts: f.pts,
            hash: hash(f),
        }
    }
}

fn open(path: &Path) -> Box<dyn VideoDecode> {
    FfmpegBackend
        .open_video(path, &VideoRequest::default())
        .unwrap()
}

/// Every frame of a sequential decode from the start, and the pts at
/// which the last one's interval ends: after its duration, or after the
/// average frame duration when it has none.
fn sequential(path: &Path) -> (Vec<Seen>, i64) {
    let mut d = open(path);
    let never = StopToken::never();
    let mut out = Vec::new();
    let mut last_end = 0;
    let tb = d.info().time_base;
    let ticks = |t: Time| t.to_pts(tb, spherecut_core::Rounding::Nearest).unwrap();
    let average = match d.info().timing {
        Timing::Constant(r) | Timing::Variable { average: Some(r) } => ticks(r.frame_duration()),
        Timing::Variable { average: None } => 1,
    };
    while let NextFrame::Frame(f) = d.next_frame(&never).unwrap() {
        out.push(Seen::of(&f));
        last_end = f.pts + f.duration.map_or(average, ticks);
    }
    assert!(!out.is_empty(), "{}", path.display());
    for w in out.windows(2) {
        assert!(w[0].pts < w[1].pts, "{}: pts {:?}", path.display(), w);
    }
    (out, last_end)
}

/// A small deterministic generator for the random targets.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

fn at(d: &dyn VideoDecode, pts: i64) -> Time {
    let info = d.info();
    Time::from_pts(pts - info.origin_pts, info.time_base).unwrap()
}

/// What a seek that wasn't stopped returned.
type Sought = (Arc<VideoFrame>, Exactness, SeekCost);

fn seek_pts(d: &mut dyn VideoDecode, pts: i64, mode: SeekMode) -> Sought {
    let t = at(d, pts);
    seek_to(d, t, mode)
}

fn seek_to(d: &mut dyn VideoDecode, t: Time, mode: SeekMode) -> Sought {
    match d.seek(t, mode, &StopToken::never()).unwrap() {
        SeekOutcome::Frame {
            frame,
            exactness,
            cost,
        } => (frame, exactness, cost),
        SeekOutcome::Stopped => panic!("stopped without a stop"),
    }
}

/// Check every seek the oracle covers on the clip at `path`: a random
/// time inside each frame's interval, and its first, middle and last
/// tick, in order and shuffled; the 3 frames after each, which follow on
/// in the seek's run; before the start and past the end; keyframe seeks;
/// step back; frames by index.
fn oracle(path: &Path) -> Vec<Seen> {
    let (frames, last_end) = sequential(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let mut d = open(path);
    let origin = d.info().origin_pts;
    assert_eq!(
        frames[0].pts, origin,
        "{name}: the origin is the first frame"
    );
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ name.len() as u64);
    let mut targets = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let end = frames.get(i + 1).map_or(last_end, |n| n.pts);
        assert!(end > f.pts, "{name}: frame {i} has an empty interval");
        let span = (end - f.pts) as u64;
        for t in [
            f.pts,
            f.pts + (end - f.pts) / 2,
            end - 1,
            f.pts + rng.below(span) as i64,
        ] {
            targets.push((t, i));
        }
    }
    let check = |d: &mut dyn VideoDecode, t: i64, i: usize, why: &str| {
        let (frame, exactness, _) = seek_pts(d, t, SeekMode::Exact);
        assert_eq!(
            Seen::of(&frame),
            frames[i],
            "{name}: {why} seek to pts {t} should give frame {i}"
        );
        assert_eq!(exactness, Exactness::Exact, "{name}: pts {t}");
        let never = StopToken::never();
        let mut last = frame;
        for k in 1..=3 {
            match (d.next_frame(&never).unwrap(), frames.get(i + k)) {
                (NextFrame::Frame(f), Some(want)) => {
                    assert_eq!(
                        Seen::of(&f),
                        *want,
                        "{name}: frame {} after a seek to pts {t}",
                        i + k
                    );
                    assert_eq!((f.run, f.seq), (last.run, last.seq + 1), "{name}: pts {t}");
                    last = f;
                }
                (NextFrame::End, None) => break,
                (got, want) => {
                    panic!("{name}: after a seek to pts {t}: {got:?}, expected {want:?}")
                }
            }
        }
    };
    for &(t, i) in &targets {
        check(&mut *d, t, i, "in order:");
    }
    // Shuffled, so that seeks go back and forth.
    let mut shuffled = targets.clone();
    for k in (1..shuffled.len()).rev() {
        shuffled.swap(k, rng.below(k as u64 + 1) as usize);
    }
    for &(t, i) in &shuffled {
        check(&mut *d, t, i, "shuffled:");
    }

    // Before the start, and past the end.
    let (f, e, _) = seek_to(&mut *d, Time::from_secs(-1), SeekMode::Exact);
    assert_eq!(
        (Seen::of(&f), e),
        (frames[0], Exactness::BeforeFirst),
        "{name}"
    );
    let last = *frames.last().unwrap();
    for t in [at(&*d, last_end), at(&*d, last_end + 1_000_000)] {
        let (f, e, _) = seek_to(&mut *d, t, SeekMode::Exact);
        assert_eq!(
            (Seen::of(&f), e),
            (last, Exactness::AfterLast),
            "{name}: {t:?}"
        );
        assert!(matches!(
            d.next_frame(&StopToken::never()).unwrap(),
            NextFrame::End
        ));
    }

    // A keyframe seek gives the keyframe one seek lands on (or the first
    // frame, which an edit list can make a B-frame), which is after the
    // target when that is one of its leading pictures. An exact seek
    // after one decodes forward to a target at or after it.
    let mut forward = 0;
    for (i, f) in frames.iter().enumerate() {
        let (k, e, _) = seek_pts(&mut *d, f.pts, SeekMode::Keyframe);
        assert_eq!(e, Exactness::KeyframeOnly, "{name}");
        assert!(
            k.key || k.pts == origin,
            "{name}: frame {} is not a keyframe",
            k.pts
        );
        let (x, e, cost) = seek_pts(&mut *d, f.pts, SeekMode::Exact);
        assert_eq!((Seen::of(&x), e), (frames[i], Exactness::Exact), "{name}");
        if k.pts <= f.pts {
            assert!(
                cost.forward_only && cost.seeks == 0,
                "{name}: frame {i}: {cost:?}"
            );
            forward += 1;
        }
    }
    assert!(forward > frames.len() / 2, "{name}: {forward} forward");

    // Step back: the greatest pts below the current one.
    for i in (1..frames.len()).rev().step_by(5) {
        let (cur, _, _) = seek_pts(&mut *d, frames[i].pts, SeekMode::Exact);
        let (prev, e, _) = seek_pts(&mut *d, cur.pts - 1, SeekMode::Exact);
        assert_eq!(
            (Seen::of(&prev), e),
            (frames[i - 1], Exactness::Exact),
            "{name}"
        );
    }

    // Frames addressed by index: (n + ½) frame durations at a constant
    // rate, counted at a variable one.
    for n in [0, 1, frames.len() / 2, frames.len() - 1] {
        let (f, e) = seek::seek_frame(&mut *d, n as u64, &StopToken::never())
            .unwrap()
            .unwrap();
        assert_eq!(
            (Seen::of(&f), e),
            (frames[n], Exactness::Exact),
            "{name}: frame {n}"
        );
    }
    frames
}

/// How many frames of `path` a naive BACKWARD seek to each frame's pts
/// misses.
fn naive_misses(path: &Path, frames: &[Seen]) -> usize {
    let mut d = FfmpegVideo::open(path, &VideoRequest::default()).unwrap();
    let origin = d.info().origin_pts;
    let tb = d.info().time_base;
    frames
        .iter()
        .filter(|f| {
            let t = Time::from_pts(f.pts - origin, tb).unwrap();
            d.naive_seek_for_tests(t).unwrap() != Some(f.pts)
        })
        .count()
}

#[test]
fn open_gop_hevc_seeks_exactly_and_a_naive_seek_does_not() {
    let dir = TempDir::new("ogop-hevc");
    let clip = write(&spec("ogop-hevc", Codec::Hevc), &dir);
    let frames = oracle(&clip);
    let misses = naive_misses(&clip, &frames);
    assert!(
        misses > 0,
        "the naive seek found every frame of open-GOP HEVC"
    );
}

#[test]
fn open_gop_h264_seeks_exactly_and_a_naive_seek_does_not() {
    let dir = TempDir::new("ogop-h264");
    let clip = write(&spec("ogop-h264", Codec::H264), &dir);
    let frames = oracle(&clip);
    let misses = naive_misses(&clip, &frames);
    assert!(
        misses > 0,
        "the naive seek found every frame of open-GOP H.264"
    );
    let d = open(&clip);
    assert_eq!(d.info().timing, Timing::Constant(FrameRate::FPS_30));
    assert_eq!(d.info().frame_count, Some(30));
}

#[test]
fn an_edit_list_hides_its_lead_in_and_clip_time_starts_after_it() {
    let dir = TempDir::new("edit");
    let clip = write(
        &Spec {
            edit_start: 3,
            frames: 27,
            ..spec("edit", Codec::H264)
        },
        &dir,
    );
    let frames = oracle(&clip);
    // The three frames before the edit are decoded but never shown.
    assert_eq!(frames.len(), 24);
    let d = open(&clip);
    assert_eq!(d.info().frame_count, Some(24));
    assert_eq!(d.info().origin_pts, 0);
}

#[test]
fn a_variable_frame_rate_seeks_exactly_and_counts_frames() {
    let dir = TempDir::new("vfr");
    let clip = write(
        &Spec {
            vfr: true,
            ..spec("vfr", Codec::H264)
        },
        &dir,
    );
    let frames = oracle(&clip);
    let d = open(&clip);
    assert!(
        matches!(d.info().timing, Timing::Variable { .. }),
        "{:?}",
        d.info().timing
    );
    assert_eq!(d.info().time_base, Rational::new(1, 90_000).unwrap());
    let mut pts = 0;
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(f.pts, pts, "frame {i}");
        pts += media::vfr_ticks(i as u32);
    }
    assert_eq!(
        seek::frame_address(d.info(), 7).unwrap(),
        FrameAddress::Counted(7)
    );
}

#[test]
fn ntsc_rates_and_their_matroska_remux_seek_exactly() {
    let dir = TempDir::new("ntsc");
    let ntsc = write(
        &Spec {
            rate: FrameRate::FPS_29_97,
            ..spec("h264-2997", Codec::H264)
        },
        &dir,
    );
    oracle(&ntsc);
    let mp4 = write(
        &Spec {
            rate: FrameRate::FPS_59_94,
            ..spec("h264-5994", Codec::H264)
        },
        &dir,
    );
    let in_mp4 = oracle(&mp4);
    assert_eq!(
        open(&mp4).info().timing,
        Timing::Constant(FrameRate::FPS_59_94)
    );
    // Matroska rounds every timestamp to the millisecond, so frame n is
    // found at (n + ½) frame durations, not at n of them.
    let mkv = media::remux_to_mkv(&mp4, &dir).unwrap();
    let in_mkv = oracle(&mkv);
    assert_eq!(in_mkv.len(), in_mp4.len());
    for (a, b) in in_mp4.iter().zip(&in_mkv) {
        assert_eq!(a.hash, b.hash);
    }
    let d = open(&mkv);
    assert_eq!(d.info().time_base, Rational::new(1, 1000).unwrap());
    assert_eq!(d.info().timing, Timing::Constant(FrameRate::FPS_59_94));
    assert_eq!(d.info().frame_count, None);
    // The frame starts that (n + ½) avoids: n frame durations rounds below
    // some of the millisecond timestamps.
    let rate = FrameRate::FPS_59_94;
    let misses = (0..in_mkv.len())
        .filter(|&n| {
            let t = rate.frame_to_time(n as i64).unwrap();
            let pts = t
                .to_pts(d.info().time_base, spherecut_core::Rounding::Floor)
                .unwrap();
            in_mkv.iter().rev().find(|f| f.pts <= pts).map(|f| f.pts) != Some(in_mkv[n].pts)
        })
        .count();
    assert!(
        misses > 0,
        "n frame durations finds every frame of the remux"
    );
}

/// One small clip each of the other formats and tags in the set, and
/// what their frames say.
#[test]
fn every_format_and_tag_seeks_exactly() {
    let dir = TempDir::new("formats");
    let full = ColorTags {
        range: 2,
        ..CLIP_COLOR
    };
    let bt601 = ColorTags {
        matrix: 6,
        primaries: 6,
        transfer: 6,
        ..CLIP_COLOR
    };
    let small = |name: &str, codec| Spec {
        frames: 12,
        ..spec(name, codec)
    };
    let cases = [
        (
            Spec {
                color: full,
                ..small("h264-full", Codec::H264)
            },
            // FFmpeg's H.264 decoder says full range so.
            "yuvj420p",
        ),
        (
            Spec {
                color: bt601,
                ..small("h264-bt601", Codec::H264)
            },
            "yuv420p",
        ),
        (
            Spec {
                bits: 10,
                gop: None,
                ..small("hevc10-lossless", Codec::HevcLossless)
            },
            "yuv420p10le",
        ),
        (
            Spec {
                bits: 10,
                chroma: Chroma::Yuv422,
                ..small("h264-422-10", Codec::H264)
            },
            "yuv422p10le",
        ),
        (
            Spec {
                chroma: Chroma::Yuv444,
                ..small("hevc-444-8", Codec::Hevc)
            },
            "yuv444p",
        ),
        (
            Spec {
                bits: 10,
                chroma: Chroma::Yuv444,
                ..small("hevc-444-10", Codec::Hevc)
            },
            "yuv444p10le",
        ),
    ];
    for (s, pix_fmt) in &cases {
        let clip = write(s, &dir);
        oracle(&clip);
        let d = open(&clip);
        let info = d.info();
        assert_eq!(info.source_pix_fmt, *pix_fmt, "{}", s.name);
        // Native: uploaded as decoded.
        assert_eq!(info.layout.name, *pix_fmt, "{}", s.name);
        assert_eq!(info.layout.family, ColorFamily::Yuv);
        assert_eq!(info.layout.bit_depth, if s.bits > 8 { 10 } else { 8 });
        let c = info.color;
        assert_eq!(c.codes.range, s.color.range, "{}", s.name);
        assert_eq!(c.codes.matrix, s.color.matrix, "{}", s.name);
        assert_eq!(
            c.range,
            if s.color.range == 2 {
                Range::Full
            } else {
                Range::Limited
            }
        );
        assert_eq!(
            c.matrix,
            if s.color.matrix == 6 {
                Matrix::Bt601
            } else {
                Matrix::Bt709
            }
        );
        assert!(
            !c.assumed.range && !c.assumed.matrix && !c.assumed.transfer,
            "{}: {c:?}",
            s.name
        );
    }
}

#[test]
fn a_truncated_clip_seeks_before_its_cut_and_says_where_it_ends() {
    let dir = TempDir::new("truncated");
    let good = write(&spec("whole", Codec::H264), &dir);
    // With `moov` first, the index lists samples past the cut.
    let fast = dir.join("faststart.mp4");
    faststart(&good, &fast);
    let bytes = std::fs::read(&fast).unwrap();
    let cut = dir.join("cut.mp4");
    std::fs::write(&cut, &bytes[..bytes.len() * 2 / 3]).unwrap();
    let mut d = open(&cut);
    // What a sequential decode gets before the read fails.
    let mut before = Vec::new();
    let never = StopToken::never();
    loop {
        match d.next_frame(&never) {
            Ok(NextFrame::Frame(f)) => before.push(Seen::of(&f)),
            Ok(NextFrame::End) => panic!("the cut file decoded to its end"),
            Ok(NextFrame::Stopped) => unreachable!(),
            Err(_) => break,
        }
    }
    assert!(before.len() > 8 && before.len() < 30, "{}", before.len());
    let (whole, _) = sequential(&good);
    let last_good = *before.last().unwrap();
    // A seek back into the good part works again.
    for (i, f) in before.iter().enumerate().step_by(4) {
        let (got, e, _) = seek_pts(&mut *d, f.pts, SeekMode::Exact);
        assert_eq!((Seen::of(&got), e), (before[i], Exactness::Exact));
    }
    // Past the cut: the last good frame, flagged, and a Damaged event.
    let target = whole.last().unwrap().pts;
    let (got, e, _) = seek_pts(&mut *d, target, SeekMode::Exact);
    assert_eq!(Seen::of(&got), last_good);
    assert_eq!(
        e,
        Exactness::Truncated {
            last_good_pts: last_good.pts
        }
    );
    assert!(
        d.take_events()
            .iter()
            .any(|e| matches!(e, DecoderEvent::Damaged { .. }))
    );
    // And back before the cut again.
    let (got, e, _) = seek_pts(&mut *d, before[2].pts, SeekMode::Exact);
    assert_eq!((Seen::of(&got), e), (before[2], Exactness::Exact));

    // A cut in the middle of frame 13's packet, a B-frame that comes after
    // keyframe 16 in decode order: later frames (16 at least) still come
    // out as the decoder drains, but 13 is lost. A seek to a lost frame
    // is never Exact, even though a later frame came out.
    for codec in [Codec::H264, Codec::Hevc] {
        let whole_clip = match codec {
            Codec::H264 => fast.clone(),
            _ => {
                let fast = dir.join("faststart-hevc.mp4");
                faststart(&write(&spec("whole-hevc", codec), &dir), &fast);
                fast
            }
        };
        let (whole, _) = sequential(&whole_clip);
        let bytes = std::fs::read(&whole_clip).unwrap();
        let at = packet_at(&whole_clip, whole[13].pts);
        let cut = dir.join("cut-13.mp4");
        std::fs::write(&cut, &bytes[..at]).unwrap();
        let mut d = open(&cut);
        let mut decoded = Vec::new();
        while let Ok(NextFrame::Frame(f)) = d.next_frame(&never) {
            decoded.push(Seen::of(&f));
        }
        assert!(!decoded.contains(&whole[13]), "{codec:?}: {decoded:?}");
        assert!(decoded.contains(&whole[16]), "{codec:?}: {decoded:?}");
        for (i, want) in whole.iter().enumerate() {
            let (got, e, _) = seek_pts(&mut *d, want.pts, SeekMode::Exact);
            let events = d.take_events();
            if decoded.contains(want) {
                assert_eq!(
                    (Seen::of(&got), e),
                    (*want, Exactness::Exact),
                    "{codec:?}: frame {i}"
                );
                continue;
            }
            let shown = *decoded.iter().rev().find(|s| s.pts < want.pts).unwrap();
            assert_eq!(
                (Seen::of(&got), e),
                (
                    shown,
                    Exactness::Truncated {
                        last_good_pts: shown.pts
                    }
                ),
                "{codec:?}: frame {i}, lost to the cut"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, DecoderEvent::Damaged { .. })),
                "{codec:?}: frame {i}: {events:?}"
            );
        }
    }
}

/// Where the video packet with pts `pts` lies in the file at `path`: the
/// byte in the middle of it.
// Reads a file this test made with ffmpeg-next alone, as `faststart` does.
#[allow(clippy::disallowed_methods)]
fn packet_at(path: &Path, pts: i64) -> usize {
    let mut ictx = ff::format::input(path).unwrap();
    let pkt = ictx
        .packets()
        .map(|(_, p)| p)
        .find(|p| p.pts() == Some(pts))
        .unwrap();
    usize::try_from(pkt.position()).unwrap() + pkt.size() / 2
}

/// Remux `src` into `dst` with `moov` before `mdat`.
// Writes a file this test made with ffmpeg-next alone, as decode_errors.rs
// does, apart from Spherecut's confined open (D24).
#[allow(clippy::disallowed_methods)]
fn faststart(src: &Path, dst: &Path) {
    let mut ictx = ff::format::input(src).unwrap();
    let mut octx = ff::format::output(dst).unwrap();
    for st in ictx.streams() {
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    let mut opts = ff::Dictionary::new();
    opts.set("movflags", "faststart");
    octx.write_header_with(opts).unwrap();
    for (st, mut pkt) in ictx.packets() {
        pkt.rescale_ts(st.time_base(), octx.stream(st.index()).unwrap().time_base());
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
}

#[test]
fn a_stop_token_stops_a_long_seek_and_decoding_goes_on() {
    let dir = TempDir::new("long-gop");
    // One GOP: the last frame is reached only by decoding every frame.
    // Many small frames make a long decode that is quick to write.
    let clip = write(
        &Spec {
            width: 64,
            height: 32,
            frames: 2400,
            gop: Some(Gop {
                keyint: 10_000,
                bframes: 3,
                open: false,
            }),
            ..spec("long-gop", Codec::H264)
        },
        &dir,
    );
    let mut d = open(&clip);
    let last = FrameRate::FPS_30.frame_to_time(2399).unwrap();
    // How long the whole seek takes, from the start.
    let started = Instant::now();
    let (end, _, cost) = seek_to(&mut *d, last, SeekMode::Exact);
    let full = started.elapsed();
    assert!(cost.frames_decoded >= 2399, "{cost:?}");
    assert!(
        full > Duration::from_millis(40),
        "the long seek took only {full:?}"
    );
    let (first, _, _) = seek_to(&mut *d, Time::ZERO, SeekMode::Exact);

    // Stopped by a newer generation a little way in.
    let source = StopSource::new();
    let token = source.next();
    let handle = std::thread::spawn(move || {
        let started = Instant::now();
        let r = d.seek(last, SeekMode::Exact, &token).unwrap();
        (d, r, started.elapsed())
    });
    std::thread::sleep(full / 8);
    source.next();
    let (mut d, outcome, took) = handle.join().unwrap();
    assert!(matches!(outcome, SeekOutcome::Stopped), "{outcome:?}");
    assert!(
        took < full,
        "the stopped seek took {took:?}, a whole one {full:?}"
    );
    // The next frame follows the last one handed out, and seeks work.
    let never = StopToken::never();
    let NextFrame::Frame(second) = d.next_frame(&never).unwrap() else {
        panic!("no frame after the stop");
    };
    assert!(second.pts > first.pts);
    // It follows on in the run of the frame before the stop.
    assert_eq!((second.run, second.seq), (first.run, first.seq + 1));
    let (again, _, _) = seek_to(&mut *d, last, SeekMode::Exact);
    assert_eq!(Seen::of(&again), Seen::of(&end));
    // A token stopped before the seek stops it at once.
    let stopped = source.current();
    source.next();
    assert!(matches!(
        d.seek(Time::ZERO, SeekMode::Exact, &stopped).unwrap(),
        SeekOutcome::Stopped
    ));
    assert!(matches!(
        d.next_frame(&stopped).unwrap(),
        NextFrame::Stopped
    ));
}

#[test]
fn catching_up_never_returns_a_frame_before_the_target() {
    let dir = TempDir::new("catch-up");
    let clip = write(&spec("catch-up", Codec::H264), &dir);
    let (frames, _) = sequential(&clip);
    let mut d = open(&clip);
    let never = StopToken::never();
    seek_to(&mut *d, Time::ZERO, SeekMode::Exact);
    let until = at(&*d, frames[20].pts);
    d.catch_up(Some(until));
    let NextFrame::Frame(f) = d.next_frame(&never).unwrap() else {
        panic!("no frame");
    };
    assert!(
        f.pts >= frames[20].pts,
        "{} before {}",
        f.pts,
        frames[20].pts
    );
    // Then back to normal: every frame after it, in order.
    let i = frames.iter().position(|s| s.pts == f.pts).unwrap();
    for want in &frames[i + 1..] {
        let NextFrame::Frame(f) = d.next_frame(&never).unwrap() else {
            panic!("no frame");
        };
        assert_eq!(Seen::of(&f), *want);
    }
}

#[test]
fn frames_share_their_layout_and_hand_out_planes_without_a_copy() {
    let dir = TempDir::new("planes");
    let clip = write(
        &Spec {
            frames: 4,
            ..spec("planes", Codec::H264)
        },
        &dir,
    );
    let mut d = open(&clip);
    let never = StopToken::never();
    let NextFrame::Frame(a) = d.next_frame(&never).unwrap() else {
        panic!()
    };
    let NextFrame::Frame(b) = d.next_frame(&never).unwrap() else {
        panic!()
    };
    assert!(Arc::ptr_eq(a.layout_arc(), b.layout_arc()));
    for i in 0..3 {
        assert_eq!(a.plane(i).unwrap().bytes.as_ptr(), a.storage_ptr(i));
    }
    assert!(a.plane(3).is_none());
    assert_eq!(a.size(), (256, 128));
    assert_eq!(a.plane(1).unwrap().width, 128);
    assert_eq!(a.duration, Some(FrameRate::FPS_30.frame_duration()));
    assert!(a.key && !b.key);
    assert_eq!((a.run, a.seq, b.seq), (0, 0, 1));
    // The clip's tags, as coded and as resolved.
    let c = a.color();
    assert_eq!(c.codes, CLIP_COLOR.into());
    assert!(!c.assumed.any(), "{c:?}");
    assert_eq!(c.siting, spherecut_core::video::ChromaSiting::Center);
}

/// Write `frames` frames of `w` × `h` as raw `format` into a MOV at
/// `path`, filling frame `i`'s packed plane with `fill(i, bytes, stride)`.
// A format no encoder of Mp4Writer makes, written with ffmpeg-next alone
// as `faststart` is.
#[allow(clippy::disallowed_methods)]
fn raw_mov(
    path: &Path,
    format: ff::format::Pixel,
    (w, h): (u32, u32),
    frames: u32,
    fill: impl Fn(u32, &mut [u8], usize),
) {
    let codec = ff::encoder::find(ff::codec::Id::RAWVIDEO).unwrap();
    let mut octx = ff::format::output_as(path, "mov").unwrap();
    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .unwrap();
    enc.set_width(w);
    enc.set_height(h);
    enc.set_format(format);
    enc.set_time_base(ff::Rational::new(1, 30));
    enc.set_frame_rate(Some(ff::Rational::new(30, 1)));
    let mut enc = enc.open_as(codec).unwrap();
    let mut ost = octx.add_stream(codec).unwrap();
    ost.set_parameters(&enc);
    ost.set_time_base(ff::Rational::new(1, 30));
    octx.write_header().unwrap();
    let tb = octx.stream(0).unwrap().time_base();
    let drain = |enc: &mut ff::encoder::Video, octx: &mut ff::format::context::Output| {
        let mut pkt = ff::Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            pkt.rescale_ts(ff::Rational::new(1, 30), tb);
            pkt.write_interleaved(octx).unwrap();
        }
    };
    for i in 0..frames {
        let mut f = ff::frame::Video::new(format, w, h);
        let stride = f.stride(0);
        fill(i, f.data_mut(0), stride);
        f.set_pts(Some(i64::from(i)));
        enc.send_frame(&f).unwrap();
        drain(&mut enc, &mut octx);
    }
    enc.send_eof().unwrap();
    drain(&mut enc, &mut octx);
    octx.write_trailer().unwrap();
}

/// Decoders whose frames aren't uploaded as they are: swscale converts
/// them to the native format of the same kind, exactly when it only
/// repacks, says so once, and seeks are as exact as ever.
#[test]
fn formats_swscale_converts_seek_exactly_and_say_so() {
    let dir = TempDir::new("converted");
    let (w, h) = (64u32, 32u32);
    let sample =
        |i: u32, x: usize, y: usize, k: usize| (i as usize * 29 + x * 7 + y * 3 + k * 61) as u8;
    // UYVY: U, Y, V, Y for each pair of pixels.
    let uyvy = dir.join("uyvy.mov");
    raw_mov(
        &uyvy,
        ff::format::Pixel::UYVY422,
        (w, h),
        12,
        |i, data, stride| {
            for y in 0..h as usize {
                for x in 0..w as usize / 2 {
                    let o = y * stride + 4 * x;
                    data[o] = sample(i, x, y, 1);
                    data[o + 1] = sample(i, 2 * x, y, 0);
                    data[o + 2] = sample(i, x, y, 2);
                    data[o + 3] = sample(i, 2 * x + 1, y, 0);
                }
            }
        },
    );
    let rgb = dir.join("rgb.mov");
    raw_mov(
        &rgb,
        ff::format::Pixel::RGB24,
        (w, h),
        12,
        |i, data, stride| {
            for y in 0..h as usize {
                for x in 0..w as usize {
                    for k in 0..3 {
                        data[y * stride + 3 * x + k] = sample(i, x, y, k);
                    }
                }
            }
        },
    );
    // Plane, and the component of `sample` each holds: gbrp's planes are
    // G, B and R.
    for (clip, source, target, planes) in [
        (&uyvy, "uyvy422", "yuv422p", [0, 1, 2]),
        (&rgb, "rgb24", "gbrp", [1, 2, 0]),
    ] {
        oracle(clip);
        let mut d = open(clip);
        assert_eq!(d.info().source_pix_fmt, source);
        assert_eq!(d.info().layout.name, target);
        let events = d.take_events();
        assert_eq!(
            events,
            vec![DecoderEvent::Converted {
                from: source.into(),
                to: target.into()
            }]
        );
        let (f, _, _) = seek_to(
            &mut *d,
            FrameRate::FPS_30.frame_to_time(5).unwrap(),
            SeekMode::Exact,
        );
        assert_eq!(f.layout().name, target);
        for (p, k) in planes.into_iter().enumerate() {
            let plane = f.plane(p).unwrap();
            for y in 0..plane.height as usize {
                let row = plane.row(y as u32, plane.width as usize).unwrap();
                for (x, &got) in row.iter().enumerate() {
                    assert_eq!(got, sample(5, x, y, k), "{source}: plane {p} at ({x}, {y})");
                }
            }
        }
        assert!(d.take_events().is_empty(), "{source}: the event came once");
    }
}
