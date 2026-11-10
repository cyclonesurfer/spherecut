//! The seam shifts a model predicts (M3 plan 8.3, step 1), for the
//! attribution matrix and the seam instrument's tests to hold the
//! images' shifts to.

use spherecut_render::seam::{BinShift, StripSpec};
use spherecut_sphere::rig::Rig;
use spherecut_synth::lens::RigTruth;

/// The shift field that unwrapping `truth`'s frame `frame` with the model
/// `rig` leaves, bin by bin as `bin_shifts` gives it under `spec`, with
/// `BinShift`'s signs: point by point in f64, each seam sample through
/// the model's forward and the truth's inverse (`synth::lens::seen`),
/// with no images.
#[expect(unused_variables, reason = "M3.4 unit (c) writes the body")]
pub fn predict_bins(truth: &RigTruth, frame: u32, rig: &Rig, spec: &StripSpec) -> Vec<BinShift> {
    todo!()
}
