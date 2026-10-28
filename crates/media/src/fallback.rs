//! Hardware decoding that goes on in software (M1 plan section 3.5,
//! DECISIONS D3, D14 and D30).
//!
//! With [`Fallback::Software`], a device that can't be opened or turns
//! the stream down at open, or fails on its first frame, has the stream
//! opened in software instead; one that fails later has it reopened in
//! software, which goes on from the frame after the last one returned
//! ([`FfmpegVideo::fell_back`]), so the frames handed out are the same as
//! software's with no gap and no repeat. A seek the device failed in is
//! done again. Either way the decoder says so once, with a
//! `tracing::warn!` giving D14's reason, a [`DecoderEvent::FellBack`] and
//! [`DecodePath::FellBack`] on every frame after. With [`Fallback::Fail`]
//! the error is returned: `HwUnsupported`, `HwFailed`, or `Hardware` for
//! a device that won't open.
//!
//! [`DecoderEvent::FellBack`]: crate::backend::DecoderEvent::FellBack
//! [`DecodePath::FellBack`]: crate::backend::DecodePath::FellBack

use std::path::{Path, PathBuf};
use std::sync::Arc;

use spherecut_core::{StopToken, Time};

use crate::backend::{
    DecodeMode, DecodePath, DecodeStats, Fallback, FallbackReason, HwKind, NextFrame, SeekMode,
    SeekOutcome, VideoDecode, VideoRequest, VideoStreamInfo,
};
use crate::seek::{FfmpegVideo, On};
use crate::{HwDevice, MediaError};

/// Open `path` as `req` asks, on a device of kind `kind` (`device` gives
/// it, and may fail to), falling back as `fallback` says.
pub(crate) fn open(
    path: &Path,
    req: &VideoRequest,
    kind: HwKind,
    device: impl FnOnce() -> Result<Arc<HwDevice>, MediaError>,
    fallback: Fallback,
) -> Result<Box<dyn VideoDecode>, MediaError> {
    let on = match stand_in(req) {
        Some(on) => Ok(on),
        None => device().map(|dev| On::Device { kind, dev }),
    };
    let hw = on.and_then(|on| FfmpegVideo::open_on(path, req, on));
    match (hw, fallback) {
        (Ok(v), Fallback::Fail) => Ok(Box::new(v)),
        (Err(e), Fallback::Fail) => Err(e),
        (Ok(v), Fallback::Software) => Ok(Box::new(FallbackVideo {
            inner: v,
            reopen: Some(Reopen {
                path: path.to_path_buf(),
                req: software(req),
                kind,
            }),
            before: Before::default(),
        })),
        (Err(e), Fallback::Software) => {
            let reason = match e {
                MediaError::HwUnsupported { refusal, .. } => FallbackReason::Refused(refusal),
                MediaError::Hardware { .. } => FallbackReason::DeviceUnavailable(e.to_string()),
                MediaError::HwFailed { .. } => FallbackReason::Failed {
                    after_pts: None,
                    error: e.to_string(),
                },
                // The file's own error, which software would meet too.
                e => return Err(e),
            };
            warn(path, kind, &reason);
            let mut sw = FfmpegVideo::open(path, &software(req))?;
            sw.fell_back(kind, reason, None);
            Ok(Box::new(sw))
        }
    }
}

/// Fault injection's stand-in for the device, when the request asks for
/// one.
#[cfg(feature = "fault-injection")]
fn stand_in(req: &VideoRequest) -> Option<On> {
    match (&req.decode, req.fault) {
        (DecodeMode::Hardware { kind, .. }, Some(plan)) if plan.stand_in => {
            Some(On::StandIn(*kind))
        }
        _ => None,
    }
}

#[cfg(not(feature = "fault-injection"))]
fn stand_in(_: &VideoRequest) -> Option<On> {
    None
}

/// `req` for software: its threads, and no fault.
fn software(req: &VideoRequest) -> VideoRequest {
    VideoRequest {
        decode: DecodeMode::Software,
        #[cfg(feature = "fault-injection")]
        fault: None,
        ..req.clone()
    }
}

fn warn(path: &Path, kind: HwKind, reason: &FallbackReason) {
    tracing::warn!(
        path = %path.display(),
        "decoding in software instead of with {kind}: {reason}"
    );
}

/// What a hardware decode reopens in software with.
#[derive(Debug)]
struct Reopen {
    path: PathBuf,
    req: VideoRequest,
    kind: HwKind,
}

/// What the decoder before the fallback did, for [`DecodeStats`].
#[derive(Debug, Clone, Copy, Default)]
struct Before {
    frames_decoded: u64,
    frames_returned: u64,
    decode_ms_total: f64,
    download_buffers: u64,
    reopens: u64,
}

/// A hardware decode that falls back to software if the device fails on
/// it ([`Fallback::Software`]).
#[derive(Debug)]
struct FallbackVideo {
    inner: FfmpegVideo,
    /// `None` once it has fallen back.
    reopen: Option<Reopen>,
    before: Before,
}

impl FallbackVideo {
    /// Reopen in software after the device failed with `e`, going on
    /// where the hardware decoder left off. `Err` when it has fallen back
    /// already, or `e` isn't the device's.
    fn fall_back(&mut self, e: MediaError) -> Result<(), MediaError> {
        if !e.is_device_failure() {
            return Err(e);
        }
        let Some(reopen) = self.reopen.take() else {
            return Err(e);
        };
        let reason = FallbackReason::Failed {
            after_pts: self.inner.last_returned(),
            error: e.to_string(),
        };
        warn(&reopen.path, reopen.kind, &reason);
        let mut sw = FfmpegVideo::open(&reopen.path, &reopen.req)?;
        sw.fell_back(reopen.kind, reason, Some(&mut self.inner));
        let s = self.inner.stats();
        self.before = Before {
            frames_decoded: self.before.frames_decoded + s.frames_decoded,
            frames_returned: self.before.frames_returned + s.frames_returned,
            decode_ms_total: self.before.decode_ms_total
                + s.mean_decode_ms * s.frames_decoded as f64,
            download_buffers: self.before.download_buffers + s.download_buffers,
            reopens: self.before.reopens + s.reopens,
        };
        self.inner = sw;
        Ok(())
    }
}

impl VideoDecode for FallbackVideo {
    fn info(&self) -> &VideoStreamInfo {
        self.inner.info()
    }

    fn decode_path(&self) -> &DecodePath {
        self.inner.decode_path()
    }

    fn seek(
        &mut self,
        at: Time,
        mode: SeekMode,
        stop: &StopToken,
    ) -> Result<SeekOutcome, MediaError> {
        match self.inner.seek(at, mode, stop) {
            Err(e) => {
                self.fall_back(e)?;
                self.inner.seek_again(at, mode, stop)
            }
            ok => ok,
        }
    }

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        match self.inner.next_frame(stop) {
            Err(e) => {
                self.fall_back(e)?;
                self.inner.next_frame(stop)
            }
            ok => ok,
        }
    }

    fn catch_up(&mut self, until: Option<Time>) {
        self.inner.catch_up(until);
    }

    fn take_events(&mut self) -> Vec<crate::backend::DecoderEvent> {
        self.inner.take_events()
    }

    fn stats(&self) -> DecodeStats {
        let s = self.inner.stats();
        let frames = self.before.frames_decoded + s.frames_decoded;
        let ms = self.before.decode_ms_total + s.mean_decode_ms * s.frames_decoded as f64;
        DecodeStats {
            frames_decoded: frames,
            frames_returned: self.before.frames_returned + s.frames_returned,
            mean_decode_ms: if frames > 0 { ms / frames as f64 } else { 0.0 },
            download_buffers: self.before.download_buffers + s.download_buffers,
            reopens: self.before.reopens + s.reopens,
            ..s
        }
    }
}
