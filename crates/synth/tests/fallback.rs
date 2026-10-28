//! Hardware decoding that falls back to software (M1 plan sections 3.5 and
//! 12.1, DECISIONS D30), at Tier 1: on FFmpeg's Vulkan device on lavapipe,
//! which decodes nothing; on a render node that isn't there; and with
//! `fault-injection`, which fails a frame "as hardware" on a software
//! stand-in for the device. Whatever the frame and whatever the decoder was
//! doing, the frames handed out are software's, with no gap and no repeat,
//! their runs link only frames that follow on, and the decoder says once
//! that it fell back.
//!
//! Each test writes its clips into its own directory under
//! `CARGO_TARGET_TMPDIR`, removed when it ends.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use spherecut_core::FrameRate;
use spherecut_core::video::VideoPlanes;
use spherecut_core::{StopSource, StopToken, Time};
use spherecut_media::backend::{
    AudioRequest, DecodeMode, DecodePath, DecoderEvent, Exactness, Fallback, FallbackReason, Fault,
    FaultPlan, FfmpegBackend, HwKind, MediaBackend, NextFrame, SeekMode, SeekOutcome, VideoDecode,
    VideoFrame, VideoRequest,
};
use spherecut_media::decode::{VideoDecoder, open_hw_device};
use spherecut_media::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_media::{HwDevice, HwRefusal, MediaError};
use spherecut_synth::media::{self, Codec, Gop, Spec};

/// A test's own directory, removed with what it holds when dropped, so
/// also when the test fails.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("fallback-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// 256x128, 30 frames at 30 fps, in open GOPs of 8 with 3 B-frames: a
/// seek lands on keyframes whose leading pictures it must seek before.
fn clip(dir: &TempDir, name: &str, codec: Codec) -> PathBuf {
    let spec = Spec {
        name: name.into(),
        codec,
        gop: Some(Gop {
            keyint: 8,
            bframes: 3,
            open: true,
            radl: 0,
        }),
        frames: 30,
        ..Spec::default()
    };
    media::write(&spec, &dir.0).unwrap()
}

/// `FfmpegBackend`, with every video open on a device of kind `kind` that
/// fails frame `plan.frame` as `plan.fault` says, falling back as
/// `fallback` says (synth's `FaultBackend`: it needs media's
/// `fault-injection`, which only synth's tests turn on, so it lives here
/// rather than in `fake.rs`).
#[derive(Debug)]
struct FaultBackend {
    inner: FfmpegBackend,
    kind: HwKind,
    plan: FaultPlan,
    fallback: Fallback,
}

impl FaultBackend {
    /// On a software stand-in for the device, which works anywhere.
    fn stand_in(frame: u64, fault: Fault, fallback: Fallback) -> Self {
        FaultBackend {
            inner: FfmpegBackend::new(),
            kind: HwKind::VaApi,
            plan: FaultPlan {
                frame,
                fault,
                stand_in: true,
            },
            fallback,
        }
    }
}

impl MediaBackend for FaultBackend {
    fn name(&self) -> &'static str {
        "fault"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.inner.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let req = VideoRequest {
            decode: DecodeMode::Hardware {
                kind: self.kind,
                device: None,
                fallback: self.fallback,
            },
            fault: Some(self.plan),
            ..req.clone()
        };
        self.inner.open_video(path, &req)
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn spherecut_media::backend::AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }
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

/// A frame as the tests know it.
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

fn software(path: &Path) -> Box<dyn VideoDecode> {
    FfmpegBackend::new()
        .open_video(path, &VideoRequest::default())
        .unwrap()
}

/// Every frame of a sequential software decode from the start.
fn sequential(path: &Path) -> Vec<Seen> {
    let mut d = software(path);
    let never = StopToken::never();
    let mut out = Vec::new();
    while let NextFrame::Frame(f) = d.next_frame(&never).unwrap() {
        out.push(Seen::of(&f));
    }
    out
}

fn at(d: &dyn VideoDecode, pts: i64) -> Time {
    let info = d.info();
    Time::from_pts(pts - info.origin_pts, info.time_base).unwrap()
}

/// The frames handed out, by run and seq: two frames of one run whose seq
/// follow on must follow on in the sequential decode (D29).
#[derive(Default)]
struct Runs(HashMap<(u64, u64), usize>);

impl Runs {
    fn note(&mut self, why: &str, f: &VideoFrame, index: usize) {
        let key = (f.run, f.seq);
        if let Some(&j) = self.0.get(&key) {
            assert_eq!(j, index, "{why}: run {key:?} was frame {j}");
        }
        if let Some(&j) = f.seq.checked_sub(1).and_then(|s| self.0.get(&(f.run, s))) {
            assert_eq!(j + 1, index, "{why}: run {key:?} follows frame {j}");
        }
        if let Some(&j) = self.0.get(&(f.run, f.seq + 1)) {
            assert_eq!(index + 1, j, "{why}: run {key:?} comes before frame {j}");
        }
        self.0.insert(key, index);
    }
}

/// One step of [`SCRIPT`], with frames by their place in the sequential
/// decode.
#[derive(Debug, Clone, Copy)]
enum Op {
    /// `next_frame`, this many times.
    Next(usize),
    Seek(usize, SeekMode),
    /// `catch_up` to the frame, or called off.
    CatchUp(Option<usize>),
    /// A seek whose stop token has already said to stop.
    StoppedSeek,
}

/// What the tests do with a decoder: play, seek ahead and back, catch up,
/// seek and be stopped, step on, scrub, and play to the end.
const SCRIPT: [Op; 14] = [
    Op::Next(3),
    Op::Seek(20, SeekMode::Exact),
    Op::Next(2),
    Op::Seek(5, SeekMode::Exact),
    Op::CatchUp(Some(13)),
    Op::Next(2),
    Op::CatchUp(None),
    Op::StoppedSeek,
    Op::Next(2),
    Op::Seek(10, SeekMode::Keyframe),
    Op::Next(1),
    Op::Seek(11, SeekMode::Exact),
    Op::Seek(27, SeekMode::Exact),
    Op::Next(4),
];

/// What one step gave.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Got {
    Frame(Seen, Option<Exactness>),
    End,
    Stopped,
    Nothing,
}

/// What a run of [`SCRIPT`] saw: each step's results, the frames the
/// decoder had made after each step, and its events.
#[derive(Debug, Default)]
struct Played {
    steps: Vec<Vec<Got>>,
    made_after: Vec<u64>,
    events: Vec<DecoderEvent>,
    paths: Vec<DecodePath>,
}

/// Run [`SCRIPT`] on `d`, checking runs against the sequential decode
/// `frames`, until it ends or fails.
fn play(d: &mut dyn VideoDecode, frames: &[Seen], why: &str) -> (Played, Option<MediaError>) {
    let index = |s: &Seen| frames.iter().position(|f| f == s);
    let mut runs = Runs::default();
    let mut out = Played::default();
    out.events.extend(d.take_events());
    let never = StopToken::never();
    let mut note = |f: &Arc<VideoFrame>, out: &mut Played| {
        let seen = Seen::of(f);
        // A frame that isn't one of software's is a mismatch the step
        // comparison reports; the runs only cover frames it knows.
        if let Some(i) = index(&seen) {
            runs.note(why, f, i);
        }
        out.paths.push(f.decoded_on.clone());
        seen
    };
    for op in SCRIPT {
        let mut got = Vec::new();
        let r: Result<(), MediaError> = (|| {
            match op {
                Op::Next(n) => {
                    for _ in 0..n {
                        match d.next_frame(&never)? {
                            NextFrame::Frame(f) => got.push(Got::Frame(note(&f, &mut out), None)),
                            NextFrame::End => got.push(Got::End),
                            NextFrame::Stopped => got.push(Got::Stopped),
                        }
                    }
                }
                Op::Seek(i, mode) => {
                    let t = at(d, frames[i].pts);
                    match d.seek(t, mode, &never)? {
                        SeekOutcome::Frame {
                            frame, exactness, ..
                        } => got.push(Got::Frame(note(&frame, &mut out), Some(exactness))),
                        SeekOutcome::Stopped => got.push(Got::Stopped),
                    }
                }
                Op::CatchUp(i) => {
                    d.catch_up(i.map(|i| at(d, frames[i].pts)));
                    got.push(Got::Nothing);
                }
                Op::StoppedSeek => {
                    let source = StopSource::new();
                    let stopped = source.current();
                    source.next();
                    match d.seek(at(d, frames[25].pts), SeekMode::Exact, &stopped)? {
                        SeekOutcome::Stopped => got.push(Got::Stopped),
                        SeekOutcome::Frame { frame, .. } => {
                            got.push(Got::Frame(note(&frame, &mut out), None))
                        }
                    }
                }
            }
            Ok(())
        })();
        out.steps.push(got);
        out.made_after.push(d.stats().frames_decoded);
        out.events.extend(d.take_events());
        if let Err(e) = r {
            return (out, Some(e));
        }
    }
    (out, None)
}

fn fell_back(events: &[DecoderEvent]) -> Vec<&FallbackReason> {
    events
        .iter()
        .filter_map(|e| match e {
            DecoderEvent::FellBack { from, reason } => {
                assert_eq!(*from, HwKind::VaApi);
                Some(reason)
            }
            _ => None,
        })
        .collect()
}

/// [`SCRIPT`] on a decoder with no fault, and the frames it made before
/// each step, for the steps to fail frames in.
fn reference(path: &Path, frames: &[Seen]) -> Played {
    // The stand-in, with a fault that never comes: it counts frames as the
    // faulty runs do (one thread, where software's own may use several).
    let backend = FaultBackend::stand_in(u64::MAX, Fault::Decode, Fallback::Software);
    let mut d = backend.open_video(path, &VideoRequest::default()).unwrap();
    assert_eq!(
        *d.decode_path(),
        DecodePath::Hardware {
            kind: HwKind::VaApi
        }
    );
    let (played, err) = play(&mut *d, frames, "reference");
    assert!(err.is_none(), "{err:?}");
    assert!(fell_back(&played.events).is_empty(), "{:?}", played.events);
    // Software's own frames, by pts and planes, and the end.
    let mut sw = software(path);
    let (sw_played, err) = play(&mut *sw, frames, "software");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(played.steps, sw_played.steps);
    assert_eq!(played.steps.last().unwrap().last(), Some(&Got::End));
    played
}

/// The frames the stand-in had made before step `i`, and after it.
fn span(r: &Played, i: usize) -> std::ops::Range<u64> {
    let before = i.checked_sub(1).map_or(1, |j| r.made_after[j]);
    before..r.made_after[i]
}

#[test]
fn a_frame_that_fails_to_decode_on_the_device_goes_on_in_software() {
    let dir = TempDir::new("decode-fault");
    for codec in [Codec::H264, Codec::Hevc] {
        let path = clip(&dir, &format!("{codec:?}.mp4"), codec);
        let frames = sequential(&path);
        let r = reference(&path, &frames);
        // Frame k fails wherever it falls: while opening (k = 0), in a
        // seek, while catching up, right after a stopped seek, in a
        // keyframe seek and playing on.
        let steps = |pick: fn(usize, Op) -> bool| -> Vec<std::ops::Range<u64>> {
            SCRIPT
                .iter()
                .enumerate()
                .filter(|&(i, &op)| pick(i, op))
                .map(|(i, _)| span(&r, i))
                .collect()
        };
        let seeks = steps(|_, op| matches!(op, Op::Seek(_, SeekMode::Exact)));
        let catching_up = steps(|i, _| i == 5);
        let after_stop = steps(|i, _| i == 8);
        let keyframe = steps(|_, op| matches!(op, Op::Seek(_, SeekMode::Keyframe)));
        for (what, spans) in [
            ("an exact seek", &seeks),
            ("catching up", &catching_up),
            ("the step after a stopped seek", &after_stop),
            ("a keyframe seek", &keyframe),
        ] {
            assert!(
                spans.iter().any(|s| !s.is_empty()),
                "{codec:?}: no frame is made during {what}: {:?}",
                r.made_after
            );
        }
        let total = *r.made_after.last().unwrap();
        for k in 0..total {
            let why = format!("{codec:?}, frame {k} failing to decode");
            let backend = FaultBackend::stand_in(k, Fault::Decode, Fallback::Software);
            let mut d = backend.open_video(&path, &VideoRequest::default()).unwrap();
            let (played, err) = play(&mut *d, &frames, &why);
            assert!(err.is_none(), "{why}: {err:?}");
            assert_eq!(played.steps, r.steps, "{why}");
            let reasons = fell_back(&played.events);
            assert_eq!(reasons.len(), 1, "{why}: {:?}", played.events);
            match reasons[0] {
                FallbackReason::Failed { after_pts, error } => {
                    assert!(error.contains("fault injected"), "{why}: {error}");
                    // Opening decodes frame 0, before any is returned; and
                    // the frame it went on after is one of the stream's.
                    match after_pts {
                        None => assert!(
                            played
                                .paths
                                .first()
                                .is_some_and(|p| matches!(p, DecodePath::FellBack { .. }))
                        ),
                        Some(pts) => {
                            assert!(k > 0, "{why}");
                            assert!(frames.iter().any(|f| f.pts == *pts), "{why}: {pts}");
                        }
                    }
                }
                other => panic!("{why}: {other:?}"),
            }
            // Never taken for damage.
            assert!(
                !played
                    .events
                    .iter()
                    .any(|e| matches!(e, DecoderEvent::Damaged { .. })),
                "{why}: {:?}",
                played.events
            );
            assert!(
                matches!(
                    d.decode_path(),
                    DecodePath::FellBack {
                        from: HwKind::VaApi,
                        ..
                    }
                ),
                "{why}: {:?}",
                d.decode_path()
            );
            // Frames from software say so, and none from the device comes
            // after them.
            let first_sw = played
                .paths
                .iter()
                .position(|p| matches!(p, DecodePath::FellBack { .. }));
            if let Some(i) = first_sw {
                assert!(
                    played.paths[i..]
                        .iter()
                        .all(|p| matches!(p, DecodePath::FellBack { .. })),
                    "{why}: {:?}",
                    played.paths
                );
            }
        }
    }
}

#[test]
fn a_frame_that_fails_to_download_goes_on_in_software() {
    // Only frames handed out are downloaded: the others never fail.
    let dir = TempDir::new("download-fault");
    let path = clip(&dir, "h264.mp4", Codec::H264);
    let frames = sequential(&path);
    let r = reference(&path, &frames);
    let total = *r.made_after.last().unwrap();
    let mut failed = 0;
    for k in 0..total {
        let why = format!("frame {k} failing to download");
        let backend = FaultBackend::stand_in(k, Fault::Download, Fallback::Software);
        let mut d = backend.open_video(&path, &VideoRequest::default()).unwrap();
        let (played, err) = play(&mut *d, &frames, &why);
        assert!(err.is_none(), "{why}: {err:?}");
        assert_eq!(played.steps, r.steps, "{why}");
        let reasons = fell_back(&played.events);
        assert!(reasons.len() <= 1, "{why}: {:?}", played.events);
        if let Some(reason) = reasons.first() {
            assert!(matches!(reason, FallbackReason::Failed { .. }), "{why}");
            failed += 1;
        }
    }
    // As many as the frames the device's run made into frames to hand
    // out (a frame shown again isn't downloaded again).
    let made = {
        let backend = FaultBackend::stand_in(u64::MAX, Fault::Download, Fallback::Software);
        let mut d = backend.open_video(&path, &VideoRequest::default()).unwrap();
        play(&mut *d, &frames, "count");
        d.stats().frames_returned
    };
    assert_eq!(failed, made);
}

#[test]
fn with_fallback_fail_a_device_failure_is_the_error() {
    let dir = TempDir::new("fail");
    let path = clip(&dir, "h264.mp4", Codec::H264);
    let frames = sequential(&path);
    let r = reference(&path, &frames);
    for k in [0, 7, span(&r, 1).start + 1, span(&r, 5).start] {
        let why = format!("frame {k}");
        let backend = FaultBackend::stand_in(k, Fault::Decode, Fallback::Fail);
        let opened = backend.open_video(&path, &VideoRequest::default());
        let err = match opened {
            Err(e) => e,
            Ok(mut d) => {
                let (played, err) = play(&mut *d, &frames, &why);
                assert!(fell_back(&played.events).is_empty(), "{why}");
                // Up to the failure, the device's frames; never damage, and
                // never a truncated seek.
                for (got, want) in played.steps.iter().zip(&r.steps) {
                    for g in got {
                        assert!(want.contains(g), "{why}: {g:?} not in {want:?}");
                    }
                }
                assert!(
                    !played
                        .events
                        .iter()
                        .any(|e| matches!(e, DecoderEvent::Damaged { .. })),
                    "{why}"
                );
                // The failure is sticky.
                let again = d.next_frame(&StopToken::never());
                assert!(
                    matches!(again, Err(MediaError::HwFailed { .. })),
                    "{why}: {again:?}"
                );
                err.unwrap()
            }
        };
        match &err {
            MediaError::HwFailed { kind, reason, .. } => {
                assert_eq!(kind, "VA-API", "{why}");
                assert!(reason.contains("fault injected"), "{why}: {reason}");
                assert_eq!(err.code(), "hw_failed");
            }
            e => panic!("{why}: {e:?}"),
        }
    }
}

/// A device that can't open another decoder for a decode from the
/// stream's start (which D29 opens afresh where the stream's first
/// picture is no keyframe; here every one after packets were sent) has
/// failed: the decode goes on in software, as the script's seek to frame
/// 5 needs, and nothing is taken for damage. With `Fallback::Fail` that
/// failure is the error.
#[test]
fn a_decoder_the_device_cannot_open_again_goes_on_in_software() {
    let dir = TempDir::new("reopen-fault");
    let path = clip(&dir, "h264.mp4", Codec::H264);
    let frames = sequential(&path);
    let r = reference(&path, &frames);
    let damaged = |events: &[DecoderEvent]| {
        events
            .iter()
            .any(|e| matches!(e, DecoderEvent::Damaged { .. }))
    };
    let backend = FaultBackend::stand_in(0, Fault::Reopen, Fallback::Software);
    let mut d = backend.open_video(&path, &VideoRequest::default()).unwrap();
    let (played, err) = play(&mut *d, &frames, "reopen");
    assert!(err.is_none(), "{err:?}");
    assert_eq!(played.steps, r.steps);
    let reasons = fell_back(&played.events);
    assert_eq!(reasons.len(), 1, "{:?}", played.events);
    match reasons[0] {
        FallbackReason::Failed { error, .. } => {
            assert!(error.contains("reopening the decoder failed"), "{error}")
        }
        other => panic!("{other:?}"),
    }
    assert!(!damaged(&played.events), "{:?}", played.events);

    let backend = FaultBackend::stand_in(0, Fault::Reopen, Fallback::Fail);
    let mut d = backend.open_video(&path, &VideoRequest::default()).unwrap();
    let (played, err) = play(&mut *d, &frames, "reopen, fail");
    assert!(
        matches!(&err, Some(MediaError::HwFailed { reason, .. }) if reason.contains("reopening")),
        "{err:?}"
    );
    assert!(!damaged(&played.events), "{:?}", played.events);
    let again = d.next_frame(&StopToken::never());
    assert!(
        matches!(again, Err(MediaError::HwFailed { .. })),
        "{again:?}"
    );
}

/// FFmpeg's Vulkan device on lavapipe, which has no video decode.
fn lavapipe() -> Arc<HwDevice> {
    open_hw_device("vulkan", Some("llvmpipe")).unwrap_or_else(|e| {
        panic!(
            "{e}; Tier 1 needs FFmpeg's Vulkan device on Mesa's lavapipe \
             (mesa-vulkan-drivers on Fedora and Ubuntu, vulkan-swrast on Arch)"
        )
    })
}

fn on_lavapipe(fallback: Fallback) -> VideoRequest {
    VideoRequest {
        decode: DecodeMode::Hardware {
            kind: HwKind::Vulkan,
            device: Some(lavapipe()),
            fallback,
        },
        ..VideoRequest::default()
    }
}

/// Ten frames of a moving ramp in MPEG-4 Part 2, which no FFmpeg decodes
/// with Vulkan Video.
fn mpeg4_clip(dir: &TempDir) -> PathBuf {
    let (w, h) = (256, 128);
    let settings = VideoSettings {
        encoder: "mpeg4".into(),
        width: w,
        height: h,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: Vec::new(),
        color: ColorTags::BT709_LIMITED,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let (w, h) = (w as usize, h as usize);
    let mut out = Mp4Writer::create(&dir.0.join("mpeg4.mp4"), &settings, None).unwrap();
    for i in 0..10 {
        let luma = (0..w * h).map(|p| ((p % w + 8 * i) % 256) as u8).collect();
        let chroma = vec![128; w * h / 4];
        out.push_video(&YuvPlanes::U8([luma, chroma.clone(), chroma]))
            .unwrap();
    }
    out.finish().unwrap()
}

/// Why `VideoDecoder` says lavapipe can't decode `path` (D14).
fn refusal(path: &Path) -> HwRefusal {
    let r = VideoDecoder::open(path, None, Some(lavapipe())).and_then(|mut d| d.next_frame());
    match r {
        Err(MediaError::HwUnsupported { refusal, .. }) => refusal,
        r => panic!("lavapipe decoded {}: {r:?}", path.display()),
    }
}

#[test]
fn a_device_that_decodes_nothing_falls_back_at_open() {
    let dir = TempDir::new("lavapipe");
    let clips = [
        clip(&dir, "h264.mp4", Codec::H264),
        clip(&dir, "hevc.mp4", Codec::Hevc),
        mpeg4_clip(&dir),
    ];
    for path in &clips {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let frames = sequential(path);
        // FFmpeg has Vulkan Video decoders for H.264 and HEVC: it offers
        // the device's format, and lavapipe, with no video queue, turns
        // it down (Withdrawn). For MPEG-4 Part 2 it has none (NotOffered).
        let refused = refusal(path);
        if name == "mpeg4.mp4" {
            assert_eq!(refused, HwRefusal::NotOffered);
        }
        let mut d = FfmpegBackend::new()
            .open_video(path, &on_lavapipe(Fallback::Software))
            .unwrap();
        let want = DecodePath::FellBack {
            from: HwKind::Vulkan,
            reason: FallbackReason::Refused(refused),
        };
        assert_eq!(*d.decode_path(), want, "{name}");
        let events = d.take_events();
        assert_eq!(
            events,
            [DecoderEvent::FellBack {
                from: HwKind::Vulkan,
                reason: FallbackReason::Refused(refused),
            }],
            "{name}"
        );
        // Software's frames, saying where they were decoded.
        let never = StopToken::never();
        let mut got = Vec::new();
        while let NextFrame::Frame(f) = d.next_frame(&never).unwrap() {
            assert_eq!(f.decoded_on, want);
            got.push(Seen::of(&f));
        }
        assert_eq!(got, frames, "{name}");
        assert!(d.take_events().is_empty());

        // With Fallback::Fail, the refusal is the error.
        match FfmpegBackend::new().open_video(path, &on_lavapipe(Fallback::Fail)) {
            Err(MediaError::HwUnsupported { kind, refusal, .. }) => {
                assert_eq!((kind.as_str(), refusal), ("Vulkan", refused), "{name}");
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn a_render_node_that_is_not_there_falls_back_with_the_reason() {
    let dir = TempDir::new("no-node");
    let path = clip(&dir, "h264.mp4", Codec::H264);
    let backend = FfmpegBackend::on_render_node("/nonexistent/renderD128");
    let req = |fallback| VideoRequest {
        decode: DecodeMode::Hardware {
            kind: HwKind::VaApi,
            device: None,
            fallback,
        },
        ..VideoRequest::default()
    };
    let mut d = backend.open_video(&path, &req(Fallback::Software)).unwrap();
    let events = d.take_events();
    let [DecoderEvent::FellBack { from, reason }] = &events[..] else {
        panic!("{events:?}");
    };
    assert_eq!(*from, HwKind::VaApi);
    let FallbackReason::DeviceUnavailable(why) = reason else {
        panic!("{reason:?}");
    };
    assert!(why.contains("/nonexistent/renderD128, as given"), "{why}");
    // Named as every other hardware message names the device.
    assert!(why.starts_with("VA-API device unavailable: "), "{why}");
    assert!(matches!(
        d.next_frame(&StopToken::never()),
        Ok(NextFrame::Frame(_))
    ));
    // The backend opens a device once: the next decoder gets the same
    // answer, and with Fallback::Fail it is the error.
    match backend.open_video(&path, &req(Fallback::Fail)) {
        Err(e @ MediaError::Hardware { .. }) => {
            assert_eq!(e.to_string(), *why);
            assert!(matches!(e, MediaError::Hardware { kind, .. } if kind == "VA-API"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        backend.render_node().path,
        Path::new("/nonexistent/renderD128")
    );
}
