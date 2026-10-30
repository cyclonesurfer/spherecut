//! `doctor`: the CLI's doctor (a1-plan section 6.5 and amendments 4
//! and 5), without the package query, since no agent tool starts a
//! process; and, with a file, the decode path look and `frame` take for
//! it, and how fast it decodes there.

use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Deserialize;
use serde_json::{Value, json};
use spherecut_core::TimeSpec;
use spherecut_media::backend::{SeekMode, Timing};
use spherecut_media::doctor::Options;

use super::{Ctx, look};
use crate::contract::Warning;
use crate::error::{ToolError, codes};
use crate::limits::SPEED_TIME;
use crate::still::{self, Failure, Still, number};
use crate::text;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub file: Option<PathBuf>,
    #[serde(default)]
    pub speed: bool,
    pub render_node: Option<PathBuf>,
}

/// The render nodes doctor may open: a DRM render node, named as the
/// kernel names them, and a character device rather than a link to
/// something else. It is a device, not media, so the roots don't apply;
/// `/dev/dri` is root's, so nothing an agent can write is behind it.
fn check_node(node: &Path) -> Result<(), ToolError> {
    let name = node.to_str().unwrap_or("");
    let named = name
        .strip_prefix("/dev/dri/renderD")
        .is_some_and(|n| !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit()));
    let bad = || {
        ToolError::new(codes::BAD_ARGUMENT, "isn't a DRM render node")
            .at("render_node")
            .valid("/dev/dri/renderD followed by a number, such as /dev/dri/renderD128")
    };
    if !named {
        return Err(bad());
    }
    match std::fs::symlink_metadata(node) {
        Ok(m) if m.file_type().is_char_device() => Ok(()),
        Ok(_) => Err(bad()),
        Err(_) => Err(ToolError::new(codes::NOT_FOUND, "no such render node")
            .at("render_node")
            .next("call doctor without render_node: it finds the GPU's own")),
    }
}

pub fn run(req: Request, cx: &Ctx) -> Result<Value, ToolError> {
    if let Some(node) = &req.render_node {
        check_node(node)?;
    }
    if req.speed && req.file.is_none() {
        return Err(
            ToolError::new(codes::BAD_ARGUMENT, "needs a file to decode")
                .at("speed")
                .valid("with file"),
        );
    }
    cx.call.check()?;
    let mut warnings = Vec::new();
    let file = match &req.file {
        Some(f) => Some(file(f, &req, cx, &mut warnings)?),
        None => None,
    };
    // The lookup hardware decoding uses (D30), as the CLI's doctor does.
    let node = spherecut_media::hw::render_node(None, req.render_node.as_deref());
    let report = spherecut_media::doctor::run_with(
        &node.path,
        Options {
            query_packages: false,
        },
    );
    let mut v = serde_json::to_value(&report)
        .map_err(|e| ToolError::internal(format!("doctor's report didn't serialize: {e}")))?;
    // FFmpeg's own words, which a file's contents can steer once doctor
    // takes a file (A1.3).
    text::untrusted_under(&mut v, "ffmpeg_log");
    if let Value::Object(map) = &mut v {
        if let Some(mut f) = file {
            // A fallback's reason can carry FFmpeg's words.
            text::untrusted_under(&mut f, "reason");
            map.insert("file".into(), f);
        }
        map.insert("warnings".into(), json!(look::warnings_json(&warnings)));
    }
    Ok(v)
}

/// The decode path `path` gets, as look and `frame` open it: VA-API on
/// the rendering GPU's node (or `render_node`), else software (D39), its
/// first frame decoded to see; and with `speed`, the frames after it
/// decoded for up to [`SPEED_TIME`].
fn file(
    path: &Path,
    req: &Request,
    cx: &Ctx,
    warnings: &mut Vec<Warning>,
) -> Result<Value, ToolError> {
    let opened = cx.roots.open_file("file", path)?;
    let stop = still::Stop::new(cx.call.token(), cx.call.interrupt());
    let opts = still::Options {
        // Any video: doctor doesn't draw it.
        equirect: true,
        render_node: req.render_node.clone(),
        ..still::Options::default()
    };
    let failed = |f: &Failure| {
        if f.code == codes::CANCELLED {
            stopped(cx)
        } else {
            look::file_failed(f)
        }
    };
    let probed = still::probe(opened.source(), &opts, &stop, warnings).map_err(|f| failed(&f))?;
    // The GPU look renders on picks the node, as it does for look. Without
    // one the node is render_node's or the first; look itself would say
    // no_gpu.
    let gpu = look::gpu().ok();
    let mut s = Still::open(
        opened.source(),
        &probed,
        gpu.as_deref(),
        &opts,
        &stop,
        warnings,
    )
    .map_err(|f| failed(&f))?;
    let info = s.info().clone();
    let first = s
        .seek(TimeSpec::default(), SeekMode::Exact, warnings)
        .map_err(|f| {
            if f.code == codes::CANCELLED {
                stopped(cx)
            } else {
                look::time_failed(&f, "file", &info)
            }
        })?;
    let mut decode = s.decode_info(&first);
    let source = s.source_info();
    let mut out = json!({
        "path": text::field(&path.to_string_lossy()),
        "stream": source.stream,
        "codec": source.codec,
        "size": [source.size.width, source.size.height],
        "pix_fmt": source.pix_fmt,
    });
    if req.speed {
        let time = SPEED_TIME.min(cx.call.remaining() / 2);
        let started = Instant::now();
        let mut frames = 0u64;
        let mut ended = false;
        while started.elapsed() < time {
            match s.next(warnings) {
                Ok(Some(frame)) => {
                    frames += 1;
                    decode = s.frame_decode_info(&frame);
                }
                Ok(None) => {
                    ended = true;
                    break;
                }
                Err(f) if f.code == codes::CANCELLED => return Err(stopped(cx)),
                Err(f) => return Err(look::time_failed(&f, "file", &info)),
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        let fps = if seconds > 0.0 {
            frames as f64 / seconds
        } else {
            0.0
        };
        let stream_fps = match info.timing {
            Timing::Constant(r) => Some(r.to_f64()),
            Timing::Variable { average } => average.map(|r| r.to_f64()),
        };
        let realtime = stream_fps.filter(|&r| r > 0.0).map(|r| fps / r);
        out["speed"] = json!({
            "frames": frames,
            "seconds": round3(seconds),
            "fps": round3(fps),
            "stream_fps": stream_fps.map(round3),
            "realtime": realtime.map(round3),
            "ended": ended,
        });
    }
    out["decode"] = serde_json::to_value(&decode)
        .map_err(|e| ToolError::internal(format!("decode info: {e}")))?;
    Ok(out)
}

/// `v` to three decimals, as the report's other numbers are.
fn round3(v: f64) -> f64 {
    number(v).parse().unwrap_or(v)
}

/// A stop, as the call's deadline or cancel has it.
fn stopped(cx: &Ctx) -> ToolError {
    match cx.call.check() {
        Err(e) if e.code == codes::TIMED_OUT => {
            e.next("call without speed, or ask the user for a smaller file")
        }
        Err(e) => e,
        Ok(()) => ToolError::new(codes::CANCELLED, "the call was stopped"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_render_nodes() {
        for bad in [
            "/dev/dri/card0",
            "/dev/dri/renderD",
            "/dev/dri/renderD1/../../x",
            "a.mp4",
            "/dev/dri/renderD12x",
            "/dev/dri/renderD1234567",
        ] {
            let e = check_node(Path::new(bad)).err();
            assert_eq!(e.map(|e| e.code), Some(codes::BAD_ARGUMENT), "{bad}");
        }
        // Named right, but not there (in this container there is none).
        if std::fs::symlink_metadata("/dev/dri/renderD199").is_err() {
            let e = check_node(Path::new("/dev/dri/renderD199")).err();
            assert_eq!(e.map(|e| e.code), Some(codes::NOT_FOUND));
        }
        if let Ok(m) = std::fs::symlink_metadata("/dev/dri/renderD128")
            && m.file_type().is_char_device()
        {
            assert!(check_node(Path::new("/dev/dri/renderD128")).is_ok());
        }
    }
}
