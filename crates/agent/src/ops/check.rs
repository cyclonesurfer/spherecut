//! `check` (a1-plan section 6.4, D46): a file's spherical and ambisonic
//! metadata against its streams, and the file against a target.
//!
//! Every call runs the rules that always hold (nothing damaged, the
//! projection fits the picture, V1 and V2 agree, SA3D fits its track)
//! where they apply, then the target's. A rule is `{id, ok, message,
//! fix}`, `fix` only when it failed; `pass` is every rule's `ok`. What a
//! target recommends without requiring goes in `advice`, and only when
//! the file doesn't follow it. YouTube's rules are from its help pages as
//! fetched on 2026-10-30 (`docs/research/youtube-360-2026-10.md`).
//!
//! The words are ours: no file name, FFmpeg text or tag reaches a
//! message (rule 4.2). Fixes are plain words, never a command; M2's
//! exports will be named in them once they exist.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spherecut_export::{ExportJob, Planned};
use spherecut_media::detect::Layout;
use spherecut_media::probe::{
    ProbeReport, StreamInfo, StreamKind, VideoInfo, codes as probe_codes,
};
use spherecut_media::spherical::{Projection, SpatialAudio, StereoMode};

use super::{Ctx, sequence};
use crate::error::{ToolError, codes};
use crate::limits::CHECK_BYTES;
use crate::text;

/// What the file is checked for, beyond the rules that always hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Target {
    /// A 360 upload to YouTube.
    #[serde(rename = "youtube_360")]
    Youtube360,
    /// A flat landscape video.
    #[serde(rename = "flat_16_9")]
    Flat16x9,
    /// A flat portrait video.
    #[serde(rename = "vertical")]
    Vertical,
    /// A copy to keep in place of the camera's original.
    #[serde(rename = "archive")]
    Archive,
}

impl Target {
    fn name(self) -> &'static str {
        match self {
            Target::Youtube360 => "youtube_360",
            Target::Flat16x9 => "flat_16_9",
            Target::Vertical => "vertical",
            Target::Archive => "archive",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub file: Option<PathBuf>,
    /// Instead of `file`: the file a render of the project would make.
    pub project: Option<PathBuf>,
    pub target: Option<Target>,
}

/// One rule's outcome.
#[derive(Debug, Clone, Serialize)]
struct Rule {
    id: &'static str,
    ok: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

impl Rule {
    fn ok(id: &'static str, message: impl Into<String>) -> Self {
        Rule {
            id,
            ok: true,
            message: message.into(),
            fix: None,
        }
    }

    fn fail(id: &'static str, message: impl Into<String>, fix: impl Into<String>) -> Self {
        Rule {
            id,
            ok: false,
            message: message.into(),
            fix: Some(fix.into()),
        }
    }
}

/// A recommendation the file doesn't follow.
#[derive(Debug, Clone, Serialize)]
struct Advice {
    id: &'static str,
    message: String,
}

fn advice(id: &'static str, message: impl Into<String>) -> Advice {
    Advice {
        id,
        message: message.into(),
    }
}

pub fn run(req: Request, cx: &Ctx) -> Result<Value, ToolError> {
    let path = match (req.file, req.project) {
        (Some(_), Some(_)) => {
            return Err(ToolError::new(codes::BAD_ARGUMENT, "can't come with file")
                .at("project")
                .valid("file, or project, not both"));
        }
        (None, None) => {
            return Err(ToolError::new(codes::BAD_ARGUMENT, "is missing")
                .at("file")
                .valid("a video as file, or a .spherecut file as project"));
        }
        (None, Some(p)) => return run_project(&p, req.target, cx),
        (Some(f), None) => f,
    };
    let opened = cx.roots.open_file("file", &path)?;
    let report =
        match spherecut_media::probe::probe_cancellable(opened.source(), cx.call.interrupt()) {
            Ok(r) => r,
            Err(e) => {
                cx.call.check()?;
                // A file FFmpeg can't open for its damage fails `readable`,
                // the one rule that can be checked; any other failure is the
                // call's.
                let rule = match e.code() {
                    "truncated" => readable_failure(probe_codes::TRUNCATED),
                    "damaged" | "open_failed" => readable_failure(probe_codes::DAMAGED),
                    code => {
                        let (code, message) = super::probe::words(code);
                        return Err(ToolError::new(code, message)
                            .at("file")
                            .next("probe it with detail \"full\" for FFmpeg's own words"));
                    }
                };
                return Ok(response(req.target, &[rule], &[]));
            }
        };
    cx.call.check()?;
    let file = File::of(&report, &path);
    let (rules, tips) = rules_of(&file, req.target);
    Ok(response(req.target, &rules, &tips))
}

/// The rules that always hold, then `target`'s; and its advice.
fn rules_of(file: &File, target: Option<Target>) -> (Vec<Rule>, Vec<Advice>) {
    let mut rules = always(file);
    let mut tips = Vec::new();
    match target {
        None => {}
        Some(Target::Youtube360) => youtube_360(file, &mut rules, &mut tips),
        Some(t @ (Target::Flat16x9 | Target::Vertical)) => flat(file, t, &mut rules),
        Some(Target::Archive) => archive(file, &mut rules, &mut tips),
    }
    (rules, tips)
}

/// The response, cut to [`CHECK_BYTES`]: the passed rules' messages go
/// first, then the advice, each with a warning that says so. The failed
/// rules always stay whole.
fn response(target: Option<Target>, rules: &[Rule], tips: &[Advice]) -> Value {
    let pass = rules.iter().all(|r| r.ok);
    let make = |rules: &[Rule], tips: &[Advice], warnings: Vec<Value>| {
        json!({
            "target": target.map(Target::name),
            "pass": pass,
            "rules": rules,
            "advice": tips,
            "warnings": warnings,
        })
    };
    let fits = |v: &Value| v.to_string().len() <= CHECK_BYTES;
    let out = make(rules, tips, Vec::new());
    if fits(&out) {
        return out;
    }
    let cut = |what: String| {
        json!({
            "code": "result_cut",
            "message": format!("{what} left out to fit {CHECK_BYTES} bytes"),
        })
    };
    let terse: Vec<Rule> = rules
        .iter()
        .map(|r| {
            let mut r = r.clone();
            if r.ok {
                r.message.clear();
            }
            r
        })
        .collect();
    let out = make(
        &terse,
        tips,
        vec![cut("the passed rules' messages were".into())],
    );
    if fits(&out) {
        return out;
    }
    make(
        &terse,
        &[],
        vec![cut(format!(
            "the passed rules' messages and {} advice were",
            tips.len()
        ))],
    )
}

/// What the rules read from a probe, found once.
struct File<'a> {
    r: &'a ProbeReport,
    /// The video streams, cover art aside.
    video: Vec<&'a StreamInfo>,
    audio: Vec<&'a StreamInfo>,
    container: Container,
    /// The file name's extension, lowercase.
    ext: String,
}

impl<'a> File<'a> {
    fn of(r: &'a ProbeReport, path: &std::path::Path) -> Self {
        let video = r
            .streams
            .iter()
            .filter(|s| {
                s.kind == StreamKind::Video
                    && s.video.is_some()
                    && !s.disposition.iter().any(|d| d == "attached_pic")
            })
            .collect();
        let audio = r
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Audio && s.audio.is_some())
            .collect();
        File {
            r,
            video,
            audio,
            container: Container::of(r),
            ext: path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
        }
    }

    /// The first video stream, the one players show.
    fn first(&self) -> Option<&'a StreamInfo> {
        self.video.first().copied()
    }

    fn layout(&self) -> Layout {
        self.r.camera.as_ref().map_or(Layout::Unknown, |c| c.layout)
    }

    fn vendor(&self) -> Option<&str> {
        self.r.camera.as_ref().and_then(|c| c.vendor.as_deref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    Mp4,
    Mov,
    /// Matroska whose streams are all WebM's codecs.
    Webm,
    Matroska,
    Other,
}

impl Container {
    fn of(r: &ProbeReport) -> Self {
        let format = r.container.format.as_str();
        if format.starts_with("mov,") || format == "mov" || format == "mp4" {
            // QuickTime files from before `ftyp` have none.
            return match r.mp4.as_ref().and_then(|m| m.brand.as_deref()) {
                Some("qt  ") | None => Container::Mov,
                Some(_) => Container::Mp4,
            };
        }
        if format.starts_with("matroska") || format == "webm" {
            let webm = r.streams.iter().all(|s| match s.kind {
                StreamKind::Video => matches!(s.codec.as_deref(), Some("vp8" | "vp9" | "av1")),
                StreamKind::Audio => matches!(s.codec.as_deref(), Some("vorbis" | "opus")),
                _ => true,
            });
            return if webm {
                Container::Webm
            } else {
                Container::Matroska
            };
        }
        Container::Other
    }

    fn name(self) -> &'static str {
        match self {
            Container::Mp4 => "MP4",
            Container::Mov => "MOV",
            Container::Webm => "WebM",
            Container::Matroska => "Matroska",
            Container::Other => "this container",
        }
    }
}

/// A stream's projection and stereo layout, from V2 where it has one,
/// else from V1.
#[derive(Debug, Clone, PartialEq)]
struct Sphere {
    projection: Shape,
    stereo: StereoMode,
}

#[derive(Debug, Clone, PartialEq)]
enum Shape {
    /// The share of the sphere the picture covers, across and down.
    Equirect {
        across: f64,
        down: f64,
        valid: bool,
    },
    Cubemap {
        layout: u32,
    },
    /// A mesh, or a projection V1 or V2 doesn't define.
    Other,
}

const FIXED_ONE: f64 = 4_294_967_296.0;

fn sphere(s: &StreamInfo) -> Option<Sphere> {
    let sph = &s.spherical;
    if let Some(p) = sph.v2.as_ref().and_then(|v2| v2.projection.as_ref()) {
        let projection = match p {
            Projection::Equirect {
                top,
                bottom,
                left,
                right,
            } => {
                let (t, b, l, r) = (
                    u64::from(*top),
                    u64::from(*bottom),
                    u64::from(*left),
                    u64::from(*right),
                );
                // The specification: bottom < 0xFFFFFFFF − top, and
                // likewise left and right.
                let valid = t + b < 0xFFFF_FFFF && l + r < 0xFFFF_FFFF;
                Shape::Equirect {
                    across: 1.0 - (l + r) as f64 / FIXED_ONE,
                    down: 1.0 - (t + b) as f64 / FIXED_ONE,
                    valid,
                }
            }
            Projection::Cubemap { layout, .. } => Shape::Cubemap { layout: *layout },
            Projection::Mesh | Projection::Other { .. } => Shape::Other,
        };
        let stereo = sph
            .v2
            .as_ref()
            .and_then(|v2| v2.stereo)
            .or_else(|| v1_stereo(s))
            .unwrap_or(StereoMode::Mono);
        return Some(Sphere { projection, stereo });
    }
    let v1 = sph.v1.as_ref().filter(|v1| v1.spherical)?;
    let projection = match v1.projection_type.as_deref().map(str::to_ascii_lowercase) {
        None => Shape::Equirect {
            across: 1.0,
            down: 1.0,
            valid: true,
        },
        Some(p) if p == "equirectangular" => Shape::Equirect {
            across: 1.0,
            down: 1.0,
            valid: true,
        },
        Some(_) => Shape::Other,
    };
    let stereo = sph
        .v2
        .as_ref()
        .and_then(|v2| v2.stereo)
        .or_else(|| v1_stereo(s))
        .unwrap_or(StereoMode::Mono);
    Some(Sphere { projection, stereo })
}

/// V1's `StereoMode`, as V2's: `None` for none, or for a word V1 doesn't
/// define (`StereoCustom` stands for those).
fn v1_stereo(s: &StreamInfo) -> Option<StereoMode> {
    let mode = s.spherical.v1.as_ref()?.stereo_mode.as_deref()?;
    Some(match mode.to_ascii_lowercase().as_str() {
        "mono" => StereoMode::Mono,
        "top-bottom" => StereoMode::TopBottom,
        "left-right" => StereoMode::LeftRight,
        _ => StereoMode::StereoCustom,
    })
}

/// The picture's width and height as shown on its pixels' aspect, before
/// any rotation.
fn picture(v: &VideoInfo) -> Option<(f64, f64)> {
    if v.width == 0 || v.height == 0 {
        return None;
    }
    let sar = v
        .sample_aspect
        .map(|r| r.to_f64())
        .filter(|f| f.is_finite() && *f > 0.0)
        .unwrap_or(1.0);
    Some((f64::from(v.width) * sar, f64::from(v.height)))
}

/// Within 1 %.
fn near(a: f64, b: f64) -> bool {
    (a / b - 1.0).abs() <= 0.01
}

/// An aspect as people write it: `16:9`, or `1.33:1`.
fn ratio(a: f64) -> String {
    const NAMED: &[(f64, &str)] = &[
        (16.0 / 9.0, "16:9"),
        (9.0 / 16.0, "9:16"),
        (2.0, "2:1"),
        (1.0, "1:1"),
        (4.0, "4:1"),
        (0.5, "1:2"),
        (1.5, "3:2"),
        (4.0 / 3.0, "4:3"),
    ];
    NAMED
        .iter()
        .find(|(v, _)| (a / v - 1.0).abs() <= 0.005)
        .map_or_else(|| format!("{a:.2}:1"), |(_, n)| (*n).to_owned())
}

fn stereo_words(s: StereoMode) -> &'static str {
    match s {
        StereoMode::Mono => "mono",
        StereoMode::TopBottom => "top-bottom stereo",
        StereoMode::LeftRight => "left-right stereo",
        StereoMode::StereoCustom => "custom stereo",
        StereoMode::Unknown(4) => "right-left stereo",
        StereoMode::Unknown(_) => "an unknown stereo mode",
    }
}

/// One eye's share of the picture, across and down; `None` for a layout
/// the file defines itself.
fn eye(s: StereoMode) -> Option<(f64, f64)> {
    match s {
        StereoMode::Mono => Some((1.0, 1.0)),
        StereoMode::TopBottom => Some((1.0, 0.5)),
        // V2's value 4 is right-left, the eyes swapped.
        StereoMode::LeftRight | StereoMode::Unknown(4) => Some((0.5, 1.0)),
        StereoMode::StereoCustom | StereoMode::Unknown(_) => None,
    }
}

// --- The rules that always hold.

fn always(f: &File) -> Vec<Rule> {
    let mut rules = vec![readable(f.r)];
    let tagged: Vec<(&StreamInfo, Sphere)> = f
        .video
        .iter()
        .filter_map(|s| sphere(s).map(|sp| (*s, sp)))
        .collect();
    if !tagged.is_empty() {
        rules.push(first_failure(
            tagged.iter().map(|(s, sp)| projection_fits(f, s, sp)),
        ));
    }
    let both: Vec<&StreamInfo> = f
        .video
        .iter()
        .copied()
        .filter(|s| {
            s.spherical.v2.is_some() && s.spherical.v1.as_ref().is_some_and(|v1| v1.spherical)
        })
        .collect();
    if !both.is_empty() {
        rules.push(first_failure(both.iter().map(|s| v1_v2_agree(f, s))));
    }
    let spatial: Vec<(&StreamInfo, &SpatialAudio)> = f
        .audio
        .iter()
        .filter_map(|s| s.spherical.spatial_audio.as_ref().map(|sa| (*s, sa)))
        .collect();
    if !spatial.is_empty() {
        rules.push(first_failure(
            spatial.iter().map(|(s, sa)| spatial_audio_fits(f, s, sa)),
        ));
    }
    rules
}

/// The first rule of these that failed, else the first. Never given
/// none.
fn first_failure(rules: impl Iterator<Item = Rule>) -> Rule {
    let mut first = None;
    for r in rules {
        if !r.ok {
            return r;
        }
        first.get_or_insert(r);
    }
    first.unwrap_or_else(|| Rule::ok("readable", ""))
}

/// `stream N: ` when the file has more than one stream of the kind.
fn which(f: &File, s: &StreamInfo) -> String {
    let n = if s.kind == StreamKind::Audio {
        f.audio.len()
    } else {
        f.video.len()
    };
    if n > 1 {
        format!("stream {}: ", s.index)
    } else {
        String::new()
    }
}

fn readable(r: &ProbeReport) -> Rule {
    let Some(failure) = r.failure() else {
        use spherecut_media::probe::StructureCheck as S;
        let how = match &r.structure {
            S::Full => "nothing is cut short or damaged",
            S::Length => "nothing is cut short (in Matroska, only its length is checked)",
            S::None { .. } => "it opens, but its structure couldn't be checked",
        };
        return Rule::ok("readable", how);
    };
    readable_failure(failure.code)
}

/// `readable` failed with probe's code.
fn readable_failure(code: &str) -> Rule {
    let (message, fix) = match code {
        probe_codes::TRUNCATED => (
            "it is cut short: it ends inside its structure",
            "copy it again from the camera or card; a recording cut short can't be completed here",
        ),
        probe_codes::NO_VIDEO_OR_AUDIO => (
            "it has no video or audio stream",
            "check that this is the file meant: there is nothing in it to play",
        ),
        _ => (
            "it is damaged",
            "copy it again from its source; probe with detail \"full\" says what is wrong",
        ),
    };
    Rule::fail("readable", message, fix)
}

fn projection_fits(f: &File, s: &StreamInfo, sp: &Sphere) -> Rule {
    const ID: &str = "projection_fits_picture";
    let at = which(f, s);
    let Some((w, h)) = s.video.as_ref().and_then(picture) else {
        return Rule::fail(
            ID,
            format!("{at}the picture has no size"),
            "probe it: the stream may be damaged",
        );
    };
    let expect = match sp.projection {
        Shape::Equirect { valid: false, .. } => {
            return Rule::fail(
                ID,
                format!("{at}its equirect bounds crop away the whole picture"),
                "write the bounds again: all zero for a full sphere",
            );
        }
        Shape::Equirect { across, down, .. } => 2.0 * across / down,
        Shape::Cubemap { layout: 0 } => 1.5,
        Shape::Cubemap { layout } => {
            return Rule::fail(
                ID,
                format!("{at}cubemap layout {layout} isn't one the specification defines"),
                "write cubemap layout 0, the only one defined, or export as equirect",
            );
        }
        Shape::Other => {
            return Rule::ok(ID, format!("{at}a projection whose shape isn't checked"));
        }
    };
    let shape = match sp.projection {
        Shape::Cubemap { .. } => "cubemap",
        _ => "equirect",
    };
    let Some((ex, ey)) = eye(sp.stereo) else {
        return Rule::ok(
            ID,
            format!("{at}custom stereo: the picture's layout is the file's own"),
        );
    };
    let got = (w * ex) / (h * ey);
    if near(got, expect) {
        return Rule::ok(
            ID,
            format!(
                "{at}{} {shape}, {} per eye as its metadata says",
                stereo_words(sp.stereo),
                ratio(expect)
            ),
        );
    }
    let picture = w / h;
    let fits = [
        StereoMode::Mono,
        StereoMode::TopBottom,
        StereoMode::LeftRight,
    ]
    .into_iter()
    .find(|m| eye(*m).is_some_and(|(x, y)| near(picture * x / y, expect)));
    let message = format!(
        "{at}the picture is {}x{} ({}), but {} {shape} is {} per eye",
        s.video.as_ref().map_or(0, |v| v.width),
        s.video.as_ref().map_or(0, |v| v.height),
        ratio(picture),
        stereo_words(sp.stereo),
        ratio(expect),
    );
    let fix = match fits {
        Some(m) => format!(
            "if it is {}, write that in st3d; else export it again whole",
            stereo_words(m)
        ),
        None => "the picture was cropped or scaled: export it again whole".to_owned(),
    };
    Rule::fail(ID, message, fix)
}

fn v1_v2_agree(f: &File, s: &StreamInfo) -> Rule {
    const ID: &str = "v1_v2_agree";
    let at = which(f, s);
    let fix = "players read V2 first: drop V1's box, or make it say the same";
    let (Some(v1), Some(v2)) = (s.spherical.v1.as_ref(), s.spherical.v2.as_ref()) else {
        return Rule::ok(ID, format!("{at}only one of V1 and V2 is there"));
    };
    let v1_equirect = v1
        .projection_type
        .as_deref()
        .is_none_or(|p| p.eq_ignore_ascii_case("equirectangular"));
    let v2_equirect = matches!(v2.projection, Some(Projection::Equirect { .. }) | None);
    if v1_equirect != v2_equirect {
        return Rule::fail(ID, format!("{at}V1 and V2 name different projections"), fix);
    }
    let one = v1_stereo(s).unwrap_or(StereoMode::Mono);
    let two = v2.stereo.unwrap_or(StereoMode::Mono);
    if one != two {
        return Rule::fail(
            ID,
            format!(
                "{at}V1 says {} and V2 says {}",
                stereo_words(one),
                stereo_words(two)
            ),
            fix,
        );
    }
    Rule::ok(ID, format!("{at}V1 and V2 say the same"))
}

fn ambix_words(sa: &SpatialAudio) -> String {
    format!(
        "{} order ambiX{}",
        super::probe::ordinal(sa.ambisonic_order),
        if sa.head_locked_stereo {
            " with head-locked stereo"
        } else {
            ""
        }
    )
}

fn spatial_audio_fits(f: &File, s: &StreamInfo, sa: &SpatialAudio) -> Rule {
    const ID: &str = "spatial_audio_fits";
    let at = which(f, s);
    if !sa.is_ambix() {
        let what = if sa.ambisonic_type & 0x7f != 0 {
            "isn't periphonic"
        } else if sa.channel_ordering != 0 {
            "isn't in ACN order"
        } else {
            "isn't SN3D"
        };
        return Rule::fail(
            ID,
            format!("{at}SA3D's ambisonics {what}, so it isn't ambiX"),
            "write SA3D as ambiX (ACN, SN3D, periphonic), with the channels to match",
        );
    }
    let channels = s.audio.as_ref().map_or(0, |a| a.channels);
    let want = sa.expected_channels();
    if want != Some(u64::from(channels)) {
        return Rule::fail(
            ID,
            format!(
                "{at}SA3D says {}, {} channels, but the track has {channels}",
                ambix_words(sa),
                want.map_or_else(|| "too many".to_owned(), |n| n.to_string())
            ),
            "this track isn't the mix SA3D describes: drop SA3D, or export the ambiX mix",
        );
    }
    if sa.channel_map.len() != channels as usize {
        return Rule::fail(
            ID,
            format!(
                "{at}SA3D maps {} channels, but the track has {channels}",
                sa.channel_map.len()
            ),
            "write SA3D's channel map with one entry for each channel",
        );
    }
    Rule::ok(
        ID,
        format!("{at}{} in {channels} channels", ambix_words(sa)),
    )
}

// --- youtube_360.

/// YouTube's supported formats (`docs/research/youtube-360-2026-10.md`
/// [4]) as FFmpeg's codec names: ProRes, DNxHR, CineForm and HEVC by
/// name, H.264 and MPEG-4 Part 2 for MP4 and MPEG-4, MPEG-1 and MPEG-2,
/// and WebM's VP8, VP9 and AV1.
const YOUTUBE_VIDEO: &[&str] = &[
    "h264",
    "hevc",
    "vp8",
    "vp9",
    "av1",
    "prores",
    "dnxhd",
    "cfhd",
    "mpeg1video",
    "mpeg2video",
    "mpeg4",
];

/// The frame rates YouTube calls common, with their NTSC forms.
const COMMON_RATES: &[f64] = &[24.0, 25.0, 30.0, 48.0, 50.0, 60.0];

fn youtube_360(f: &File, rules: &mut Vec<Rule>, tips: &mut Vec<Advice>) {
    let Some(v) = f.first() else {
        rules.push(Rule::fail(
            "spherical_metadata",
            "it has no video stream",
            "check a video file",
        ));
        return;
    };
    match sphere(v) {
        None => rules.push(no_sphere(f)),
        Some(sp) => {
            rules.push(Rule::ok(
                "spherical_metadata",
                if v.spherical
                    .v2
                    .as_ref()
                    .is_some_and(|v2| v2.projection.is_some())
                {
                    "Spherical Video V2"
                } else {
                    "Spherical Video V1"
                },
            ));
            rules.push(match sp.projection {
                Shape::Equirect { .. } => Rule::ok("equirect", "equirect"),
                Shape::Cubemap { .. } => Rule::fail(
                    "equirect",
                    "it is a cubemap; YouTube's pages name only equirect for 360",
                    "export it as equirect",
                ),
                Shape::Other => Rule::fail(
                    "equirect",
                    "its projection isn't equirect",
                    "export it as equirect",
                ),
            });
        }
    }
    rules.push(match f.container {
        Container::Mp4 | Container::Mov | Container::Webm => {
            Rule::ok("container", f.container.name())
        }
        Container::Matroska => Rule::fail(
            "container",
            "Matroska (.mkv) isn't on YouTube's list of formats",
            "put the same streams in an MP4; nothing needs encoding again",
        ),
        Container::Other => Rule::fail(
            "container",
            "its container isn't on YouTube's list of formats",
            "export it as MP4",
        ),
    });
    let codec = v.codec.as_deref().unwrap_or("");
    rules.push(if YOUTUBE_VIDEO.contains(&codec) {
        Rule::ok("video_codec", text::field(codec))
    } else {
        Rule::fail(
            "video_codec",
            format!(
                "{} isn't a codec YouTube lists",
                if codec.is_empty() {
                    "its codec".to_owned()
                } else {
                    text::field(codec)
                }
            ),
            "export it as H.264 or HEVC",
        )
    });
    let interlaced = v
        .video
        .as_ref()
        .and_then(|i| i.field_order.as_deref())
        .is_some_and(|o| matches!(o, "tt" | "bb" | "tb" | "bt"));
    rules.push(if interlaced {
        Rule::fail(
            "progressive",
            "it is interlaced",
            "deinterlace it when exporting, as YouTube asks",
        )
    } else {
        Rule::ok("progressive", "progressive")
    });
    let spatial = f
        .audio
        .iter()
        .find_map(|s| s.spherical.spatial_audio.as_ref().map(|sa| (*s, sa)));
    if let Some((s, sa)) = spatial {
        rules.push(youtube_spatial(f, s, sa));
        rules.push(if f.audio.len() == 1 {
            Rule::ok("one_audio_track", "one audio track")
        } else {
            Rule::fail(
                "one_audio_track",
                format!(
                    "it has {} audio tracks; YouTube uses one, and not spatial beside stereo",
                    f.audio.len()
                ),
                "keep only the ambiX track",
            )
        });
    }
    youtube_advice(f, v, spatial.is_some(), tips);
}

/// `spherical_metadata` failed: the fix depends on what the picture is.
fn no_sphere(f: &File) -> Rule {
    let (message, fix) = match f.layout() {
        Layout::Insta360DualFisheye
        | Layout::DjiDualFisheye
        | Layout::GoProEac
        | Layout::LowResCompanion => (
            "it is the camera's own unstitched recording, with no spherical metadata",
            "stitch it to equirect first (the camera maker's app does, and tags it)",
        ),
        Layout::EquirectLikely => (
            "it is 2:1 but has no spherical metadata, so YouTube would show it flat",
            "add Spherical Video V2 metadata (st3d, sv3d), as Google's spatial-media injector does",
        ),
        _ => (
            "it has no spherical metadata, so YouTube would show it flat",
            "export it as 360: equirect with Spherical Video V2 metadata",
        ),
    };
    Rule::fail("spherical_metadata", message, fix)
}

fn kbps(b: i64) -> String {
    format!("{} kb/s", b / 1000)
}

fn youtube_spatial(f: &File, s: &StreamInfo, sa: &SpatialAudio) -> Rule {
    const ID: &str = "spatial_audio_youtube";
    let Some(a) = s.audio.as_ref() else {
        return Rule::fail(ID, "its spatial audio track has no format", "probe it");
    };
    if !sa.is_ambix() || sa.ambisonic_order != 1 {
        return Rule::fail(
            ID,
            "YouTube takes first-order ambiX only",
            "export the ambiX mix at first order",
        );
    }
    let want = if sa.head_locked_stereo { 6 } else { 4 };
    if a.channels != want {
        return Rule::fail(
            ID,
            format!(
                "YouTube takes {want} channels for this; the track has {}",
                a.channels
            ),
            "export the ambiX mix with its own channel count",
        );
    }
    if a.sample_rate != 48_000 {
        return Rule::fail(
            ID,
            format!(
                "it is {} Hz; YouTube's spatial audio is 48 kHz",
                a.sample_rate
            ),
            "export the audio at 48 kHz",
        );
    }
    let codec = s.codec.as_deref().unwrap_or("");
    let rate = s.bit_rate;
    let (container_ok, least, what) = match codec {
        "aac" => (
            matches!(f.container, Container::Mp4 | Container::Mov),
            Some(256_000),
            "AAC in MP4 or MOV",
        ),
        "opus" => {
            if f.container == Container::Mp4
                && let Some(family) = a.opus_mapping_family.filter(|m| *m != 2)
            {
                return Rule::fail(
                    ID,
                    format!("its Opus uses channel mapping family {family}; YouTube asks for 2"),
                    "encode the Opus with mapping family 2 (ambisonics)",
                );
            }
            (
                f.container == Container::Mp4,
                Some(if sa.head_locked_stereo {
                    768_000
                } else {
                    512_000
                }),
                "Opus in MP4",
            )
        }
        c if c.starts_with("pcm_") => (f.container == Container::Mov, None, "PCM in MOV"),
        _ => {
            return Rule::fail(
                ID,
                "YouTube takes spatial audio as AAC, Opus or PCM",
                "export it as AAC at 256 kb/s or more",
            );
        }
    };
    if !container_ok {
        return Rule::fail(
            ID,
            format!(
                "YouTube takes {what} for spatial audio, not in {}",
                f.container.name()
            ),
            format!("export it as {what}"),
        );
    }
    let codec = match codec {
        "aac" => "AAC",
        "opus" => "Opus",
        _ => "PCM",
    };
    let held = format!("{codec} in {}, {want} channels, 48 kHz", f.container.name());
    // An encoder set to the least rate makes a hair under it on average
    // (255 kb/s for 256), so 1 % under passes.
    match (least, rate) {
        (Some(least), Some(rate)) if rate * 100 < least * 99 => Rule::fail(
            ID,
            format!("it is {}; YouTube asks {} or more", kbps(rate), kbps(least)),
            format!("export the audio at {} or more", kbps(least)),
        ),
        (Some(_), None) => Rule::ok(
            ID,
            format!("{held}; its bit rate isn't recorded, so it isn't checked"),
        ),
        (Some(_), Some(rate)) => Rule::ok(ID, format!("{held}, {}", kbps(rate))),
        (None, _) => Rule::ok(ID, held),
    }
}

fn youtube_advice(f: &File, v: &StreamInfo, spatial: bool, tips: &mut Vec<Advice>) {
    let codec = v.codec.as_deref().unwrap_or("");
    if codec != "h264" && YOUTUBE_VIDEO.contains(&codec) {
        tips.push(advice("h264", "YouTube recommends H.264"));
    }
    let info = v.video.as_ref();
    if info
        .and_then(|i| i.chroma_subsampling)
        .is_some_and(|c| c != (1, 1))
    {
        tips.push(advice("chroma_420", "YouTube recommends 4:2:0 chroma"));
    }
    if let Some(i) = info {
        let hdr = matches!(
            i.color.transfer.as_deref(),
            Some("smpte2084" | "arib-std-b67")
        );
        let matrix = i.color.matrix.as_deref().filter(|m| *m != "unknown");
        if !hdr && matrix.is_some_and(|m| m != "bt709") {
            tips.push(advice("bt709", "YouTube recommends BT.709 for SDR"));
        }
    }
    let fps = v
        .avg_frame_rate
        .or(v.r_frame_rate)
        .map(|r| r.to_f64())
        .filter(|f| f.is_finite() && *f > 0.0);
    if let Some(fps) = fps {
        let common = COMMON_RATES.iter().any(|r| {
            (fps / r - 1.0).abs() <= 0.001 || (fps / (r * 1000.0 / 1001.0) - 1.0).abs() <= 0.001
        });
        if !common {
            let fps = if (fps - fps.round()).abs() < 0.0005 {
                format!("{}", fps.round())
            } else {
                format!("{fps:.3}")
            };
            tips.push(advice(
                "frame_rate",
                format!(
                    "{fps} fps isn't one of YouTube's common rates (24, 25, 30, 48, 50, 60), though it takes others"
                ),
            ));
        }
    }
    if let Some(a) = f.audio.first() {
        let aac_lc =
            a.codec.as_deref() == Some("aac") && a.profile.as_deref().is_none_or(|p| p == "LC");
        if !aac_lc && a.codec.as_deref() != Some("opus") {
            tips.push(advice("audio_codec", "YouTube recommends AAC-LC or Opus"));
        }
        // Spatial audio's rule already asks for 48 kHz.
        if !spatial && a.audio.as_ref().is_some_and(|i| i.sample_rate != 48_000) {
            tips.push(advice("sample_rate", "YouTube recommends 48 kHz audio"));
        }
    }
}

// --- flat_16_9 and vertical.

fn flat(f: &File, target: Target, rules: &mut Vec<Rule>) {
    let tagged = f.video.iter().any(|s| sphere(s).is_some());
    rules.push(if tagged {
        Rule::fail(
            "not_spherical",
            "it has spherical metadata, so a player would wrap it round a sphere",
            "reframe it to a flat export, or drop sv3d if the picture is already flat",
        )
    } else {
        Rule::ok("not_spherical", "no spherical metadata")
    });
    let (want, name, sizes) = match target {
        Target::Vertical => (9.0 / 16.0, "9:16", "1080x1920 or 2160x3840"),
        _ => (16.0 / 9.0, "16:9", "1920x1080 or 3840x2160"),
    };
    let Some(info) = f.first().and_then(|v| v.video.as_ref()) else {
        rules.push(Rule::fail(
            "aspect",
            "it has no video stream",
            "check a video file",
        ));
        return;
    };
    match picture(info) {
        None => rules.push(Rule::fail(
            "aspect",
            "the picture has no size",
            "probe it: the stream may be damaged",
        )),
        Some((w, h)) => {
            let quarter = info
                .rotation_deg
                .is_some_and(|d| ((d / 90.0).round() as i64).rem_euclid(2) == 1);
            let (w, h) = if quarter { (h, w) } else { (w, h) };
            let shown = w / h;
            rules.push(if near(shown, want) {
                Rule::ok("aspect", format!("shown at {name}"))
            } else {
                Rule::fail(
                    "aspect",
                    format!(
                        "it is shown at {}{}, not {name}",
                        ratio(shown),
                        if quarter { ", rotated" } else { "" }
                    ),
                    format!("export it at {name} ({sizes}), reframed or cropped"),
                )
            });
        }
    }
    let square = info
        .sample_aspect
        .is_none_or(|r| r.to_f64() == 1.0 || !r.is_positive());
    rules.push(if square {
        Rule::ok("square_pixels", "square pixels")
    } else {
        Rule::fail(
            "square_pixels",
            format!(
                "its pixels are {:.3} times as wide as they are high",
                info.sample_aspect.map_or(1.0, |r| r.to_f64())
            ),
            "export it with square pixels; some players and sites ignore the stretch",
        )
    });
}

// --- archive (amendment 29).

fn archive(f: &File, rules: &mut Vec<Rule>, tips: &mut Vec<Advice>) {
    let undecodable =
        f.r.warnings
            .iter()
            .filter(|w| matches!(w.code, probe_codes::NO_DECODER | probe_codes::CANNOT_DECODE))
            .count();
    rules.push(if undecodable == 0 {
        Rule::ok(
            "streams_decodable",
            "FFmpeg here can decode every video and audio stream",
        )
    } else {
        Rule::fail(
            "streams_decodable",
            format!("FFmpeg here can't decode {undecodable} of its streams"),
            "check it where FFmpeg has the decoders (doctor lists them) before deleting another copy",
        )
    });
    rules.push(camera_metadata(f));
    if f.layout() == Layout::EquirectLikely {
        tips.push(advice(
            "spherical_metadata",
            "it is 2:1 with no spherical metadata: if it is 360, players will show it flat",
        ));
    }
}

fn camera_metadata(f: &File) -> Rule {
    const ID: &str = "camera_metadata";
    let lost = |message: &str| {
        Rule::fail(
            ID,
            message,
            "keep the camera's original file: this copy can't be stitched as well",
        )
    };
    match (f.ext.as_str(), f.vendor()) {
        ("insv", v) if v != Some("Insta360") => {
            return lost("an .insv without Insta360's trailer: its gyro and lens data are gone");
        }
        ("360", v) if v != Some("GoPro") => {
            return lost("a .360 without GoPro's GPMF track: its gyro data is gone");
        }
        ("osv", v) if v != Some("DJI") => {
            return lost("an .osv without DJI's metadata track: its gyro data is gone");
        }
        _ => {}
    }
    let evidence =
        f.r.camera
            .as_ref()
            .map(|c| c.evidence.as_slice())
            .unwrap_or_default();
    let kept = match f.vendor() {
        Some("Insta360") if evidence.iter().any(|e| e.starts_with("Insta360 trailer")) => {
            "Insta360's trailer is kept"
        }
        Some("GoPro") if evidence.iter().any(|e| e.starts_with("GPMF")) => {
            "GoPro's GPMF track is kept"
        }
        Some("DJI") if evidence.iter().any(|e| e.starts_with("DJI metadata")) => {
            "DJI's metadata track is kept"
        }
        _ => "no Insta360, GoPro or DJI metadata is in it to keep",
    };
    Rule::ok(ID, kept)
}

// --- a project: what a render of it would make (a2-plan 7.2, D59).

/// The name a render's report is checked under: the export writes MP4.
const RENDER_NAME: &str = "render.mp4";

/// `check` with `project`: the rules on the file a render would make,
/// found as `render` finds it (the export's preset, encoders and audio
/// choice) without rendering. The rules keep their words: "it" is the
/// render. A render's own check, once `job` has it done, is the proof.
fn run_project(path: &Path, target: Option<Target>, cx: &Ctx) -> Result<Value, ToolError> {
    let loaded = sequence::load(cx, "project", path)?;
    let media =
        spherecut_media::probe::probe_cancellable(loaded.opened.source(), cx.call.interrupt())
            .map_err(|e| {
                cx.call.check().err().unwrap_or_else(|| {
                    let (code, message) = super::probe::words(e.code());
                    ToolError::new(code, format!("the project's media: {message}"))
                        .at("project")
                        .next("probe the project's media file")
                })
            })?;
    cx.call.check()?;
    let bits = media
        .streams
        .iter()
        .find(|s| s.index == loaded.media.video.stream as usize)
        .and_then(|s| s.video.as_ref())
        .and_then(|v| v.bit_depth)
        .unwrap_or(8);
    let proc = loaded.opened.proc_path().to_path_buf();
    let id = loaded.media.id;
    let project = loaded.project.reading_media_from(|m| {
        if m.id == id {
            proc.clone()
        } else {
            m.path.clone()
        }
    });
    let gpu = super::look::gpu()?;
    let job = ExportJob::new(project, PathBuf::from(RENDER_NAME), (*gpu).clone());
    let prepared = spherecut_export::prepare(&job).map_err(|e| super::render::prepare_error(&e))?;
    let planned =
        spherecut_export::planned(&job, &prepared).map_err(|e| super::render::prepare_error(&e))?;
    cx.call.check()?;
    let report = rendered(&planned, bits > 8 && !job.eight_bit);
    let file = File::of(&report, Path::new(RENDER_NAME));
    let (rules, tips) = rules_of(&file, target);
    let mut v = response(target, &rules, &tips);
    v["project"] = json!(text::field(&path.to_string_lossy()));
    v["checked"] = json!("the file a render of the project would make");
    if let Some(w) = v["warnings"].as_array_mut() {
        w.extend(
            planned
                .warnings
                .iter()
                .map(|w| json!({"code": w.code, "message": text::field(&w.message)})),
        );
    }
    Ok(v)
}

/// The report a probe of the render would give, as far as the rules read
/// one: an MP4 with its moov first, the video at the preset's size, rate
/// and depth in BT.709 4:2:0, with Spherical Video V2 and V1 for 360,
/// and the audio the export chose, with `SA3D` on ambiX.
fn rendered(planned: &Planned, ten_bit: bool) -> ProbeReport {
    use spherecut_core::project::Codec;
    use spherecut_media::probe::{
        AudioInfo, ColorInfo, Container as Format, Mp4Summary, StructureCheck,
    };
    use spherecut_media::spherical::{SphericalV1, SphericalV2, TrackSpherical};
    let want = &planned.file;
    let rate = want.rate.as_rational();
    let stream = |index, kind| StreamInfo {
        index,
        kind,
        codec: None,
        codec_tag: None,
        profile: None,
        level: None,
        time_base: rate.recip().unwrap_or(spherecut_core::Rational::ONE),
        start_pts: Some(0),
        duration: Some(want.duration),
        frames: None,
        avg_frame_rate: None,
        r_frame_rate: None,
        bit_rate: None,
        disposition: vec!["default".into()],
        handler: None,
        language: None,
        video: None,
        audio: None,
        spherical: TrackSpherical::default(),
        tags: Default::default(),
    };
    let mut video = stream(0, StreamKind::Video);
    video.codec = Some(
        match want.codec {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
            Codec::Av1 => "av1",
        }
        .into(),
    );
    video.frames = i64::try_from(want.frames).ok();
    video.avg_frame_rate = Some(rate);
    video.r_frame_rate = Some(rate);
    video.video = Some(VideoInfo {
        width: want.size.0,
        height: want.size.1,
        sample_aspect: None,
        pix_fmt: Some(if ten_bit { "yuv420p10le" } else { "yuv420p" }.into()),
        bit_depth: Some(if ten_bit { 10 } else { 8 }),
        chroma_subsampling: Some((1, 1)),
        color: ColorInfo {
            range: Some("tv".into()),
            matrix: Some("bt709".into()),
            primaries: Some("bt709".into()),
            transfer: Some("bt709".into()),
            chroma_location: Some("left".into()),
        },
        field_order: Some("progressive".into()),
        rotation_deg: None,
    });
    if want.sphere {
        video.spherical.v2 = Some(SphericalV2 {
            stereo: Some(StereoMode::Mono),
            metadata_source: Some("Spherecut".into()),
            pose_yaw: 0.0,
            pose_pitch: 0.0,
            pose_roll: 0.0,
            projection: Some(Projection::Equirect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 0,
            }),
        });
        video.spherical.v1 = Some(SphericalV1 {
            spherical: true,
            stitched: true,
            projection_type: Some("equirectangular".into()),
            stereo_mode: None,
            stitching_software: Some("Spherecut".into()),
            raw_xml: String::new(),
        });
    }
    let mut streams = vec![video];
    if let Some(a) = &want.audio {
        let mut audio = stream(1, StreamKind::Audio);
        audio.codec = Some(a.codec.clone());
        audio.profile = (a.codec == "aac").then(|| "LC".into());
        audio.bit_rate = planned.audio_bits.and_then(|b| i64::try_from(b).ok());
        audio.audio = Some(AudioInfo {
            sample_rate: a.rate,
            channels: u32::from(a.channels),
            layout: None,
            sample_fmt: None,
            opus_mapping_family: None,
        });
        if a.ambix {
            audio.spherical.spatial_audio = Some(SpatialAudio {
                version: 0,
                ambisonic_type: 0,
                head_locked_stereo: false,
                ambisonic_order: 1,
                channel_ordering: 0,
                normalization: 0,
                channel_map: vec![0, 1, 2, 3],
            });
        }
        streams.push(audio);
    }
    ProbeReport {
        path: PathBuf::from(RENDER_NAME),
        file_size: 0,
        container: Format {
            format: "mov,mp4,m4a,3gp,3g2,mj2".into(),
            format_long: "QuickTime / MOV".into(),
            duration: Some(want.duration),
            start_time: Some(spherecut_core::Time::ZERO),
            bit_rate: None,
            tags: Default::default(),
        },
        streams,
        mp4: Some(Mp4Summary {
            brand: Some("isom".into()),
            top_level: vec![("ftyp".into(), 0), ("moov".into(), 0), ("mdat".into(), 0)],
            trailing_bytes: 0,
            truncated: None,
            warnings: Vec::new(),
        }),
        camera: None,
        structure: StructureCheck::Full,
        damage: Vec::new(),
        warnings: Vec::new(),
        ffmpeg_log: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use spherecut_core::Rational;
    use spherecut_media::probe::{
        AudioInfo, ColorInfo, Container as Format, Mp4Summary, StructureCheck,
    };
    use spherecut_media::spherical::{SphericalV1, SphericalV2, TrackSpherical};

    use super::*;

    fn stream(index: usize, kind: StreamKind, codec: &str) -> StreamInfo {
        StreamInfo {
            index,
            kind,
            codec: Some(codec.into()),
            codec_tag: None,
            profile: None,
            level: None,
            time_base: Rational::ONE,
            start_pts: None,
            duration: None,
            frames: None,
            avg_frame_rate: Rational::new(30, 1).ok(),
            r_frame_rate: Rational::new(30, 1).ok(),
            bit_rate: None,
            disposition: Vec::new(),
            handler: None,
            language: None,
            video: (kind == StreamKind::Video).then(|| VideoInfo {
                width: 3840,
                height: 1920,
                sample_aspect: None,
                pix_fmt: Some("yuv420p".into()),
                bit_depth: Some(8),
                chroma_subsampling: Some((1, 1)),
                color: ColorInfo {
                    range: None,
                    matrix: None,
                    primaries: None,
                    transfer: None,
                    chroma_location: None,
                },
                field_order: Some("progressive".into()),
                rotation_deg: None,
            }),
            audio: (kind == StreamKind::Audio).then_some(AudioInfo {
                sample_rate: 48_000,
                channels: 4,
                layout: None,
                sample_fmt: None,
                opus_mapping_family: None,
            }),
            spherical: TrackSpherical::default(),
            tags: BTreeMap::new(),
        }
    }

    fn equirect() -> SphericalV2 {
        SphericalV2 {
            stereo: None,
            metadata_source: None,
            pose_yaw: 0.0,
            pose_pitch: 0.0,
            pose_roll: 0.0,
            projection: Some(Projection::Equirect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 0,
            }),
        }
    }

    fn ambix(head_locked: bool) -> SpatialAudio {
        let n = if head_locked { 6 } else { 4 };
        SpatialAudio {
            version: 0,
            ambisonic_type: if head_locked { 0x80 } else { 0 },
            head_locked_stereo: head_locked,
            ambisonic_order: 1,
            channel_ordering: 0,
            normalization: 0,
            channel_map: (0..n).collect(),
        }
    }

    /// An MP4 (`brand` "isom") or MOV ("qt  ") of these streams.
    fn report(brand: &str, streams: Vec<StreamInfo>) -> ProbeReport {
        ProbeReport {
            path: "a.mp4".into(),
            file_size: 1,
            container: Format {
                format: "mov,mp4,m4a,3gp,3g2,mj2".into(),
                format_long: String::new(),
                duration: None,
                start_time: None,
                bit_rate: None,
                tags: BTreeMap::new(),
            },
            streams,
            mp4: Some(Mp4Summary {
                brand: Some(brand.into()),
                top_level: Vec::new(),
                trailing_bytes: 0,
                truncated: None,
                warnings: Vec::new(),
            }),
            camera: None,
            structure: StructureCheck::Full,
            damage: Vec::new(),
            warnings: Vec::new(),
            ffmpeg_log: Vec::new(),
        }
    }

    fn tagged_video() -> StreamInfo {
        let mut v = stream(0, StreamKind::Video, "h264");
        v.spherical.v2 = Some(equirect());
        v
    }

    fn spatial(codec: &str, bit_rate: i64) -> StreamInfo {
        let mut a = stream(1, StreamKind::Audio, codec);
        a.spherical.spatial_audio = Some(ambix(false));
        a.bit_rate = Some(bit_rate);
        a
    }

    fn rules_of(r: &ProbeReport, target: Option<Target>) -> (Vec<Rule>, Vec<Advice>) {
        let f = File::of(r, std::path::Path::new("a.mp4"));
        let mut rules = always(&f);
        let mut tips = Vec::new();
        match target {
            None => {}
            Some(Target::Youtube360) => youtube_360(&f, &mut rules, &mut tips),
            Some(t @ (Target::Flat16x9 | Target::Vertical)) => flat(&f, t, &mut rules),
            Some(Target::Archive) => archive(&f, &mut rules, &mut tips),
        }
        (rules, tips)
    }

    fn rule<'a>(rules: &'a [Rule], id: &str) -> &'a Rule {
        rules
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("no {id} in {rules:?}"))
    }

    #[test]
    fn v1_and_v2_must_agree() {
        let mut v = tagged_video();
        v.spherical.v1 = Some(SphericalV1 {
            spherical: true,
            stitched: true,
            projection_type: Some("equirectangular".into()),
            stereo_mode: Some("top-bottom".into()),
            stitching_software: None,
            raw_xml: String::new(),
        });
        // A 1:1 picture, so that V1's top-bottom fits it and V2's mono
        // doesn't.
        v.video.as_mut().unwrap().height = 3840;
        let r = report("isom", vec![v.clone()]);
        let (rules, _) = rules_of(&r, None);
        let agree = rule(&rules, "v1_v2_agree");
        assert!(!agree.ok);
        assert_eq!(agree.message, "V1 says top-bottom stereo and V2 says mono");
        // V2's stereo, where it has one, is what the shape is checked
        // against; without one, V1's.
        assert!(rule(&rules, "projection_fits_picture").ok, "{rules:?}");
        v.spherical.v2.as_mut().unwrap().stereo = Some(StereoMode::TopBottom);
        let (rules, _) = rules_of(&report("isom", vec![v]), None);
        assert!(rule(&rules, "v1_v2_agree").ok);
    }

    #[test]
    fn youtube_spatial_audio_by_codec_container_and_rate() {
        let cases = [
            ("isom", spatial("aac", 320_000), true),
            ("isom", spatial("aac", 128_000), false),
            // An encoder at 256 kb/s averages a hair under.
            ("isom", spatial("aac", 255_000), true),
            ("isom", spatial("aac", 250_000), false),
            ("qt  ", spatial("pcm_s24le", 0), true),
            ("isom", spatial("pcm_s24le", 0), false),
            ("isom", spatial("opus", 512_000), true),
            ("qt  ", spatial("opus", 512_000), false),
            ("isom", spatial("mp3", 320_000), false),
        ];
        for (brand, a, ok) in cases {
            let what = format!("{brand} {:?} {:?}", a.codec, a.bit_rate);
            let r = report(brand, vec![tagged_video(), a]);
            let (rules, _) = rules_of(&r, Some(Target::Youtube360));
            let got = rule(&rules, "spatial_audio_youtube");
            assert_eq!(got.ok, ok, "{what}: {got:?}");
            assert_eq!(got.fix.is_some(), !ok);
        }
        let mut family_1 = spatial("opus", 512_000);
        family_1.audio.as_mut().unwrap().opus_mapping_family = Some(1);
        let (rules, _) = rules_of(
            &report("isom", vec![tagged_video(), family_1]),
            Some(Target::Youtube360),
        );
        let got = rule(&rules, "spatial_audio_youtube");
        assert!(!got.ok && got.message.contains("family 1"), "{got:?}");
        // Head-locked stereo: six channels, and Opus at 768 kb/s.
        let mut six = spatial("opus", 600_000);
        six.spherical.spatial_audio = Some(ambix(true));
        six.audio.as_mut().unwrap().channels = 6;
        let (rules, _) = rules_of(
            &report("isom", vec![tagged_video(), six]),
            Some(Target::Youtube360),
        );
        assert!(rule(&rules, "spatial_audio_fits").ok, "{rules:?}");
        let got = rule(&rules, "spatial_audio_youtube");
        assert!(!got.ok && got.message.contains("768"), "{got:?}");
    }

    #[test]
    fn spatial_audio_beside_stereo_is_two_tracks() {
        let stereo = stream(2, StreamKind::Audio, "aac");
        let r = report(
            "isom",
            vec![tagged_video(), spatial("aac", 320_000), stereo],
        );
        let (rules, _) = rules_of(&r, Some(Target::Youtube360));
        assert!(!rule(&rules, "one_audio_track").ok);
    }

    #[test]
    fn interlaced_rotated_and_stretched_pictures() {
        let mut v = tagged_video();
        v.video.as_mut().unwrap().field_order = Some("tt".into());
        let (rules, _) = rules_of(&report("isom", vec![v]), Some(Target::Youtube360));
        assert!(!rule(&rules, "progressive").ok);

        // 1920x1080 rotated a quarter turn shows at 9:16.
        let mut v = stream(0, StreamKind::Video, "h264");
        let info = v.video.as_mut().unwrap();
        (info.width, info.height, info.rotation_deg) = (1920, 1080, Some(-90.0));
        let r = report("isom", vec![v.clone()]);
        assert!(rule(&rules_of(&r, Some(Target::Vertical)).0, "aspect").ok);
        assert!(!rule(&rules_of(&r, Some(Target::Flat16x9)).0, "aspect").ok);

        // 1440x1080 at 4:3 pixels shows at 16:9, but its pixels aren't
        // square.
        let info = v.video.as_mut().unwrap();
        (info.width, info.rotation_deg) = (1440, None);
        info.sample_aspect = Rational::new(4, 3).ok();
        let (rules, _) = rules_of(&report("isom", vec![v]), Some(Target::Flat16x9));
        assert!(rule(&rules, "aspect").ok, "{rules:?}");
        assert!(!rule(&rules, "square_pixels").ok);
    }

    #[test]
    fn youtube_advice_names_only_what_is_off() {
        let (_, tips) = rules_of(
            &report("isom", vec![tagged_video()]),
            Some(Target::Youtube360),
        );
        assert!(tips.is_empty(), "{tips:?}");
        let mut v = stream(0, StreamKind::Video, "hevc");
        v.spherical.v2 = Some(equirect());
        let info = v.video.as_mut().unwrap();
        info.chroma_subsampling = Some((1, 0));
        info.color.matrix = Some("bt2020nc".into());
        v.avg_frame_rate = Rational::new(15, 1).ok();
        let mut a = stream(1, StreamKind::Audio, "aac");
        a.profile = Some("HE-AAC".into());
        a.audio.as_mut().unwrap().sample_rate = 44_100;
        let (_, tips) = rules_of(&report("isom", vec![v, a]), Some(Target::Youtube360));
        let ids: Vec<&str> = tips.iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            [
                "h264",
                "chroma_420",
                "bt709",
                "frame_rate",
                "audio_codec",
                "sample_rate"
            ]
        );
        // An NTSC rate is common.
        let mut v = tagged_video();
        v.avg_frame_rate = Rational::new(30_000, 1001).ok();
        let (_, tips) = rules_of(&report("isom", vec![v]), Some(Target::Youtube360));
        assert!(tips.is_empty(), "{tips:?}");
    }

    #[test]
    fn bounds_and_custom_stereo() {
        // Bounds that crop half the sphere's height: 4:1.
        let mut v = tagged_video();
        v.spherical.v2.as_mut().unwrap().projection = Some(Projection::Equirect {
            top: 1 << 30,
            bottom: 1 << 30,
            left: 0,
            right: 0,
        });
        let r = report("isom", vec![v.clone()]);
        let (rules, _) = rules_of(&r, None);
        assert!(!rule(&rules, "projection_fits_picture").ok);
        v.video.as_mut().unwrap().height = 960;
        let (rules, _) = rules_of(&report("isom", vec![v.clone()]), None);
        assert!(rule(&rules, "projection_fits_picture").ok, "{rules:?}");
        // Bounds that crop it all.
        v.spherical.v2.as_mut().unwrap().projection = Some(Projection::Equirect {
            top: u32::MAX,
            bottom: 1,
            left: 0,
            right: 0,
        });
        let (rules, _) = rules_of(&report("isom", vec![v.clone()]), None);
        let got = rule(&rules, "projection_fits_picture");
        assert!(!got.ok && got.message.contains("bounds"), "{got:?}");
        // Custom stereo can't be checked, so it passes.
        v.spherical.v2 = Some(SphericalV2 {
            stereo: Some(StereoMode::StereoCustom),
            ..equirect()
        });
        let (rules, _) = rules_of(&report("isom", vec![v]), None);
        assert!(rule(&rules, "projection_fits_picture").ok);
    }

    #[test]
    fn ratios_read_as_people_write_them() {
        assert_eq!(ratio(1920.0 / 1080.0), "16:9");
        assert_eq!(ratio(1080.0 / 1920.0), "9:16");
        assert_eq!(ratio(2.0), "2:1");
        assert_eq!(ratio(1.25), "1.25:1");
    }

    #[test]
    fn an_eye_is_its_share_of_the_picture() {
        // A 2:1 equirect: 3840x1920 mono, 3840x3840 top-bottom, 7680x1920
        // left-right.
        for (w, h, s) in [
            (3840.0, 1920.0, StereoMode::Mono),
            (3840.0, 3840.0, StereoMode::TopBottom),
            (7680.0, 1920.0, StereoMode::LeftRight),
            (7680.0, 1920.0, StereoMode::Unknown(4)),
        ] {
            let (x, y) = eye(s).unwrap();
            assert!(near(w * x / (h * y), 2.0), "{s:?}");
        }
        assert!(eye(StereoMode::StereoCustom).is_none());
    }

    #[test]
    fn the_response_is_cut_to_fit() {
        let long = "x".repeat(400);
        let rules: Vec<Rule> = (0..6)
            .map(|i| {
                if i % 2 == 0 {
                    Rule::ok("readable", long.clone())
                } else {
                    Rule::fail("readable", "short", "short")
                }
            })
            .collect();
        let tips = vec![advice("h264", long.clone()); 4];
        let out = response(Some(Target::Youtube360), &rules, &tips);
        assert!(out.to_string().len() <= CHECK_BYTES, "{out}");
        assert_eq!(out["pass"], false);
        assert_eq!(out["advice"], json!([]));
        assert_eq!(out["warnings"][0]["code"], "result_cut");
        for r in out["rules"].as_array().unwrap() {
            if r["ok"] == false {
                assert_eq!(r["message"], "short");
            }
        }
    }
}
