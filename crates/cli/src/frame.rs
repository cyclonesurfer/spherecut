//! `spherecut frame`: one frame of a stitched equirect, rendered through a
//! virtual camera into a PNG (M1 plan section 5, DECISIONS D27); or with
//! `--project`, the frame of a project's sequence at a time, as `spherecut
//! render` renders it (M2 plan amendment 2 item 5, D51).
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
//! 4. with `--project`, the project and its media, loaded as `render`
//!    loads them (a camera's pair is refused `not_yet` until M3.11,
//!    before the offline check and after any `provisional_geometry`
//!    warning; an offline clip is exit 1), and the sequence's frame at
//!    the time asked for, in sequence time (`out_of_range` past its end):
//!    the source frame it shows, the output's size, and its views, as
//!    [`frame_views`] gives the export them; then probe, through the
//!    confinement (D24), and the layout: only a stitched equirect
//!    renders, unless `--equirect` says it is one (a project's clip is),
//!    or a camera's pair of lenses (an Insta360 file): stitched
//!    ([`Still::open_pair`], with `--geometry`, `--stitch-distance` and
//!    `--seam`) on the GPU, or on the CPU under `--reference`, both
//!    lenses decoded as any stream is (M3.7), or one lens raw with
//!    `--lens`; a pair that can't be stitched (no calibration, or one
//!    that can't be read) is `not_equirect` with `--lens 0|1`'s hint;
//!    the pair's flags on any other file are a usage error, and probe's
//!    `bad_trailer` warnings are the run's too;
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
use spherecut_agent::still::{self, Failure, PairOptions, Still, about, failure, number, seconds};
use spherecut_cli::contract::{
    self, Envelope, Exit, FrameResult, FrameShown, GeometryInfo, LensFrame, SequenceFrame, Size,
    StitchInfo, ViewInfo, Warning, codes,
};
use spherecut_core::cancel::StopSource;
use spherecut_core::fs::{TempFile, follow_links, replaceable};
use spherecut_core::project::Output;
use spherecut_core::project::file;
use spherecut_core::video::VideoPlanes;
use spherecut_core::{FrameTime, Rounding, Time, TimeSpec};
use spherecut_export::FramePlan;
use spherecut_media::Source;
use spherecut_media::backend::SeekMode;
use spherecut_media::camera::status_word;
use spherecut_media::detect::Layout;
use spherecut_media::probe::{ProbeReport, codes as probe_codes};
use spherecut_render::output::{frame_views, view_at};
use spherecut_render::project::OutKind;
use spherecut_render::reference::{RigDebug, RigOptions};
use spherecut_render::{AdapterChoice, Depth, Filter, Gpu, Image, ViewParams};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_sphere::rig::{
    Binding, CanvasMap, Generation, Geometry, QuarterTurn, Status, Tune, TurnAxis, TzSign,
};

use crate::{emit, interrupt, print_json, render, to_stderr};

/// How often the main thread looks for a Ctrl+C while the worker works.
const POLL: Duration = Duration::from_millis(10);

/// The smallest and largest side of the picture `--size` asks for.
const SIDES: std::ops::RangeInclusive<u32> = 16..=16384;

#[derive(Args, Debug, Clone)]
pub struct FrameArgs {
    /// The video: a stitched equirect, tagged with spherical metadata or
    /// treated as one with --equirect, or a camera's pair of lenses (an
    /// Insta360 file), stitched on the GPU, or on the CPU with --reference.
    /// Not with --project.
    #[arg(required_unless_present = "project")]
    pub file: Option<PathBuf>,
    /// A project file: render its sequence's frame at --time or --frame,
    /// in sequence time, as `spherecut render` renders it: the sequence's
    /// output size, its keyed view and its motion blur, with the export's
    /// filter. The view, --size, --filter and --stream flags are the
    /// project's then.
    #[arg(
        long,
        value_name = "PROJECT",
        conflicts_with_all = [
            "file", "yaw", "pitch", "roll", "fov", "projection", "morph_to", "morph", "size",
            "filter", "equirect", "stream", "reference", "lens", "seam", "geometry",
            "stitch_distance",
        ],
    )]
    pub project: Option<PathBuf>,
    /// The PNG to write. Something already there is refused unless
    /// --overwrite is given.
    #[arg(short, long, value_name = "OUT.png")]
    pub output: PathBuf,
    /// When, in clip time from the first frame (with --project, in
    /// sequence time from its start): 12, 12.5, 1:02.5, 01:02:03.250,
    /// 30000/1001, or a frame number such as 370f [default: 0]
    // Hyphens too, so that a negative time gets its own message.
    #[arg(long, value_name = "T", value_parser = time_spec, conflicts_with = "frame", allow_hyphen_values = true)]
    pub time: Option<TimeSpec>,
    /// Frame N, counting the first frame shown as 0 (the same as --time Nf);
    /// with --project, the sequence's frame N at its rate.
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
    /// 360 fisheye and 340 stereographic [default: 100]. Not with
    /// --projection equirect, which is the whole sphere.
    #[arg(long, value_name = "DEG", value_parser = angle, allow_negative_numbers = true)]
    pub fov: Option<f64>,
    /// The virtual camera's projection; equirect is the whole sphere,
    /// turned by --yaw, --pitch and --roll, drawn at --size (2:1, such as
    /// 2048x1024, for a standard equirect).
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
    /// software; vaapi and vulkan fail instead (exit 3). A camera's pair
    /// decodes each lens so, stitched on the GPU or with --reference, as
    /// --lens does its one.
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
    /// Render with the CPU reference (f64, slow) instead of the GPU, a
    /// camera's pair of lenses (an Insta360 file) stitched so too.
    #[arg(long)]
    pub reference: bool,
    /// A camera's pair: lens N alone (0 or 1, by probe's lenses.tracks),
    /// raw, as its track codes it, with no stitch and no calibration;
    /// the view flags go unused.
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u8).range(0..=1),
        conflicts_with_all = ["seam", "geometry", "stitch_distance", "equirect", "stream"],
    )]
    pub lens: Option<u8>,
    /// A camera's pair's seam, through the view, on the GPU or with
    /// --reference: weights paints calibration block 0's weight red and
    /// block 1's blue; difference paints |lens 0 − lens 1| × 8 in linear
    /// light where both see, black elsewhere, clipped at 1 in the PNG.
    #[arg(long, value_enum, conflicts_with_all = ["equirect", "stream"])]
    pub seam: Option<SeamArg>,
    /// A camera's pair's geometry, key by key over the built-in table's
    /// entry for the camera: binding=direct|reversed, canvas=a|b|c,
    /// turn=y|x, tz=as-stored|negated, upright=zero|plus90|minus90|half,
    /// generation=v3|v6. Its status is then user.
    #[arg(
        long,
        value_name = "K=V,...",
        value_parser = geometry_edits,
        conflicts_with_all = ["equirect", "stream"],
    )]
    pub geometry: Option<GeometryEdits>,
    /// How far away a camera's pair's lenses are stitched to meet: metres,
    /// 0.3 or more, such as 2m, or inf [default: 2m].
    #[arg(
        long,
        value_name = "Nm|inf",
        value_parser = distance,
        allow_hyphen_values = true,
        conflicts_with_all = ["equirect", "stream"],
    )]
    pub stitch_distance: Option<Distance>,
    /// Keep the frame on the device, with no copy through the CPU (slice
    /// 5b): VA-API exports it as DMA-BUFs and the GPU imports them, or,
    /// with `--decoder vulkan`, Vulkan Video decodes on the renderer's own
    /// device and the frame is copied out on the GPU; a camera's pair
    /// keeps both lenses so. Fails (exit 3) when the device, its driver
    /// or the decoder can't.
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
    /// The whole sphere, through the turn alone: --projection only, never
    /// --morph-to.
    Equirect,
}

impl ProjectionArg {
    /// The lens's projection; `None` for equirect, which is no lens.
    fn projection(self) -> Option<Projection> {
        match self {
            ProjectionArg::Rectilinear => Some(Projection::Rectilinear),
            ProjectionArg::Fisheye => Some(Projection::Fisheye),
            ProjectionArg::Stereographic => Some(Projection::Stereographic),
            ProjectionArg::Equirect => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            ProjectionArg::Rectilinear => "rectilinear",
            ProjectionArg::Fisheye => "fisheye",
            ProjectionArg::Stereographic => "stereographic",
            ProjectionArg::Equirect => "equirect",
        }
    }

    /// The widest `--fov`, in degrees; `None` for equirect, which takes
    /// none.
    fn max_fov(self) -> Option<f64> {
        self.projection().map(|p| p.max_hfov().to_degrees().round())
    }
}

/// What `--seam` paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SeamArg {
    /// Each calibration block's weight: block 0's red, block 1's blue.
    Weights,
    /// How far the two lenses disagree where both see.
    Difference,
}

/// `--stitch-distance`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Distance {
    Metres(f64),
    Infinite,
}

impl Distance {
    /// The stitch's q, in 1/m: 1/D, and 0 at infinity.
    fn q_per_m(self) -> f64 {
        match self {
            Distance::Metres(m) => 1.0 / m,
            Distance::Infinite => 0.0,
        }
    }
}

/// `--geometry`'s keys, each over the table's entry where given.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeometryEdits {
    /// The flag's value, as given, for the evidence.
    given: String,
    binding: Option<Binding>,
    canvas: Option<CanvasMap>,
    turn: Option<TurnAxis>,
    tz: Option<TzSign>,
    upright: Option<QuarterTurn>,
    generation: Option<Generation>,
}

impl GeometryEdits {
    /// `entry` with each key given changed, its status `user` and its
    /// evidence saying so.
    fn apply(&self, entry: &Geometry) -> Geometry {
        let mut g = entry.clone();
        if let Some(v) = self.binding {
            g.binding = v;
        }
        if let Some(v) = self.canvas {
            g.canvas = v;
        }
        if let Some(v) = self.turn {
            g.turn = v;
        }
        if let Some(v) = self.tz {
            g.tz = v;
        }
        if let Some(v) = self.upright {
            g.upright = v;
        }
        if let Some(v) = self.generation {
            g.generation = v;
        }
        g.status = Status::User;
        g.evidence = format!(
            "--geometry {} over the table's entry ({})",
            self.given, entry.evidence
        );
        g
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

impl FilterArg {
    /// The filter as the JSON result names it.
    fn word(self) -> &'static str {
        match self {
            FilterArg::Iso => "iso",
            FilterArg::Aniso => "aniso",
        }
    }
}

impl From<FilterArg> for Filter {
    fn from(f: FilterArg) -> Filter {
        match f {
            FilterArg::Iso => Filter::Isotropic,
            FilterArg::Aniso => Filter::Anisotropic,
        }
    }
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

/// The nearest `--stitch-distance`, in metres: plan 11.2's bound on q,
/// 1/D at most 3.34.
const NEAREST_M: f64 = 0.3;

/// The field of view without `--fov`, in degrees.
const DEFAULT_FOV: f64 = 100.0;

fn distance(s: &str) -> Result<Distance, String> {
    if s == "inf" {
        return Ok(Distance::Infinite);
    }
    let metres = s
        .strip_suffix('m')
        .and_then(|n| n.parse::<f64>().ok())
        .filter(|n| n.is_finite());
    match metres {
        Some(m) if m >= NEAREST_M => Ok(Distance::Metres(m)),
        Some(_) => Err(format!(
            "{s} is nearer than the stitch reaches: give {NEAREST_M}m or more, or inf"
        )),
        None => Err(format!("expected metres such as 2m, or inf, not {s}")),
    }
}

/// `--geometry`'s value: `key=value` pairs, comma-separated, each key
/// once. A key or value Spherecut doesn't know is named in the error,
/// with the ones it does.
fn geometry_edits(s: &str) -> Result<GeometryEdits, String> {
    fn pick<T: Copy>(key: &str, value: &str, values: &[(&str, T)]) -> Result<T, String> {
        values
            .iter()
            .find(|(name, _)| *name == value)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                let names: Vec<&str> = values.iter().map(|(name, _)| *name).collect();
                format!("{key}={value}: {key} is one of {}", names.join(", "))
            })
    }
    fn once<T>(slot: &mut Option<T>, key: &str, v: T) -> Result<(), String> {
        if slot.replace(v).is_some() {
            return Err(format!("{key} is given twice"));
        }
        Ok(())
    }
    const KEYS: &str = "binding, canvas, turn, tz, upright and generation";
    let mut e = GeometryEdits {
        given: s.to_string(),
        ..GeometryEdits::default()
    };
    for pair in s.split(',') {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!(
                "{pair}: expected key=value, such as binding=direct"
            ));
        };
        match key {
            "binding" => {
                let v = pick(
                    key,
                    value,
                    &[("direct", Binding::Direct), ("reversed", Binding::Reversed)],
                )?;
                once(&mut e.binding, key, v)?;
            }
            "canvas" => {
                let v = pick(
                    key,
                    value,
                    &[
                        ("a", CanvasMap::A),
                        ("b", CanvasMap::B),
                        ("c", CanvasMap::C),
                    ],
                )?;
                once(&mut e.canvas, key, v)?;
            }
            "turn" => {
                let v = pick(key, value, &[("y", TurnAxis::Y), ("x", TurnAxis::X)])?;
                once(&mut e.turn, key, v)?;
            }
            "tz" => {
                let v = pick(
                    key,
                    value,
                    &[
                        ("as-stored", TzSign::AsStored),
                        ("negated", TzSign::Negated),
                    ],
                )?;
                once(&mut e.tz, key, v)?;
            }
            "upright" => {
                let v = pick(
                    key,
                    value,
                    &[
                        ("zero", QuarterTurn::Zero),
                        ("plus90", QuarterTurn::Plus90),
                        ("minus90", QuarterTurn::Minus90),
                        ("half", QuarterTurn::Half),
                    ],
                )?;
                once(&mut e.upright, key, v)?;
            }
            "generation" => {
                let v = pick(
                    key,
                    value,
                    &[("v3", Generation::V3), ("v6", Generation::V6)],
                )?;
                once(&mut e.generation, key, v)?;
            }
            other => {
                return Err(format!("no key {other}: the keys are {KEYS}"));
            }
        }
    }
    Ok(e)
}

impl FrameArgs {
    /// What clap can't check alone: the field of view the projection
    /// allows, and the filter the CPU reference has. The message is a
    /// usage error's.
    pub fn check(&self) -> Result<(), String> {
        if self.morph_to == Some(ProjectionArg::Equirect) {
            return Err(
                "--morph-to equirect: a lens morphs to rectilinear, fisheye or stereographic; \
                 --projection equirect is the whole sphere"
                    .into(),
            );
        }
        match self.projection.max_fov() {
            None => {
                if let Some(fov) = self.fov {
                    return Err(format!(
                        "--fov {}: --projection equirect is the whole sphere, and takes no --fov",
                        number(fov)
                    ));
                }
                if self.morph_to.is_some() {
                    return Err(
                        "--morph: --projection equirect is the whole sphere, and morphs to \
                         nothing"
                            .into(),
                    );
                }
            }
            Some(max) => {
                let fov = self.fov();
                if fov <= 0.0 || fov > max {
                    return Err(format!(
                        "--fov {}: {} takes more than 0 and at most {max} degrees",
                        number(fov),
                        self.projection.name()
                    ));
                }
            }
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

    /// `--fov`, or its default.
    fn fov(&self) -> f64 {
        self.fov.unwrap_or(DEFAULT_FOV)
    }

    /// The virtual camera's lens; `None` for `--projection equirect`.
    fn lens(&self) -> Option<Lens> {
        let from = self.projection.projection()?;
        Some(Lens {
            from,
            to: self
                .morph_to
                .and_then(ProjectionArg::projection)
                .unwrap_or(from),
            morph: self.morph.unwrap_or(0.0),
            hfov: self.fov().to_radians(),
        })
    }

    /// The view: the turn through the lens, or with `--projection
    /// equirect` the whole sphere through the turn.
    fn view(&self) -> ViewParams {
        let turn = Ypr::from_degrees(self.yaw, self.pitch, self.roll).to_quat();
        match self.lens() {
            Some(lens) => ViewParams::new(turn, lens),
            None => ViewParams::equirect(turn),
        }
    }

    fn view_info(&self) -> ViewInfo {
        match self.projection {
            ProjectionArg::Equirect => ViewInfo {
                yaw: self.yaw,
                pitch: self.pitch,
                roll: self.roll,
                fov: 360.0,
                projection: self.projection.name(),
                morph_to: None,
                morph: 0.0,
            },
            _ => ViewInfo {
                yaw: self.yaw,
                pitch: self.pitch,
                roll: self.roll,
                fov: self.fov(),
                projection: self.projection.name(),
                morph_to: self.morph_to.map(ProjectionArg::name),
                morph: self.morph.unwrap_or(0.0),
            },
        }
    }

    /// The first flag given that only a camera's pair takes.
    fn pair_flag(&self) -> Option<&'static str> {
        [
            (self.lens.is_some(), "--lens"),
            (self.seam.is_some(), "--seam"),
            (self.geometry.is_some(), "--geometry"),
            (self.stitch_distance.is_some(), "--stitch-distance"),
        ]
        .into_iter()
        .find_map(|(given, flag)| given.then_some(flag))
    }

    /// The distance the stitch reports, in metres: null for `inf`.
    fn distance_m(&self) -> Option<f64> {
        match self.stitch_distance {
            Some(Distance::Metres(m)) => Some(m),
            Some(Distance::Infinite) => None,
            None => Some(1.0 / Tune::default().q_per_m),
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

    /// What the flags render: the file, through their view.
    fn shot(&self) -> Result<Shot, Failure> {
        let Some(file) = self.file.clone() else {
            return Err(failure(codes::USAGE, "give a FILE, or --project"));
        };
        Ok(Shot {
            file,
            opts: self.still_options(),
            when: self.when(),
            size: self.size,
            views: vec![self.view()],
            view: self.view_info(),
            filter: Some(self.filter),
            requested: None,
            sequence: None,
        })
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

/// What a run renders: the flags' view of the file, or with `--project`
/// the sequence's frame.
struct Shot {
    file: PathBuf,
    opts: still::Options,
    /// What the seek goes to.
    when: TimeSpec,
    size: (u32, u32),
    /// One, or a frame's over the shutter for motion blur.
    views: Vec<ViewParams>,
    view: ViewInfo,
    /// The filter asked for; `None` is the export's, anisotropic where
    /// the GPU filters so (D49).
    filter: Option<FilterArg>,
    /// What `--time` or `--frame` asked for, when the seek's time is
    /// another: the sequence's time, not the source's.
    requested: Option<FrameTime>,
    sequence: Option<SequenceFrame>,
}

/// Step 4 with `--project`: the project at `path` and its media, loaded
/// as `render` loads them, and the sequence's frame at `when`: the
/// source frame it shows, at the output's size, through the views the
/// export renders it through ([`frame_views`]), so that the PNG is the
/// export's frame before its encode.
fn project_shot(
    args: &FrameArgs,
    path: &Path,
    warnings: &mut Vec<Warning>,
) -> Result<Shot, Failure> {
    let project = file::load(path).map_err(|e| render::project_error(path, &e))?;
    render::media(&project, "rendering", warnings)?;
    let sequence = project.the_sequence();
    let clip = project.the_clip();
    let media = project.media_by_id(clip.media).ok_or_else(|| {
        failure(
            codes::INVALID_PROJECT,
            format!("the clip's media {} isn't in the project", clip.media.0),
        )
    })?;
    let plan = FramePlan::new(clip.source, sequence.rate)
        .map_err(|e| failure(codes::INVALID_PROJECT, e.to_string()))?;
    let (n, requested) = sequence_frame(&plan, clip.duration(), args.when())?;
    // Every frame of the plan has a time that fits.
    let fits = |e| failure(codes::INTERNAL, format!("frame {n}'s time: {e}"));
    let at = plan.source_time(n).map_err(fits)?;
    let shown = plan.sequence_time(n).map_err(fits)?;
    let t = at.to_secs_f64();
    let frame_s = sequence.rate.frame_duration().to_secs_f64();
    let (anim, targets) = (&clip.view, project.targets());
    let views = frame_views(
        anim,
        targets,
        &sequence.output,
        t,
        frame_s,
        sequence.export.motion_blur,
    );
    let opts = still::Options {
        stream: Some(media.video.stream as usize),
        equirect: true,
        ..args.still_options()
    };
    Ok(Shot {
        file: media.path.clone(),
        opts,
        when: TimeSpec::Time(at),
        size: views.size,
        view: shown_view(&view_at(anim, targets, &sequence.output, t)),
        filter: None,
        requested: Some(requested),
        sequence: Some(SequenceFrame {
            project: path.to_string_lossy().into_owned(),
            frame: FrameTime {
                time: shown,
                frame: Some(n),
            },
            output: match sequence.output {
                Output::Flat { .. } => "flat",
                Output::Equirect { .. } => "equirect",
            },
            views: views.views.len(),
            capped: views.capped,
        }),
        views: views.views,
    })
}

/// The frame of the sequence `plan` makes that `when` names, and what
/// was asked for, in sequence time: frame N at the sequence's rate, or
/// the frame on screen at a time, the last that starts at or before it.
/// `out_of_range` at or past `length`, where the sequence ends.
fn sequence_frame(
    plan: &FramePlan,
    length: Time,
    when: TimeSpec,
) -> Result<(u64, FrameTime), Failure> {
    let (rate, count) = (plan.rate(), plan.count());
    let out = |message: String| failure(codes::OUT_OF_RANGE, message);
    match when {
        TimeSpec::Frame(n) => {
            if n >= count {
                return Err(out(format!(
                    "no frame {n}: the sequence's frames are 0 to {}",
                    count - 1
                )));
            }
            let requested = FrameTime::at(rate, n).map_err(|e| out(format!("frame {n}: {e}")))?;
            Ok((n, requested))
        }
        TimeSpec::Time(t) => {
            let n = (t >= Time::ZERO && t < length)
                .then(|| rate.time_to_frame(t, Rounding::Floor).ok())
                .flatten()
                .and_then(|n| u64::try_from(n).ok())
                .filter(|&n| n < count);
            let Some(n) = n else {
                let last = plan
                    .sequence_time(count - 1)
                    .map_or_else(|_| "?".into(), seconds);
                return Err(out(format!(
                    "no frame at {} s: the sequence runs from 0 to {} s (its last frame starts at \
                     {last} s)",
                    seconds(t),
                    seconds(length)
                )));
            };
            Ok((
                n,
                FrameTime {
                    time: t,
                    frame: None,
                },
            ))
        }
    }
}

/// The view a sequence's frame is rendered through, as `view` reports
/// it: the keyed camera of a flat output at the frame's time (its FOV
/// the output frame's), or the turn of an equirect one, whose picture
/// is the whole sphere (`projection` is then `equirect`, 360°).
fn shown_view(v: &ViewParams) -> ViewInfo {
    // Degrees to a millionth, so that a keyed 30° isn't 29.999999999999996.
    let deg = |r: f64| (r.to_degrees() * 1e6).round() / 1e6 + 0.0;
    let ypr = Ypr::from_quat(v.orientation);
    let (yaw, pitch, roll) = (deg(ypr.yaw), deg(ypr.pitch), deg(ypr.roll));
    match v.out {
        OutKind::Equirect => ViewInfo {
            yaw,
            pitch,
            roll,
            fov: 360.0,
            projection: "equirect",
            morph_to: None,
            morph: 0.0,
        },
        OutKind::Lens => {
            let morphs = v.lens.to != v.lens.from;
            ViewInfo {
                yaw,
                pitch,
                roll,
                fov: deg(v.lens.hfov),
                projection: lens_name(v.lens.from),
                morph_to: morphs.then(|| lens_name(v.lens.to)),
                morph: if morphs { v.lens.morph } else { 0.0 },
            }
        }
    }
}

fn lens_name(p: Projection) -> &'static str {
    match p {
        Projection::Rectilinear => "rectilinear",
        Projection::Fisheye => "fisheye",
        Projection::Stereographic => "stereographic",
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
pub(crate) fn check_output(out: &Path, overwrite: bool) -> Result<PathBuf, Failure> {
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

    fn run(&mut self, tmp: TempFile) -> Result<Done, Failure> {
        let stop = still::Stop::of(&self.stop);

        // Step 4.
        let shot = match self.args.project.clone() {
            Some(p) => project_shot(&self.args, &p, &mut self.warnings)?,
            None => self.args.shot()?,
        };
        stop.go_on()?;
        let src = Source::path(&shot.file);
        let report = still::probe_report(src, &stop)?;
        let file = shot.file.display().to_string();
        // The trailer's faults, which a stitch or a lens's track would
        // have read, said on the run that renders the file.
        for w in &report.warnings {
            if w.code == probe_codes::BAD_TRAILER {
                self.warn(w.code, format!("{file}: {}", w.message));
            }
        }
        let pair = shot.sequence.is_none()
            && !shot.opts.equirect
            && shot.opts.stream.is_none()
            && report
                .camera
                .as_ref()
                .is_some_and(|c| c.layout == Layout::Insta360DualFisheye);
        if let (false, Some(flag)) = (pair, self.args.pair_flag()) {
            return Err(failure(
                codes::USAGE,
                format!(
                    "{flag}: {file} isn't a camera's pair of lenses (an Insta360 dual fisheye \
                     file), the only file {flag} takes"
                ),
            ));
        }
        let lens = self.args.lens.filter(|_| pair);
        if pair && lens.is_none() {
            return self.run_pair(tmp, &shot, src, &report, &stop);
        }
        let opts = match lens {
            // One lens's own track, raw: no equirect to check.
            Some(n) => still::Options {
                equirect: true,
                stream: Some(lens_stream(&report, n)?),
                ..shot.opts.clone()
            },
            None => shot.opts.clone(),
        };
        let probed = still::probed(report, &opts, &stop, &mut self.warnings)
            .map_err(|e| self.flags(e, pair))?;

        // Step 5.
        let (gpu, filter) = self.gpu(&shot, &opts)?;
        stop.go_on()?;

        // Step 6.
        let mut still = Still::open(src, &probed, gpu.as_ref(), &opts, &stop, &mut self.warnings)?;
        stop.go_on()?;

        // Step 7.
        let decoded = still.seek(shot.when, SeekMode::Exact, &mut self.warnings)?;
        stop.go_on()?;

        // Step 8. --lens draws the track as it is coded, with no view.
        let size = shot.size;
        let look = still::Look {
            draw: match lens {
                Some(_) => still::Draw::Coded,
                None => still::Draw::View,
            },
            filter: filter.into(),
            depth: self.args.depth(),
        };
        let image = still.render(&decoded, &shot.views, size, look)?;
        stop.go_on()?;

        // Step 9.
        self.publish(tmp, &image)?;

        // Step 10.
        let result = FrameResult {
            output: self.args.output.to_string_lossy().into_owned(),
            frame: FrameShown {
                pts: decoded.frame.pts,
                time: decoded.shown,
            },
            requested: shot.requested.unwrap_or(decoded.requested),
            exactness: "exact",
            size: Size {
                width: size.0,
                height: size.1,
            },
            depth: self.depth_bits(),
            view: shot.view,
            filter: filter.word(),
            renderer: if gpu.is_some() { "gpu" } else { "reference" },
            decode: still.decode_info(&decoded),
            gpu: gpu.as_ref().map(|g| still::gpu_info(&g.caps.info)),
            color: *decoded.frame.color(),
            source: still.source_info(),
            sequence: shot.sequence,
            stitch: None,
        };
        let line = summary(&result, lens);
        Ok(Done { result, line })
    }

    /// Step 5: the GPU, unless `--reference` renders on the CPU, and the
    /// filter rendered with: iso where aniso was asked for and the device
    /// can't, with a warning; with `--project`, the export's.
    fn gpu(
        &mut self,
        shot: &Shot,
        opts: &still::Options,
    ) -> Result<(Option<Gpu>, FilterArg), Failure> {
        if self.args.reference {
            return Ok((None, shot.filter.unwrap_or(FilterArg::Iso)));
        }
        let gpu = still::gpu(&self.args.adapter(), opts)?;
        gpu.check_size(shot.size).map_err(|e| match shot.sequence {
            None => failure(codes::USAGE, format!("--size: {e}")),
            Some(_) => still::render_error(e),
        })?;
        still::check_gpu(&gpu, opts)?;
        let filter = match shot.filter {
            Some(FilterArg::Aniso) if !gpu.caps.anisotropy => {
                self.warn(
                    codes::warnings::ANISOTROPY_UNAVAILABLE,
                    format!(
                        "{} has no anisotropic filtering: rendering with --filter iso",
                        gpu.caps.info.name
                    ),
                );
                FilterArg::Iso
            }
            Some(f) => f,
            None if gpu.caps.anisotropy => FilterArg::Aniso,
            None => FilterArg::Iso,
        };
        Ok((Some(gpu), filter))
    }

    /// Steps 5 to 10 for a camera's pair: both lens tracks decoded as any
    /// stream is (`--decoder`, `--render-node` and `--zero-copy`, which
    /// imports both lenses) and stitched ([`Still::open_pair`]) on the
    /// GPU, or on the CPU reference under `--reference`, by the file's
    /// calibration and the table's geometry, or `--geometry`'s over it,
    /// at `--stitch-distance`, through the view or `--seam`'s debug mode.
    /// A pair that can't be stitched is refused with the flags that
    /// answer it ([`Self::flags`]).
    fn run_pair(
        &mut self,
        tmp: TempFile,
        shot: &Shot,
        src: Source<'_>,
        report: &ProbeReport,
        stop: &still::Stop,
    ) -> Result<Done, Failure> {
        let pair_opts = self.pair_options(src)?;
        let (gpu, filter) = self.gpu(shot, &shot.opts)?;
        stop.go_on()?;
        let mut pair = Still::open_pair(
            src,
            report,
            gpu.as_ref(),
            &shot.opts,
            &pair_opts,
            stop,
            &mut self.warnings,
        )
        .map_err(|e| self.flags(e, true))?;
        stop.go_on()?;
        // A pair opens only on a calibration that parses, which probe
        // hashes too.
        let Some(calibration) = report.camera_file.calibration.as_ref() else {
            return Err(failure(
                codes::INTERNAL,
                format!(
                    "{}'s pair opened, but probe's report has no calibration",
                    src.name.display()
                ),
            ));
        };
        let decoded = pair.seek(shot.when, SeekMode::Exact, &mut self.warnings)?;
        stop.go_on()?;
        let look = still::Look {
            draw: still::Draw::View,
            filter: filter.into(),
            depth: self.args.depth(),
        };
        let image = pair.render(&decoded, &shot.views, shot.size, look)?;
        stop.go_on()?;
        self.publish(tmp, &image)?;
        let geometry = pair.geometry();
        let stitch = StitchInfo {
            geometry: GeometryInfo {
                status: status_word(geometry.status),
                evidence: geometry.evidence.clone(),
                // The table's notice for the file's entry, which
                // --geometry's edits start from.
                notice: report.camera_file.geometry.as_ref().and_then(|g| g.notice),
            },
            calibration: calibration.hash.clone(),
            distance_m: self.args.distance_m(),
            lenses: (0..2)
                .map(|t| LensFrame {
                    track: t,
                    stream: pair.lens(t).stream().index,
                    pts: decoded.frames[t].pts,
                })
                .collect(),
        };
        let first = pair.lens(0);
        let result = FrameResult {
            output: self.args.output.to_string_lossy().into_owned(),
            frame: FrameShown {
                pts: decoded.frames[0].pts,
                time: decoded.shown,
            },
            requested: decoded.requested,
            exactness: "exact",
            size: Size {
                width: shot.size.0,
                height: shot.size.1,
            },
            depth: self.depth_bits(),
            view: shot.view.clone(),
            filter: filter.word(),
            renderer: if gpu.is_some() { "gpu" } else { "reference" },
            decode: first.frame_decode_info(&decoded.frames[0]),
            gpu: gpu.as_ref().map(|g| still::gpu_info(&g.caps.info)),
            color: *decoded.frames[0].color(),
            source: first.source_info(),
            sequence: None,
            stitch: Some(stitch),
        };
        let line = summary(&result, None);
        Ok(Done { result, line })
    }

    /// How the pair is stitched: `--geometry` over the file's own entry
    /// ([`still::pair_stitching`]'s, never the table's row alone),
    /// `--stitch-distance`'s q, and `--seam`'s debug mode.
    fn pair_options(&self, src: Source<'_>) -> Result<PairOptions, Failure> {
        let geometry = match &self.args.geometry {
            Some(edits) => Some(edits.apply(&still::pair_stitching(src)?.known.geometry)),
            None => None,
        };
        let mut tune = Tune::default();
        if let Some(d) = self.args.stitch_distance {
            tune.q_per_m = d.q_per_m();
        }
        Ok(PairOptions {
            geometry,
            tune,
            rig: RigOptions {
                debug: match self.args.seam {
                    None => RigDebug::Off,
                    Some(SeamArg::Weights) => RigDebug::Weights,
                    Some(SeamArg::Difference) => RigDebug::SeamDiff,
                },
                ..RigOptions::default()
            },
        })
    }

    fn depth_bits(&self) -> u8 {
        match self.args.depth {
            DepthArg::Eight => 8,
            DepthArg::Sixteen => 16,
        }
    }

    /// Step 9: `image` as a PNG, written to the temporary file and
    /// published only if nothing has appeared at the output meanwhile
    /// (D16), or over it with `--overwrite`.
    fn publish(&self, mut tmp: TempFile, image: &Image) -> Result<(), Failure> {
        let out = &self.args.output;
        let io = |e: std::io::Error| cannot_write(out, &e);
        let png = image.to_png().map_err(still::render_error)?;
        tmp.file_mut().write_all(&png).map_err(io)?;
        self.stop_go_on()?;
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
        Ok(())
    }

    fn stop_go_on(&self) -> Result<(), Failure> {
        still::Stop::of(&self.stop).go_on()
    }

    /// Step 4's errors with the flags that answer them: for a file that
    /// isn't an equirect, `--equirect`, and for a camera's pair that
    /// can't be stitched (no calibration, or one that can't be read)
    /// `--lens 0|1` first, which shows each lens as coded; `--stream` for
    /// a stream it hasn't.
    fn flags(&self, mut e: Failure, pair: bool) -> Failure {
        match (e.code, self.args.stream) {
            (codes::NOT_EQUIRECT, _) => {
                if pair {
                    e.message.push_str("; --lens 0|1 shows each lens as coded");
                }
                e.message
                    .push_str("; --equirect renders it as equirect anyway");
            }
            (codes::USAGE, Some(n)) => e.message = format!("--stream {n}: {}", e.message),
            _ => {}
        }
        e
    }
}

/// Lens `n`'s stream: probe's `lenses.tracks[n]`, or, when the trailer
/// can't be read for them, the file's video stream `n` as probe would
/// have named it (the first two that aren't cover pictures).
fn lens_stream(report: &ProbeReport, n: u8) -> Result<usize, Failure> {
    let k = usize::from(n);
    match &report.camera_file.lenses {
        Some(l) => Ok(l.tracks[k]),
        None => still::video_streams(report)
            .get(k)
            .map(|s| s.index)
            .ok_or_else(|| {
                about(
                    failure(
                        codes::NOT_EQUIRECT,
                        format!(
                            "--lens {n}: {} has no lens {n}: it has fewer than two video \
                             streams",
                            report.path.display()
                        ),
                    ),
                    &report.path,
                )
            }),
    }
}

/// The report's line: "wrote out.png: 1920×1080 rectilinear 100° at
/// 62.233 s (frame 1867, pts 186700), decoded on VA-API, rendered on AMD
/// Radeon 8060S (RADV)"; with `--project`, "... 1920×1080 rectilinear
/// 90° at 2.5 s of p.spherecut (its frame 75; the source's at 4.5 s,
/// frame 135, pts 135135), ..."; a camera's pair, "... 1024×512 equirect
/// stitched at 2 m (geometry provisional) at 1 s ..."; `--lens 0`, "...
/// 384×384 lens 0 raw (stream 0) at ...".
fn summary(r: &FrameResult, raw: Option<u8>) -> String {
    let mut lens = match r.view.projection {
        "equirect" => "equirect".to_string(),
        p => format!("{p} {}°", number(r.view.fov)),
    };
    if let Some(to) = r.view.morph_to {
        lens.push_str(&format!(" morphed {} to {to}", number(r.view.morph)));
    }
    if let Some(n) = raw {
        lens = format!("lens {n} raw (stream {})", r.source.stream);
    }
    if let Some(s) = &r.stitch {
        let at = s
            .distance_m
            .map_or_else(|| "infinity".to_string(), |m| format!("{} m", number(m)));
        lens.push_str(&format!(
            " stitched at {at} (geometry {})",
            s.geometry.status
        ));
    }
    let frame = match r.frame.time.frame {
        Some(n) => format!("frame {n}, pts {}", r.frame.pts),
        None => format!("pts {}", r.frame.pts),
    };
    let at = match &r.sequence {
        Some(s) => format!(
            "at {} s of {} (its frame {}; the source's at {} s, {frame})",
            seconds(s.frame.time),
            s.project,
            s.frame.frame.unwrap_or(0),
            seconds(r.frame.time.time)
        ),
        None => format!("at {} s ({frame})", seconds(r.frame.time.time)),
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
        "wrote {}: {}×{} {lens} {at}, {decoded}, {rendered}",
        r.output, r.size.width, r.size.height
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
