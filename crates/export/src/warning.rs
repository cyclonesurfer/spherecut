//! What an export did that the project didn't say: each with a stable
//! code, which `spherecut render --json` gives as a warning's `code`
//! (D51), and a message for people. A hardware path the export didn't
//! take is in the report's own fields instead (`Report::fallback`,
//! `Report::decode`), as frame's decode is (D3).

use std::fmt;

/// One thing the export did that wasn't asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// One of [`codes`].
    pub code: &'static str,
    pub message: String,
}

impl Warning {
    pub fn new(code: &'static str, message: impl Into<String>) -> Warning {
        Warning {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The warnings' codes. Within a schema version a code keeps its name
/// and meaning; `docs/cli-json.md` lists them all.
pub mod codes {
    /// A stereo source: its left eye is exported (frame's code too).
    pub const STEREO_LEFT_EYE: &str = "stereo_left_eye";
    /// A stereo layout the export doesn't know: the whole picture is
    /// exported, as mono (frame's code too).
    pub const STEREO_UNKNOWN: &str = "stereo_unknown";
    /// The source covers part of the sphere: the rest is black (frame's
    /// code too).
    pub const CROP_APPLIED: &str = "crop_applied";
    /// The source's Spherical Video V2 pose is not applied (frame's code
    /// too).
    pub const POSE_NOT_APPLIED: &str = "pose_not_applied";
    /// H.264 wider than 4096 pixels, which needs level 6.
    pub const H264_LEVEL_6: &str = "h264_level_6";
    /// The clip has no audio to export.
    pub const NO_AUDIO: &str = "no_audio";
    /// Copy was asked, and the audio is encoded again as AAC: its codec
    /// or timing can't be copied, or ambiX goes into a flat export.
    pub const AUDIO_ENCODED: &str = "audio_encoded";
    /// A 360 export turned from the source: the ambiX sound field isn't
    /// turned with it.
    pub const AMBIX_NOT_TURNED: &str = "ambix_not_turned";
    /// The clip's audio ends before the export does.
    pub const AUDIO_ENDS_EARLY: &str = "audio_ends_early";
    /// The output's folder couldn't say how much room it has.
    pub const FREE_SPACE_UNKNOWN: &str = "free_space_unknown";

    pub const ALL: &[&str] = &[
        STEREO_LEFT_EYE,
        STEREO_UNKNOWN,
        CROP_APPLIED,
        POSE_NOT_APPLIED,
        H264_LEVEL_6,
        NO_AUDIO,
        AUDIO_ENCODED,
        AMBIX_NOT_TURNED,
        AUDIO_ENDS_EARLY,
        FREE_SPACE_UNKNOWN,
    ];
}
