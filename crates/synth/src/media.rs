//! Writing synthetic patterns to real media files, plus deliberately broken
//! files for robustness tests.

use std::path::{Path, PathBuf};

use spherecut_core::{FrameRate, Rational, Time};
use spherecut_media::encode::{
    self, AudioSettings, ColorTags, EncodeError, Mp4Writer, MuxedAs, PixelLayout, VideoSettings,
    YuvPlanes,
};
use spherecut_media::inject::{self, EntryEdit, TrackKind};
use spherecut_media::mp4::{self, Mp4Tree};
use spherecut_media::packets::{NextPacket, PacketReader};
use spherecut_media::spherical::{StereoMode, write};
use thiserror::Error;

use crate::code::{self, CodeError};
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
    #[error(transparent)]
    Code(#[from] CodeError),
    /// A file synth reads back isn't what it should be.
    #[error("{}: {why}", path.display())]
    Read { path: PathBuf, why: String },
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
    /// 440 Hz left, 660 Hz right ([`audio::stereo`]).
    Stereo,
    /// 440 Hz in one channel ([`audio::mono`]).
    Mono,
    /// 5.1: 440 Hz front left, 660 Hz front right, the rest silent
    /// ([`audio::surround`]).
    Surround,
    /// First-order ambiX with a source at (azimuth, elevation) degrees.
    AmbiX {
        azimuth: i32,
        elevation: i32,
    },
    /// First-order ambiX with a different tone in each channel
    /// ([`audio::ambix_tones`]).
    AmbiXTones,
    /// Stereo silence with a 1 kHz beep of [`audio::BEEP_MS`] starting at
    /// the presentation time of each of [`Spec::white`]'s frames, the same
    /// in both channels ([`audio::beeps`]): `sync-flash-beep`'s.
    Beeps,
    /// Stereo 1 kHz at known levels, silence and clipping, by time
    /// ([`audio::levels`]): `levels`'.
    Levels,
}

/// How a clip's audio is coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// FFmpeg's `aac`, without noise substitution.
    Aac,
    /// 16-bit PCM, which FFmpeg's MP4 muxer stores as ISO `ipcm`.
    Pcm,
    /// FFmpeg's own Opus encoder, which is experimental.
    Opus,
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
    /// What the audio is coded as.
    pub audio_codec: AudioCodec,
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
    /// CRF 18) makes about 200 Mbps of it. Two seeds that differ only in
    /// their lowest bit give the same noise, since the generator's state
    /// is made odd.
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
    /// Frames shown pure white (the flashes of `sync-flash-beep`).
    pub white: Vec<u32>,
    /// The audio's sample rate.
    pub sample_rate: u32,
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
    /// An average bitrate in kbit/s, for x264 or x265 in place of their
    /// CRF (their one-pass ABR), so that a noisy clip costs the decoder
    /// what a camera's bitrate does: [`perf_clips`].
    pub bitrate_kbps: Option<u32>,
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
            audio_codec: AudioCodec::Aac,
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
            white: Vec::new(),
            sample_rate: 48_000,
            cut_away: Vec::new(),
            scenecut: false,
            bitrate_kbps: None,
        }
    }
}

/// The most frames x265 holds back for B-frames at the presets synth
/// uses (a B-pyramid): a clip no longer than this is written without
/// B-frames (see `Spec::video_settings`).
pub const X265_BFRAME_DELAY: u32 = 2;

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
        // x265 (3.5, and its master in 2026) leaves the dts of the frames
        // within its B-frame delay undefined until the frame after that
        // delay arrives (`Encoder::m_bframeDelayTime` is set only then, and
        // never initialised), so a clip of no more frames than the delay
        // (two at the presets here, which use a B-pyramid) gets its pts
        // minus whatever x265's heap held. FFmpeg 6.1's mov muxer writes
        // that; 8.1's refuses it now and then ("pts/dts pair unsupported",
        // `AVERROR_PATCHWELCOME`). Without B-frames the dts is the pts.
        if encoder == "libx265" && self.frames <= X265_BFRAME_DELAY {
            params.push("bframes=0".into());
        }
        // Single-threaded encodes keep the bitstream reproducible.
        options.push(("threads", "1"));
        let mut options: Vec<(String, String)> = options
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(kbps) = self.bitrate_kbps {
            if !matches!(self.codec, Codec::H264 | Codec::Hevc) {
                return Err(EncodeError::Invalid(
                    "a lossless clip has no bitrate to set".into(),
                ));
            }
            // FFmpeg's x264 and x265 wrappers take the context's bit rate
            // (`b`) only when no CRF is set.
            options.retain(|(k, _)| k != "crf");
            options.push(("b".into(), format!("{kbps}k")));
        }
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
            device: None,
        })
    }
}

/// Render and encode `spec` into `dir`. Returns the file path.
///
/// [`Mp4Writer`] renames a file into place only when it is complete, so if
/// the path exists it holds a whole clip, even after an interrupted run,
/// and callers may cache clips by whether the file exists.
pub fn write(spec: &Spec, dir: &Path) -> Result<PathBuf, SynthError> {
    write_with(spec, dir, Lens::NONE)
}

/// What a lens of a [`PairSpec`] adds to its [`Spec`]'s clip.
#[derive(Debug, Clone, Copy)]
struct Lens {
    /// The lens whose corner codes each frame carries, if any.
    code: Option<usize>,
    /// The encoder picks its own threads ([`PairSpec::threaded`]).
    threaded: bool,
}

impl Lens {
    /// A clip of its own.
    const NONE: Lens = Lens {
        code: None,
        threaded: false,
    };
}

/// [`write`], with what `lens` adds.
fn write_with(spec: &Spec, dir: &Path, lens: Lens) -> Result<PathBuf, SynthError> {
    std::fs::create_dir_all(dir).map_err(|error| SynthError::Io {
        path: dir.to_path_buf(),
        error,
    })?;
    let out = dir.join(spec.file_name());
    let sample_rate = spec.sample_rate;
    // No perceptual noise substitution: FFmpeg's AAC decoder fills those
    // bands from a random generator that a seek restarts, so a clip that
    // used it would decode differently after a seek, at its noise floor
    // (D35).
    let aac_options = vec![("aac_pns".to_string(), "0".to_string())];
    let audio_settings = match spec.audio {
        Audio::None => None,
        Audio::Stereo | Audio::Beeps | Audio::Levels => Some(AudioSettings {
            encoder: "aac".into(),
            sample_rate,
            channels: 2,
            bit_rate: 192_000,
            options: aac_options.clone(),
        }),
        Audio::Mono => Some(AudioSettings {
            encoder: "aac".into(),
            sample_rate,
            channels: 1,
            bit_rate: 96_000,
            options: aac_options.clone(),
        }),
        Audio::Surround => Some(AudioSettings {
            encoder: "aac".into(),
            sample_rate,
            channels: 6,
            bit_rate: 384_000,
            options: aac_options.clone(),
        }),
        Audio::AmbiX { .. } => Some(AudioSettings {
            encoder: "aac".into(),
            sample_rate,
            channels: 4,
            bit_rate: 256_000,
            options: aac_options.clone(),
        }),
        // No intensity stereo either, which codes the second and third
        // channels (a pair to AAC) as one.
        Audio::AmbiXTones => Some(AudioSettings {
            encoder: "aac".into(),
            sample_rate,
            channels: 4,
            bit_rate: 384_000,
            options: [aac_options.clone(), vec![("aac_is".into(), "0".into())]].concat(),
        }),
    };
    let audio_settings = audio_settings.map(|a| match spec.audio_codec {
        AudioCodec::Aac => a,
        AudioCodec::Pcm => AudioSettings {
            encoder: "pcm_s16le".into(),
            bit_rate: 0,
            options: Vec::new(),
            ..a
        },
        AudioCodec::Opus => AudioSettings {
            encoder: "opus".into(),
            bit_rate: 128_000 * a.channels as usize / 2,
            options: vec![("strict".into(), "experimental".into())],
            ..a
        },
    });
    let mut settings = spec.video_settings()?;
    if lens.threaded {
        for (k, v) in &mut settings.options {
            if k == "threads" {
                *v = "0".into();
            }
        }
    }
    let mut w = Mp4Writer::create(&out, &settings, audio_settings.as_ref())?;
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
        Audio::Mono => Some(audio::mono(sample_rate, total)),
        Audio::Surround => Some(audio::surround(sample_rate, total)),
        Audio::Beeps => {
            let onsets: Vec<usize> = spec
                .white
                .iter()
                .map(|&i| samples_at(u64::from(i)))
                .collect();
            Some(audio::beeps(sample_rate, total, &onsets))
        }
        Audio::Levels => Some(audio::levels(sample_rate, total)),
        Audio::AmbiXTones => Some(audio::ambix_tones(sample_rate, total)),
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
    let mut pts = 0;
    for i in 0..spec.frames {
        let planes = frame_planes(spec, lens.code, i)?;
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
        if matches!(spec.audio, Audio::AmbiX { .. } | Audio::AmbiXTones) {
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

/// Frame `i` of `spec`'s clip as [`write`] codes it: the pattern with the
/// spec's flash, white and cut-away frames, lens `code`'s corner codes if
/// any ([`PairSpec::corner_codes`]), and the spec's noise.
pub fn frame_planes(spec: &Spec, code: Option<usize>, i: u32) -> Result<YuvPlanes, SynthError> {
    let mut img = render_equirect(spec.pattern, spec.width, spec.height, i, 2);
    if spec.flash.contains(&i) {
        for px in &mut img.pixels {
            *px = px.map(|c| 0.5 + c / 2.0);
        }
    }
    if spec.white.contains(&i) {
        for px in &mut img.pixels {
            *px = px.map(|_| 1.0);
        }
    }
    if spec.cut_away.contains(&i) {
        for px in &mut img.pixels {
            *px = px.map(|c| 1.0 - c);
        }
    }
    if let Some(k) = code {
        code::corner_code(&mut img, k, i)?;
    }
    let mut planes = yuv::to_yuv(&img, spec.bits, &spec.yuv_format());
    if let Some(seed) = spec.noise {
        add_noise(&mut planes, spec.bits, seed, i);
    }
    Ok(planes)
}

/// The noise's format, raised whenever [`add_noise`] would add other
/// noise for the same seed and frame: its generator, its levels or its
/// scaling. The noise sets how hard a clip is to code and decode, so a
/// perf pair's stamp holds this and hw-test makes the pair again when it
/// changes; a test pins the noise it names. The perf clips of one stream
/// are stamped by their spec alone, as before the pairs.
pub const NOISE_FORMAT: u32 = 1;

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

/// `sync-flash-beep`'s name.
pub const SYNC_FLASH_BEEP: &str = "sync-flash-beep-2997";

/// `sync-flash-beep` (M1 plan section 9.3): 10 s of the grid at 29.97 fps,
/// where the frame shown at each whole second (the last at or before it)
/// is white, and a 1 kHz beep starts at that frame's presentation time.
/// For checking A/V sync by eye and ear, and for the decode test that
/// each beep starts within 1 ms of its frame.
pub fn sync_flash_beep() -> Spec {
    let rate = FrameRate::FPS_29_97;
    let frames = 300u32;
    let white = (0..10i64)
        .filter_map(|s| {
            rate.time_to_frame(Time::from_secs(s), spherecut_core::Rounding::Floor)
                .ok()
                .and_then(|f| u32::try_from(f).ok())
        })
        .filter(|&f| f < frames)
        .collect();
    Spec {
        name: SYNC_FLASH_BEEP.into(),
        pattern: Pattern::Grid,
        width: 1024,
        height: 512,
        rate,
        frames,
        codec: Codec::H264,
        audio: Audio::Beeps,
        spherical: true,
        white,
        ..Spec::default()
    }
}

/// `levels`' name.
pub const LEVELS: &str = "levels-3s";

/// `levels`: 3 s of the grid at 30 fps with [`Audio::Levels`]: a loud
/// tone, silence, a clipped tone and a quiet one, at the times
/// [`audio::LEVELS_LOUD`] and the others give. For `analyze`'s audio
/// measures.
pub fn levels() -> Spec {
    Spec {
        name: LEVELS.into(),
        frames: 90,
        audio: Audio::Levels,
        ..Spec::default()
    }
}

/// `jitter-dups`' name.
pub const JITTER_DUPS: &str = "jitter-dups";

/// `jitter-dups`' slots: 30 a second for 10 s.
pub const JITTER_SLOTS: u32 = 300;

/// Ticks of 1/[`VFR_TIMESCALE`] s from one slot to the next: 1/30 s.
pub const JITTER_SLOT_TICKS: i64 = 3000;

/// Slots shown [`JITTER_TICKS`] early (even ones) or late (odd ones), so
/// that the frames there last 0.8 and 1.2 slots by turns: ±20 %.
pub const JITTER_RANGE: std::ops::Range<u32> = 60..90;

/// How far [`JITTER_RANGE`]'s slots move, in ticks: a tenth of a slot.
pub const JITTER_TICKS: i64 = 300;

/// The first of each run of [`JITTER_GAP_SLOTS`] slots that no frame
/// fills: the frame before each run lasts four slots.
pub const JITTER_GAPS: [u32; 2] = [150, 210];

/// Slots in each of [`JITTER_GAPS`]' runs.
pub const JITTER_GAP_SLOTS: u32 = 3;

/// Slots followed by an extra frame one tick later: a duplicate
/// timestamp within a tick in the MP4, and the very same timestamp in a
/// Matroska remux, whose ticks are milliseconds.
pub const JITTER_DUPS_AFTER: [u32; 2] = [120, 240];

/// Slots whose picture is exactly the frame before's, which the encoder
/// codes as a P-frame of a few bytes. None is a keyframe.
pub const JITTER_REPEATS: [u32; 10] = [12, 13, 14, 44, 103, 136, 177, 199, 255, 287];

/// Frames every [`JITTER_KEYINT`] are keyframes, counted as written.
pub const JITTER_KEYINT: u32 = 30;

/// One frame of `jitter-dups`, as [`jitter_dups_frames`] plans it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlantedFrame {
    pub slot: u32,
    /// In ticks of 1/[`VFR_TIMESCALE`] s.
    pub pts: i64,
    /// The extra frame after a slot of [`JITTER_DUPS_AFTER`].
    pub dup: bool,
    /// Its picture is the frame before's ([`JITTER_REPEATS`]).
    pub repeat: bool,
}

/// `jitter-dups`' frames in order, each with its pts.
pub fn jitter_dups_frames() -> Vec<PlantedFrame> {
    let mut out = Vec::new();
    for slot in 0..JITTER_SLOTS {
        if JITTER_GAPS
            .iter()
            .any(|&g| (g..g + JITTER_GAP_SLOTS).contains(&slot))
        {
            continue;
        }
        let shift = match (JITTER_RANGE.contains(&slot), slot % 2) {
            (false, _) => 0,
            (true, 0) => -JITTER_TICKS,
            (true, _) => JITTER_TICKS,
        };
        let pts = i64::from(slot) * JITTER_SLOT_TICKS + shift;
        out.push(PlantedFrame {
            slot,
            pts,
            dup: false,
            repeat: JITTER_REPEATS.contains(&slot),
        });
        if JITTER_DUPS_AFTER.contains(&slot) {
            out.push(PlantedFrame {
                slot,
                pts: pts + 1,
                dup: true,
                repeat: false,
            });
        }
    }
    out
}

/// Write `jitter-dups.mp4` into `dir` (a1-plan section 6.3): 10 s of
/// the noisy grid, 256×128 H.264 with no B-frames, its frames on a 30 fps
/// grid of [`JITTER_SLOTS`] slots but for what is planted: jitter in
/// [`JITTER_RANGE`], two gaps of three frames ([`JITTER_GAPS`]), two
/// duplicate timestamps ([`JITTER_DUPS_AFTER`]) and ten repeated
/// pictures ([`JITTER_REPEATS`]). [`jitter_dups_frames`] lists every
/// frame. Heavy noise makes every other P-frame large, as a camera's are.
pub fn write_jitter_dups(dir: &Path) -> Result<PathBuf, SynthError> {
    std::fs::create_dir_all(dir).map_err(|error| SynthError::Io {
        path: dir.to_path_buf(),
        error,
    })?;
    const SEED: u64 = 7;
    let frames = jitter_dups_frames();
    let spec = Spec {
        name: JITTER_DUPS.into(),
        frames: u32::try_from(frames.len()).unwrap_or(u32::MAX),
        gop: Some(Gop {
            keyint: JITTER_KEYINT,
            bframes: 0,
            open: false,
            radl: 0,
        }),
        vfr: true,
        noise: Some(SEED),
        ..Spec::default()
    };
    let out = dir.join(spec.file_name());
    let mut settings = spec.video_settings()?;
    // x264 sizes a frame by how long it lasts, given timestamps: a frame
    // a tick long would come out tiny, as a repeated picture does. As
    // though at a constant rate, every frame is coded alike.
    for (k, v) in &mut settings.options {
        if k == "x264-params" {
            v.push_str(":force-cfr=1");
        }
        // Coarse enough that a repeated picture is all skipped blocks
        // once the frame before it is refined: 12 bytes, and the first of
        // a run under 4 % of a P-frame.
        if k == "crf" {
            *v = "24".into();
        }
    }
    let mut w = Mp4Writer::create(&out, &settings, None)?;
    let format = spec.yuv_format();
    let mut last: Option<YuvPlanes> = None;
    for f in frames {
        let planes = match last.take() {
            Some(p) if f.repeat => p,
            _ => {
                let img = render_equirect(spec.pattern, spec.width, spec.height, f.slot, 2);
                let mut planes = yuv::to_yuv(&img, spec.bits, &format);
                // The extra frame is a picture of its own.
                let noise = if f.dup { 10_000 + f.slot } else { f.slot };
                // Sixteen layers, about ±11 levels, which the encoder codes
                // at its CRF: every other P-frame 3 KB or more.
                for layer in 0..16 {
                    add_noise(&mut planes, spec.bits, SEED + 31 * layer, noise);
                }
                planes
            }
        };
        w.push_video_at(&planes, f.pts)?;
        last = Some(planes);
    }
    w.finish()?;
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
        // A/V sync by eye and ear (M1 plan section 9.3).
        sync_flash_beep(),
        // analyze's audio measures (a1-plan section 6.3).
        levels(),
    ]
}

/// [`perf_clips`]' names: what hw-test writes and the playback benchmarks
/// ask for.
pub const PERF_5K7_8BIT: &str = "hevc8-5k7-30fps-noisy-20s";
pub const PERF_5K7_10BIT: &str = "hevc10-5k7-30fps-noisy-20s";
pub const PERF_8K_8BIT: &str = "hevc8-8k-30fps-noisy-20s";

/// The perf clips `cargo xtask hw-test` makes for the playback benchmarks
/// (M1 plan section 12.4, DECISIONS D37): 20 s of the moving grid at 30
/// fps with seeded noise on every frame, coded as a camera codes, in HEVC
/// open GOPs of 30 frames with B-frames, at about a camera's bitrate.
/// They are 5760x2880 at 8 and 10 bits and 100 Mbps, and 7680x3840 at 8
/// bits and 120 Mbps (the X5's 8K30 is two 3840² streams at about 125
/// Mbps in all). A clean grid decodes several times faster than footage,
/// so it would prove nothing about real time. x265's rate control makes
/// them differ a little from one FFmpeg build to another, which doesn't
/// matter here.
pub fn perf_clips() -> Vec<Spec> {
    let clip = |name: &str, width: u32, bits: u32, kbps: u32| Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width,
        height: width / 2,
        rate: FrameRate::FPS_30,
        frames: 600,
        codec: Codec::Hevc,
        bits,
        audio: Audio::None,
        spherical: true,
        gop: Some(Gop {
            keyint: 30,
            bframes: 3,
            open: true,
            radl: 0,
        }),
        noise: Some(0x9e7f_0000 + u64::from(width) + u64::from(bits)),
        bitrate_kbps: Some(kbps),
        ..Spec::default()
    };
    vec![
        clip(PERF_5K7_8BIT, 5760, 8, 100_000),
        clip(PERF_5K7_10BIT, 5760, 10, 100_000),
        clip(PERF_8K_8BIT, 7680, 8, 120_000),
    ]
}

/// The colour of a lens track: the X5's full range and BT.709, with
/// synth's own centre-sited 4:2:0 chroma ([`yuv::to_yuv`]), which it
/// tags, where the X5's is left.
pub const LENS_COLOR: ColorTags = ColorTags {
    range: 2,
    matrix: 1,
    transfer: 1,
    primaries: 1,
    chroma_location: 2,
};

/// A camera's raw file as synth makes it: two square lens tracks in one
/// MP4, as the X5 writes them (M3 plan 7.2). Each lens is coded alone
/// from its own [`Spec`], with its corner codes ([`crate::code`]) and its
/// own noise, and the two are muxed by
/// [`mux_video_streams_as`](spherecut_media::encode::mux_video_streams_as)'s
/// MP4 output, which keeps each track's time base, so
/// [`mp4::track_timing`] reads each track as it was coded.
#[derive(Debug, Clone)]
pub struct PairSpec {
    /// The file's name, without `.mp4`.
    pub name: String,
    /// Lens k's track, for k = 0 and 1: its square picture, length,
    /// rate, GOP, coding and noise. Its `name` names its own file while
    /// the pair is written. Audio and spherical metadata are refused: the
    /// file holds the two video tracks only.
    pub lenses: [Spec; 2],
    /// Each frame of each lens carries its corner code: lens k, its
    /// frame number.
    pub corner_codes: bool,
    /// The encoder codes several frames at once, on as many threads as
    /// it picks, rather than one frame at a time: for the perf clips,
    /// which would take hours more. The bitstream then depends on the
    /// machine's cores, which doesn't matter there.
    pub threaded: bool,
}

impl PairSpec {
    pub fn file_name(&self) -> String {
        format!("{}.mp4", self.name)
    }

    /// The same coding at `side`² and `frames` frames a lens, at the same
    /// bits a pixel: each lens's bitrate scaled by its pixels, so that a
    /// short, small version tests the coding of a perf clip that takes
    /// hours to make. The GOP is kept, so `frames` should hold a few.
    pub fn scaled(&self, side: u32, frames: u32) -> PairSpec {
        let lens = |s: &Spec| {
            let (from, to) = (
                u64::from(s.width) * u64::from(s.height),
                u64::from(side) * u64::from(side),
            );
            Spec {
                name: format!("{}-{side}-{frames}f", s.name),
                width: side,
                height: side,
                frames,
                bitrate_kbps: s
                    .bitrate_kbps
                    .map(|k| ((u64::from(k) * to).div_ceil(from.max(1))) as u32),
                ..s.clone()
            }
        };
        PairSpec {
            name: format!("{}-{side}-{frames}f", self.name),
            lenses: [lens(&self.lenses[0]), lens(&self.lenses[1])],
            ..self.clone()
        }
    }
}

/// Write `spec` into `dir` and return the file's path: each lens coded
/// on its own into a folder of its own beside the file, then the two
/// muxed, and the folder removed. The file appears only once whole.
pub fn write_pair(spec: &PairSpec, dir: &Path) -> Result<PathBuf, SynthError> {
    let invalid = |why: &str| -> SynthError {
        EncodeError::Invalid(format!("pair {}: {why}", spec.name)).into()
    };
    let [a, b] = &spec.lenses;
    if a.name == b.name {
        return Err(invalid("its lenses need names of their own"));
    }
    if spec
        .lenses
        .iter()
        .any(|l| l.audio != Audio::None || l.spherical)
    {
        return Err(invalid("a lens track has no audio or spherical metadata"));
    }
    let lenses = dir.join(format!(".{}-lenses", spec.name));
    let written = (|| {
        let mut paths = Vec::with_capacity(2);
        for (k, l) in spec.lenses.iter().enumerate() {
            let lens = Lens {
                code: spec.corner_codes.then_some(k),
                threaded: spec.threaded,
            };
            paths.push(write_with(l, &lenses, lens)?);
        }
        let srcs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        Ok(encode::mux_video_streams_as(
            &srcs,
            &dir.join(spec.file_name()),
            MuxedAs::Mp4,
        )?)
    })();
    let _ = std::fs::remove_dir_all(&lenses);
    written
}

/// [`perf_pairs`]' names: what hw-test writes and the pair benchmarks ask
/// for.
pub const PERF_PAIR_3840_2997: &str = "pair-hevc8-3840-2997-noisy-20s";
pub const PERF_PAIR_2880_5994: &str = "pair-hevc8-2880-5994-noisy-10s";
pub const PERF_PAIR_2880_2997: &str = "pair-hevc8-2880-2997-noisy-20s";

/// What the rows of [`PERF_PAIR_2880_5994`] say of its coding, wherever
/// they are read (the pair benchmarks and hw-test's report, each keyed on
/// the pair's name). The X5 codes every other picture at 59.94 as a
/// non-reference TSA_N, which halves the decode a catch-up needs. This
/// pair is P only, every picture but the IDRs a reference, because its
/// spec asks x265 for no temporal layers, so it decodes no faster for
/// skipping non-reference pictures. x265 3.5, as linked in the cloud,
/// couldn't code it otherwise: it marks only reordered B-frames
/// non-reference ([`perf_pairs`]).
pub const PERF_PAIR_P_ONLY: &str = "P only: the X5 codes TSA_N on every other picture, and every \
                                    picture here but the IDRs is a reference P frame";

/// The two-track perf clips hw-test makes for the pair benchmarks (M3
/// plan 4.3's P-DEC and 7.2), each 600 frames a lens, coded as the X5
/// codes each mode where x265 can: square 8-bit HEVC lenses in full
/// range, closed GOPs without B-frames, one-pass ABR at the masters'
/// bitrate a lens, each lens with its corner codes and its own noise.
///
/// - 2×3840² at 29.97: an IDR every 60 frames, P frames only, 65 Mbps a
///   lens (the masters: 61 to 68).
/// - 2×2880² at 59.94: an IDR every 120 frames, 80 Mbps a lens (79). The
///   X5 codes every other frame as a non-reference TSA_N picture. This
///   clip asks x265 for no temporal layers, so it is P frames only, every
///   one a reference, and decodes no faster for skipping non-reference
///   frames, as the X5's 5.7K60 does ([`PERF_PAIR_P_ONLY`]). x265 3.5, as
///   linked in the cloud, couldn't match the X5 here: its temporal layers
///   put the non-reference B-frames alone in the upper layer, and a
///   B-frame is decoded after the frame it is shown before, where the X5
///   reorders nothing. What 4.x makes of temporal layers is unverified.
/// - 2×2880² at 29.97: an IDR every 60 frames, P frames only, 80 Mbps a
///   lens. No sample has this mode yet.
///
/// Two more differences from the masters, whose slice headers show 10
/// slices a picture at 8K and 5 at 5.7K, with WPP off, and IDR_W_RADL
/// pictures: these clips have x265's one slice a picture with WPP on,
/// and IDR_N_LP. x265 3.5 refuses several slices without WPP, and writes
/// IDR_W_RADL only before RADL pictures, which need B-frames. Neither the
/// masters nor the clips have leading pictures, so the two IDR types
/// decode alike, and media's software decoder threads by frame, so WPP's
/// rows don't speed it up. What the slices cost a hardware decoder is not
/// measured.
///
/// x265 codes them on as many threads as it likes ([`PairSpec::threaded`]),
/// and its rate control makes them differ a little from one machine to
/// another, which doesn't matter here.
pub fn perf_pairs() -> Vec<PairSpec> {
    // Each lens of each clip has noise of its own: lens k's seed is
    // `seed + 2k`, since seeds that differ only in their lowest bit give
    // the same noise (`Spec::noise`).
    let pair = |name: &str, side: u32, rate: FrameRate, keyint: u32, kbps: u32, seed: u64| {
        let lens = |k: u32| Spec {
            name: format!("{name}-lens{k}"),
            pattern: Pattern::Grid,
            width: side,
            height: side,
            rate,
            frames: 600,
            codec: Codec::Hevc,
            bits: 8,
            audio: Audio::None,
            color: LENS_COLOR,
            gop: Some(Gop {
                keyint,
                bframes: 0,
                open: false,
                radl: 0,
            }),
            noise: Some(seed + 2 * u64::from(k)),
            bitrate_kbps: Some(kbps),
            ..Spec::default()
        };
        PairSpec {
            name: name.into(),
            lenses: [lens(0), lens(1)],
            corner_codes: true,
            threaded: true,
        }
    };
    vec![
        pair(
            PERF_PAIR_3840_2997,
            3840,
            FrameRate::FPS_29_97,
            60,
            65_000,
            0x7a1b_0010,
        ),
        pair(
            PERF_PAIR_2880_5994,
            2880,
            FrameRate::FPS_59_94,
            120,
            80_000,
            0x7a1b_0020,
        ),
        pair(
            PERF_PAIR_2880_2997,
            2880,
            FrameRate::FPS_29_97,
            60,
            80_000,
            0x7a1b_0030,
        ),
    ]
}

/// One lens of a [`pair_64`] file: its frames and its keyframe interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LensTrack {
    pub frames: u32,
    pub keyint: u32,
}

/// A two-track file of 2 × 64² (M3 plan section 9), with a length and a
/// GOP for each lens: HEVC at 29.97 fps in closed GOPs without B-frames,
/// in the lens colour, each frame with its corner codes, coded on one
/// thread so that the file is the same on every machine.
pub fn pair_64(name: &str, lenses: [LensTrack; 2]) -> PairSpec {
    let lens = |k: usize| Spec {
        name: format!("{name}-lens{k}"),
        pattern: Pattern::Grid,
        width: 64,
        height: 64,
        rate: FrameRate::FPS_29_97,
        frames: lenses[k].frames,
        codec: Codec::Hevc,
        color: LENS_COLOR,
        gop: Some(Gop {
            keyint: lenses[k].keyint,
            bframes: 0,
            open: false,
            radl: 0,
        }),
        ..Spec::default()
    };
    PairSpec {
        name: name.into(),
        lenses: [lens(0), lens(1)],
        corner_codes: true,
        threaded: false,
    }
}

/// [`pairs_64`]' names.
pub const PAIR_64_MATCHED: &str = "pair64-matched";
pub const PAIR_64_GOP_FAULT: &str = "pair64-gop-fault";
pub const PAIR_64_LENS_SHORT: &str = "pair64-lens-short";

/// Section 9's 2 × 64² files for the frame lock's tests (M3.8), each of
/// 30 frames a lens: [`PAIR_64_MATCHED`], whose lenses match, with an
/// IDR every 10 frames; [`PAIR_64_GOP_FAULT`], with GOPs of 8 and 10, so
/// that their sync samples differ; and [`PAIR_64_LENS_SHORT`], whose lens
/// 1 is a frame short.
pub fn pairs_64() -> Vec<PairSpec> {
    let track = |frames, keyint| LensTrack { frames, keyint };
    vec![
        pair_64(PAIR_64_MATCHED, [track(30, 10), track(30, 10)]),
        pair_64(PAIR_64_GOP_FAULT, [track(30, 8), track(30, 10)]),
        pair_64(PAIR_64_LENS_SHORT, [track(30, 10), track(29, 10)]),
    ]
}

/// The bytes of the samples of stream `stream` of the file at `path`,
/// counted from its packets: with its length, the track's bitrate.
pub fn track_bytes(path: &Path, stream: usize) -> Result<u64, SynthError> {
    let read = |why: String| SynthError::Read {
        path: path.to_path_buf(),
        why,
    };
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut r = PacketReader::open(path, stream, stop).map_err(|e| read(e.to_string()))?;
    let mut bytes = 0;
    loop {
        match r.next_packet() {
            NextPacket::Packet(p) => bytes += p.size as u64,
            NextPacket::End { damaged: false } => return Ok(bytes),
            NextPacket::End { damaged: true } => {
                return Err(read(format!("stream {stream} is damaged")));
            }
            NextPacket::Stopped => return Err(read("stopped".into())),
        }
    }
}

/// The NAL unit type of each picture of stream `stream`, an HEVC track
/// of the MP4 file at `path`, in decode order: its first slice's (19 and
/// 20 IDR, 1 TRAIL_R, 0 TRAIL_N, 2 TSA_N, 3 TSA_R and so on). The
/// samples are found from the track's own tables and read a NAL unit's
/// header at a time, without FFmpeg. Tables that don't agree on how
/// many samples there are, or that put more bytes of samples in the
/// file than it holds, are [`SynthError::Read`], whatever their counts
/// say, and nothing is sized by a count before that.
pub fn hevc_picture_types(path: &Path, stream: usize) -> Result<Vec<u8>, SynthError> {
    let read = |why: String| SynthError::Read {
        path: path.to_path_buf(),
        why,
    };
    let io = |error| SynthError::Io {
        path: path.to_path_buf(),
        error,
    };
    let tree = mp4::read_file(path).map_err(|e| read(e.to_string()))?;
    let (trak, hvcc) = hevc_track(&tree, stream).map_err(read)?;
    let length_bytes =
        nal_length_bytes(hvcc).map_err(|why| read(format!("track {stream}'s {why}")))?;
    let mut f = std::fs::File::open(path).map_err(io)?;
    let file_len = f.metadata().map_err(io)?.len();
    let samples =
        SampleTable::of(trak, file_len).map_err(|why| read(format!("track {stream}: {why}")))?;
    // One type a picture read, so never more than the file has pictures.
    let mut types = Vec::new();
    for (i, (offset, size)) in samples.ranges().enumerate() {
        let end = offset + u64::from(size);
        let mut at = offset;
        let slice = loop {
            let Some(nal) = nal_at(&mut f, at, end, length_bytes).map_err(io)? else {
                return Err(read(format!("picture {i} of track {stream} has no slice")));
            };
            // A VCL unit's type is below 32: the picture's first slice.
            if nal.kind < 32 {
                break nal.kind;
            }
            at = nal.next;
        };
        types.push(slice);
    }
    Ok(types)
}

/// Track `stream` of `tree`, and its `hvcC`, if it is an HEVC track.
fn hevc_track(tree: &Mp4Tree, stream: usize) -> Result<(&mp4::Mp4Box, &mp4::Mp4Box), String> {
    let trak = tree
        .tracks()
        .nth(stream)
        .ok_or_else(|| format!("no track {stream}"))?;
    let entry = trak
        .path(&[b"mdia", b"minf", b"stbl", b"stsd"])
        .and_then(|stsd| stsd.children.first())
        .ok_or_else(|| format!("track {stream} has no sample entry"))?;
    let hvcc = entry
        .child(b"hvcC")
        .ok_or_else(|| format!("track {stream} is no HEVC: no `hvcC` in `{}`", entry.fourcc))?;
    Ok((trak, hvcc))
}

/// The bytes of each NAL unit's length in the samples of a track with
/// `hvcc` (its `lengthSizeMinusOne`, plus one).
fn nal_length_bytes(hvcc: &mp4::Mp4Box) -> Result<u64, String> {
    match hvcc.payload.get(21) {
        Some(b) => Ok(u64::from(b & 3) + 1),
        None => Err("`hvcC` is cut short".into()),
    }
}

/// A NAL unit of a sample, as [`nal_at`] finds it.
struct Nal {
    /// Its type (H.265 table 7-1).
    kind: u8,
    /// Where its header starts in the file.
    at: u64,
    /// Its bytes, header included, as its length says.
    len: u64,
    /// Where the next NAL unit's length starts.
    next: u64,
}

/// The NAL unit whose length starts at `at` of a sample that ends at
/// `end` in `f`, each unit's length in `length_bytes` bytes, or `None`
/// when no unit's length and header fit there.
fn nal_at(
    f: &mut std::fs::File,
    at: u64,
    end: u64,
    length_bytes: u64,
) -> std::io::Result<Option<Nal>> {
    use std::io::{Read, Seek, SeekFrom};

    if at + length_bytes + 1 > end {
        return Ok(None);
    }
    let mut head = [0u8; 5];
    let head = &mut head[..length_bytes as usize + 1];
    f.seek(SeekFrom::Start(at))?;
    f.read_exact(head)?;
    let (len, header) = head.split_at(length_bytes as usize);
    let len = len.iter().fold(0u64, |v, b| v << 8 | u64::from(*b));
    Ok(Some(Nal {
        kind: header[0] >> 1 & 0x3f,
        at: at + length_bytes,
        len,
        next: at + length_bytes + len,
    }))
}

/// Each sample's bytes, as `stsz` gives them.
enum Sizes {
    /// `count` samples of `size` bytes each.
    Fixed { size: u32, count: u32 },
    /// Each sample's bytes, from the table itself.
    Each(Vec<u32>),
}

impl Sizes {
    fn count(&self) -> u64 {
        match self {
            Sizes::Fixed { count, .. } => u64::from(*count),
            Sizes::Each(v) => v.len() as u64,
        }
    }

    /// The bytes of sample `i`, from 0, which is below [`Sizes::count`].
    fn of(&self, i: u64) -> u32 {
        match self {
            Sizes::Fixed { size, .. } => *size,
            Sizes::Each(v) => v.get(i as usize).copied().unwrap_or(0),
        }
    }

    /// The bytes of every sample.
    fn total(&self) -> u64 {
        match self {
            Sizes::Fixed { size, count } => u64::from(*size) * u64::from(*count),
            Sizes::Each(v) => v.iter().map(|&s| u64::from(s)).sum(),
        }
    }
}

/// Where the samples of a track lie in its file: from its `stsz`, its
/// `stsc` and its `stco` or `co64`, checked against each other and the
/// file's length.
struct SampleTable {
    sizes: Sizes,
    /// Where each chunk starts.
    offsets: Vec<u64>,
    /// The samples each chunk holds.
    per_chunk: Vec<u32>,
}

impl SampleTable {
    /// The table of `trak`, in a file of `file_len` bytes. Every table is
    /// read only as far as its payload holds entries, so a count in it
    /// never sizes anything: a fixed sample size with a count of 2³² − 1
    /// is refused here, as the chunks hold fewer, rather than read.
    fn of(trak: &mp4::Mp4Box, file_len: u64) -> Result<SampleTable, String> {
        let stbl = trak
            .path(&[b"mdia", b"minf", b"stbl"])
            .ok_or("no sample table")?;
        let table = |t: &[u8; 4]| -> Option<mp4::Cursor<'_>> {
            let b = stbl.child(t)?;
            // A payload too large to keep reads as cut short below.
            let mut c = mp4::Cursor::new(&b.payload);
            c.version_flags()?;
            Some(c)
        };
        let short = |t: &str| format!("`{t}` is missing or cut short");
        let mut c = table(b"stsz").ok_or_else(|| short("stsz"))?;
        let (fixed, count) = (
            c.u32().ok_or_else(|| short("stsz"))?,
            c.u32().ok_or_else(|| short("stsz"))?,
        );
        let sizes = match fixed {
            0 => Sizes::Each(
                (0..count)
                    .map(|_| c.u32())
                    .collect::<Option<_>>()
                    .ok_or_else(|| short("stsz"))?,
            ),
            size => Sizes::Fixed { size, count },
        };
        let offsets: Vec<u64> = match (table(b"stco"), table(b"co64")) {
            (Some(mut c), _) => {
                let n = c.u32().ok_or_else(|| short("stco"))?;
                (0..n)
                    .map(|_| c.u32().map(u64::from))
                    .collect::<Option<_>>()
                    .ok_or_else(|| short("stco"))?
            }
            (None, Some(mut c)) => {
                let n = c.u32().ok_or_else(|| short("co64"))?;
                (0..n)
                    .map(|_| c.u64())
                    .collect::<Option<_>>()
                    .ok_or_else(|| short("co64"))?
            }
            (None, None) => return Err("no `stco` or `co64`".into()),
        };
        // Runs of chunks, each as its first chunk (from 1) and its samples
        // a chunk.
        let mut c = table(b"stsc").ok_or_else(|| short("stsc"))?;
        let n = c.u32().ok_or_else(|| short("stsc"))?;
        let runs: Vec<(u32, u32)> = (0..n)
            .map(|_| {
                let run = (c.u32()?, c.u32()?);
                c.u32()?;
                Some(run)
            })
            .collect::<Option<_>>()
            .ok_or_else(|| short("stsc"))?;
        let per_chunk: Vec<u32> = (1..=offsets.len() as u64)
            .map(|chunk| {
                runs.iter()
                    .rev()
                    .find(|(first, _)| u64::from(*first) <= chunk)
                    .map_or(0, |r| r.1)
            })
            .collect();
        let held: u64 = per_chunk.iter().map(|&n| u64::from(n)).sum();
        if held != sizes.count() {
            return Err(format!(
                "`stsz` counts {} samples, and its chunks hold {held}",
                sizes.count()
            ));
        }
        let total = sizes.total();
        if total > file_len {
            return Err(format!(
                "its samples' {total} bytes are more than the file's {file_len}"
            ));
        }
        Ok(SampleTable {
            sizes,
            offsets,
            per_chunk,
        })
    }

    /// Where each sample lies, in decode order, as (offset, bytes).
    fn ranges(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        let mut sample = 0u64;
        self.offsets
            .iter()
            .zip(&self.per_chunk)
            .flat_map(move |(&offset, &per)| {
                let first = sample;
                sample += u64::from(per);
                let mut at = offset;
                (first..first + u64::from(per)).map(move |i| {
                    let size = self.sizes.of(i);
                    let range = (at, size);
                    at += u64::from(size);
                    range
                })
            })
    }
}

/// The name H.265 table 7-1 gives NAL unit type `t` of a picture's
/// slices (0 to 21), or `None` for a type that isn't a slice's.
pub fn hevc_nal_name(t: u8) -> Option<&'static str> {
    const NAMES: [&str; 22] = [
        "TRAIL_N",
        "TRAIL_R",
        "TSA_N",
        "TSA_R",
        "STSA_N",
        "STSA_R",
        "RADL_N",
        "RADL_R",
        "RASL_N",
        "RASL_R",
        "RSV_VCL_N10",
        "RSV_VCL_R11",
        "RSV_VCL_N12",
        "RSV_VCL_R13",
        "RSV_VCL_N14",
        "RSV_VCL_R15",
        "BLA_W_LP",
        "BLA_W_RADL",
        "BLA_N_LP",
        "IDR_W_RADL",
        "IDR_N_LP",
        "CRA_NUT",
    ];
    NAMES.get(usize::from(t)).copied()
}

/// How one HEVC track of an MP4 file is coded, as hw-test reports each
/// lens of a perf pair beside the masters' coding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackCoding {
    /// Its sync samples (`stss`), numbered from 1, or `None` when the
    /// track has no `stss`, so that every sample is one.
    pub sync: Option<Vec<u32>>,
    /// The NAL type of each picture in decode order
    /// ([`hevc_picture_types`]).
    pub types: Vec<u8>,
}

/// How stream `stream`, an HEVC track of the MP4 file at `path`, is
/// coded: its sync samples from [`mp4::track_timing`] and its pictures'
/// NAL types from their slices.
pub fn track_coding(path: &Path, stream: usize) -> Result<TrackCoding, SynthError> {
    let timing = mp4::track_timing(path, stream).map_err(|e| SynthError::Read {
        path: path.to_path_buf(),
        why: e.to_string(),
    })?;
    Ok(TrackCoding {
        sync: timing.stss,
        types: hevc_picture_types(path, stream)?,
    })
}

/// The text of each user data SEI message (`user_data_unregistered`,
/// H.265 D.2.7) that stream `stream`, an HEVC track of the MP4 file at
/// `path`, holds in its `hvcC` and in its first picture, after the
/// message's UUID and up to its first NUL: where x265 names itself and
/// the options it coded with ([`X265Version`]).
pub fn hevc_user_data(path: &Path, stream: usize) -> Result<Vec<String>, SynthError> {
    use std::io::{Read, Seek, SeekFrom};

    let read = |why: String| SynthError::Read {
        path: path.to_path_buf(),
        why,
    };
    let io = |error| SynthError::Io {
        path: path.to_path_buf(),
        error,
    };
    let tree = mp4::read_file(path).map_err(|e| read(e.to_string()))?;
    let (trak, hvcc) = hevc_track(&tree, stream).map_err(read)?;
    let mut texts = Vec::new();
    // The `hvcC`'s arrays of NAL units, after its 22 bytes of
    // configuration and its count of arrays: each a byte whose low six
    // bits are the units' type, their count, and each unit after its
    // two-byte length.
    let mut c = mp4::Cursor::new(&hvcc.payload);
    let arrays = c
        .take(22)
        .and_then(|_| c.u8())
        .ok_or_else(|| read(format!("track {stream}'s `hvcC` is cut short")))?;
    for _ in 0..arrays {
        let units = (|| {
            let kind = c.u8()? & 0x3f;
            let n = c.u16()?;
            let mut units = Vec::new();
            for _ in 0..n {
                let len = c.u16()?;
                units.push((kind, c.take(usize::from(len))?));
            }
            Some(units)
        })()
        .ok_or_else(|| read(format!("track {stream}'s `hvcC` is cut short")))?;
        for (kind, unit) in units {
            if matches!(kind, SEI_PREFIX | SEI_SUFFIX) {
                texts.extend(user_data_texts(unit));
            }
        }
    }
    // The SEI units before the first picture's first slice.
    let length_bytes =
        nal_length_bytes(hvcc).map_err(|why| read(format!("track {stream}'s {why}")))?;
    let mut f = std::fs::File::open(path).map_err(io)?;
    let file_len = f.metadata().map_err(io)?.len();
    let samples =
        SampleTable::of(trak, file_len).map_err(|why| read(format!("track {stream}: {why}")))?;
    if let Some((offset, size)) = samples.ranges().next() {
        let end = offset + u64::from(size);
        let mut at = offset;
        while let Some(nal) = nal_at(&mut f, at, end, length_bytes).map_err(io)? {
            if nal.kind < 32 {
                break;
            }
            if matches!(nal.kind, SEI_PREFIX | SEI_SUFFIX) {
                // Within the sample, which lies within the file.
                let mut unit = vec![0u8; nal.len.min(end - nal.at) as usize];
                f.seek(SeekFrom::Start(nal.at)).map_err(io)?;
                f.read_exact(&mut unit).map_err(io)?;
                texts.extend(user_data_texts(&unit));
            }
            at = nal.next;
        }
    }
    Ok(texts)
}

/// The NAL unit types of SEI, before and after a picture's slices.
const SEI_PREFIX: u8 = 39;
const SEI_SUFFIX: u8 = 40;

/// The SEI payload type of user data that a UUID names
/// (`user_data_unregistered`).
const USER_DATA_UNREGISTERED: usize = 5;

/// The texts of the user data messages of `unit`, an SEI NAL unit with
/// its two-byte header, as [`hevc_user_data`] gives them.
fn user_data_texts(unit: &[u8]) -> Vec<String> {
    // The RBSP: the unit after its header, without the emulation
    // prevention bytes (a 3 after two zeros).
    let mut rbsp = Vec::with_capacity(unit.len());
    let mut zeros = 0;
    for &b in unit.iter().skip(2) {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        rbsp.push(b);
    }
    // Each message's type, then its size, each as bytes of 255 and one
    // below that are summed, then its payload, until the RBSP's trailing
    // bits.
    fn number(c: &mut &[u8]) -> Option<usize> {
        let mut v = 0usize;
        loop {
            let (&b, rest) = c.split_first()?;
            *c = rest;
            v = v.checked_add(usize::from(b))?;
            if b != 0xff {
                return Some(v);
            }
        }
    }
    let mut texts = Vec::new();
    let mut c = &rbsp[..];
    while !c.is_empty() && c != [0x80] {
        let (Some(kind), Some(size)) = (number(&mut c), number(&mut c)) else {
            break;
        };
        let Some(payload) = c.get(..size) else {
            break;
        };
        c = &c[size..];
        if kind == USER_DATA_UNREGISTERED && size >= 16 {
            let text = &payload[16..];
            let text = text.split(|&b| b == 0).next().unwrap_or(text);
            texts.push(String::from_utf8_lossy(text).into_owned());
        }
    }
    texts
}

/// The x265 that coded a stream, as the info SEI it writes into every
/// stream it codes (unless asked not to, `info=0`) names it: "x265
/// (build 199) - 3.5+1-f0c1022b6:[Linux][GCC 13.2.0][64 bit]
/// 8bit+10bit+12bit - H.265/HEVC codec - … - options: …" from the cloud's
/// x265 3.5. x265 4.x is read by the same rule, unchecked against one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X265Version {
    /// Its API build number (`X265_BUILD`): 199 for 3.5.
    pub build: u32,
    /// Its version, such as "3.5+1-f0c1022b6", or "4.1" for one built
    /// from a release's sources alone.
    pub version: String,
}

impl X265Version {
    /// Read `text`, an x265 info SEI's text, or `None` for any other.
    pub fn parse(text: &str) -> Option<X265Version> {
        let rest = text.strip_prefix("x265 (build ")?;
        let (build, rest) = rest.split_once(") - ")?;
        let (version, _) = rest.split_once(':')?;
        Some(X265Version {
            build: build.parse().ok()?,
            version: version.trim().to_string(),
        })
        .filter(|v| !v.version.is_empty())
    }

    /// Its major version: 3 for "3.5+1-f0c1022b6".
    pub fn major(&self) -> Option<u32> {
        self.version
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    }

    /// The `temporal-layers` value that asks this x265 for one temporal
    /// layer above the base: x265 3.x takes a switch (1), and x265 4.0
    /// made it a count of layers from 2 to 5, of which 2 puts the
    /// reference pictures in the base layer and the rest above it. `None`
    /// when its major version can't be read.
    pub fn temporal_layers(&self) -> Option<u32> {
        Some(if self.major()? >= 4 { 2 } else { 1 })
    }
}

impl std::fmt::Display for X265Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (build {})", self.version, self.build)
    }
}

/// The x265 that synth codes HEVC with, whichever FFmpeg links: read
/// from the info SEI of one 64² frame coded into `dir` as synth codes
/// its clips, and the file removed again. It is the x265 the perf clips
/// are coded with, whatever headers `pkg-config` finds or doesn't.
pub fn x265_version(dir: &Path) -> Result<X265Version, SynthError> {
    use std::sync::atomic::{AtomicU32, Ordering};

    // A name of its own for each call, so that calls never share a file.
    static CALLS: AtomicU32 = AtomicU32::new(0);
    let spec = Spec {
        name: format!(
            ".x265-version-{}-{}",
            std::process::id(),
            CALLS.fetch_add(1, Ordering::Relaxed)
        ),
        width: 64,
        height: 64,
        frames: 1,
        codec: Codec::Hevc,
        ..Spec::default()
    };
    let path = write(&spec, dir)?;
    let texts = hevc_user_data(&path, 0);
    let _ = std::fs::remove_file(&path);
    let texts = texts?;
    texts
        .iter()
        .find_map(|t| X265Version::parse(t))
        .ok_or_else(|| SynthError::Read {
            path,
            why: format!(
                "x265 named no version: no info SEI among its {} user data messages",
                texts.len()
            ),
        })
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
        device: None,
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

    /// The noise [`NOISE_FORMAT`] names, pinned: one seed's noise for two
    /// frames over flat 8-bit and 10-bit planes, hashed. A change to the
    /// noise fails this until `NOISE_FORMAT` is raised and the pin moved
    /// with it, so that hw-test makes the perf pairs again rather than
    /// taking those of the old noise as made.
    #[test]
    fn the_noise_its_format_names_is_pinned() {
        let fingerprint = |seed: u64| {
            let mut bytes = Vec::new();
            for frame in [0, 599] {
                let mut p8 = YuvPlanes::U8([vec![128; 256], vec![128; 64], vec![128; 64]]);
                add_noise(&mut p8, 8, seed, frame);
                let mut p10 = YuvPlanes::U16([vec![512; 256], vec![512; 64], vec![512; 64]]);
                add_noise(&mut p10, 10, seed, frame);
                for p in [&p8, &p10] {
                    match p {
                        YuvPlanes::U8(p) => bytes.extend(p.concat()),
                        YuvPlanes::U16(p) => {
                            bytes.extend(p.concat().iter().flat_map(|v| v.to_le_bytes()));
                        }
                    }
                }
            }
            spherecut_testkit::fnv64(&bytes)
        };
        let pinned = fingerprint(0x7a1b_0020);
        eprintln!("noise: format {NOISE_FORMAT}, {pinned:#018x}");
        assert_eq!((NOISE_FORMAT, pinned), (1, 0x0093_9790_e07d_8384));
        // Its control: the next lens's seed adds other noise, so the pin
        // sees the samples.
        spherecut_testkit::control::must_fail(
            "lens 1's noise taken for the pinned lens 0's",
            fingerprint(0x7a1b_0022) == pinned,
        );
    }

    fn option<'a>(v: &'a VideoSettings, key: &str) -> Option<&'a str> {
        v.options
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_bitrate_takes_the_place_of_the_crf() {
        let spec = |codec, bitrate_kbps| Spec {
            codec,
            bitrate_kbps,
            ..Spec::default()
        };
        for codec in [Codec::H264, Codec::Hevc] {
            let v = spec(codec, None).video_settings().unwrap();
            assert!(option(&v, "crf").is_some());
            assert_eq!(option(&v, "b"), None);
            let v = spec(codec, Some(100_000)).video_settings().unwrap();
            assert_eq!(option(&v, "crf"), None, "{:?}", v.options);
            assert_eq!(option(&v, "b"), Some("100000k"));
            assert_eq!(option(&v, "preset"), Some("veryfast"));
        }
        for codec in [Codec::H264Lossless, Codec::HevcLossless] {
            assert!(matches!(
                spec(codec, Some(1)).video_settings(),
                Err(EncodeError::Invalid(_))
            ));
        }
    }

    #[test]
    fn the_perf_clips_are_a_cameras_streams() {
        let clips = perf_clips();
        let names: Vec<&str> = clips.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, [PERF_5K7_8BIT, PERF_5K7_10BIT, PERF_8K_8BIT]);
        for c in &clips {
            assert_eq!(
                (c.width / 2, c.frames, c.rate),
                (c.height, 600, FrameRate::FPS_30)
            );
            assert_eq!(c.codec, Codec::Hevc);
            assert!(c.noise.is_some() && c.spherical && c.audio == Audio::None);
            let gop = c.gop.unwrap();
            assert_eq!((gop.keyint, gop.open), (30, true));
            assert!(gop.bframes > 0);
            let v = c.video_settings().unwrap();
            assert!(option(&v, "b").is_some(), "{:?}", v.options);
        }
        assert_eq!(
            clips.iter().map(|c| (c.width, c.bits)).collect::<Vec<_>>(),
            [(5760, 8), (5760, 10), (7680, 8)]
        );
    }

    /// The text of the cloud's x265 3.5's info SEI, its options cut short.
    const X265_3_5: &str = "x265 (build 199) - 3.5+1-f0c1022b6:[Linux][GCC 13.2.0][64 bit] \
                            8bit+10bit+12bit - H.265/HEVC codec - Copyright 2013-2018 (c) \
                            Multicoreware, Inc - http://x265.org - options: cpuid=1111039";

    #[test]
    fn x265s_info_sei_names_its_version_and_the_temporal_layers_it_takes() {
        let v = X265Version::parse(X265_3_5).unwrap();
        assert_eq!(
            (v.build, v.version.as_str(), v.major()),
            (199, "3.5+1-f0c1022b6", Some(3))
        );
        assert_eq!(v.to_string(), "3.5+1-f0c1022b6 (build 199)");
        assert_eq!(v.temporal_layers(), Some(1));
        // x265 4.x, as odin's and huginn's Fedora 44 link it, by the same
        // printf as 3.5's: from a repository's checkout, or from a
        // release's sources alone. Neither text is one a 4.x wrote here,
        // since the cloud has 3.5 alone.
        for (text, version) in [
            (
                "x265 (build 215) - 4.1+1-1d117be:[Linux][GCC 15.2.1][64 bit] 8bit+10bit+12bit \
                 - H.265/HEVC codec - Copyright 2013-2018 (c) Multicoreware, Inc - \
                 http://x265.org - options: cpuid=1111039",
                "4.1+1-1d117be",
            ),
            (
                "x265 (build 215) - 4.1:[Linux][GCC 15.2.1][64 bit] 8bit",
                "4.1",
            ),
        ] {
            let v4 = X265Version::parse(text).unwrap();
            assert_eq!((v4.build, v4.version.as_str()), (215, version));
            assert_eq!((v4.major(), v4.temporal_layers()), (Some(4), Some(2)));
        }
        // Its control: 3.5 isn't asked for 4.x's count of layers, which it
        // would take as the switch it is.
        spherecut_testkit::control::must_fail(
            "x265 3.5 asked for 4.x's count of temporal layers",
            v.temporal_layers() == Some(2),
        );
        // x265's log line names the version too, but goes to stderr, not
        // into the stream, and isn't the SEI's text; nor is an encoder's
        // name.
        for other in [
            "x265 [info]: HEVC encoder version 3.5+1-f0c1022b6",
            "Lavc60.31.102 libx265",
            "x265 (build 199) - :[Linux]",
            "x265 (build ?) - 3.5:[Linux]",
        ] {
            assert_eq!(X265Version::parse(other), None, "{other}");
        }
    }

    /// An SEI NAL unit of one message of `kind`, whose payload is a UUID of
    /// zeros and `text` with its NUL, with the emulation prevention bytes
    /// its zeros need.
    fn sei_unit(kind: u8, text: &str) -> Vec<u8> {
        let mut payload = vec![0u8; 16];
        payload.extend(text.as_bytes());
        payload.push(0);
        let mut rbsp = vec![kind];
        let mut size = payload.len();
        while size >= 255 {
            rbsp.push(0xff);
            size -= 255;
        }
        rbsp.push(size as u8);
        rbsp.extend(payload);
        rbsp.push(0x80);
        let mut unit = vec![SEI_PREFIX << 1, 1];
        let mut zeros = 0;
        for b in rbsp {
            if zeros == 2 && b <= 3 {
                unit.push(3);
                zeros = 0;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            unit.push(b);
        }
        unit
    }

    #[test]
    fn user_data_reads_through_emulation_prevention_and_long_sizes() {
        // As long as x265's, whose options run to a kilobyte or more.
        let text = format!("{X265_3_5} {}", "no-wpp ".repeat(40));
        let unit = sei_unit(USER_DATA_UNREGISTERED as u8, &text);
        // The UUID's zeros took emulation prevention bytes, and the size
        // is over 255.
        assert!(unit.windows(3).any(|w| w == [0, 0, 3]), "{unit:?}");
        assert!(text.len() + 17 > 255);
        assert_eq!(user_data_texts(&unit), [text.as_str()]);
        // Its control: the same bytes in a message of another type (4,
        // user data registered by a country code) are no user data of
        // the kind x265 writes.
        let other = sei_unit(4, &text);
        spherecut_testkit::control::must_fail(
            "registered user data read as x265's",
            user_data_texts(&other).contains(&text),
        );
        // A message cut short ends the reading rather than panicking.
        assert!(user_data_texts(&unit[..unit.len() / 2]).is_empty());
        assert!(user_data_texts(&[SEI_PREFIX << 1, 1]).is_empty());
    }

    #[test]
    fn nal_types_have_table_7_1s_names() {
        let names: Vec<_> = [0, 1, 2, 19, 20, 21, 22, 39]
            .into_iter()
            .map(hevc_nal_name)
            .collect();
        assert_eq!(
            names,
            [
                Some("TRAIL_N"),
                Some("TRAIL_R"),
                Some("TSA_N"),
                Some("IDR_W_RADL"),
                Some("IDR_N_LP"),
                Some("CRA_NUT"),
                None,
                None
            ]
        );
    }
}
