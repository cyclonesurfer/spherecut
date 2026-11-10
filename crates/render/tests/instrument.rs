//! Rung 1 of the round trip (M3 plan 4.2) and the attribution matrix
//! (8.3): synth's lens images through the CPU reference's stitch, against
//! the analytic ideal and against what the geometry predicts. One binary,
//! so that rung 1 and the matrix share the renders of
//! `RenderCache::shared()` (7.1).
//!
//! Budget (4.1 rule 5, 19.6): with the weighted metrics' and the seam's
//! own tests, 15 s; each test prints its time.

/// Rung 1 on the X5-like rig: the stitch against the ideal within 4.2's
/// thresholds, away from the seam, each lens alone and in the band, with
/// each geometric control failing.
#[test]
#[ignore = "M3.4 unit (d)"]
fn rung1_x5_like_rig_meets_the_thresholds() {
    todo!()
}

/// Rung 1 on the Kannala–Brandt rig, which shows that the source path
/// isn't Mei's alone.
#[test]
#[ignore = "M3.4 unit (d)"]
fn rung1_kb_rig_meets_the_thresholds() {
    todo!()
}

/// With lens 1 at +0.5 EV and +5 % red, the step across the seam is under
/// 1 LSB once the measured gains are applied; with the gains off it
/// isn't.
#[test]
#[ignore = "M3.4 unit (d)"]
fn rung1_exposure_step_once_matched() {
    todo!()
}

/// Both lenses turned 0.5° about +Y pass every seam check and fail the
/// comparison with the ideal: the common-mode control.
#[test]
#[ignore = "M3.4 unit (d)"]
fn rung1_common_mode_control() {
    todo!()
}

/// 8.3's matrix: each planted error is predicted point by point, measured
/// from the images within 0.05 px rms of the prediction, and lands on the
/// term the prediction names, with the other terms under their bound.
#[test]
#[ignore = "M3.4 unit (d)"]
fn seam_signature_attributes_planted_errors() {
    todo!()
}

/// 3.4's budget, its pixels per degree across and along the seam and the
/// size of each planted difference, holds on the images.
#[test]
#[ignore = "M3.4 unit (d)"]
fn error_budget_matches_the_images() {
    todo!()
}
