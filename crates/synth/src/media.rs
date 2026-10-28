//! Writing synthetic patterns to real media files, plus deliberately broken
//! files for robustness tests.

use std::path::{Path, PathBuf};

use spherecut_core::{FrameRate, Rational};
use spherecut_media::encode::{
    AudioSettings, ColorTags, EncodeError, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes,
};
use spherecut_media::inject::{self, EntryEdit, TrackKind};
use spherecut_media::mp4::{self, Mp4Tree};
use spherecut_media::spherical::{StereoMode, write};
use thiserror::Error;

use crate::yuv::{Chroma, YuvFormat};
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
    /// Frames written, those an edit list hides included.
    pub frames: u32,
    pub codec: Codec,
    pub bits: u32,
    pub audio: Audio,
    /// Write Spherical Video V2 (and SA3D for ambiX) metadata.
    pub spherical: bool,
    pub chroma: Chroma,
    /// The colour the stream is tagged with. The pixels follow its matrix
    /// and range.
    pub color: ColorTags,
    /// The GOP structure, or `None` for the encoder's own.
    pub gop: Option<Gop>,
    /// Frames at the start that an edit list hides (see
    /// [`VideoSettings::edit_start`]). Audio, if any, still starts with
    /// the first frame written.
    pub edit_start: u32,
    /// A true variable frame rate: frame `i` lasts [`vfr_ticks`]`(i)` of
    /// 1/90000 s. `rate` is then only the encoder's hint, and the clip
    /// can't have audio.
    pub vfr: bool,
    /// Noise from this seed on every sample, new each frame (1 to 4 levels
    /// up or down at 8 bits, scaled with the depth), so that the encoder
    /// needs a camera's bitrate and the decoder does a camera's work: the
    /// M1 plan's noisy perf clips. A clean grid decodes several times
    /// faster than footage. At 5760x2880 and 30 fps, x265 (`veryfast`,
    /// CRF 18) makes about 200 Mbps of it.
    pub noise: Option<u64>,
    /// Luma columns the stream crops on the left, an even number: the
    /// clip is rendered and coded `width` wide, and decodes `crop_left`
    /// narrower, without the pattern's left columns (see
    /// [`VideoSettings::crop_left`]).
    pub crop_left: u32,
    /// The reference frames x264 or x265 may keep (their `ref`), or
    /// `None` for the preset's own: 1 at `veryfast`, 3 at x264's default
    /// `medium`.
    pub refs: Option<u32>,
    /// Frames the encoder is made to code as I-frames
    /// ([`Mp4Writer::push_video_intra`]). In an open GOP they are not
    /// keyframes unless `keyint`'s `min-keyint` (at most half of it) has
    /// passed since the last: x264 codes them with no recovery point, and
    /// the frames after them may refer to frames before them, as it codes
    /// a flash. Not with `vfr`.
    pub intra: Vec<u32>,
    /// Frames shown as a flash: the pattern halfway to white.
    pub flash: Vec<u32>,
    /// Frames of another scene, cut to and back from: the pattern with its
    /// colours inverted.
    pub cut_away: Vec<u32>,
    /// With `gop`, the encoder's own scene-cut detection at its default
    /// threshold (40), which `gop` otherwise turns off: x264 codes a frame
    /// unlike the one before (a [`cut_away`](Self::cut_away), and the
    /// frame after it) as an I-frame, an IDR picture once `min-keyint` has
    /// passed and otherwise one the frames after it may refer across.
    /// Without B-frames x264 makes every frame [`intra`](Self::intra)
    /// forces IDR, so this is how such a stream gets the latter.
    pub scenecut: bool,
}

/// Keyframes every `keyint` frames, with `bframes` B-frames between
/// references. An open GOP lets the B-frames before a keyframe (in
/// presentation order) refer to the GOP before it, so they are dropped
/// when decoding starts at that keyframe: x264's `open-gop`, and x265's
/// CRA pictures with RASL leading pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gop {
    pub keyint: u32,
    pub bframes: u32,
    pub open: bool,
    /// HEVC only: RADL pictures, B-frames shown before each keyframe but
    /// decoded after it from it alone (x265's `radl`, for closed GOPs).
    pub radl: u32,
}

/// The tags of synth's clips: BT.709, limited range, and centre-sited
/// chroma, since [`yuv::to_yuv`] averages each 2×2 block of 4:2:0. Its
/// 4:2:2 is co-sited, which H.264 and HEVC don't code: their encoders
/// leave this location out for it, and FFmpeg reads it as left.
pub const CLIP_COLOR: ColorTags = ColorTags::BT709_LIMITED.with_chroma_location(2);

/// Ticks per second in [`Spec::vfr`] clips.
pub const VFR_TIMESCALE: i64 = 90_000;

/// How long frame `i` of a [`Spec::vfr`] clip lasts, in ticks of
/// 1/[`VFR_TIMESCALE`] s: 3000 to 6000 ticks (1/30 to 1/15 s) in steps
/// of 750, in an order with no short period.
pub fn vfr_ticks(i: u32) -> i64 {
    3000 + i64::from(i.wrapping_mul(7919) % 5) * 750
}

impl Default for Spec {
    /// A 256x128 H.264 grid clip of 30 frames at 30 fps, without audio
    /// or spherical metadata.
    fn default() -> Self {
        Spec {
            name: "clip".into(),
            pattern: Pattern::Grid,
            width: 256,
            height: 128,
            rate: FrameRate::FPS_30,
            frames: 30,
            codec: Codec::H264,
            bits: 8,
            audio: Audio::None,
            spherical: false,
            chroma: Chroma::Yuv420,
            color: CLIP_COLOR,
            gop: None,
            edit_start: 0,
            vfr: false,
            noise: None,
            crop_left: 0,
            refs: None,
            intra: Vec::new(),
            flash: Vec::new(),
            cut_away: Vec::new(),
            scenecut: false,
        }
    }
}

impl Spec {
    pub fn file_name(&self) -> String {
        format!("{}.mp4", self.name)
    }

    /// How the pixels are written.
    pub fn yuv_format(&self) -> YuvFormat {
        YuvFormat::tagged(&self.color, self.chroma)
    }

    fn layout(&self) -> PixelLayout {
        match (self.chroma, self.bits > 8) {
            (Chroma::Yuv420, false) => PixelLayout::Yuv420p,
            (Chroma::Yuv420, true) => PixelLayout::Yuv420p10,
            (Chroma::Yuv422, false) => PixelLayout::Yuv422p,
            (Chroma::Yuv422, true) => PixelLayout::Yuv422p10,
            (Chroma::Yuv444, false) => PixelLayout::Yuv444p,
            (Chroma::Yuv444, true) => PixelLayout::Yuv444p10,
        }
    }

    fn video_settings(&self) -> Result<VideoSettings, EncodeError> {
        let (encoder, mut options, mut params) = match self.codec {
            Codec::H264 => (
                "libx264",
                vec![("crf", "16"), ("preset", "veryfast")],
                vec![],
            ),
            Codec::Hevc => (
                "libx265",
                vec![("crf", "18"), ("preset", "veryfast")],
                vec!["log-level=error".to_string()],
            ),
            Codec::H264Lossless => ("libx264", vec![("qp", "0"), ("preset", "veryfast")], vec![]),
            Codec::HevcLossless => (
                "libx265",
                vec![("preset", "veryfast")],
                vec!["lossless=1".to_string(), "log-level=error".to_string()],
            ),
        };
        if let Some(g) = self.gop {
            // x264 and x265 spell these the same.
            params.push(format!(
                "keyint={k}:min-keyint={k}:bframes={b}:scenecut={s}:open-gop={o}",
                k = g.keyint,
                b = g.bframes,
                s = if self.scenecut { 40 } else { 0 },
                o = u8::from(g.open)
            ));
            if g.radl > 0 {
                if encoder != "libx265" {
                    return Err(EncodeError::Invalid("only HEVC has RADL pictures".into()));
                }
                params.push(format!("radl={}", g.radl));
            }
        }
        if let Some(r) = self.refs {
            params.push(format!("ref={r}"));
        }
        // Single-threaded encodes keep the bitstream reproducible.
        options.push(("threads", "1"));
        let mut options: Vec<(String, String)> = options
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if !params.is_empty() {
            let key = if encoder == "libx265" {
                "x265-params"
            } else {
                "x264-params"
            };
            options.push((key.into(), params.join(":")));
        }
        let time_base = match self.vfr {
            true => Some(
                Rational::new(1, VFR_TIMESCALE)
                    .map_err(|e| EncodeError::Invalid(format!("time base: {e}")))?,
            ),
            false => None,
        };
        Ok(VideoSettings {
            encoder: encoder.into(),
            width: self.width,
            height: self.height,
            rate: self.rate,
            layout: self.layout(),
            options,
            color: self.color,
            edit_start: self.edit_start,
            time_base,
            crop_left: self.crop_left,
        })
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
    let mut w = Mp4Writer::create(&out, &spec.video_settings()?, audio_settings.as_ref())?;
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
    if spec.vfr && full_audio.is_some() {
        return Err(
            EncodeError::Invalid("a variable frame rate clip can't have audio".into()).into(),
        );
    }
    if spec.vfr && !spec.intra.is_empty() {
        return Err(
            EncodeError::Invalid("a variable frame rate clip can't force I-frames".into()).into(),
        );
    }
    let format = spec.yuv_format();
    let mut pts = 0;
    for i in 0..spec.frames {
        let mut img = render_equirect(spec.pattern, spec.width, spec.height, i, 2);
        if spec.flash.contains(&i) {
            for px in &mut img.pixels {
                *px = px.map(|c| 0.5 + c / 2.0);
            }
        }
        if spec.cut_away.contains(&i) {
            for px in &mut img.pixels {
                *px = px.map(|c| 1.0 - c);
            }
        }
        let mut planes = yuv::to_yuv(&img, spec.bits, &format);
        if let Some(seed) = spec.noise {
            add_noise(&mut planes, spec.bits, seed, i);
        }
        if spec.vfr {
            w.push_video_at(&planes, pts)?;
            pts += vfr_ticks(i);
        } else if spec.intra.contains(&i) {
            w.push_video_intra(&planes)?;
        } else {
            w.push_video(&planes)?;
        }
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

/// Add [`Spec::noise`]'s noise for frame `frame` to `planes`, of `bits`
/// bits, clamped to the samples' range.
fn add_noise(planes: &mut YuvPlanes, bits: u32, seed: u64, frame: u32) {
    // xorshift64*, from a state that is never 0.
    let mut state = (seed ^ u64::from(frame).wrapping_mul(0x9e37_79b9_7f4a_7c15)) | 1;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    // One draw gives 21 offsets of 3 bits each, -4 to 4 but never 0.
    let offsets = |n: usize, next: &mut dyn FnMut() -> u64| -> Vec<i32> {
        const LEVELS: [i32; 8] = [-4, -3, -2, -1, 1, 2, 3, 4];
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let mut r = next();
            for _ in 0..21 {
                out.push(LEVELS[(r & 7) as usize]);
                r >>= 3;
            }
        }
        out.truncate(n);
        out
    };
    let scale = 1i32 << bits.saturating_sub(8).min(8);
    let max = (1i32 << bits.min(16)) - 1;
    match planes {
        YuvPlanes::U8(p) => {
            for plane in p.iter_mut() {
                let noise = offsets(plane.len(), &mut next);
                for (v, o) in plane.iter_mut().zip(noise) {
                    *v = (i32::from(*v) + o).clamp(0, 255) as u8;
                }
            }
        }
        YuvPlanes::U16(p) => {
            for plane in p.iter_mut() {
                let noise = offsets(plane.len(), &mut next);
                for (v, o) in plane.iter_mut().zip(noise) {
                    *v = (i32::from(*v) + o * scale).clamp(0, max) as u16;
                }
            }
        }
    }
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
        ..Spec::default()
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
    ("bad-trak-renamed.mp4", rename_last_trak),
    ("bad-timescale.mp4", zero_first_timescale),
    ("bad-codec-config.mp4", flip_first_codec_config),
    ("bad-audio-config.mp4", reserve_audio_sample_rate),
    ("bad-projection.mp4", rename_first_proj),
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

/// One bit of the last `trak`'s type flipped so that it reads `Trak`,
/// which is still text. FFmpeg skips it just the same.
fn rename_last_trak(tree: &Mp4Tree, out: &mut [u8]) -> bool {
    let Some(trak) = tree.tracks().last() else {
        return false;
    };
    out.get_mut(trak.offset as usize + 4)
        .map(|x| *x ^= 0x20)
        .is_some()
}

/// One bit of the type of the first `proj` (Spherical Video V2) flipped
/// so that it reads `qroj`. FFmpeg opens the file and says "Missing
/// projection box" in its log, and the projection is gone.
fn rename_first_proj(tree: &Mp4Tree, out: &mut [u8]) -> bool {
    let proj = tree
        .tracks()
        .filter_map(|t| t.path(&[b"mdia", b"minf", b"stbl", b"stsd"]))
        .flat_map(|stsd| &stsd.children)
        .find_map(|entry| entry.path(&[b"sv3d", b"proj"]));
    let Some(proj) = proj else {
        return false;
    };
    out.get_mut(proj.offset as usize + 4)
        .map(|x| *x ^= 0x01)
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

/// Two 64x64 frames of MJPEG in MP4 whose sample entry and frame headers
/// say each picture is `width` × `height`: a file that declares pictures
/// over [`spherecut_media::MAX_PIXELS`] without being as large as one.
/// Decoders limited to it stop at the frame header, before they read the
/// picture data. The file is written and then changed in place.
pub fn write_mjpeg_declaring(
    dir: &Path,
    name: &str,
    width: u16,
    height: u16,
) -> Result<PathBuf, SynthError> {
    let invalid = |path: &Path, why: &str| SynthError::Io {
        path: path.to_path_buf(),
        error: std::io::Error::new(std::io::ErrorKind::InvalidData, why.to_string()),
    };
    let path = dir.join(name);
    let settings = VideoSettings {
        encoder: "mjpeg".into(),
        width: 64,
        height: 64,
        rate: FrameRate::FPS_30,
        layout: PixelLayout::Yuv420p,
        // FFmpeg's MJPEG encoder takes limited-range 4:2:0 only as an
        // unofficial extension.
        options: vec![("strict".into(), "-1".into())],
        color: ColorTags::BT709_LIMITED,
        edit_start: 0,
        time_base: None,
        crop_left: 0,
    };
    let mut w = Mp4Writer::create(&path, &settings, None)?;
    for _ in 0..2 {
        let chroma = vec![128; 32 * 32];
        w.push_video(&YuvPlanes::U8([vec![100; 64 * 64], chroma.clone(), chroma]))?;
    }
    w.finish()?;

    let mut bytes = std::fs::read(&path).map_err(|error| SynthError::Io {
        path: path.clone(),
        error,
    })?;
    let tree =
        mp4::read(&mut std::io::Cursor::new(&bytes)).map_err(|e| invalid(&path, &e.to_string()))?;
    let entry = tree
        .tracks()
        .next()
        .and_then(|t| t.path(&[b"mdia", b"minf", b"stbl", b"stsd"]))
        .and_then(|stsd| stsd.children.first())
        .ok_or_else(|| invalid(&path, "no sample entry"))?;
    // A visual sample entry's width and height follow 24 bytes of
    // reserved and predefined fields.
    let at = (entry.offset + entry.header_len) as usize + 24;
    let size = [width.to_be_bytes(), height.to_be_bytes()].concat();
    bytes
        .get_mut(at..at + 4)
        .ok_or_else(|| invalid(&path, "sample entry cut short"))?
        .copy_from_slice(&size);
    let mdat = tree
        .top
        .iter()
        .find(|b| b.fourcc.0 == *b"mdat")
        .ok_or_else(|| invalid(&path, "no mdat"))?;
    let data = bytes
        .get_mut((mdat.offset + mdat.header_len) as usize..(mdat.offset + mdat.size) as usize)
        .ok_or_else(|| invalid(&path, "mdat cut short"))?;
    if set_jpeg_frame_sizes(data, width, height) != 2 {
        return Err(invalid(&path, "not two JPEG frame headers in mdat"));
    }
    std::fs::write(&path, &bytes).map_err(|error| SynthError::Io {
        path: path.clone(),
        error,
    })?;
    Ok(path)
}

/// Set `width` and `height` in the baseline frame header (SOF0) of each
/// JPEG picture in `data`, and say how many it changed. Each picture
/// starts with SOI (FF D8); its header segments follow, each a marker
/// (FF xx) and a 16-bit length that counts itself. Entropy-coded data
/// never holds FF D8, since FF is followed there by 00 or a restart
/// marker.
fn set_jpeg_frame_sizes(data: &mut [u8], width: u16, height: u16) -> usize {
    let mut changed = 0;
    let mut at = 0;
    while let Some(soi) = data
        .get(at..)
        .and_then(|d| d.windows(3).position(|w| w == [0xff, 0xd8, 0xff]))
    {
        let mut m = at + soi + 2;
        while let Some(&[0xff, marker, hi, lo]) = data.get(m..m + 4) {
            if marker == 0xc0 {
                // After the length, the sample precision, then the height
                // and width.
                let size = [height.to_be_bytes(), width.to_be_bytes()].concat();
                if let Some(d) = data.get_mut(m + 5..m + 9) {
                    d.copy_from_slice(&size);
                    changed += 1;
                }
                break;
            }
            m += 2 + u16::from_be_bytes([hi, lo]) as usize;
        }
        at = m.max(at + soi + 3);
    }
    changed
}

/// Remux `src` into `dir` as Matroska, with the same name and `.mkv`
/// (see [`spherecut_media::encode::remux_to_matroska`]: timestamps in
/// milliseconds, and no edit list).
pub fn remux_to_mkv(src: &Path, dir: &Path) -> Result<PathBuf, SynthError> {
    let name = src.file_stem().ok_or_else(|| SynthError::Io {
        path: src.to_path_buf(),
        error: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"),
    })?;
    let dst = dir.join(name).with_extension("mkv");
    Ok(spherecut_media::encode::remux_to_matroska(src, &dst)?)
}

/// Remux `src` into the Matroska file `dst` with a default duration of
/// one frame at `rate`, whatever the frames' timestamps: a variable-rate
/// file that says a standard rate (see
/// [`spherecut_media::encode::remux_to_matroska_at`]).
pub fn remux_to_mkv_at(src: &Path, dst: &Path, rate: FrameRate) -> Result<PathBuf, SynthError> {
    Ok(spherecut_media::encode::remux_to_matroska_at(
        src,
        dst,
        Some(rate),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_seeded_new_each_frame_and_bounded() {
        let flat8 = || YuvPlanes::U8([vec![128; 64], vec![128; 16], vec![2; 16]]);
        let noisy = |seed, frame| {
            let mut p = flat8();
            add_noise(&mut p, 8, seed, frame);
            p
        };
        let samples = |p: &YuvPlanes| match p {
            YuvPlanes::U8(p) => p.concat().into_iter().map(i32::from).collect::<Vec<_>>(),
            YuvPlanes::U16(p) => p.concat().into_iter().map(i32::from).collect(),
        };
        let a = samples(&noisy(7, 0));
        assert_eq!(a, samples(&noisy(7, 0)));
        assert_ne!(a, samples(&noisy(7, 1)));
        assert_ne!(a, samples(&noisy(8, 0)));
        // 1 to 4 levels around 128, and clamped at 0 near the bottom.
        assert!(
            a[..80].iter().all(|v| (124..=132).contains(v) && *v != 128),
            "{a:?}"
        );
        assert!(a[80..].iter().all(|v| (0..=6).contains(v)), "{a:?}");
        assert!(a[80..].contains(&0));
        // Scaled with the depth at 10 bits.
        let mut p = YuvPlanes::U16([vec![512; 64], vec![1020; 16], vec![512; 16]]);
        add_noise(&mut p, 10, 7, 0);
        let b = samples(&p);
        assert!(b[..64].iter().all(|v| (496..=528).contains(v)), "{b:?}");
        assert!(b[64..80].iter().all(|v| *v <= 1023), "{b:?}");
        assert!(b.iter().any(|v| (v - 512).abs() > 8));
    }
}
