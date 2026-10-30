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
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;
use clap::{Args, ValueEnum};
use spherecut_agent::still::{self, Failure, Still, about, failure, number, seconds};
use spherecut_cli::contract::{
    self, Envelope, Exit, FrameResult, FrameShown, Size, ViewInfo, Warning, codes,
};
use spherecut_core::TimeSpec;
use spherecut_core::cancel::StopSource;
use spherecut_core::fs::{TempFile, follow_links, replaceable};
use spherecut_core::video::VideoPlanes;
use spherecut_media::Source;
use spherecut_render::{AdapterChoice, Depth, Filter, ViewParams};
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
    /// Keep the frame on the device, with no copy through the CPU (slice
    /// 5b): VA-API exports it as DMA-BUFs and the GPU imports them, or,
    /// with `--decoder vulkan`, Vulkan Video decodes on the renderer's own
    /// device and the frame is copied out on the GPU. Fails (exit 3) when
    /// the device, its driver or the decoder can't.
    #[arg(long, conflicts_with = "reference")]
    pub zero_copy: bool,
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
        if self.zero_copy && self.decoder == DecoderArg::Software {
            return Err(
                "--zero-copy decodes on the GPU: give --decoder auto, vaapi or vulkan with it"
                    .into(),
            );
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

    fn still_options(&self) -> still::Options {
        still::Options {
            decoder: match self.decoder {
                DecoderArg::Auto => still::Decoder::Auto,
                DecoderArg::Software => still::Decoder::Software,
                DecoderArg::Vaapi => still::Decoder::Vaapi,
                DecoderArg::Vulkan => still::Decoder::Vulkan,
            },
            render_node: self.render_node.clone(),
            equirect: self.equirect,
            stream: self.stream,
            zero_copy: self.zero_copy,
        }
    }
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

/// Steps 4 to 9, on the worker thread. Steps 4 to 8 are
/// [`spherecut_agent::still`]'s, which `look` renders with too.
struct Job {
    args: FrameArgs,
    stop: StopSource,
    warnings: Vec<Warning>,
}

impl Job {
    fn warn(&mut self, code: &'static str, message: String) {
        tracing::warn!("{message}");
        self.warnings.push(Warning::new(code, message));
    }

    fn run(&mut self, mut tmp: TempFile) -> Result<Done, Failure> {
        let stop = still::Stop::of(&self.stop);
        let opts = self.args.still_options();
        let file = self.args.file.clone();
        let src = Source::path(&file);

        // Step 4.
        let probed =
            still::probe(src, &opts, &stop, &mut self.warnings).map_err(|e| self.flags(e))?;

        // Step 5. The filter is the one rendered with: iso where aniso was
        // asked for and the device can't.
        let mut filter = self.args.filter;
        let gpu = if self.args.reference {
            None
        } else {
            let gpu = still::gpu(&self.args.adapter(), &opts)?;
            gpu.check_size(self.args.size)
                .map_err(|e| failure(codes::USAGE, format!("--size: {e}")))?;
            still::check_gpu(&gpu, &opts)?;
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
        stop.go_on()?;

        // Step 6.
        let mut still = Still::open(src, &probed, gpu.as_ref(), &opts, &stop, &mut self.warnings)?;
        stop.go_on()?;

        // Step 7.
        let decoded = still.seek(self.args.when(), &mut self.warnings)?;
        stop.go_on()?;

        // Step 8.
        let size = self.args.size;
        let look = still::Look {
            filter: match filter {
                FilterArg::Iso => Filter::Isotropic,
                FilterArg::Aniso => Filter::Anisotropic,
            },
            depth: self.args.depth(),
        };
        let image = still.render(&decoded, &self.args.view(), size, look)?;
        stop.go_on()?;

        // Step 9.
        let out = &self.args.output;
        let io = |e: std::io::Error| cannot_write(out, &e);
        let png = image.to_png().map_err(still::render_error)?;
        tmp.file_mut().write_all(&png).map_err(io)?;
        stop.go_on()?;
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
            decode: still.decode_info(&decoded),
            gpu: gpu.as_ref().map(|g| still::gpu_info(&g.caps.info)),
            color: *decoded.frame.color(),
            source: still.source_info(),
        };
        let line = summary(&result);
        Ok(Done { result, line })
    }

    /// Step 4's errors with the flags that answer them: `--equirect` for a
    /// file that isn't one, and `--stream` for a stream it hasn't.
    fn flags(&self, mut e: Failure) -> Failure {
        match (e.code, self.args.stream) {
            (codes::NOT_EQUIRECT, _) => {
                e.message
                    .push_str("; --equirect renders it as equirect anyway");
            }
            (codes::USAGE, Some(n)) => e.message = format!("--stream {n}: {}", e.message),
            _ => {}
        }
        e
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
