//! The export's audio (M2 plan section 8.5, amendment 2 items 7 and 27):
//! AAC from the trim's start to its end, to within a sample, with the
//! edit list that says so in a movie timescale of 48 kHz; ambiX kept as
//! four channels in order with its `SA3D` box; and no track when there
//! is no audio or none is asked for.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::{Scratch, clip, export, open, project, secs, with_audio};
use spherecut_core::cancel::StopToken;
use spherecut_core::project::{Aspect, AudioChoice, Codec, Orientation, Output, Range};
use spherecut_core::time::{FrameRate, Time};
use spherecut_export::AudioReport;
use spherecut_media::audio::FfmpegAudio;
use spherecut_media::backend::{AudioDecode, AudioOutFormat, AudioRequest, NextAudio};
use spherecut_media::mp4::{self, Edit};
use spherecut_media::probe::{self, StreamKind};
use spherecut_synth::Pattern;
use spherecut_synth::audio::{AMBIX_TONES_HZ, tone_level};
use spherecut_synth::media::{Audio, Spec};

const FLAT_1080: Output = Output::Flat {
    aspect: Aspect::Wide,
    short_side: 1080,
};

/// 2 s of the grid at 30 fps, 1024×512, with `audio` at 48 kHz.
fn source(name: &str, audio: Audio) -> std::path::PathBuf {
    clip(&Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width: 1024,
        height: 512,
        rate: FrameRate::FPS_30,
        frames: 60,
        audio,
        spherical: true,
        ..Spec::default()
    })
}

/// Every channel of `path`'s audio from clip time `from` to the end,
/// planar: stereo at 48 kHz, or `raw`, the stream's own channels. Clip
/// time 0 is the video's first frame.
fn decode(path: &Path, raw: bool, from: Time) -> Vec<Vec<f32>> {
    let origin = open(path).info().origin().unwrap();
    let req = AudioRequest {
        stream: None,
        out: AudioOutFormat {
            rate: 48_000,
            channels: 2,
        },
        origin: Some(origin),
        interrupt: Arc::new(AtomicBool::new(false)),
    };
    let mut dec = if raw {
        FfmpegAudio::open_raw(path, &req).unwrap()
    } else {
        FfmpegAudio::open(path, &req).unwrap()
    };
    let stop = StopToken::never();
    dec.seek(from, &stop).unwrap();
    let channels = if raw {
        usize::from(dec.info().channels)
    } else {
        2
    };
    let mut out = vec![Vec::new(); channels];
    loop {
        match dec.next_chunk(&stop).unwrap() {
            NextAudio::Chunk(c) => {
                for (i, v) in c.data.iter().enumerate() {
                    out[i % channels].push(*v);
                }
            }
            NextAudio::End => break,
            NextAudio::Stopped => panic!("stopped"),
        }
    }
    out
}

/// The lag, from −`max` to `max` samples, at which `b` matches `a` best,
/// and the normalized correlation there.
fn best_lag(a: &[f32], b: &[f32], max: i64) -> (i64, f64) {
    let n = a.len().min(b.len()) as i64 - 2 * max;
    assert!(n > 1000, "too little audio to compare");
    (-max..=max)
        .map(|lag| {
            let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
            for i in max..max + n {
                let (x, y) = (f64::from(a[i as usize]), f64::from(b[(i + lag) as usize]));
                ab += x * y;
                aa += x * x;
                bb += y * y;
            }
            (lag, ab / (aa * bb).sqrt().max(1e-12))
        })
        .fold((0, f64::MIN), |best, c| if c.1 > best.1 { c } else { best })
}

/// The audio track's edit list and the movie's timescale.
fn audio_edits(path: &Path) -> (Vec<Edit>, u32) {
    let tree = mp4::read(&mut std::fs::File::open(path).unwrap()).unwrap();
    let trak = tree
        .tracks()
        .find(|t| mp4::handler_type(t).is_some_and(|h| h.0 == *b"soun"))
        .expect("an audio track");
    (
        mp4::edit_list(trak).expect("an edit list"),
        mp4::movie_timescale(&tree).unwrap(),
    )
}

fn audio_streams(path: &Path) -> Vec<probe::StreamInfo> {
    probe::probe(path)
        .unwrap()
        .streams
        .into_iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .collect()
}

#[test]
fn stereo_aac_starts_and_ends_within_a_sample_of_the_trim() {
    let src = source("audio-stereo", Audio::Stereo);
    let scratch = Scratch::new("audio-stereo");
    // On no frame, sample or AAC packet boundary.
    let trim = Range {
        start: secs(51_234, 100_000),
        end: secs(108_111, 100_000),
    };
    let mut p = project(&src, trim, FLAT_1080, Codec::H264, 0.0);
    with_audio(&mut p, AudioChoice::Aac);
    let out = scratch.0.join("stereo.mp4");
    let report = export(&p, &out);
    // round(1.08111 × 48000) − round(0.51234 × 48000) = 51893 − 24592.
    let samples = 27_301;
    assert_eq!(
        report.audio,
        Some(AudioReport {
            stream: 1,
            encoder: "aac",
            rate: 48_000,
            channels: 2,
            ambix: false,
            samples,
        })
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // The edit shows exactly the trim's samples, after the priming.
    let (edits, timescale) = audio_edits(&out);
    assert_eq!(timescale, 48_000);
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(edits[0].duration, samples);
    assert_eq!(edits[0].media_time, 1024, "AAC's priming");

    // Decoded, it is that long, and lines up with the source from the
    // trim's start: the lag of the best match is 0.
    let got = decode(&out, false, Time::ZERO);
    let n = got[0].len() as i64;
    assert!(
        (n - samples as i64).abs() <= 1,
        "{n} samples, want {samples}"
    );
    let want = decode(&src, false, trim.start);
    for ch in 0..2 {
        let (lag, r) = best_lag(&got[ch], &want[ch], 20);
        assert!(lag.abs() <= 1, "channel {ch}: best lag {lag} (r {r:.4})");
        assert!(r > 0.99, "channel {ch}: correlation {r:.4}");
    }
}

#[test]
fn ambix_in_a_360_export_stays_four_channels_in_order() {
    let src = source("audio-ambix-tones", Audio::AmbiXTones);
    let scratch = Scratch::new("audio-ambix");
    let trim = Range {
        start: secs(1, 4),
        end: secs(11, 20),
    };
    let sphere = Output::Equirect {
        width: 3840,
        orientation: Orientation::IDENTITY,
    };
    let mut p = project(&src, trim, sphere, Codec::Hevc, 0.0);
    with_audio(&mut p, AudioChoice::Aac);
    let out = scratch.0.join("ambix.mp4");
    let report = export(&p, &out);
    let a = report.audio.unwrap();
    assert_eq!((a.channels, a.ambix, a.samples), (4, true, 14_400));

    let streams = audio_streams(&out);
    assert_eq!(streams.len(), 1);
    let sa3d = streams[0].spherical.spatial_audio.as_ref().expect("SA3D");
    assert!(sa3d.is_ambix() && sa3d.ambisonic_order == 1, "{sa3d:?}");

    // Each channel holds its own tone and not the others'.
    let got = decode(&out, true, Time::ZERO);
    assert_eq!(got.len(), 4);
    for (ch, samples) in got.iter().enumerate() {
        let levels: Vec<f64> = AMBIX_TONES_HZ
            .iter()
            .map(|&f| tone_level(samples, 48_000, f))
            .collect();
        let own = levels[ch];
        assert!((own - 0.25).abs() < 0.02, "channel {ch}: {levels:?}");
        for (other, &l) in levels.iter().enumerate() {
            if other != ch {
                assert!(l < own / 30.0, "channel {ch} has tone {other}: {levels:?}");
            }
        }
    }
}

#[test]
fn ambix_in_a_flat_export_is_heard_as_stereo() {
    let src = source("audio-ambix-tones", Audio::AmbiXTones);
    let scratch = Scratch::new("audio-ambix-flat");
    let trim = Range {
        start: Time::ZERO,
        end: secs(3, 10),
    };
    let mut p = project(&src, trim, FLAT_1080, Codec::H264, 0.0);
    with_audio(&mut p, AudioChoice::Aac);
    let out = scratch.0.join("flat.mp4");
    let a = export(&p, &out).audio.unwrap();
    assert_eq!((a.channels, a.ambix), (2, false));
    let streams = audio_streams(&out);
    assert!(streams[0].spherical.spatial_audio.is_none());
    // L = (W + Y) / 2 and R = (W − Y) / 2: W's tone in both, Y's in
    // both with opposite signs, nothing of Z or X.
    let got = decode(&out, true, Time::ZERO);
    for samples in &got {
        let level = |i: usize| tone_level(samples, 48_000, AMBIX_TONES_HZ[i]);
        assert!((level(0) - 0.125).abs() < 0.02, "W {}", level(0));
        assert!((level(1) - 0.125).abs() < 0.02, "Y {}", level(1));
        assert!(
            level(2) < 0.005 && level(3) < 0.005,
            "Z {}, X {}",
            level(2),
            level(3)
        );
    }
}

#[test]
fn no_audio_asked_for_or_none_there_makes_no_track() {
    let scratch = Scratch::new("audio-none");
    let trim = Range {
        start: Time::ZERO,
        end: secs(1, 5),
    };
    let src = source("audio-stereo", Audio::Stereo);
    let mut p = project(&src, trim, FLAT_1080, Codec::H264, 0.0);
    with_audio(&mut p, AudioChoice::None);
    let out = scratch.0.join("none.mp4");
    let report = export(&p, &out);
    assert_eq!(report.audio, None);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(audio_streams(&out).is_empty());

    let silent = source("audio-silent", Audio::None);
    let mut p = project(&silent, trim, FLAT_1080, Codec::H264, 0.0);
    with_audio(&mut p, AudioChoice::Aac);
    let out = scratch.0.join("silent.mp4");
    let report = export(&p, &out);
    assert_eq!(report.audio, None);
    assert_eq!(report.warnings, ["the clip has no audio"]);
    assert!(audio_streams(&out).is_empty());
}
