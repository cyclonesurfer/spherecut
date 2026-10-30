//! First-pass camera and layout detection from container structure.
//!
//! This only identifies what kind of file we have. Parsing each vendor's
//! metadata belongs to the camera crate (M3). The signatures here come from
//! the research notes in `docs/formats/` and are confirmed against real
//! samples as they arrive; each guess lists its evidence so a wrong guess is
//! easy to diagnose.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::Serialize;

use crate::Source;
use crate::mp4::{Cursor, Mp4Box, Mp4Tree};
use crate::probe::{StreamInfo, StreamKind};

/// What the file appears to contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Stitched equirectangular, tagged with spherical metadata.
    EquirectTagged,
    /// 2:1 frame without spherical metadata: probably equirect, unconfirmed.
    EquirectLikely,
    /// Insta360 dual fisheye (.insv).
    Insta360DualFisheye,
    /// GoPro Max-style two-stream equi-angular cubemap (.360).
    GoProEac,
    /// DJI Osmo 360 dual-lens (.OSV).
    DjiDualFisheye,
    /// A camera's low-resolution companion (.lrv / .LRV / .LRF).
    LowResCompanion,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct CameraGuess {
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub layout: Layout,
    pub evidence: Vec<String>,
}

/// Insta360 files end with this ASCII marker after their metadata trailer.
pub const INSTA360_TRAILER_MAGIC: &[u8; 32] = b"8db42d694ccc418790edff439fe026bf";

/// The handler name of a `trak` (`mdia/hdlr`), trimmed of padding.
pub fn handler_name(trak: &Mp4Box) -> Option<String> {
    let hdlr = trak.path(&[b"mdia", b"hdlr"])?;
    let mut c = Cursor::new(&hdlr.payload);
    c.version_flags()?;
    c.u32()?; // pre_defined
    c.take(4)?; // handler_type
    c.take(12)?; // reserved
    let rest = c.remaining();
    // MP4 uses a C string; QuickTime a Pascal string (GoPro's is
    // "\tGoPro MET"). A leading non-printable byte that fits as a length
    // means Pascal.
    let s = match rest.split_first() {
        Some((&n, tail)) if n != 0 && !n.is_ascii_graphic() && n as usize <= tail.len() => {
            String::from_utf8_lossy(&tail[..n as usize]).into_owned()
        }
        _ => {
            let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            String::from_utf8_lossy(&rest[..end]).into_owned()
        }
    };
    let s = s
        .trim_matches(|c: char| c.is_control() || c.is_whitespace())
        .to_string();
    (!s.is_empty()).then_some(s)
}

fn sample_entry_types(trak: &Mp4Box) -> Vec<[u8; 4]> {
    trak.path(&[b"mdia", b"minf", b"stbl", b"stsd"])
        .map(|s| s.children.iter().map(|c| c.fourcc.0).collect())
        .unwrap_or_default()
}

fn has_trailer_magic(path: &Path) -> bool {
    spherecut_core::may_block("reading a file's trailer");
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    if f.seek(SeekFrom::End(-32)).is_err() {
        return false;
    }
    let mut b = [0u8; 32];
    f.read_exact(&mut b).is_ok() && &b == INSTA360_TRAILER_MAGIC
}

/// GoPro firmware string from `udta/FIRM`, e.g. "H19.03.02.00.00".
fn gopro_firmware(tree: &Mp4Tree) -> Option<String> {
    let firm = tree.moov()?.path(&[b"udta", b"FIRM"])?;
    let s = String::from_utf8_lossy(&firm.payload)
        .trim_matches(char::from(0))
        .trim()
        .to_string();
    (!s.is_empty()).then_some(s)
}

/// The camera that made the file, from its structure and from the
/// extension of [`Source::name`] (`.lrv`, `.lrf`).
pub fn guess(
    src: Source<'_>,
    tree: Option<&Mp4Tree>,
    streams: &[StreamInfo],
    tags: &BTreeMap<String, String>,
) -> Option<CameraGuess> {
    let ext = src
        .name
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut g = CameraGuess {
        vendor: None,
        model: None,
        firmware: None,
        layout: Layout::Unknown,
        evidence: Vec::new(),
    };
    let video: Vec<&StreamInfo> = streams
        .iter()
        .filter(|s| {
            s.kind == StreamKind::Video && !s.disposition.iter().any(|d| d == "attached_pic")
        })
        .collect();

    if let Some(tree) = tree {
        let traks: Vec<&Mp4Box> = tree.tracks().collect();
        let handlers: Vec<String> = traks.iter().filter_map(|t| handler_name(t)).collect();
        let entries: Vec<[u8; 4]> = traks.iter().flat_map(|t| sample_entry_types(t)).collect();

        // GoPro: GPMF telemetry track.
        if entries.contains(b"gpmd") || handlers.iter().any(|h| h.contains("GoPro MET")) {
            g.vendor = Some("GoPro".into());
            g.evidence
                .push("GPMF telemetry track (gpmd / \"GoPro MET\")".into());
            g.firmware = gopro_firmware(tree);
            if let Some(fw) = &g.firmware {
                g.evidence.push(format!("udta/FIRM = {fw}"));
            }
            let hevc_pair = video
                .iter()
                .filter(|s| s.codec.as_deref() == Some("hevc"))
                .count()
                >= 2;
            g.layout = if ext == "lrv" {
                Layout::LowResCompanion
            } else if hevc_pair {
                g.evidence.push("two HEVC video streams".into());
                Layout::GoProEac
            } else {
                Layout::Unknown
            };
        }

        // DJI: protobuf metadata tracks.
        if g.vendor.is_none()
            && (entries.contains(b"djmd")
                || handlers.iter().any(|h| h.contains("CAM meta"))
                || tree.top.iter().any(|b| b.fourcc.0 == *b"camd"))
        {
            g.vendor = Some("DJI".into());
            g.evidence
                .push("DJI metadata (djmd track / \"CAM meta\" / camd box)".into());
            g.layout = match ext.as_str() {
                "lrf" => Layout::LowResCompanion,
                _ if video.len() >= 2 => {
                    g.evidence
                        .push(format!("{} video streams (one per lens)", video.len()));
                    Layout::DjiDualFisheye
                }
                _ => Layout::Unknown,
            };
        }

        // Insta360: metadata trailer ending in a fixed magic, either loose
        // after `moov` or (newer X5 firmware) wrapped in a top-level `inst`
        // box. See docs/formats/insta360.md section 3.
        if g.vendor.is_none() && has_trailer_magic(src.open) {
            g.vendor = Some("Insta360".into());
            let wrapped = tree.top.iter().any(|b| b.fourcc.0 == *b"inst");
            g.evidence.push(if wrapped {
                "Insta360 trailer magic at end of file, inside an `inst` box".to_string()
            } else {
                format!(
                    "Insta360 trailer magic at end of file ({} bytes after the last box)",
                    tree.trailing_bytes
                )
            });
            g.layout = if ext == "lrv" {
                Layout::LowResCompanion
            } else {
                Layout::Insta360DualFisheye
            };
        }
    }

    if g.vendor.is_none() {
        if streams.iter().any(|s| {
            s.spherical.v2.is_some() || s.spherical.v1.as_ref().is_some_and(|v| v.spherical)
        }) {
            g.layout = Layout::EquirectTagged;
            g.evidence.push("spherical video metadata present".into());
        } else if let Some(v) = video
            .first()
            .and_then(|s| s.video.as_ref())
            .filter(|v| v.width > 0 && v.width == 2 * v.height)
        {
            g.layout = Layout::EquirectLikely;
            g.evidence.push(format!(
                "{}x{} is 2:1 but has no spherical metadata",
                v.width, v.height
            ));
        }
    }

    for key in ["com.apple.quicktime.make", "make", "Make"] {
        if let Some(v) = tags.get(key) {
            g.evidence.push(format!("{key} = {v}"));
            g.vendor.get_or_insert_with(|| v.clone());
        }
    }
    for key in ["com.apple.quicktime.model", "model", "Model"] {
        if let Some(v) = tags.get(key) {
            g.model.get_or_insert_with(|| v.clone());
        }
    }
    if let Some(enc) = tags.get("encoder") {
        g.evidence.push(format!("encoder = {enc}"));
    }

    (g.vendor.is_some() || g.layout != Layout::Unknown).then_some(g)
}
