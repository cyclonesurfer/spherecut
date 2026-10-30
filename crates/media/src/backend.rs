//! The `MediaBackend` traits: what the player, the CLI and the tests open
//! media through (M1 plan section 3.2, DECISIONS D29).
//!
//! A backend probes files and opens decoders. A [`VideoDecode`] hands out
//! [`VideoFrame`]s on the clip's own time axis, where 0 is the first
//! presented frame, and seeks frame-exactly (section 3.3, `seek.rs`).
//! [`FfmpegBackend`] is the real one; synth's `SyntheticBackend` and
//! `GatedBackend` stand in for it in tests.
//!
//! Frames are decoded in software, or with [`DecodeMode::Hardware`] on a
//! VA-API or Vulkan Video device: the backend's own for its kind, opened
//! once on the render node of the GPU that renders (`media::hw`), or one
//! the request brings. A device that can't decode the stream, or fails on
//! it, either fails the decode ([`Fallback::Fail`]) or hands it to
//! software, which says so with [`DecoderEvent::FellBack`] and goes on
//! from the frame after the last one returned (DECISIONS D30). Slice 5b
//! adds a GPU-resident [`FrameStorage`] variant. Audio streams decode
//! through their own demuxer and decoder, to the output's rate and
//! channels on the same clip time axis (`media::audio`, D35).

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use spherecut_core::video::{
    ColorInfo, DmabufLayer, PixelLayout, PlaneData, PlaneDesc, SampleBytes, VideoPlanes, VkPicture,
    fourcc,
};
use spherecut_core::{FrameRate, Rational, StopToken, Time};
use thiserror::Error;

use crate::hw::RenderNode;
use crate::probe::{ProbeError, ProbeReport};
use crate::spherical::{Projection, StereoMode};
use crate::{HwDevice, HwError, HwRefusal, MediaError, ff, ffi};

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
    /// best frame held during a seek, one spare (`extra_hw_frames`).
    /// With [`HwOutput::DrmPrime`] it is at least [`ZERO_COPY_SURFACES`].
    pub extra_hw_frames: u8,
    /// What a device decoder hands frames out as (slice 5b).
    pub output: HwOutput,
    /// Set at shutdown only: FFmpeg stops reading inside a read, and the
    /// stream ends for good (D13). Seeks stop through [`StopToken`]s.
    pub interrupt: Arc<AtomicBool>,
    /// Tests only: fail a frame as the device would.
    #[cfg(feature = "fault-injection")]
    pub fault: Option<FaultPlan>,
}

impl Default for VideoRequest {
    fn default() -> Self {
        VideoRequest {
            stream: None,
            decode: DecodeMode::Software,
            threads: None,
            extra_hw_frames: 2,
            output: HwOutput::Download,
            interrupt: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "fault-injection")]
            fault: None,
        }
    }
}

/// What a device decoder hands frames out as (M1 plan slice 5b).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwOutput {
    /// Copied into system memory (D30).
    #[default]
    Download,
    /// On VA-API, left on the device and exported as DMA-BUFs, a layer a
    /// plane ([`VideoPlanes::dmabuf`]), for a GPU to import. A frame that
    /// doesn't export so (a compressed layout, cropping on the left or
    /// top, a format that needs converting) is downloaded, and its
    /// [`VideoFrame::transfer`] says so. Vulkan Video frames are always
    /// downloaded.
    DrmPrime,
    /// On Vulkan, left on the device as FFmpeg's image
    /// ([`VideoPlanes::vulkan`]), for the GPU that shares the device to
    /// copy (slice 5b's path B, `HwDevice::vulkan_shared`). A frame that
    /// can't be (cropped on the left or top, a format that needs
    /// converting, images that don't hold its planes) is downloaded, and
    /// its [`VideoFrame::transfer`] says so. VA-API frames are always
    /// downloaded.
    Vulkan,
}

/// Surfaces a VA-API decoder keeps beyond its own with
/// [`HwOutput::DrmPrime`], for the exported frames others hold: the frame
/// cache's, those in flight to the GPU and those on screen.
pub const ZERO_COPY_SURFACES: u8 = 24;

/// How a frame left its decoder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Transfer {
    /// Decoded in system memory.
    #[default]
    System,
    /// Copied from the device; with the reason when it was to have been
    /// exported ([`HwOutput::DrmPrime`]).
    Downloaded { not_exported: Option<String> },
    /// Exported as DMA-BUFs, still on the device.
    DrmPrime,
    /// Left on the Vulkan device as FFmpeg's image.
    Vulkan,
}

/// Which frame a test fails "as hardware", and how (the `fault-injection`
/// feature, which only synth's tests turn on).
#[cfg(feature = "fault-injection")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultPlan {
    /// The frame that fails, by the order the decoder makes frames in
    /// since it opened: 0 is the first. Frames dropped as corrupt or as
    /// leading pictures count.
    pub frame: u64,
    pub fault: Fault,
    /// Decode in software in the device's place, labelled as on it: for
    /// machines where no device decodes (Tier 1). The fallback then reopens
    /// in software for real.
    pub stand_in: bool,
}

/// How a [`FaultPlan`]'s frame fails.
#[cfg(feature = "fault-injection")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// As it comes out of the decoder, as when decoding fails.
    Decode,
    /// As it is copied to system memory, when it is about to be handed
    /// out; a frame never handed out never fails.
    Download,
    /// Not a frame: every decode from the stream's start after packets
    /// were sent opens the decoder afresh, as for a stream whose first
    /// picture is no keyframe (D29), and that fails, as when the device can
    /// make no more decoders. The plan's `frame` isn't looked at.
    Reopen,
}

#[derive(Debug, Clone)]
pub enum DecodeMode {
    Software,
    /// On a device of kind `kind`: `device`, or the backend's own when
    /// `None`. What happens when the device can't be opened, can't decode
    /// the stream or fails on it is `fallback`'s choice (D30).
    Hardware {
        kind: HwKind,
        device: Option<Arc<HwDevice>>,
        fallback: Fallback,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwKind {
    VaApi,
    Vulkan,
}

impl HwKind {
    /// FFmpeg's name for the device type.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            HwKind::VaApi => "vaapi",
            HwKind::Vulkan => "vulkan",
        }
    }
}

impl std::fmt::Display for HwKind {
    /// "VA-API" or "Vulkan", as errors and notices say it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            HwKind::VaApi => "VA-API",
            HwKind::Vulkan => "Vulkan",
        })
    }
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
    /// Asked for hardware, decoding in software ([`Fallback::Software`]).
    FellBack {
        from: HwKind,
        reason: FallbackReason,
    },
}

/// Why a hardware decode went on in software.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackReason {
    /// The device turned the stream down ([`MediaError::HwUnsupported`]).
    Refused(HwRefusal),
    /// The device could not be opened: [`MediaError::Hardware`]'s words.
    DeviceUnavailable(String),
    /// Decoding or downloading a frame failed on the device
    /// ([`MediaError::HwFailed`]'s words), after the frame at `after_pts`
    /// was returned: software goes on from the frame after it. `None`
    /// when no frame had been returned.
    Failed {
        after_pts: Option<i64>,
        error: String,
    },
}

impl std::fmt::Display for FallbackReason {
    /// D14's reason, for the warning and the notices.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FallbackReason::Refused(r) => write!(f, "{r}"),
            FallbackReason::DeviceUnavailable(e) => f.write_str(e),
            FallbackReason::Failed {
                after_pts: Some(pts),
                error,
            } => write!(f, "{error} (after the frame at pts {pts})"),
            FallbackReason::Failed {
                after_pts: None,
                error,
            } => f.write_str(error),
        }
    }
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
    /// The frame after the last one `seek` or `next_frame` returned, once
    /// the frame after it is known too (for its duration), or the end, or
    /// [`NextFrame::Stopped`]. Damage that the decode goes on past is a
    /// [`DecoderEvent::Damaged`]; an error is returned only when the stream
    /// can't go on (a read that failed), and again by every call until a
    /// seek. A read that failed is a `Damaged` event too, with its cause,
    /// once: before the first frame the decoder drains after it (frames
    /// the damage lost may come before that one), else before the error.
    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError>;
    /// `Some(t)`: skip frames nothing refers to (`AVDISCARD_NONREF`), and
    /// never return a frame before `t`, until one at or after it has been
    /// returned. `None`: back to normal, as after a seek: the next frame
    /// is the one after the last returned, even when catching up had
    /// passed it over. A later `t` goes on catching up, and an earlier one
    /// starts again from that frame. For playback that has fallen behind
    /// the clock. A stream whose frames are counted from the start (whose
    /// timestamps go backwards, or that has none) skips no frames.
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
    /// The clip time at which the last frame's interval ends, where a
    /// seek at or after it is `AfterLast`: the stream's duration (MP4),
    /// else its `DURATION` tag (Matroska), else the file's. When the
    /// container says nothing, or ends the stream at or before its last
    /// frame, it becomes the last frame's pts plus its duration once that
    /// frame has been decoded.
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

    /// Clip time 0 in the file's own time: the first presented frame's
    /// pts, in seconds.
    pub fn origin(&self) -> Result<Time, spherecut_core::TimeError> {
        Time::from_pts(self.origin_pts, self.time_base)
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
    /// the same and `ctts` shows them as evenly, or from a Matroska track
    /// whose default duration is a standard rate that its first blocks'
    /// timestamps and its Cues keep, to the millisecond. It becomes
    /// [`Variable`](Timing::Variable) if timestamps turn out to go
    /// backwards ([`DecoderEvent::NotExactlySeekable`]).
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
    /// Decoding went on in software, as [`DecodePath::FellBack`] says:
    /// once per decoder, before the events of the software decode.
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
        cause: DamageCause,
    },
    /// Timestamps went backwards after `at_pts` in decode order, so exact
    /// seeks decode from the start from now on: correct, and slow.
    NotExactlySeekable { at_pts: i64 },
}

/// What kind of damage a [`DecoderEvent::Damaged`] is, which says the
/// CLI's code for it (D27, the M1 plan's amendment 8): a seek past it is
/// [`Exactness::Truncated`] whatever the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DamageCause {
    /// The file ends before the data its index lists: it was cut short.
    Cut,
    /// Reading the file failed: an I/O error, such as a failing disk.
    Read,
    /// A read was interrupted by the decoder's interrupt flag (shutdown).
    Cancelled,
    /// The data is damaged: packets the demuxer can't read, or that the
    /// decoder fails on.
    Corrupt,
}

impl DamageCause {
    /// The code of the CLI's JSON contract for a frame lost to it:
    /// `truncated`, `io_error`, `cancelled` or `damaged`.
    pub fn code(self) -> &'static str {
        match self {
            DamageCause::Cut => "truncated",
            DamageCause::Read => "io_error",
            DamageCause::Cancelled => "cancelled",
            DamageCause::Corrupt => "damaged",
        }
    }
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
    /// Buffers allocated so far to download frames into, for every
    /// format and size (0 in software). Once the decoder's pool holds the
    /// frames kept alive, a download allocates none (D30).
    pub download_buffers: u64,
    /// Times the decoder was opened afresh to decode the stream's start
    /// again (D29): each is a new decoder, and on a device a new VA-API
    /// context or Vulkan video session.
    pub reopens: u64,
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
    /// The run it was decoded in, and its place in decode-output order
    /// in it: two frames of one run whose seq follow on are adjacent in
    /// the stream, which is what the cache links runs of frames by. A run
    /// starts at a seek (one that decodes forward goes on with the run
    /// before), and at the first frame handed out after catching up may
    /// have skipped frames.
    pub run: u64,
    pub seq: u64,
    /// How it left the decoder.
    pub transfer: Transfer,
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
    /// A VA-API frame, and its mapping to DRM PRIME, which holds the
    /// exported fds the layers name.
    DrmPrime {
        device: ff::frame::Video,
        _mapped: ff::frame::Video,
        layers: Vec<DmabufLayer>,
    },
    /// A Vulkan Video frame, and where its planes are in its images.
    Vulkan(ffi::VkFrame),
}

/// The DRM fourcc a plane exports as, with a layer per plane.
fn plane_fourcc(plane: &PlaneDesc) -> Option<u32> {
    match (plane.components, plane.bytes) {
        (1, SampleBytes::One) => Some(fourcc::R8),
        (2, SampleBytes::One) => Some(fourcc::GR88),
        (1, SampleBytes::Two) => Some(fourcc::R16),
        (2, SampleBytes::Two) => Some(fourcc::GR1616),
        _ => None,
    }
}

/// `layers` as `layout`'s planes for a `size` picture: a layer of one
/// memory plane for each plane, of the plane's own fourcc. What doesn't
/// match is the reason the frame is downloaded instead.
pub(crate) fn match_layers(
    layout: &PixelLayout,
    size: (u32, u32),
    layers: &[ffi::DrmLayer],
) -> Result<Vec<DmabufLayer>, String> {
    if layers.len() != layout.planes.len() {
        return Err(format!(
            "it exported {} layers for {}'s {} planes",
            layers.len(),
            layout.name,
            layout.planes.len()
        ));
    }
    layout
        .planes
        .iter()
        .zip(layers)
        .enumerate()
        .map(|(i, (plane, l))| {
            if l.planes != 1 {
                return Err(format!(
                    "layer {i} has {} memory planes (modifier {:#x}), \
                     a compressed layout a single-plane import can't take",
                    l.planes, l.modifier
                ));
            }
            let want = plane_fourcc(plane).ok_or_else(|| format!("plane {i} has no fourcc"))?;
            if l.fourcc != want {
                return Err(format!(
                    "layer {i} is {} where {} was expected",
                    fourcc::name(l.fourcc),
                    fourcc::name(want)
                ));
            }
            Ok(DmabufLayer {
                fd: l.fd,
                fourcc: l.fourcc,
                modifier: l.modifier,
                offset: l.offset,
                pitch: l.pitch,
                size: plane.size(size.0, size.1),
            })
        })
        .collect()
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
        let bytes = match ffi::buffer_bytes(&frame) {
            0 => (0..layout.planes.len())
                .filter_map(|i| ffi::plane(&frame, i))
                .map(<[u8]>::len)
                .sum(),
            held => held,
        };
        VideoFrame {
            pts,
            time,
            duration: None,
            key: f.key,
            concealed: f.concealed,
            decoded_on: DecodePath::Software { threads: None },
            run: 0,
            seq: 0,
            transfer: Transfer::System,
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
            transfer: Transfer::System,
            storage: FrameStorage::Owned { planes, strides },
            layout,
            color,
            size,
            bytes,
        })
    }

    /// A frame left on its VA-API device: `device`, mapped to DRM PRIME as
    /// `mapped` (whose fds `layers` name), as `layout`'s planes at `size`.
    /// It keeps no system memory, so it counts no bytes.
    pub(crate) fn from_drm_prime(
        device: ff::frame::Video,
        mapped: ff::frame::Video,
        layers: Vec<DmabufLayer>,
        layout: Arc<PixelLayout>,
        color: ColorInfo,
        size: (u32, u32),
        (pts, time): (i64, Time),
    ) -> VideoFrame {
        let f = ffi::frame_fields(&device);
        VideoFrame {
            pts,
            time,
            duration: None,
            key: f.key,
            concealed: f.concealed,
            decoded_on: DecodePath::Software { threads: None },
            run: 0,
            seq: 0,
            transfer: Transfer::DrmPrime,
            storage: FrameStorage::DrmPrime {
                device,
                _mapped: mapped,
                layers,
            },
            layout,
            color,
            size,
            bytes: 0,
        }
    }

    /// A frame left on its Vulkan device: `device`, as `vk` has its
    /// planes, as `layout`'s planes at `size`. It keeps no system memory,
    /// so it counts no bytes.
    pub(crate) fn from_vulkan(
        vk: ffi::VkFrame,
        layout: Arc<PixelLayout>,
        color: ColorInfo,
        size: (u32, u32),
        (pts, time): (i64, Time),
    ) -> VideoFrame {
        let f = ffi::frame_fields(vk.frame());
        VideoFrame {
            pts,
            time,
            duration: None,
            key: f.key,
            concealed: f.concealed,
            decoded_on: DecodePath::Software { threads: None },
            run: 0,
            seq: 0,
            transfer: Transfer::Vulkan,
            storage: FrameStorage::Vulkan(vk),
            layout,
            color,
            size,
            bytes: 0,
        }
    }

    /// This system-memory frame uploaded by FFmpeg onto `device`, a
    /// Vulkan device, as a frame left there would be: for tests of what
    /// reads such frames, without a Vulkan Video decoder.
    #[doc(hidden)]
    pub fn uploaded_to_vulkan(&self, device: &HwDevice) -> Result<VideoFrame, MediaError> {
        let fail = |reason: String| MediaError::Decode {
            path: PathBuf::new(),
            reason: format!("uploading a frame to Vulkan: {reason}"),
        };
        let planes = (0..self.layout.planes.len())
            .map(|i| self.plane(i).map(|p| (p.bytes, p.stride)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| fail("the frame isn't in system memory".into()))?;
        let frame =
            ffi::upload_to_vulkan(device, &self.layout, self.size, &planes).map_err(fail)?;
        let vk = ffi::VkFrame::new(frame, &self.layout).map_err(|(_, why)| fail(why))?;
        let mut out = VideoFrame::from_vulkan(
            vk,
            self.layout.clone(),
            self.color,
            self.size,
            (self.pts, self.time),
        );
        out.duration = self.duration;
        out.key = self.key;
        out.decoded_on = self.decoded_on.clone();
        out.run = self.run;
        out.seq = self.seq;
        Ok(out)
    }

    /// Whether the frame is still on its device ([`Transfer::DrmPrime`]
    /// or [`Transfer::Vulkan`]): its planes are imported or copied on the
    /// GPU, not read.
    pub fn on_device(&self) -> bool {
        matches!(
            self.storage,
            FrameStorage::DrmPrime { .. } | FrameStorage::Vulkan(_)
        )
    }

    /// A frame still on its device, copied into system memory: what a
    /// caller that can't import it reads (`None` for one that isn't on a
    /// device). Blocks while the device copies.
    pub fn to_system(&self) -> Option<Result<VideoFrame, MediaError>> {
        let device = match &self.storage {
            FrameStorage::DrmPrime { device, .. } => device,
            FrameStorage::Vulkan(vk) => vk.frame(),
            _ => return None,
        };
        let down = match ffi::download(device) {
            Ok(d) => d,
            Err(e) => {
                return Some(Err(MediaError::Decode {
                    path: PathBuf::new(),
                    reason: format!("copying a frame from its device: {e}"),
                }));
            }
        };
        let mut out =
            VideoFrame::from_ffmpeg(down, self.layout.clone(), self.color, self.pts, self.time);
        out.duration = self.duration;
        out.key = self.key;
        out.concealed = self.concealed;
        out.decoded_on = self.decoded_on.clone();
        out.run = self.run;
        out.seq = self.seq;
        out.transfer = Transfer::Downloaded {
            not_exported: Some(
                match self.transfer {
                    Transfer::Vulkan => {
                        "downloaded from its Vulkan image, for a reader that can't copy it"
                    }
                    _ => "downloaded after the export, for a reader that can't import",
                }
                .into(),
            ),
        };
        Some(Ok(out))
    }

    /// The bytes keeping it keeps, for the frame cache's budget (M1 plan
    /// section 8.3): an FFmpeg frame's buffers by their `AVBufferRef`
    /// sizes, so with row padding, alignment and the rows a decoder
    /// allocates past the height; an owned frame's planes.
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
            FrameStorage::DrmPrime { .. } | FrameStorage::Vulkan(_) => std::ptr::null(),
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
            FrameStorage::DrmPrime { .. } | FrameStorage::Vulkan(_) => return None,
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

    fn dmabuf(&self) -> Option<&[DmabufLayer]> {
        match &self.storage {
            FrameStorage::DrmPrime { layers, .. } => Some(layers),
            _ => None,
        }
    }

    fn vulkan(&self) -> Option<&dyn VkPicture> {
        match &self.storage {
            FrameStorage::Vulkan(vk) => Some(vk),
            _ => None,
        }
    }
}

/// How to open an audio stream.
#[derive(Debug, Clone)]
pub struct AudioRequest {
    /// The stream, by its index in the file; `None` for the first audio
    /// stream with a decoder.
    pub stream: Option<usize>,
    pub out: AudioOutFormat,
    /// Clip time 0 in the file's own time: the first presented frame of
    /// the video that the audio plays with ([`VideoStreamInfo::origin`]).
    /// `None` puts it at the audio's first sample.
    pub origin: Option<Time>,
    /// Set at shutdown only, as [`VideoRequest::interrupt`].
    pub interrupt: Arc<AtomicBool>,
}

/// Packed f32 at `rate`, `channels` to a frame.
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

/// The FFmpeg backend: every open is confined to the file (D24). Frames
/// are decoded in software, or on a hardware device. The backend opens
/// one device of each kind, on the render node of the GPU that renders
/// (`media::hw`), the first time a decoder asks for it, and every decoder
/// after shares it, as does the answer when it can't be opened (D30).
#[derive(Debug, Default)]
pub struct FfmpegBackend {
    /// The render node to open devices on instead of the default's.
    render_node: Option<RenderNode>,
    vaapi: OnceLock<Result<Arc<HwDevice>, HwError>>,
    vulkan: OnceLock<Result<Arc<HwDevice>, HwError>>,
}

impl FfmpegBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// A backend whose devices open on `node` (`--render-node`).
    pub fn on_render_node(node: impl Into<PathBuf>) -> Self {
        Self::on_node(crate::hw::render_node(None, Some(&node.into())))
    }

    /// A backend whose devices open on `node`, as [`crate::hw::render_node`]
    /// found it for the adapter that renders (`spherecut frame`).
    pub fn on_node(node: RenderNode) -> Self {
        FfmpegBackend {
            render_node: Some(node),
            ..Self::default()
        }
    }

    /// The render node its devices open on, and why that one: the one it
    /// was made with, else the default.
    pub fn render_node(&self) -> RenderNode {
        self.render_node
            .clone()
            .unwrap_or_else(|| crate::hw::render_node(None, None))
    }

    /// The backend's device of kind `kind`, opened on its render node the
    /// first time; its error, the same every time, when it can't be. The
    /// Vulkan device is the one behind the node (D38), and Vulkan devices
    /// that run on the CPU don't count: they have no video decode.
    pub fn device(&self, kind: HwKind) -> Result<Arc<HwDevice>, MediaError> {
        let cell = match kind {
            HwKind::VaApi => &self.vaapi,
            HwKind::Vulkan => &self.vulkan,
        };
        cell.get_or_init(|| {
            let node = self.render_node();
            match open_device(kind, &node) {
                Ok((device, on)) => {
                    tracing::info!("hardware decoding with {kind} on {on}");
                    Ok(device)
                }
                Err(e) => {
                    tracing::info!("no {kind} device for hardware decoding: {e}");
                    Err(e)
                }
            }
        })
        .clone()
        .map_err(|reason| MediaError::Hardware {
            kind: kind.to_string(),
            reason,
        })
    }
}

/// A device of kind `kind` on `node`, for [`FfmpegBackend::device`], and
/// what it runs on: the node for VA-API, and for Vulkan the GPU, which is
/// the first one listed when none is listed as the one behind the node
/// (D38), even a node `--render-node` names.
fn open_device(kind: HwKind, node: &RenderNode) -> Result<(Arc<HwDevice>, String), HwError> {
    let no_device = |reason: String| HwError::NoDevice {
        node: node.to_string(),
        reason,
    };
    match kind {
        HwKind::VaApi => {
            let path = node
                .path
                .to_str()
                .ok_or_else(|| no_device("the path is not UTF-8".into()))?;
            HwDevice::open(kind.ffmpeg_name(), Some(path))
                .map(|d| (Arc::new(d), node.to_string()))
                .map_err(|e| no_device(e.to_string()))
        }
        HwKind::Vulkan => {
            let choice = crate::decode::open_vulkan_device(Some(&node.path)).map_err(|e| {
                no_device(match e {
                    MediaError::Hardware { reason, .. } => reason.to_string(),
                    e => e.to_string(),
                })
            })?;
            if choice.used.is_cpu() {
                return Err(no_device(format!(
                    "every Vulkan device FFmpeg lists runs on the CPU, with no video \
                     decode: {choice}"
                )));
            }
            let on = choice.to_string();
            Ok((choice.device, on))
        }
    }
}

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
            DecodeMode::Hardware {
                kind,
                device,
                fallback,
            } => {
                let device = || match device {
                    Some(d) => Ok(d.clone()),
                    None => self.device(*kind),
                };
                crate::fallback::open(path, req, *kind, device, *fallback)
            }
        }
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        Ok(Box::new(crate::audio::FfmpegAudio::open(path, req)?))
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
    send::<VideoStreamInfo>();
    sync::<VideoStreamInfo>();
    send::<SeekOutcome>();
    send::<NextFrame>();
    send::<DecoderEvent>();
    send::<Box<dyn AudioDecode>>();
    send::<crate::audio::FfmpegAudio>();
    send::<StopToken>();
    sync::<StopToken>();
};

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(fmt: ffmpeg_sys_next::AVPixelFormat) -> PixelLayout {
        ffi::pixel_layout(fmt as i32).expect("a layout")
    }

    fn layer(fourcc: u32, offset: u64, pitch: u64) -> ffi::DrmLayer {
        ffi::DrmLayer {
            fourcc,
            planes: 1,
            fd: 7,
            modifier: 0x0200_0000_0004_0b01,
            offset,
            pitch,
        }
    }

    #[test]
    fn nv12_and_p010_export_a_layer_a_plane_of_their_own_fourcc() {
        use ffmpeg_sys_next::AVPixelFormat as F;
        let nv12 = layout(F::AV_PIX_FMT_NV12);
        let got = match_layers(
            &nv12,
            (5761, 2881),
            &[
                layer(fourcc::R8, 0, 6144),
                layer(fourcc::GR88, 6144 * 2944, 6144),
            ],
        )
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].size, (5761, 2881));
        // Chroma rounds up, as FFmpeg's planes do.
        assert_eq!(got[1].size, (2881, 1441));
        assert_eq!((got[1].offset, got[1].pitch), (6144 * 2944, 6144));
        assert_eq!(got[1].fd, 7);
        let p010 = layout(F::AV_PIX_FMT_P010LE);
        let got = match_layers(
            &p010,
            (64, 32),
            &[layer(fourcc::R16, 0, 256), layer(fourcc::GR1616, 8192, 256)],
        )
        .unwrap();
        assert_eq!(
            got.iter().map(|l| l.fourcc).collect::<Vec<_>>(),
            [fourcc::R16, fourcc::GR1616]
        );
    }

    #[test]
    fn layers_that_aren_t_the_planes_are_refused_with_the_reason() {
        use ffmpeg_sys_next::AVPixelFormat as F;
        let nv12 = layout(F::AV_PIX_FMT_NV12);
        let size = (64, 32);
        // One composed layer for both planes.
        let why = match_layers(&nv12, size, &[layer(fourcc::R8, 0, 64)]).unwrap_err();
        assert!(why.contains("1 layers for nv12's 2 planes"), "{why}");
        // A compression modifier's metadata plane.
        let mut dcc = layer(fourcc::GR88, 2048, 64);
        dcc.planes = 2;
        let why = match_layers(&nv12, size, &[layer(fourcc::R8, 0, 64), dcc]).unwrap_err();
        assert!(why.contains("layer 1 has 2 memory planes"), "{why}");
        // A 10-bit plane where 8 bits were expected.
        let why = match_layers(
            &nv12,
            size,
            &[layer(fourcc::R16, 0, 64), layer(fourcc::GR88, 2048, 64)],
        )
        .unwrap_err();
        assert_eq!(why, "layer 0 is R16 where R8 was expected");
        assert_eq!(fourcc::name(fourcc::GR1616), "GR32");
    }
}
