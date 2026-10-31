//! A project's media entry from a probed video (M2 plan section 5.3):
//! what the GUI's untitled project and A2's `project` create both make.

use std::path::Path;

use spherecut_core::project::{Fingerprint, Media, MediaId, MediaVideo, file};

use crate::Source;
use crate::backend::{Timing, VideoStreamInfo};

/// The media entry for the video `source` opens, whose stream opened as
/// `info`, recorded in the project as `path` (absolute). It reads the
/// file's size and its first MiB through `source.open`, so an agent's
/// entry is of the file its roots opened (D45).
pub fn project_media(
    source: Source<'_>,
    path: &Path,
    info: &VideoStreamInfo,
) -> Result<Media, String> {
    let rate = match info.timing {
        Timing::Constant(r) | Timing::Variable { average: Some(r) } => r,
        Timing::Variable { average: None } => return Err("its frame rate isn't known".into()),
    };
    let duration = info.duration.ok_or("its length isn't known")?;
    let named = |e: std::io::Error| format!("{}: {e}", source.name.display());
    let bytes = std::fs::metadata(source.open).map_err(named)?.len();
    let head = file::head_hash(source.open).map_err(named)?;
    Ok(Media {
        id: MediaId::NEW,
        path: std::path::absolute(path).map_err(|e| e.to_string())?,
        fingerprint: Fingerprint { bytes, head },
        video: MediaVideo {
            stream: u32::try_from(info.index).map_err(|e| e.to_string())?,
            width: info.size.0,
            height: info.size.1,
            rate,
            duration,
        },
    })
}
