//! Two-track files (M3 plan 7.2 and section 9): the perf clips' specs and
//! a short, small version of each coded as the clip is, and the 2×64²
//! files the frame lock's tests read, each back through `track_timing`.
//!
//! The perf clips themselves take hours, so each is coded here at 192²
//! for 121 frames, at its bits a pixel, and its NAL types, GOP,
//! timing and bitrate a track are read back from the file: the types from
//! the slices' headers, an instrument that shares no code with the
//! writer. Whether x265 can mark every other P frame non-reference, as
//! the X5 does at 59.94, is checked on its output too.
//!
//! Budget (M3 plan 4.1 rule 5): 15 s for the suite, which each test
//! prints its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use spherecut_core::{FrameRate, StopToken};
use spherecut_media::backend::{FfmpegBackend, MediaBackend, NextFrame, VideoRequest};
use spherecut_media::encode::{Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
use spherecut_media::mp4::{self, TrackTiming};
use spherecut_synth::code::{CornerCode, Luma, read_corner_code};
use spherecut_synth::media::{
    self, Audio, Codec, LENS_COLOR, LensTrack, PairSpec, Spec, hevc_picture_types, track_bytes,
};
use spherecut_synth::yuv::{self, Chroma, YuvFormat};
use spherecut_synth::{Pattern, render_equirect};
use spherecut_testkit::control::{at_least, at_most, must_fail};

/// The suite's budget in seconds.
const BUDGET_S: f64 = 15.0;

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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// HEVC NAL unit types (H.265 table 7-1).
const TRAIL_N: u8 = 0;
const TRAIL_R: u8 = 1;
const TSA_N: u8 = 2;
const IDR_W_RADL: u8 = 19;
const IDR_N_LP: u8 = 20;

/// Whether `spec` codes a lens as the X5 does in every mode, in what
/// x265 can match: square 8-bit HEVC in full range BT.709, closed GOPs
/// without B-frames, at an average bitrate, with noise, and no audio or
/// metadata of its own. x265 can't match the masters' slices, WPP or
/// IDR type (`perf_pairs`' doc).
fn x5_coding(spec: &Spec) -> Result<(), String> {
    let gop = spec.gop.ok_or("no GOP of its own")?;
    let checks = [
        (spec.width == spec.height, "square"),
        (spec.codec == Codec::Hevc, "HEVC"),
        (spec.bits == 8, "8-bit"),
        (spec.chroma == Chroma::Yuv420, "4:2:0"),
        (spec.color == LENS_COLOR, "full range BT.709"),
        (!gop.open && gop.radl == 0, "closed GOPs"),
        (gop.bframes == 0, "no B-frames"),
        (spec.bitrate_kbps.is_some(), "one-pass ABR"),
        (spec.noise.is_some(), "noise"),
        (spec.audio == Audio::None && !spec.spherical, "video alone"),
        (
            spec.edit_start == 0 && !spec.vfr,
            "every frame shown at its rate",
        ),
    ];
    match checks.iter().find(|(ok, _)| !ok) {
        Some((_, what)) => Err(format!("{} isn't {what}", spec.name)),
        None => Ok(()),
    }
}

/// The least share of samples in which two lenses' noise must differ.
const NOISE_APART: f64 = 0.75;

/// The share of the samples, over every plane, in which the noise of
/// frame `frame` of `a` and of `b` differs: each spec's noise is its frame
/// at 64² without corner codes less the same frame without noise.
fn noise_apart(a: &Spec, b: &Spec, frame: u32) -> f64 {
    let samples = |s: &Spec| match media::frame_planes(s, None, frame).unwrap() {
        YuvPlanes::U8(p) => p.concat(),
        YuvPlanes::U16(_) => panic!("{} isn't 8-bit", s.name),
    };
    let noise = |s: &Spec| -> Vec<i16> {
        let small = Spec {
            width: 64,
            height: 64,
            ..s.clone()
        };
        let quiet = Spec {
            noise: None,
            ..small.clone()
        };
        samples(&small)
            .iter()
            .zip(samples(&quiet))
            .map(|(n, q)| i16::from(*n) - i16::from(q))
            .collect()
    };
    let (a, b) = (noise(a), noise(b));
    assert_eq!(a.len(), b.len());
    a.iter().zip(&b).filter(|(x, y)| x != y).count() as f64 / a.len() as f64
}

#[test]
fn the_perf_pairs_are_coded_as_the_x5_codes_each_mode() {
    let _time = timed("the_perf_pairs_are_coded_as_the_x5_codes_each_mode");
    let pairs = media::perf_pairs();
    // (name, side, rate, IDR interval, kbps a lens)
    let modes = [
        (
            media::PERF_PAIR_3840_2997,
            3840,
            FrameRate::FPS_29_97,
            60,
            65_000,
        ),
        (
            media::PERF_PAIR_2880_5994,
            2880,
            FrameRate::FPS_59_94,
            120,
            80_000,
        ),
        (
            media::PERF_PAIR_2880_2997,
            2880,
            FrameRate::FPS_29_97,
            60,
            80_000,
        ),
    ];
    assert_eq!(pairs.len(), modes.len());
    for (pair, (name, side, rate, keyint, kbps)) in pairs.iter().zip(modes) {
        assert_eq!(pair.name, name);
        assert_eq!(pair.file_name(), format!("{name}.mp4"));
        assert!(pair.corner_codes && pair.threaded, "{name}");
        assert_ne!(pair.lenses[0].name, pair.lenses[1].name);
        for lens in &pair.lenses {
            x5_coding(lens).unwrap();
            assert_eq!(
                (lens.width, lens.rate, lens.frames),
                (side, rate, 600),
                "{name}"
            );
            assert_eq!(lens.gop.map(|g| g.keyint), Some(keyint), "{name}");
            assert_eq!(lens.bitrate_kbps, Some(kbps), "{name}");
        }
    }
    // Each lens has noise of its own: the noise of a frame of one lens
    // differs from the same frame's of the other in most samples if the
    // two are drawn apart (7 in 8 where neither clamps), and in none if
    // one is the other's.
    for pair in &pairs {
        for frame in [0, 1, 599] {
            let apart = noise_apart(&pair.lenses[0], &pair.lenses[1], frame);
            eprintln!(
                "{} frame {frame}: the lenses' noise differs in {apart:.3} of the samples",
                pair.name
            );
            assert_eq!(
                at_least(apart, NOISE_APART).passes(),
                Some(true),
                "{} frame {frame}: {apart}",
                pair.name
            );
        }
    }
    // Its control: seeds that differ only in their lowest bit give the
    // same noise, as the lenses' seeds `seed` and `seed + 1` once did.
    let lens = &pairs[0].lenses[0];
    let twin = Spec {
        noise: lens.noise.map(|s| s ^ 1),
        ..lens.clone()
    };
    must_fail(
        "seeds a bit apart taken for noise of their own",
        at_least(noise_apart(lens, &twin, 0), NOISE_APART),
    );

    // Its control: M2's perf clips, open GOPs of 30 with 3 B-frames, are
    // not coded as the X5 codes.
    let m2 = &media::perf_clips()[0];
    let why = must_fail("M2's perf clip taken for the X5's coding", x5_coding(m2));
    assert!(why.ends_with("isn't square"), "{why}");
    let square = Spec {
        height: m2.width,
        color: LENS_COLOR,
        spherical: false,
        ..m2.clone()
    };
    let why = must_fail("M2's coding taken for the X5's", x5_coding(&square));
    assert!(why.ends_with("isn't closed GOPs"), "{why}");
}

/// Whether a track's pictures are as the X5 codes them without
/// non-reference frames: an IDR at every `keyint`th frame and nowhere
/// else, every other picture a trailing reference picture (TRAIL_R), and
/// none decoded out of the order it is shown in.
fn p_only(types: &[u8], timing: &TrackTiming, keyint: u32) -> Result<(), String> {
    if timing.ctts_hash.is_some() {
        return Err("its pictures are reordered: the track has composition offsets".into());
    }
    for (i, &t) in types.iter().enumerate() {
        let idr = i % keyint as usize == 0;
        let ok = if idr {
            matches!(t, IDR_W_RADL | IDR_N_LP)
        } else {
            t == TRAIL_R
        };
        if !ok {
            return Err(format!("picture {i} has NAL type {t}"));
        }
    }
    let n = types.len() as u32;
    let stss: Vec<u32> = (0..n).step_by(keyint as usize).map(|i| i + 1).collect();
    if timing.stss.as_deref() != Some(&stss[..]) {
        return Err(format!("sync samples {:?}, not {stss:?}", timing.stss));
    }
    Ok(())
}

/// A clip of x265's own at 256², 59.94 fps, with `params` added: what it
/// codes when asked for temporal layers.
fn x265_clip(dir: &Path, name: &str, params: &str) -> PathBuf {
    let settings = VideoSettings {
        encoder: "libx265".into(),
        width: 256,
        height: 256,
        rate: FrameRate::FPS_59_94,
        layout: PixelLayout::Yuv420p,
        options: vec![
            ("preset".into(), "veryfast".into()),
            ("threads".into(), "1".into()),
            (
                "x265-params".into(),
                format!("keyint=24:min-keyint=24:scenecut=0:open-gop=0:log-level=error:{params}"),
            ),
        ],
        color: LENS_COLOR,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
        device: None,
    };
    let path = dir.join(format!("{name}.mp4"));
    let mut w = Mp4Writer::create(&path, &settings, None).unwrap();
    let format = YuvFormat::tagged(&LENS_COLOR, Chroma::Yuv420);
    for i in 0..48 {
        let img = render_equirect(Pattern::Grid, 256, 256, i, 1);
        w.push_video(&yuv::to_yuv(&img, 8, &format)).unwrap();
    }
    w.finish().unwrap()
}

#[test]
fn short_perf_pairs_keep_their_nal_types_gops_and_bitrates() {
    let _time = timed("short_perf_pairs_keep_their_nal_types_gops_and_bitrates");
    let dir = temp_dir("pairs-perf");
    // Each pair's lens 0: its bitrate and its target, in kbit/s.
    let mut rates = Vec::new();
    for full in media::perf_pairs() {
        let keyint = full.lenses[0].gop.unwrap().keyint;
        // Two GOPs and a frame at 29.97, one and a frame at 59.94: long
        // enough for x265's rate control to settle near its target.
        let frames = 121;
        let spec = full.scaled(192, frames);
        let began = Instant::now();
        let path = media::write_pair(&spec, &dir.0).unwrap();
        let made = began.elapsed().as_secs_f64();
        let mut timings = Vec::new();
        for (k, lens) in spec.lenses.iter().enumerate() {
            let types = hevc_picture_types(&path, k).unwrap();
            let timing = mp4::track_timing(&path, k).unwrap();
            assert_eq!(types.len() as u32, frames, "{}", spec.name);
            p_only(&types, &timing, keyint).unwrap();
            let rate = lens.rate.as_rational();
            assert_eq!(timing.timescale as i64, rate.num(), "{}", spec.name);
            assert_eq!(timing.stts, [(frames, rate.den() as u32)], "{}", spec.name);
            // The bitrate a track, against the full clip's scaled to 192²
            // by pixels.
            let seconds = f64::from(frames) / lens.rate.to_f64();
            let kbps = track_bytes(&path, k).unwrap() as f64 * 8.0 / seconds / 1000.0;
            let want = f64::from(lens.bitrate_kbps.unwrap());
            eprintln!(
                "{} lens {k}: {frames} frames in {made:.1} s, IDR every {keyint}, NAL types {:?}, \
                 {kbps:.0} kbit/s against {want:.0} (the full clip's {} kbit/s scaled)",
                spec.name,
                count_types(&types),
                full.lenses[k].bitrate_kbps.unwrap()
            );
            let off = (kbps / want - 1.0).abs();
            assert_eq!(
                at_most(off, 0.2).passes(),
                Some(true),
                "{}: {kbps} kbit/s",
                spec.name
            );
            if k == 0 {
                rates.push((kbps, want));
            }
            timings.push(timing);
        }
        assert_eq!(timings[0].stts_mismatch(&timings[1]), None);
        assert_eq!(timings[0].stss_mismatch(&timings[1]), None);
        assert_eq!(timings[0].first_pts, timings[1].first_pts);
    }

    // The bitrate's control: 2×3840²'s 65 Mbps is fewer bits a pixel
    // than 2×2880²'s 80 Mbps by more than half, and the check tells them
    // apart.
    let (kbps_3840, want_2880) = (rates[0].0, rates[2].1);
    must_fail(
        "2×3840²'s bitrate taken for 2×2880²'s",
        at_most((kbps_3840 / want_2880 - 1.0).abs(), 0.2),
    );

    // Whether x265 can mark every other frame non-reference without
    // reordering, as the X5's TSA_N at 59.94: its temporal layers put the
    // non-reference B-frames alone in the upper layer, so without
    // B-frames every frame is still a reference.
    let layered = x265_clip(&dir.0, "layered-p", "bframes=0:temporal-layers=1");
    let types = hevc_picture_types(&layered, 0).unwrap();
    let timing = mp4::track_timing(&layered, 0).unwrap();
    assert!(
        !types.iter().any(|t| matches!(*t, TRAIL_N | TSA_N)),
        "{types:?}"
    );
    p_only(&types, &timing, 24).unwrap();
    eprintln!(
        "x265 asked for temporal layers without B-frames: {:?}",
        count_types(&types)
    );

    // Its control: with a B-frame between references, the upper layer's
    // pictures are non-reference TSA_N, as the X5's, but each is decoded
    // after the frame shown after it, which the X5 never does, so the
    // check fails.
    let b = x265_clip(
        &dir.0,
        "layered-b",
        "bframes=1:b-adapt=0:b-pyramid=0:temporal-layers=1",
    );
    let types = hevc_picture_types(&b, 0).unwrap();
    let timing = mp4::track_timing(&b, 0).unwrap();
    eprintln!(
        "x265 asked for temporal layers with a B-frame: {:?}",
        count_types(&types)
    );
    assert!(types.contains(&TSA_N), "{types:?}");
    let why = must_fail(
        "x265's TSA_N taken for the X5's",
        p_only(&types, &timing, 24),
    );
    assert!(why.contains("reordered"), "{why}");
}

/// How many pictures there are of each NAL type, by type.
fn count_types(types: &[u8]) -> Vec<(u8, usize)> {
    let mut counts: Vec<(u8, usize)> = Vec::new();
    for &t in types {
        match counts.iter_mut().find(|(u, _)| *u == t) {
            Some((_, n)) => *n += 1,
            None => counts.push((t, 1)),
        }
    }
    counts.sort();
    counts
}

/// The corner code of every frame of track `k` of `path`, in order.
fn codes(path: &Path, k: usize) -> Vec<CornerCode> {
    let never = StopToken::never();
    let req = VideoRequest {
        stream: Some(k),
        ..VideoRequest::default()
    };
    let mut dec = FfmpegBackend::default().open_video(path, &req).unwrap();
    let mut out = Vec::new();
    loop {
        match dec.next_frame(&never).unwrap() {
            NextFrame::Frame(f) => {
                out.push(read_corner_code(&Luma::of_picture(&*f).unwrap()).unwrap())
            }
            NextFrame::End => return out,
            NextFrame::Stopped => panic!("stopped"),
        }
    }
}

#[test]
fn the_64_pairs_come_back_through_track_timing_as_specified() {
    let _time = timed("the_64_pairs_come_back_through_track_timing_as_specified");
    let dir = temp_dir("pairs-64");
    let pairs = media::pairs_64();
    let names: Vec<&str> = pairs.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            media::PAIR_64_MATCHED,
            media::PAIR_64_GOP_FAULT,
            media::PAIR_64_LENS_SHORT
        ]
    );
    let tracks = |p: &PairSpec| {
        p.lenses.clone().map(|l| LensTrack {
            frames: l.frames,
            keyint: l.gop.unwrap().keyint,
        })
    };
    let track = |frames, keyint| LensTrack { frames, keyint };
    assert_eq!(tracks(&pairs[0]), [track(30, 10), track(30, 10)]);
    assert_eq!(tracks(&pairs[1]), [track(30, 8), track(30, 10)]);
    assert_eq!(tracks(&pairs[2]), [track(30, 10), track(29, 10)]);

    let mut timings = Vec::new();
    for pair in &pairs {
        let path = media::write_pair(pair, &dir.0).unwrap();
        assert!(!dir.0.join(format!(".{}-lenses", pair.name)).exists());
        let mut both = Vec::new();
        for (k, t) in tracks(pair).iter().enumerate() {
            let timing = mp4::track_timing(&path, k).unwrap();
            let stss: Vec<u32> = (0..t.frames)
                .step_by(t.keyint as usize)
                .map(|i| i + 1)
                .collect();
            assert_eq!(timing.timescale, 30_000, "{}", pair.name);
            assert_eq!(timing.count, u64::from(t.frames), "{}", pair.name);
            assert_eq!(timing.stts, [(t.frames, 1001)], "{}", pair.name);
            assert_eq!(timing.stss, Some(stss), "{}", pair.name);
            assert_eq!(timing.ctts_hash, None, "{}", pair.name);
            assert_eq!(
                (timing.first_shown, timing.first_pts),
                (0, 0),
                "{}",
                pair.name
            );
            let types = hevc_picture_types(&path, k).unwrap();
            p_only(&types, &timing, t.keyint).unwrap();
            let want: Vec<CornerCode> = (0..t.frames).map(|i| CornerCode::of(k, i)).collect();
            assert_eq!(codes(&path, k), want, "{}", pair.name);
            both.push(timing);
        }
        timings.push(both);
    }
    let [matched, gop_fault, short] = &timings[..] else {
        unreachable!()
    };
    assert_eq!(matched[0], matched[1]);
    assert_eq!(matched[0].stts_mismatch(&matched[1]), None);
    assert_eq!(matched[0].stss_mismatch(&matched[1]), None);
    // A frame short: the tables agree over the frames both lenses have.
    assert_eq!(short[0].stts_mismatch(&short[1]), None);
    assert_eq!(short[0].stss_mismatch(&short[1]), None);
    assert_eq!(short[0].count - short[1].count, 1);
    // GOPs of 8 and 10: the first IDR one has and the other hasn't is
    // lens 0's second, sample 8.
    assert_eq!(gop_fault[0].stts_mismatch(&gop_fault[1]), None);
    assert_eq!(gop_fault[0].stss_mismatch(&gop_fault[1]), Some(8));

    // Its control: the GOP fault's sync samples don't agree.
    must_fail(
        "the GOP fault's sync samples agree",
        gop_fault[0].stss_mismatch(&gop_fault[1]).is_none(),
    );
}

/// A pair's lenses go by their own names while it is written, and a
/// pair that would give them one, or audio, is refused.
#[test]
fn a_pair_that_cant_be_written_says_why() {
    let _time = timed("a_pair_that_cant_be_written_says_why");
    let dir = temp_dir("pairs-bad");
    let mut same = media::pair_64(
        "same",
        [LensTrack {
            frames: 2,
            keyint: 2,
        }; 2],
    );
    same.lenses[1].name = same.lenses[0].name.clone();
    let e = media::write_pair(&same, &dir.0).unwrap_err();
    assert!(e.to_string().contains("names of their own"), "{e}");
    let mut loud = media::pair_64(
        "loud",
        [LensTrack {
            frames: 2,
            keyint: 2,
        }; 2],
    );
    loud.lenses[0].audio = Audio::Stereo;
    let e = media::write_pair(&loud, &dir.0).unwrap_err();
    assert!(e.to_string().contains("no audio"), "{e}");
    // A lens that isn't square has no corner codes, and nothing is left.
    let mut wide = media::pair_64(
        "wide",
        [LensTrack {
            frames: 2,
            keyint: 2,
        }; 2],
    );
    wide.lenses[1].width = 128;
    let e = media::write_pair(&wide, &dir.0).unwrap_err();
    assert!(e.to_string().contains("isn't square"), "{e}");
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
}
