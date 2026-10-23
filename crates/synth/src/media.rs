//! Writing synthetic patterns to real media files, plus deliberately broken
//! files for robustness tests.

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::encode::{AudioSettings, Mp4Writer, PixelLayout, VideoSettings};
use spherecut_media::inject::{self, EntryEdit, TrackKind};
use spherecut_media::mp4::{self, Mp4Tree};
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

/// Broken files derived from a good one with video and audio: truncated at
/// several points, corrupted inside `moov`, empty, and not media at all.
pub fn write_bad_files(good: &Path, dir: &Path) -> Result<Vec<PathBuf>, SynthError> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |error| SynthError::Io { path, error }
    };
    let invalid = |why: String| SynthError::Io {
        path: good.to_path_buf(),
        error: std::io::Error::new(std::io::ErrorKind::InvalidData, why),
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
    let tree = mp4::read(&mut std::io::Cursor::new(&bytes)).map_err(|e| invalid(e.to_string()))?;
    for (name, corrupt) in CORRUPTIONS {
        let mut b = bytes.clone();
        if !corrupt(&tree, &mut b) {
            return Err(invalid(format!("{name}: nothing in `moov` to corrupt")));
        }
        put(name, &b)?;
    }
    Ok(out)
}

/// Changes a file's bytes in `moov`, as parsed in the tree, and says
/// whether it found what it changes.
type Corruption = fn(&Mp4Tree, &mut [u8]) -> bool;

/// Damage inside `moov` that FFmpeg opens without complaint. Only probe's
/// checks find it.
const CORRUPTIONS: &[(&str, Corruption)] = &[
    ("bad-corrupt-moov.mp4", flip_chunk_offsets),
    ("bad-trak-type.mp4", flip_last_trak_type),
    ("bad-timescale.mp4", zero_first_timescale),
    ("bad-codec-config.mp4", flip_first_codec_config),
    ("bad-audio-config.mp4", reserve_audio_sample_rate),
];

/// The top byte of every chunk offset (`stco`/`co64`) flipped, so the
/// chunks lie far past the end of a small file. FFmpeg still opens such a
/// file (it refuses one whose sample sizes are absurd); only checking the
/// tables shows the damage.
fn flip_chunk_offsets(tree: &Mp4Tree, out: &mut [u8]) -> bool {
    let mut flipped = false;
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
                    flipped = true;
                }
            }
        }
    }
    flipped
}

/// The first byte of the last `trak`'s type flipped, as random flips once
/// did to the audio track in the M0 review: FFmpeg skips the box as one it
/// does not know, and the stream is gone without a word.
fn flip_last_trak_type(tree: &Mp4Tree, out: &mut [u8]) -> bool {
    let Some(trak) = tree.tracks().last() else {
        return false;
    };
    out.get_mut(trak.offset as usize + 4)
        .map(|x| *x ^= 0xa5)
        .is_some()
}

/// The first track's `mdhd` timescale set to 0. FFmpeg takes 1 instead and
/// says so only in its log, and every time in the track is wrong.
fn zero_first_timescale(tree: &Mp4Tree, out: &mut [u8]) -> bool {
    let Some(mdhd) = tree
        .tracks()
        .next()
        .and_then(|t| t.path(&[b"mdia", b"mdhd"]))
    else {
        return false;
    };
    // After the version, flags, and creation and modification times.
    let times = if mdhd.payload.first() == Some(&1) {
        16
    } else {
        8
    };
    let at = (mdhd.offset + mdhd.header_len) as usize + 4 + times;
    out.get_mut(at..at + 4).map(|ts| ts.fill(0)).is_some()
}

/// Every byte of the first track's codec configuration (`avcC`, `hvcC`)
/// flipped. FFmpeg opens the file, but its decoder cannot set up from it,
/// so the stream has no pixel format.
fn flip_first_codec_config(tree: &Mp4Tree, out: &mut [u8]) -> bool {
    let config = tree
        .tracks()
        .next()
        .and_then(|t| t.path(&[b"mdia", b"minf", b"stbl", b"stsd"]))
        .and_then(|stsd| stsd.children.first())
        .and_then(|entry| {
            entry
                .children
                .iter()
                .find(|b| matches!(&b.fourcc.0, b"avcC" | b"hvcC"))
        });
    let Some(c) = config else {
        return false;
    };
    let (start, end) = (
        (c.offset + c.header_len) as usize,
        (c.offset + c.size) as usize,
    );
    out.get_mut(start..end)
        .map(|b| b.iter_mut().for_each(|x| *x ^= 0xa5))
        .is_some()
}

/// The sampling frequency index of the first audio track's AAC setup (the
/// AudioSpecificConfig in `esds`) set to 13, which is reserved. FFmpeg
/// opens the file and takes the sample rate and channels from the sample
/// entry, so the stream looks set up, but its AAC decoder refuses to open.
fn reserve_audio_sample_rate(tree: &Mp4Tree, out: &mut [u8]) -> bool {
    let esds = tree
        .tracks()
        .filter(|t| mp4::handler_type(t).is_some_and(|h| h.0 == *b"soun"))
        .find_map(|t| {
            let stsd = t.path(&[b"mdia", b"minf", b"stbl", b"stsd"])?;
            stsd.children.first()?.child(b"esds")
        });
    let Some((esds, config)) = esds.and_then(|e| Some((e, audio_specific_config(&e.payload)?)))
    else {
        return false;
    };
    let at = (esds.offset + esds.header_len) as usize + config;
    let Some(b) = out.get_mut(at..at + 2) else {
        return false;
    };
    // Five bits of object type, then four of sampling frequency index.
    let v = u16::from_be_bytes([b[0], b[1]]) & !(0xf << 7) | 13 << 7;
    b.copy_from_slice(&v.to_be_bytes());
    true
}

/// Where the AudioSpecificConfig starts in an `esds` payload, laid out as
/// FFmpeg writes it: the version and flags, an ES descriptor without its
/// optional fields, then a decoder config descriptor holding the decoder
/// specific info.
fn audio_specific_config(esds: &[u8]) -> Option<usize> {
    let mut at = 4;
    // Each descriptor's tag, and the fixed fields before the next one.
    for (tag, fixed) in [(3, 3), (4, 13), (5, 0)] {
        if *esds.get(at)? != tag {
            return None;
        }
        at += 1;
        // Up to four length bytes; all but the last have the top bit set.
        for _ in 0..4 {
            at += 1;
            if esds.get(at - 1)? & 0x80 == 0 {
                break;
            }
        }
        // The ES descriptor's flags, after its ID, say whether optional
        // fields follow.
        if tag == 3 && esds.get(at + 2)? & 0xe0 != 0 {
            return None;
        }
        at += fixed;
    }
    (at + 2 <= esds.len()).then_some(at)
}
