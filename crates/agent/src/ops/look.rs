//! `look` (a1-plan section 6.2; amendments 2, 7, 12, 14, 16, 19, 23, 27
//! and 31): frames of a video as one image of labelled tiles, and the JSON
//! that says what each tile holds, so that a client that drops the image
//! still knows what was in it.
//!
//! Each time is decoded once, in the order the file plays (the times
//! sorted), with frame's decoder and the copy path ([`still`]), and all of
//! its tiles are rendered from that frame on the one GPU a process keeps.
//! [`crate::image`] lays the tiles out and labels them; the whole is
//! encoded in memory, then returned inline within the image budget or
//! written to `output` under the write root.
//!
//! still's messages name the file and quote its metadata, so none reaches
//! the agent: each failure is put in look's own words by its code, at the
//! argument it is about (`file`, `times[2]`, `size`). So are its warnings,
//! but for the few whose words come from Spherecut alone.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use spherecut_core::{FrameTime, Rational, Rounding, Time, TimeSpec};
use spherecut_media::backend::{SeekMode, Timing, VideoStreamInfo};
use spherecut_media::probe::{StreamInfo, StreamKind};
use spherecut_render::{AdapterChoice, Depth, Filter, Gpu, Image, ViewParams};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection as LensProjection};

use super::Ctx;
use crate::contract::{self, Warning};
use crate::error::{ToolError, codes};
use crate::image::{self as picture, Canvas, Layout};
use crate::limits::{
    IMAGE_DEFAULT_EDGE, IMAGE_MAX_EDGE, JPEG_QSCALE, JPEG_QSCALE_MAX, MAX_TILES, RAW_STREAMS,
};
use crate::still::{self, Decoded, Draw, Failure, Still, number, seconds};
use crate::{rpc, schema, text, tools};

/// What each tile shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// A virtual camera.
    #[default]
    View,
    /// The whole equirect, under a labelled 30° grid.
    Sphere,
    /// The six faces of a cube around the camera.
    Cube,
    /// A small tile a time, for an overview.
    Sheet,
    /// The picture as coded, each video stream side by side.
    Raw,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::View => "view",
            Mode::Sphere => "sphere",
            Mode::Cube => "cube",
            Mode::Sheet => "sheet",
            Mode::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Jpeg,
    Png,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "jpeg",
            Format::Png => "png",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Format::Jpeg => "image/jpeg",
            Format::Png => "image/png",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seek {
    /// The frame each time names.
    #[default]
    Exact,
    /// The keyframe a seek to each time lands on: faster, and near it.
    Keyframe,
}

impl Seek {
    fn mode(self) -> SeekMode {
        match self {
            Seek::Exact => SeekMode::Exact,
            Seek::Keyframe => SeekMode::Keyframe,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Seek::Exact => "exact",
            Seek::Keyframe => "keyframe",
        }
    }
}

/// A virtual camera's lens (`sphere::perspective`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    #[default]
    Rectilinear,
    Fisheye,
    Stereographic,
}

impl Projection {
    fn lens(self) -> LensProjection {
        match self {
            Projection::Rectilinear => LensProjection::Rectilinear,
            Projection::Fisheye => LensProjection::Fisheye,
            Projection::Stereographic => LensProjection::Stereographic,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Projection::Rectilinear => "rectilinear",
            Projection::Fisheye => "fisheye",
            Projection::Stereographic => "stereographic",
        }
    }

    /// What a label adds for it.
    fn tag(self) -> &'static str {
        match self {
            Projection::Rectilinear => "",
            Projection::Fisheye => " fish",
            Projection::Stereographic => " stereo",
        }
    }

    /// The widest `fov`, in degrees, as `frame --fov` has it.
    fn max_fov(self) -> f64 {
        self.lens().max_hfov().to_degrees().round()
    }

    /// The field of view down a tile `aspect` times as wide as it is tall
    /// for `fov` across: the angle at the top edge's normalised radius,
    /// `1 / aspect`, through the projection's own θ(ρ) (amendment 31).
    fn vfov(self, fov: f64, aspect: f64) -> f64 {
        let half = (fov.min(self.max_fov()) / 2.0).to_radians();
        2.0 * self.lens().theta(1.0 / aspect, half).to_degrees()
    }

    /// The field of view across whose [`vfov`](Self::vfov) is `vfov`.
    fn fov(self, vfov: f64, aspect: f64) -> f64 {
        let h = (vfov / 2.0).to_radians();
        let half = match self {
            Projection::Rectilinear => (aspect * h.tan()).atan(),
            Projection::Fisheye => aspect * h,
            Projection::Stereographic => 2.0 * (aspect * (h / 2.0).tan()).atan(),
        };
        2.0 * half.to_degrees()
    }
}

/// A camera as the agent gives it, in degrees (D33's axes).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    #[serde(default)]
    pub yaw: f64,
    #[serde(default)]
    pub pitch: f64,
    #[serde(default)]
    pub roll: f64,
    pub fov: Option<f64>,
    pub vfov: Option<f64>,
    #[serde(default)]
    pub projection: Projection,
}

/// A time as an agent writes it (a1-plan section 4.1): a string, or a
/// `[num, den]` pair of seconds, which [`time`] checks.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TimeArg {
    Text(String),
    Pair(Vec<Value>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub file: PathBuf,
    #[serde(default)]
    pub mode: Mode,
    pub times: Option<Vec<TimeArg>>,
    /// Times spread evenly: between `times`' two, or over the clip.
    pub count: Option<u32>,
    pub view: Option<View>,
    /// Each checked against `view`'s schema here: the schema gives its
    /// items no properties of their own, so that `tools/list` stays small.
    pub views: Option<Vec<Value>>,
    pub size: Option<u32>,
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub seek: Seek,
    pub output: Option<PathBuf>,
    #[serde(default)]
    pub assume_equirect: bool,
}

/// A view tile's shape: a video frame's.
const VIEW_ASPECT: f64 = 16.0 / 9.0;

/// The shape the whole image comes closest to: a screen's, or for
/// `sphere` an equirect's (a1-plan section 6.2).
const SCREEN: f64 = 16.0 / 9.0;

/// The `fov` a camera has when none is given.
const FOV: f64 = 90.0;

/// The six faces of `cube`, by D33's yaw and pitch, each 90° square.
const FACES: [(&str, f64, f64); 6] = [
    ("front", 0.0, 0.0),
    ("right", 90.0, 0.0),
    ("back", 180.0, 0.0),
    ("left", -90.0, 0.0),
    ("up", 0.0, 90.0),
    ("down", 0.0, -90.0),
];

/// The most times `cube` takes: six tiles each.
const CUBE_TIMES: usize = MAX_TILES / FACES.len();

/// The times a sheet spreads over the clip when none are given.
const SHEET_TIMES: u32 = 12;

/// How far before the end the last of a spread stops when the frames
/// can't be counted: less than any frame lasts, at up to 2,000 fps.
const BEFORE_END: (i64, i64) = (1, 2000);

/// The ways a time is written, for `valid`.
const TIME_FORMS: &str =
    "\"12.5\", \"1:02.5\", \"30000/1001\", \"370f\" (frame 370), or [num, den] of seconds";

/// What to try when the call runs out of time (amendment 2).
const FASTER: &str = "seek \"keyframe\", fewer times, or a clip's files.proxy from probe";

/// A camera as rendered: both fields of view, for the tile's shape.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Camera {
    yaw: f64,
    pitch: f64,
    roll: f64,
    fov: f64,
    vfov: f64,
    projection: Projection,
}

impl Camera {
    /// `v`, the argument `arg`, on tiles `aspect` wide for each unit
    /// tall.
    fn new(v: &View, aspect: f64, arg: &str) -> Result<Camera, ToolError> {
        let p = v.projection;
        let max = p.max_fov();
        let (fov, vfov) = match (v.fov, v.vfov) {
            (Some(_), Some(_)) => {
                return Err(ToolError::new(codes::BAD_ARGUMENT, "can't come with fov")
                    .at(format!("{arg}.vfov"))
                    .valid("fov (across) or vfov (down), not both"));
            }
            (Some(f), None) => (f, p.vfov(f, aspect)),
            (None, Some(vf)) => (p.fov(vf, aspect), vf),
            (None, None) => (FOV, p.vfov(FOV, aspect)),
        };
        // A NaN from vfov's conversion fails here too.
        if !(fov > 0.0 && fov <= max) {
            let e = ToolError::new(codes::OUT_OF_RANGE, format!("too wide for {}", p.name()));
            return Err(if v.vfov.is_some() {
                e.at(format!("{arg}.vfov")).valid(format!(
                    "at most {} on these tiles, which give a fov of {} at most",
                    number(p.vfov(max, aspect)),
                    number(max)
                ))
            } else {
                e.at(format!("{arg}.fov"))
                    .valid(format!("more than 0 and at most {}", number(max)))
            });
        }
        Ok(Camera {
            yaw: v.yaw,
            pitch: v.pitch,
            roll: v.roll,
            fov,
            vfov,
            projection: p,
        })
    }

    fn face(yaw: f64, pitch: f64) -> Camera {
        Camera {
            yaw,
            pitch,
            roll: 0.0,
            fov: 90.0,
            vfov: 90.0,
            projection: Projection::Rectilinear,
        }
    }

    fn params(&self) -> ViewParams {
        ViewParams::new(
            Ypr::from_degrees(self.yaw, self.pitch, self.roll).to_quat(),
            Lens::new(self.projection.lens(), self.fov.to_radians()),
        )
    }

    fn json(&self) -> Value {
        let round = |v: f64| (v * 1000.0).round() / 1000.0;
        json!({
            "yaw": self.yaw,
            "pitch": self.pitch,
            "roll": self.roll,
            "fov": round(self.fov),
            "vfov": round(self.vfov),
            "projection": self.projection.name(),
        })
    }

    /// A label's second line: `y90 p-20 f100`, then the roll and the
    /// projection when they aren't the default (amendment 16).
    fn label(&self) -> String {
        let short = |v: f64| {
            if (v - v.round()).abs() < 0.05 {
                format!("{}", v.round() as i64)
            } else {
                format!("{v:.1}")
            }
        };
        let mut s = format!(
            "y{} p{} f{}",
            short(self.yaw),
            short(self.pitch),
            short(self.fov)
        );
        if self.roll != 0.0 {
            s.push_str(&format!(" r{}", short(self.roll)));
        }
        s.push_str(self.projection.tag());
        s
    }
}

/// What one tile shows of its time.
#[derive(Debug, Clone, Copy)]
enum Shows {
    Camera(Camera),
    /// Face `n` of [`FACES`].
    Face(usize),
    /// The equirect, under a grid: labelled for `sphere`, light for a
    /// sheet.
    Equirect {
        labelled: bool,
    },
    /// The `n`th video stream raw shows, as coded.
    Stream(usize),
}

#[derive(Debug, Clone, Copy)]
struct Tile {
    /// Which of the times.
    time: usize,
    shows: Shows,
}

/// A time to show, and the argument that asked for it.
#[derive(Debug, Clone)]
struct When {
    arg: String,
    spec: TimeSpec,
}

/// How the request asks for its times, before the file says how long it
/// is.
#[derive(Debug, Clone)]
enum Times {
    /// As given.
    Given(Vec<When>),
    /// `count` spread evenly between two times, or over the clip.
    Spread {
        arg: &'static str,
        count: u32,
        ends: Option<(When, When)>,
    },
}

impl Times {
    fn len(&self) -> usize {
        match self {
            Times::Given(w) => w.len(),
            Times::Spread { count, .. } => *count as usize,
        }
    }
}

/// The request checked, as far as it can be without the file.
#[derive(Debug)]
struct Plan {
    times: Times,
    /// The cameras of `view`, or each of `views`; none for the other
    /// modes, and for a sheet without a view.
    cameras: Vec<Camera>,
    edge: u32,
    /// The argument a tile count past the limits is blamed on.
    tiles_arg: &'static str,
}

fn bad(arg: impl Into<String>, message: impl Into<String>) -> ToolError {
    ToolError::new(codes::BAD_ARGUMENT, message).at(arg)
}

/// `t`, the argument `arg`, as a time.
fn time(arg: &str, t: &TimeArg) -> Result<TimeSpec, ToolError> {
    match t {
        TimeArg::Text(s) => s.parse::<TimeSpec>().map_err(|e| {
            ToolError::new(codes::BAD_ARGUMENT, e.to_string())
                .at(arg)
                .valid(TIME_FORMS)
        }),
        TimeArg::Pair(v) => {
            let pair = || {
                bad(arg, "a pair is two integers, seconds as [num, den]")
                    .valid("such as [30000, 1001] for 30000/1001 s")
            };
            let (Some(num), Some(den)) = (
                v.first().and_then(Value::as_i64),
                v.get(1).and_then(Value::as_i64),
            ) else {
                return Err(pair());
            };
            if v.len() != 2 {
                return Err(pair());
            }
            if den <= 0 {
                return Err(bad(arg, "the denominator must be 1 or more"));
            }
            if num < 0 {
                return Err(bad(arg, "a time can't be negative: the clip starts at 0"));
            }
            let r = Rational::new(num, den).map_err(|_| pair())?;
            Ok(TimeSpec::Time(Time::seconds(r)))
        }
    }
}

/// `views[i]` checked against `view`'s schema, then read.
fn view_at(i: usize, v: &Value) -> Result<View, ToolError> {
    let arg = format!("views[{i}]");
    let shape = tools::schema("look").map(|s| &s["properties"]["view"]);
    if let Some(shape) = shape {
        schema::validate(shape, v).map_err(|mut e| {
            e.argument = Some(match e.argument.take() {
                Some(a) => format!("{arg}.{a}"),
                None => arg.clone(),
            });
            e
        })?;
    }
    serde_json::from_value(v.clone()).map_err(|_| bad(arg, "isn't a view").valid("as view"))
}

/// Check what can be checked before the file is opened.
fn plan(req: &Request, cx: &Ctx) -> Result<Plan, ToolError> {
    let mode = req.mode;
    if req.view.is_some() && req.views.is_some() {
        return Err(bad("views", "can't come with view").valid("view, or views, not both"));
    }
    let wants_camera = req.view.is_some() || req.views.is_some();
    if wants_camera && !matches!(mode, Mode::View | Mode::Sheet) {
        let arg = if req.view.is_some() { "view" } else { "views" };
        return Err(
            bad(arg, format!("isn't used by mode {}", mode.name())).valid("mode view or sheet")
        );
    }
    if mode == Mode::Sheet && req.views.is_some() {
        return Err(bad("views", "a sheet shows one view at every time")
            .valid("view")
            .next("mode view shows several views of one time"));
    }
    let given = |ts: &[TimeArg]| -> Result<Vec<When>, ToolError> {
        ts.iter()
            .enumerate()
            .map(|(i, t)| {
                let arg = format!("times[{i}]");
                Ok(When {
                    spec: time(&arg, t)?,
                    arg,
                })
            })
            .collect()
    };
    let times = match (&req.times, req.count) {
        (Some(ts), Some(count)) => {
            if ts.len() != 2 {
                return Err(bad("times", "with count, times are the first and the last")
                    .valid("two times, or none to spread count over the clip"));
            }
            let mut w = given(ts)?.into_iter();
            let (Some(a), Some(b)) = (w.next(), w.next()) else {
                return Err(ToolError::internal("two times went missing"));
            };
            Times::Spread {
                arg: "count",
                count,
                ends: Some((a, b)),
            }
        }
        (None, Some(count)) => Times::Spread {
            arg: "count",
            count,
            ends: None,
        },
        (Some(ts), None) => Times::Given(given(ts)?),
        (None, None) if mode == Mode::Sheet => Times::Spread {
            arg: "times",
            count: SHEET_TIMES,
            ends: None,
        },
        (None, None) => Times::Given(vec![When {
            arg: "times".into(),
            spec: TimeSpec::default(),
        }]),
    };
    let n = times.len();
    if req.views.is_some() && n != 1 {
        return Err(bad("views", "views are of one time")
            .valid("one time with views, or one view with several times"));
    }
    if mode == Mode::Cube && n > CUBE_TIMES {
        return Err(ToolError::new(
            codes::OUT_OF_RANGE,
            format!("cube takes {CUBE_TIMES} times at most, six faces each"),
        )
        .at("times")
        .valid(format!("1 to {CUBE_TIMES} times")));
    }
    if mode == Mode::Sheet && n < 2 {
        return Err(bad("times", "a sheet shows 2 times or more")
            .valid(format!("2 to {MAX_TILES} times, or count"))
            .next("mode view or sphere shows one time"));
    }
    let aspect = if mode == Mode::Cube { 1.0 } else { VIEW_ASPECT };
    let cameras = match (&req.view, &req.views) {
        (Some(v), _) => vec![Camera::new(v, aspect, "view")?],
        (None, Some(vs)) => vs
            .iter()
            .enumerate()
            .map(|(i, v)| Camera::new(&view_at(i, v)?, aspect, &format!("views[{i}]")))
            .collect::<Result<_, _>>()?,
        (None, None) if mode == Mode::View => vec![Camera::new(&View::default(), aspect, "view")?],
        (None, None) => Vec::new(),
    };
    let per_time = match mode {
        Mode::View => cameras.len(),
        Mode::Cube => FACES.len(),
        // raw's streams are known once the file is open.
        Mode::Sphere | Mode::Sheet | Mode::Raw => 1,
    };
    let tiles_arg = if req.views.is_some() {
        "views"
    } else {
        "times"
    };
    if n * per_time > MAX_TILES {
        return Err(too_many(n * per_time, tiles_arg));
    }
    let edge = req.size.unwrap_or(IMAGE_DEFAULT_EDGE);
    if edge > IMAGE_MAX_EDGE && req.output.is_none() {
        return Err(ToolError::new(
            codes::IMAGE_TOO_LARGE,
            format!("past the {IMAGE_MAX_EDGE} pixels an image inline may have"),
        )
        .at("size")
        .valid(format!("64 to {IMAGE_MAX_EDGE}, or up to 4096 with output"))
        .next("give output, a new file under the write root, for the larger image"));
    }
    if let Some(out) = &req.output {
        check_output(out, req.format)?;
        if !cx.roots.writes() {
            // The error create gives without writing on, before the work.
            cx.roots.create("output", out)?;
        }
    }
    Ok(Plan {
        times,
        cameras,
        edge,
        tiles_arg,
    })
}

fn too_many(n: usize, arg: &str) -> ToolError {
    ToolError::new(
        codes::OUT_OF_RANGE,
        format!("{n} tiles, past the {MAX_TILES} an image holds"),
    )
    .at(arg)
    .valid(format!(
        "{MAX_TILES} tiles at most: times × views, × 6 for cube, × streams for raw"
    ))
}

/// An output named for one format can't take the other.
fn check_output(out: &Path, format: Format) -> Result<(), ToolError> {
    let ext = out
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let named = match ext.as_deref() {
        Some("jpg" | "jpeg") => Format::Jpeg,
        Some("png") => Format::Png,
        _ => return Ok(()),
    };
    if named == format {
        return Ok(());
    }
    Err(
        bad("output", format!("is named for {}", named.name())).valid(format!(
            "format {}, or a name for {}",
            named.name(),
            format.name()
        )),
    )
}

/// The one GPU a process renders on (a1-plan section 6.2): made by the
/// first call that needs it, on `SPHERECUT_ADAPTER`'s choice, and made
/// again after it is lost.
static GPU: Mutex<Option<Arc<Gpu>>> = Mutex::new(None);

fn gpu() -> Result<Arc<Gpu>, ToolError> {
    let mut slot = GPU.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(g) = slot.as_ref().filter(|g| !g.is_lost()) {
        return Ok(g.clone());
    }
    let adapter = AdapterChoice::from_env().unwrap_or(AdapterChoice::Auto);
    let g =
        Arc::new(still::gpu(&adapter, &still::Options::default()).map_err(|f| render_failed(&f))?);
    *slot = Some(g.clone());
    Ok(g)
}

/// Forget `g` when it is lost, so that the next call makes another.
fn forget_if_lost(g: &Arc<Gpu>) {
    if g.is_lost() {
        let mut slot = GPU.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, g)) {
            *slot = None;
        }
    }
}

/// A failure opening or probing the file, in look's words.
fn file_failed(f: &Failure) -> ToolError {
    let (code, message, next) = match f.code {
        contract::codes::NOT_EQUIRECT => (
            codes::NOT_EQUIRECT,
            "isn't a tagged equirect",
            "mode \"raw\" shows the picture as coded; assume_equirect: true renders a stitched \
             file without spherical metadata",
        ),
        "decode_failed" | "no_stream" => (codes::NO_VIDEO, "has no video stream", "probe it"),
        "no_decoder" => (
            codes::DECODE_FAILED,
            "has video this FFmpeg has no decoder for",
            "call doctor: it lists the decoders; spherecut doctor in a terminal says what to \
             install",
        ),
        "too_large" => (
            codes::DECODE_FAILED,
            "has a picture larger than Spherecut decodes",
            "a clip's files.proxy from probe",
        ),
        "unsupported_pixel_format" | "not_implemented" => (
            codes::DECODE_FAILED,
            "has video in a form Spherecut doesn't decode yet",
            "probe it with detail \"full\"",
        ),
        "hw_unsupported" | "hw_failed" | "hw_unavailable" => (
            codes::DECODE_FAILED,
            "failed to decode on the GPU",
            "call doctor",
        ),
        code => {
            let (code, message) = super::probe::words(code);
            (
                code,
                message,
                "probe it with detail \"full\" for FFmpeg's own words",
            )
        }
    };
    ToolError::new(code, message).at("file").next(next)
}

/// A failure at a time, the argument `arg`, in look's words. A stop is
/// the caller's to word.
fn time_failed(f: &Failure, arg: &str, info: &VideoStreamInfo) -> ToolError {
    match f.code {
        codes::OUT_OF_RANGE => {
            let mut e = ToolError::new(codes::OUT_OF_RANGE, "is past the end of the video").at(arg);
            if let Some(d) = info.duration {
                e = e.valid(format!("under {} s", seconds(d)));
            }
            e
        }
        "truncated" => ToolError::new(codes::DAMAGED, "the video is cut short before this time")
            .at(arg)
            .next("an earlier time"),
        "damaged" => ToolError::new(codes::DAMAGED, "the video is damaged at this time")
            .at(arg)
            .next("a time a few seconds away"),
        "io_error" => ToolError::new(codes::DAMAGED, "the file can't be read at this time")
            .at(arg)
            .next("a time a few seconds away"),
        codes::INTERNAL => ToolError::internal("a seek didn't land where it should").at(arg),
        _ => ToolError::new(
            codes::DECODE_FAILED,
            "the frame at this time doesn't decode",
        )
        .at(arg)
        .next("a time a few seconds away"),
    }
}

/// A GPU's failure, in look's words.
fn render_failed(f: &Failure) -> ToolError {
    match f.code {
        "no_gpu" => ToolError::new(
            codes::NO_GPU,
            "no GPU here, not even Mesa's software Vulkan (lavapipe)",
        )
        .next("ask the user to install mesa-vulkan-drivers; doctor says more"),
        "adapter_not_found" => {
            ToolError::new(codes::NO_GPU, "the GPU SPHERECUT_ADAPTER names isn't here")
                .next("ask the user to change SPHERECUT_ADAPTER or unset it")
        }
        "out_of_gpu_memory" => ToolError::new(codes::RENDER_FAILED, "the GPU ran out of memory")
            .at("size")
            .next("a smaller size, or fewer tiles"),
        _ => ToolError::new(codes::RENDER_FAILED, "the GPU failed to render a tile")
            .next("call again; doctor checks the GPU"),
    }
}

/// A stop, as the call's deadline or cancel has it, with how far it got.
fn halted(cx: &Ctx, done: usize, total: usize) -> ToolError {
    match cx.call.check() {
        Err(e) if e.code == codes::TIMED_OUT && total == 0 => e.next(FASTER),
        Err(e) if e.code == codes::TIMED_OUT => ToolError::new(
            codes::TIMED_OUT,
            format!("{} after {done} of {total} tiles", e.message),
        )
        .next(FASTER),
        Err(e) => e,
        Ok(()) => ToolError::new(codes::CANCELLED, "the call was stopped"),
    }
}

/// The video streams of the file, cover pictures aside, as raw shows them.
fn videos(probed: &still::Probed) -> Vec<StreamInfo> {
    probed
        .report
        .streams
        .iter()
        .filter(|s| {
            s.kind == StreamKind::Video && !s.disposition.iter().any(|d| d == "attached_pic")
        })
        .cloned()
        .collect()
}

/// `count` times spread evenly over `info`'s stream: by frame on a
/// constant-rate stream that says how many it has, else by time, the last
/// a moment before the end.
fn over_clip(count: u32, arg: &str, info: &VideoStreamInfo) -> Result<Vec<When>, ToolError> {
    let last = count.saturating_sub(1).max(1);
    let when = |spec| When {
        arg: arg.to_owned(),
        spec,
    };
    if let Timing::Constant(rate) = info.timing {
        let frames = info.frame_count.or_else(|| {
            let d = info.duration?;
            let n = d.as_rational().checked_mul(rate.as_rational()).ok()?;
            u64::try_from(n.round(Rounding::Ceil)).ok()
        });
        if let Some(n) = frames.filter(|&n| n > 0) {
            let (n, last) = (n - 1, u64::from(last));
            return Ok((0..u64::from(count))
                .map(|i| when(TimeSpec::Frame((n * i + last / 2) / last)))
                .collect());
        }
    }
    let Some(d) = info.duration else {
        return Err(bad(arg, "the video doesn't say how long it is")
            .next("give two times, the first and the last, for count to spread between"));
    };
    let end = Rational::new(BEFORE_END.0, BEFORE_END.1)
        .ok()
        .and_then(|e| d.as_rational().checked_sub(e).ok())
        .filter(|e| e.is_positive())
        .map_or(Time::ZERO, Time::seconds);
    Ok((0..count)
        .map(|i| when(TimeSpec::Time(lerp(Time::ZERO, end, i, last))))
        .collect())
}

/// The time `i / last` of the way from `a` to `b`: exact, or to the
/// microsecond where the fraction would overflow.
fn lerp(a: Time, b: Time, i: u32, last: u32) -> Time {
    let exact = || {
        let f = Rational::new(i64::from(i), i64::from(last))?;
        let d = b
            .as_rational()
            .checked_sub(a.as_rational())?
            .checked_mul(f)?;
        a.as_rational().checked_add(d).map(Time::seconds)
    };
    exact().unwrap_or_else(|_| {
        let s =
            a.to_secs_f64() + (b.to_secs_f64() - a.to_secs_f64()) * f64::from(i) / f64::from(last);
        Rational::new((s * 1e6).round() as i64, 1_000_000).map_or(a, Time::seconds)
    })
}

/// `count` times spread evenly from `a` to `b`, both included.
fn between(count: u32, a: &When, b: &When, info: &VideoStreamInfo) -> Result<Vec<When>, ToolError> {
    let as_time = |w: &When| match w.spec {
        TimeSpec::Time(t) => Ok(t),
        TimeSpec::Frame(n) => info
            .frame_rate()
            .and_then(|r| FrameTime::at(r, n).ok())
            .map(|f| f.time)
            .ok_or_else(|| {
                bad(
                    &w.arg,
                    "count spreads times, not frames, on a video whose frame rate varies",
                )
                .valid("a time in seconds")
            }),
    };
    let (ta, tb) = (as_time(a)?, as_time(b)?);
    if tb <= ta {
        return Err(bad(
            &b.arg,
            "must come after times[0], for count to spread between them",
        ));
    }
    let last = count.saturating_sub(1).max(1);
    Ok((0..count)
        .map(|i| When {
            arg: "count".into(),
            spec: TimeSpec::Time(lerp(ta, tb, i, last)),
        })
        .collect())
}

/// Whether `w` is past what the stream says it holds, before any seek.
fn check_time(w: &When, info: &VideoStreamInfo) -> Result<(), ToolError> {
    match w.spec {
        TimeSpec::Time(t) => match info.duration {
            Some(d) if t >= d => Err(ToolError::new(
                codes::OUT_OF_RANGE,
                "is past the end of the video",
            )
            .at(&w.arg)
            .valid(format!("under {} s", seconds(d)))),
            _ => Ok(()),
        },
        TimeSpec::Frame(n) => match info.frame_count {
            Some(c) if n >= c => Err(ToolError::new(
                codes::OUT_OF_RANGE,
                format!("the video has {c} frames"),
            )
            .at(&w.arg)
            .valid(format!("0f to {}f", c.saturating_sub(1)))),
            _ => Ok(()),
        },
    }
}

/// Seconds into the clip, near enough to sort times by.
fn order(spec: TimeSpec, info: &VideoStreamInfo) -> f64 {
    match spec {
        TimeSpec::Time(t) => t.to_secs_f64(),
        TimeSpec::Frame(n) => {
            let rate = match info.timing {
                Timing::Constant(r) => Some(r),
                Timing::Variable { average } => average,
            };
            n as f64 / rate.map_or(30.0, |r| r.to_f64())
        }
    }
}

/// Look's words for a still's warning: Spherecut's own words are kept,
/// and the rest, whose messages can quote the file, are said by code.
fn warning(w: &Warning) -> (&'static str, String) {
    if contract::codes::warnings::ALL.contains(&w.code) {
        return (w.code, w.message.clone());
    }
    let message = match w.code {
        "truncated" => "the file is cut short: probe says where",
        "io_error" => "parts of the file couldn't be read",
        _ => "the file is damaged in places: probe with detail \"full\" says where",
    };
    (w.code, message.into())
}

/// `img` letterboxed into a `tile`: its top left, and its size there.
fn fit(size: (u32, u32), tile: (u32, u32)) -> ((u32, u32), (u32, u32)) {
    let s = (f64::from(tile.0) / f64::from(size.0.max(1)))
        .min(f64::from(tile.1) / f64::from(size.1.max(1)));
    let w = ((f64::from(size.0) * s).round() as u32).clamp(1, tile.0);
    let h = ((f64::from(size.1) * s).round() as u32).clamp(1, tile.1);
    (((tile.0 - w) / 2, (tile.1 - h) / 2), (w, h))
}

/// The encoded image.
struct Encoded {
    bytes: Vec<u8>,
    /// JPEG's quantizer scale.
    quality: Option<u8>,
}

/// The base64 length of `n` bytes.
fn b64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// `canvas` as `format`; inline, within `budget` bytes of base64: JPEG's
/// quantizer rises from [`JPEG_QSCALE`] until it fits, up to
/// [`JPEG_QSCALE_MAX`] (amendment 19).
fn encode(canvas: Canvas, format: Format, budget: Option<usize>) -> Result<Encoded, ToolError> {
    let over = |n: usize, budget: usize| {
        ToolError::new(
            codes::IMAGE_TOO_LARGE,
            format!(
                "the image would take {} KB of base64, past the {} KB a result may carry",
                n.div_ceil(1000),
                budget / 1000
            ),
        )
        .at("size")
        .next(
            "a smaller size or fewer tiles; output, a new file under the write root; or ask \
             the user to raise --image-budget-kb",
        )
    };
    match format {
        Format::Png => {
            let bytes = canvas
                .into_image()
                .to_png()
                .map_err(|e| ToolError::internal(format!("PNG: {e}")))?;
            if let Some(b) = budget
                && b64_len(bytes.len()) > b
            {
                return Err(over(b64_len(bytes.len()), b).at("format").next(
                    "format \"jpeg\", a smaller size, or output, a new file under the write root",
                ));
            }
            Ok(Encoded {
                bytes,
                quality: None,
            })
        }
        Format::Jpeg => {
            let steps = [3u8, 4, 5, 6, 8, 10, 12]
                .into_iter()
                .filter(|q| (JPEG_QSCALE..=JPEG_QSCALE_MAX).contains(q));
            let mut last = 0;
            for q in steps {
                let bytes = spherecut_media::encode::encode_jpeg(
                    canvas.rgb(),
                    canvas.width,
                    canvas.height,
                    q,
                )
                .map_err(|e| {
                    tracing::warn!("look: JPEG: {e}");
                    ToolError::new(codes::RENDER_FAILED, "FFmpeg's JPEG encoder failed")
                        .at("format")
                        .next("format \"png\"")
                })?;
                last = b64_len(bytes.len());
                match budget {
                    Some(b) if last > b => continue,
                    _ => {
                        return Ok(Encoded {
                            bytes,
                            quality: Some(q),
                        });
                    }
                }
            }
            Err(over(last, budget.unwrap_or(0)))
        }
    }
}

/// A tile's first label line: `12.512s f375`.
fn when_label(t: &FrameTime) -> String {
    match t.frame {
        Some(n) => format!("{}s f{n}", seconds(t.time)),
        None => format!("{}s", seconds(t.time)),
    }
}

pub fn run(req: Request, cx: &Ctx) -> Result<(Value, Vec<rpc::Image>), ToolError> {
    let plan = plan(&req, cx)?;
    cx.call.check()?;
    let opened = cx.roots.open_file("file", &req.file)?;
    let stop = still::Stop::new(cx.call.token(), cx.call.interrupt());
    let opts = still::Options {
        // The equirect check is mode's: raw renders anything.
        equirect: req.assume_equirect || req.mode == Mode::Raw,
        ..still::Options::default()
    };
    let mut warnings: Vec<Warning> = Vec::new();
    let stopped = |f: &Failure| f.code == codes::CANCELLED;
    let probed = still::probe(opened.source(), &opts, &stop, &mut warnings).map_err(|f| {
        if stopped(&f) {
            halted(cx, 0, 0)
        } else {
            file_failed(&f)
        }
    })?;
    let gpu = gpu()?;
    let streams = match req.mode {
        Mode::Raw => {
            let all = videos(&probed);
            if all.len() > RAW_STREAMS {
                warnings.push(Warning::new(
                    "streams_left_out",
                    format!(
                        "the file has {} video streams: raw shows the first {RAW_STREAMS}",
                        all.len()
                    ),
                ));
            }
            all.into_iter().take(RAW_STREAMS).collect()
        }
        _ => vec![probed.stream.clone()],
    };
    let mut stills = Vec::with_capacity(streams.len());
    for s in streams {
        let p = still::Probed {
            report: probed.report.clone(),
            stream: s,
        };
        let opened_still =
            Still::open(opened.source(), &p, Some(&gpu), &opts, &stop, &mut warnings).map_err(
                |f| {
                    if stopped(&f) {
                        halted(cx, 0, 0)
                    } else {
                        file_failed(&f)
                    }
                },
            )?;
        stills.push(opened_still);
    }
    let Some(first) = stills.first() else {
        return Err(ToolError::new(codes::NO_VIDEO, "has no video stream").at("file"));
    };
    let info = first.info().clone();
    let whens = match &plan.times {
        Times::Given(w) => w.clone(),
        Times::Spread { arg, count, ends } => match ends {
            Some((a, b)) => between(*count, a, b, &info)?,
            None => over_clip(*count, arg, &info)?,
        },
    };
    for w in &whens {
        check_time(w, &info)?;
    }

    // The tiles, in the order the request lists them.
    let mut tiles = Vec::new();
    for t in 0..whens.len() {
        match req.mode {
            Mode::View => tiles.extend(plan.cameras.iter().map(|&c| Tile {
                time: t,
                shows: Shows::Camera(c),
            })),
            Mode::Cube => tiles.extend((0..FACES.len()).map(|f| Tile {
                time: t,
                shows: Shows::Face(f),
            })),
            Mode::Sphere => tiles.push(Tile {
                time: t,
                shows: Shows::Equirect { labelled: true },
            }),
            Mode::Sheet => tiles.push(Tile {
                time: t,
                shows: match plan.cameras.first() {
                    Some(&c) => Shows::Camera(c),
                    None => Shows::Equirect { labelled: false },
                },
            }),
            Mode::Raw => tiles.extend((0..stills.len()).map(|s| Tile {
                time: t,
                shows: Shows::Stream(s),
            })),
        }
    }
    if tiles.len() > MAX_TILES {
        return Err(too_many(tiles.len(), plan.tiles_arg));
    }
    let (aspect, target) = match (req.mode, plan.cameras.is_empty()) {
        (Mode::Cube, _) => (1.0, SCREEN),
        (Mode::Sphere, _) => (2.0, 2.0),
        (Mode::Sheet, true) => (2.0, SCREEN),
        (Mode::Raw, _) => {
            let (w, h) = info.size;
            (f64::from(w.max(1)) / f64::from(h.max(1)), SCREEN)
        }
        _ => (VIEW_ASPECT, SCREEN),
    };
    let n = tiles.len() as u32;
    let Some(layout) = picture::layout(n, aspect, plan.edge, target) else {
        let most = picture::most_tiles(aspect, plan.edge, target, MAX_TILES as u32);
        return Err(ToolError::new(
            codes::OUT_OF_RANGE,
            format!(
                "{n} tiles don't fit at size {}: a tile would be under {} pixels wide",
                plan.edge,
                crate::limits::MIN_TILE_WIDTH
            ),
        )
        .at("size")
        .valid(if most == 0 {
            "a larger size".to_owned()
        } else {
            format!("{most} tiles at most at this size")
        })
        .next("a larger size, or fewer times or views"));
    };

    let mut canvas = Canvas::new(layout.size.0, layout.size.1);
    let mut entries: Vec<Option<Value>> = vec![None; tiles.len()];
    let total = tiles.len();
    let mut done = 0;
    let mut order_of: Vec<usize> = (0..whens.len()).collect();
    order_of.sort_by(|&a, &b| order(whens[a].spec, &info).total_cmp(&order(whens[b].spec, &info)));
    // The frames of the last time decoded, one a still, for a repeat.
    let mut last: Option<(TimeSpec, Vec<Decoded>)> = None;
    for t in order_of {
        let w = &whens[t];
        let decoded = match last.take() {
            Some((spec, d)) if spec == w.spec => d,
            _ => {
                let mut d = Vec::with_capacity(stills.len());
                for s in stills.iter_mut() {
                    cx.call.check().map_err(|_| halted(cx, done, total))?;
                    let frame = s
                        .seek(w.spec, req.seek.mode(), &mut warnings)
                        .map_err(|f| {
                            if stopped(&f) {
                                halted(cx, done, total)
                            } else {
                                time_failed(&f, &w.arg, s.info())
                            }
                        })?;
                    d.push(frame);
                }
                d
            }
        };
        for (i, tile) in tiles.iter().enumerate().filter(|(_, tile)| tile.time == t) {
            cx.call.check().map_err(|_| halted(cx, done, total))?;
            let entry = draw(&mut canvas, &layout, i, tile, &mut stills, &decoded)
                .inspect_err(|_| forget_if_lost(&gpu))?;
            entries[i] = Some(entry);
            done += 1;
            cx.call.progress(done as f64, Some(total as f64), None);
        }
        last = Some((w.spec, decoded));
    }
    drop(stills);

    let budget = req.output.is_none().then_some(cx.image_budget);
    let (width, height) = (canvas.width, canvas.height);
    let encoded = encode(canvas, req.format, budget)?;
    let mut image = Map::new();
    image.insert("width".into(), json!(width));
    image.insert("height".into(), json!(height));
    image.insert("format".into(), json!(req.format.name()));
    image.insert("bytes".into(), json!(encoded.bytes.len()));
    if let Some(q) = encoded.quality {
        image.insert("quality".into(), json!(q));
    }
    let mut images = Vec::new();
    match &req.output {
        Some(out) => {
            let mut file = cx.roots.create("output", out)?;
            let wrote = file.write_all(&encoded.bytes).and_then(|()| file.keep());
            if let Err(e) = wrote {
                tracing::warn!("look: writing the output: {e}");
                return Err(
                    ToolError::new(codes::WRITE_REFUSED, "couldn't be written in full")
                        .at("output")
                        .next("is the disk full? The partial file was removed"),
                );
            }
            image.insert("path".into(), json!(text::field(&out.to_string_lossy())));
        }
        None => images.push(rpc::Image {
            mime: req.format.mime(),
            data: encoded.bytes,
        }),
    }
    let mut seen = BTreeSet::new();
    let warnings: Vec<Value> = warnings
        .iter()
        .map(warning)
        .filter(|w| seen.insert(w.clone()))
        .map(|(code, message)| json!({"code": code, "message": message}))
        .collect();
    let mut result = json!({
        "file": text::field(&req.file.to_string_lossy()),
        "mode": req.mode.name(),
        "seek": req.seek.name(),
        "tiles": entries.into_iter().flatten().collect::<Vec<_>>(),
        "image": image,
        "warnings": warnings,
    });
    // A fallback's reason can carry FFmpeg's words.
    text::untrusted_under(&mut result, "reason");
    Ok((result, images))
}

/// Draw tile `i` of `layout`, its label and its picture, from `decoded`
/// (one frame a still); its entry in the response.
fn draw(
    canvas: &mut Canvas,
    layout: &Layout,
    i: usize,
    tile: &Tile,
    stills: &mut [Still<'_>],
    decoded: &[Decoded],
) -> Result<Value, ToolError> {
    let (row, column) = layout.cell(i);
    let (x, y) = layout.origin(i);
    let at = (x, y + layout.band);
    let size = layout.tile;
    let look = |draw| still::Look {
        draw,
        filter: Filter::Isotropic,
        depth: Depth::Eight,
    };
    let render = |s: &mut Still<'_>, d: &Decoded, cam: &Camera, size, draw| {
        s.render(d, &cam.params(), size, look(draw))
            .map_err(|f| render_failed(&f))
    };
    let stream = match tile.shows {
        Shows::Stream(k) => k,
        _ => 0,
    };
    let (Some(s), Some(d)) = (stills.get_mut(stream), decoded.get(stream)) else {
        return Err(ToolError::internal("a tile without its stream"));
    };
    let mut entry = json!({
        "index": i,
        "row": row,
        "column": column,
        // The tile's picture in the image, below its label.
        "rect": [at.0, at.1, size.0, size.1],
        "time": d.shown,
        "requested": d.requested,
    });
    let line2 = match tile.shows {
        Shows::Camera(cam) => {
            let img = render(s, d, &cam, size, Draw::View)?;
            canvas.put(at.0, at.1, &img);
            entry["view"] = cam.json();
            cam.label()
        }
        Shows::Face(f) => {
            let (name, yaw, pitch) = FACES[f];
            let cam = Camera::face(yaw, pitch);
            let img = render(s, d, &cam, size, Draw::View)?;
            canvas.put(at.0, at.1, &img);
            entry["face"] = json!(name);
            entry["view"] = cam.json();
            name.to_owned()
        }
        Shows::Equirect { labelled } => {
            let [u0, v0, u1, v1] = s.placement().cover();
            let (w, h) = (f64::from(size.0), f64::from(size.1));
            let sub = (
                (((u1 - u0) * w).round() as u32).clamp(1, size.0),
                (((v1 - v0) * h).round() as u32).clamp(1, size.1),
            );
            let img: Image = render(s, d, &Camera::face(0.0, 0.0), sub, Draw::Eye)?;
            canvas.put(
                at.0 + (u0 * w).round() as u32,
                at.1 + (v0 * h).round() as u32,
                &img,
            );
            let alpha = if labelled { 0.5 } else { 0.25 };
            picture::grid(canvas, at, size, alpha, labelled);
            "equirect".to_owned()
        }
        Shows::Stream(_) => {
            let index = s.stream().index;
            let (off, fitted) = fit(s.info().size, size);
            let img = render(s, d, &Camera::face(0.0, 0.0), fitted, Draw::Coded)?;
            canvas.put(at.0 + off.0, at.1 + off.1, &img);
            entry["stream"] = json!(index);
            format!("stream {index}")
        }
    };
    entry["decode"] = serde_json::to_value(s.decode_info(d))
        .map_err(|e| ToolError::internal(format!("decode info: {e}")))?;
    picture::caption(
        canvas,
        (x, y),
        size.0,
        layout.scale,
        [&when_label(&d.shown), &line2],
    );
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn vfov_and_fov_are_inverse_for_every_projection() {
        for p in [
            Projection::Rectilinear,
            Projection::Fisheye,
            Projection::Stereographic,
        ] {
            for aspect in [1.0, 16.0 / 9.0, 2.0] {
                for fov in [10.0, 60.0, 90.0, 150.0] {
                    let v = p.vfov(fov, aspect);
                    assert!(close(p.fov(v, aspect), fov), "{p:?} {aspect} {fov}: {v}");
                }
            }
        }
        // A square tile is as tall as it is wide.
        assert!(close(Projection::Rectilinear.vfov(90.0, 1.0), 90.0));
        // 16:9 at 90° across is 58.7° down, rectilinear.
        let v = Projection::Rectilinear.vfov(90.0, 16.0 / 9.0);
        assert!((v - 58.7155).abs() < 1e-3, "{v}");
        // A fisheye's angle is proportional to the radius.
        assert!(close(Projection::Fisheye.vfov(160.0, 2.0), 80.0));
    }

    #[test]
    fn a_camera_takes_fov_or_vfov_within_the_projection() {
        let v = |fov, vfov, projection| View {
            fov,
            vfov,
            projection,
            ..View::default()
        };
        let c = Camera::new(&v(None, None, Projection::Rectilinear), VIEW_ASPECT, "view")
            .expect("the default camera");
        assert_eq!(c.fov, 90.0);
        let e = Camera::new(
            &v(Some(90.0), Some(50.0), Projection::Rectilinear),
            VIEW_ASPECT,
            "view",
        )
        .err();
        assert_eq!(e.and_then(|e| e.argument), Some("view.vfov".into()));
        let e = Camera::new(
            &v(Some(171.0), None, Projection::Rectilinear),
            VIEW_ASPECT,
            "views[2]",
        )
        .err()
        .map(|e| (e.code, e.argument));
        assert_eq!(e, Some((codes::OUT_OF_RANGE, Some("views[2].fov".into()))));
        assert!(
            Camera::new(
                &v(Some(340.0), None, Projection::Stereographic),
                1.0,
                "view"
            )
            .is_ok()
        );
        // vfov 170 rectilinear on 16:9 is past 170 across.
        let e = Camera::new(
            &v(None, Some(170.0), Projection::Rectilinear),
            VIEW_ASPECT,
            "view",
        )
        .err()
        .map(|e| e.argument);
        assert_eq!(e, Some(Some("view.vfov".into())));
        let c = Camera::new(&v(None, Some(90.0), Projection::Fisheye), 2.0, "view").expect("ok");
        assert!(close(c.fov, 180.0));
    }

    #[test]
    fn labels_are_short() {
        let c = Camera {
            yaw: 90.0,
            pitch: -20.0,
            roll: 0.0,
            fov: 100.0,
            vfov: 70.0,
            projection: Projection::Rectilinear,
        };
        assert_eq!(c.label(), "y90 p-20 f100");
        let c = Camera {
            yaw: 12.5,
            roll: 15.0,
            projection: Projection::Fisheye,
            ..c
        };
        assert_eq!(c.label(), "y12.5 p-20 f100 r15 fish");
        let t = FrameTime {
            time: Time::seconds(Rational::new(12512, 1000).expect("a time")),
            frame: Some(375),
        };
        assert_eq!(when_label(&t), "12.512s f375");
    }

    #[test]
    fn times_parse_as_strings_or_pairs() {
        let s = |v: Value| serde_json::from_value::<TimeArg>(v).expect("a time arg");
        assert_eq!(
            time("t", &s(json!("370f"))).ok(),
            Some(TimeSpec::Frame(370))
        );
        assert_eq!(
            time("t", &s(json!([30000, 1001]))).ok(),
            Some(TimeSpec::Time(Time::seconds(
                Rational::new(30000, 1001).expect("rational")
            )))
        );
        for bad in [
            json!("soon"),
            json!([1]),
            json!([1, 2, 3]),
            json!([1, 0]),
            json!([-1, 2]),
            json!([1.5, 2]),
        ] {
            let e = time("times[1]", &s(bad.clone())).err();
            assert_eq!(
                e.map(|e| (e.code, e.argument)),
                Some((codes::BAD_ARGUMENT, Some("times[1]".into()))),
                "{bad}"
            );
        }
    }

    #[test]
    fn letterboxing_keeps_the_shape() {
        assert_eq!(fit((3840, 3840), (200, 100)), ((50, 0), (100, 100)));
        assert_eq!(fit((1920, 1080), (160, 90)), ((0, 0), (160, 90)));
        assert_eq!(fit((100, 400), (100, 100)), ((37, 0), (25, 100)));
    }

    #[test]
    fn a_stop_says_how_far_the_call_got() {
        let roots = crate::Roots::new(&[], None).expect("roots");
        let call =
            rpc::CallCx::standalone(std::time::Instant::now() + std::time::Duration::from_secs(60));
        let cx = Ctx {
            roots: &roots,
            call: &call,
            image_budget: crate::limits::INLINE_IMAGE_BYTES,
        };
        let e = halted(&cx, 3, 24);
        assert_eq!(e.code, codes::CANCELLED, "not stopped, so stopped by still");
        call.expire();
        let e = halted(&cx, 3, 24);
        assert_eq!(e.code, codes::TIMED_OUT);
        assert!(e.message.ends_with("after 3 of 24 tiles"), "{}", e.message);
        assert_eq!(e.next.as_deref(), Some(FASTER));
        let e = halted(&cx, 0, 0);
        assert!(!e.message.contains("tiles"), "{}", e.message);
    }

    #[test]
    fn spreads_include_both_ends() {
        let a = Time::ZERO;
        let b = Time::from_secs(10);
        let t: Vec<f64> = (0..5).map(|i| lerp(a, b, i, 4).to_secs_f64()).collect();
        assert_eq!(t, vec![0.0, 2.5, 5.0, 7.5, 10.0]);
    }
}
