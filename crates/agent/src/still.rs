//! One frame of a video through a virtual camera: the core of `spherecut
//! frame` (M1 plan section 5, D27), which `look` renders its tiles with
//! (a1-plan section 2). The steps are frame's 4 to 8:
//!
//! 4. [`probe`]: the file probed through the confinement (D24), its video
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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
use spherecut_render::dmabuf::{self, Importer};
use spherecut_render::vkshare::Copied;
use spherecut_render::{
    AdapterChoice, Depth, Filter, Gpu, Image, Pixels, Placement, RenderError, SlotShape, Stereo,
    StillOptions, StillRenderer, ViewParams, reference, upload, wgpu,
};

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
    let file = src.name;
    let report = match probe::probe_cancellable(src, stop.flag.clone()) {
        Ok(r) => r,
        Err(e) => {
            let mut f = about(failure(e.code(), e.to_string()), file);
            f.ffmpeg_log = e.ffmpeg_log;
            return Err(f);
        }
    };
    let stream = choose_stream(&report, opts.stream, file)?.clone();
    if !opts.equirect {
        check_equirect(&report, &stream)?;
    }
    for d in &report.damage {
        warn(
            warnings,
            d.code,
            format!("{} is damaged: {}", file.display(), d.message),
        );
    }
    stop.go_on()?;
    Ok(Probed { report, stream })
}

/// `stream`, which must be a video stream, else the first video stream
/// that isn't a cover picture. A `stream` the file hasn't is a usage
/// error, which says what the file has; the caller names its flag.
fn choose_stream<'r>(
    report: &'r ProbeReport,
    stream: Option<usize>,
    file: &Path,
) -> Result<&'r StreamInfo, Failure> {
    let is_video = |s: &&StreamInfo| {
        s.kind == StreamKind::Video && !s.disposition.iter().any(|d| d == "attached_pic")
    };
    let videos: Vec<&StreamInfo> = report.streams.iter().filter(is_video).collect();
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

/// Whether the file is a stitched equirect: tagged so (Spherical Video
/// V2's `equi`, or V1's `equirectangular`, in an MP4 or MOV file), or
/// else `not_equirect`, naming what it is, or that its Matroska spherical
/// metadata isn't read. The message says what the file is, and the
/// caller how to render it anyway.
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
    // probe reads spherical metadata from MP4 boxes only.
    if report.container.format.starts_with("matroska") {
        return Err(refuse(format!(
            "a {size} Matroska/WebM file, and Spherecut doesn't read Matroska's spherical \
             metadata yet (Spherical Video V2's Projection element), so it can't tell whether \
             it is an equirect"
        )));
    }
    match report.camera.as_ref().map(|c| c.layout) {
        Some(Layout::Insta360DualFisheye) => Err(refuse(
            "Insta360 dual fisheye: stitching arrives in M3".into(),
        )),
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
        let ids = gpu
            .filter(|g| !g.caps.is_cpu)
            .map(|g| gpu_ids(&g.caps.info));
        let node = hw::render_node(ids.as_ref(), opts.render_node.as_deref());
        let (mut decode, skipped) = decode_mode(opts.decoder, &node);
        let shared = opts.shared();
        if let (true, Some(gpu)) = (shared, gpu) {
            decode = DecodeMode::Hardware {
                kind: HwKind::Vulkan,
                device: Some(shared_device(gpu)?),
                fallback: Fallback::Fail,
            };
        }
        let backend = FfmpegBackend::on_node(node);
        let req = VideoRequest {
            stream: Some(probed.stream.index),
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
            stream: probed.stream.clone(),
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
        decode_info(
            &decoded.frame.decoded_on,
            &decoded.frame.transfer,
            self.skipped.clone(),
        )
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

    /// Step 8: `decoded` through `view` (or flat, as `look.draw` says),
    /// `size` pixels large, on the GPU (one [`StillRenderer`] a still, so
    /// that its pipelines are made once) or the CPU reference, which
    /// draws only through a view.
    pub fn render(
        &mut self,
        decoded: &Decoded,
        view: &ViewParams,
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
            if look.draw != Draw::View {
                return Err(failure(
                    codes::INTERNAL,
                    "the CPU reference draws only through a view",
                ));
            }
            let f64s = reference::render(
                frame,
                view,
                size,
                &reference::Options {
                    level0: false,
                    placement,
                },
            )
            .map_err(render_error)?;
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
                view,
                size,
                &opts,
            )
        } else if self.opts.zero_copy {
            let planes = import(gpu, &decoded.frame)?;
            renderer.render_imported(gpu, frame, &planes, view, size, &opts)
        } else {
            renderer.render(gpu, frame, view, size, &opts)
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
fn import(gpu: &Gpu, frame: &VideoFrame) -> Result<Vec<wgpu::Texture>, Failure> {
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
    Importer::new().import(gpu, &planes).map_err(render_error)
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
/// GPU stores them.
fn quantize(img: &reference::ImageF64, depth: Depth) -> Image {
    let code = |v: f64, max: f64| (v.clamp(0.0, 1.0) * max).round();
    let pixels = match depth {
        Depth::Sixteen => {
            Pixels::Rgb16(img.data.iter().map(|&v| code(v, 65535.0) as u16).collect())
        }
        _ => Pixels::Rgb8(img.data.iter().map(|&v| code(v, 255.0) as u8).collect()),
    };
    Image {
        width: img.width,
        height: img.height,
        pixels,
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
