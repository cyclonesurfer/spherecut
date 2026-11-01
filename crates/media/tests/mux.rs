//! `encode::mux_video_streams`: a camera's raw file of one video stream a
//! lens, in Matroska or in MP4, whose tracks keep their sources' tables.
//!
//! The sources are FFmpeg's own MP4s (`Mp4Writer`) of different lengths,
//! GOPs, rates and time bases, one with B-frames. Each output track is
//! held to its source twice: by the box reader (`mp4::track_timing`),
//! and packet by packet as FFmpeg's demuxer reads both (`PacketReader`),
//! an instrument that shares no code with it. The Matroska output's
//! packets are the sources' rounded to the millisecond, which the exact
//! comparison the MP4 output passes must catch.
//!
//! Budget (M3 plan 4.1 rule 5): 3 s for the suite, which each test
//! prints its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use spherecut_core::{FrameRate, Rational};
use spherecut_media::encode::{
    self, ColorTags, Mp4Writer, MuxedAs, PixelLayout, VideoSettings, YuvPlanes,
};
use spherecut_media::mp4::{self, Mp4Error};
use spherecut_media::packets::{NextPacket, PacketInfo, PacketReader};
use spherecut_testkit::control::must_fail;

/// The suite's budget in seconds.
const BUDGET_S: f64 = 3.0;

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

/// A source: its name, encoder, encoder options, rate, time base and
/// frames.
struct Source {
    name: &'static str,
    encoder: &'static str,
    options: &'static [(&'static str, &'static str)],
    rate: FrameRate,
    time_base: Option<Rational>,
    frames: u32,
}

/// The sources: HEVC at 29.97 with an IDR every 6 frames, in ticks of a
/// frame (a timescale of 30000); H.264 at 25 with 2 B-frames and a GOP of
/// 5, in ticks of 1/90000 s; and HEVC at 25 in ticks of a frame, which
/// FFmpeg's muxer doubles to a timescale of 12800.
fn sources() -> [Source; 3] {
    const X265: &[(&str, &str)] = &[
        ("preset", "ultrafast"),
        (
            "x265-params",
            "keyint=6:min-keyint=6:bframes=0:scenecut=0:log-level=error",
        ),
    ];
    [
        Source {
            name: "hevc-2997-gop6",
            encoder: "libx265",
            options: X265,
            rate: FrameRate::FPS_29_97,
            time_base: None,
            frames: 20,
        },
        Source {
            name: "h264-25-bframes",
            encoder: "libx264",
            options: &[("g", "5"), ("bf", "2"), ("preset", "ultrafast")],
            rate: FrameRate::FPS_25,
            time_base: Some(Rational::new(1, 90_000).unwrap()),
            frames: 13,
        },
        Source {
            name: "hevc-25-gop4",
            encoder: "libx265",
            options: &[
                ("preset", "ultrafast"),
                (
                    "x265-params",
                    "keyint=4:min-keyint=4:bframes=0:scenecut=0:log-level=error",
                ),
            ],
            rate: FrameRate::FPS_25,
            time_base: None,
            frames: 9,
        },
    ]
}

/// `src` written into `dir` by FFmpeg's MP4 muxer, 64² and each frame a
/// different grey.
fn written(dir: &Path, src: &Source) -> PathBuf {
    let settings = VideoSettings {
        encoder: src.encoder.into(),
        width: 64,
        height: 64,
        rate: src.rate,
        layout: PixelLayout::Yuv420p,
        options: src
            .options
            .iter()
            .map(|&(k, v)| (k.into(), v.into()))
            .collect(),
        color: ColorTags::BT709_LIMITED,
        edit_start: 0,
        time_base: src.time_base,
        crop_left: 0,
        device: None,
    };
    let path = dir.join(format!("{}.mp4", src.name));
    let mut w = Mp4Writer::create(&path, &settings, None).unwrap();
    for i in 0..src.frames {
        let chroma = vec![128; 32 * 32];
        w.push_video(&YuvPlanes::U8([
            vec![16 + 9 * i as u8; 64 * 64],
            chroma.clone(),
            chroma,
        ]))
        .unwrap();
    }
    w.finish().unwrap()
}

/// The sources, written once into `dir`.
fn written_sources(dir: &Path) -> Vec<PathBuf> {
    sources().iter().map(|s| written(dir, s)).collect()
}

/// The packets of stream `index` that FFmpeg's demuxer shows, and its
/// time base.
fn demuxed(path: &Path, index: usize) -> (Rational, Vec<PacketInfo>) {
    let mut r = PacketReader::open(path, index, Arc::new(AtomicBool::new(false))).unwrap();
    let tb = r.time_base();
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
    (tb, packets)
}

/// Whether two packet streams hold the same packets at the same times,
/// each time compared exactly in seconds across the two time bases, or
/// the first difference.
fn same_packets(src: &Path, out: &Path, index: usize) -> Result<(), String> {
    same_packets_but(src, out, index, false)
}

/// [`same_packets`], durations aside when `durations_differ`: an MP4's
/// are the gaps between decode times, which a Matroska source's
/// rounding leaves uneven where its own durations are the rate's.
fn same_packets_but(
    src: &Path,
    out: &Path,
    index: usize,
    durations_differ: bool,
) -> Result<(), String> {
    let (ta, a) = demuxed(src, 0);
    let (tb, b) = demuxed(out, index);
    if a.len() != b.len() {
        return Err(format!("{} packets, then {}", a.len(), b.len()));
    }
    // t·n/d in one base equals u·m/e in the other when t·n·e = u·m·d.
    let (n, d) = (i128::from(ta.num()), i128::from(ta.den()));
    let (m, e) = (i128::from(tb.num()), i128::from(tb.den()));
    let same = |t: i64, u: i64| i128::from(t) * n * e == i128::from(u) * m * d;
    let same_opt = |t: Option<i64>, u: Option<i64>| match (t, u) {
        (Some(t), Some(u)) => same(t, u),
        (t, u) => t.is_none() && u.is_none(),
    };
    for (i, (p, q)) in a.iter().zip(&b).enumerate() {
        let timed = same_opt(p.pts, q.pts)
            && same_opt(p.dts, q.dts)
            && (durations_differ || same(p.duration, q.duration));
        if !timed || p.size != q.size || p.key != q.key {
            return Err(format!(
                "packet {i}: {p:?} in {}/{}, then {q:?} in {}/{}",
                ta.num(),
                ta.den(),
                tb.num(),
                tb.den()
            ));
        }
    }
    Ok(())
}

/// `t` ticks of `tb` in milliseconds, to the nearest and halves away from
/// zero, as FFmpeg's `av_rescale_q` rounds.
fn to_ms(t: i64, tb: Rational) -> i64 {
    let (num, den) = (
        i128::from(t) * i128::from(tb.num()) * 1000,
        i128::from(tb.den()),
    );
    let half_up = (2 * num.abs() + den) / (2 * den);
    (if num < 0 { -half_up } else { half_up }) as i64
}

/// The sample entry type of track `index`'s first sample description.
fn sample_entry(path: &Path, index: usize) -> [u8; 4] {
    let tree = mp4::read_file(path).unwrap();
    let trak = tree.tracks().nth(index).unwrap();
    let stsd = trak.path(&[b"mdia", b"minf", b"stbl", b"stsd"]).unwrap();
    stsd.children[0].fourcc.0
}

#[test]
fn mp4_output_keeps_each_track_s_tables() {
    let _time = timed("mp4_output_keeps_each_track_s_tables");
    let dir = temp_dir("mux-mp4");
    let srcs = written_sources(&dir.0);
    let refs: Vec<&Path> = srcs.iter().map(PathBuf::as_path).collect();
    let out = encode::mux_video_streams_as(&refs, &dir.0.join("pair.mp4"), MuxedAs::Mp4).unwrap();

    let timings: Vec<_> = srcs
        .iter()
        .map(|s| mp4::track_timing(s, 0).unwrap())
        .collect();
    // The sources differ where the output must keep each apart.
    assert_eq!(
        timings.iter().map(|t| t.timescale).collect::<Vec<_>>(),
        [30_000, 90_000, 12_800]
    );
    assert_eq!(
        timings.iter().map(|t| t.count).collect::<Vec<_>>(),
        [20, 13, 9]
    );
    assert_eq!(timings[0].stss.as_deref(), Some(&[1, 7, 13, 19][..]));
    assert_eq!(timings[2].stss.as_deref(), Some(&[1, 5, 9][..]));
    assert!(timings[1].ctts_hash.is_some(), "{:?}", timings[1]);
    assert!(timings[0].ctts_hash.is_none(), "{:?}", timings[0]);

    for (k, (src, timing)) in srcs.iter().zip(&timings).enumerate() {
        assert_eq!(&mp4::track_timing(&out, k).unwrap(), timing, "track {k}");
        same_packets(src, &out, k).unwrap();
    }
    assert_eq!(sample_entry(&out, 0), *b"hvc1");
    assert_eq!(sample_entry(&out, 1), *b"avc1");
    assert_eq!(sample_entry(&out, 2), *b"hvc1");

    // A source's own time base under 10000 is doubled by the MP4 muxer
    // until it isn't, which keeps each time exact: Matroska's 1/1000 s
    // comes out in ticks of 1/16000 s.
    let mkv = encode::remux_to_matroska(&srcs[0], &dir.0.join("lens.mkv")).unwrap();
    let again =
        encode::mux_video_streams_as(&[&mkv], &dir.0.join("again.mp4"), MuxedAs::Mp4).unwrap();
    assert_eq!(mp4::track_timing(&again, 0).unwrap().timescale, 16_000);
    same_packets_but(&mkv, &again, 0, true).unwrap();

    // Its control: the comparison tells the tracks apart, so a mux that
    // put one source in two tracks would fail it.
    must_fail(
        "track 1 held to track 0's source",
        same_packets(&srcs[0], &out, 1),
    );
    let other = mp4::track_timing(&out, 1).unwrap();
    must_fail(
        "track 1's tables are track 0's source's",
        other == timings[0],
    );
}

#[test]
fn matroska_output_rounds_to_the_millisecond() {
    let _time = timed("matroska_output_rounds_to_the_millisecond");
    let dir = temp_dir("mux-mkv");
    let srcs = written_sources(&dir.0);
    let refs: Vec<&Path> = srcs.iter().map(PathBuf::as_path).collect();
    let out =
        encode::mux_video_streams_as(&refs, &dir.0.join("pair.mkv"), MuxedAs::Matroska).unwrap();
    // As before the MP4 output: every packet in its order, of its size
    // and its kind, at its source's time rounded to the millisecond.
    for (k, src) in srcs.iter().enumerate() {
        let (ta, a) = demuxed(src, 0);
        let (tb, b) = demuxed(&out, k);
        assert_eq!((tb.num(), tb.den()), (1, 1000));
        assert_eq!(a.len(), b.len(), "track {k}");
        let ms = |t: i64| to_ms(t, ta);
        for (p, q) in a.iter().zip(&b) {
            assert_eq!((p.size, p.key), (q.size, q.key), "track {k}");
            assert_eq!(p.pts.map(ms), q.pts, "track {k}: {p:?} then {q:?}");
            // Matroska keeps presentation times alone. Without B-frames
            // FFmpeg's demuxer takes the decode times from them; with
            // them, it guesses.
            if k != 1 {
                assert_eq!(p.dts.map(ms), q.dts, "track {k}: {p:?} then {q:?}");
            }
        }
    }
    assert!(mp4::track_timing(&out, 0).is_err());

    // The control: the comparison the MP4 output passes fails Matroska's
    // rounded times, for each track whose frames aren't whole
    // milliseconds (29.97's 33.367 ms), so a return to Matroska would
    // fail it. Its first packet, at 0, differs in its duration alone.
    let why = must_fail(
        "Matroska kept 29.97's durations",
        same_packets(&srcs[0], &out, 0),
    );
    assert!(why.starts_with("packet 0:"), "{why}");
    let why = must_fail(
        "Matroska kept 29.97's times",
        same_packets_but(&srcs[0], &out, 0, true),
    );
    assert!(why.starts_with("packet 1:"), "{why}");
}

#[test]
fn the_destination_s_extension_picks_the_container() {
    let _time = timed("the_destination_s_extension_picks_the_container");
    for (name, muxed) in [
        ("a.mkv", MuxedAs::Matroska),
        ("a.MKV", MuxedAs::Matroska),
        ("a.mk3d", MuxedAs::Matroska),
        ("a.mka", MuxedAs::Matroska),
        ("a.WebM", MuxedAs::Matroska),
        ("a.mp4", MuxedAs::Mp4),
        ("a.mov", MuxedAs::Mp4),
        ("VID_20261022_165451_00_004.insv", MuxedAs::Mp4),
        ("a.360", MuxedAs::Mp4),
        ("a.mkv.part", MuxedAs::Mp4),
        ("mkv", MuxedAs::Mp4),
        ("a", MuxedAs::Mp4),
    ] {
        assert_eq!(MuxedAs::for_path(Path::new(name)), muxed, "{name}");
    }

    let dir = temp_dir("mux-ext");
    let src = written(&dir.0, &sources()[0]);
    let insv = encode::mux_video_streams(&[&src, &src], &dir.0.join("lenses.insv")).unwrap();
    let mkv = encode::mux_video_streams(&[&src, &src], &dir.0.join("lenses.MKV")).unwrap();
    for k in 0..2 {
        same_packets(&src, &insv, k).unwrap();
    }
    let ebml = std::fs::read(&mkv).unwrap();
    assert_eq!(ebml[..4], [0x1a, 0x45, 0xdf, 0xa3]);
    // Its control: the `.MKV` file is no MP4, and its times are rounded.
    let e = must_fail("lenses.MKV read as an MP4", mp4::track_timing(&mkv, 0));
    assert!(!matches!(e, Mp4Error::NoTrack { .. }), "{e:?}");
    must_fail("lenses.MKV kept 29.97's times", same_packets(&src, &mkv, 0));
}
