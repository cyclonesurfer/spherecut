//! Media access for Spherecut: probing, the container metadata we parse
//! ourselves, capability checks, and (from M1) the `MediaBackend` trait
//! with its FFmpeg implementation.

// Only `ffi` and `hwprobe`, the FFI modules D11 allows, may use unsafe
// code (D28).
#![deny(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Once;

use thiserror::Error;

pub mod backend;
pub mod color;
pub mod decode;
pub mod detect;
pub mod doctor;
pub mod encode;
mod fallback;
#[allow(unsafe_code)]
mod ffi;
pub mod hw;
#[allow(unsafe_code)]
mod hwprobe;
pub mod inject;
pub mod mp4;
mod nal;
pub mod probe;
pub mod seek;
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
    /// While opening the file, FFmpeg asked to open another file or URL,
    /// which was refused (D24). A guard: the demuxers Spherecut allows,
    /// with `enable_drefs` off, aren't known to ask.
    #[error(
        "cannot open {}: FFmpeg tried to open `{url}` from inside it, which is refused: Spherecut lets FFmpeg read only the file it was given",
        path.display()
    )]
    NestedOpenRefused { path: PathBuf, url: String },
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
    /// This FFmpeg has no decoder for the stream's codec, which `codec`
    /// names as FFmpeg does (`hevc`).
    #[error("cannot decode {}: this FFmpeg has no {codec} decoder", path.display())]
    NoDecoder { path: PathBuf, codec: String },
    /// The stream's pictures are larger than any decoder Spherecut opens
    /// makes (D24).
    #[error("cannot decode {}: stream #{stream} is {size}", path.display())]
    PictureTooLarge {
        path: PathBuf,
        stream: usize,
        size: TooLarge,
    },
    /// The stream can't be decoded on the hardware device; software may
    /// still decode it.
    #[error("cannot decode {} on {kind}: {refusal}", path.display())]
    HwUnsupported {
        path: PathBuf,
        /// "VA-API" or "Vulkan".
        kind: String,
        refusal: HwRefusal,
    },
    /// Decoding or downloading a frame on the hardware device failed, and
    /// the device can't be told to have turned the stream down. It is the
    /// device's failure, never the file's damage, even when the device
    /// failed on a damaged packet (software, after a fallback, says
    /// whether it was, D30); software may still decode the stream. Once a
    /// decoder has returned it, every later call returns it again.
    #[error("decoding {} on {kind} failed: {reason}", path.display())]
    HwFailed {
        path: PathBuf,
        /// "VA-API" or "Vulkan".
        kind: String,
        reason: String,
    },
    /// The reason is part of the message, so it is not also the error's
    /// `source()`, which would print it twice in an error chain.
    #[error("{kind} device unavailable: {reason}")]
    Hardware {
        /// "VA-API" or "Vulkan", as the other hardware errors say it.
        kind: String,
        reason: HwError,
    },
    /// The decoder's frames are in a pixel format that is neither uploaded
    /// as it is nor converted by swscale (D30).
    #[error("cannot decode {}: swscale cannot convert its {pix_fmt} frames", path.display())]
    UnsupportedFormat { path: PathBuf, pix_fmt: String },
    /// A part of the media backend that a later M1 slice brings.
    #[error("{what} is not implemented yet")]
    NotYet { what: &'static str },
}

pub use ffi::{CONTAINERS, HwDevice, HwError, HwRefusal, MAX_PIXELS, TooLarge};
pub use hwprobe::{capture_ffmpeg_log, capture_hw_decode_log};

impl MediaError {
    /// Every code [`MediaError::code`] gives.
    pub const CODES: &'static [&'static str] = &[
        "not_found",
        "io_error",
        "not_a_file",
        "open_failed",
        "nested_open_refused",
        "unsupported_container",
        "decode_failed",
        "no_decoder",
        "too_large",
        "hw_unsupported",
        "hw_failed",
        "hw_unavailable",
        "unsupported_pixel_format",
        "not_implemented",
    ];

    /// The error's stable code, as the CLI's JSON gives it (D27): within a
    /// schema version a code keeps its name and meaning.
    /// `docs/cli-json.md` lists them all.
    pub fn code(&self) -> &'static str {
        match self {
            MediaError::Io { error, .. } if error.kind() == std::io::ErrorKind::NotFound => {
                "not_found"
            }
            MediaError::Io { .. } => "io_error",
            MediaError::NotAFile(_) => "not_a_file",
            MediaError::Open { .. } => "open_failed",
            MediaError::NestedOpenRefused { .. } => "nested_open_refused",
            MediaError::UnsupportedContainer { .. } => "unsupported_container",
            MediaError::Decode { .. } => "decode_failed",
            MediaError::NoDecoder { .. } => "no_decoder",
            MediaError::PictureTooLarge { .. } => "too_large",
            MediaError::HwUnsupported { .. } => "hw_unsupported",
            MediaError::HwFailed { .. } => "hw_failed",
            MediaError::Hardware { .. } => "hw_unavailable",
            MediaError::UnsupportedFormat { .. } => "unsupported_pixel_format",
            MediaError::NotYet { .. } => "not_implemented",
        }
    }

    /// Whether the hardware device, not the file, stopped the decode: it
    /// turned the stream down ([`HwUnsupported`](MediaError::HwUnsupported))
    /// or failed on it ([`HwFailed`](MediaError::HwFailed)). A decoder never
    /// reports either as damage, so that a caller can go on in software
    /// (D3, D14, D30).
    pub fn is_device_failure(&self) -> bool {
        matches!(
            self,
            MediaError::HwUnsupported { .. } | MediaError::HwFailed { .. }
        )
    }

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
            ffi::OpenError::Nested { url } => MediaError::NestedOpenRefused { path, url },
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io;

    use super::*;

    #[test]
    fn every_error_has_a_listed_code() {
        let path = || PathBuf::from("x.mp4");
        let io = |kind| MediaError::Io {
            path: path(),
            error: io::Error::from(kind),
        };
        let errors = [
            io(io::ErrorKind::NotFound),
            io(io::ErrorKind::PermissionDenied),
            MediaError::NotAFile(path()),
            MediaError::Open {
                path: path(),
                reason: "r".into(),
            },
            MediaError::NestedOpenRefused {
                path: path(),
                url: "http://h/x".into(),
            },
            MediaError::UnsupportedContainer {
                path: path(),
                format: "hls".into(),
                description: "Apple HTTP Live Streaming".into(),
            },
            MediaError::Decode {
                path: path(),
                reason: "r".into(),
            },
            MediaError::NoDecoder {
                path: path(),
                codec: "prores".into(),
            },
            MediaError::PictureTooLarge {
                path: path(),
                stream: 0,
                size: TooLarge {
                    width: 16400,
                    height: 8200,
                },
            },
            MediaError::HwUnsupported {
                path: path(),
                kind: "VA-API".into(),
                refusal: HwRefusal::Withdrawn,
            },
            MediaError::HwFailed {
                path: path(),
                kind: "Vulkan".into(),
                reason: "r".into(),
            },
            MediaError::Hardware {
                kind: "VA-API".into(),
                reason: HwError::NotBuiltIn("VA-API".into()),
            },
            MediaError::UnsupportedFormat {
                path: path(),
                pix_fmt: "xyz12le".into(),
            },
            MediaError::NotYet { what: "audio" },
        ];
        let mut seen = BTreeSet::new();
        for e in &errors {
            // No wildcard, so that a new variant fails to build here until
            // it is given a code above.
            match e {
                MediaError::Io { .. }
                | MediaError::NotAFile(_)
                | MediaError::Open { .. }
                | MediaError::NestedOpenRefused { .. }
                | MediaError::UnsupportedContainer { .. }
                | MediaError::Decode { .. }
                | MediaError::NoDecoder { .. }
                | MediaError::PictureTooLarge { .. }
                | MediaError::HwUnsupported { .. }
                | MediaError::HwFailed { .. }
                | MediaError::Hardware { .. }
                | MediaError::UnsupportedFormat { .. }
                | MediaError::NotYet { .. } => {}
            }
            assert!(MediaError::CODES.contains(&e.code()), "{e:?}");
            seen.insert(e.code());
        }
        assert_eq!(seen.len(), MediaError::CODES.len(), "{seen:?}");
    }

    /// The helpers that read files and decode say they can block, so a
    /// thread marked as the UI thread panics in them before anything is
    /// opened (M1 plan section 8.6). The path doesn't exist: nothing gets
    /// that far.
    #[test]
    fn the_ui_thread_may_not_open_or_probe_media() {
        use spherecut_core::blocking::{OnBlock, mark_ui_thread_with};

        use crate::backend::{FfmpegBackend, MediaBackend, VideoRequest};

        let on_ui_thread = |f: fn()| {
            let e = std::thread::spawn(move || {
                mark_ui_thread_with(OnBlock::Panic);
                f();
            })
            .join()
            .unwrap_err();
            e.downcast_ref::<String>().cloned().unwrap_or_default()
        };
        fn missing() -> PathBuf {
            PathBuf::from("/nonexistent/spherecut-ui-thread.mp4")
        }
        let m = on_ui_thread(|| drop(probe::probe(&missing())));
        assert!(
            m.starts_with("probing a media file would block the UI thread"),
            "{m}"
        );
        let m = on_ui_thread(|| {
            drop(FfmpegBackend::new().open_video(&missing(), &VideoRequest::default()))
        });
        assert!(
            m.starts_with("opening a media file would block the UI thread"),
            "{m}"
        );
        let m = on_ui_thread(|| drop(mp4::read_file(&missing())));
        assert!(
            m.starts_with("reading an MP4 file's boxes would block"),
            "{m}"
        );
        let m = on_ui_thread(|| drop(hw::render_node(None, None)));
        assert!(
            m.starts_with("reading the GPUs' render nodes in sysfs would block"),
            "{m}"
        );
        // Unmarked, they go ahead and fail on the path.
        assert!(probe::probe(&missing()).is_err());
    }

    #[test]
    fn a_refused_nested_open_says_so_as_before() {
        let e = MediaError::opening(
            Path::new("list.m3u8"),
            ffi::OpenError::Nested {
                url: "http://h/x.ts".into(),
            },
            |e| e.to_string(),
        );
        assert_eq!(e.code(), "nested_open_refused");
        assert_eq!(
            e.to_string(),
            "cannot open list.m3u8: FFmpeg tried to open `http://h/x.ts` from inside it, which is refused: Spherecut lets FFmpeg read only the file it was given"
        );
    }
}
