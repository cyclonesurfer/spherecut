//! `spherecut frame`: one frame of a stitched equirect, rendered through a
//! virtual camera into a PNG (M1 plan section 5, DECISIONS D27).
//!
//! The steps, in order:
//!
//! 1. the command line, which clap parses and [`FrameArgs::check`] checks
//!    further (exit 2);
//! 2. the output, before any work: something already there is refused
//!    unless `--overwrite` is given, and a directory, pipe or device
//!    either way ([`check_output`]);
//! 3. the SIGINT handler (`crate::interrupt`), then the temporary file;
//!    the work runs on a worker thread while this one waits for it, and
//!    the first Ctrl+C stops it (exit 4, no output);
//! 4. probe, through the confinement (D24), and the layout: only a
//!    stitched equirect renders, unless `--equirect` says it is one;
//! 5. the GPU, unless `--reference` renders on the CPU (exit 3 without
//!    one);
//! 6. the decoder, on the render node of the GPU that renders (D39 has the
//!    order `auto` tries);
//! 7. the seek, which must be exact;
//! 8. the render;
//! 9. the PNG, written next to the output and published only if nothing
//!    has appeared there meanwhile (D16), or over it with `--overwrite`;
//! 10. the report: one line on stdout, or the `--json` document.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;
use clap::{Args, ValueEnum};
use spherecut_cli::contract::{
    self, DecodeInfo, Envelope, ErrorBody, Exit, FallbackInfo, FrameResult, FrameShown, GpuInfo,
    Size, SourceInfo, ViewInfo, Warning, codes,
};
use spherecut_core::cancel::{StopSource, StopToken};
use spherecut_core::fs::{TempFile, follow_links, replaceable};
use spherecut_core::video::VideoPlanes;
use spherecut_core::{FrameTime, Rounding, Time, TimeSpec};
use spherecut_media::backend::{
    DamageCause, DecodeMode, DecodePath, DecoderEvent, Exactness, Fallback, FallbackReason,
    FfmpegBackend, HwKind, MediaBackend, NextFrame, SeekMode, SeekOutcome, Timing, VideoDecode,
    VideoFrame, VideoRequest, VideoStreamInfo,
};
use spherecut_media::detect::Layout;
use spherecut_media::hw::{self, GpuIds, RenderNode};
use spherecut_media::probe::{self, ProbeReport, StreamInfo, StreamKind};
use spherecut_media::seek::{FrameAddress, frame_address};
use spherecut_media::spherical::{Projection as SvProjection, StereoMode};
use spherecut_render::{
    AdapterChoice, Depth, Filter, Gpu, Image, Pixels, Placement, RenderError, Stereo, StillOptions,
    ViewParams, reference, render_still, wgpu,
};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};

use crate::{emit, interrupt, print_json, to_stderr};

/// How often the main thread looks for a Ctrl+C while the worker works.
const POLL: Duration = Duration::from_millis(10);

/// The smallest and largest side of the picture `--size` asks for.
const SIDES: std::ops::RangeInclusive<u32> = 16..=16384;

#[derive(Args, Debug, Clone)]
pub struct FrameArgs {
    /// The video: a stitched equirect, tagged with spherical metadata or
    /// treated as one with --equirect.
    pub file: PathBuf,
    /// The PNG to write. Something already there is refused unless
    /// --overwrite is given.
    #[arg(short, long, value_name = "OUT.png")]
    pub output: PathBuf,
    /// When, in clip time from the first frame: 12, 12.5, 1:02.5,
    /// 01:02:03.250, 30000/1001, or a frame number such as 370f [default:
    /// 0]
    // Hyphens too, so that a negative time gets its own message.
    #[arg(long, value_name = "T", value_parser = time_spec, conflicts_with = "frame", allow_hyphen_values = true)]
    pub time: Option<TimeSpec>,
    /// Frame N, counting the first frame shown as 0 (the same as --time Nf).
    #[arg(long, value_name = "N", value_parser = frame_number, allow_negative_numbers = true)]
    pub frame: Option<u64>,
    /// Degrees to the right.
    #[arg(long, value_name = "DEG", default_value_t = 0.0, value_parser = angle, allow_negative_numbers = true)]
    pub yaw: f64,
    /// Degrees up.
    #[arg(long, value_name = "DEG", default_value_t = 0.0, value_parser = angle, allow_negative_numbers = true)]
    pub pitch: f64,
    /// Degrees clockwise, as seen from behind the camera.
    #[arg(long, value_name = "DEG", default_value_t = 0.0, value_parser = angle, allow_negative_numbers = true)]
    pub roll: f64,
    /// The horizontal field of view in degrees: at most 170 rectilinear,
    /// 360 fisheye and 340 stereographic.
    #[arg(long, value_name = "DEG", default_value_t = 100.0, value_parser = angle, allow_negative_numbers = true)]
    pub fov: f64,
    #[arg(long, value_enum, default_value_t = ProjectionArg::Rectilinear)]
    pub projection: ProjectionArg,
    /// A projection to morph towards, by --morph.
    #[arg(long, value_enum, value_name = "PROJECTION", requires = "morph")]
    pub morph_to: Option<ProjectionArg>,
    /// How far towards --morph-to: 0 is --projection, 1 is --morph-to.
    #[arg(long, value_name = "0..1", value_parser = unit, requires = "morph_to")]
    pub morph: Option<f64>,
    /// The picture's width and height, each 16 to 16384 and within the
    /// GPU's limit.
    #[arg(long, value_name = "WxH", default_value = "1920x1080", value_parser = size)]
    pub size: (u32, u32),
    /// Bits per channel of the PNG.
    #[arg(long, value_enum, default_value_t = DepthArg::Eight)]
    pub depth: DepthArg,
    /// iso is the same on every driver; aniso is sharper where the view
    /// squeezes the source, and the driver's own.
    #[arg(long, value_enum, default_value_t = FilterArg::Iso)]
    pub filter: FilterArg,
    /// auto tries VA-API on the GPU's render node and falls back to
    /// software; vaapi and vulkan fail instead (exit 3).
    #[arg(long, value_enum, default_value_t = DecoderArg::Auto)]
    pub decoder: DecoderArg,
    /// The DRM render node to decode on [default: the rendering GPU's,
    /// else /dev/dri/renderD128].
    #[arg(long, value_name = "PATH")]
    pub render_node: Option<PathBuf>,
    /// The Vulkan adapter to render on: auto (a GPU, else lavapipe), cpu
    /// (lavapipe), or part of an adapter's name or its PCI bus id
    /// [default: $SPHERECUT_ADAPTER, else auto].
    #[arg(long, value_name = "auto|cpu|NAME", conflicts_with = "reference")]
    pub adapter: Option<String>,
    /// Treat the file as a stitched equirect whatever its metadata says.
    #[arg(long)]
    pub equirect: bool,
    /// The video stream, by FFmpeg's index [default: the first].
    #[arg(long, value_name = "N")]
    pub stream: Option<usize>,
    /// Render with the CPU reference (f64, slow) instead of the GPU.
    #[arg(long)]
    pub reference: bool,
    /// Replace what is at the output, if it is a regular file.
    #[arg(long)]
    pub overwrite: bool,
    /// Print one JSON document instead of text (docs/cli-json.md).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProjectionArg {
    Rectilinear,
    Fisheye,
    Stereographic,
}

impl ProjectionArg {
    fn projection(self) -> Projection {
        match self {
            ProjectionArg::Rectilinear => Projection::Rectilinear,
            ProjectionArg::Fisheye => Projection::Fisheye,
            ProjectionArg::Stereographic => Projection::Stereographic,
        }
    }

    fn name(self) -> &'static str {
        match self {
            ProjectionArg::Rectilinear => "rectilinear",
            ProjectionArg::Fisheye => "fisheye",
            ProjectionArg::Stereographic => "stereographic",
        }
    }

    /// The widest `--fov`, in degrees.
    fn max_fov(self) -> f64 {
        self.projection().max_hfov().to_degrees().round()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DepthArg {
    #[value(name = "8")]
    Eight,
    #[value(name = "16")]
    Sixteen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FilterArg {
    Iso,
    Aniso,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DecoderArg {
    Auto,
    Software,
    Vaapi,
    Vulkan,
}

fn time_spec(s: &str) -> Result<TimeSpec, String> {
    s.parse()
        .map_err(|e: spherecut_core::TimeSpecError| e.to_string())
}

fn frame_number(s: &str) -> Result<u64, String> {
    match s.parse::<u64>() {
        Ok(n) => Ok(n),
        Err(_)
            if s.strip_prefix('-')
                .is_some_and(|n| n.parse::<u64>().is_ok()) =>
        {
            Err("a frame number can't be negative: the first frame is 0".into())
        }
        Err(_) => Err("expected a frame number, counting the first frame as 0".into()),
    }
}

fn angle(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err("expected a number of degrees".into()),
    }
}

fn unit(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if (0.0..=1.0).contains(&v) => Ok(v),
        _ => Err("expected a number from 0 to 1".into()),
    }
}

fn size(s: &str) -> Result<(u32, u32), String> {
    let syntax = || "expected a width and a height, such as 1920x1080".to_string();
    let (w, h) = s.split_once(['x', 'X']).ok_or_else(syntax)?;
    let side = |v: &str| v.parse::<u32>().map_err(|_| syntax());
    let (w, h) = (side(w)?, side(h)?);
    if !SIDES.contains(&w) || !SIDES.contains(&h) {
        return Err(format!(
            "each side is {} to {} pixels",
            SIDES.start(),
            SIDES.end()
        ));
    }
    Ok((w, h))
}

impl FrameArgs {
    /// What clap can't check alone: the field of view the projection
    /// allows, and the filter the CPU reference has. The message is a
    /// usage error's.
    pub fn check(&self) -> Result<(), String> {
        let max = self.projection.max_fov();
        if self.fov <= 0.0 || self.fov > max {
            return Err(format!(
                "--fov {}: {} takes more than 0 and at most {max} degrees",
                number(self.fov),
                self.projection.name()
            ));
        }
        if self.reference && self.filter == FilterArg::Aniso {
            return Err("--reference renders with --filter iso only".into());
        }
        Ok(())
    }

    /// What `--time` or `--frame` asks for.
    fn when(&self) -> TimeSpec {
        match (self.frame, self.time) {
            (Some(n), _) => TimeSpec::Frame(n),
            (None, Some(t)) => t,
            (None, None) => TimeSpec::default(),
        }
    }

    fn lens(&self) -> Lens {
        let from = self.projection.projection();
        Lens {
            from,
            to: self.morph_to.map_or(from, ProjectionArg::projection),
            morph: self.morph.unwrap_or(0.0),
            hfov: self.fov.to_radians(),
        }
    }

    fn view(&self) -> ViewParams {
        ViewParams::new(
            Ypr::from_degrees(self.yaw, self.pitch, self.roll).to_quat(),
            self.lens(),
        )
    }

    fn view_info(&self) -> ViewInfo {
        ViewInfo {
            yaw: self.yaw,
            pitch: self.pitch,
            roll: self.roll,
            fov: self.fov,
            projection: self.projection.name(),
            morph_to: self.morph_to.map(ProjectionArg::name),
            morph: self.morph.unwrap_or(0.0),
        }
    }

    fn depth(&self) -> Depth {
        match self.depth {
            DepthArg::Eight => Depth::Eight,
            DepthArg::Sixteen => Depth::Sixteen,
        }
    }

    fn adapter(&self) -> AdapterChoice {
        match &self.adapter {
            Some(a) => AdapterChoice::parse(a),
            None => AdapterChoice::from_env().unwrap_or(AdapterChoice::Auto),
        }
    }
}

/// A failed step: the error the run ends with.
type Failure = ErrorBody;

fn failure(code: &'static str, message: impl Into<String>) -> Failure {
    ErrorBody::new(code, message)
}

/// `e` as the run's error, about `path`.
fn about(mut e: Failure, path: &Path) -> Failure {
    e.path = Some(path.to_string_lossy().into_owned());
    e
}

/// Run `spherecut frame`, and print its report.
pub fn run(args: FrameArgs) -> Result<Exit> {
    let json = args.json;
    let (warnings, outcome) = work(args);
    match outcome {
        Ok(done) => {
            if json {
                let envelope =
                    Envelope::new(contract::FRAME, Some(done.result), None).with_warnings(warnings);
                print_json(&envelope, Exit::Ok)
            } else {
                emit(&format!("{}\n", done.line))?;
                Ok(Exit::Ok)
            }
        }
        Err(e) => {
            let exit = contract::frame_exit(e.code);
            if json {
                let envelope = Envelope::<FrameResult>::new(contract::FRAME, None, Some(e))
                    .with_warnings(warnings);
                print_json(&envelope, exit)
            } else {
                to_stderr(&format!(
                    "error: {}\n{}",
                    e.message,
                    spherecut_media::probe::ffmpeg_lines(&e.ffmpeg_log)
                ));
                Ok(exit)
            }
        }
    }
}

/// What a run that wrote its frame reports.
struct Done {
    result: FrameResult,
    /// The line for people.
    line: String,
}

/// Steps 2 and 3, and the worker for the rest: the warnings, and what the
/// run came to.
fn work(args: FrameArgs) -> (Vec<Warning>, Result<Done, Failure>) {
    let dest = match check_output(&args.output, args.overwrite) {
        Ok(dest) => dest,
        Err(e) => return (Vec::new(), Err(e)),
    };
    // Before the temporary file, so that the first Ctrl+C never leaves
    // one behind.
    if let Err(e) = interrupt::install() {
        return (
            Vec::new(),
            Err(failure(
                codes::INTERNAL,
                format!("cannot install the SIGINT handler: {e}"),
            )),
        );
    }
    let tmp = match TempFile::create(&dest) {
        Ok(tmp) => tmp,
        Err(e) => return (Vec::new(), Err(cannot_write(&args.output, &e))),
    };
    let stop = StopSource::new();
    let (tx, rx) = mpsc::channel();
    let mut job = Job {
        args,
        stop: stop.clone(),
        token: stop.current(),
        warnings: Vec::new(),
    };
    let spawned = std::thread::Builder::new()
        .name("frame".into())
        .spawn(move || {
            let outcome = job.run(tmp);
            // The receiver is waiting; it never goes away first.
            let _ = tx.send((job.warnings, outcome));
        });
    let worker = match spawned {
        Ok(w) => w,
        Err(e) => {
            return (
                Vec::new(),
                Err(failure(
                    codes::INTERNAL,
                    format!("cannot start the worker thread: {e}"),
                )),
            );
        }
    };
    let sent = loop {
        match rx.recv_timeout(POLL) {
            Ok(sent) => break Some(sent),
            Err(RecvTimeoutError::Timeout) => {
                if interrupt::interrupted() && !stop.is_shut_down() {
                    tracing::info!("interrupted: stopping");
                    // Stops seeks, and FFmpeg inside a read.
                    stop.shutdown();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break None,
        }
    };
    // A panic on the worker is the run's, as on this thread: under --json
    // the panic hook has already written the document.
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
    let Some((warnings, outcome)) = sent else {
        return (
            Vec::new(),
            Err(failure(
                codes::INTERNAL,
                "the worker thread ended without a result",
            )),
        );
    };
    let outcome = match outcome {
        // Whatever stopped the work, the interrupt did.
        Err(_) if stop.is_shut_down() => Err(cancelled()),
        o => o,
    };
    (warnings, outcome)
}

fn cancelled() -> Failure {
    failure(codes::CANCELLED, "interrupted: no frame written")
}

/// `io_error` for the output `out`.
fn cannot_write(out: &Path, e: &std::io::Error) -> Failure {
    about(
        failure("io_error", format!("cannot write {}: {e}", out.display())),
        out,
    )
}

/// Step 2: where the PNG goes, once nothing is in the way, before any
/// work. Without `--overwrite`, anything at the output is refused, a link
/// included (`output_exists`); with it, links are followed and a regular
/// file there is replaced. A directory, pipe, socket or device is refused
/// either way (`not_a_file`), since a rename would replace the node
/// instead of writing to it (D16), and so is a name that ends in `/`,
/// which names a directory whether or not there is one.
fn check_output(out: &Path, overwrite: bool) -> Result<PathBuf, Failure> {
    use std::os::unix::ffi::OsStrExt;
    let io = |e: std::io::Error| cannot_write(out, &e);
    let not_a_file = |e: std::io::Error| {
        let why = match e.kind() {
            std::io::ErrorKind::IsADirectory => "it is a directory".to_string(),
            _ => e.to_string(),
        };
        about(
            failure(
                "not_a_file",
                format!("cannot write {}: {why}", out.display()),
            ),
            out,
        )
    };
    let name = out.as_os_str().as_bytes();
    if name.ends_with(b"/") || name.ends_with(b"/.") || out.file_name().is_none() {
        return Err(about(
            failure(
                "not_a_file",
                format!(
                    "cannot write {}: the name is a directory's, not a file's",
                    out.display()
                ),
            ),
            out,
        ));
    }
    let dest = if overwrite {
        follow_links(out).map_err(io)?
    } else {
        match std::fs::symlink_metadata(out) {
            Ok(_) => {
                replaceable(out).map_err(not_a_file)?;
                return Err(about(
                    failure(
                        codes::OUTPUT_EXISTS,
                        format!("{} exists; pass --overwrite to replace it", out.display()),
                    ),
                    out,
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.to_path_buf(),
            Err(e) => return Err(io(e)),
        }
    };
    replaceable(&dest).map_err(not_a_file)?;
    Ok(dest)
}

/// Steps 4 to 9, on the worker thread.
struct Job {
    args: FrameArgs,
    stop: StopSource,
    token: StopToken,
    warnings: Vec<Warning>,
}

/// The decoder and what it found, for the report.
struct Decoded {
    frame: Arc<VideoFrame>,
    requested: FrameTime,
    shown: FrameTime,
}

impl Job {
    fn warn(&mut self, code: &'static str, message: String) {
        tracing::warn!("{message}");
        self.warnings.push(Warning::new(code, message));
    }

    /// `Err(cancelled)` once the run has been interrupted.
    fn go_on(&self) -> Result<(), Failure> {
        if self.stop.is_shut_down() {
            Err(cancelled())
        } else {
            Ok(())
        }
    }

    fn file(&self) -> &Path {
        &self.args.file
    }

    fn media_error(&self, e: spherecut_media::MediaError) -> Failure {
        about(failure(e.code(), e.to_string()), self.file())
    }

    fn run(&mut self, mut tmp: TempFile) -> Result<Done, Failure> {
        // Step 4.
        let report = match probe::probe(self.file()) {
            Ok(r) => r,
            Err(e) => {
                let mut f = about(failure(e.code(), e.to_string()), self.file());
                f.ffmpeg_log = e.ffmpeg_log;
                return Err(f);
            }
        };
        let stream = self.choose_stream(&report)?.clone();
        if !self.args.equirect {
            check_equirect(&report, &stream)?;
        }
        // Damage the seek may not reach: it fails the run only if it does.
        for d in &report.damage {
            self.warn(
                d.code,
                format!("{} is damaged: {}", self.file().display(), d.message),
            );
        }
        self.go_on()?;

        // Step 5. The filter is the one rendered with: iso where aniso was
        // asked for and the device can't.
        let mut filter = self.args.filter;
        let gpu = if self.args.reference {
            None
        } else {
            let gpu = Gpu::headless(&self.args.adapter()).map_err(render_error)?;
            gpu.check_size(self.args.size)
                .map_err(|e| failure(codes::USAGE, format!("--size: {e}")))?;
            if self.args.filter == FilterArg::Aniso && !gpu.caps.anisotropy {
                self.warn(
                    codes::warnings::ANISOTROPY_UNAVAILABLE,
                    format!(
                        "{} has no anisotropic filtering: rendering with --filter iso",
                        gpu.caps.info.name
                    ),
                );
                filter = FilterArg::Iso;
            }
            Some(gpu)
        };
        self.go_on()?;

        // Step 6.
        let ids = gpu
            .as_ref()
            .filter(|g| !g.caps.is_cpu)
            .map(|g| gpu_ids(&g.caps.info));
        let node = hw::render_node(ids.as_ref(), self.args.render_node.as_deref());
        let (decode, skipped) = decode_mode(self.args.decoder, &node);
        let backend = FfmpegBackend::on_node(node);
        let req = VideoRequest {
            stream: Some(stream.index),
            decode,
            interrupt: self.stop.shutdown_flag(),
            ..VideoRequest::default()
        };
        let mut dec = backend
            .open_video(self.file(), &req)
            .map_err(|e| self.media_error(e))?;
        let placement = self.placement(dec.as_ref(), &stream);
        self.go_on()?;

        // Step 7.
        let decoded = self.seek(dec.as_mut())?;
        self.go_on()?;

        // Step 8.
        let size = self.args.size;
        let view = self.args.view();
        let image = match &gpu {
            Some(gpu) => render_still(
                gpu,
                decoded.frame.as_ref(),
                &view,
                size,
                StillOptions {
                    filter: match filter {
                        FilterArg::Iso => Filter::Isotropic,
                        FilterArg::Aniso => Filter::Anisotropic,
                    },
                    depth: self.args.depth(),
                    placement,
                    ..StillOptions::default()
                },
            )
            .map_err(render_error)?,
            None => {
                let f64s = reference::render(
                    decoded.frame.as_ref(),
                    &view,
                    size,
                    &reference::Options {
                        level0: false,
                        placement,
                    },
                )
                .map_err(render_error)?;
                quantize(&f64s, self.args.depth())
            }
        };
        self.go_on()?;

        // Step 9.
        let out = &self.args.output;
        let io = |e: std::io::Error| cannot_write(out, &e);
        let png = image.to_png().map_err(render_error)?;
        tmp.file_mut().write_all(&png).map_err(io)?;
        self.go_on()?;
        if self.args.overwrite {
            tmp.persist_replace().map_err(io)?;
        } else {
            #[cfg(debug_assertions)]
            debug_race(tmp.dest());
            tmp.persist_new().map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    about(
                        failure(
                            codes::OUTPUT_EXISTS,
                            format!(
                                "{} appeared while the frame was rendered, and is left alone; \
                                 pass --overwrite to replace it",
                                out.display()
                            ),
                        ),
                        out,
                    )
                } else {
                    io(e)
                }
            })?;
        }

        // Step 10.
        let decode = decode_info(&decoded.frame.decoded_on, skipped);
        let result = FrameResult {
            output: out.to_string_lossy().into_owned(),
            frame: FrameShown {
                pts: decoded.frame.pts,
                time: decoded.shown,
            },
            requested: decoded.requested,
            exactness: "exact",
            size: Size {
                width: size.0,
                height: size.1,
            },
            depth: match self.args.depth {
                DepthArg::Eight => 8,
                DepthArg::Sixteen => 16,
            },
            view: self.args.view_info(),
            filter: match filter {
                FilterArg::Iso => "iso",
                FilterArg::Aniso => "aniso",
            },
            renderer: if gpu.is_some() { "gpu" } else { "reference" },
            decode,
            gpu: gpu.as_ref().map(|g| gpu_info(&g.caps.info)),
            color: *decoded.frame.color(),
            source: SourceInfo {
                stream: stream.index,
                codec: dec.info().codec.clone(),
                size: Size {
                    width: dec.info().size.0,
                    height: dec.info().size.1,
                },
                pix_fmt: dec.info().source_pix_fmt.clone(),
            },
        };
        let line = summary(&result);
        Ok(Done { result, line })
    }

    /// `--stream`, which must be a video stream, else the first video
    /// stream that isn't a cover picture.
    fn choose_stream<'r>(&self, report: &'r ProbeReport) -> Result<&'r StreamInfo, Failure> {
        let is_video = |s: &&StreamInfo| {
            s.kind == StreamKind::Video && !s.disposition.iter().any(|d| d == "attached_pic")
        };
        let videos: Vec<&StreamInfo> = report.streams.iter().filter(is_video).collect();
        match self.args.stream {
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
                            "--stream {n}: {} has no video stream {n} (its video streams: {})",
                            self.file().display(),
                            if listed.is_empty() { "none" } else { &listed }
                        ),
                    )
                }),
            None => videos.first().copied().ok_or_else(|| {
                about(
                    failure(
                        "decode_failed",
                        format!("{} has no video stream", self.file().display()),
                    ),
                    self.file(),
                )
            }),
        }
    }

    /// Where the picture sits on the sphere, from the stream's spherical
    /// metadata, with a warning for each thing M1 does with it: the left
    /// eye of a stereo picture, a crop, and a pose it doesn't apply.
    fn placement(&mut self, dec: &dyn VideoDecode, stream: &StreamInfo) -> Placement {
        let m = dec.info().mapping.clone();
        let stereo = match m.stereo {
            StereoMode::Mono => Stereo::Mono,
            StereoMode::TopBottom => {
                self.warn(
                    codes::warnings::STEREO_LEFT_EYE,
                    "stereo, top-bottom: rendering the left eye (the top half)".into(),
                );
                Stereo::TopBottom
            }
            StereoMode::LeftRight => {
                self.warn(
                    codes::warnings::STEREO_LEFT_EYE,
                    "stereo, left-right: rendering the left eye (the left half)".into(),
                );
                Stereo::LeftRight
            }
            other => {
                self.warn(
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
            self.warn(
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
            let pose = stream
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
            self.warn(
                codes::warnings::POSE_NOT_APPLIED,
                format!("the file's pose{pose} is not applied (M2)"),
            );
        }
        placement
    }

    /// Step 7: the frame `--time` or `--frame` names, exactly.
    fn seek(&mut self, dec: &mut dyn VideoDecode) -> Result<Decoded, Failure> {
        let when = self.args.when();
        let rate = dec.info().frame_rate();
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
                let address =
                    frame_address(dec.info(), n).map_err(|_| self.beyond(dec.info(), when))?;
                match address {
                    FrameAddress::At(t) => {
                        let requested = match rate {
                            Some(rate) => {
                                FrameTime::at(rate, n).map_err(|_| self.beyond(dec.info(), when))?
                            }
                            None => FrameTime {
                                time: t,
                                frame: Some(n),
                            },
                        };
                        (t, requested)
                    }
                    FrameAddress::Counted(n) => return self.count(dec, n),
                }
            }
        };
        // A time that no timestamp of the stream can hold is past its end,
        // which the seek would only fail on.
        if at.to_pts(dec.info().time_base, Rounding::Floor).is_err() {
            return Err(self.beyond(dec.info(), when));
        }
        let outcome = dec
            .seek(at, SeekMode::Exact, &self.token)
            .map_err(|e| self.media_error(e))?;
        let (frame, exactness) = match outcome {
            SeekOutcome::Frame {
                frame, exactness, ..
            } => (frame, exactness),
            SeekOutcome::Stopped => return Err(cancelled()),
        };
        let events = dec.take_events();
        match exactness {
            Exactness::Exact => {}
            Exactness::AfterLast => return Err(self.past_the_end(dec, when, &frame)),
            Exactness::Truncated { last_good_pts } => {
                return Err(self.damaged(dec, when, last_good_pts, &events));
            }
            Exactness::BeforeFirst | Exactness::KeyframeOnly => {
                return Err(failure(
                    codes::INTERNAL,
                    format!("an exact seek to {when} came back {exactness:?}"),
                ));
            }
        }
        self.damage_after(&events);
        let shown = shown(dec, &frame);
        self.check_index(dec, when, &shown)?;
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
        dec: &mut dyn VideoDecode,
        when: TimeSpec,
        shown: &FrameTime,
    ) -> Result<(), Failure> {
        let (TimeSpec::Frame(n), Some(_)) = (when, dec.info().frame_rate()) else {
            return Ok(());
        };
        let Some(i) = shown.frame.filter(|&i| i != n) else {
            return Ok(());
        };
        let file = self.file().display().to_string();
        let e = if self.is_last(dec)? {
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
    fn is_last(&mut self, dec: &mut dyn VideoDecode) -> Result<bool, Failure> {
        let next = dec.next_frame(&self.token);
        self.damage_after(&dec.take_events());
        match next {
            Ok(NextFrame::Frame(_)) => Ok(false),
            Ok(NextFrame::End) => Ok(true),
            Ok(NextFrame::Stopped) => Err(cancelled()),
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
    fn count(&mut self, dec: &mut dyn VideoDecode, n: u64) -> Result<Decoded, Failure> {
        let when = TimeSpec::Frame(n);
        let mut frame = match dec
            .seek(Time::ZERO, SeekMode::Exact, &self.token)
            .map_err(|e| self.media_error(e))?
        {
            SeekOutcome::Frame { frame, .. } => frame,
            SeekOutcome::Stopped => return Err(cancelled()),
        };
        for i in 0..n {
            let next = dec.next_frame(&self.token);
            let events = dec.take_events();
            if let Some(e) = self.damage_before(dec, &events, when, &frame) {
                return Err(e);
            }
            frame = match next.map_err(|e| self.media_error(e))? {
                NextFrame::Frame(f) => f,
                NextFrame::Stopped => return Err(cancelled()),
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
        self.damage_after(&dec.take_events());
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
    fn past_the_end(&self, dec: &dyn VideoDecode, when: TimeSpec, last: &VideoFrame) -> Failure {
        let info = dec.info();
        let end = info
            .duration
            .or_else(|| last.duration.and_then(|d| last.time.checked_add(d).ok()))
            .unwrap_or(last.time);
        let message = match when {
            TimeSpec::Frame(n) => match shown(dec, last).frame {
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
    fn damaged(
        &self,
        dec: &dyn VideoDecode,
        when: TimeSpec,
        last_good_pts: i64,
        events: &[DecoderEvent],
    ) -> Failure {
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
        let good = dec.info().clip_time(last_good_pts).map_or_else(
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
        dec: &dyn VideoDecode,
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
                    .and_then(|p| dec.info().clip_time(p).ok())
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
    fn damage_after(&mut self, events: &[DecoderEvent]) {
        for e in events {
            if let DecoderEvent::Damaged { reason, cause, .. } = e {
                self.warn(
                    cause.code(),
                    format!(
                        "{} is damaged after the frame: {reason}",
                        self.file().display()
                    ),
                );
            }
        }
    }
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

/// Whether the file is a stitched equirect: tagged so (Spherical Video
/// V2's `equi`, or V1's `equirectangular`, in an MP4 or MOV file), or
/// else `not_equirect`, naming what it is, or that its Matroska spherical
/// metadata isn't read.
fn check_equirect(report: &ProbeReport, stream: &StreamInfo) -> Result<(), Failure> {
    let file = report.path.display();
    let refuse = |what: String| {
        about(
            failure(
                codes::NOT_EQUIRECT,
                format!("{file} is {what}; --equirect renders it as equirect anyway"),
            ),
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

/// How the decoder is opened for `--decoder`, on `node`, and the fallback
/// `auto` took without trying, when there is no device to try.
fn decode_mode(decoder: DecoderArg, node: &RenderNode) -> (DecodeMode, Option<FallbackInfo>) {
    let hardware = |kind, fallback| DecodeMode::Hardware {
        kind,
        device: None,
        fallback,
    };
    match decoder {
        DecoderArg::Software => (DecodeMode::Software, None),
        // VA-API on the GPU's node, then software (D39). Without a node
        // there is nothing to try.
        DecoderArg::Auto if !node.path.exists() => (
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
        DecoderArg::Auto => (hardware(HwKind::VaApi, Fallback::Software), None),
        DecoderArg::Vaapi => (hardware(HwKind::VaApi, Fallback::Fail), None),
        DecoderArg::Vulkan => (hardware(HwKind::Vulkan, Fallback::Fail), None),
    }
}

/// Where the frame was decoded, for the JSON.
fn decode_info(path: &DecodePath, skipped: Option<FallbackInfo>) -> DecodeInfo {
    match path {
        DecodePath::Software { .. } => DecodeInfo {
            path: "software",
            fallback: skipped,
        },
        DecodePath::Hardware { kind } => DecodeInfo {
            path: kind.ffmpeg_name(),
            fallback: None,
        },
        DecodePath::FellBack { from, reason } => DecodeInfo {
            path: "software",
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

fn gpu_info(info: &wgpu::AdapterInfo) -> GpuInfo {
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

fn render_error(e: RenderError) -> Failure {
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

/// The report's line: "wrote out.png: 1920×1080 rectilinear 100° at
/// 62.233 s (frame 1867, pts 186700), decoded on VA-API, rendered on AMD
/// Radeon 8060S (RADV)".
fn summary(r: &FrameResult) -> String {
    let mut lens = format!("{} {}°", r.view.projection, number(r.view.fov));
    if let Some(to) = r.view.morph_to {
        lens.push_str(&format!(" morphed {} to {to}", number(r.view.morph)));
    }
    let frame = match r.frame.time.frame {
        Some(n) => format!("frame {n}, pts {}", r.frame.pts),
        None => format!("pts {}", r.frame.pts),
    };
    let decoded = match (&r.decode.path, &r.decode.fallback) {
        (&"software", Some(f)) => {
            format!("decoded in software ({}: {})", hw_name(f.from), f.reason)
        }
        (&"software", None) => "decoded in software".to_string(),
        (path, _) => format!("decoded on {}", hw_name(path)),
    };
    let rendered = match &r.gpu {
        Some(g) if g.name.to_lowercase().contains(&g.driver.to_lowercase()) => {
            format!("rendered on {}", g.name)
        }
        Some(g) => format!("rendered on {} ({})", g.name, g.driver),
        None => "rendered by the CPU reference".to_string(),
    };
    format!(
        "wrote {}: {}×{} {lens} at {} s ({frame}), {decoded}, {rendered}",
        r.output,
        r.size.width,
        r.size.height,
        seconds(r.frame.time.time)
    )
}

/// "VA-API" or "Vulkan" for FFmpeg's name of the device type.
fn hw_name(ffmpeg: &str) -> &str {
    match ffmpeg {
        "vaapi" => "VA-API",
        "vulkan" => "Vulkan",
        other => other,
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
fn seconds(t: Time) -> String {
    number(t.to_secs_f64())
}

/// A number for people: to three decimals, without trailing zeros.
fn number(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// In debug builds, when `SPHERECUT_DEBUG_FRAME_RACE` is set, write the
/// output just before the frame is published, as another program might:
/// the race the contract test shows `frame` losing without replacing
/// what appeared. Release builds have no such switch.
#[cfg(debug_assertions)]
fn debug_race(dest: &Path) {
    if std::env::var_os("SPHERECUT_DEBUG_FRAME_RACE").is_some() {
        let _ = std::fs::write(dest, b"written by someone else");
    }
}
