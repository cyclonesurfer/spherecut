//! The CLI's JSON contract (DECISIONS D27, `docs/cli-json.md`). With
//! `--json`, a run prints exactly one [`Envelope`] on stdout as one line
//! of compact JSON, on success and on failure, and exits with an [`Exit`]
//! status. stderr carries only logs for people. A1's `spherecut call` and
//! `spherecut mcp` are to reuse these types.
//!
//! Within a schema version, fields are only added: a field is never
//! removed, renamed or given another type, and a code never renamed or
//! given another meaning. Anything else is a new version.

use std::ffi::OsString;
use std::path::Path;

use serde::Serialize;
use spherecut_media::MediaError;
use spherecut_media::doctor::{DoctorReport, Severity};
use spherecut_media::probe::{self, ProbeError, ProbeReport};

/// The schema of `spherecut probe --json`.
pub const PROBE: &str = "spherecut.probe/1";
/// The schema of `spherecut doctor --json`.
pub const DOCTOR: &str = "spherecut.doctor/1";
/// The schema of a usage error that names no subcommand.
pub const CLI: &str = "spherecut/1";

/// How a run ends, as its exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Exit {
    /// The run did what was asked.
    Ok = 0,
    /// An input or a check failed: a file probe could not read, or a
    /// problem of severity error that doctor found.
    Failed = 1,
    /// The command line is wrong.
    Usage = 2,
    /// Reserved for `frame` and the GUI: a codec or device the command
    /// itself needs is missing (no GPU, no display, no decoder).
    Missing = 3,
    /// Reserved for `frame`: interrupted (SIGINT), leaving no output.
    Cancelled = 4,
}

impl From<Exit> for std::process::ExitCode {
    fn from(e: Exit) -> Self {
        std::process::ExitCode::from(e as u8)
    }
}

/// The CLI's own codes, for `error.code` when the run as a whole failed.
/// The codes of single files come from [`MediaError::code`] and
/// [`probe::codes`].
pub mod codes {
    /// The command line is wrong (exit 2).
    pub const USAGE: &str = "usage";
    /// probe: at least one file failed; each file's `error` says why.
    pub const FILES_FAILED: &str = "files_failed";
    /// doctor: at least one problem is of severity error.
    pub const DOCTOR_ERRORS: &str = "doctor_errors";
    /// A failure of Spherecut's own, to be reported as a bug.
    pub const INTERNAL: &str = "internal";

    /// Every code above.
    pub const ALL: &[&str] = &[USAGE, FILES_FAILED, DOCTOR_ERRORS, INTERNAL];
}

/// Every code the JSON can carry, as an error or a warning, sorted and
/// without repeats: the CLI's own, [`MediaError::CODES`] and
/// [`probe::codes::ALL`].
pub fn all_codes() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = codes::ALL
        .iter()
        .chain(MediaError::CODES)
        .chain(probe::codes::ALL)
        .copied()
        .collect();
    all.sort_unstable();
    all.dedup();
    all
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
/// doctor report their findings in `result` and have none of these yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
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

/// Whether the command line asks for JSON: `--json` before any `--`.
/// Checked before parsing, so that a command line that doesn't parse can
/// still be answered in JSON.
pub fn wants_json(args: &[OsString]) -> bool {
    args.iter()
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
    }

    #[test]
    fn usage_errors_name_the_subcommand_when_they_can() {
        assert_eq!(schema_for(&args(&["spherecut", "probe", "--x"])), PROBE);
        assert_eq!(
            schema_for(&args(&["spherecut", "--json", "doctor"])),
            DOCTOR
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
