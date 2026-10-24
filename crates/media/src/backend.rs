//! The `MediaBackend` traits: what the player, the CLI and the tests open
//! media through (M1 plan section 3.2, DECISIONS D29).
//!
//! A backend probes files and opens decoders. A [`VideoDecode`] hands out
//! [`VideoFrame`]s on the clip's own time axis, where 0 is the first
//! presented frame, and seeks frame-exactly (section 3.3, `seek.rs`).
//! [`FfmpegBackend`] is the real one; synth's `SyntheticBackend` and
//! `GatedBackend` stand in for it in tests.
//!
//! This slice (2a) decodes in software. [`DecodeMode::Hardware`],
//! [`DecodePath::FellBack`] and [`DecoderEvent::FellBack`] are declared
//! for slice 2b, which adds hardware decoding and fallback without
//! changing these types, and slice 5b adds a GPU-resident
//! [`FrameStorage`] variant. Audio decoding arrives in slice 6; until
//! then [`MediaBackend::open_audio`] says so with
//! [`MediaError::NotYet`].

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::video::{ColorInfo, PixelLayout, PlaneData, VideoPlanes};
use spherecut_core::{FrameRate, Rational, StopToken, Time};
use thiserror::Error;

use crate::probe::{ProbeError, ProbeReport};
use crate::spherical::{Projection, StereoMode};
use crate::{HwDevice, HwRefusal, MediaError, ff, ffi};

/// Opens media. Every method blocks.
pub trait MediaBackend: Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;
    /// The confined open (D24) plus Spherecut's own MP4 and Matroska
    /// checks.
    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError>;
    /// A confined demuxer and decoder for one video stream. Decodes the
    /// first presented frame, which fixes the clip's origin.
    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError>;
    /// A demuxer and decoder of its own for one audio stream, since a
    /// video decoder discards the other streams (D13).
    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError>;
}

/// How to open a video stream. Build one from [`VideoRequest::default`],
/// so that fields added later keep their defaults.
#[derive(Debug, Clone)]
pub struct VideoRequest {
    /// The stream's index, or `None` for FFmpeg's best video stream.
    pub stream: Option<usize>,
    pub decode: DecodeMode,
    /// Software decoding threads; `None` lets FFmpeg pick (D3).
    pub threads: Option<NonZeroU32>,
    /// Surfaces a hardware decoder keeps beyond its own needs: one for the
    /// best frame held during a seek, one spare (slice 2b).
    pub extra_hw_frames: u8,
    /// Set at shutdown only: FFmpeg stops reading inside a read, and the
    /// stream ends for good (D13). Seeks stop through [`StopToken`]s.
    pub interrupt: Arc<AtomicBool>,
}

impl Default for VideoRequest {
    fn default() -> Self {
        VideoRequest {
            stream: None,
            decode: DecodeMode::Software,
            threads: None,
            extra_hw_frames: 2,
            interrupt: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[derive(Debug, Clone)]
pub enum DecodeMode {
    Software,
    /// On `device`, a device of kind `kind`; what happens when it can't
    /// decode the stream is `fallback`'s choice. Slice 2b.
    Hardware {
        kind: HwKind,
        device: Arc<HwDevice>,
        fallback: Fallback,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwKind {
    VaApi,
    Vulkan,
}

/// What a hardware decode does when the device can't decode the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Go on in software, and say so (D3, D14).
    Software,
    /// Fail with the reason: every Tier 2 test.
    Fail,
}

/// Where frames are decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodePath {
    Software {
        threads: Option<NonZeroU32>,
    },
    Hardware {
        kind: HwKind,
    },
    /// Asked for hardware, decoding in software: slice 2b.
    FellBack {
        from: HwKind,
        reason: FallbackReason,
    },
}

/// Why a hardware decode went on in software (slice 2b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackReason {
    /// The device turned the stream down.
    Refused(HwRefusal),
    /// The device could not be opened.
    DeviceUnavailable(String),
    /// Decoding or downloading frame `at_pts` failed on the device.
    Failed { at_pts: i64, error: String },
}

/// One video stream, decoding. Times are clip times: 0 is the first
/// presented frame (section 3.3).
pub trait VideoDecode: Send + std::fmt::Debug {
    fn info(&self) -> &VideoStreamInfo;
    fn decode_path(&self) -> &DecodePath;
    /// The frame shown at clip time `at` (section 3.3), or
    /// [`SeekOutcome::Stopped`] once `stop` says to.
    fn seek(
        &mut self,
        at: Time,
        mode: SeekMode,
        stop: &StopToken,
    ) -> Result<SeekOutcome, MediaError>;
    /// The frame after the last one `seek` or `next_frame` returned.
    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError>;
    /// `Some(t)`: skip frames nothing refers to (`AVDISCARD_NONREF`), and
    /// never return a frame before `t`, until one at or after it has been
    /// returned. `None`: back to normal, as after a seek. For playback
    /// that has fallen behind the clock.
    fn catch_up(&mut self, until: Option<Time>);
    /// What happened since the last call, oldest first.
    fn take_events(&mut self) -> Vec<DecoderEvent>;
    fn stats(&self) -> DecodeStats;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    /// The frame shown at the time asked for.
    Exact,
    /// Quickly, for scrubbing: the keyframe one seek to the time lands
    /// on, which is after the time when that is one of the keyframe's
    /// leading pictures. The decoder stays there, so an exact seek to the
    /// time after it decodes forward when the keyframe is at or before it.
    Keyframe,
}

#[derive(Debug)]
pub enum SeekOutcome {
    Frame {
        frame: Arc<VideoFrame>,
        exactness: Exactness,
        cost: SeekCost,
    },
    Stopped,
}

/// How the frame a seek returned relates to the time asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exactness {
    /// The frame shown at that time.
    Exact,
    /// The time is before 0: the first frame.
    BeforeFirst,
    /// The time is at or after the end of the last frame: the last frame.
    AfterLast,
    /// [`SeekMode::Keyframe`]: the keyframe a seek landed on.
    KeyframeOnly,
    /// The file is damaged before the time (D13): the last frame decoded
    /// before the damage, at `last_good_pts`.
    Truncated { last_good_pts: i64 },
}

/// What a seek took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeekCost {
    /// `av_seek_frame` calls, the first included.
    pub seeks: u32,
    /// Of those, seeks again to before the keyframe landed on.
    pub reseeks: u32,
    pub frames_decoded: u32,
    /// It decoded forward from where the decoder was, without seeking.
    pub forward_only: bool,
}

#[derive(Debug)]
pub enum NextFrame {
    Frame(Arc<VideoFrame>),
    End,
    Stopped,
}

/// What a video stream is, as far as the player and `frame` care.
#[derive(Debug, Clone)]
pub struct VideoStreamInfo {
    pub index: usize,
    /// FFmpeg's codec name, such as `hevc`.
    pub codec: String,
    /// In pixels, after cropping.
    pub size: (u32, u32),
    /// Seconds per tick of the stream's timestamps.
    pub time_base: Rational,
    /// The pts of the first presented frame: clip time 0.
    pub origin_pts: i64,
    /// The clip time at which the last frame's interval ends, as the
    /// container gives it.
    pub duration: Option<Time>,
    pub timing: Timing,
    /// Frames shown, when the demuxer's index lists them all (MP4).
    pub frame_count: Option<u64>,
    /// What frames carry: their own layout, or swscale's target.
    pub layout: Arc<PixelLayout>,
    /// FFmpeg's name for the decoder's pixel format.
    pub source_pix_fmt: String,
    pub color: ColorInfo,
    pub mapping: SourceMapping,
    pub rotation_deg: Option<f64>,
}

impl VideoStreamInfo {
    /// The stream's frame rate when it is constant.
    pub fn frame_rate(&self) -> Option<FrameRate> {
        match self.timing {
            Timing::Constant(r) => Some(r),
            Timing::Variable { .. } => None,
        }
    }

    /// Clip time of stream tick `pts`.
    pub fn clip_time(&self, pts: i64) -> Result<Time, spherecut_core::TimeError> {
        Time::from_pts(pts.saturating_sub(self.origin_pts), self.time_base)
    }
}

/// Whether frames come at a constant rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// Exact: from an MP4's `stts` when every sample but the last lasts
    /// the same, or from a Matroska track whose default duration is a
    /// standard rate.
    Constant(FrameRate),
    /// FFmpeg's average rate, when it has one.
    Variable { average: Option<FrameRate> },
}

/// How a frame maps onto the sphere, from the stream's spherical metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceMapping {
    /// From Spherical Video V2 (`sv3d/proj`), or equirect for V1's
    /// `equirectangular`; `None` without spherical metadata.
    pub projection: Option<Projection>,
    /// The part of the sphere's equirect uv square the frame covers, as
    /// left, top, right and bottom: (0, 0, 1, 1) unless `equi`'s crop
    /// bounds say otherwise.
    pub cover: [f64; 4],
    /// From `st3d`, or V1's `StereoMode`; mono when neither says.
    pub stereo: StereoMode,
    /// An `sv3d` pose that isn't zero, which M1 shows but doesn't apply
    /// (D33).
    pub pose_present: bool,
}

impl Default for SourceMapping {
    fn default() -> Self {
        SourceMapping {
            projection: None,
            cover: [0.0, 0.0, 1.0, 1.0],
            stereo: StereoMode::Mono,
            pose_present: false,
        }
    }
}

/// Something a decoder reports alongside its frames.
#[derive(Debug, Clone, PartialEq)]
pub enum DecoderEvent {
    /// Decoding went on in software (slice 2b).
    FellBack {
        from: HwKind,
        reason: FallbackReason,
    },
    /// Frames from here on have another layout or size.
    LayoutChanged {
        layout: Arc<PixelLayout>,
        size: (u32, u32),
    },
    /// The decoder's frames are in a format that isn't uploaded as it is,
    /// so swscale converts each one (D30).
    Converted { from: String, to: String },
    /// The file is damaged at this point; `last_good_pts` is the last
    /// frame decoded before it.
    Damaged {
        reason: String,
        last_good_pts: Option<i64>,
    },
    /// Timestamps went backwards after `at_pts` in decode order, so exact
    /// seeks decode from the start from now on: correct, and slow.
    NotExactlySeekable { at_pts: i64 },
}

/// How decoding is going, for the overlay.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DecodeStats {
    pub frames_decoded: u64,
    pub frames_returned: u64,
    /// Milliseconds the last frame returned took to decode, and their
    /// mean over every frame decoded.
    pub last_decode_ms: f64,
    pub mean_decode_ms: f64,
    /// Milliseconds the last frame took to download from the device (0
    /// in software) and to convert (0 for a native layout).
    pub last_download_ms: f64,
    pub last_convert_ms: f64,
}

/// A decoded picture: owned, immutable, `Send` and `Sync`, and shared as
/// `Arc<VideoFrame>` between the frame cache and the upload thread, so
/// that no plane is copied to share it.
pub struct VideoFrame {
    /// `AVFrame.pts`, else `best_effort_timestamp`, in the stream's time
    /// base.
    pub pts: i64,
    /// `pts` in clip time, exact.
    pub time: Time,
    /// Until the next frame's pts, when that was known, else
    /// `AVFrame.duration`.
    pub duration: Option<Time>,
    pub key: bool,
    /// The decoder concealed errors in it.
    pub concealed: bool,
    pub decoded_on: DecodePath,
    /// The seek it was decoded after, and its place in decode-output
    /// order since then: the cache links runs of frames by these.
    pub run: u64,
    pub seq: u64,
    storage: FrameStorage,
    layout: Arc<PixelLayout>,
    color: ColorInfo,
    size: (u32, u32),
    bytes: usize,
}

/// Where a frame's planes live.
pub(crate) enum FrameStorage {
    /// An FFmpeg frame in system memory, whose planes are handed out
    /// through [`ffi::plane`]'s bounded slices.
    Ffmpeg(ff::frame::Video),
    /// Planes made without FFmpeg.
    Owned {
        planes: Vec<Vec<u8>>,
        strides: Vec<usize>,
    },
}

/// A plane of an owned frame doesn't hold its rows.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("plane {plane}: {why}")]
pub struct PlaneMismatch {
    pub plane: usize,
    pub why: String,
}

impl VideoFrame {
    /// A frame from an FFmpeg frame in system memory in `layout`.
    pub(crate) fn from_ffmpeg(
        frame: ff::frame::Video,
        layout: Arc<PixelLayout>,
        color: ColorInfo,
        pts: i64,
        time: Time,
    ) -> VideoFrame {
        let f = ffi::frame_fields(&frame);
        let bytes = (0..layout.planes.len())
            .filter_map(|i| ffi::plane(&frame, i))
            .map(<[u8]>::len)
            .sum();
        VideoFrame {
            pts,
            time,
            duration: None,
            key: f.key,
            concealed: f.concealed,
            decoded_on: DecodePath::Software { threads: None },
            run: 0,
            seq: 0,
            storage: FrameStorage::Ffmpeg(frame),
            layout,
            color,
            size: (f.width, f.height),
            bytes,
        }
    }

    /// A frame whose planes were made without FFmpeg, as synthetic sources
    /// make them: plane `i` of `layout` in `planes[i]`, rows `strides[i]`
    /// bytes apart. Every row must lie inside its plane. The other fields
    /// start as a software frame of run 0, with no duration; set them
    /// after.
    pub fn owned(
        layout: Arc<PixelLayout>,
        size: (u32, u32),
        planes: Vec<Vec<u8>>,
        strides: Vec<usize>,
        color: ColorInfo,
        (pts, time): (i64, Time),
    ) -> Result<VideoFrame, PlaneMismatch> {
        if planes.len() != layout.planes.len() || strides.len() != planes.len() {
            return Err(PlaneMismatch {
                plane: planes.len().min(strides.len()),
                why: format!(
                    "{} planes and {} strides for {}'s {}",
                    planes.len(),
                    strides.len(),
                    layout.name,
                    layout.planes.len()
                ),
            });
        }
        for (i, (desc, (p, &stride))) in layout
            .planes
            .iter()
            .zip(planes.iter().zip(&strides))
            .enumerate()
        {
            let (w, h) = desc.size(size.0, size.1);
            let row = desc.row_bytes(w);
            let need = stride
                .checked_mul((h as usize).saturating_sub(1))
                .and_then(|n| n.checked_add(row));
            if stride < row || need.is_none_or(|n| n > p.len()) {
                return Err(PlaneMismatch {
                    plane: i,
                    why: format!(
                        "{} bytes with rows {stride} apart can't hold {h} rows of {row}",
                        p.len()
                    ),
                });
            }
        }
        let bytes = planes.iter().map(Vec::len).sum();
        Ok(VideoFrame {
            pts,
            time,
            duration: None,
            key: true,
            concealed: false,
            decoded_on: DecodePath::Software { threads: None },
            run: 0,
            seq: 0,
            storage: FrameStorage::Owned { planes, strides },
            layout,
            color,
            size,
            bytes,
        })
    }

    /// The bytes its planes take, padding included.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The layout, shared with the frames decoded with it.
    pub fn layout_arc(&self) -> &Arc<PixelLayout> {
        &self.layout
    }

    /// Where plane `index` starts in memory, for tests that check a plane
    /// is handed out without a copy: `AVFrame.data` for an FFmpeg frame.
    #[doc(hidden)]
    pub fn storage_ptr(&self, index: usize) -> *const u8 {
        match &self.storage {
            FrameStorage::Ffmpeg(f) => ffi::data_ptr(f, index),
            FrameStorage::Owned { planes, .. } => {
                planes.get(index).map_or(std::ptr::null(), |p| p.as_ptr())
            }
        }
    }
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("pts", &self.pts)
            .field("time", &self.time)
            .field("duration", &self.duration)
            .field("key", &self.key)
            .field("layout", &self.layout.name)
            .field("size", &self.size)
            .field("run", &self.run)
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

impl VideoPlanes for VideoFrame {
    fn layout(&self) -> &PixelLayout {
        &self.layout
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Without a copy: an FFmpeg frame's planes are its own buffers.
    fn plane(&self, index: usize) -> Option<PlaneData<'_>> {
        let desc = self.layout.planes.get(index)?;
        let (width, height) = desc.size(self.size.0, self.size.1);
        let (bytes, stride) = match &self.storage {
            FrameStorage::Ffmpeg(frame) => ffi::plane_and_stride(frame, index)?,
            FrameStorage::Owned { planes, strides } => {
                (planes.get(index)?.as_slice(), *strides.get(index)?)
            }
        };
        Some(PlaneData {
            bytes,
            stride,
            width,
            height,
        })
    }

    fn color(&self) -> &ColorInfo {
        &self.color
    }
}

/// How to open an audio stream (slice 6).
#[derive(Debug, Clone)]
pub struct AudioRequest {
    pub stream: Option<usize>,
    pub out: AudioOutFormat,
    /// Set at shutdown only, as [`VideoRequest::interrupt`].
    pub interrupt: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOutFormat {
    pub rate: u32,
    pub channels: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStreamInfo {
    pub index: usize,
    pub codec: String,
    pub rate: u32,
    pub channels: u16,
    pub layout: Option<String>,
    /// First-order ambiX by its SA3D box (D17).
    pub ambisonic: bool,
    /// Where its first sample lies in clip time.
    pub start: Time,
}

/// One audio stream, decoding and resampling to the output format (slice
/// 6).
pub trait AudioDecode: Send + std::fmt::Debug {
    fn info(&self) -> &AudioStreamInfo;
    /// The next chunk starts exactly at output sample round(at × rate) of
    /// the clip's time axis.
    fn seek(&mut self, at: Time, stop: &StopToken) -> Result<(), MediaError>;
    fn next_chunk(&mut self, stop: &StopToken) -> Result<NextAudio, MediaError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    /// Index of its first sample at the output rate, in clip time.
    pub start: i64,
    pub channels: u16,
    /// Interleaved.
    pub data: Vec<f32>,
}

#[derive(Debug)]
pub enum NextAudio {
    Chunk(AudioChunk),
    End,
    Stopped,
}

/// The FFmpeg backend: every open is confined to the file (D24), and
/// frames are decoded in software (slice 2a).
#[derive(Debug, Default, Clone, Copy)]
pub struct FfmpegBackend;

impl MediaBackend for FfmpegBackend {
    fn name(&self) -> &'static str {
        "ffmpeg"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        crate::probe::probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        match &req.decode {
            DecodeMode::Software => Ok(Box::new(crate::seek::FfmpegVideo::open(path, req)?)),
            DecodeMode::Hardware { .. } => Err(MediaError::NotYet {
                what: "hardware decoding through MediaBackend (M1 slice 2b)",
            }),
        }
    }

    fn open_audio(
        &self,
        _path: &Path,
        _req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        Err(MediaError::NotYet {
            what: "audio decoding (M1 slice 6)",
        })
    }
}

// An ffmpeg-next change must not drop `Send` or `Sync` without a build
// failing here.
const _: () = {
    const fn send<T: Send>() {}
    const fn sync<T: Sync>() {}
    send::<VideoFrame>();
    sync::<VideoFrame>();
    send::<Arc<VideoFrame>>();
    send::<crate::decode::VideoDecoder>();
    send::<crate::seek::FfmpegVideo>();
    send::<Box<dyn VideoDecode>>();
    send::<FfmpegBackend>();
    sync::<FfmpegBackend>();
};
