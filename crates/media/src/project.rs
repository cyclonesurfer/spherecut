//! A project's media entry from a probed video (M2 plan section 5.3):
//! what the GUI's untitled project and A2's `project` create both make;
//! and from M3 a camera file's source (M3 plan 11.5) and whether the
//! built-in geometry table has since superseded a source's geometry
//! (11.4).
//!
//! [`project_media`] still makes the equirect entry it made in M2: a
//! camera file's source is [`camera_source`]'s, which M3.10's open flow
//! and an agent's `project create` call beside it.

use std::path::Path;

use spherecut_core::project::{self, DualFisheye, Fingerprint, Media, MediaId, MediaVideo, file};
use spherecut_sphere::rig::{Geometry, Status};

use crate::Source;
use crate::backend::{Timing, VideoStreamInfo};
use crate::probe::ProbeReport;

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
        source: project::Source::Equirect {},
        caps: project::Caps::default(),
    })
}

/// The two-lens source of the camera file `source` opens, which `report`
/// probed (M3 plan 11.5): the calibration `camera::insta360` finds in its
/// trailer, kept raw and parsed with its hash, the geometry table's
/// entry for its model and firmware (`known::lookup`), `Tune::default()`,
/// gains off and the default front track, its camera's make, model,
/// firmware and serial as the file names them. `None` for a file with no
/// calibration a stitch reads, which stays equirect, keeping
/// `not_equirect`'s doubt so it shows as lenses. An error, in words, when
/// the trailer can't be read again or its calibration can't be one.
#[expect(unused_variables, reason = "M3.9 unit (b) writes the body")]
pub fn camera_source(
    source: Source<'_>,
    report: &ProbeReport,
) -> Result<Option<DualFisheye>, String> {
    todo!("M3.9 unit (b)")
}

/// A media's geometry that the built-in table has since superseded (M3
/// plan 11.4): the table's status and evidence, which the app's "Use
/// it?" notice, `render`'s warning and `check`'s `provisional_geometry`
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superseded {
    pub status: Status,
    pub evidence: String,
}

/// Whether `table`, the table's geometry for a camera, supersedes
/// `stored`, a media's (11.4): when `stored` is provisional and `table`
/// is settled, or is provisional on newer evidence. Only an edit changes
/// a project's geometry, so this only says so.
#[expect(unused_variables, reason = "M3.9 unit (b) writes the body")]
pub fn superseded(stored: &Geometry, table: &Geometry) -> Option<Superseded> {
    todo!("M3.9 unit (b)")
}

/// [`superseded`] for `source`'s geometry against the table's entry for
/// its camera's model and firmware (`camera::insta360::known::lookup`).
#[expect(unused_variables, reason = "M3.9 unit (b) writes the body")]
pub fn superseded_by_table(source: &DualFisheye) -> Option<Superseded> {
    todo!("M3.9 unit (b)")
}
