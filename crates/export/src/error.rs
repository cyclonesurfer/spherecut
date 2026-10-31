//! What can go wrong in an export (M2 plan section 8.6): one enum,
//! naming the stage and, where there is one, the time of the frame
//! involved, with the underlying error's text.

use std::fmt;
use std::path::PathBuf;

use spherecut_core::time::Time;
use thiserror::Error;

/// The part of the export that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Opening or reading the clip's file.
    Decode,
    /// The GPU's projection, YUV pass or readback.
    Render,
    /// The encoder.
    Encode,
    /// The MP4 muxer.
    Mux,
    /// The boxes added after muxing: spherical video and audio.
    Metadata,
    /// The checks of the finished file.
    Check,
    /// The output file itself: its folder, the disk, putting it in place.
    File,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Decode => "decode",
            Stage::Render => "render",
            Stage::Encode => "encode",
            Stage::Mux => "mux",
            Stage::Metadata => "metadata",
            Stage::Check => "check",
            Stage::File => "file",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ExportError {
    /// Cancelled: nothing was left at the output.
    #[error("the export was cancelled")]
    Cancelled,
    /// The job can't be exported as asked: an empty trim, a size that
    /// isn't a preset, a missing media entry.
    #[error("{0}")]
    Invalid(String),
    /// Something is already at the output, and the job doesn't replace
    /// it (D16).
    #[error("{} already exists", .0.display())]
    OutputExists(PathBuf),
    /// No encoder the job can use for its codec.
    #[error("{0}")]
    NoEncoder(String),
    /// A stage failed, at the frame shown at `at` (sequence time) where
    /// there is one.
    #[error("{stage}{}: {message}", at.map(|t| format!(" at {:.3} s", t.to_secs_f64())).unwrap_or_default())]
    Failed {
        stage: Stage,
        at: Option<Time>,
        message: String,
    },
}

impl ExportError {
    pub fn failed(stage: Stage, at: Option<Time>, message: impl Into<String>) -> ExportError {
        ExportError::Failed {
            stage,
            at,
            message: message.into(),
        }
    }

    /// The stage that failed, for [`ExportError::Failed`].
    pub fn stage(&self) -> Option<Stage> {
        match self {
            ExportError::Failed { stage, .. } => Some(*stage),
            _ => None,
        }
    }
}
