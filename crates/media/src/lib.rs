//! Media access for Spherecut: probing, the container metadata we parse
//! ourselves, capability checks, and (from M1) the `MediaBackend` trait
//! with its FFmpeg implementation.

use std::path::PathBuf;
use std::sync::Once;

use thiserror::Error;

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
}

static INIT: Once = Once::new();

/// Initialise FFmpeg once per process and keep its own logging quiet; our
/// errors carry the details instead.
pub fn init() {
    INIT.call_once(|| {
        if let Err(e) = ff::init() {
            tracing::warn!("FFmpeg init: {e}");
        }
        ff::util::log::set_level(ff::util::log::Level::Fatal);
    });
}
