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

use std::collections::HashMap;
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
    radl: 0,
});

/// What the oracle knows of a clip in GOPs of 8.
const GOP_8: Expect = Expect {
    keyint: Some(8),
    backwards: false,
    spliced: false,
    damaged: false,
    unmarked_keyframes: false,
};

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
    FfmpegBackend::new()
        .open_video(path, &VideoRequest::default())
        .unwrap()
}

/// Every frame of a sequential decode from the start, in output order,
/// and the pts at which the interval of the frame with the greatest pts
/// ends, as the stream's duration says once every frame is decoded.
/// Timestamps may tie or go backwards; [`shown_at`] says which frame a
/// time shows. The clip is a good one: nothing in it is damage.
fn sequential(path: &Path) -> (Vec<Seen>, i64) {
    let (frames, end, events) = sequential_events(path);
    assert_no_damage(&path.display().to_string(), &events);
    (frames, end)
}

/// [`sequential`]'s frames and end, and the events the decode raised.
fn sequential_events(path: &Path) -> (Vec<Seen>, i64, Vec<DecoderEvent>) {
    let mut d = open(path);
    let never = StopToken::never();
    let name = path.display();
    // MP4 and Matroska both say where the stream ends.
    assert!(d.info().duration.is_some(), "{name}: no duration");
    let mut out = Vec::new();
    while let NextFrame::Frame(f) = d.next_frame(&never).unwrap() {
        out.push(Seen::of(&f));
    }
    assert!(!out.is_empty(), "{name}");
    let info = d.info();
    let end = info.origin_pts
        + info
            .duration
            .unwrap()
            .to_pts(info.time_base, spherecut_core::Rounding::Floor)
            .unwrap();
    assert!(
        out.iter().all(|f| f.pts < end),
        "{name}: the stream ends at pts {end}, before a frame"
    );
    (out, end, d.take_events())
}

/// Fails if `events` include a `Damaged` event.
fn assert_no_damage(name: &str, events: &[DecoderEvent]) {
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DecoderEvent::Damaged { .. })),
        "{name}: a good clip is damaged: {events:?}"
    );
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

/// The frame shown at pts `t`, by its place in output order: the one with
/// the greatest pts at or before `t`, the later of frames that tie.
fn shown_at(frames: &[Seen], t: i64) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, f) in frames.iter().enumerate() {
        if f.pts <= t && best.is_none_or(|b| f.pts >= frames[b].pts) {
            best = Some(i);
        }
    }
    best
}

/// The frames handed out so far, by run and seq: two frames of one run
/// whose seq follow on must follow on in the sequential decode, which is
/// what the frame cache links runs by (D29).
#[derive(Default)]
struct Runs(HashMap<(u64, u64), usize>);

impl Runs {
    /// Frame `f`, which is frame `index` of the sequential decode.
    fn note(&mut self, name: &str, f: &VideoFrame, index: usize) {
        let key = (f.run, f.seq);
        if let Some(&j) = self.0.get(&key) {
            assert_eq!(j, index, "{name}: run {key:?} was frame {j}");
        }
        if let Some(&j) = f.seq.checked_sub(1).and_then(|s| self.0.get(&(f.run, s))) {
            assert_eq!(j + 1, index, "{name}: run {key:?} follows frame {j}");
        }
        if let Some(&j) = self.0.get(&(f.run, f.seq + 1)) {
            assert_eq!(index + 1, j, "{name}: run {key:?} comes before frame {j}");
        }
        self.0.insert(key, index);
    }
}

/// What the oracle knows of a clip besides its frames.
#[derive(Debug, Clone, Copy, Default)]
struct Expect {
    /// Frames from one keyframe to the next, when the clip has a fixed GOP:
    /// a seek that doesn't decode forward then decodes at most two GOPs.
    keyint: Option<u32>,
    /// Its timestamps go backwards. It is played through first, which marks
    /// it not exactly seekable, and every exact seek then decodes from the
    /// start.
    backwards: bool,
    /// Its edit list splices pieces of the stream, so a keyframe seek can
    /// give the first frame a piece shows, its keyframe hidden.
    spliced: bool,
    /// FFmpeg says a picture of it is damaged, so decodes and seeks may
    /// raise `Damaged` events, as the HEVC splice does up to FFmpeg 7.0.
    /// Otherwise none may: the oracle's clips are good.
    damaged: bool,
    /// It is an MP4 whose keyframes after the first are pictures FFmpeg
    /// doesn't flag as keyframes (H.264 I-frames that aren't IDR, with no
    /// recovery point SEI), so a keyframe seek gives a sync sample.
    /// FFmpeg says decoding recovers at none of them, so an exact seek
    /// never decodes forward from one after a seek but from the start of
    /// the stream, a keyframe at a time at most: its cost isn't bounded
    /// by two GOPs (D29).
    unmarked_keyframes: bool,
}

/// What [`oracle`] saw.
struct Report {
    frames: Vec<Seen>,
    /// Seeks again to before the keyframe landed on, over every seek.
    reseeks: u32,
}

/// Check every seek the oracle covers on the clip at `path`: a random
/// time inside each frame's interval, and its first, middle and last
/// tick, in order and shuffled; the 3 frames after each, which follow on
/// in the seek's run; what each seek cost; before the start and past the
/// end; keyframe seeks; step back; frames by index.
fn oracle(path: &Path, expect: Expect) -> Report {
    let (frames, last_end, events) = sequential_events(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    if !expect.damaged {
        assert_no_damage(&name, &events);
    }
    let mut d = open(path);
    let origin = d.info().origin_pts;
    assert_eq!(
        frames[0].pts, origin,
        "{name}: the origin is the first frame"
    );
    let never = StopToken::never();
    if expect.backwards {
        while let NextFrame::Frame(_) = d.next_frame(&never).unwrap() {}
        let events = d.take_events();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, DecoderEvent::NotExactlySeekable { .. })),
            "{name}: {events:?}"
        );
    }
    let mut runs = Runs::default();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ name.len() as u64);
    // Each distinct pts shows from there to the next one.
    let mut starts: Vec<i64> = frames.iter().map(|f| f.pts).collect();
    starts.sort_unstable();
    starts.dedup();
    let mut targets = Vec::new();
    for (k, &start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(last_end);
        assert!(end > start, "{name}: pts {start} has an empty interval");
        let span = (end - start) as u64;
        for t in [
            start,
            start + (end - start) / 2,
            end - 1,
            start + rng.below(span) as i64,
        ] {
            targets.push((t, shown_at(&frames, t).unwrap()));
        }
    }
    let mut reseeks = 0;
    let mut check = |d: &mut dyn VideoDecode, runs: &mut Runs, t: i64, i: usize, why: &str| {
        let (frame, exactness, cost) = seek_pts(d, t, SeekMode::Exact);
        assert_eq!(
            Seen::of(&frame),
            frames[i],
            "{name}: {why} seek to pts {t} should give frame {i}"
        );
        assert_eq!(exactness, Exactness::Exact, "{name}: pts {t}");
        runs.note(&name, &frame, i);
        reseeks += cost.reseeks;
        if !cost.forward_only && !expect.backwards {
            // Never back to the start: a seek to before the keyframe
            // landed on always lands earlier.
            assert_eq!(cost.seeks, cost.reseeks + 1, "{name}: pts {t}: {cost:?}");
            if expect.unmarked_keyframes {
                // No more than from the start: the frames up to the
                // target and the one after it, and the first frame of
                // each keyframe landed on before.
                assert!(
                    cost.frames_decoded <= i as u32 + 2 + cost.reseeks,
                    "{name}: pts {t}: {cost:?}"
                );
            } else if let Some(k) = expect.keyint {
                assert!(
                    cost.frames_decoded <= 2 * k + 2,
                    "{name}: pts {t}: {cost:?}"
                );
            }
        }
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
                    runs.note(&name, &f, i + k);
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
        check(&mut *d, &mut runs, t, i, "in order:");
    }
    // Shuffled, so that seeks go back and forth.
    let mut shuffled = targets.clone();
    for k in (1..shuffled.len()).rev() {
        shuffled.swap(k, rng.below(k as u64 + 1) as usize);
    }
    for &(t, i) in &shuffled {
        check(&mut *d, &mut runs, t, i, "shuffled:");
    }

    // Before the start, and past the end.
    let first = shown_at(&frames, origin).unwrap();
    let (f, e, _) = seek_to(&mut *d, Time::from_secs(-1), SeekMode::Exact);
    assert_eq!(
        (Seen::of(&f), e),
        (frames[first], Exactness::BeforeFirst),
        "{name}"
    );
    let last = shown_at(&frames, i64::MAX).unwrap();
    for t in [at(&*d, last_end), at(&*d, last_end + 1_000_000)] {
        let (f, e, _) = seek_to(&mut *d, t, SeekMode::Exact);
        assert_eq!(
            (Seen::of(&f), e),
            (frames[last], Exactness::AfterLast),
            "{name}: {t:?}"
        );
        match (d.next_frame(&never).unwrap(), frames.get(last + 1)) {
            (NextFrame::End, None) => {}
            (NextFrame::Frame(f), Some(want)) => assert_eq!(Seen::of(&f), *want, "{name}"),
            (got, want) => panic!("{name}: after the end: {got:?}, expected {want:?}"),
        }
    }

    // A keyframe seek gives the keyframe one seek lands on (or the first
    // frame, which an edit list can make a B-frame), which is after the
    // target when that is one of its leading pictures. An exact seek
    // after one decodes forward to a target at or after it, unless it is
    // an unmarked keyframe, which only FFmpeg's flags can vouch for. A
    // stream that isn't exactly seekable gives the exact frame either way.
    let mut forward = 0;
    // The first frame of each piece of a splice, after a gap.
    let step = frames
        .windows(2)
        .map(|w| w[1].pts - w[0].pts)
        .filter(|&s| s > 0)
        .min();
    let piece_starts: Vec<i64> = frames
        .windows(2)
        .filter(|w| Some(w[1].pts - w[0].pts) > step)
        .map(|w| w[1].pts)
        .collect();
    let listed = match expect.unmarked_keyframes {
        true => sync_samples(path),
        false => Vec::new(),
    };
    for &(t, i) in targets.iter().step_by(4) {
        let (k, e, _) = seek_pts(&mut *d, t, SeekMode::Keyframe);
        if expect.backwards {
            assert_eq!((Seen::of(&k), e), (frames[i], Exactness::Exact), "{name}");
            continue;
        }
        assert_eq!(e, Exactness::KeyframeOnly, "{name}");
        assert!(
            k.key
                || k.pts == origin
                || (expect.spliced && piece_starts.contains(&k.pts))
                || listed.contains(&k.pts),
            "{name}: frame {} is not a keyframe",
            k.pts
        );
        let (x, e, cost) = seek_pts(&mut *d, t, SeekMode::Exact);
        assert_eq!((Seen::of(&x), e), (frames[i], Exactness::Exact), "{name}");
        runs.note(&name, &x, i);
        if !k.key && k.pts != origin && listed.contains(&k.pts) {
            assert!(!cost.forward_only, "{name}: frame {i}: {cost:?}");
        } else if k.pts <= t {
            assert!(
                cost.forward_only && cost.seeks == 0,
                "{name}: frame {i}: {cost:?}"
            );
            forward += 1;
        }
    }
    assert!(
        expect.backwards || expect.unmarked_keyframes || forward > starts.len() / 2,
        "{name}: {forward} forward"
    );

    // Step back: the greatest pts below the current one.
    for i in (1..frames.len()).rev().step_by(5) {
        let (cur, _, _) = seek_pts(&mut *d, frames[i].pts, SeekMode::Exact);
        let Some(want) = shown_at(&frames, cur.pts - 1) else {
            continue;
        };
        let (prev, e, _) = seek_pts(&mut *d, cur.pts - 1, SeekMode::Exact);
        assert_eq!(
            (Seen::of(&prev), e),
            (frames[want], Exactness::Exact),
            "{name}"
        );
    }

    // Frames addressed by index, every one: (n + ½) frame durations at a
    // constant rate, counted at a variable one.
    for (n, want) in frames.iter().enumerate() {
        let (f, e) = seek::seek_frame(&mut *d, n as u64, &never)
            .unwrap()
            .unwrap();
        assert_eq!(
            (Seen::of(&f), e),
            (*want, Exactness::Exact),
            "{name}: frame {n} ({:?})",
            d.info().timing
        );
    }
    // None of those seeks found damage (a backwards clip's
    // NotExactlySeekable was taken above).
    if !expect.damaged {
        assert_no_damage(&name, &d.take_events());
    }
    Report { frames, reseeks }
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
    let r = oracle(&clip, GOP_8);
    // Its RASL pictures are reached by seeking again.
    assert!(r.reseeks > 0);
    let misses = naive_misses(&clip, &r.frames);
    assert!(
        misses > 0,
        "the naive seek found every frame of open-GOP HEVC"
    );
    // Closed GOPs whose keyframes each have two RADL pictures, decoded
    // after the keyframe from it alone: a keyframe seek still gives the
    // keyframe, and an exact seek to a RADL picture seeks again.
    let radl = write(
        &Spec {
            gop: Some(Gop {
                open: false,
                radl: 2,
                ..OPEN_GOP.unwrap()
            }),
            ..spec("radl-hevc", Codec::Hevc)
        },
        &dir,
    );
    assert!(oracle(&radl, GOP_8).reseeks > 0);
}

#[test]
fn open_gop_h264_seeks_exactly_and_a_naive_seek_does_not() {
    let dir = TempDir::new("ogop-h264");
    let clip = write(&spec("ogop-h264", Codec::H264), &dir);
    let r = oracle(&clip, GOP_8);
    assert!(r.reseeks > 0);
    let misses = naive_misses(&clip, &r.frames);
    assert!(
        misses > 0,
        "the naive seek found every frame of open-GOP H.264"
    );
    let d = open(&clip);
    assert_eq!(d.info().timing, Timing::Constant(FrameRate::FPS_30));
    assert_eq!(d.info().frame_count, Some(30));
    // 25 frames, so the stream ends on keyframe 24 and then its leading
    // B-frames in decode order, as a file cut at a GOP boundary does: a
    // seek that lands on 24 decodes them without their references, and
    // FFmpeg gives them up unflagged as it drains.
    let short = write(
        &Spec {
            frames: 25,
            ..spec("ogop-h264-25", Codec::H264)
        },
        &dir,
    );
    oracle(&short, GOP_8);
    oracle(&media::remux_to_mkv(&short, &dir).unwrap(), GOP_8);
}

/// Copy the H.264 MP4 `src` into the MP4 `dst` without its SEI NAL units
/// (type 6), so that its keyframes after the first, which aren't IDR,
/// have no recovery point: as broadcast streams, TS remuxes and cuts
/// copied from them have them. The copy starts at video sample `from` in
/// decode order (a keyframe), its timestamps moved back by that sample's
/// pts, so a cut from 0 is a whole copy.
fn without_sei(src: &Path, dst: &Path, from: usize) {
    copy_h264(src, dst, from, |t| t != 6, |_, key| key);
}

/// Copy the H.264 MP4 `src` into the MP4 `dst` from video sample `from`
/// in decode order, its timestamps moved back by that sample's pts: of
/// each sample, the NAL units whose type `keep` keeps, and listed as a
/// sync sample (`stss`) when `sync`, given its pts and whether it was
/// one, says so. The first sample copied must be one.
// Writes a file this test made with ffmpeg-next alone, as `faststart`
// does.
#[allow(clippy::disallowed_methods)]
fn copy_h264(
    src: &Path,
    dst: &Path,
    from: usize,
    keep: impl Fn(u8) -> bool,
    sync: impl Fn(i64, bool) -> bool,
) {
    let mut ictx = ff::format::input(src).unwrap();
    let mut octx = ff::format::output(dst).unwrap();
    for st in ictx.streams() {
        assert_eq!(st.parameters().medium(), ff::media::Type::Video);
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    octx.write_header().unwrap();
    let mut shift = None;
    for (k, (st, pkt)) in ictx.packets().enumerate().skip(from) {
        let pts = pkt.pts().unwrap();
        let shift = *shift.get_or_insert(pts);
        // NAL units with 4-byte lengths, as FFmpeg's MP4 muxer writes them.
        let data = pkt.data().unwrap();
        let mut kept = Vec::new();
        let mut at = 0;
        while at < data.len() {
            let len = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            let nal = &data[at..at + 4 + len];
            if keep(nal[4] & 0x1f) {
                kept.extend_from_slice(nal);
            }
            at += 4 + len;
        }
        let key = sync(pts, pkt.is_key());
        assert!(k > from || key, "sample {from} is no keyframe");
        let mut out = ff::Packet::copy(&kept);
        out.set_pts(Some(pts - shift));
        out.set_dts(pkt.dts().map(|d| d - shift));
        out.set_duration(pkt.duration());
        let flags = pkt.flags() - ff::packet::Flags::KEY;
        out.set_flags(match key {
            true => flags | ff::packet::Flags::KEY,
            false => flags,
        });
        out.set_stream(st.index());
        out.rescale_ts(st.time_base(), octx.stream(st.index()).unwrap().time_base());
        out.set_position(-1);
        out.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
}

/// The slice type of the first slice of an H.264 sample, NAL units with
/// 4-byte lengths, as `slice_type % 5`: 0 P, 1 B, 2 I.
fn slice_type(sample: &[u8]) -> u32 {
    let mut at = 0;
    while at + 4 < sample.len() {
        let len = u32::from_be_bytes(sample[at..at + 4].try_into().unwrap()) as usize;
        let nal = &sample[at + 4..at + 4 + len];
        if matches!(nal[0] & 0x1f, 1 | 5) {
            // Two Exp-Golomb codes, first_mb_in_slice and slice_type, in
            // the first bytes of the header, which hold no emulation
            // prevention byte (that needs two zero bytes).
            let mut bits = nal[1..nal.len().min(9)]
                .iter()
                .flat_map(|b| (0..8).rev().map(move |i| u32::from((b >> i) & 1)));
            let mut ue = || {
                let zeros = bits.by_ref().take_while(|&b| b == 0).count();
                let rest = (0..zeros).fold(0, |v, _| (v << 1) | bits.next().unwrap());
                (1 << zeros) - 1 + rest
            };
            let _first_mb = ue();
            return ue() % 5;
        }
        at += 4 + len;
    }
    panic!("no slice in the sample");
}

/// H.264 whose keyframes after the first are I-frames that aren't IDR and
/// carry no recovery point SEI, in open GOPs with B-frames, as broadcast
/// streams, TS remuxes and cuts copied from them have them. FFmpeg's
/// decoder starts at such a keyframe only by a heuristic, and flags
/// corrupt the frames it hasn't seen decoding recover by (D29).
///
/// With one reference frame (synth's `veryfast`), the heuristic takes the
/// keyframe, and from 7.0 the decoder flags the frames it decoded after
/// it and before it came out, though every reference they need is there;
/// 6.1 hands them out unflagged. On 8.1.3 Spherecut took them for damage
/// after a seek, said `Damaged` and dropped them, so an exact seek to one
/// gave the frame before it.
///
/// With three (x264's default `medium` keeps 3), the gap in `frame_num`
/// a seek leaves is filled with three stand-in references, more than the
/// heuristic allows, so the decoder flags every frame after such a
/// keyframe up to the next IDR picture, the keyframe too, and every
/// FFmpeg from 6.1 on: Spherecut dropped them all until the frames FFmpeg
/// drains, unflagged, at the end of the stream. A keyframe seek gave the
/// clip's last frames, exact seeks decoded from the start, and a cut
/// opened on its second last frame. With 25 frames the clip ends on a
/// keyframe and its leading B-frames, which then come out as it drains,
/// unflagged, and must still be dropped.
///
/// Each clip is sought as any good clip, and each cut, which starts at a
/// keyframe that isn't IDR, plays every frame from that keyframe on.
/// FFmpeg's H.264 parser doesn't flag those keyframes' packets as
/// keyframes either (only `stss` lists them), so a remux to Matroska
/// would have none.
///
/// Nothing in the file says that the frames after such a keyframe don't
/// refer to frames before it (`h264_i_frames_listed_as_sync_samples_seek_exactly`
/// has some that do), so no exact seek decodes from one: it decodes from
/// the start of the stream, a keyframe at a time (D29). It decodes on
/// from where decoding is when it began at the start, past such
/// keyframes.
#[test]
fn h264_keyframes_without_recovery_points_seek_exactly() {
    let dir = TempDir::new("no-sei");
    let unmarked = Expect {
        unmarked_keyframes: true,
        ..GOP_8
    };
    let bare = |name: &str, bframes, refs, frames, from_key: usize| {
        let clip = write(
            &Spec {
                gop: Some(Gop {
                    bframes,
                    ..OPEN_GOP.unwrap()
                }),
                refs,
                frames,
                ..spec(name, Codec::H264)
            },
            &dir,
        );
        let pk = samples(&clip);
        let keys: Vec<usize> = (0..pk.len()).filter(|&i| pk[i].3).collect();
        let out = dir.join(format!("{name}-{from_key}.mp4"));
        without_sei(&clip, &out, keys[from_key]);
        (clip, out, pk[keys[from_key]].0)
    };
    for (name, bframes, refs, frames) in [
        ("nosei-h264-b3", 3, None, 30),
        ("nosei-h264-b2", 2, None, 30),
        ("nosei-h264-ref3", 3, Some(3), 30),
        ("nosei-h264-ref3-25", 3, Some(3), 25),
    ] {
        let (clip, whole, _) = bare(name, bframes, refs, frames, 0);
        // The SEI changes no picture.
        let frames = oracle(&whole, unmarked).frames;
        assert_eq!(frames, sequential(&clip).0, "{name}");
        exact_after_keyframe(name, &whole, &frames);
    }
    // A cut's first frame is its keyframe, whose leading B-frames refer to
    // the GOP cut off; every frame after it comes out. No packet of a cut
    // is flagged a keyframe, so `forward_is_cheaper` learns how far
    // keyframes are shown after their decode time from the ones it lands
    // on, the first at open: with 1 B-frame, it took them for shown at
    // their decode time, and an exact seek from the start to the frame
    // before keyframe 8 sought again (D29).
    for (name, bframes, refs) in [
        ("nosei-h264-cut", 3, None),
        ("nosei-h264-b1-cut", 1, None),
        ("nosei-h264-ref3-cut", 3, Some(3)),
    ] {
        let (clip, cut, key_pts) = bare(name, bframes, refs, 30, 1);
        let whole = sequential(&clip).0;
        let from = whole.iter().position(|f| f.pts == key_pts).unwrap();
        let shifted: Vec<Seen> = whole[from..]
            .iter()
            .map(|f| Seen {
                pts: f.pts - key_pts,
                ..*f
            })
            .collect();
        assert_eq!(oracle(&cut, unmarked).frames, shifted, "{name}");
        exact_after_keyframe(name, &cut, &shifted);
        let mut d = open(&cut);
        let (x, e, cost) = seek_pts(&mut *d, shifted[7].pts, SeekMode::Exact);
        assert_eq!((Seen::of(&x), e), (shifted[7], Exactness::Exact), "{name}");
        assert!(cost.forward_only, "{name}: {cost:?}");
    }
}

/// A keyframe seek to frame 12 of the clip at `path`, whose frames are
/// `frames`, gives keyframe 8, an unmarked one, and an exact seek to
/// frame 12 after it doesn't decode on from there: it seeks to before it.
fn exact_after_keyframe(name: &str, path: &Path, frames: &[Seen]) {
    let mut d = open(path);
    assert!(sync_samples(path).contains(&frames[8].pts), "{name}");
    let (k, e, _) = seek_pts(&mut *d, frames[12].pts, SeekMode::Keyframe);
    assert_eq!(
        (Seen::of(&k), e, k.key),
        (frames[8], Exactness::KeyframeOnly, false),
        "{name}"
    );
    let (x, e, cost) = seek_pts(&mut *d, frames[12].pts, SeekMode::Exact);
    assert_eq!((Seen::of(&x), e), (frames[12], Exactness::Exact), "{name}");
    assert!(!cost.forward_only && cost.reseeks > 0, "{name}: {cost:?}");
}

/// H.264 whose sync samples include I-frames that start nothing. x264
/// codes an I-frame forced before `min-keyint` (as at a flash or a scene
/// cut) with no recovery point, and keeping 3 references (its default
/// `medium`), the frames after it may refer to frames before it. Some
/// muxers list every I-frame in `stss` all the same, as the copies here
/// do. A seek lands on such an I-frame, which FFmpeg flags corrupt, as it
/// flags any I-frame it doesn't take for a place to start: decoding from
/// it, the frames after it refer to grey stand-ins for the frames before
/// it. From d4bfe81 Spherecut took the I-frame for the first good frame
/// (as it must for a cut that starts there) and handed out the frames
/// after it, flagged, as `Exact`: 56 of 80 exact seeks were wrong, with
/// no event, on 6.1 and 8.1.3. An exact seek now never ends where
/// decoding began at an I-frame FFmpeg flagged (D29). Here an I-frame is
/// forced at frames 10 and 22 of 40, with 3 B-frames and with 1, and one
/// clip flashes at frame 10, so that the frames after it refer across it
/// for most of their pixels.
#[test]
fn h264_i_frames_listed_as_sync_samples_seek_exactly() {
    let dir = TempDir::new("listed-i");
    let listed = Expect {
        keyint: None,
        unmarked_keyframes: true,
        ..GOP_8
    };
    for (name, bframes, refs, flash) in [
        ("listed-i-b3", 3, 3, vec![]),
        ("listed-i-b1", 1, 3, vec![]),
        ("listed-i-b3-flash", 3, 3, vec![10]),
        ("listed-i-b3-ref2", 3, 2, vec![]),
    ] {
        let clip = write(
            &Spec {
                gop: Some(Gop {
                    keyint: 100,
                    bframes,
                    open: true,
                    radl: 0,
                }),
                refs: Some(refs),
                intra: vec![10, 22],
                flash,
                frames: 40,
                ..spec(name, Codec::H264)
            },
            &dir,
        );
        let whole = sequential(&clip).0;
        let forced = [whole[10].pts, whole[22].pts];
        // x264 coded them as I-frames with no recovery point, and FFmpeg's
        // parser doesn't flag them keyframes.
        let bytes = std::fs::read(&clip).unwrap();
        let pk = samples(&clip);
        for &pts in &forced {
            let &(_, pos, size, key) = pk.iter().find(|s| s.0 == pts).unwrap();
            assert_eq!(slice_type(&bytes[pos..pos + size]), 2, "{name}: pts {pts}");
            assert!(!key, "{name}: pts {pts} has a recovery point");
        }
        let out = dir.join(format!("{name}-listed.mp4"));
        copy_h264(
            &clip,
            &out,
            0,
            |_| true,
            |pts, key| key || forced.contains(&pts),
        );
        assert_eq!(sync_samples(&out), [0, forced[0], forced[1]], "{name}");
        // Frames after each refer to frames before it: decoded from the
        // I-frame on, some come out otherwise.
        for (k, &pts) in forced.iter().enumerate() {
            let cut = dir.join(format!("{name}-cut{k}.mp4"));
            let from = pk.iter().position(|s| s.0 == pts).unwrap();
            copy_h264(&clip, &cut, from, |_| true, |p, _| p == pts);
            let start = whole.iter().position(|f| f.pts == pts).unwrap();
            let shifted: Vec<Seen> = whole[start..]
                .iter()
                .map(|f| Seen {
                    pts: f.pts - pts,
                    ..*f
                })
                .collect();
            assert_ne!(sequential(&cut).0, shifted, "{name}: from pts {pts}");
        }
        assert_eq!(oracle(&out, listed).frames, whole, "{name}");
        // A keyframe seek gives the I-frame, and an exact seek after it
        // doesn't decode on from there.
        let mut d = open(&out);
        let (k, e, _) = seek_pts(&mut *d, whole[15].pts, SeekMode::Keyframe);
        assert_eq!(
            (Seen::of(&k), e),
            (whole[10], Exactness::KeyframeOnly),
            "{name}"
        );
        let (x, e, cost) = seek_pts(&mut *d, whole[15].pts, SeekMode::Exact);
        assert_eq!((Seen::of(&x), e), (whole[15], Exactness::Exact), "{name}");
        assert!(!cost.forward_only && cost.reseeks > 0, "{name}: {cost:?}");
    }
}

/// FFmpeg 7.1 and later's HEVC decoder queues the frames it outputs (all
/// it has left at the end of the stream, several at once where a keyframe
/// empties its picture buffer), and its flush leaves that queue as it is.
/// On one thread, as hardware decoders run (D30), a frame from before a
/// seek then came out after it: after a seek into the last GOP and the
/// frame after it, a keyframe seek there gave the clip's last frame and
/// took it for timestamps going backwards (D2, D29). Here every frame is
/// sought, then the frame after it, then a keyframe there and a frame
/// elsewhere, and each must be the sequential decode's.
#[test]
fn on_one_thread_no_frame_from_before_a_seek_comes_out_after_it() {
    let dir = TempDir::new("one-thread");
    let closed = Some(Gop {
        open: false,
        ..OPEN_GOP.unwrap()
    });
    for (name, codec, gop) in [
        ("one-thread-hevc", Codec::Hevc, OPEN_GOP),
        ("one-thread-closed-hevc", Codec::Hevc, closed),
        ("one-thread-h264", Codec::H264, OPEN_GOP),
    ] {
        let clip = write(
            &Spec {
                gop,
                ..spec(name, codec)
            },
            &dir,
        );
        let (frames, _) = sequential(&clip);
        let req = VideoRequest {
            threads: std::num::NonZeroU32::new(1),
            ..VideoRequest::default()
        };
        let mut d = FfmpegBackend::new().open_video(&clip, &req).unwrap();
        let never = StopToken::never();
        let n = frames.len();
        for k in 0..n {
            let (f, e, _) = seek_pts(&mut *d, frames[k].pts, SeekMode::Exact);
            assert_eq!(
                (Seen::of(&f), e),
                (frames[k], Exactness::Exact),
                "{name}: {k}"
            );
            match (d.next_frame(&never).unwrap(), frames.get(k + 1)) {
                (NextFrame::Frame(f), Some(want)) => {
                    assert_eq!(Seen::of(&f), *want, "{name}: after {k}");
                }
                (NextFrame::End, None) => {}
                (got, want) => panic!("{name}: after {k}: {got:?}, expected {want:?}"),
            }
            let (f, e, _) = seek_pts(&mut *d, frames[k].pts, SeekMode::Keyframe);
            assert_eq!(e, Exactness::KeyframeOnly, "{name}: keyframe at {k}");
            assert!(f.key, "{name}: keyframe at {k} gave pts {}", f.pts);
            let i = frames.iter().position(|s| s.pts == f.pts);
            assert_eq!(
                i.map(|i| frames[i]),
                Some(Seen::of(&f)),
                "{name}: keyframe at {k}"
            );
            let j = (k * 7 + 3) % n;
            let (f, e, _) = seek_pts(&mut *d, frames[j].pts, SeekMode::Exact);
            assert_eq!(
                (Seen::of(&f), e),
                (frames[j], Exactness::Exact),
                "{name}: {k}, {j}"
            );
        }
        let events = d.take_events();
        assert!(events.is_empty(), "{name}: {events:?}");
    }
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
    let frames = oracle(&clip, GOP_8).frames;
    // The three frames before the edit are decoded but never shown.
    assert_eq!(frames.len(), 24);
    let d = open(&clip);
    assert_eq!(d.info().frame_count, Some(24));
    assert_eq!(d.info().origin_pts, 0);
}

#[test]
fn a_variable_frame_rate_seeks_exactly_and_counts_frames() {
    let dir = TempDir::new("vfr");
    // The HEVC clip's last frame says it lasts a decode-time step, which
    // isn't how long it's shown; the stream's duration says that.
    for (name, codec) in [("vfr", Codec::H264), ("vfr-hevc", Codec::Hevc)] {
        let clip = write(
            &Spec {
                vfr: true,
                ..spec(name, codec)
            },
            &dir,
        );
        let frames = oracle(&clip, GOP_8).frames;
        let d = open(&clip);
        assert!(
            matches!(d.info().timing, Timing::Variable { .. }),
            "{:?}",
            d.info().timing
        );
        assert_eq!(d.info().time_base, Rational::new(1, 90_000).unwrap());
        let mut pts = 0;
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.pts, pts, "{name}: frame {i}");
            pts += media::vfr_ticks(i as u32);
        }
        assert_eq!(
            seek::frame_address(d.info(), 7).unwrap(),
            FrameAddress::Counted(7)
        );
        // In Matroska with a default duration of 30 fps, which its
        // average rate isn't: the blocks say it varies.
        let mkv = dir.join(format!("{name}-30.mkv"));
        media::remux_to_mkv_at(&clip, &mkv, FrameRate::FPS_30).unwrap();
        let in_mkv = oracle(&mkv, GOP_8).frames;
        assert_eq!(in_mkv.len(), frames.len());
        let timing = open(&mkv).info().timing;
        assert!(matches!(timing, Timing::Variable { .. }), "{timing:?}");
    }
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
    oracle(&ntsc, GOP_8);
    let mp4 = write(
        &Spec {
            rate: FrameRate::FPS_59_94,
            ..spec("h264-5994", Codec::H264)
        },
        &dir,
    );
    let in_mp4 = oracle(&mp4, GOP_8).frames;
    assert_eq!(
        open(&mp4).info().timing,
        Timing::Constant(FrameRate::FPS_59_94)
    );
    // Matroska rounds every timestamp to the millisecond, so frame n is
    // found at (n + ½) frame durations, not at n of them.
    let mkv = media::remux_to_mkv(&mp4, &dir).unwrap();
    let in_mkv = oracle(&mkv, GOP_8).frames;
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
    // With the chroma location FFmpeg reports: x264 and x265 write the
    // clips' centre siting for 4:2:0 only. For 4:2:2, FFmpeg's H.264
    // decoder says left, its default; for 4:4:4, its HEVC decoder says
    // nothing, and no siting is assumed, since none matters.
    let cases = [
        (
            Spec {
                color: full,
                ..small("h264-full", Codec::H264)
            },
            // FFmpeg's H.264 decoder says full range so.
            "yuvj420p",
            2,
        ),
        (
            Spec {
                color: bt601,
                ..small("h264-bt601", Codec::H264)
            },
            "yuv420p",
            2,
        ),
        (
            Spec {
                bits: 10,
                gop: None,
                ..small("hevc10-lossless", Codec::HevcLossless)
            },
            "yuv420p10le",
            2,
        ),
        (
            Spec {
                bits: 10,
                chroma: Chroma::Yuv422,
                ..small("h264-422-10", Codec::H264)
            },
            "yuv422p10le",
            1,
        ),
        (
            Spec {
                chroma: Chroma::Yuv444,
                ..small("hevc-444-8", Codec::Hevc)
            },
            "yuv444p",
            0,
        ),
        (
            Spec {
                bits: 10,
                chroma: Chroma::Yuv444,
                ..small("hevc-444-10", Codec::Hevc)
            },
            "yuv444p10le",
            0,
        ),
    ];
    for (s, pix_fmt, chroma_location) in &cases {
        let clip = write(s, &dir);
        let keyint = s.gop.map(|g| g.keyint);
        oracle(
            &clip,
            Expect {
                keyint,
                ..Expect::default()
            },
        );
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
        assert_eq!(c.codes.chroma_location, *chroma_location, "{}", s.name);
        assert!(!c.assumed.siting, "{}: {c:?}", s.name);
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
    let mut last_key = None;
    let never = StopToken::never();
    loop {
        match d.next_frame(&never) {
            Ok(NextFrame::Frame(f)) => {
                before.push(Seen::of(&f));
                if f.key {
                    last_key = before.last().copied();
                }
            }
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
    // Past the cut: the last good frame, flagged, and a Damaged event,
    // decoded from the last keyframe the file holds, not from the start.
    let target = whole.last().unwrap().pts;
    let (got, e, cost) = seek_pts(&mut *d, target, SeekMode::Exact);
    assert_eq!(Seen::of(&got), last_good);
    assert_eq!(
        e,
        Exactness::Truncated {
            last_good_pts: last_good.pts
        }
    );
    assert!(
        cost.seeks == 2 && cost.reseeks == 1 && cost.frames_decoded <= 8 + 3,
        "{cost:?}"
    );
    assert!(
        d.take_events()
            .iter()
            .any(|e| matches!(e, DecoderEvent::Damaged { .. }))
    );
    // A keyframe seek there gives the last keyframe the file holds, also
    // with a Damaged event, rather than the first frame.
    let (got, e, _) = seek_pts(&mut *d, target, SeekMode::Keyframe);
    assert_eq!(
        (Some(Seen::of(&got)), e),
        (last_key, Exactness::KeyframeOnly)
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

/// The video samples of the MP4 at `path` in decode order: each one's
/// pts, where it starts in the file, its size, and whether it is a
/// keyframe.
// Reads a file this test made with ffmpeg-next alone, as `faststart` does.
#[allow(clippy::disallowed_methods)]
fn samples(path: &Path) -> Vec<(i64, usize, usize, bool)> {
    let mut ictx = ff::format::input(path).unwrap();
    ictx.packets()
        .filter(|(s, _)| s.parameters().medium() == ff::media::Type::Video)
        .map(|(_, p)| {
            (
                p.pts().unwrap(),
                usize::try_from(p.position()).unwrap(),
                p.size(),
                p.is_key(),
            )
        })
        .collect()
}

/// The pts of the video samples the MP4 at `path` lists as sync samples
/// (`stss`), the keyframes of FFmpeg's index, whether or not its H.264
/// parser flags their packets as keyframes.
fn sync_samples(path: &Path) -> Vec<i64> {
    let bytes = std::fs::read(path).unwrap();
    let tree = spherecut_media::mp4::read(&mut std::io::Cursor::new(&bytes)).unwrap();
    let stss = tree
        .tracks()
        .find_map(|t| t.path(&[b"mdia", b"minf", b"stbl", b"stss"]))
        .unwrap();
    let pk = samples(path);
    // After the version, flags and entry count, 1-based sample numbers.
    stss.payload[8..]
        .chunks_exact(4)
        .map(|n| pk[u32::from_be_bytes(n.try_into().unwrap()) as usize - 1].0)
        .collect()
}

/// Zero the first bytes of the slice header of the first slice in the
/// sample at `pos`, `size` bytes of NAL units with 4-byte lengths, so the
/// decoder can't decode that picture: the parameter set (HEVC) or first
/// macroblock (H.264) it names is out of range.
fn break_slice_header(bytes: &mut [u8], pos: usize, size: usize, codec: Codec) {
    let mut at = pos;
    while at + 4 < pos + size {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let header = bytes[at + 4];
        let (slice, header_len) = match codec {
            Codec::H264 => ((1..=5).contains(&(header & 0x1f)), 1),
            _ => (((header >> 1) & 0x3f) < 32, 2),
        };
        if slice {
            let start = at + 4 + header_len;
            bytes[start..start + 6.min(len - header_len)].fill(0);
            return;
        }
        at += 4 + len;
    }
    panic!("no slice in the sample at byte {pos}");
}

/// Damage that loses a reference picture is a `Damaged` event, and
/// seeks agree with the decode from the start (D29). The picture decoded
/// after a GOP's keyframe has its slice header broken, and every later
/// picture of the GOP refers to it, directly or not. FFmpeg 6.1 fails it
/// as invalid data and decodes the rest from a grey stand-in. From 7.1
/// the HEVC decoder skips it without an error, and from 8.0 it flags the
/// pictures that refer to it corrupt, which are dropped: only their
/// flags say the file is damaged. FFmpeg 7.1.x does neither, and nothing
/// says so (D29's limits).
#[test]
fn damage_that_loses_a_reference_picture_is_reported_and_seeks_agree() {
    let dir = TempDir::new("lost-reference");
    let v = ff::codec::version();
    let (v7_1, v8_0) = (61 << 16 | 19 << 8, 62 << 16 | 11 << 8);
    let hevc_silent = (v7_1..v8_0).contains(&v);
    let hevc_flags = v >= v8_0;
    let never = StopToken::never();
    let damaged = |events: &[DecoderEvent]| -> Vec<Option<i64>> {
        events
            .iter()
            .filter_map(|e| match e {
                DecoderEvent::Damaged { last_good_pts, .. } => Some(*last_good_pts),
                _ => None,
            })
            .collect()
    };
    for (name, codec, bframes) in [
        ("lost-hevc-p", Codec::Hevc, 0),
        ("lost-hevc-b", Codec::Hevc, 3),
        ("lost-h264-p", Codec::H264, 0),
    ] {
        let gop = Some(Gop {
            keyint: 8,
            bframes,
            open: false,
            radl: 0,
        });
        let clip = write(
            &Spec {
                gop,
                frames: 24,
                ..spec(name, codec)
            },
            &dir,
        );
        let (whole, _) = sequential(&clip);
        let pk = samples(&clip);
        let keys: Vec<usize> = (0..pk.len()).filter(|&i| pk[i].3).collect();
        assert_eq!(keys.len(), 3, "{name}: keyframes at {keys:?}");
        let (key, next) = (keys[1], keys[2]);
        let (lost_pts, pos, size, _) = pk[key + 1];
        let mut bytes = std::fs::read(&clip).unwrap();
        break_slice_header(&mut bytes, pos, size, codec);
        let bad = dir.join(format!("{name}-damaged.mp4"));
        std::fs::write(&bad, &bytes).unwrap();
        // The frames decoded from the lost picture on, to the next
        // keyframe, which with B-frames include some shown before it.
        let lost_from = pk[key + 1..next].iter().map(|p| p.0).min().unwrap();
        let lost = lost_from..pk[next].0;
        let reported = codec == Codec::H264 || !hevc_silent;
        for threads in [None, std::num::NonZeroU32::new(1)] {
            let what = format!("{name} on {threads:?} threads");
            let req = VideoRequest {
                threads,
                ..VideoRequest::default()
            };
            let open_bad = || FfmpegBackend::new().open_video(&bad, &req).unwrap();
            let mut d = open_bad();
            let mut seen = Vec::new();
            while let NextFrame::Frame(f) = d.next_frame(&never).unwrap() {
                seen.push(Seen::of(&f));
            }
            let events = d.take_events();
            let after = damaged(&events);
            assert_eq!(!after.is_empty(), reported, "{what}: {events:?}");
            if codec == Codec::Hevc && hevc_flags {
                // Once, after the last frame decoded before the frames
                // flagged corrupt: the GOP's keyframe.
                assert_eq!(after, [Some(pk[key].0)], "{what}: {events:?}");
            }
            // Nothing outside the frames that refer to the lost picture is
            // lost or changed, and that picture never comes out.
            for w in whole.iter().filter(|w| !lost.contains(&w.pts)) {
                assert!(seen.contains(w), "{what}: frame at pts {} lost", w.pts);
            }
            assert!(
                seen.iter().all(|s| s.pts != lost_pts),
                "{what}: the damaged picture came out"
            );
            // A seek to any frame of the clip gives the frame the decode
            // from the start shows then.
            for w in &whole {
                let (f, e, _) = seek_pts(&mut *d, w.pts, SeekMode::Exact);
                let shown = seen[shown_at(&seen, w.pts).unwrap()];
                assert_eq!(
                    (Seen::of(&f), e),
                    (shown, Exactness::Exact),
                    "{what}: seek to pts {}",
                    w.pts
                );
            }
            // And one to a frame that refers to the lost picture, from a
            // decoder just opened, says the file is damaged, as does a
            // keyframe seek there, which gives the GOP's keyframe.
            for w in whole.iter().filter(|w| lost.contains(&w.pts)) {
                for mode in [SeekMode::Exact, SeekMode::Keyframe] {
                    let mut d = open_bad();
                    let (f, e, _) = seek_pts(&mut *d, w.pts, mode);
                    if mode == SeekMode::Keyframe {
                        assert_eq!(
                            (f.pts, e),
                            (pk[key].0, Exactness::KeyframeOnly),
                            "{what}: keyframe seek to pts {}",
                            w.pts
                        );
                    }
                    let events = d.take_events();
                    assert_eq!(
                        !damaged(&events).is_empty(),
                        reported,
                        "{what}: {mode:?} seek to pts {}: {events:?}",
                        w.pts
                    );
                }
            }
        }
    }
}

/// Matroska's demuxer reads its index (the Cues, after the clusters)
/// only at its first seek; until then the index lists only the keyframes
/// read, and a far seek must not take it to mean that the target is in
/// the GOP being decoded.
#[test]
fn a_long_matroska_file_seeks_far_rather_than_decoding_there() {
    let dir = TempDir::new("long-mkv");
    let clip = write(
        &Spec {
            width: 64,
            height: 32,
            frames: 300,
            ..spec("long", Codec::H264)
        },
        &dir,
    );
    let mkv = media::remux_to_mkv(&clip, &dir).unwrap();
    let mut d = open(&mkv);
    play(&mut *d, 20);
    // Frame 290, at its millisecond.
    let pts = FrameRate::FPS_30
        .frame_to_time(290)
        .unwrap()
        .to_pts(d.info().time_base, spherecut_core::Rounding::Nearest)
        .unwrap();
    let (f, e, cost) = seek_pts(&mut *d, pts, SeekMode::Exact);
    assert_eq!((f.pts, e), (pts, Exactness::Exact));
    assert!(
        cost.seeks >= 1 && !cost.forward_only && cost.frames_decoded <= 2 * 8 + 2,
        "{cost:?}"
    );
}

/// A frame at 30 fps in ticks of an MP4 video track's 1/15360 s.
const FRAME_30: i64 = 512;

/// Copy the MP4 `src`, whose `moov` comes after its media data, into
/// `dst` with its track's edit list replaced by `edits`, each (duration
/// in the movie's ticks, media time in the track's), played at rate 1,
/// in version 0. As an editor that splices pieces of a clip writes it.
fn splice(src: &Path, dst: &Path, edits: &[(u32, i32)]) {
    let mut elst = (edits.len() as u32).to_be_bytes().to_vec();
    for &(duration, media_time) in edits {
        elst.extend_from_slice(&duration.to_be_bytes());
        elst.extend_from_slice(&media_time.to_be_bytes());
        elst.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    }
    let elst = mp4_box(b"elst", &[&[0; 4][..], &elst].concat());
    // The boxes in `b`, with the `elst` under `moov/trak/edts` replaced.
    fn rebuild(b: &[u8], elst: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0;
        while at + 8 <= b.len() {
            let size = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
            assert!(size >= 8, "a box of size {size}");
            let kind: [u8; 4] = b[at + 4..at + 8].try_into().unwrap();
            match &kind {
                b"moov" | b"trak" | b"edts" => {
                    out.extend(mp4_box(&kind, &rebuild(&b[at + 8..at + size], elst)))
                }
                b"elst" => out.extend_from_slice(elst),
                _ => out.extend_from_slice(&b[at..at + size]),
            }
            at += size;
        }
        out
    }
    let bytes = std::fs::read(src).unwrap();
    let (mdat, moov) = (find_box(&bytes, b"mdat"), find_box(&bytes, b"moov"));
    // Chunk offsets stay right only when nothing before the media moves.
    assert!(mdat < moov, "{}: moov first", src.display());
    let out = rebuild(&bytes, &elst);
    assert_ne!(out, bytes, "{}: no edit list", src.display());
    std::fs::write(dst, out).unwrap();
}

/// A box of type `kind` holding `payload`.
fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut b = (payload.len() as u32 + 8).to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    b.extend_from_slice(payload);
    b
}

/// Where the top-level box of type `kind` starts in `file`.
fn find_box(file: &[u8], kind: &[u8; 4]) -> usize {
    let mut at = 0;
    loop {
        let size = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        if &file[at + 4..at + 8] == kind {
            return at;
        }
        assert!(size >= 8, "a box of size {size}");
        at += size;
    }
}

/// An edit list of two pieces of the clip, as editors write: FFmpeg drops
/// frames where they meet, so frames aren't shown a frame apart however
/// even `stts` is, and up to FFmpeg 7.0 the HEVC decoder says the splice
/// is damage.
#[test]
fn an_edit_list_that_splices_two_pieces_seeks_exactly_and_counts_frames() {
    let dir = TempDir::new("splice");
    for codec in [Codec::H264, Codec::Hevc] {
        let name = format!("whole-{codec:?}");
        let whole = write(
            &Spec {
                frames: 60,
                ..spec(&name, codec)
            },
            &dir,
        );
        // Frames 0 to 14, then 30 to 44: 500 ms each, after the lead-in
        // of B-frame delay the clip's own edit hides.
        let spliced = dir.join(format!("spliced-{codec:?}.mp4"));
        let lead_in = 2 * FRAME_30 as i32;
        splice(
            &whole,
            &spliced,
            &[(500, lead_in), (500, lead_in + 30 * FRAME_30 as i32)],
        );
        // Up to FFmpeg 7.0 the HEVC decoder says the splice is damage
        // (below).
        let fails_at_splice = codec == Codec::Hevc && ff::codec::version() < (61 << 16 | 19 << 8);
        let frames = oracle(
            &spliced,
            Expect {
                spliced: true,
                damaged: fails_at_splice,
                ..GOP_8
            },
        )
        .frames;
        let gaps = frames
            .windows(2)
            .filter(|w| w[1].pts - w[0].pts != FRAME_30);
        assert!(gaps.count() > 0, "{codec:?}: {frames:?}");
        // Played through, and caught up across the splice from a seek
        // before it: the first frame from the target comes out. The second
        // piece's keyframe has a POC the HEVC decoder still holds. Up to
        // FFmpeg 7.0 sending it fails with invalid data, which is a
        // `Damaged` event. From 7.1 (libavcodec 61.19) the decoder skips a
        // picture it can't decode with a warning and no error, and none of
        // the frames after this one comes out flagged corrupt, so there is
        // nothing to report: the same frames come out (D29).
        let never = StopToken::never();
        let mut d = open(&spliced);
        while let NextFrame::Frame(_) = d.next_frame(&never).unwrap() {}
        let played = d.take_events();
        assert_eq!(played.is_empty(), !fails_at_splice, "{played:?}");
        for (from, until) in [(10, 15), (10, 16), (12, 14), (5, 17), (13, 15)] {
            seek_pts(&mut *d, frames[from].pts, SeekMode::Exact);
            d.catch_up(Some(at(&*d, frames[until].pts)));
            let got = d.next_frame(&never).unwrap();
            let NextFrame::Frame(f) = got else {
                panic!("{codec:?}: {got:?}");
            };
            assert_eq!(Seen::of(&f), frames[until], "{codec:?}: {from} to {until}");
            let events = d.take_events();
            assert_eq!(events.is_empty(), played.is_empty(), "{events:?}");
        }
        assert!(
            matches!(d.info().timing, Timing::Variable { .. }),
            "{codec:?}: {:?}",
            d.info().timing
        );
    }
}

/// Copy `src` into the MP4 `dst`, giving video packet `k` in decode order
/// the pts `pts(k, its pts)` and keeping its dts: timestamps that tie or
/// go backwards, which no encoder writes but broken muxers do.
// Writes a file this test made with ffmpeg-next alone, as `faststart`
// does.
#[allow(clippy::disallowed_methods)]
fn retime(src: &Path, dst: &Path, pts: impl Fn(usize, i64) -> i64) {
    let mut ictx = ff::format::input(src).unwrap();
    let mut octx = ff::format::output(dst).unwrap();
    for st in ictx.streams() {
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    octx.write_header().unwrap();
    let mut k = 0;
    for (st, mut pkt) in ictx.packets() {
        if st.parameters().medium() == ff::media::Type::Video {
            pkt.set_pts(Some(pts(k, pkt.pts().unwrap())));
            k += 1;
        }
        pkt.rescale_ts(st.time_base(), octx.stream(st.index()).unwrap().time_base());
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
}

/// A broken mux: the tie rule picks the later of frames at one pts, and a
/// stream whose timestamps go backwards is found out by playing it, and
/// is then sought exactly by decoding from the start.
#[test]
fn timestamps_that_tie_or_go_backwards_seek_by_the_tie_rule() {
    let dir = TempDir::new("broken-pts");
    // P-frames only, so that dts are pts, and 20 frames, since every
    // exact seek in the backwards clip decodes it twice.
    let p_only = write(
        &Spec {
            frames: 20,
            gop: Some(Gop {
                keyint: 8,
                bframes: 0,
                open: false,
                radl: 0,
            }),
            ..spec("p-only", Codec::H264)
        },
        &dir,
    );
    // Frames 9 and 10 at one pts: 9 is never shown.
    let ties = dir.join("ties.mp4");
    retime(&p_only, &ties, |k, pts| {
        pts + if k == 10 { 0 } else { FRAME_30 }
    });
    let r = oracle(&ties, GOP_8);
    assert_eq!(r.frames[9].pts, r.frames[10].pts);
    // `stts` says 30 fps, but frames aren't shown every 30th of a second.
    let mut d = open(&ties);
    assert!(
        matches!(d.info().timing, Timing::Variable { .. }),
        "{:?}",
        d.info().timing
    );
    // Played up to 9, then a seek stopped at once: the next frame is 10,
    // which ties with 9, and it follows on in 9's run.
    let nine = play(&mut *d, 10);
    assert_eq!(Seen::of(&nine), r.frames[9]);
    stopped_seek(&mut *d);
    let ten = play(&mut *d, 1);
    assert_eq!(Seen::of(&ten), r.frames[10]);
    assert_eq!((ten.run, ten.seq), (nine.run, nine.seq + 1));
    // And in Matroska, tied at the millisecond, with a default duration of
    // 30 fps.
    let mkv = dir.join("ties.mkv");
    media::remux_to_mkv_at(&ties, &mkv, FrameRate::FPS_30).unwrap();
    oracle(&mkv, GOP_8);
    // Frame 7 shown at keyframe 8's pts, in MP4 and Matroska. Played
    // through, so that the ties are known: a seek whose first frame is 8
    // has no frame before it tied with it, and after a stopped seek the
    // next frame is 9 all the same, in 8's run.
    let never = StopToken::never();
    let key_tie = dir.join("key-tie.mp4");
    retime(&p_only, &key_tie, |k, pts| {
        pts + if k == 7 { FRAME_30 } else { 0 }
    });
    let key_tie_mkv = dir.join("key-tie.mkv");
    media::remux_to_mkv_at(&key_tie, &key_tie_mkv, FrameRate::FPS_30).unwrap();
    for clip in [&key_tie, &key_tie_mkv] {
        let frames = oracle(clip, GOP_8).frames;
        assert_eq!(frames[7].pts, frames[8].pts);
        for mode in [SeekMode::Exact, SeekMode::Keyframe] {
            let mut d = open(clip);
            while let NextFrame::Frame(_) = d.next_frame(&never).unwrap() {}
            let (eight, _, _) = seek_pts(&mut *d, frames[8].pts, mode);
            assert_eq!(Seen::of(&eight), frames[8], "{mode:?}");
            stopped_seek(&mut *d);
            let nine = play(&mut *d, 1);
            assert_eq!(Seen::of(&nine), frames[9], "{mode:?}");
            let mut runs = Runs::default();
            runs.note("key-tie", &eight, 8);
            runs.note("key-tie", &nine, 9);
        }
    }

    // Frames 10 to 14 shown a frame and a half late, so 15 is shown
    // before 14.
    let back = dir.join("backwards.mp4");
    retime(&p_only, &back, |k, pts| {
        pts + if (10..15).contains(&k) {
            3 * FRAME_30 / 2
        } else {
            0
        }
    });
    let backwards = Expect {
        backwards: true,
        ..GOP_8
    };
    let r = oracle(&back, backwards);
    let mkv = dir.join("backwards.mkv");
    media::remux_to_mkv_at(&back, &mkv, FrameRate::FPS_30).unwrap();
    let in_mkv = oracle(&mkv, backwards).frames;
    // Played on from a seek made before the step was found, so that the
    // frames handed out have no place counted from the start: after a
    // stopped seek, the next frame is still the one after the last, and
    // it follows on in that frame's run only where that is known.
    for (clip, frames) in [(&back, &r.frames), (&mkv, &in_mkv)] {
        for n in 12..=16 {
            let mut d = open(clip);
            seek_pts(&mut *d, frames[17].pts, SeekMode::Exact);
            let (nine, e, _) = seek_pts(&mut *d, frames[9].pts, SeekMode::Exact);
            assert_eq!((Seen::of(&nine), e), (frames[9], Exactness::Exact));
            let last = play(&mut *d, n - 9);
            assert_eq!(Seen::of(&last), frames[n], "{n}");
            stopped_seek(&mut *d);
            let next = play(&mut *d, 1);
            assert_eq!(Seen::of(&next), frames[n + 1], "after {n}");
            let mut runs = Runs::default();
            runs.note("backwards", &last, n);
            runs.note("backwards", &next, n + 1);
        }
    }
    let (f14, f15) = (r.frames[14], r.frames[15]);
    assert!(f15.pts < f14.pts, "{:?}", r.frames);
    // Not yet played: the seek that decodes 15 after 14 finds the step,
    // and still gives the frame with the greatest pts, not the last
    // decoded.
    let mut d = open(&back);
    assert!(matches!(d.info().timing, Timing::Variable { .. }));
    let (got, e, _) = seek_pts(&mut *d, f14.pts + 1, SeekMode::Exact);
    assert_eq!((Seen::of(&got), e), (f14, Exactness::Exact));
    assert!(
        d.take_events()
            .iter()
            .any(|e| matches!(e, DecoderEvent::NotExactlySeekable { .. }))
    );
    // Showing 15, with 16 next: a frame between the two, 14, is shown
    // after 15's pts.
    seek_pts(&mut *d, f14.pts, SeekMode::Exact);
    let NextFrame::Frame(f) = d.next_frame(&StopToken::never()).unwrap() else {
        panic!("no frame after 14");
    };
    assert_eq!(Seen::of(&f), f15);
    let (got, e, _) = seek_pts(&mut *d, f14.pts + 1, SeekMode::Exact);
    assert_eq!((Seen::of(&got), e), (f14, Exactness::Exact));
    // Played up to 15, then a seek stopped at once: the next frame is 16,
    // though 14 is the first decoded after it with a greater pts.
    let mut d = open(&back);
    let fifteen = play(&mut *d, 16);
    assert_eq!(Seen::of(&fifteen), f15);
    stopped_seek(&mut *d);
    let sixteen = play(&mut *d, 1);
    assert_eq!(Seen::of(&sixteen), r.frames[16]);
    assert_eq!((sixteen.run, sixteen.seq), (fifteen.run, fifteen.seq + 1));
}

/// The last of the next `n` frames.
fn play(d: &mut dyn VideoDecode, n: usize) -> Arc<VideoFrame> {
    let mut last = None;
    for _ in 0..n {
        match d.next_frame(&StopToken::never()).unwrap() {
            NextFrame::Frame(f) => last = Some(f),
            got => panic!("{got:?}"),
        }
    }
    last.unwrap()
}

/// A seek to the start with a token stopped before it.
fn stopped_seek(d: &mut dyn VideoDecode) {
    let source = StopSource::new();
    let stopped = source.current();
    source.next();
    assert!(matches!(
        d.seek(Time::ZERO, SeekMode::Exact, &stopped).unwrap(),
        SeekOutcome::Stopped
    ));
}

#[test]
fn a_stop_token_stops_a_long_seek_and_decoding_goes_on() {
    let dir = TempDir::new("long-gop");
    // One GOP: the last frame is reached only by decoding every frame.
    // Many small frames make a long decode that is quick to write. Where
    // they decode too fast for the stops below to come part way in (frame
    // threads on FFmpeg 8.1.3 took 35 ms for 2400 now and then, and about
    // 100 most times), the clip is written again longer, up to 16 times.
    let mut frames = 2400;
    let (mut d, last, end, full) = loop {
        let clip = write(
            &Spec {
                width: 64,
                height: 32,
                frames,
                gop: Some(Gop {
                    keyint: 100_000,
                    bframes: 3,
                    open: false,
                    radl: 0,
                }),
                ..spec(&format!("long-gop-{frames}"), Codec::H264)
            },
            &dir,
        );
        let mut d = open(&clip);
        let last = FrameRate::FPS_30
            .frame_to_time(i64::from(frames) - 1)
            .unwrap();
        // How long the whole seek takes, from the start.
        let started = Instant::now();
        let (end, _, cost) = seek_to(&mut *d, last, SeekMode::Exact);
        let full = started.elapsed();
        assert!(cost.frames_decoded >= frames - 1, "{cost:?}");
        if full > Duration::from_millis(40) || frames >= 2400 * 16 {
            break (d, last, end, full);
        }
        // Enough frames for about 80 ms at this rate, and at least twice as
        // many.
        let times = (Duration::from_millis(80).as_nanos() / full.as_nanos().max(1)).max(1) + 1;
        frames = (frames * u32::try_from(times).unwrap_or(16)).min(2400 * 16);
    };
    assert!(
        full > Duration::from_millis(40),
        "the long seek of {frames} frames took only {full:?}"
    );
    let (mut first, _, _) = seek_to(&mut *d, Time::ZERO, SeekMode::Exact);

    // Stopped by a newer generation a little way in. When the machine's
    // load drops after `full` was timed, the seek can end before the stop
    // comes: then it starts again from the first frame, and the stop comes
    // after half the wait.
    let source = StopSource::new();
    let mut wait = full / 8;
    let mut tries = 0;
    let (mut d, took) = loop {
        let token = source.next();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let r = d.seek(last, SeekMode::Exact, &token).unwrap();
            (d, r, started.elapsed())
        });
        std::thread::sleep(wait);
        source.next();
        let (back, outcome, took) = handle.join().unwrap();
        d = back;
        tries += 1;
        if matches!(outcome, SeekOutcome::Stopped) {
            break (d, took);
        }
        assert!(tries < 8, "no stop in {tries} tries, the last {outcome:?}");
        wait /= 2;
        (first, _, _) = seek_to(&mut *d, Time::ZERO, SeekMode::Exact);
    };
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

/// Catching up hands out frames that follow on from the last one before
/// it, or that start a run, however it ends: at its target, after a stop,
/// or called off by a seek or by catching up to an earlier time. On one
/// thread, as a hardware decoder runs, the frames it skips never come out
/// of the decoder at all. In a stream whose timestamps go backwards, it
/// skips none, since every frame is counted from the start.
#[test]
fn catching_up_keeps_its_place_and_links_only_frames_that_follow_on() {
    let dir = TempDir::new("catch-up-runs");
    let req = VideoRequest {
        threads: std::num::NonZeroU32::new(1),
        ..VideoRequest::default()
    };
    let never = StopToken::never();
    let source = StopSource::new();
    let stopped = source.current();
    source.next();
    let h264 = write(&spec("catch-up-h264", Codec::H264), &dir);
    // Packets 10 to 14 in decode order shown a frame and a half late.
    let back = dir.join("catch-up-back.mp4");
    retime(&h264, &back, |k, pts| {
        pts + if (10..15).contains(&k) {
            3 * FRAME_30 / 2
        } else {
            0
        }
    });
    let hevc = write(&spec("catch-up-hevc", Codec::Hevc), &dir);
    let clips = [
        ("catch-up-h264", h264),
        ("catch-up-hevc", hevc),
        ("catch-up-back", back),
    ];
    for (name, clip) in clips {
        let (frames, _) = sequential(&clip);
        let backwards = frames.windows(2).any(|w| w[1].pts < w[0].pts);
        let mut d = FfmpegBackend::new().open_video(&clip, &req).unwrap();
        let d = &mut *d;
        if name == "catch-up-back" {
            assert!(backwards);
            // Found out by playing it through.
            while let NextFrame::Frame(_) = d.next_frame(&never).unwrap() {}
            assert!(
                d.take_events()
                    .iter()
                    .any(|e| matches!(e, DecoderEvent::NotExactlySeekable { .. })),
                "{name}"
            );
        }
        // The first frame from frame `i` on, in output order, shown at or
        // after `pts`.
        let first_from = |i: usize, pts: i64| (i..).find(|&j| frames[j].pts >= pts).unwrap();
        let mut runs = Runs::default();
        // Which frame of the sequential decode `f` is, noting its run.
        let mut note = |f: &VideoFrame| {
            let i = frames.iter().position(|s| s.pts == f.pts).unwrap();
            assert_eq!(Seen::of(f), frames[i], "{name}");
            runs.note(name, f, i);
            i
        };
        let next = |d: &mut dyn VideoDecode| match d.next_frame(&never).unwrap() {
            NextFrame::Frame(f) => f,
            got => panic!("{name}: {got:?}"),
        };
        let seek = |d: &mut dyn VideoDecode, i: usize| {
            let (f, e, _) = seek_pts(d, frames[i].pts, SeekMode::Exact);
            assert_eq!(e, Exactness::Exact, "{name}");
            f
        };
        for k in [3, 9, 14] {
            for gap in [2, 3, 5] {
                let target = k + 1 + gap;
                let until = at(d, frames[target].pts);
                let seen = format!("{name}: frame {k}, catching up to {target}");

                // Stopped before the next frame is known, then catching up.
                let f = seek(d, k);
                assert_eq!(note(&f), k, "{seen}");
                assert!(
                    matches!(d.next_frame(&stopped).unwrap(), NextFrame::Stopped),
                    "{seen}"
                );
                d.catch_up(Some(until));
                let reached = first_from(k + 1, frames[target].pts);
                for want in reached..reached + 3 {
                    assert_eq!(note(&next(d)), want, "{seen}");
                }
                // And then stopped: it goes on from there.
                stopped_seek(d);
                assert_eq!(note(&next(d)), reached + 3, "{seen}");

                // Catching up after a seek that was stopped before it began.
                let f = seek(d, k);
                note(&f);
                stopped_seek(d);
                d.catch_up(Some(at(d, frames[k + gap].pts)));
                let want = first_from(k + 1, frames[k + gap].pts);
                assert_eq!(note(&next(d)), want, "{seen}");

                // Stopped after passing a frame over, then called off: what
                // it passed over is found again.
                for call_off in 0..3 {
                    let f = seek(d, k);
                    note(&f);
                    d.catch_up(Some(until));
                    assert!(
                        matches!(d.next_frame(&stopped).unwrap(), NextFrame::Stopped),
                        "{seen}"
                    );
                    let f = match call_off {
                        0 => seek(d, k + 1),
                        1 => {
                            d.catch_up(None);
                            next(d)
                        }
                        _ => {
                            d.catch_up(Some(at(d, frames[k + 1].pts)));
                            next(d)
                        }
                    };
                    let how = format!("{seen}, called off by {call_off}");
                    assert_eq!(note(&f), k + 1, "{how}");
                    assert_eq!(note(&next(d)), k + 2, "{how}");
                }
            }
        }
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
    // Keeping it keeps its buffers, which hold a little more than the
    // rows it has (about 2% here).
    let rows: usize = (0..3).map(|i| a.plane(i).unwrap().bytes.len()).sum();
    assert!(
        rows < a.bytes() && a.bytes() < 2 * rows,
        "{} bytes for {rows} in rows",
        a.bytes()
    );
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
        // Every frame of rawvideo is a keyframe.
        oracle(
            clip,
            Expect {
                keyint: Some(1),
                ..Expect::default()
            },
        );
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
