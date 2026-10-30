//! `doctor`: the CLI's doctor (a1-plan section 6.5 and amendments 4
//! and 5), without the package query, since no agent tool starts a
//! process.

use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use spherecut_media::doctor::Options;

use super::Ctx;
use crate::error::{ToolError, codes};
use crate::text;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
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
    cx.call.check()?;
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
        map.insert("warnings".into(), json!([]));
    }
    Ok(v)
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
