//! Everything that writes media: the synthetic clip writer, the MP4
//! encoder and the metadata injector, on real files.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::decode::VideoDecoder;
use spherecut_media::encode::{
    AudioSettings, EncodeError, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes,
};
use spherecut_media::ff;
use spherecut_media::inject::{self, EntryEdit, InjectError, TrackKind};
use spherecut_media::probe::{self, StreamKind};
use spherecut_media::spherical::write;
use spherecut_synth::media::{self, Audio, Codec, Spec};
use spherecut_synth::{Pattern, audio, decode_frame_code, yuv};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("spherecut-writers-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// File names in `dir`, sorted, hidden temp files included.
fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn spec(name: &str, width: u32, frames: u32, codec: Codec, bits: u32, audio: Audio) -> Spec {
    Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width,
        height: width / 2,
        rate: FrameRate::FPS_30,
        frames,
        codec,
        bits,
        audio,
        spherical: true,
    }
}

#[test]
fn write_leaves_only_complete_files() {
    let dir = tmp("atomic");
    let tagged = spec("tagged", 64, 2, Codec::H264, 8, Audio::Stereo);
    let untagged = Spec {
        spherical: false,
        ..spec("untagged", 64, 2, Codec::H264, 8, Audio::None)
    };
    for s in [&tagged, &untagged] {
        let p = media::write(s, &dir).unwrap();
        assert_eq!(p, dir.join(s.file_name()));
    }
    assert_eq!(listing(&dir), ["tagged.mp4", "untagged.mp4"]);

    // When the final rename fails (a directory is in the way), the error
    // names the destination and no temporary file is left.
    let blocked = tmp("atomic-blocked");
    for s in [&tagged, &untagged] {
        let dest = blocked.join(s.file_name());
        std::fs::create_dir(&dest).unwrap();
        let e = media::write(s, &blocked).unwrap_err().to_string();
        assert!(
            e.contains(&*dest.to_string_lossy()) && !e.contains(".tmp"),
            "{}: {e}",
            s.name
        );
    }
    assert_eq!(listing(&blocked), ["tagged.mp4", "untagged.mp4"]);
    assert!(blocked.join("tagged.mp4").is_dir());
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&blocked).ok();
}

#[test]
fn clips_with_the_longest_legal_names_can_be_written() {
    // Temporary names used to grow with the clip's and pass NAME_MAX.
    let dir = tmp("long-name");
    let s = spec(&"c".repeat(251), 64, 2, Codec::H264, 8, Audio::Stereo);
    assert_eq!(s.file_name().len(), 255);
    media::write(&s, &dir).unwrap();
    assert_eq!(listing(&dir), [s.file_name()]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn concurrent_writes_of_one_clip_all_succeed() {
    // Tier 2 tests share cached clips, and may generate one from several
    // threads at once.
    let dir = tmp("concurrent");
    let s = spec("shared", 64, 2, Codec::H264, 8, Audio::Stereo);
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| media::write(&s, &dir)))
            .collect();
        for w in writers {
            w.join().unwrap().unwrap();
        }
    });
    assert_eq!(listing(&dir), ["shared.mp4"]);
    let r = probe::probe(&dir.join("shared.mp4")).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn inject_refuses_to_overwrite_its_own_input() {
    // This used to truncate the input and then copy it onto itself until
    // the disk was full. Every inject copy is now bounded by the size the
    // parser saw, so this test can't run away even if the check regresses.
    let dir = tmp("same");
    let clip = media::write(
        &Spec {
            spherical: false,
            ..spec("clip", 64, 2, Codec::H264, 8, Audio::Stereo)
        },
        &dir,
    )
    .unwrap();
    let before = std::fs::read(&clip).unwrap();
    let edits = [EntryEdit {
        track: TrackKind::Audio,
        boxes: write::sa3d_first_order_ambix(),
    }];
    match inject::inject(&clip, &clip, &edits) {
        Err(InjectError::SameFile { .. }) => {}
        r => panic!("{r:?}"),
    }
    assert!(std::fs::read(&clip).unwrap() == before, "input changed");
    assert_eq!(listing(&dir), ["clip.mp4"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn h264_10bit_is_in_the_standard_set_and_round_trips() {
    assert!(
        media::standard_set()
            .iter()
            .any(|s| s.codec == Codec::H264 && s.bits == 10)
    );
    let dir = tmp("h264-10");
    let path = media::write(&spec("h264-10", 512, 4, Codec::H264, 10, Audio::None), &dir).unwrap();
    let r = probe::probe(&path).unwrap();
    let v = r
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .unwrap();
    assert_eq!(v.codec.as_deref(), Some("h264"));
    assert_eq!(v.profile.as_deref(), Some("High 10"));
    assert_eq!(v.video.as_ref().unwrap().bit_depth, Some(10));
    assert!(v.spherical.v2.is_some());
    let mut d = VideoDecoder::open(&path, None, None).unwrap();
    let mut n = 0;
    while let Some(f) = d.next_frame().unwrap() {
        let img = yuv::from_yuv420(&YuvPlanes::U16(f.planes_u16().unwrap()), 512, 256, 10);
        assert_eq!(decode_frame_code(&img), Some(n));
        n += 1;
    }
    assert_eq!(n, 4);
    std::fs::remove_dir_all(&dir).ok();
}

/// All decoded audio of `path`, planar.
fn decode_audio(path: &Path) -> (Vec<Vec<f32>>, String) {
    spherecut_media::init();
    let mut ictx = ff::format::input(path).unwrap();
    let st = ictx.streams().best(ff::media::Type::Audio).unwrap();
    let idx = st.index();
    let mut dec = ff::codec::context::Context::from_parameters(st.parameters())
        .unwrap()
        .decoder()
        .audio()
        .unwrap();
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut frame = ff::frame::Audio::empty();
    let mut grab = |dec: &mut ff::decoder::Audio, out: &mut Vec<Vec<f32>>| {
        while dec.receive_frame(&mut frame).is_ok() {
            assert_eq!(
                frame.format(),
                ff::format::Sample::F32(ff::format::sample::Type::Planar)
            );
            out.resize(frame.planes(), Vec::new());
            for (ch, o) in out.iter_mut().enumerate() {
                o.extend_from_slice(frame.plane::<f32>(ch));
            }
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
    let layout = probe::probe(path)
        .unwrap()
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Audio)
        .and_then(|s| s.audio.as_ref()?.layout.clone())
        .unwrap_or_default();
    (out, layout)
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum()
}

#[test]
fn ambix_survives_aac_with_its_direction_and_sa3d() {
    let dir = tmp("ambix");
    // Back left and below: Y, Z and X all differ in size and X and Z are
    // negative, so swapping or flipping any channel moves the direction. At
    // azimuth 45, Y and X are equal and a swap went unnoticed.
    let (az, el) = (120, -20);
    let s = spec(
        "ambix",
        64,
        30,
        Codec::H264,
        8,
        Audio::AmbiX {
            azimuth: az,
            elevation: el,
        },
    );
    let path = media::write(&s, &dir).unwrap();
    let r = probe::probe(&path).unwrap();
    let a = r
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Audio)
        .unwrap();
    assert_eq!(a.audio.as_ref().unwrap().channels, 4);
    assert!(a.spherical.spatial_audio.as_ref().unwrap().is_ambix());
    let (ch, layout) = decode_audio(&path);
    // FFmpeg's AAC decoder calls it 4.0 whatever the container says; the
    // channel order survives, and SA3D says what the channels are.
    eprintln!("ambiX AAC decodes with layout {layout:?}");
    assert_eq!(ch.len(), 4);
    // Channel by channel: each one's gain against W is the one
    // ambix_source gave it, so W, Y, Z, X come back in that order.
    let (a, e) = ((az as f64).to_radians(), (el as f64).to_radians());
    let want = [1.0, a.sin() * e.cos(), e.sin(), a.cos() * e.cos()];
    let got: Vec<f64> = ch
        .iter()
        .map(|c| dot(&ch[0], c) / dot(&ch[0], &ch[0]))
        .collect();
    for (k, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(
            (g - w).abs() < 0.03,
            "channel {k} has gain {g:.3}, not {w:.3}: {got:.3?}"
        );
    }
    let (raz, rel) = audio::ambix_direction(&ch).unwrap();
    assert!(
        (raz - az as f64).abs() < 2.0 && (rel - el as f64).abs() < 2.0,
        "{az},{el} came back as {raz:.2},{rel:.2}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

fn video_settings() -> VideoSettings {
    VideoSettings {
        encoder: "libx264".into(),
        width: 64,
        height: 32,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        options: vec![("preset".into(), "ultrafast".into())],
        bt709_limited: true,
    }
}

fn black_frame() -> YuvPlanes {
    YuvPlanes::U8([vec![16; 64 * 32], vec![128; 32 * 16], vec![128; 32 * 16]])
}

#[test]
fn unfinished_exports_leave_the_destination_alone() {
    let dir = tmp("unfinished");
    let out = dir.join("out.mp4");
    std::fs::write(&out, b"old").unwrap();
    // A writer dropped part way, as after an encode error or a cancel.
    let mut w = Mp4Writer::create(&out, &video_settings(), None).unwrap();
    w.push_video(&black_frame()).unwrap();
    drop(w);
    // A missing encoder is found before anything is written. This used to
    // leave an empty file.
    let missing = AudioSettings {
        encoder: "no-such-encoder".into(),
        ..aac(2)
    };
    let r = Mp4Writer::create(&dir.join("new.mp4"), &video_settings(), Some(&missing));
    assert!(matches!(r, Err(EncodeError::NoEncoder(_))), "{r:?}");
    assert_eq!(std::fs::read(&out).unwrap(), b"old");
    assert_eq!(listing(&dir), ["out.mp4"]);

    // Finishing replaces the file, through a symlink to it.
    let link = dir.join("link.mp4");
    std::os::unix::fs::symlink("out.mp4", &link).unwrap();
    let mut w = Mp4Writer::create(&link, &video_settings(), None).unwrap();
    w.push_video(&black_frame()).unwrap();
    assert_eq!(w.finish().unwrap(), link);
    assert!(link.symlink_metadata().unwrap().is_symlink());
    let r = probe::probe(&out).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(listing(&dir), ["link.mp4", "out.mp4"]);
    std::fs::remove_dir_all(&dir).ok();
}

fn aac(channels: u32) -> AudioSettings {
    AudioSettings {
        encoder: "aac".into(),
        sample_rate: 48_000,
        channels,
        bit_rate: 128_000,
    }
}

#[test]
fn push_audio_rejects_bad_input_without_panicking() {
    let dir = tmp("bad-audio");
    let mut w = Mp4Writer::create(&dir.join("a.mp4"), &video_settings(), Some(&aac(2))).unwrap();
    let wrong_count = w.push_audio(&[vec![0.0; 1600]]);
    assert!(
        matches!(wrong_count, Err(EncodeError::Invalid(_))),
        "{wrong_count:?}"
    );
    // Used to panic on a slice index out of range.
    let ragged = w.push_audio(&[vec![0.0; 1600], vec![0.0; 1599]]);
    assert!(matches!(ragged, Err(EncodeError::Invalid(_))), "{ragged:?}");
    // The writer is still usable after refusing input.
    w.push_video(&black_frame()).unwrap();
    w.push_audio(&audio::stereo(48_000, 1600)).unwrap();
    w.finish().unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn audio_encoders_with_small_fixed_frames_get_exact_frames() {
    // libvorbis takes exactly 64 samples per frame; the writer used to
    // send at least 1024 and fail.
    let dir = tmp("vorbis");
    let a = AudioSettings {
        encoder: "libvorbis".into(),
        sample_rate: 48_000,
        channels: 2,
        bit_rate: 128_000,
    };
    let path = dir.join("vorbis.mp4");
    let mut w = match Mp4Writer::create(&path, &video_settings(), Some(&a)) {
        Ok(w) => w,
        Err(EncodeError::NoEncoder(e)) => {
            eprintln!("skipped: this FFmpeg has no {e} encoder");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    let samples = audio::stereo(48_000, 48_000);
    for i in 0..30 {
        w.push_video(&black_frame()).unwrap();
        let chunk: Vec<Vec<f32>> = samples
            .iter()
            .map(|c| c[i * 1600..(i + 1) * 1600].to_vec())
            .collect();
        w.push_audio(&chunk).unwrap();
    }
    w.finish().unwrap();
    let (ch, _) = decode_audio(&path);
    assert_eq!(ch.len(), 2);
    // Nothing is trimmed from Vorbis in MP4, so a little encoder padding
    // comes back too: allow up to one long block (2048 samples).
    assert!(
        (48_000..=48_000 + 2048).contains(&ch[0].len()),
        "{} samples",
        ch[0].len()
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn audio_encoders_that_need_packed_samples_are_refused_clearly() {
    let dir = tmp("packed");
    let a = AudioSettings {
        encoder: "libopus".into(),
        sample_rate: 48_000,
        channels: 2,
        bit_rate: 128_000,
    };
    match Mp4Writer::create(&dir.join("opus.mp4"), &video_settings(), Some(&a)) {
        Err(EncodeError::Invalid(e)) => assert!(e.contains("planar"), "{e}"),
        Err(EncodeError::NoEncoder(e)) => eprintln!("skipped: this FFmpeg has no {e} encoder"),
        r => panic!("{r:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}
