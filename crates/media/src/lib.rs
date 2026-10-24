//! Media access for Spherecut: probing, the container metadata we parse
//! ourselves, capability checks, and (from M1) the `MediaBackend` trait
//! with its FFmpeg implementation.

use std::path::{Path, PathBuf};
use std::sync::Once;

use thiserror::Error;

pub mod decode;
pub mod detect;
pub mod doctor;
pub mod encode;
mod ffi;
mod hwprobe;
pub mod inject;
pub mod mp4;
pub mod probe;
pub mod spherical;

pub use ffmpeg_next as ff;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("{}: {error}", path.display())]
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("{} is not a regular file", .0.display())]
    NotAFile(PathBuf),
    #[error("cannot open {}: {reason}", path.display())]
    Open { path: PathBuf, reason: String },
    /// FFmpeg would read the file with a demuxer Spherecut doesn't let it
    /// use (D24). `format` is FFmpeg's name for the demuxer, such as `hls`.
    #[error(
        "FFmpeg reads {} as {description} (`{format}`), and Spherecut opens only {} files",
        path.display(),
        ffi::CONTAINERS
    )]
    UnsupportedContainer {
        path: PathBuf,
        format: String,
        description: String,
    },
    #[error("cannot decode {}: {reason}", path.display())]
    Decode { path: PathBuf, reason: String },
    /// The stream can't be decoded on the hardware device; software may
    /// still decode it.
    #[error("cannot decode {} on {kind}: {refusal}", path.display())]
    HwUnsupported {
        path: PathBuf,
        /// "VA-API" or "Vulkan".
        kind: String,
        refusal: HwRefusal,
    },
    /// The reason is part of the message, so it is not also the error's
    /// `source()`, which would print it twice in an error chain.
    #[error("{kind} device unavailable: {reason}")]
    Hardware { kind: String, reason: HwError },
}

pub use ffi::{CONTAINERS, HwDevice, HwError, HwRefusal, MAX_PIXELS};

impl MediaError {
    /// The error for `path`, which [`ffi::open_input`] could not open as
    /// `e` says. `ffmpeg` words FFmpeg's own errors.
    pub(crate) fn opening(
        path: &Path,
        e: ffi::OpenError,
        ffmpeg: impl FnOnce(ff::Error) -> String,
    ) -> Self {
        let path = path.to_path_buf();
        match e {
            ffi::OpenError::Io(error) => MediaError::Io { path, error },
            ffi::OpenError::NotAFile => MediaError::NotAFile(path),
            ffi::OpenError::Container {
                format,
                description,
            } => MediaError::UnsupportedContainer {
                path,
                format,
                description,
            },
            ffi::OpenError::Ffmpeg(e) => MediaError::Open {
                path,
                reason: ffmpeg(e),
            },
            e @ ffi::OpenError::Nested { .. } => MediaError::Open {
                path,
                reason: e.to_string(),
            },
        }
    }
}

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
