//! Writing synthetic patterns to real media files, plus deliberately broken
//! files for robustness tests.

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::encode::{AudioSettings, Mp4Writer, PixelLayout, VideoSettings};
use spherecut_media::inject::{self, EntryEdit, TrackKind};
use spherecut_media::mp4;
use spherecut_media::spherical::{StereoMode, write};
use thiserror::Error;

use crate::{Pattern, audio, render_equirect, yuv};

#[derive(Debug, Error)]
pub enum SynthError {
    #[error(transparent)]
    Encode(#[from] spherecut_media::encode::EncodeError),
    #[error(transparent)]
    Inject(#[from] inject::InjectError),
    #[error("I/O error on {}: {error}", path.display())]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    /// Mathematically lossless H.264 (x264 qp 0), for exact comparisons.
    H264Lossless,
    /// Mathematically lossless HEVC (x265 lossless), 8 or 10 bit.
    HevcLossless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audio {
    None,
    Stereo,
    /// First-order ambiX with a source at (azimuth, elevation) degrees.
    AmbiX {
        azimuth: i32,
        elevation: i32,
    },
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub name: String,
    pub pattern: Pattern,
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    pub frames: u32,
    pub codec: Codec,
    pub bits: u32,
    pub audio: Audio,
    /// Write Spherical Video V2 (and SA3D for ambiX) metadata.
    pub spherical: bool,
}

impl Spec {
    pub fn file_name(&self) -> String {
        format!("{}.mp4", self.name)
    }

    fn video_settings(&self) -> VideoSettings {
        let (encoder, mut options) = match self.codec {
            Codec::H264 => ("libx264", vec![("crf", "16"), ("preset", "veryfast")]),
            Codec::Hevc => (
                "libx265",
                vec![
                    ("crf", "18"),
                    ("preset", "veryfast"),
                    ("x265-params", "log-level=error"),
                ],
            ),
            Codec::H264Lossless => ("libx264", vec![("qp", "0"), ("preset", "veryfast")]),
            Codec::HevcLossless => (
                "libx265",
                vec![
                    ("preset", "veryfast"),
                    ("x265-params", "lossless=1:log-level=error"),
                ],
            ),
        };
        // Single-threaded encodes keep the bitstream reproducible.
        options.push(("threads", "1"));
        VideoSettings {
            encoder: encoder.into(),
            width: self.width,
            height: self.height,
            rate: self.rate,
            layout: if self.bits > 8 {
                PixelLayout::Yuv420p10
            } else {
                PixelLayout::Yuv420p
            },
            options: options
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            bt709_limited: true,
        }
    }
}

/// Render and encode `spec` into `dir`. Returns the file path.
///
/// [`Mp4Writer`] renames a file into place only when it is complete, so if
/// the path exists it holds a whole clip, even after an interrupted run,
/// and callers may cache clips by whether the file exists.
pub fn write(spec: &Spec, dir: &Path) -> Result<PathBuf, SynthError> {
    std::fs::create_dir_all(dir).map_err(|error| SynthError::Io {
        path: dir.to_path_buf(),
        error,
    })?;
    let out = dir.join(spec.file_name());
    let sample_rate = 48_000u32;
    let audio_settings = match spec.audio {
        Audio::None => None,
        Audio::Stereo => Some(AudioSettings {
            encoder: "aac".into(),
            sample_rate,
            channels: 2,
            bit_rate: 192_000,
            options: Vec::new(),
        }),
        Audio::AmbiX { .. } => Some(AudioSettings {
            encoder: "aac".into(),
            sample_rate,
            channels: 4,
            bit_rate: 256_000,
            options: Vec::new(),
        }),
    };
    let mut w = Mp4Writer::create(&out, &spec.video_settings(), audio_settings.as_ref())?;
    // Exact per-frame sample counts: frame i covers samples
    // [i·sr/fps, (i+1)·sr/fps).
    let fps = spec.rate.as_rational();
    let samples_at = |i: u64| -> usize {
        (i as u128 * sample_rate as u128 * fps.den() as u128 / fps.num() as u128) as usize
    };
    let total = samples_at(spec.frames as u64);
    let full_audio = match spec.audio {
        Audio::None => None,
        Audio::Stereo => Some(audio::stereo(sample_rate, total)),
        Audio::AmbiX { azimuth, elevation } => Some(audio::ambix_source(
            sample_rate,
            total,
            azimuth as f64,
            elevation as f64,
        )),
    };
    for i in 0..spec.frames {
        let img = render_equirect(spec.pattern, spec.width, spec.height, i, 2);
        w.push_video(&yuv::to_yuv420(&img, spec.bits))?;
        if let Some(a) = &full_audio {
            let (s, e) = (samples_at(i as u64), samples_at(i as u64 + 1));
            let chunk: Vec<Vec<f32>> = a.iter().map(|c| c[s..e].to_vec()).collect();
            w.push_audio(&chunk)?;
        }
    }
    if spec.spherical {
        let mut edits = vec![EntryEdit {
            track: TrackKind::Video,
            boxes: [
                write::st3d(StereoMode::Mono),
                write::sv3d_equirect(spherecut_core::brand::APP_NAME),
            ]
            .concat(),
        }];
        if matches!(spec.audio, Audio::AmbiX { .. }) {
            edits.push(EntryEdit {
                track: TrackKind::Audio,
                boxes: write::sa3d_first_order_ambix(),
            });
        }
        w.finish_with_edits(&edits)?;
    } else {
        w.finish()?;
    }
    Ok(out)
}

/// The standard set written by `cargo xtask gen-media`.
pub fn standard_set() -> Vec<Spec> {
    let base = |name: &str| Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width: 1024,
        height: 512,
        rate: FrameRate::FPS_30,
        frames: 30,
        codec: Codec::Hevc,
        bits: 8,
        audio: Audio::Stereo,
        spherical: true,
    };
    vec![
        Spec {
            ..base("grid-hevc8-30")
        },
        Spec {
            bits: 10,
            rate: FrameRate::FPS_29_97,
            ..base("grid-hevc10-2997")
        },
        Spec {
            codec: Codec::H264,
            rate: FrameRate::FPS_59_94,
            frames: 60,
            ..base("grid-h264-5994")
        },
        Spec {
            codec: Codec::H264,
            rate: FrameRate::FPS_25,
            frames: 25,
            ..base("grid-h264-25")
        },
        // High 10, which common GPUs don't decode: the software path.
        Spec {
            codec: Codec::H264,
            bits: 10,
            ..base("grid-h264-10bit-30")
        },
        Spec {
            spherical: false,
            ..base("grid-hevc8-30-untagged")
        },
        // Front left and up, where Y, Z and X all differ, so a swapped
        // channel shows.
        Spec {
            audio: Audio::AmbiX {
                azimuth: 30,
                elevation: 20,
            },
            ..base("grid-hevc8-30-ambix")
        },
        Spec {
            codec: Codec::HevcLossless,
            bits: 10,
            frames: 10,
            audio: Audio::None,
            ..base("grid-hevc10-lossless")
        },
        Spec {
            codec: Codec::H264Lossless,
            frames: 10,
            audio: Audio::None,
            ..base("grid-h264-lossless")
        },
        Spec {
            pattern: Pattern::HighFrequency,
            width: 2048,
            height: 1024,
            codec: Codec::HevcLossless,
            frames: 2,
            audio: Audio::None,
            ..base("hifreq-hevc-lossless")
        },
        Spec {
            pattern: Pattern::Seam,
            codec: Codec::HevcLossless,
            frames: 2,
            audio: Audio::None,
            ..base("seam-hevc-lossless")
        },
        Spec {
            width: 3840,
            height: 1920,
            frames: 30,
            ..base("grid-hevc8-30-4k")
        },
    ]
}

/// Broken files derived from a good one: truncated at several points,
/// corrupted inside `moov`, empty, and not media at all.
pub fn write_bad_files(good: &Path, dir: &Path) -> Result<Vec<PathBuf>, SynthError> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |error| SynthError::Io { path, error }
    };
    let bytes = std::fs::read(good).map_err(io_err(good))?;
    let mut out = Vec::new();
    let mut put = |name: &str, data: &[u8]| -> Result<(), SynthError> {
        let p = dir.join(name);
        std::fs::write(&p, data).map_err(io_err(&p))?;
        out.push(p);
        Ok(())
    };
    put("bad-empty.mp4", &[])?;
    put("bad-text.mp4", b"this is not a video file\n")?;
    put("bad-trunc-header.mp4", &bytes[..bytes.len().min(20)])?;
    put("bad-trunc-half.mp4", &bytes[..bytes.len() / 2])?;
    put(
        "bad-trunc-end.mp4",
        &bytes[..bytes.len().saturating_sub(100)],
    )?;
    let corrupt = corrupt_sample_tables(&bytes).map_err(|e| SynthError::Io {
        path: good.to_path_buf(),
        error: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
    })?;
    put("bad-corrupt-moov.mp4", &corrupt)?;
    Ok(out)
}

/// `bytes` with the sample tables in `moov` damaged: the top byte of every
/// chunk offset (`stco`/`co64`) flipped, so the chunks lie far past the end
/// of a small file. FFmpeg still opens such a file (it refuses one whose
/// sample sizes are absurd); only checking the tables shows the damage.
fn corrupt_sample_tables(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let tree = mp4::read(&mut std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut out = bytes.to_vec();
    let mut flipped = 0;
    for trak in tree.tracks() {
        let Some(stbl) = trak.path(&[b"mdia", b"minf", b"stbl"]) else {
            continue;
        };
        // Entries start after the version, flags and entry count.
        for (t, width) in [(b"stco", 4), (b"co64", 8)] {
            let Some(b) = stbl.child(t) else { continue };
            let start = (b.offset + b.header_len) as usize + 8;
            let end = (b.offset + b.size) as usize;
            for at in (start..end).step_by(width) {
                if let Some(x) = out.get_mut(at) {
                    *x ^= 0xa5;
                    flipped += 1;
                }
            }
        }
    }
    if flipped == 0 {
        return Err("no sample tables in `moov` to corrupt".into());
    }
    Ok(out)
}
