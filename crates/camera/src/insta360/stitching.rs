//! A file's calibration strings as sphere's types (M3 plan 5.2 and 5.3):
//! the stored numbers as a [`Calibration`], and with the table's
//! [`Known`] entry, all that `Rig::build` needs from a file but the tune.

use spherecut_sphere::rig::Calibration;

use super::calib::Calib;
use super::known::Known;
use super::metadata::{Metadata, Window};
use crate::CameraError;

/// What a file gives a stitch: its stored calibration and the table's
/// entry for its camera, whose geometry reads the generation field 136
/// names.
#[derive(Debug, Clone, PartialEq)]
pub struct Stitching {
    pub calibration: Calibration,
    pub known: Known,
}

/// sphere's [`Calibration`] of a file's parsed V3 and V6 strings and its
/// field 27 window, either string or both: src is the canvas's height,
/// dst the window's, each block's numbers are taken as stored but block
/// 1's cx, which is made local to its own square (cx − src), and the
/// blocks go under their generation.
///
/// Plan 5.2's `to_calibration(&Calib)` can't fill a `Calibration`, which
/// holds src, field 27's dst and both generations, so it takes all
/// three. Strings that parse but can't be a calibration, such as two
/// generations on different canvases or a canvas that isn't two squares
/// side by side, or a missing window, are
/// [`CameraError::NotCalibration`].
#[expect(unused_variables, reason = "M3.5 unit (b) writes the body")]
pub fn to_calibration(
    v3: Option<&Calib>,
    v6: Option<&Calib>,
    window: Option<&Window>,
) -> Result<Calibration, CameraError> {
    todo!("M3.5 unit (b)")
}

/// What `meta` gives a stitch, the one entry probe and `agent::still`
/// share: the working V3 and V6 strings parsed and through
/// [`to_calibration`] with field 27's window, and the table's entry for
/// the model and firmware ([`super::known::lookup`]) with its geometry's
/// generation the one field 136 names. `None` when the metadata holds
/// neither a V3 nor a V6 string; an error when one doesn't parse or
/// can't be a calibration.
#[expect(unused_variables, reason = "M3.5 unit (b) writes the body")]
pub fn stitching(meta: &Metadata) -> Result<Option<Stitching>, CameraError> {
    todo!("M3.5 unit (b)")
}
