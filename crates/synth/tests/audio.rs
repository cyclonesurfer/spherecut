//! The audio decoder (M1 plan sections 9.1 and 12.1, DECISIONS D35), on
//! clips synth writes: exact sample counts and rates, the tones' pitches,
//! AAC's priming gone and the audio on the video's time axis, seeks that
//! give a sequential decode's samples, 44.1 kHz resampled to 48 kHz to the
//! sample, mono duplicated, ambiX decoded to two cardioids, the output's
//! channels, origins before and after the audio, and `sync-flash-beep`'s
//! beeps on its white frames.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::{FrameRate, Rational, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioOutFormat, AudioRequest, FfmpegBackend, MediaBackend, NextAudio, NextFrame,
    VideoRequest,
};
use spherecut_synth::media::{self, Audio, Spec};

const STEREO_48K: AudioOutFormat = AudioOutFormat {
    rate: 48_000,
    channels: 2,
};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("spherecut-audio-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A small H.264 clip of `seconds` at 30 fps with `audio` at
/// `sample_rate`.
fn clip(name: &str, audio: Audio, sample_rate: u32, seconds: u32) -> PathBuf {
    let spec = Spec {
        name: name.into(),
        frames: seconds * 30,
        audio,
        sample_rate,
        ..Spec::default()
    };
    media::write(&spec, &dir(name)).unwrap()
}

fn request(out: AudioOutFormat, origin: Option<Time>) -> AudioRequest {
    AudioRequest {
        stream: None,
        out,
        origin,
        interrupt: Arc::new(AtomicBool::new(false)),
    }
}

fn open(path: &Path, out: AudioOutFormat, origin: Option<Time>) -> Box<dyn AudioDecode> {
    FfmpegBackend::default()
        .open_audio(path, &request(out, origin))
        .unwrap()
}

/// The video's clip time 0 in the file's time.
fn video_origin(path: &Path) -> Time {
    FfmpegBackend::default()
        .open_video(path, &VideoRequest::default())
        .unwrap()
        .info()
        .origin()
        .unwrap()
}

/// Chunks from the decoder's position on, up to `frames` frames (all of
/// them with `None`): the first chunk's start and the samples, checked to
/// follow on without a gap or an overlap.
fn drain(dec: &mut dyn AudioDecode, frames: Option<usize>) -> (i64, Vec<f32>) {
    let never = StopToken::never();
    let mut start = None;
    let mut next = 0;
    let mut out = Vec::new();
    loop {
        match dec.next_chunk(&never).unwrap() {
            NextAudio::Chunk(c) => {
                let ch = usize::from(c.channels);
                assert_eq!(c.data.len() % ch, 0);
                if let Some(s) = start {
                    assert_eq!(c.start, next, "chunks follow on (first at {s})");
                } else {
                    start = Some(c.start);
                }
                next = c.start + (c.data.len() / ch) as i64;
                out.extend_from_slice(&c.data);
                if frames.is_some_and(|f| out.len() / ch >= f) {
                    out.truncate(frames.unwrap() * ch);
                    break;
                }
            }
            NextAudio::End => break,
            NextAudio::Stopped => panic!("stopped with a token that never stops"),
        }
    }
    (start.unwrap_or(0), out)
}

fn channel(data: &[f32], channels: usize, c: usize) -> Vec<f32> {
    data.iter().skip(c).step_by(channels).copied().collect()
}

/// A tone's pitch from its upward zero crossings over `samples`.
fn pitch(samples: &[f32], rate: u32) -> f64 {
    let ups = samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    ups as f64 * f64::from(rate) / samples.len() as f64
}

fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|&x| f64::from(x) * f64::from(x)).sum()
}

#[test]
fn sample_counts_are_exact() {
    for (name, audio, rate) in [
        ("count-stereo-48", Audio::Stereo, 48_000),
        ("count-mono-48", Audio::Mono, 48_000),
        ("count-stereo-441", Audio::Stereo, 44_100),
    ] {
        let path = clip(name, audio, rate, 2);
        let mut dec = open(&path, STEREO_48K, Some(video_origin(&path)));
        assert_eq!(dec.info().rate, rate, "{name}");
        assert_eq!(dec.info().start, Time::ZERO, "{name}");
        let (start, data) = drain(dec.as_mut(), None);
        assert_eq!(start, 0, "{name}");
        assert_eq!(data.len() / 2, 96_000, "{name}: 2 s at 48 kHz");
    }
}

#[test]
fn the_tones_are_440_and_660_hz() {
    let path = clip("pitch", Audio::Stereo, 48_000, 2);
    let mut dec = open(&path, STEREO_48K, None);
    let (_, data) = drain(dec.as_mut(), None);
    let middle = &data[48_000..2 * 72_000];
    let (l, r) = (channel(middle, 2, 0), channel(middle, 2, 1));
    assert!(
        (pitch(&l, 48_000) - 440.0).abs() < 2.0,
        "{}",
        pitch(&l, 48_000)
    );
    assert!(
        (pitch(&r, 48_000) - 660.0).abs() < 2.0,
        "{}",
        pitch(&r, 48_000)
    );
}

#[test]
fn forty_four_one_khz_resamples_to_exactly_ten_seconds_at_48() {
    let path = clip("resample-10s", Audio::Stereo, 44_100, 10);
    let mut dec = open(&path, STEREO_48K, Some(video_origin(&path)));
    let (start, data) = drain(dec.as_mut(), None);
    assert_eq!(start, 0);
    assert_eq!(data.len() / 2, 480_000);
    let l = channel(&data[2 * 48_000..2 * 432_000], 2, 0);
    assert!(
        (pitch(&l, 48_000) - 440.0).abs() < 1.0,
        "{}",
        pitch(&l, 48_000)
    );
}

/// AAC's encoder delays its output by 1024 samples, which the MP4's edit
/// list and libavcodec take away again: the samples come out where the
/// source put them, on the video's axis, to the sample.
#[test]
fn priming_is_gone_and_the_audio_starts_on_the_video_axis() {
    let path = clip("priming", Audio::Stereo, 48_000, 2);
    assert_eq!(video_origin(&path), Time::ZERO);
    let mut dec = open(&path, STEREO_48K, Some(video_origin(&path)));
    let (start, data) = drain(dec.as_mut(), None);
    assert_eq!(start, 0);
    let source = &spherecut_synth::audio::stereo(48_000, 96_000)[0];
    let decoded = channel(&data, 2, 0);
    // The tone fades in from sample 0, so its first loud sample places
    // it; a tone repeats every period, so the lag is then searched only
    // within a period of 440 Hz.
    let loud = |s: &[f32]| s.iter().position(|x| x.abs() > 0.05).unwrap() as i64;
    assert!(
        (loud(&decoded) - loud(source)).abs() <= 3,
        "{} {}",
        loud(&decoded),
        loud(source)
    );
    let correlation = |lag: i64| -> f64 {
        (2000..90_000i64)
            .map(|i| f64::from(decoded[i as usize]) * f64::from(source[(i + lag) as usize]))
            .sum()
    };
    let best = (-50..=50).max_by(|&a, &b| correlation(a).total_cmp(&correlation(b)));
    assert_eq!(best, Some(0));
}

#[test]
fn a_seek_gives_a_sequential_decodes_samples() {
    for (name, rate) in [("seek-48", 48_000), ("seek-441", 44_100)] {
        let path = clip(name, Audio::Stereo, rate, 3);
        let origin = Some(video_origin(&path));
        let mut dec = open(&path, STEREO_48K, origin);
        let (_, all) = drain(dec.as_mut(), None);
        let never = StopToken::never();
        for at in [
            Time::seconds(Rational::new(1, 10).unwrap()),
            Time::seconds(Rational::new(1, 2).unwrap()),
            Time::seconds(Rational::new(1_234_567, 1_000_000).unwrap()),
            Time::seconds(Rational::new(1001, 30_000).unwrap()),
            Time::from_secs(2),
        ] {
            dec.seek(at, &never).unwrap();
            let (start, data) = drain(dec.as_mut(), Some(9600));
            let want = (at.to_secs_f64() * 48_000.0).round() as i64;
            assert_eq!(start, want, "{name} at {at:?}");
            let from = start as usize * 2;
            let worst = data
                .iter()
                .zip(&all[from..])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(worst <= 1e-6, "{name} at {at:?}: off by {worst}");
            assert_eq!(data.len(), 9600 * 2, "{name} at {at:?}");
        }
        // Before the start, the samples before 0 are silence; past the
        // end there are none.
        dec.seek(Time::seconds(Rational::new(-1, 10).unwrap()), &never)
            .unwrap();
        let (start, data) = drain(dec.as_mut(), Some(9600));
        assert_eq!(start, -4800);
        assert!(data[..4800 * 2].iter().all(|&x| x == 0.0));
        assert_eq!(&data[4800 * 2..], &all[..4800 * 2]);
        dec.seek(Time::from_secs(5), &never).unwrap();
        assert!(matches!(dec.next_chunk(&never).unwrap(), NextAudio::End));
    }
}

#[test]
fn mono_is_duplicated() {
    let path = clip("mono", Audio::Mono, 48_000, 1);
    let mut dec = open(&path, STEREO_48K, None);
    assert_eq!(dec.info().channels, 1);
    let (_, data) = drain(dec.as_mut(), None);
    assert!(data.chunks_exact(2).all(|f| f[0] == f[1]));
    let l = channel(&data[9600..86_400], 2, 0);
    assert!((pitch(&l, 48_000) - 440.0).abs() < 2.0);
}

/// 5.1 is mixed down by swresample's default matrix: the front pair's
/// tones stay on their sides.
#[test]
fn surround_is_mixed_down_to_stereo() {
    let path = clip("surround", Audio::Surround, 48_000, 1);
    let mut dec = open(&path, STEREO_48K, None);
    assert_eq!(dec.info().channels, 6);
    let (_, data) = drain(dec.as_mut(), None);
    assert_eq!(data.len(), 48_000 * 2);
    let middle = &data[9600..86_400];
    let (l, r) = (channel(middle, 2, 0), channel(middle, 2, 1));
    assert!(
        (pitch(&l, 48_000) - 440.0).abs() < 2.0,
        "{}",
        pitch(&l, 48_000)
    );
    assert!(
        (pitch(&r, 48_000) - 660.0).abs() < 2.0,
        "{}",
        pitch(&r, 48_000)
    );
}

/// A source at azimuth +90° is on the left, where the left cardioid
/// points, and one at -90° on the right.
#[test]
fn ambix_decodes_to_two_cardioids() {
    for (azimuth, louder) in [(90, 0), (-90, 1)] {
        let name = format!("ambix-{azimuth}");
        let path = clip(
            &name,
            Audio::AmbiX {
                azimuth,
                elevation: 0,
            },
            48_000,
            1,
        );
        // The SA3D box comes with spherical metadata only.
        let spherical = Spec {
            name: format!("{name}-tagged"),
            frames: 30,
            audio: Audio::AmbiX {
                azimuth,
                elevation: 0,
            },
            spherical: true,
            ..Spec::default()
        };
        let tagged = media::write(&spherical, &dir(&spherical.name)).unwrap();
        let mut dec = open(&tagged, STEREO_48K, None);
        assert!(dec.info().ambisonic, "{name}");
        let (_, data) = drain(dec.as_mut(), None);
        let (l, r) = (energy(&channel(&data, 2, 0)), energy(&channel(&data, 2, 1)));
        let ratio = if louder == 0 { l / r } else { r / l };
        assert!(ratio > 4.0, "{name}: L {l}, R {r}");
        // Without SA3D the four channels are 4.0 surround, mixed down by
        // swresample: not ambiX.
        let dec = open(&path, STEREO_48K, None);
        assert!(!dec.info().ambisonic, "{name}");
    }
}

#[test]
fn the_output_has_the_devices_channels() {
    let path = clip("channels", Audio::Stereo, 48_000, 1);
    let (_, stereo) = drain(open(&path, STEREO_48K, None).as_mut(), None);
    let one = AudioOutFormat {
        rate: 48_000,
        channels: 1,
    };
    let (_, mono) = drain(open(&path, one, None).as_mut(), None);
    assert_eq!(mono.len() * 2, stereo.len());
    for (m, s) in mono.iter().zip(stereo.chunks_exact(2)) {
        assert!((m - 0.5 * (s[0] + s[1])).abs() <= 1e-7);
    }
    let six = AudioOutFormat {
        rate: 48_000,
        channels: 6,
    };
    let (_, wide) = drain(open(&path, six, None).as_mut(), None);
    assert_eq!(wide.len(), stereo.len() * 3);
    for (w, s) in wide.chunks_exact(6).zip(stereo.chunks_exact(2)) {
        assert_eq!(&w[..2], s);
        assert!(w[2..].iter().all(|&x| x == 0.0));
    }
}

/// Clip time 0 is the video's: audio from before it is dropped, and audio
/// that starts after it is led by silence.
#[test]
fn audio_before_clip_time_0_is_dropped_and_audio_after_it_led_by_silence() {
    let path = clip("origins", Audio::Stereo, 48_000, 2);
    let (_, all) = drain(open(&path, STEREO_48K, None).as_mut(), None);
    let half = Time::seconds(Rational::new(1, 2).unwrap());
    let mut late = open(&path, STEREO_48K, Some(half));
    assert_eq!(
        late.info().start,
        Time::seconds(Rational::new(-1, 2).unwrap())
    );
    let (start, data) = drain(late.as_mut(), None);
    assert_eq!(start, 0);
    assert_eq!(data, all[24_000 * 2..]);
    let early = Time::seconds(Rational::new(-1, 2).unwrap());
    let mut early = open(&path, STEREO_48K, Some(early));
    assert_eq!(early.info().start, half);
    let (start, data) = drain(early.as_mut(), None);
    assert_eq!(start, 0);
    assert!(data[..24_000 * 2].iter().all(|&x| x == 0.0));
    assert_eq!(&data[24_000 * 2..], &all[..]);
}

#[test]
fn a_file_without_audio_has_no_stream() {
    let path = clip("silent", Audio::None, 48_000, 1);
    let e = FfmpegBackend::default()
        .open_audio(&path, &request(STEREO_48K, None))
        .unwrap_err();
    assert!(
        matches!(e, MediaError::NoStream { kind: "audio", .. }),
        "{e}"
    );
    assert_eq!(e.code(), "no_stream");
}

/// Matroska keeps times to the millisecond: a remux's samples are the
/// MP4's, placed on the clip's axis within a millisecond of where the MP4
/// puts them, and it ends where its last block's duration says, since it
/// has no count of AAC's padding.
#[test]
fn a_matroska_remux_decodes_as_the_mp4_does_within_a_millisecond() {
    let path = clip("mkv", Audio::Stereo, 48_000, 2);
    let mkv = media::remux_to_mkv(&path, &dir("mkv-remux")).unwrap();
    let (s0, mp4) = drain(
        open(&path, STEREO_48K, Some(video_origin(&path))).as_mut(),
        None,
    );
    let mut dec = open(&mkv, STEREO_48K, Some(video_origin(&mkv)));
    let (s1, data) = drain(dec.as_mut(), None);
    assert_eq!((s0, s1), (0, 0));
    let (l0, l1) = (channel(&mp4, 2, 0), channel(&data, 2, 0));
    let off = |lag: i64| {
        (5000..90_000i64)
            .map(|i| (l1[i as usize] - l0[(i + lag) as usize]).abs())
            .fold(0.0f32, f32::max)
    };
    let lag = (-48..=48).find(|&lag| off(lag) <= 1e-6);
    assert!(lag.is_some(), "no lag within 1 ms matches");
    assert!(
        l1.len().abs_diff(l0.len()) <= 1024 + 48,
        "{} {}",
        l1.len(),
        l0.len()
    );
}

/// `sync-flash-beep`: each beep starts within 1 ms of the white frame it
/// goes with, measured from what the decoders give.
#[test]
fn sync_flash_beep_beeps_on_its_white_frames() {
    let spec = media::sync_flash_beep();
    assert_eq!(spec.rate, FrameRate::FPS_29_97);
    assert_eq!(spec.white.len(), 10);
    let path = media::write(&spec, &dir("sync")).unwrap();
    let never = StopToken::never();
    let mut video = FfmpegBackend::default()
        .open_video(&path, &VideoRequest::default())
        .unwrap();
    let origin = video.info().origin().unwrap();
    let mut times = Vec::new();
    let mut index = 0u32;
    while let NextFrame::Frame(f) = video.next_frame(&never).unwrap() {
        if spec.white.contains(&index) {
            times.push(f.time.to_secs_f64());
        }
        index += 1;
    }
    assert_eq!(index, spec.frames);
    assert_eq!(times.len(), spec.white.len());
    let mut audio = open(&path, STEREO_48K, Some(origin));
    let (start, data) = drain(audio.as_mut(), None);
    assert_eq!(start, 0);
    let l = channel(&data, 2, 0);
    for (k, &t) in times.iter().enumerate() {
        // The first sample past a fifth of the beep's amplitude, looking
        // from 0.3 s before the frame.
        let from = ((t - 0.3).max(0.0) * 48_000.0) as usize;
        let onset = l[from..]
            .iter()
            .position(|&x| x.abs() > 0.1)
            .map(|i| (from + i) as f64 / 48_000.0)
            .unwrap();
        assert!(
            (onset - t).abs() <= 0.001,
            "beep {k}: at {onset:.5} s, its frame at {t:.5} s"
        );
        assert!(
            t >= k as f64 - 1.0 / 29.97 && t <= k as f64,
            "frame {k} at {t}"
        );
    }
}
