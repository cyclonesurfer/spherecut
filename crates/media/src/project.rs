//! A project's media entry from a probed video (M2 plan section 5.3):
//! what the GUI's untitled project and A2's `project` create both make;
//! and from M3 a camera file's source (M3 plan 11.5) and whether the
//! built-in geometry table has since superseded a source's geometry
//! (11.4).
//!
//! [`project_media`] still makes the equirect entry it made in M2: a
//! camera file's source is [`camera_source`]'s, which M3.10's open flow
//! and an agent's `project create` call beside it.

use std::fs::File;
use std::path::Path;

use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{known, parse_metadata, read_record, read_trailer, stitching};
use spherecut_core::project::{
    self, CalibSnapshot, Camera, DualFisheye, Fingerprint, Gains, Media, MediaId, MediaVideo,
    RawStrings, file,
};
use spherecut_sphere::rig::{Geometry, Status, Tune};

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
/// trailer, kept raw and parsed with its hash; the geometry of
/// `camera::insta360::stitching`'s entry, the table's row for its model
/// and firmware with the generation its field 136 names (never
/// `known::lookup`'s row as it stands, which reads V6 and which a rig
/// refuses on a trailer of V3 alone); `Tune::default()`, gains off and
/// the default front track; and its camera's make, model, firmware and
/// serial as the file names them. `None` for a file with no
/// calibration a stitch reads, which stays equirect, keeping
/// `not_equirect`'s doubt so it shows as lenses. An error, in words, when
/// the trailer can't be read again or its calibration can't be one.
///
/// It calls [`spherecut_core::may_block`] before it opens the file, as
/// probe does, so on the UI thread it panics in a debug build. The
/// trailer is read again here, as `agent::still::pair_stitching` reads
/// it, rather than kept from probe, which holds no raw strings.
pub fn camera_source(
    source: Source<'_>,
    report: &ProbeReport,
) -> Result<Option<DualFisheye>, String> {
    /// The trailer's metadata record.
    const METADATA: u8 = 1;
    let Some(lenses) = &report.camera_file.lenses else {
        return Ok(None);
    };
    spherecut_core::may_block("reading an Insta360 file's trailer");
    let name = source.name.display();
    let io = |e: std::io::Error| format!("{name}: {e}");
    let mut f = File::open(source.open).map_err(io)?;
    let len = f.metadata().map_err(io)?.len();
    let trailer = match read_trailer(&mut f, len) {
        Ok(Some(t)) => t,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("{name}: the Insta360 trailer can't be read: {e}")),
    };
    let metadata = match read_record(&mut f, &trailer, METADATA, MAX_METADATA) {
        Ok(Some(m)) => m,
        Ok(None) => return Ok(None),
        Err(e) => {
            return Err(format!(
                "{name}: the Insta360 trailer's record {METADATA} can't be read: {e}"
            ));
        }
    };
    let metadata = parse_metadata(&metadata)
        .map_err(|e| format!("{name}: the Insta360 trailer's metadata (record {METADATA}): {e}"))?;
    let stitch = match stitching(&metadata) {
        Ok(Some(s)) => s,
        Ok(None) => return Ok(None),
        Err(e) => {
            return Err(format!(
                "{name}: the Insta360 trailer's calibration can't be read: {e}"
            ));
        }
    };
    let track = |t: usize| {
        u32::try_from(t).map_err(|_| format!("{name}: its lens stream {t} is past a project's"))
    };
    let tracks = [track(lenses.tracks[0])?, track(lenses.tracks[1])?];
    let strings = &metadata.calib;
    let raw = RawStrings {
        v1: strings.v1.working.clone(),
        v2: strings.v2.working.clone(),
        v3: strings.v3.working.clone(),
        v6: strings.v6.working.clone(),
    };
    let hash = strings.hash();
    if hash != raw.hash() {
        return Err(format!(
            "{name}: the trailer's calibration hashes to {hash} in camera's reading and {} in \
             the project's",
            raw.hash()
        ));
    }
    let c = stitch.calibration;
    let canvas_width = c
        .src
        .checked_mul(2)
        .ok_or_else(|| format!("{name}: its calibration canvas of {} is too large", c.src))?;
    Ok(Some(DualFisheye {
        camera: Camera {
            make: report.camera.as_ref().and_then(|g| g.vendor.clone()),
            model: metadata.model.clone(),
            firmware: metadata.firmware.clone(),
            serial: metadata.serial.clone(),
        },
        tracks,
        front_track: 0,
        calibration: CalibSnapshot {
            canvas: [canvas_width, c.src],
            window: [c.dst, c.dst],
            window_offset: metadata.window.as_ref().map_or([0, 0], |w| w.offset),
            raw,
            hash,
            v3: c.v3,
            v6: c.v6,
        },
        geometry: stitch.known.geometry,
        tune: Tune::default(),
        gains: Gains::Off {},
        profile: None,
    }))
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
///
/// "Newer" is read as "other": the table keeps no date or version beside
/// its evidence, and only a new row or a fit changes it, so a provisional
/// entry whose evidence differs from the one the media was made with is
/// what the table says now. A settled entry supersedes any provisional
/// geometry; a stored geometry that is settled, fitted or the user's is
/// never superseded, nor one whose table entry is fitted or the user's,
/// which no table entry is.
pub fn superseded(stored: &Geometry, table: &Geometry) -> Option<Superseded> {
    let newer = match (stored.status, table.status) {
        (Status::Provisional, Status::Settled) => true,
        (Status::Provisional, Status::Provisional) => stored.evidence != table.evidence,
        _ => false,
    };
    newer.then(|| Superseded {
        status: table.status,
        evidence: table.evidence.clone(),
    })
}

/// [`superseded`] for `source`'s geometry against the table's entry for
/// its camera's model and firmware (`camera::insta360::known::lookup`).
/// The entry's generation is set to the stored one, as `stitching` sets
/// it to the one field 136 names, so that the entry is the geometry a
/// `set_source` taking it would hold.
pub fn superseded_by_table(source: &DualFisheye) -> Option<Superseded> {
    let mut table = known::lookup(
        source.camera.model.as_deref().unwrap_or_default(),
        source.camera.firmware.as_deref().unwrap_or_default(),
    )
    .geometry;
    table.generation = source.geometry.generation;
    superseded(&source.geometry, &table)
}
