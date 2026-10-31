//! The CLI's JSON contract (DECISIONS D27, `docs/cli-json.md`). With
//! `--json`, a run prints exactly one [`Envelope`] on stdout as one line
//! of compact JSON, on success and on failure, and exits with an [`Exit`]
//! status. stderr carries only logs for people. It lives here, below the
//! CLI, since A1.4 (a1-plan amendment 13): [`crate::still`], frame's core,
//! fails with its [`ErrorBody`] and reports in its types, and the CLI
//! re-exports it as `spherecut_cli::contract`.
//!
//! Within a schema version, fields are only added: a field is never
//! removed, renamed or given another type, and a code never renamed or
//! given another meaning. Anything else is a new version.

use std::ffi::OsString;
use std::path::Path;

use serde::Serialize;
use spherecut_core::FrameTime;
use spherecut_core::time::{FrameRate, Time};
use spherecut_core::video::ColorInfo;
use spherecut_export::warning::codes as export_warnings;
use spherecut_export::{ExportError, Fallback};
use spherecut_media::MediaError;
use spherecut_media::doctor::{DoctorReport, Severity};
use spherecut_media::probe::{self, ProbeError, ProbeReport};
use spherecut_render::RenderError;

/// The schema of `spherecut probe --json`.
pub const PROBE: &str = "spherecut.probe/1";
/// The schema of `spherecut doctor --json`.
pub const DOCTOR: &str = "spherecut.doctor/1";
/// The schema of `spherecut frame --json`.
pub const FRAME: &str = "spherecut.frame/1";
/// The schema of `spherecut render --json`: one envelope a line, each
/// naming its event (D51).
pub const RENDER: &str = "spherecut.render/1";
/// The schema of `spherecut call`, which always prints JSON: an agent
/// tool's Response as `result`, or its error (a1-plan amendment 7).
pub const CALL: &str = "spherecut.call/1";
/// The schema of a usage error that names no subcommand.
pub const CLI: &str = "spherecut/1";

/// How a run ends, as its exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// The run did what was asked.
    Ok = 0,
    /// An input or a check failed: a file probe could not read, a
    /// problem of severity error that doctor found, or a frame that
    /// isn't there or can't be written.
    Failed = 1,
    /// The command line is wrong.
    Usage = 2,
    /// `frame` (and the GUI, later): a codec or device the command itself
    /// needs is missing (no GPU, a hardware decoder asked for that can't
    /// decode, no decoder; no display). probe and doctor never exit so.
    Missing = 3,
    /// `frame`: interrupted (SIGINT), leaving no output file. The run's
    /// document still goes out, with the code `cancelled`. probe and
    /// doctor never exit so.
    Cancelled = 4,
}

impl From<Exit> for std::process::ExitCode {
    fn from(e: Exit) -> Self {
        std::process::ExitCode::from(e as u8)
    }
}

/// The CLI's own codes, for `error.code` when the run as a whole failed,
/// and for frame's warnings. The codes of single files come from
/// [`MediaError::code`] and [`probe::codes`], and the GPU's from
/// [`RenderError::code`].
pub mod codes {
    /// The command line is wrong (exit 2).
    pub const USAGE: &str = "usage";
    /// probe: at least one file failed; each file's `error` says why.
    pub const FILES_FAILED: &str = "files_failed";
    /// doctor: at least one problem is of severity error.
    pub const DOCTOR_ERRORS: &str = "doctor_errors";
    /// A failure of Spherecut's own, to be reported as a bug.
    pub const INTERNAL: &str = "internal";
    /// frame: something is at the output already, and `--overwrite`
    /// wasn't given (exit 1).
    pub const OUTPUT_EXISTS: &str = "output_exists";
    /// frame: the file isn't a stitched equirect (dual fisheye, a
    /// cubemap, no spherical metadata), and `--equirect` wasn't given
    /// (exit 1).
    pub const NOT_EQUIRECT: &str = "not_equirect";
    /// frame: no frame at the time or number asked for: it is past the
    /// end (exit 1). render: `--range` reaches outside the sequence
    /// (exit 1).
    pub const OUT_OF_RANGE: &str = "out_of_range";
    /// frame and render: interrupted by SIGINT, leaving no output file
    /// (exit 4).
    pub const CANCELLED: &str = "cancelled";
    /// The GUI: no Wayland or X11 display to open its window on (exit 3).
    pub const NO_DISPLAY: &str = "no_display";
    /// The GUI's smoke run: no frame of the clip within 60 s, or no UI
    /// pass for 15 s (exit 1).
    pub const TIMED_OUT: &str = "timed_out";
    /// render: the file isn't a Spherecut project (exit 1).
    pub const NOT_PROJECT: &str = "not_project";
    /// render: the project was saved by a newer Spherecut (exit 1).
    pub const NEWER_SCHEMA: &str = "newer_schema";
    /// render: the project file doesn't read as one: a field of the wrong
    /// kind, or a time whose `seconds` and `exact` disagree (exit 1).
    pub const MALFORMED_PROJECT: &str = "malformed_project";
    /// render: the project breaks one of its rules, as loaded or with the
    /// flags' changes (exit 1).
    pub const INVALID_PROJECT: &str = "invalid_project";
    /// render: the clip's media file isn't where the project says, so the
    /// clip is offline (exit 1).
    pub const MEDIA_OFFLINE: &str = "media_offline";

    /// Every error code above.
    pub const ALL: &[&str] = &[
        USAGE,
        FILES_FAILED,
        DOCTOR_ERRORS,
        INTERNAL,
        OUTPUT_EXISTS,
        NOT_EQUIRECT,
        OUT_OF_RANGE,
        CANCELLED,
        NO_DISPLAY,
        TIMED_OUT,
        NOT_PROJECT,
        NEWER_SCHEMA,
        MALFORMED_PROJECT,
        INVALID_PROJECT,
        MEDIA_OFFLINE,
    ];

    /// frame's warnings that aren't errors' codes too: what it did with
    /// the file's spherical metadata, and the filter it could use.
    pub mod warnings {
        /// A stereo source: its left eye is rendered.
        pub const STEREO_LEFT_EYE: &str = "stereo_left_eye";
        /// A stereo layout frame doesn't know: the picture is rendered
        /// whole, as mono.
        pub const STEREO_UNKNOWN: &str = "stereo_unknown";
        /// Spherical Video V2 crop bounds: the picture covers part of the
        /// sphere, and the rest is black.
        pub const CROP_APPLIED: &str = "crop_applied";
        /// A non-zero Spherical Video V2 pose, which M1 doesn't apply.
        pub const POSE_NOT_APPLIED: &str = "pose_not_applied";
        /// `--filter aniso` on a device without anisotropic filtering:
        /// the render is isotropic.
        pub const ANISOTROPY_UNAVAILABLE: &str = "anisotropy_unavailable";
        /// render: the clip's media file is there but isn't the one added
        /// (its size or first MiB differs); it is exported anyway.
        pub const MEDIA_CHANGED: &str = "media_changed";

        pub const ALL: &[&str] = &[
            STEREO_LEFT_EYE,
            STEREO_UNKNOWN,
            CROP_APPLIED,
            POSE_NOT_APPLIED,
            ANISOTROPY_UNAVAILABLE,
            MEDIA_CHANGED,
        ];
    }
}

/// Every code the JSON can carry, as an error or a warning, sorted and
/// without repeats: the CLI's own, [`MediaError::CODES`],
/// [`probe::codes::ALL`], [`RenderError::CODES`], and the export's
/// ([`ExportError::CODES`] and its warnings').
pub fn all_codes() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = codes::ALL
        .iter()
        .chain(codes::warnings::ALL)
        .chain(MediaError::CODES)
        .chain(probe::codes::ALL)
        .chain(RenderError::CODES)
        .chain(ExportError::CODES)
        .chain(export_warnings::ALL)
        .copied()
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// The exit status of a `frame` or `render` run, or the GUI's, that
/// failed with `code`: 2 for a usage error, 3 for a device, codec,
/// encoder or display the run needs and doesn't have, 4 when it was
/// cancelled, and 1 for anything else (D27, D51). `hw_*` codes fail a
/// run only when `--decoder` or `--encoder` named the hardware; `auto`
/// falls back to software instead.
pub fn frame_exit(code: &str) -> Exit {
    match code {
        codes::USAGE => Exit::Usage,
        "no_gpu"
        | "adapter_not_found"
        | "hw_unavailable"
        | "hw_unsupported"
        | "hw_failed"
        | "import_failed"
        | "no_decoder"
        | "no_encoder"
        | codes::NO_DISPLAY => Exit::Missing,
        codes::CANCELLED => Exit::Cancelled,
        _ => Exit::Failed,
    }
}

/// The one document a `--json` run prints. `ok` is true exactly when
/// there is no `error`.
#[derive(Debug, Serialize)]
pub struct Envelope<T> {
    schema: &'static str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
    warnings: Vec<Warning>,
}

impl<T: Serialize> Envelope<T> {
    /// A document of `schema`, which failed when there is an `error`.
    pub fn new(schema: &'static str, result: Option<T>, error: Option<ErrorBody>) -> Self {
        Envelope {
            schema,
            ok: error.is_none(),
            result,
            error,
            warnings: Vec::new(),
        }
    }

    /// The same document with `warnings` about the run as a whole.
    pub fn with_warnings(mut self, warnings: Vec<Warning>) -> Self {
        self.warnings = warnings;
        self
    }

    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    pub fn schema(&self) -> &'static str {
        self.schema
    }

    pub fn ok(&self) -> bool {
        self.ok
    }

    pub fn error(&self) -> Option<&ErrorBody> {
        self.error.as_ref()
    }

    /// The document as it is printed: one line of compact JSON and a
    /// newline. JSON escapes the newlines inside strings.
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }
}

/// Why a run, or one of its files, failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
    /// Stable, snake_case: `docs/cli-json.md` lists every code.
    pub code: &'static str,
    /// For people; its wording may change.
    pub message: String,
    /// The file it is about, as text: bytes that aren't UTF-8 are U+FFFD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// FFmpeg's warnings and errors about a file it could not open.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ffmpeg_log: Vec<String>,
}

impl ErrorBody {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ErrorBody {
            code,
            message: message.into(),
            path: None,
            ffmpeg_log: Vec::new(),
        }
    }
}

/// Something about the run as a whole that didn't stop it. probe and
/// doctor report their findings in `result` and have none of these yet;
/// frame's are about its file and its devices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
}

impl Warning {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Warning {
            code,
            message: message.into(),
        }
    }
}

/// What `spherecut frame --json` gives in `result` when it wrote the
/// frame (`docs/cli-json.md` has every field). A failed run has none.
#[derive(Debug, Clone, Serialize)]
pub struct FrameResult {
    /// The PNG written, as given.
    pub output: String,
    /// The frame rendered.
    pub frame: FrameShown,
    /// What `--time` or `--frame` asked for: a time, with `frame` when a
    /// frame number was asked for (for a constant-rate stream, the time
    /// is that frame's start).
    pub requested: FrameTime,
    /// How the frame relates to what was asked for: `exact`, the only
    /// value `frame` writes in version 1 (anything else fails the run).
    pub exactness: &'static str,
    pub size: Size,
    /// 8 or 16 bits per channel.
    pub depth: u8,
    pub view: ViewInfo,
    /// The filter rendered with, `iso` or `aniso`: `iso` where `--filter
    /// aniso` met a device without anisotropic filtering, which a warning
    /// says.
    pub filter: &'static str,
    /// `gpu`, or `reference` for `--reference`'s CPU render.
    pub renderer: &'static str,
    pub decode: DecodeInfo,
    /// The adapter rendered on; null for `--reference`.
    pub gpu: Option<GpuInfo>,
    /// The colour the frame was decoded as, with what was assumed.
    pub color: ColorInfo,
    pub source: SourceInfo,
}

/// A frame of the stream: its own timestamp, and its clip time.
#[derive(Debug, Clone, Serialize)]
pub struct FrameShown {
    /// In ticks of the stream's time base.
    pub pts: i64,
    /// Clip time, with the frame's index when it is known: for a
    /// constant-rate stream, or a frame reached by counting.
    pub time: FrameTime,
}

/// A width and a height, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// The virtual camera, as given: degrees, and the projections by name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewInfo {
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    /// Horizontal, in degrees.
    pub fov: f64,
    /// `rectilinear`, `fisheye` or `stereographic`.
    pub projection: &'static str,
    /// The projection morphed towards, or null.
    pub morph_to: Option<&'static str>,
    /// 0 is `projection`, 1 is `morph_to`.
    pub morph: f64,
}

/// Where the frame was decoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodeInfo {
    /// `software`, `vaapi` or `vulkan`.
    pub path: &'static str,
    /// How the frame left its decoder: `system` (decoded in system
    /// memory), `downloaded` (copied from the device) or `drm_prime`
    /// (exported as DMA-BUFs, `--zero-copy`).
    pub transfer: &'static str,
    /// Why a hardware decode went on in software, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackInfo>,
}

/// A hardware decode that went on in software, or (render) a hardware
/// encoder that did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FallbackInfo {
    /// `vaapi` or `vulkan` for a decode; FFmpeg's name for an encoder,
    /// such as `hevc_vaapi`.
    pub from: &'static str,
    /// `hw_unavailable` (no device), `hw_unsupported` (the device turned
    /// the stream down) or `hw_failed` (it failed on a frame).
    pub code: &'static str,
    pub reason: String,
}

/// The adapter a frame was rendered on, as wgpu describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuInfo {
    pub name: String,
    pub driver: String,
    pub driver_info: String,
    /// `discrete_gpu`, `integrated_gpu`, `virtual_gpu`, `cpu` (lavapipe)
    /// or `other`.
    pub device_type: &'static str,
}

/// The stream the frame came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceInfo {
    /// FFmpeg's stream index.
    pub stream: usize,
    /// FFmpeg's codec name, such as `hevc`.
    pub codec: String,
    /// The decoded picture, after cropping.
    pub size: Size,
    /// The decoder's pixel format, as FFmpeg names it.
    pub pix_fmt: String,
}

impl From<&Fallback> for FallbackInfo {
    fn from(f: &Fallback) -> Self {
        FallbackInfo {
            from: f.from,
            code: f.code,
            reason: f.reason.clone(),
        }
    }
}

/// A line of `spherecut render --json`, as its envelope's `result` (D51):
/// `start` once the export has started, `progress` at most once a
/// second, and `result` last, for an export that wrote its file. A run
/// that fails ends instead with an envelope that has an `error` and no
/// `result`, as every command's failure does.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RenderEvent {
    Start(RenderStart),
    Progress(RenderProgress),
    Result(RenderResult),
}

/// What the export is about to do.
#[derive(Debug, Clone, Serialize)]
pub struct RenderStart {
    /// The file to write, as given.
    pub output: String,
    /// The project file, as given.
    pub project: String,
    /// The preset's name, such as `flat-16x9-1080` or `360-5760`.
    pub preset: String,
    /// The picture's size: the preset's, or its draft's.
    pub size: Size,
    /// `--draft`: half the size, the encoders' fastest settings.
    pub draft: bool,
    pub rate: FrameRate,
    /// The part of the sequence exported, in sequence time: all of it,
    /// or `--range`.
    pub range: TimeRange,
    pub frames: u64,
    /// `frames / rate`.
    pub duration: Time,
    /// `h264`, `hevc` or `av1`.
    pub codec: &'static str,
    /// `high`, `medium` or `small`.
    pub quality: &'static str,
    /// The encoder tried first, as FFmpeg names it.
    pub encoder: &'static str,
    /// The hardware encoder `auto` passed over without trying, and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackInfo>,
    /// `copy`, `aac` or `none`, as asked.
    pub audio: &'static str,
    pub motion_blur: bool,
}

/// A span of time, [start, end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimeRange {
    pub start: Time,
    pub end: Time,
}

/// How far the export has got.
#[derive(Debug, Clone, Serialize)]
pub struct RenderProgress {
    /// Frames muxed: never fewer than the line before said, even when an
    /// encoder that failed in its first second starts over in software.
    pub frames_done: u64,
    pub frames_total: u64,
    /// Frames a second over the last two seconds; 0 until it can tell.
    pub fps: f64,
    /// Seconds since the export started.
    pub elapsed: f64,
    /// Seconds left at `fps`; null until it can tell.
    pub eta: Option<f64>,
    /// The encoder now.
    pub encoder: &'static str,
    /// The hardware encoder the export passed over or left, and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackInfo>,
    /// The hardware decode the export passed over or left, and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_fallback: Option<FallbackInfo>,
    /// The last pass, which writes the finished file: bytes written and
    /// in all, once it has started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finishing: Option<Finishing>,
}

/// The last pass's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Finishing {
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// The finished file, which passed every check before it was put at
/// `output`.
#[derive(Debug, Clone, Serialize)]
pub struct RenderResult {
    pub output: String,
    pub frames: u64,
    pub duration: Time,
    pub size: Size,
    pub draft: bool,
    pub rate: FrameRate,
    pub codec: &'static str,
    /// The encoder that wrote the video, as FFmpeg names it.
    pub encoder: &'static str,
    /// The hardware encoder passed over, or left in its first second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackInfo>,
    pub decode: RenderDecode,
    pub ten_bit: bool,
    /// The audio track; null for none.
    pub audio: Option<RenderAudio>,
    /// The file's size.
    pub bytes: u64,
    /// Frames whose motion blur needed more views than the cap.
    pub blur_capped: u64,
    /// The checks the file passed, in order.
    pub checks: Vec<RenderCheck>,
    /// Seconds from start to finish.
    pub elapsed: f64,
}

/// Where the export's frames were decoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderDecode {
    /// `vaapi` or `software`: where the last frame was decoded.
    pub path: &'static str,
    /// The hardware decode passed over or left, and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackInfo>,
}

/// The export's audio track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderAudio {
    /// The source's stream, by FFmpeg's index.
    pub stream: usize,
    /// FFmpeg's name for the encoder, or the copied codec's.
    pub encoder: &'static str,
    pub rate: u32,
    pub channels: u16,
    /// First-order ambiX, with an `SA3D` box.
    pub ambix: bool,
    /// Samples a channel.
    pub samples: u64,
}

/// A check the finished file passed, and what it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderCheck {
    /// `probe`, `layout`, `codec`, `size`, `frames`, `duration`, `rate`,
    /// `st3d`, `sv3d`, `v1`, `spherical`, `audio` or `sa3d`.
    pub name: &'static str,
    pub found: String,
}

/// What `spherecut probe --json` gives in `result`: one entry per file,
/// in the order given, for one file as for many.
#[derive(Debug, Serialize)]
pub struct ProbeResult {
    pub files: Vec<ProbeFile>,
}

/// One file probed: its report when FFmpeg could open it, and its error
/// when probe fails it. A damaged file has both.
#[derive(Debug, Serialize)]
pub struct ProbeFile {
    /// As given, as text: bytes that aren't UTF-8 are U+FFFD.
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<ProbeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl ProbeFile {
    /// The entry for `path`, which [`probe::probe`] gave `probed`.
    pub fn new(path: &Path, probed: Result<ProbeReport, ProbeError>) -> Self {
        let path = path.to_string_lossy().into_owned();
        let (report, error) = match probed {
            Ok(report) => {
                let error = report.failure().map(|p| ErrorBody {
                    path: Some(path.clone()),
                    ..ErrorBody::new(p.code, p.message)
                });
                (Some(report), error)
            }
            Err(e) => {
                let mut error = ErrorBody::new(e.code(), e.to_string());
                error.path = Some(path.clone());
                error.ffmpeg_log = e.ffmpeg_log;
                (None, Some(error))
            }
        };
        ProbeFile {
            path,
            ok: error.is_none(),
            report,
            error,
        }
    }
}

/// probe's document and exit status: it fails when any file does.
pub fn probe(files: Vec<ProbeFile>) -> (Envelope<ProbeResult>, Exit) {
    let failed: Vec<&ErrorBody> = files.iter().filter_map(|f| f.error.as_ref()).collect();
    let error = failed.first().map(|first| {
        let more = match failed.len() - 1 {
            0 => String::new(),
            n => format!(" (and {n} more)"),
        };
        ErrorBody::new(
            codes::FILES_FAILED,
            format!(
                "{} of {} {} failed: {}{more}",
                failed.len(),
                files.len(),
                if files.len() == 1 { "file" } else { "files" },
                first.message
            ),
        )
    });
    let exit = if error.is_some() {
        Exit::Failed
    } else {
        Exit::Ok
    };
    (
        Envelope::new(PROBE, Some(ProbeResult { files }), error),
        exit,
    )
}

/// doctor's document and exit status: it fails when any problem is of
/// severity error (D18), and the report comes with the error.
pub fn doctor(report: &DoctorReport) -> (Envelope<&DoctorReport>, Exit) {
    let errors: Vec<&str> = report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.what.as_str())
        .collect();
    let error = (!errors.is_empty()).then(|| {
        ErrorBody::new(
            codes::DOCTOR_ERRORS,
            format!(
                "doctor found {} {}: {}",
                errors.len(),
                if errors.len() == 1 { "error" } else { "errors" },
                errors.join("; ")
            ),
        )
    });
    let exit = if error.is_some() {
        Exit::Failed
    } else {
        Exit::Ok
    };
    (Envelope::new(DOCTOR, Some(report), error), exit)
}

/// Whether the command line asks for JSON: `--json` before any `--`, or
/// `call`, which has no other output. Checked before parsing, so that a
/// command line that doesn't parse can still be answered in JSON.
pub fn wants_json(args: &[OsString]) -> bool {
    schema_for(args) == CALL
        || args
            .iter()
            .skip(1)
            .take_while(|a| *a != "--")
            .any(|a| a == "--json")
}

/// The schema of the document for a command line, which may not parse:
/// the subcommand's, when the first argument that isn't an option names
/// one, else [`CLI`]. Usage errors and panics are reported in it.
pub fn schema_for(args: &[OsString]) -> &'static str {
    let first = args
        .iter()
        .skip(1)
        .find(|a| !a.to_string_lossy().starts_with('-'));
    match first.and_then(|a| a.to_str()) {
        Some("probe") => PROBE,
        Some("doctor") => DOCTOR,
        Some("frame") => FRAME,
        Some("render") => RENDER,
        Some("call") => CALL,
        _ => CLI,
    }
}

/// The document for a command line that doesn't parse, with clap's
/// `message`.
pub fn usage(args: &[OsString], message: &str) -> (Envelope<()>, Exit) {
    let message = message.trim();
    let message = message.strip_prefix("error: ").unwrap_or(message);
    (
        Envelope::new(
            schema_for(args),
            None,
            Some(ErrorBody::new(codes::USAGE, message)),
        ),
        Exit::Usage,
    )
}

/// The document for a failure of Spherecut's own in a run of `schema`.
pub fn internal(schema: &'static str, message: impl Into<String>) -> (Envelope<()>, Exit) {
    (
        Envelope::new(schema, None, Some(ErrorBody::new(codes::INTERNAL, message))),
        Exit::Failed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn json_is_asked_for_only_before_a_double_dash() {
        assert!(wants_json(&args(&[
            "spherecut",
            "probe",
            "--json",
            "a.mp4"
        ])));
        assert!(wants_json(&args(&["spherecut", "--json"])));
        assert!(!wants_json(&args(&["spherecut", "probe", "--", "--json"])));
        assert!(!wants_json(&args(&["spherecut", "probe", "--json=1"])));
        // argv[0] is the program, whatever it is called.
        assert!(!wants_json(&args(&["--json", "probe"])));
        assert!(wants_json(&args(&["spherecut", "call", "probe", "{}"])));
        assert_eq!(schema_for(&args(&["spherecut", "call"])), CALL);
    }

    #[test]
    fn usage_errors_name_the_subcommand_when_they_can() {
        assert_eq!(schema_for(&args(&["spherecut", "probe", "--x"])), PROBE);
        assert_eq!(
            schema_for(&args(&["spherecut", "--json", "doctor"])),
            DOCTOR
        );
        assert_eq!(
            schema_for(&args(&["spherecut", "frame", "--json", "a.mp4"])),
            FRAME
        );
        assert_eq!(schema_for(&args(&["spherecut", "--json"])), CLI);
        assert_eq!(schema_for(&args(&["spherecut", "frobnicate"])), CLI);
        let (env, exit) = usage(
            &args(&["spherecut", "probe"]),
            "error: the following required arguments were not provided:\n  <FILES>...\n",
        );
        assert_eq!(exit, Exit::Usage);
        assert!(!env.ok());
        let e = env.error().unwrap();
        assert_eq!(e.code, codes::USAGE);
        assert!(e.message.starts_with("the following required"), "{e:?}");
        assert!(e.message.ends_with("<FILES>..."), "{e:?}");
    }

    #[test]
    fn frame_exits_by_its_code() {
        assert_eq!(frame_exit("usage"), Exit::Usage);
        for code in [
            "no_gpu",
            "adapter_not_found",
            "hw_unavailable",
            "hw_unsupported",
            "hw_failed",
            "import_failed",
            "no_decoder",
        ] {
            assert_eq!(frame_exit(code), Exit::Missing, "{code}");
        }
        assert_eq!(frame_exit("cancelled"), Exit::Cancelled);
        for code in [
            "not_found",
            "truncated",
            "not_equirect",
            "out_of_range",
            "output_exists",
            "io_error",
            "damaged",
            "gpu_failed",
            "internal",
        ] {
            assert_eq!(frame_exit(code), Exit::Failed, "{code}");
        }
        // Every code the run can end with is known.
        let all = all_codes();
        for code in codes::ALL.iter().chain(RenderError::CODES) {
            assert!(all.contains(code), "{code}");
        }
    }

    #[test]
    fn a_document_is_one_line() {
        let (env, _) = internal(PROBE, "line one\nline two");
        let line = env.to_line().unwrap();
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
        assert_eq!(
            line,
            "{\"schema\":\"spherecut.probe/1\",\"ok\":false,\"error\":{\"code\":\"internal\",\"message\":\"line one\\nline two\"},\"warnings\":[]}\n"
        );
    }
}
