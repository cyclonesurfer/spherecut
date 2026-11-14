//! One frame of a video through a virtual camera: the core of `spherecut
//! frame` (M1 plan section 5, D27), which `look` renders its tiles with
//! (a1-plan section 2). The steps are frame's 4 to 8:
//!
//! 4. [`probe()`]: the file probed through the confinement (D24), its video
//!    stream chosen, and checked to be a stitched equirect unless the
//!    caller says it is one;
//! 5. [`gpu`] and [`check_gpu`]: a headless GPU, and whether it can keep
//!    frames on the device when that is asked;
//! 6. [`Still::open`]: the decoder, on the render node of the GPU that
//!    renders (D39 has the order `auto` tries);
//! 7. [`Still::seek`]: the frame, exactly;
//! 8. [`Still::render`]: the picture, on the GPU or the CPU reference.
//!
//! Each step fails with D27's [`ErrorBody`], in the words frame has
//! always used; what a caller adds to them (frame's flags, look's `next`)
//! it adds itself. Warnings go to the list the caller passes.
//!
//! A file of two lenses, an Insta360 `.insv` (M3.5), opens with
//! [`Still::open_pair`] in place of steps 4's choice and 6: from
//! [`probe_report`]'s report, not a [`Probed`], since [`probed`] picks
//! one stream and refuses the layout unless [`Options::equirect`]. Its
//! [`StillPair`] seeks both lenses to one time and stitches them through
//! the views on the GPU (`render_blurred` of a `SourceFrames::Pair`,
//! M3.7), or with no GPU through one view on the CPU reference
//! (`render::reference::render_rig_of`). [`pair_stitching`] reads the
//! stitch the file's trailer gives, for a caller that changes the
//! geometry.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{Stitching, parse_metadata, read_record, read_trailer, stitching};
use spherecut_core::cancel::{StopSource, StopToken};
use spherecut_core::video::VideoPlanes;
use spherecut_core::{FrameTime, Rounding, Time, TimeSpec};
use spherecut_media::backend::{
    DamageCause, DecodeMode, DecodePath, DecoderEvent, Exactness, Fallback, FallbackReason,
    FfmpegBackend, HwKind, HwOutput, MediaBackend, NextFrame, SeekMode, SeekOutcome, Timing,
    Transfer, VideoDecode, VideoFrame, VideoRequest, VideoStreamInfo,
};
use spherecut_media::detect::Layout;
use spherecut_media::hw::{self, GpuIds, RenderNode};
use spherecut_media::probe::{self, ProbeReport, StreamInfo, StreamKind};
use spherecut_media::seek::{FrameAddress, frame_address};
use spherecut_media::spherical::{Projection as SvProjection, StereoMode};
use spherecut_media::{HwDevice, MediaError, Source};
use spherecut_render::dmabuf::{self, Imported, Importer};
use spherecut_render::reference::RigOptions;
use spherecut_render::vkshare::Copied;
use spherecut_render::{
    AdapterChoice, DebugMode, Depth, Filter, Gpu, Image, LensFrame, Pixels, Placement, PlaneSource,
    RenderError, SlotShape, SourceFrames, Stereo, StillOptions, StillRenderer, ViewParams,
    reference, upload, wgpu,
};
use spherecut_sphere::rig::{Geometry, Rig, Tune};

use crate::contract::{
    DecodeInfo, ErrorBody, FallbackInfo, GpuInfo, Size, SourceInfo, Warning, codes,
};

/// A failed step.
pub type Failure = ErrorBody;

/// What decodes, as `frame --decoder` names it (D39).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decoder {
    /// VA-API on the GPU's render node, else software.
    #[default]
    Auto,
    Software,
    /// VA-API, or fail.
    Vaapi,
    /// Vulkan Video, or fail.
    Vulkan,
}

/// How a still is made.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub decoder: Decoder,
    /// The DRM render node to decode on; `None` for the rendering GPU's,
    /// else `/dev/dri/renderD128`.
    pub render_node: Option<PathBuf>,
    /// Render the file as a stitched equirect whatever its metadata says.
    pub equirect: bool,
    /// The video stream, by FFmpeg's index; `None` for the first.
    pub stream: Option<usize>,
    /// Keep the frame on the device, with no copy through the CPU (slice
    /// 5b): VA-API's DMA-BUFs imported, or with [`Decoder::Vulkan`]
    /// Vulkan Video on the renderer's own device.
    pub zero_copy: bool,
}

impl Options {
    /// Whether FFmpeg decodes on the renderer's own Vulkan device (path B).
    pub fn shared(&self) -> bool {
        self.zero_copy && self.decoder == Decoder::Vulkan
    }
}

/// What stops a still's work: a token for seeks, and the flag FFmpeg's
/// interrupt callback reads inside a read. Both stop together.
#[derive(Debug, Clone)]
pub struct Stop {
    token: StopToken,
    flag: Arc<AtomicBool>,
}

impl Stop {
    pub fn new(token: StopToken, flag: Arc<AtomicBool>) -> Stop {
        Stop { token, flag }
    }

    /// What stops when `source` shuts down.
    pub fn of(source: &StopSource) -> Stop {
        Stop::new(source.current(), source.shutdown_flag())
    }

    /// `Err(cancelled)` once the work should stop.
    pub fn go_on(&self) -> Result<(), Failure> {
        if self.flag.load(Ordering::Acquire) {
            Err(stopped())
        } else {
            Ok(())
        }
    }
}

/// The error of work that was stopped. Callers say why in their own
/// words: frame that it was interrupted, look that the call was cancelled
/// or ran out of time.
fn stopped() -> Failure {
    failure(codes::CANCELLED, "stopped")
}

pub fn failure(code: &'static str, message: impl Into<String>) -> Failure {
    ErrorBody::new(code, message)
}

/// `e` as the run's error, about `path`.
pub fn about(mut e: Failure, path: &Path) -> Failure {
    e.path = Some(path.to_string_lossy().into_owned());
    e
}

fn warn(warnings: &mut Vec<Warning>, code: &'static str, message: String) {
    tracing::warn!("{message}");
    warnings.push(Warning::new(code, message));
}

/// Step 4's findings: the file's report, and the stream to decode.
#[derive(Debug, Clone)]
pub struct Probed {
    pub report: ProbeReport,
    pub stream: StreamInfo,
}

/// Step 4: `src` probed, through the confinement (D24), its video stream
/// chosen, and, unless [`Options::equirect`], checked to be a stitched
/// equirect. Damage the seek may not reach is a warning: it fails the
/// run only if the seek does reach it.
pub fn probe(
    src: Source<'_>,
    opts: &Options,
    stop: &Stop,
    warnings: &mut Vec<Warning>,
) -> Result<Probed, Failure> {
    let report = probe_report(src, stop)?;
    probed(report, opts, stop, warnings)
}

/// Step 4's probe alone: `src` probed through the confinement (D24), or
/// probe's failure for it, with nothing chosen or checked yet.
pub fn probe_report(src: Source<'_>, stop: &Stop) -> Result<ProbeReport, Failure> {
    probe::probe_cancellable(src, stop.flag.clone()).map_err(|e| {
        let mut f = about(failure(e.code(), e.to_string()), src.name);
        f.ffmpeg_log = e.ffmpeg_log;
        f
    })
}

/// The rest of step 4, on a report [`probe_report`] made: its video
/// stream chosen and checked as [`probe()`] does, and its damage a warning.
pub fn probed(
    report: ProbeReport,
    opts: &Options,
    stop: &Stop,
    warnings: &mut Vec<Warning>,
) -> Result<Probed, Failure> {
    let file = report.path.as_path();
    let stream = choose_stream(&report, opts.stream, file)?.clone();
    if !opts.equirect {
        check_equirect(&report, &stream)?;
    }
    damage_warnings(&report, warnings);
    stop.go_on()?;
    Ok(Probed { report, stream })
}

/// The damage probe found in `report`, each a warning naming the file,
/// added to `warnings`: damage the seek may not reach, which fails the
/// run only if the seek does reach it. [`probed`] and
/// [`Still::open_pair`] give it, once the file is one they go on with.
pub fn damage_warnings(report: &ProbeReport, warnings: &mut Vec<Warning>) {
    for d in &report.damage {
        warn(
            warnings,
            d.code,
            format!("{} is damaged: {}", report.path.display(), d.message),
        );
    }
}

/// Why `probed` may not be a stitched equirect, as [`probe()`] without
/// [`Options::equirect`] refuses it: `None` when it is tagged one.
pub fn equirect_doubt(probed: &Probed) -> Option<String> {
    check_equirect(&probed.report, &probed.stream)
        .err()
        .map(|f| f.message)
}

/// The report's video streams that aren't cover pictures, which
/// [`probe()`] chooses from.
pub fn video_streams(report: &ProbeReport) -> Vec<&StreamInfo> {
    report
        .streams
        .iter()
        .filter(|s| {
            s.kind == StreamKind::Video && !s.disposition.iter().any(|d| d == "attached_pic")
        })
        .collect()
}

/// `stream`, which must be a video stream, else the first video stream
/// that isn't a cover picture. A `stream` the file hasn't is a usage
/// error, which says what the file has; the caller names its flag.
fn choose_stream<'r>(
    report: &'r ProbeReport,
    stream: Option<usize>,
    file: &Path,
) -> Result<&'r StreamInfo, Failure> {
    let videos = video_streams(report);
    match stream {
        Some(n) => videos
            .iter()
            .find(|s| s.index == n)
            .copied()
            .ok_or_else(|| {
                let listed = videos
                    .iter()
                    .map(|s| s.index.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                failure(
                    codes::USAGE,
                    format!(
                        "{} has no video stream {n} (its video streams: {})",
                        file.display(),
                        if listed.is_empty() { "none" } else { &listed }
                    ),
                )
            }),
        None => videos.first().copied().ok_or_else(|| {
            about(
                failure(
                    "decode_failed",
                    format!("{} has no video stream", file.display()),
                ),
                file,
            )
        }),
    }
}

/// What `not_equirect` says an Insta360 file is, after "{file} is ",
/// when its calibration reads ([`pair_reads`]): not one equirect, but a
/// pair that frame and look stitch (M3.7) and a project renders from
/// M3.11. The words compare's refusal of a file and a project's doubt
/// give, through [`equirect_doubt`]; frame and look reach the pair
/// through [`Still::open_pair`] instead.
pub const INSTA360_PAIR: &str = "an Insta360 camera's pair of lenses (dual fisheye), not one \
                                 equirect: frame and look stitch it, and project create makes a \
                                 project of it, which render renders from M3.11";

/// What `not_equirect` says an Insta360 file is, after "{file} is ",
/// when its calibration doesn't read: in project create's words for it
/// (`no_calibration`), which look shows so.
pub const INSTA360_NO_CALIBRATION: &str =
    "an Insta360 camera's pair of lenses (dual fisheye) with no calibration: shown as its lenses";

/// Whether `report` names a camera's pair of lenses whose calibration
/// reads: probe found its two lens tracks, and a calibration string a
/// stitch takes, for which the built-in table gives a geometry. Such a
/// file is never called unstitched: frame and look stitch it.
pub fn pair_reads(report: &ProbeReport) -> bool {
    report.camera_file.lenses.is_some() && report.camera_file.geometry.is_some()
}

/// Whether the file is a stitched equirect: tagged so (Spherical Video
/// V2's `equi` in an MP4 or MOV file, the `Projection` element FFmpeg
/// exports from a Matroska or WebM file, or V1's `equirectangular`), or
/// else `not_equirect`, naming what it is. The message says what the file
/// is, and the caller how to render it anyway.
fn check_equirect(report: &ProbeReport, stream: &StreamInfo) -> Result<(), Failure> {
    let file = report.path.display();
    let refuse = |what: String| {
        about(
            failure(codes::NOT_EQUIRECT, format!("{file} is {what}")),
            &report.path,
        )
    };
    let sph = &stream.spherical;
    if let Some(v2) = &sph.v2 {
        return match &v2.projection {
            Some(SvProjection::Equirect { .. }) => Ok(()),
            Some(SvProjection::Cubemap { .. }) => Err(refuse(
                "a cubemap (Spherical Video V2 cbmp), not an equirect: M1 renders only equirect"
                    .into(),
            )),
            Some(SvProjection::Mesh) => Err(refuse(
                "a mesh projection (Spherical Video V2 mshp), not an equirect: M1 renders only \
                 equirect"
                    .into(),
            )),
            Some(SvProjection::Other { fourcc }) => Err(refuse(format!(
                "in a projection Spherecut doesn't know (Spherical Video V2 {fourcc}): M1 \
                 renders only equirect"
            ))),
            None => Err(refuse(
                "tagged spherical (Spherical Video V2) with no projection".into(),
            )),
        };
    }
    if let Some(v1) = &sph.v1 {
        return match v1.projection_type.as_deref() {
            Some("equirectangular") => Ok(()),
            Some(other) => Err(refuse(format!(
                "in the projection {other} (Spherical Video V1): M1 renders only equirect"
            ))),
            None => Err(refuse(
                "tagged spherical (Spherical Video V1) with no projection".into(),
            )),
        };
    }
    let vendor = report
        .camera
        .as_ref()
        .and_then(|c| c.vendor.clone())
        .unwrap_or_else(|| "a camera's".into());
    let size = stream
        .video
        .as_ref()
        .map(|v| format!("{}x{}", v.width, v.height))
        .unwrap_or_else(|| "of unknown size".into());
    match report.camera.as_ref().map(|c| c.layout) {
        Some(Layout::Insta360DualFisheye) if pair_reads(report) => {
            Err(refuse(INSTA360_PAIR.into()))
        }
        Some(Layout::Insta360DualFisheye) => Err(refuse(INSTA360_NO_CALIBRATION.into())),
        Some(Layout::DjiDualFisheye) => Err(refuse(
            "DJI dual fisheye, one stream per lens: stitching arrives in M3".into(),
        )),
        Some(Layout::GoProEac) => Err(refuse(
            "a GoPro equi-angular cubemap pair (EAC): stitching arrives in M3".into(),
        )),
        Some(Layout::LowResCompanion) => Err(refuse(format!(
            "{vendor} low-resolution companion (an LRV proxy), not a stitched equirect: M3 \
             uses these as proxies"
        ))),
        Some(Layout::EquirectLikely) => Err(refuse(format!(
            "{size}, 2:1, but has no spherical metadata, so it may not be an equirect"
        ))),
        _ => Err(refuse(format!(
            "{size} with no spherical metadata, not a tagged equirect"
        ))),
    }
}

/// Step 5: a headless GPU on `adapter`, whose device FFmpeg's Vulkan
/// Video can share when `opts` asks for that (path B).
pub fn gpu(adapter: &AdapterChoice, opts: &Options) -> Result<Gpu, Failure> {
    if opts.shared() {
        Gpu::headless_shared(adapter)
    } else {
        Gpu::headless(adapter)
    }
    .map_err(render_error)
}

/// Step 5's checks of what `opts` asks of `gpu`: that it shares its
/// device with FFmpeg's Vulkan Video, or imports DMA-BUFs, for zero copy
/// (`import_failed`).
pub fn check_gpu(gpu: &Gpu, opts: &Options) -> Result<(), Failure> {
    if opts.shared()
        && let Err(why) = gpu.shared_video()
    {
        return Err(render_error(RenderError::Import(format!(
            "{} can't share its device with FFmpeg's Vulkan Video: {why}",
            gpu.caps.info.name
        ))));
    }
    if opts.zero_copy && !opts.shared() && !gpu.caps.dmabuf_import {
        return Err(render_error(RenderError::Import(format!(
            "{} can't import DMA-BUFs (it lacks \
             VK_EXT_external_memory_dma_buf or VK_EXT_image_drm_format_modifier)",
            gpu.caps.info.name
        ))));
    }
    Ok(())
}

/// How a still is drawn: through the view, or flat; the filter; the bits
/// per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub draw: Draw,
    pub filter: Filter,
    pub depth: Depth,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            draw: Draw::View,
            filter: Filter::Isotropic,
            depth: Depth::Eight,
        }
    }
}

/// What a still draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Draw {
    /// The sphere through the view, as frame does.
    #[default]
    View,
    /// The picture of the eye shown, flat, scaled to the size: an
    /// equirect as it is, for look's `sphere` and `sheet`.
    Eye,
    /// The coded picture whole, flat, scaled to the size: look's `raw`.
    Coded,
}

/// The decoder and what it found, for the report.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub frame: Arc<VideoFrame>,
    pub requested: FrameTime,
    pub shown: FrameTime,
}

/// A file open for stills: its decoder, where its picture sits on the
/// sphere, and what renders it. Each [`seek`](Self::seek) decodes a frame
/// that [`render`](Self::render) can draw through any number of views.
pub struct Still<'g> {
    /// The path messages name the file by.
    name: PathBuf,
    gpu: Option<&'g Gpu>,
    renderer: Option<StillRenderer>,
    dec: Box<dyn VideoDecode>,
    stream: StreamInfo,
    placement: Placement,
    /// The fallback `auto` took without trying, with no device to try.
    skipped: Option<FallbackInfo>,
    opts: Options,
    stop: Stop,
}

impl std::fmt::Debug for Still<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Still")
            .field("name", &self.name)
            .field("stream", &self.stream.index)
            .field("placement", &self.placement)
            .finish_non_exhaustive()
    }
}

impl<'g> Still<'g> {
    /// Step 6: the decoder for `probed`'s stream of `src`, on the render
    /// node of `gpu` (or, with no GPU or the CPU's, the node
    /// [`Options::render_node`] names, else `renderD128`). With `gpu`
    /// `None` the still renders on the CPU reference.
    pub fn open(
        src: Source<'_>,
        probed: &Probed,
        gpu: Option<&'g Gpu>,
        opts: &Options,
        stop: &Stop,
        warnings: &mut Vec<Warning>,
    ) -> Result<Self, Failure> {
        Self::open_stream(src, &probed.stream, gpu, opts, stop, warnings)
    }

    /// [`Still::open`] of `stream`, one of `src`'s.
    fn open_stream(
        src: Source<'_>,
        stream: &StreamInfo,
        gpu: Option<&'g Gpu>,
        opts: &Options,
        stop: &Stop,
        warnings: &mut Vec<Warning>,
    ) -> Result<Self, Failure> {
        Self::open_stream_on(src, stream, gpu, opts, None, stop, warnings)
    }

    /// [`Still::open_stream`], decoding on `device` when FFmpeg shares
    /// the renderer's Vulkan device (path B): a pair's two lenses share
    /// one, as playback's streams do. `None` makes the still its own.
    fn open_stream_on(
        src: Source<'_>,
        stream: &StreamInfo,
        gpu: Option<&'g Gpu>,
        opts: &Options,
        device: Option<Arc<HwDevice>>,
        stop: &Stop,
        warnings: &mut Vec<Warning>,
    ) -> Result<Self, Failure> {
        let ids = gpu
            .filter(|g| !g.caps.is_cpu)
            .map(|g| gpu_ids(&g.caps.info));
        let node = hw::render_node(ids.as_ref(), opts.render_node.as_deref());
        let (mut decode, skipped) = decode_mode(opts.decoder, &node);
        let shared = opts.shared();
        if let (true, Some(gpu)) = (shared, gpu) {
            let device = match device {
                Some(d) => d,
                None => shared_device(gpu)?,
            };
            decode = DecodeMode::Hardware {
                kind: HwKind::Vulkan,
                device: Some(device),
                fallback: Fallback::Fail,
            };
        }
        let backend = FfmpegBackend::on_node(node);
        let req = VideoRequest {
            stream: Some(stream.index),
            decode,
            output: match (opts.zero_copy, shared) {
                (true, true) => HwOutput::Vulkan,
                (true, false) => HwOutput::DrmPrime,
                (false, _) => HwOutput::Download,
            },
            interrupt: stop.flag.clone(),
            ..VideoRequest::default()
        };
        let dec = backend
            .open_video(src.open, &req)
            .map_err(|e| media_error(e, src.name))?;
        let mut still = Still {
            name: src.name.to_path_buf(),
            gpu,
            renderer: None,
            dec,
            stream: stream.clone(),
            placement: Placement::default(),
            skipped,
            opts: opts.clone(),
            stop: stop.clone(),
        };
        still.placement = still.place(warnings);
        Ok(still)
    }

    fn file(&self) -> &Path {
        &self.name
    }

    /// The decoder's stream, as it found it.
    pub fn info(&self) -> &VideoStreamInfo {
        self.dec.info()
    }

    /// The stream probe found.
    pub fn stream(&self) -> &StreamInfo {
        &self.stream
    }

    /// Where the picture sits on the sphere.
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// The stream as D27's `source` reports it.
    pub fn source_info(&self) -> SourceInfo {
        let info = self.dec.info();
        SourceInfo {
            stream: self.stream.index,
            codec: info.codec.clone(),
            size: Size {
                width: info.size.0,
                height: info.size.1,
            },
            pix_fmt: info.source_pix_fmt.clone(),
        }
    }

    /// Where `decoded` was decoded, and how it left the decoder.
    pub fn decode_info(&self, decoded: &Decoded) -> DecodeInfo {
        self.frame_decode_info(&decoded.frame)
    }

    /// Where `frame` was decoded, and how it left the decoder.
    pub fn frame_decode_info(&self, frame: &VideoFrame) -> DecodeInfo {
        decode_info(&frame.decoded_on, &frame.transfer, self.skipped.clone())
    }

    /// The frame after the last one decoded, or `None` at the stream's
    /// end: doctor's speed test, which decodes on from a seek. Damage the
    /// decoder goes on past is a warning.
    pub fn next(
        &mut self,
        warnings: &mut Vec<Warning>,
    ) -> Result<Option<Arc<VideoFrame>>, Failure> {
        let next = self.dec.next_frame(&self.stop.token);
        let events = self.dec.take_events();
        self.damage_after(&events, warnings);
        match next {
            Ok(NextFrame::Frame(f)) => Ok(Some(f)),
            Ok(NextFrame::End) => Ok(None),
            Ok(NextFrame::Stopped) => Err(stopped()),
            Err(e) => Err(self.media_error(e)),
        }
    }

    fn media_error(&self, e: MediaError) -> Failure {
        media_error(e, self.file())
    }

    /// Where the picture sits on the sphere, from the stream's spherical
    /// metadata, with a warning for each thing M1 does with it: the left
    /// eye of a stereo picture, a crop, and a pose it doesn't apply.
    fn place(&self, warnings: &mut Vec<Warning>) -> Placement {
        let m = self.dec.info().mapping.clone();
        let stereo = match m.stereo {
            StereoMode::Mono => Stereo::Mono,
            StereoMode::TopBottom => {
                warn(
                    warnings,
                    codes::warnings::STEREO_LEFT_EYE,
                    "stereo, top-bottom: rendering the left eye (the top half)".into(),
                );
                Stereo::TopBottom
            }
            StereoMode::LeftRight => {
                warn(
                    warnings,
                    codes::warnings::STEREO_LEFT_EYE,
                    "stereo, left-right: rendering the left eye (the left half)".into(),
                );
                Stereo::LeftRight
            }
            other => {
                warn(
                    warnings,
                    codes::warnings::STEREO_UNKNOWN,
                    format!(
                        "stereo layout {other:?} is unknown: rendering the whole picture, as mono"
                    ),
                );
                Stereo::Mono
            }
        };
        let [u0, v0, u1, v1] = m.cover;
        let placement = Placement {
            left: u0,
            top: v0,
            right: 1.0 - u1,
            bottom: 1.0 - v1,
            stereo,
        };
        if m.cover != [0.0, 0.0, 1.0, 1.0] {
            warn(
                warnings,
                codes::warnings::CROP_APPLIED,
                format!(
                    "the picture covers part of the sphere (cropped by {} left, {} top, {} right \
                     and {} bottom): the rest is black",
                    number(placement.left),
                    number(placement.top),
                    number(placement.right),
                    number(placement.bottom)
                ),
            );
        }
        if m.pose_present {
            let pose = self
                .stream
                .spherical
                .v2
                .as_ref()
                .map(|v2| {
                    format!(
                        " (yaw {}°, pitch {}°, roll {}°)",
                        number(v2.pose_yaw),
                        number(v2.pose_pitch),
                        number(v2.pose_roll)
                    )
                })
                .unwrap_or_default();
            warn(
                warnings,
                codes::warnings::POSE_NOT_APPLIED,
                format!("the file's pose{pose} is not applied (M2)"),
            );
        }
        placement
    }

    /// Step 7: the frame `when` names, exactly; or with
    /// [`SeekMode::Keyframe`] the keyframe a seek to it lands on, which
    /// may come after it (`look`'s `seek: "keyframe"`). A frame number on
    /// a variable-rate stream is counted exactly either way.
    pub fn seek(
        &mut self,
        when: TimeSpec,
        mode: SeekMode,
        warnings: &mut Vec<Warning>,
    ) -> Result<Decoded, Failure> {
        let rate = self.dec.info().frame_rate();
        tracing::info!("seeking to {when}");
        let (at, requested) = match when {
            TimeSpec::Time(t) => (
                t,
                FrameTime {
                    time: t,
                    frame: None,
                },
            ),
            TimeSpec::Frame(n) => {
                let info = self.dec.info();
                let address = frame_address(info, n).map_err(|_| self.beyond(info, when))?;
                match address {
                    FrameAddress::At(t) => {
                        let requested = match rate {
                            Some(rate) => {
                                FrameTime::at(rate, n).map_err(|_| self.beyond(info, when))?
                            }
                            None => FrameTime {
                                time: t,
                                frame: Some(n),
                            },
                        };
                        (t, requested)
                    }
                    FrameAddress::Counted(n) => return self.count(n, warnings),
                }
            }
        };
        // A time that no timestamp of the stream can hold is past its end,
        // which the seek would only fail on.
        if at
            .to_pts(self.dec.info().time_base, Rounding::Floor)
            .is_err()
        {
            return Err(self.beyond(self.dec.info(), when));
        }
        let outcome = self.dec.seek(at, mode, &self.stop.token);
        let outcome = outcome.map_err(|e| self.media_error(e))?;
        let (frame, exactness) = match outcome {
            SeekOutcome::Frame {
                frame, exactness, ..
            } => (frame, exactness),
            SeekOutcome::Stopped => return Err(stopped()),
        };
        let events = self.dec.take_events();
        match exactness {
            Exactness::Exact => {}
            Exactness::KeyframeOnly if mode == SeekMode::Keyframe => {}
            Exactness::AfterLast => return Err(self.past_the_end(when, &frame)),
            Exactness::Truncated { last_good_pts } => {
                return Err(self.damaged(when, last_good_pts, &events));
            }
            Exactness::BeforeFirst | Exactness::KeyframeOnly => {
                return Err(failure(
                    codes::INTERNAL,
                    format!("an exact seek to {when} came back {exactness:?}"),
                ));
            }
        }
        self.damage_after(&events, warnings);
        let shown = shown(self.dec.as_ref(), &frame);
        if mode == SeekMode::Exact {
            self.check_index(when, &shown, warnings)?;
        }
        Ok(Decoded {
            frame,
            requested,
            shown,
        })
    }

    /// A frame number on a constant-rate stream must be the frame
    /// `shown`, which the seek can't tell where frames are missing: past
    /// the last frame of a stream whose container says it runs on, frame
    /// `n` is `out_of_range`; before it, frames are missing in its place
    /// (`damaged`). A cut the decoder can't see (in a Matroska file that
    /// doesn't say its size, D13) leaves either.
    fn check_index(
        &mut self,
        when: TimeSpec,
        shown: &FrameTime,
        warnings: &mut Vec<Warning>,
    ) -> Result<(), Failure> {
        let (TimeSpec::Frame(n), Some(_)) = (when, self.dec.info().frame_rate()) else {
            return Ok(());
        };
        let Some(i) = shown.frame.filter(|&i| i != n) else {
            return Ok(());
        };
        let file = self.file().display().to_string();
        let e = if self.is_last(warnings)? {
            failure(
                codes::OUT_OF_RANGE,
                format!("no frame {n}: {file} has {} frames, 0 to {i}", i + 1),
            )
        } else {
            failure(
                DamageCause::Corrupt.code(),
                format!(
                    "{file} has no frame {n}: at its constant rate frame {i} is shown in its \
                     place, so frames are missing there"
                ),
            )
        };
        Err(about(e, self.file()))
    }

    /// Whether the frame the last seek returned is the stream's last: the
    /// next one is decoded to see, and damage after it is a warning.
    fn is_last(&mut self, warnings: &mut Vec<Warning>) -> Result<bool, Failure> {
        let next = self.dec.next_frame(&self.stop.token);
        let events = self.dec.take_events();
        self.damage_after(&events, warnings);
        match next {
            Ok(NextFrame::Frame(_)) => Ok(false),
            Ok(NextFrame::End) => Ok(true),
            Ok(NextFrame::Stopped) => Err(stopped()),
            // The device failing is no answer.
            Err(e) if e.is_device_failure() => Err(self.media_error(e)),
            // The stream ends there, said as damage.
            Err(_) => Ok(true),
        }
    }

    /// `out_of_range` for `when`, too far on for any timestamp of the
    /// stream, naming its range as far as `info` knows it.
    fn beyond(&self, info: &VideoStreamInfo, when: TimeSpec) -> Failure {
        let file = self.file().display();
        let what = match when {
            TimeSpec::Time(t) => format!("no frame at {} s", seconds(t)),
            TimeSpec::Frame(n) => format!("no frame {n}"),
        };
        let message = match (when, info.frame_count, info.duration) {
            (TimeSpec::Frame(_), Some(c), _) if c > 0 => {
                format!("{what}: {file} has {c} frames, 0 to {}", c - 1)
            }
            (_, _, Some(d)) => format!("{what}: the clip runs from 0 to {} s", seconds(d)),
            _ => format!("{what}: it is past the end of {file}"),
        };
        about(failure(codes::OUT_OF_RANGE, message), self.file())
    }

    /// Frame `n` of a variable-rate stream, counted from the first. Damage
    /// before it fails the run, by its cause: frames it lost would go
    /// uncounted. That includes a read that failed, which the decoder says
    /// before the first frame it gives up after it, and before the error
    /// the stream ends with.
    fn count(&mut self, n: u64, warnings: &mut Vec<Warning>) -> Result<Decoded, Failure> {
        let when = TimeSpec::Frame(n);
        let first = self.dec.seek(Time::ZERO, SeekMode::Exact, &self.stop.token);
        let mut frame = match first.map_err(|e| self.media_error(e))? {
            SeekOutcome::Frame { frame, .. } => frame,
            SeekOutcome::Stopped => return Err(stopped()),
        };
        for i in 0..n {
            let next = self.dec.next_frame(&self.stop.token);
            let events = self.dec.take_events();
            if let Some(e) = self.damage_before(&events, when, &frame) {
                return Err(e);
            }
            frame = match next.map_err(|e| self.media_error(e))? {
                NextFrame::Frame(f) => f,
                NextFrame::Stopped => return Err(stopped()),
                NextFrame::End => {
                    let message = format!(
                        "no frame {n}: {} has {} frames, 0 to {i}",
                        self.file().display(),
                        i + 1
                    );
                    return Err(about(failure(codes::OUT_OF_RANGE, message), self.file()));
                }
            };
        }
        let events = self.dec.take_events();
        self.damage_after(&events, warnings);
        let at = FrameTime {
            time: frame.time,
            frame: Some(n),
        };
        Ok(Decoded {
            frame,
            requested: at,
            shown: at,
        })
    }

    /// `out_of_range` for a seek past the last frame, `last`, naming the
    /// clip's range.
    fn past_the_end(&self, when: TimeSpec, last: &VideoFrame) -> Failure {
        let info = self.dec.info();
        let end = info
            .duration
            .or_else(|| last.duration.and_then(|d| last.time.checked_add(d).ok()))
            .unwrap_or(last.time);
        let message = match when {
            TimeSpec::Frame(n) => match shown(self.dec.as_ref(), last).frame {
                Some(i) => format!(
                    "no frame {n}: {} has {} frames, 0 to {i}",
                    self.file().display(),
                    i + 1
                ),
                None => format!("no frame {n}: {} ends before it", self.file().display()),
            },
            TimeSpec::Time(t) => format!(
                "no frame at {} s: the clip runs from 0 to {} s (its last frame starts at {} s)",
                seconds(t),
                seconds(end),
                seconds(last.time)
            ),
        };
        about(failure(codes::OUT_OF_RANGE, message), self.file())
    }

    /// The error for a seek that damage stopped short of `when`: its
    /// cause's code (D27, the M1 plan's amendment 8), naming the last good
    /// frame's time.
    fn damaged(&self, when: TimeSpec, last_good_pts: i64, events: &[DecoderEvent]) -> Failure {
        // The damage the seek stopped at: the event that names the frame
        // it returned, else the last.
        let damage: Vec<(&str, DamageCause, Option<i64>)> = events
            .iter()
            .filter_map(|e| match e {
                DecoderEvent::Damaged {
                    reason,
                    last_good_pts,
                    cause,
                } => Some((reason.as_str(), *cause, *last_good_pts)),
                _ => None,
            })
            .collect();
        let (reason, cause) = damage
            .iter()
            .find(|d| d.2 == Some(last_good_pts))
            .or(damage.last())
            .map_or(("damaged data", DamageCause::Corrupt), |d| (d.0, d.1));
        let good = self.dec.info().clip_time(last_good_pts).map_or_else(
            |_| format!("pts {last_good_pts}"),
            |t| format!("{} s", seconds(t)),
        );
        about(
            failure(
                cause.code(),
                format!(
                    "{} is damaged before {}: {reason}; the last good frame is at {good}",
                    self.file().display(),
                    describe(when)
                ),
            ),
            self.file(),
        )
    }

    /// Damage that a counted frame `when` lies past, after `last`, the
    /// last frame counted: the error for it, naming the last good frame,
    /// as the damage says it or else `last`.
    fn damage_before(
        &self,
        events: &[DecoderEvent],
        when: TimeSpec,
        last: &VideoFrame,
    ) -> Option<Failure> {
        events.iter().find_map(|e| match e {
            DecoderEvent::Damaged {
                reason,
                cause,
                last_good_pts,
            } => {
                let good = last_good_pts
                    .and_then(|p| self.dec.info().clip_time(p).ok())
                    .unwrap_or(last.time);
                Some(about(
                    failure(
                        cause.code(),
                        format!(
                            "{} is damaged before {}: {reason}; the last good frame is at {} s",
                            self.file().display(),
                            describe(when),
                            seconds(good)
                        ),
                    ),
                    self.file(),
                ))
            }
            _ => None,
        })
    }

    /// Damage after the frame, which the run goes on past: warnings.
    fn damage_after(&self, events: &[DecoderEvent], warnings: &mut Vec<Warning>) {
        for e in events {
            if let DecoderEvent::Damaged { reason, cause, .. } = e {
                warn(
                    warnings,
                    cause.code(),
                    format!(
                        "{} is damaged after the frame: {reason}",
                        self.file().display()
                    ),
                );
            }
        }
    }

    /// Step 8: `decoded` through `views` (or flat, as `look.draw` says,
    /// when `views` goes unused), `size` pixels large, on the GPU (one
    /// [`StillRenderer`] a still, so that its pipelines are made once) or
    /// the CPU reference ([`reference::render_flat`] for flat, else
    /// through one view). Several views are a frame's over the shutter,
    /// averaged for motion blur (`frame --project`).
    pub fn render(
        &mut self,
        decoded: &Decoded,
        views: &[ViewParams],
        size: (u32, u32),
        look: Look,
    ) -> Result<Image, Failure> {
        let placement = match look.draw {
            Draw::View | Draw::Eye => self.placement,
            Draw::Coded => Placement::default(),
        };
        let opts = StillOptions {
            filter: look.filter,
            depth: look.depth,
            placement,
            flat: look.draw != Draw::View,
            ..StillOptions::default()
        };
        let frame = decoded.frame.as_ref();
        let Some(gpu) = self.gpu else {
            let cpu = reference::Options {
                level0: false,
                placement,
            };
            if look.draw != Draw::View {
                // The flat pass, as `StillOptions::flat` draws it: no view.
                let f64s = reference::render_flat(frame, size, &cpu).map_err(render_error)?;
                return Ok(quantize(&f64s, look.depth));
            }
            let [view] = views else {
                return Err(failure(
                    codes::INTERNAL,
                    "the CPU reference draws through one view, with no motion blur",
                ));
            };
            let f64s = reference::render(frame, view, size, &cpu).map_err(render_error)?;
            return Ok(quantize(&f64s, look.depth));
        };
        let renderer = self.renderer.get_or_insert_with(|| StillRenderer::new(gpu));
        let image = if self.opts.shared() {
            let copied = copy_out(gpu, &decoded.frame, &placement)?;
            renderer.render_copied(
                gpu,
                frame,
                &copied.buffer,
                &copied.planes,
                views,
                size,
                &opts,
            )
        } else if self.opts.zero_copy {
            let planes = import(gpu, &decoded.frame)?;
            renderer.render_imported(gpu, frame, &planes, views, size, &opts)
        } else {
            renderer.render_blurred(gpu, frame, views, size, &opts)
        };
        image.map_err(render_error)
    }
}

/// media's error as the run's, about `file`.
fn media_error(e: MediaError, file: &Path) -> Failure {
    about(failure(e.code(), e.to_string()), file)
}

/// The frame's clip time, with its index on a constant-rate stream.
fn shown(dec: &dyn VideoDecode, frame: &VideoFrame) -> FrameTime {
    let index = match dec.info().timing {
        Timing::Constant(rate) => rate
            .time_to_frame(frame.time, Rounding::Nearest)
            .ok()
            .and_then(|i| u64::try_from(i).ok()),
        Timing::Variable { .. } => None,
    };
    FrameTime {
        time: frame.time,
        frame: index,
    }
}

/// How the decoder is opened for `decoder`, on `node`, and the fallback
/// `auto` took without trying, when there is no device to try.
fn decode_mode(decoder: Decoder, node: &RenderNode) -> (DecodeMode, Option<FallbackInfo>) {
    let hardware = |kind, fallback| DecodeMode::Hardware {
        kind,
        device: None,
        fallback,
    };
    match decoder {
        Decoder::Software => (DecodeMode::Software, None),
        // VA-API on the GPU's node, then software (D39). Without a node
        // there is nothing to try.
        Decoder::Auto if !node.path.exists() => (
            DecodeMode::Software,
            Some(FallbackInfo {
                from: HwKind::VaApi.ffmpeg_name(),
                code: "hw_unavailable",
                reason: format!(
                    "no VA-API device: no render node at {}",
                    node.path.display()
                ),
            }),
        ),
        Decoder::Auto => (hardware(HwKind::VaApi, Fallback::Software), None),
        Decoder::Vaapi => (hardware(HwKind::VaApi, Fallback::Fail), None),
        Decoder::Vulkan => (hardware(HwKind::Vulkan, Fallback::Fail), None),
    }
}

/// `frame`'s planes imported into `gpu` (`--zero-copy`): `import_failed`
/// when the decoder didn't export it, or the GPU can't import it.
fn import<'f>(gpu: &Gpu, frame: &'f VideoFrame) -> Result<Imported<'f>, Failure> {
    let refused = |why: String| render_error(RenderError::Import(why));
    let layers = match (&frame.transfer, frame.dmabuf()) {
        (Transfer::DrmPrime, Some(layers)) => layers,
        (
            Transfer::Downloaded {
                not_exported: Some(why),
            },
            _,
        ) => return Err(refused(format!("VA-API didn't export the frame: {why}"))),
        (Transfer::System, _) => {
            return Err(refused(
                "the frame was decoded in software, not on VA-API".into(),
            ));
        }
        _ => {
            return Err(refused(
                "the decoder copied the frame from its device".into(),
            ));
        }
    };
    let whole = SlotShape::of(frame.layout(), frame.size()).map_err(render_error)?;
    let planes = dmabuf::plan(&whole, layers).map_err(refused)?;
    Importer::new()
        .import(gpu, frame, &planes)
        .map_err(render_error)
}

/// FFmpeg's Vulkan context on `gpu`'s device, shared with it
/// (`--zero-copy --decoder vulkan`): `import_failed` when FFmpeg refuses
/// it.
fn shared_device(gpu: &Gpu) -> Result<Arc<HwDevice>, Failure> {
    let video = gpu
        .shared_video()
        .map_err(|why| render_error(RenderError::Import(why)))?;
    HwDevice::vulkan_shared(&video.handles)
        .map(Arc::new)
        .map_err(|e| {
            render_error(RenderError::Import(format!(
                "FFmpeg can't decode on {}'s device: {e}",
                gpu.caps.info.name
            )))
        })
}

/// `frame`'s shown planes copied out of its Vulkan image on `gpu`
/// (`--zero-copy --decoder vulkan`), the copy done and ordered before
/// wgpu's next submit: `import_failed` when the decoder downloaded the
/// frame instead, or the copier refuses it.
fn copy_out(gpu: &Gpu, frame: &VideoFrame, placement: &Placement) -> Result<Copied, Failure> {
    let refused = |why: String| render_error(RenderError::Import(why));
    let picture = match (&frame.transfer, frame.vulkan()) {
        (Transfer::Vulkan, Some(p)) => p,
        (
            Transfer::Downloaded {
                not_exported: Some(why),
            },
            _,
        ) => {
            return Err(refused(format!(
                "the frame wasn't left on the Vulkan device: {why}"
            )));
        }
        _ => {
            return Err(refused(
                "the decoder copied the frame from its device".into(),
            ));
        }
    };
    let video = gpu.shared_video().map_err(refused)?;
    let regions = upload::shown_regions(frame, placement).map_err(render_error)?;
    let copied = video.copier.copy(picture, &regions).map_err(render_error)?;
    video.copier.wait(&copied).map_err(render_error)?;
    video.copier.order_before(&gpu.queue, &copied);
    Ok(copied)
}

/// How the frame left its decoder, as the JSON names it.
fn transfer_name(t: &Transfer) -> &'static str {
    match t {
        Transfer::System => "system",
        Transfer::Downloaded { .. } => "downloaded",
        Transfer::DrmPrime => "drm_prime",
        Transfer::Vulkan => "vulkan",
    }
}

/// Where the frame was decoded, and how it left the decoder, for the
/// JSON.
fn decode_info(
    path: &DecodePath,
    transfer: &Transfer,
    skipped: Option<FallbackInfo>,
) -> DecodeInfo {
    let transfer = transfer_name(transfer);
    match path {
        DecodePath::Software { .. } => DecodeInfo {
            path: "software",
            transfer,
            fallback: skipped,
        },
        DecodePath::Hardware { kind } => DecodeInfo {
            path: kind.ffmpeg_name(),
            transfer,
            fallback: None,
        },
        DecodePath::FellBack { from, reason } => DecodeInfo {
            path: "software",
            transfer,
            fallback: Some(FallbackInfo {
                from: from.ffmpeg_name(),
                code: match reason {
                    FallbackReason::Refused(_) => "hw_unsupported",
                    FallbackReason::DeviceUnavailable(_) => "hw_unavailable",
                    FallbackReason::Failed { .. } => "hw_failed",
                },
                reason: reason.to_string(),
            }),
        },
    }
}

// A pair of lenses (M3.5).

/// How a two-lens file is stitched for a still.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PairOptions {
    /// The geometry, whole, or `None` for the file's own: the table's
    /// entry as `camera::insta360::stitching` gives it, its generation the
    /// one field 136 names. A `Some` replaces that entry whole, so a caller
    /// that changes one field starts from [`pair_stitching`]'s entry,
    /// never from `known::lookup`'s row, which reads V6 and which
    /// `Rig::build` refuses on a trailer with only V3
    /// (`RigError::NoGeneration`).
    pub geometry: Option<Geometry>,
    pub tune: Tune,
    /// How the stitch renders: its debug mode (`RigOptions::debug`,
    /// `RigDebug`'s 4 to 7) and level 0 alone (`RigOptions::level0`). The
    /// GPU's stitch takes it through `RigOptions::mode`, which refuses
    /// level 0 together with a debug mode; the CPU reference takes both.
    pub rig: RigOptions,
}

/// Both lenses' frames of one time, by track, and what was asked for.
#[derive(Debug, Clone)]
pub struct DecodedPair {
    /// Track t's decoded frame, whose planes ([`VideoPlanes`]) a test
    /// hashes.
    pub frames: [Arc<VideoFrame>; 2],
    pub requested: FrameTime,
    /// The time both frames show.
    pub shown: FrameTime,
}

/// A two-lens file open for stills: each track's decoder and the rig its
/// calibration and geometry build. Each [`seek`](Self::seek) decodes
/// both lenses' frames of one time, which [`render`](Self::render)
/// stitches through a view.
pub struct StillPair<'g> {
    /// The path messages name the file by.
    name: PathBuf,
    /// By track: each a still of its own track, with the caller's GPU and
    /// decoder as [`Still::open`] takes them, so that a lens decodes,
    /// seeks, counts and fails as a one-stream file does.
    lenses: [Still<'g>; 2],
    /// What the rig was built of: `PairOptions`' geometry, or the file's.
    geometry: Geometry,
    rig: Rig,
    options: RigOptions,
    /// The GPU's renderer, made at the first render on a GPU, so that
    /// its pipelines are made once.
    renderer: Option<StillRenderer>,
    /// With no GPU, the last pair of frames made ready to sample, which
    /// another view of the same frames samples again.
    ready: Option<([Arc<VideoFrame>; 2], reference::LensPair)>,
}

impl std::fmt::Debug for StillPair<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StillPair")
            .field("name", &self.name)
            .field("streams", &self.lenses.each_ref().map(|l| l.stream.index))
            .field("geometry", &self.geometry)
            .finish_non_exhaustive()
    }
}

/// The stitch `src`'s trailer gives, read through the confinement as
/// probe reads it (D24): its metadata (record 1) through
/// `camera::insta360::stitching`, the calibration and the table's entry
/// for the camera, with the entry's generation the one field 136 names.
/// A caller that changes one field of the geometry starts from this
/// entry's ([`PairOptions::geometry`]).
///
/// Each failure is `not_equirect`, the code frame gives a file it can't
/// render as a stitched picture, since probe's `bad_trailer` is a
/// warning, never an error: a file with no calibration to stitch by (no
/// trailer, no metadata, or neither a V3 nor a V6 string) says "no
/// calibration", and a trailer, record or string that can't be read says
/// "can't be stitched" with probe's words and camera's, which name the
/// part at fault.
///
/// It calls [`spherecut_core::may_block`] first, before it opens the
/// file, so on the UI thread it panics in a debug build.
pub fn pair_stitching(src: Source<'_>) -> Result<Stitching, Failure> {
    /// The trailer's metadata record.
    const METADATA: u8 = 1;
    spherecut_core::may_block("reading an Insta360 file's trailer");
    let file = src.name.display();
    let refuse = |code, message: String| about(failure(code, message), src.name);
    let none = |why: &str| {
        refuse(
            codes::NOT_EQUIRECT,
            format!("{file} has no calibration to stitch by: {why}"),
        )
    };
    let bad = |why: String| {
        refuse(
            codes::NOT_EQUIRECT,
            format!("{file} can't be stitched: {why}"),
        )
    };
    let io = |error| {
        media_error(
            MediaError::Io {
                path: src.name.to_path_buf(),
                error,
            },
            src.name,
        )
    };
    let mut f = File::open(src.open).map_err(io)?;
    let len = f.metadata().map_err(io)?.len();
    let trailer = match read_trailer(&mut f, len) {
        Ok(Some(t)) => t,
        Ok(None) => return Err(none("it has no Insta360 trailer")),
        Err(e) => {
            return Err(bad(format!("the Insta360 trailer can't be read: {e}")));
        }
    };
    let metadata = match read_record(&mut f, &trailer, METADATA, MAX_METADATA) {
        Ok(Some(m)) => m,
        Ok(None) => return Err(none("its Insta360 trailer has no metadata (record 1)")),
        Err(e) => {
            return Err(bad(format!(
                "the Insta360 trailer's record {METADATA} can't be read: {e}"
            )));
        }
    };
    let metadata = parse_metadata(&metadata).map_err(|e| {
        bad(format!(
            "the Insta360 trailer's metadata (record {METADATA}): {e}"
        ))
    })?;
    match stitching(&metadata) {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(none(
            "its Insta360 trailer holds neither a V3 nor a V6 calibration string",
        )),
        Err(e) => Err(bad(format!(
            "the Insta360 trailer's calibration can't be read: {e}"
        ))),
    }
}

/// `e`, a lens's failure, saying which track it is about.
fn on_track(track: usize, stream: usize, mut e: Failure) -> Failure {
    if e.code != codes::CANCELLED {
        e.message = format!("track {track} (stream {stream}): {}", e.message);
    }
    e
}

impl<'g> Still<'g> {
    /// Steps 4 and 6 for a file of two lenses: `report`'s two lens tracks
    /// of `src` (`lenses.tracks`), each with its own decoder, opened on
    /// `gpu` with `opts`' decoder, render node and zero copy as
    /// [`Still::open`] opens one stream (on path B, `--zero-copy
    /// --decoder vulkan`, both on one FFmpeg device sharing the
    /// renderer's), and the rig `sphere::rig::Rig::build` makes of the
    /// file's calibration ([`pair_stitching`]), `pair`'s geometry or the
    /// file's own, and `pair`'s tune, with track 0 in front and each
    /// track's coded side. With `gpu` `None` the pair renders on the CPU
    /// reference.
    ///
    /// Each fails with a code `docs/cli-json.md` lists:
    /// `not_equirect` for a file that isn't a camera's pair of lenses,
    /// one with no calibration ("no calibration"), one whose trailer
    /// can't be read ("can't be stitched", as [`pair_stitching`] says,
    /// before "isn't a camera's pair" for an Insta360 file whose trailer
    /// probe couldn't read for its lens tracks),
    /// one whose camera the table says isn't stitchable, and a rig that
    /// can't be built ("build no rig", with sphere's `RigError`); and a
    /// lens's own failure to open, naming its track. The tables' open
    /// checks are the player's (M3.8's lock), not this one's: a pair whose
    /// lenses don't show one time fails at [`StillPair::seek`].
    ///
    /// Once those checks pass, before the lenses open, `warnings` gains
    /// probe's damage ([`damage_warnings`], as [`probed`] gives it) and,
    /// for a model the table has no entry for, `geometry_assumed` with
    /// the table's notice ("geometry assumed from the X5").
    pub fn open_pair(
        src: Source<'_>,
        report: &ProbeReport,
        gpu: Option<&'g Gpu>,
        opts: &Options,
        pair: &PairOptions,
        stop: &Stop,
        warnings: &mut Vec<Warning>,
    ) -> Result<StillPair<'g>, Failure> {
        let file = src.name.display();
        let refuse = |code, message: String| about(failure(code, message), src.name);
        let Some(lenses) = &report.camera_file.lenses else {
            // An Insta360 file whose trailer probe couldn't read for its
            // tracks: the trailer's own failure, naming the part at fault.
            let insta360 = report
                .camera
                .as_ref()
                .is_some_and(|c| c.layout == Layout::Insta360DualFisheye);
            if insta360 {
                pair_stitching(src)?;
            }
            return Err(refuse(
                codes::NOT_EQUIRECT,
                format!(
                    "{file} isn't a camera's pair of lenses: probe finds no Insta360 dual \
                     fisheye's two lens tracks in it"
                ),
            ));
        };
        let mut streams = Vec::with_capacity(2);
        for index in lenses.tracks {
            match report.streams.iter().find(|s| s.index == index) {
                Some(s) => streams.push(s),
                None => {
                    return Err(refuse(
                        codes::INTERNAL,
                        format!("probe's report of {file} names lens stream {index} but has none"),
                    ));
                }
            }
        }
        let stitched = pair_stitching(src)?;
        if let Some(why) = stitched.known.refusal() {
            return Err(refuse(
                codes::NOT_EQUIRECT,
                format!("{file} can't be stitched: {why}"),
            ));
        }
        damage_warnings(report, warnings);
        if let Some(notice) = stitched.known.notice {
            warn(
                warnings,
                codes::warnings::GEOMETRY_ASSUMED,
                format!("{file}: {notice} ({})", stitched.known.geometry.evidence),
            );
        }
        stop.go_on()?;
        // Each lens decodes as a one-stream file does under `opts`; on
        // path B both share one FFmpeg device on the renderer's.
        let lens_opts = Options {
            equirect: false,
            stream: None,
            ..opts.clone()
        };
        let device = match (opts.shared(), gpu) {
            (true, Some(gpu)) => Some(shared_device(gpu)?),
            _ => None,
        };
        let mut open = |track: usize| {
            let stream = streams[track];
            Still::open_stream_on(src, stream, gpu, &lens_opts, device.clone(), stop, warnings)
                .map_err(|e| on_track(track, stream.index, e))
        };
        let lenses = [open(0)?, open(1)?];
        let geometry = pair
            .geometry
            .clone()
            .unwrap_or_else(|| stitched.known.geometry.clone());
        let sides = lenses.each_ref().map(|l| l.info().size.0);
        // Track 0 in front, the default, until the plan's question 3
        // settles which lens is the front one (6.2, 11.1).
        let rig =
            Rig::build(&stitched.calibration, &geometry, &pair.tune, 0, sides).map_err(|e| {
                refuse(
                    codes::NOT_EQUIRECT,
                    format!("{file}'s calibration, geometry and tune build no rig: {e}"),
                )
            })?;
        Ok(StillPair {
            name: src.name.to_path_buf(),
            lenses,
            geometry,
            rig,
            options: pair.rig,
            renderer: None,
            ready: None,
        })
    }
}

impl<'g> StillPair<'g> {
    /// The rig the pair is stitched by.
    pub fn rig(&self) -> &Rig {
        &self.rig
    }

    /// The geometry the rig was built under: [`PairOptions::geometry`],
    /// or the file's own entry.
    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// Track `track`'s still, for its stream and decoder's report.
    ///
    /// # Panics
    ///
    /// For a track other than 0 or 1.
    pub fn lens(&self, track: usize) -> &Still<'g> {
        &self.lenses[track]
    }

    /// Step 7 for both lenses: the frame `when` names on each track,
    /// exactly, both at one time, or with [`SeekMode::Keyframe`] the
    /// keyframe a seek on track 0 lands on and track 1's frame at its
    /// time. Each track seeks in its own clip time, from its own first
    /// frame, and fails as [`Still::seek`] does, naming the track; then
    /// the two frames' timestamps, as times in the file, must be equal,
    /// or the pair is `damaged` and the message names both. A track that
    /// starts late shows its frames late by as much, and is never paired
    /// with the nearer frame.
    pub fn seek(
        &mut self,
        when: TimeSpec,
        mode: SeekMode,
        warnings: &mut Vec<Warning>,
    ) -> Result<DecodedPair, Failure> {
        let streams = self.lenses.each_ref().map(|l| l.stream.index);
        let first = self.lenses[0]
            .seek(when, mode, warnings)
            .map_err(|e| on_track(0, streams[0], e))?;
        let then = match mode {
            SeekMode::Exact => when,
            _ => TimeSpec::Time(first.frame.time),
        };
        let second = self.lenses[1]
            .seek(then, SeekMode::Exact, warnings)
            .map_err(|e| on_track(1, streams[1], e))?;
        let frames = [first.frame, second.frame];
        let at = |track: usize| {
            let tb = self.lenses[track].info().time_base;
            Time::from_pts(frames[track].pts, tb).ok()
        };
        let times = [at(0), at(1)];
        if times[0].is_none() || times[0] != times[1] {
            let said = |track: usize| {
                let time = times[track].map_or_else(
                    || "a time out of range".into(),
                    |t| format!("{} s", seconds(t)),
                );
                format!(
                    "track {track} (stream {}) at pts {}, {time}",
                    streams[track], frames[track].pts
                )
            };
            return Err(about(
                failure(
                    DamageCause::Corrupt.code(),
                    format!(
                        "{}'s lenses don't show one time at {}: {} and {}",
                        self.name.display(),
                        describe(when),
                        said(0),
                        said(1)
                    ),
                ),
                &self.name,
            ));
        }
        Ok(DecodedPair {
            frames,
            requested: first.requested,
            shown: first.shown,
        })
    }

    /// Step 8: `decoded` stitched through `views`, `size` pixels large,
    /// with unit gains and the pair's `RigOptions` (its debug mode reaches
    /// the GPU through [`RigOptions::mode`], which refuses level 0 with a
    /// debug mode).
    ///
    /// On the GPU (the lenses' own, one [`StillRenderer`] a pair) it is
    /// `render_blurred` of a [`SourceFrames::Pair`], the lenses by track,
    /// each uploaded as [`Still::render`] uploads one frame: copied out of
    /// FFmpeg's Vulkan images on path B, imported as DMA-BUFs with zero
    /// copy (both held until the render is done), else from the frames'
    /// bytes. Several views are a frame's over the shutter, averaged for
    /// motion blur as for one frame. The weights and the seam difference
    /// (`RigDebug`'s modes 6 and 7) are read back linear and coded with no
    /// OETF, as the CPU reference writes them.
    ///
    /// With no GPU it is the CPU reference: the two frames made ready
    /// once (`render::reference::LensPair`, kept for the next view of the
    /// same frames), then `render_rig_of`, quantized as [`Still::render`]
    /// quantizes. The CPU reference draws only through one view: several
    /// are `internal`. A draw other than [`Draw::View`] is `internal`
    /// either way, since a pair has no flat form.
    pub fn render(
        &mut self,
        decoded: &DecodedPair,
        views: &[ViewParams],
        size: (u32, u32),
        look: Look,
    ) -> Result<Image, Failure> {
        if look.draw != Draw::View {
            return Err(failure(
                codes::INTERNAL,
                "a pair of lenses is stitched only through a view: it has no flat form",
            ));
        }
        if let Some(gpu) = self.lenses[0].gpu {
            return self.render_on(gpu, decoded, views, size, look);
        }
        let [view] = views else {
            return Err(failure(
                codes::INTERNAL,
                "the CPU reference stitches a pair through one view, with no motion blur",
            ));
        };
        let same = |(frames, _): &([Arc<VideoFrame>; 2], reference::LensPair)| {
            Arc::ptr_eq(&frames[0], &decoded.frames[0])
                && Arc::ptr_eq(&frames[1], &decoded.frames[1])
        };
        if !self.ready.as_ref().is_some_and(same) {
            // The last pair goes before the next is made, so that two
            // pairs' mip chains are never held at once.
            self.ready = None;
            let lenses = decoded
                .frames
                .each_ref()
                .map(|f| f.as_ref() as &dyn VideoPlanes);
            let pair = reference::LensPair::new(lenses).map_err(render_error)?;
            self.ready = Some((decoded.frames.clone(), pair));
        }
        let Some((_, pair)) = &self.ready else {
            return Err(failure(codes::INTERNAL, "no pair made ready"));
        };
        let f64s = reference::render_rig_of(pair, &self.rig, UNIT_GAINS, view, size, &self.options)
            .map_err(render_error)?;
        Ok(quantize(&f64s, look.depth))
    }

    /// [`StillPair::render`] on `gpu`.
    fn render_on(
        &mut self,
        gpu: &Gpu,
        decoded: &DecodedPair,
        views: &[ViewParams],
        size: (u32, u32),
        look: Look,
    ) -> Result<Image, Failure> {
        let debug = self.options.mode().map_err(render_error)?;
        // The weights and the seam difference are values, not light: read
        // back as the linear target holds them and coded here with no
        // OETF, as the CPU reference writes them and `quantize` codes them.
        let values = matches!(debug, DebugMode::Weights | DebugMode::SeamDiff);
        let opts = StillOptions {
            filter: look.filter,
            depth: if values { Depth::Linear } else { look.depth },
            debug,
            ..StillOptions::default()
        };
        let frames = &decoded.frames;
        let lens_opts = &self.lenses[0].opts;
        let renderer = self.renderer.get_or_insert_with(|| StillRenderer::new(gpu));
        let mut render = |from: [PlaneSource<'_>; 2]| {
            let lenses = [0, 1].map(|t| LensFrame {
                frame: frames[t].as_ref(),
                from: from[t],
            });
            let source = SourceFrames::Pair {
                lenses,
                rig: &self.rig,
                gains: UNIT_GAINS,
            };
            renderer.render_blurred(gpu, source, views, size, &opts)
        };
        let image = if lens_opts.shared() {
            // Each lens's own buffer, both live until the render is done.
            let copied = [
                copy_out(gpu, &frames[0], &Placement::FULL)?,
                copy_out(gpu, &frames[1], &Placement::FULL)?,
            ];
            render(
                copied
                    .each_ref()
                    .map(|c| PlaneSource::Buffer(&c.buffer, &c.planes)),
            )
        } else if lens_opts.zero_copy {
            let imported = [import(gpu, &frames[0])?, import(gpu, &frames[1])?];
            render(imported.each_ref().map(|i| PlaneSource::Imported(i)))
        } else {
            render([PlaneSource::Bytes; 2])
        };
        let image = image.map_err(render_error)?;
        Ok(match (values, image.pixels) {
            (true, Pixels::LinearF32(v)) => Image {
                width: image.width,
                height: image.height,
                pixels: codes(v.into_iter().map(f64::from), look.depth),
            },
            (_, pixels) => Image { pixels, ..image },
        })
    }
}

/// The gains a still stitches a pair with: unity in each block, until a
/// project's `Gains::at` reaches it (M3.11).
const UNIT_GAINS: [[f64; 3]; 2] = [[1.0; 3]; 2];

/// The ids hardware decoding finds the adapter's render node by.
fn gpu_ids(info: &wgpu::AdapterInfo) -> GpuIds {
    GpuIds {
        pci_bus_id: info.device_pci_bus_id.clone(),
        vendor: info.vendor,
        device: info.device,
    }
}

pub fn gpu_info(info: &wgpu::AdapterInfo) -> GpuInfo {
    GpuInfo {
        name: info.name.clone(),
        driver: info.driver.clone(),
        driver_info: info.driver_info.clone(),
        device_type: match info.device_type {
            wgpu::DeviceType::DiscreteGpu => "discrete_gpu",
            wgpu::DeviceType::IntegratedGpu => "integrated_gpu",
            wgpu::DeviceType::VirtualGpu => "virtual_gpu",
            wgpu::DeviceType::Cpu => "cpu",
            wgpu::DeviceType::Other => "other",
        },
    }
}

pub fn render_error(e: RenderError) -> Failure {
    failure(e.code(), e.to_string())
}

/// The reference's picture at `depth`'s codes, rounded to nearest as the
/// GPU stores them: what [`Still::render`] and [`StillPair::render`] make
/// of the CPU reference's picture, for a test that renders its own.
pub fn quantize(img: &reference::ImageF64, depth: Depth) -> Image {
    Image {
        width: img.width,
        height: img.height,
        pixels: codes(img.data.iter().copied(), depth),
    }
}

/// `values`, each clamped to 0 to 1, as `depth`'s codes rounded to
/// nearest: 16-bit for [`Depth::Sixteen`], else 8-bit.
fn codes(values: impl Iterator<Item = f64>, depth: Depth) -> Pixels {
    let code = |v: f64, max: f64| (v.clamp(0.0, 1.0) * max).round();
    match depth {
        Depth::Sixteen => Pixels::Rgb16(values.map(|v| code(v, 65535.0) as u16).collect()),
        _ => Pixels::Rgb8(values.map(|v| code(v, 255.0) as u8).collect()),
    }
}

/// `--time` or `--frame` as the messages name it.
fn describe(when: TimeSpec) -> String {
    match when {
        TimeSpec::Time(t) => format!("{} s", seconds(t)),
        TimeSpec::Frame(n) => format!("frame {n}"),
    }
}

/// Seconds for people: to the millisecond, without trailing zeros.
pub fn seconds(t: Time) -> String {
    number(t.to_secs_f64())
}

/// A number for people: to three decimals, without trailing zeros.
pub fn number(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}
