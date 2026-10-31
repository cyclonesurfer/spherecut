//! A tool's failure, as the agent gets it (a1-plan section 4.2): a stable
//! code, the argument at fault, our own message, what would be valid,
//! and the next step, in under [`ERROR_CHARS`] characters of JSON. No
//! text read from a media file goes in one: errors name the argument
//! instead.

use serde::Serialize;
use serde_json::{Value, json};

use crate::limits::ERROR_CHARS;

/// The codes a tool fails with. Stable: agents may act on them, and
/// `docs/agents.md` lists every one.
pub mod codes {
    /// An argument of the wrong type, an unknown or missing one, or a
    /// time given as a bare number.
    pub const BAD_ARGUMENT: &str = "bad_argument";
    /// An argument outside its range: a time past the end, too many
    /// files or tiles.
    pub const OUT_OF_RANGE: &str = "out_of_range";
    /// A path outside every read root, or outside the write root for an
    /// output, or one that leads out of them (`..`, a symlink out).
    pub const OUTSIDE_ROOTS: &str = "outside_roots";
    pub const NOT_FOUND: &str = "not_found";
    pub const NOT_A_FILE: &str = "not_a_file";
    pub const NOT_A_FOLDER: &str = "not_a_folder";
    /// Not MP4/MOV or Matroska/WebM (D24).
    pub const UNSUPPORTED_CONTAINER: &str = "unsupported_container";
    pub const NO_VIDEO: &str = "no_video";
    pub const NO_AUDIO: &str = "no_audio";
    /// A view of a source that isn't an equirect (yet).
    pub const NOT_EQUIRECT: &str = "not_equirect";
    /// Truncated or corrupt where the call needed it.
    pub const DAMAGED: &str = "damaged";
    pub const DECODE_FAILED: &str = "decode_failed";
    pub const RENDER_FAILED: &str = "render_failed";
    /// No GPU, and no lavapipe either.
    pub const NO_GPU: &str = "no_gpu";
    /// An image over the inline cap or budget, with no file to write it
    /// to. (D27's `too_large` is a picture over `MAX_PIXELS`.)
    pub const IMAGE_TOO_LARGE: &str = "image_too_large";
    /// Writing is off, or the file system refused the new file. A path
    /// outside the write root is `outside_roots`.
    pub const WRITE_REFUSED: &str = "write_refused";
    /// Something is already at the output path; nothing is replaced.
    pub const OUTPUT_EXISTS: &str = "output_exists";
    /// A folder changed between pages.
    pub const CURSOR_STALE: &str = "cursor_stale";
    /// The call ran past its time limit.
    pub const TIMED_OUT: &str = "timed_out";
    /// Too many calls are waiting already, or too many renders.
    pub const BUSY: &str = "busy";
    /// The call was cancelled. The agent never sees this one: a cancelled
    /// call gets no response at all. A render cancelled with `job` is in
    /// the state `cancelled`, which isn't an error.
    pub const CANCELLED: &str = "cancelled";
    /// Spherecut's fault, not the agent's.
    pub const INTERNAL: &str = "internal";
    /// An edit the project's rules refuse: its op and the rule's code
    /// are in the message. Nothing was saved.
    pub const BAD_EDIT: &str = "bad_edit";
    /// The project's revision isn't `if_revision`, or its history was
    /// written with another revision of it.
    pub const STALE_REVISION: &str = "stale_revision";
    /// The step to undo or redo is the user's, not the agent's.
    pub const NOT_TOP: &str = "not_top";
    pub const NOTHING_TO_UNDO: &str = "nothing_to_undo";
    pub const NOTHING_TO_REDO: &str = "nothing_to_redo";
    /// Spherecut's window has the project open, or another call is
    /// writing it.
    pub const PROJECT_LOCKED: &str = "project_locked";
    /// Something other than Spherecut wrote the project since it was
    /// read; nothing was saved over it.
    pub const CHANGED_ON_DISK: &str = "changed_on_disk";
    /// The file isn't a Spherecut project.
    pub const NOT_PROJECT: &str = "not_project";
    /// A project file that doesn't read: malformed, or breaking a rule.
    pub const BAD_PROJECT: &str = "bad_project";
    /// Saved by a newer Spherecut.
    pub const NEWER_SCHEMA: &str = "newer_schema";
    /// Reading or writing a project failed partway: the disk, a
    /// permission. Nothing was saved.
    pub const IO_ERROR: &str = "io_error";
    /// A project's media file isn't there, or isn't beneath the roots.
    pub const MEDIA_MISSING: &str = "media_missing";
    /// A project's media file isn't the one the project was made with:
    /// its size or its first MiB differ.
    pub const MEDIA_CHANGED: &str = "media_changed";
    /// The output's disk hasn't room for the render: both numbers are in
    /// the message.
    pub const NO_SPACE: &str = "no_space";
    /// A job of a server that has ended: its render went with it.
    pub const JOB_LOST: &str = "job_lost";
    /// The project can't be exported as it is: its output isn't a preset,
    /// or its trim has no frames (D51).
    pub const CANNOT_EXPORT: &str = "cannot_export";
    /// No encoder this machine has for the project's codec (D51).
    pub const NO_ENCODER: &str = "no_encoder";
    /// The hardware encoder isn't there, turned the job down, or failed
    /// (D27, D3).
    pub const HW_UNAVAILABLE: &str = "hw_unavailable";
    pub const HW_UNSUPPORTED: &str = "hw_unsupported";
    pub const HW_FAILED: &str = "hw_failed";
    /// A render's stage failed: the encoder, the muxer, the metadata
    /// boxes, or the checks of the finished file (D51).
    pub const ENCODE_FAILED: &str = "encode_failed";
    pub const MUX_FAILED: &str = "mux_failed";
    pub const METADATA_FAILED: &str = "metadata_failed";
    pub const CHECK_FAILED: &str = "check_failed";

    /// Every code, for the docs and their test.
    pub const ALL: &[&str] = &[
        BAD_ARGUMENT,
        OUT_OF_RANGE,
        OUTSIDE_ROOTS,
        NOT_FOUND,
        NOT_A_FILE,
        NOT_A_FOLDER,
        UNSUPPORTED_CONTAINER,
        NO_VIDEO,
        NO_AUDIO,
        NOT_EQUIRECT,
        DAMAGED,
        DECODE_FAILED,
        RENDER_FAILED,
        NO_GPU,
        IMAGE_TOO_LARGE,
        WRITE_REFUSED,
        OUTPUT_EXISTS,
        CURSOR_STALE,
        TIMED_OUT,
        BUSY,
        CANCELLED,
        INTERNAL,
        BAD_EDIT,
        STALE_REVISION,
        NOT_TOP,
        NOTHING_TO_UNDO,
        NOTHING_TO_REDO,
        PROJECT_LOCKED,
        CHANGED_ON_DISK,
        NOT_PROJECT,
        BAD_PROJECT,
        NEWER_SCHEMA,
        IO_ERROR,
        MEDIA_MISSING,
        MEDIA_CHANGED,
        NO_SPACE,
        JOB_LOST,
        CANNOT_EXPORT,
        NO_ENCODER,
        HW_UNAVAILABLE,
        HW_UNSUPPORTED,
        HW_FAILED,
        ENCODE_FAILED,
        MUX_FAILED,
        METADATA_FAILED,
        CHECK_FAILED,
    ];
}

/// Why a tool call failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolError {
    pub code: &'static str,
    /// A JSON path into the arguments (`file`, `times[3]`, `view.fov`),
    /// or null when no one argument is at fault.
    pub argument: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            argument: None,
            message: message.into(),
            valid: None,
            next: None,
        }
    }

    /// The argument at fault.
    pub fn at(mut self, argument: impl Into<String>) -> Self {
        self.argument = Some(argument.into());
        self
    }

    /// What would be valid there.
    pub fn valid(mut self, valid: impl Into<String>) -> Self {
        self.valid = Some(valid.into());
        self
    }

    /// What to do next.
    pub fn next(mut self, next: impl Into<String>) -> Self {
        self.next = Some(next.into());
        self
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL, message)
    }

    /// `{"error": {...}}`, the error's JSON, cut to [`ERROR_CHARS`]
    /// characters: the message first, then `valid`, then `next` give way
    /// if it is longer, which a test makes sure no error of ours needs.
    pub fn to_json(&self) -> Value {
        let mut e = self.clone();
        for _ in 0..3 {
            let len = json!({ "error": &e }).to_string().chars().count();
            let Some(over) = len.checked_sub(ERROR_CHARS).filter(|o| *o > 0) else {
                break;
            };
            let field = if e.message.chars().count() > 40 {
                &mut e.message
            } else if let Some(v) = e.valid.as_mut().filter(|v| v.chars().count() > 20) {
                v
            } else if let Some(n) = e.next.as_mut() {
                n
            } else {
                &mut e.message
            };
            let keep = field.chars().count().saturating_sub(over + 1);
            *field = field.chars().take(keep).chain(['…']).collect();
        }
        json!({ "error": e })
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.argument {
            Some(a) => write!(f, "{}: {}: {}", self.code, a, self.message),
            None => write!(f, "{}: {}", self.code, self.message),
        }
    }
}

impl std::error::Error for ToolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_json_has_every_part() {
        let e = ToolError::new(codes::OUT_OF_RANGE, "is after the clip's last frame")
            .at("times[3]")
            .valid("0 to 31.965 s")
            .next("use \"31.965\"");
        assert_eq!(
            e.to_json().to_string(),
            r#"{"error":{"argument":"times[3]","code":"out_of_range","message":"is after the clip's last frame","next":"use \"31.965\"","valid":"0 to 31.965 s"}}"#
        );
        let bare = ToolError::new(codes::INTERNAL, "x").to_json();
        assert_eq!(bare["error"]["argument"], Value::Null);
        assert!(bare["error"].get("valid").is_none());
    }

    #[test]
    fn a_long_error_is_cut_to_the_limit() {
        let e = ToolError::new(codes::BAD_ARGUMENT, "m".repeat(2000))
            .at("a")
            .valid("v".repeat(300))
            .next("n".repeat(300));
        let text = e.to_json().to_string();
        assert!(text.chars().count() <= ERROR_CHARS, "{}", text.len());
        assert!(text.contains('…'));
    }

    /// Every code an export fails with reaches the agent as it is.
    #[test]
    fn the_exports_codes_are_codes() {
        for c in spherecut_export::ExportError::CODES {
            assert!(codes::ALL.contains(c), "{c}");
        }
    }

    #[test]
    fn codes_are_unique_snake_case() {
        let mut seen = std::collections::BTreeSet::new();
        for c in codes::ALL {
            assert!(seen.insert(c), "{c} twice");
            assert!(
                c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{c}"
            );
        }
    }
}
