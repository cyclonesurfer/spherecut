//! End-to-end checks on generated media: what we write, FFmpeg and our own
//! parsers read back exactly.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use spherecut_core::{FrameRate, Rational};
use spherecut_media::detect::Layout;
use spherecut_media::encode::YuvPlanes;
use spherecut_media::ff;
use spherecut_media::probe::{self, StreamKind};
use spherecut_synth::media::{self, Audio, Codec, Spec};
use spherecut_synth::{Pattern, decode_frame_code, yuv};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("spherecut-synth-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn small(name: &str, codec: Codec, bits: u32, audio: Audio, spherical: bool) -> Spec {
    Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width: 512,
        height: 256,
        rate: FrameRate::FPS_29_97,
        frames: 6,
        codec,
        bits,
        audio,
        spherical,
    }
}

/// Decode every video frame in software and return them as planes.
// Reads a file this test wrote with ffmpeg-next alone, apart from
// Spherecut's confined open (D24).
#[allow(clippy::disallowed_methods)]
fn decode_all(path: &Path) -> Vec<(i64, YuvPlanes, u32, u32, u32)> {
    spherecut_media::init();
    let mut ictx = ff::format::input(path).unwrap();
    let st = ictx.streams().best(ff::media::Type::Video).unwrap();
    let idx = st.index();
    let mut dec = ff::codec::context::Context::from_parameters(st.parameters())
        .unwrap()
        .decoder()
        .video()
        .unwrap();
    let mut out = Vec::new();
    let mut frame = ff::frame::Video::empty();
    let mut grab = |dec: &mut ff::decoder::Video, out: &mut Vec<_>| {
        while dec.receive_frame(&mut frame).is_ok() {
            let (w, h) = (frame.width(), frame.height());
            let ten = frame.format() == ff::format::Pixel::YUV420P10LE;
            let planes = (0..3)
                .map(|i| {
                    let (pw, ph) = if i == 0 { (w, h) } else { (w / 2, h / 2) };
                    let stride = frame.stride(i);
                    let data = frame.data(i);
                    (0..ph as usize)
                        .flat_map(|y| {
                            let row = &data[y * stride..];
                            (0..pw as usize).map(move |x| {
                                if ten {
                                    u16::from_le_bytes([row[2 * x], row[2 * x + 1]])
                                } else {
                                    row[x] as u16
                                }
                            })
                        })
                        .collect::<Vec<u16>>()
                })
                .collect::<Vec<_>>();
            let [y, u, v]: [Vec<u16>; 3] = planes.try_into().unwrap();
            let p = if ten {
                YuvPlanes::U16([y, u, v])
            } else {
                YuvPlanes::U8([y, u, v].map(|p| p.into_iter().map(|x| x as u8).collect()))
            };
            out.push((frame.pts().unwrap_or(-1), p, w, h, if ten { 10 } else { 8 }));
        }
    };
    for (s, pkt) in ictx.packets() {
        if s.index() == idx {
            dec.send_packet(&pkt).unwrap();
            grab(&mut dec, &mut out);
        }
    }
    dec.send_eof().unwrap();
    grab(&mut dec, &mut out);
    out
}

#[test]
fn lossless_hevc10_round_trips_frames_timing_and_metadata() {
    let dir = tmp("lossless");
    let spec = small(
        "t-lossless",
        Codec::HevcLossless,
        10,
        Audio::AmbiX {
            azimuth: 45,
            elevation: 30,
        },
        true,
    );
    let path = media::write(&spec, &dir).unwrap();

    let r = probe::probe(&path).unwrap();
    let v = r
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .unwrap();
    assert_eq!(v.codec.as_deref(), Some("hevc"));
    let vi = v.video.as_ref().unwrap();
    assert_eq!((vi.width, vi.height, vi.bit_depth), (512, 256, Some(10)));
    assert_eq!(v.avg_frame_rate, Some(Rational::new(30000, 1001).unwrap()));
    assert_eq!(v.frames, Some(6));
    assert_eq!(vi.color.matrix.as_deref(), Some("bt709"));
    assert_eq!(vi.color.range.as_deref(), Some("tv"));
    assert!(v.spherical.v2.is_some(), "sv3d/st3d missing");
    let a = r
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Audio)
        .unwrap();
    assert_eq!(a.audio.as_ref().unwrap().channels, 4);
    assert!(a.spherical.spatial_audio.as_ref().unwrap().is_ambix());
    assert_eq!(r.camera.as_ref().unwrap().layout, Layout::EquirectTagged);

    // Lossless: every decoded frame matches what we encoded, and carries
    // its own frame number.
    let frames = decode_all(&path);
    assert_eq!(frames.len(), 6);
    for (i, (_, planes, w, h, bits)) in frames.iter().enumerate() {
        assert_eq!(*bits, 10);
        let src = yuv::to_yuv420(
            &spherecut_synth::render_equirect(Pattern::Grid, 512, 256, i as u32, 2),
            10,
        );
        match (planes, &src) {
            (YuvPlanes::U16(a), YuvPlanes::U16(b)) => assert!(a == b, "frame {i} differs"),
            _ => panic!("bit depth mismatch"),
        }
        let img = yuv::from_yuv420(planes, *w, *h, 10);
        assert_eq!(decode_frame_code(&img), Some(i as u32));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lossy_h264_untagged_is_flagged_as_likely_equirect() {
    let dir = tmp("untagged");
    let path = media::write(
        &small("t-untagged", Codec::H264, 8, Audio::Stereo, false),
        &dir,
    )
    .unwrap();
    let r = probe::probe(&path).unwrap();
    assert_eq!(r.camera.as_ref().unwrap().layout, Layout::EquirectLikely);
    assert!(r.streams.iter().all(|s| s.spherical.is_empty()));
    // Frame codes survive lossy coding.
    for (i, (_, planes, w, h, bits)) in decode_all(&path).iter().enumerate() {
        let img = yuv::from_yuv420(planes, *w, *h, *bits);
        assert_eq!(decode_frame_code(&img), Some(i as u32));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn broken_files_give_errors_not_panics() {
    let dir = tmp("bad");
    let good = media::write(&small("t-good", Codec::H264, 8, Audio::Stereo, true), &dir).unwrap();
    let r = probe::probe(&good).unwrap();
    assert!(r.damage.is_empty(), "{:?}", r.damage);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(r.ffmpeg_log.is_empty(), "{:?}", r.ffmpeg_log);
    let bad = media::write_bad_files(&good, &dir).unwrap();
    assert_eq!(bad.len(), 12);
    // (file, why, its code in the CLI's JSON)
    let reasons = [
        ("bad-empty.mp4", "the file is empty", "not_media"),
        ("bad-text.mp4", "not a recognised media file", "not_media"),
        (
            "bad-trunc-header.mp4",
            "`ftyp` box at offset 0",
            "truncated",
        ),
        ("bad-trunc-half.mp4", "`mdat` box at offset", "truncated"),
        ("bad-trunc-end.mp4", "`moov` box at offset", "truncated"),
        (
            "bad-corrupt-moov.mp4",
            "runs past the end of the file",
            "damaged",
        ),
        (
            "bad-trak-type.mp4",
            "has a type that is not text, so its header is damaged",
            "damaged",
        ),
        ("bad-trak-renamed.mp4", "box `Trak` at offset", "damaged"),
        (
            "bad-timescale.mp4",
            "track #0: its `mdhd` gives timescale 0",
            "damaged",
        ),
        (
            "bad-codec-config.mp4",
            "stream #0 (sample entry `avc1`) has no pixel format",
            "damaged",
        ),
        (
            "bad-audio-config.mp4",
            "stream #1 (sample entry `mp4a`) cannot be opened by FFmpeg's aac decoder",
            "damaged",
        ),
        (
            "bad-projection.mp4",
            "in `avc1` has no `proj`, so FFmpeg ignores its projection",
            "damaged",
        ),
    ];
    for b in &bad {
        let name = b.file_name().unwrap().to_string_lossy().into_owned();
        // Either FFmpeg cannot open the file and the error says why, or it
        // can (late truncation, corrupt tables) and the damage is reported.
        let (why, code) = match probe::probe(b) {
            Err(e) => (e.to_string(), e.code()),
            Ok(r) => {
                assert!(!r.damage.is_empty(), "{name} probed cleanly");
                let failure = r.failure().unwrap();
                let damage: Vec<&str> = r.damage.iter().map(|d| d.message.as_str()).collect();
                (damage.join("; "), failure.code)
            }
        };
        let (_, want, want_code) = reasons.iter().find(|(n, ..)| *n == name).unwrap();
        assert!(why.contains(want), "{name}: {why}");
        assert_eq!(code, *want_code, "{name}: {why}");
    }
    // FFmpeg opens these without complaint; only probe's checks find the
    // damage.
    for name in [
        "bad-trunc-end.mp4",
        "bad-corrupt-moov.mp4",
        "bad-trak-type.mp4",
        "bad-trak-renamed.mp4",
        "bad-timescale.mp4",
        "bad-codec-config.mp4",
        "bad-audio-config.mp4",
        "bad-projection.mp4",
    ] {
        let r = probe::probe(&dir.join(name));
        assert!(r.is_ok_and(|r| !r.damage.is_empty()), "{name}");
    }
    // The stream looks set up: FFmpeg took the sample rate and channels
    // from the sample entry.
    let r = probe::probe(&dir.join("bad-audio-config.mp4")).unwrap();
    let audio = r.streams.iter().find_map(|s| s.audio.as_ref()).unwrap();
    assert!(
        audio.channels > 0 && audio.sample_fmt.is_some(),
        "{audio:?}"
    );
    // The audio track is gone from what FFmpeg reads, whether its type is
    // still text or not.
    for name in ["bad-trak-type.mp4", "bad-trak-renamed.mp4"] {
        let r = probe::probe(&dir.join(name)).unwrap();
        assert!(r.streams.iter().all(|s| s.kind != StreamKind::Audio));
    }
    // FFmpeg reads no projection from an `sv3d` without `proj`, and says so
    // only in its log.
    let r = probe::probe(&dir.join("bad-projection.mp4")).unwrap();
    assert!(
        r.ffmpeg_log
            .iter()
            .any(|l| l.contains("] Missing projection box")),
        "{:?}",
        r.ffmpeg_log
    );
    // FFmpeg's own word on it is kept, apart from the damage.
    let r = probe::probe(&dir.join("bad-timescale.mp4")).unwrap();
    assert!(
        r.ffmpeg_log
            .iter()
            .any(|l| l.contains("] Invalid mdhd time scale 0")),
        "{:?}",
        r.ffmpeg_log
    );
    // Damage inside the `moov` FFmpeg wrote: bytes changed anywhere, or
    // words of the sample tables of both tracks set to small or arbitrary
    // values. Tests build with overflow checks, so arithmetic on the
    // damaged values that would wrap panics here.
    use spherecut_media::mp4;
    let mut bytes = std::fs::read(&good).unwrap();
    let tree = mp4::read(&mut std::io::Cursor::new(&bytes)).unwrap();
    // The audio track's type, or its size and type, zeroed: FFmpeg skips
    // it as it does any box of a type it does not know.
    let audio = tree.tracks().last().unwrap().offset as usize;
    for zeroed in [audio + 4..audio + 8, audio..audio + 8] {
        let mut f = bytes.clone();
        f[zeroed].fill(0);
        let path = dir.join("zero-trak.mp4");
        std::fs::write(&path, &f).unwrap();
        let r = probe::probe(&path).unwrap();
        assert!(r.streams.iter().all(|s| s.kind != StreamKind::Audio));
        assert!(
            r.damage
                .iter()
                .any(|d| d.message.contains("in `moov` has a type that is not text")),
            "{:?}",
            r.damage
        );
    }
    let moov = tree.moov().unwrap();
    // (start, length) of `moov` and of each sample table's entries.
    let mut ranges = vec![(moov.offset as usize, moov.size as usize)];
    for trak in tree.tracks() {
        let stbl = trak.path(&[b"mdia", b"minf", b"stbl"]).unwrap();
        for b in &stbl.children {
            if [b"stts", b"stsz", b"stsc", b"stco"].contains(&&b.fourcc.0) {
                ranges.push(((b.offset + b.header_len) as usize + 4, b.payload.len() - 4));
            }
        }
    }
    assert_eq!(ranges.len(), 9);
    let mut seed = 0x9e37_79b9_u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut detected = 0;
    for _ in 0..4000 {
        let mut edits = Vec::new();
        for _ in 0..1 + next() % 3 {
            let (start, len) = ranges[next() as usize % ranges.len()];
            let at = start + next() as usize % len;
            if start == ranges[0].0 {
                edits.push((at, next() as u8));
            } else {
                let word = start + (at - start) / 4 * 4;
                let v = match next() % 4 {
                    0 => next(),
                    1 => u32::MAX,
                    _ => next() % 8,
                };
                edits.extend((0..4).map(|i| (word + i, v.to_be_bytes()[i])));
            }
        }
        let old: Vec<u8> = edits.iter().map(|&(at, _)| bytes[at]).collect();
        for &(at, x) in &edits {
            bytes[at] = x;
        }
        detected += match mp4::read(&mut std::io::Cursor::new(&bytes)) {
            Ok(t) => usize::from(!t.damage().is_empty()),
            Err(_) => 1,
        };
        for (&(at, _), x) in edits.iter().zip(old).rev() {
            bytes[at] = x;
        }
    }
    // Most edits land in the sample tables and are found (3453 of them
    // with FFmpeg 6.1). The rest change sizes and times that stay
    // consistent, or bytes nothing checks, such as names.
    eprintln!("{detected} of 4000 edits found");
    assert!(detected >= 3000, "only {detected} of 4000 edits found");
    // A directory is not a media file.
    assert!(probe::probe(&dir).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn video_decoder_yields_every_frame_with_exact_times() {
    use spherecut_media::decode::VideoDecoder;
    let dir = tmp("decoder");
    let path = media::write(&small("t-dec", Codec::Hevc, 8, Audio::None, true), &dir).unwrap();
    let mut d = VideoDecoder::open(&path, None, None).unwrap();
    assert_eq!((d.width(), d.height()), (512, 256));
    let mut n = 0;
    let mut first = None;
    while let Some(f) = d.next_frame().unwrap() {
        assert!(!f.hw_decoded);
        let t = f.time.unwrap();
        let t0 = *first.get_or_insert(t);
        assert_eq!(
            t.checked_sub(t0).unwrap(),
            FrameRate::FPS_29_97.frame_to_time(n).unwrap(),
            "time of frame {n}"
        );
        n += 1;
    }
    assert_eq!(n, 6);
    // Reading past the end stays at the end.
    assert!(d.next_frame().unwrap().is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_hardware_device_is_an_error_not_a_panic() {
    let e = spherecut_media::decode::open_hw_device("vaapi", Some("/nonexistent/renderD999"));
    assert!(e.is_err());
    assert!(spherecut_media::decode::open_hw_device("no-such-api", None).is_err());
}
