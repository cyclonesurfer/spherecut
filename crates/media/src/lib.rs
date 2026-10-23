//! Media access for Spherecut: probing, the container metadata we parse
//! ourselves, capability checks, and (from M1) the `MediaBackend` trait
//! with its FFmpeg implementation.

use std::path::PathBuf;
use std::sync::Once;

use thiserror::Error;

pub mod decode;
pub mod detect;
pub mod doctor;
pub mod encode;
mod ffi;
pub mod inject;
pub mod mp4;
pub mod probe;
pub mod spherical;

pub use ffmpeg_next as ff;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} is not a regular file", .0.display())]
    NotAFile(PathBuf),
    #[error("cannot open {}: {reason}", path.display())]
    Open { path: PathBuf, reason: String },
    #[error("cannot decode {}: {reason}", path.display())]
    Decode { path: PathBuf, reason: String },
    /// The reason is part of the message, so it is not also the error's
    /// `source()`, which would print it twice in an error chain.
    #[error("{kind} device unavailable: {reason}")]
    Hardware { kind: String, reason: HwError },
}

pub use ffi::{HwDevice, HwError};

static INIT: Once = Once::new();

/// Initialise FFmpeg once per process, and send its log to `tracing` under
/// the target "ffmpeg" (FFmpeg's verbose level as debug, its debug as
/// trace). FFmpeg only formats messages the subscriber accepts for that
/// target, so install the subscriber first, or call
/// [`sync_ffmpeg_log_level`] after changing it.
pub fn init() {
    INIT.call_once(|| {
        ffi::route_log_to_tracing();
        if let Err(e) = ff::init() {
            tracing::warn!("FFmpeg init: {e}");
        }
        tracing::debug!(version = %ffi::ffmpeg_version(), "FFmpeg initialised");
    });
}

/// Match FFmpeg's log level to what the `tracing` subscriber now accepts
/// for target "ffmpeg".
pub fn sync_ffmpeg_log_level() {
    ffi::sync_log_level();
}
