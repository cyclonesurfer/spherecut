//! The seam instrument's own tests (M3 plan 8.2, and 8.3's self-tests):
//! `seam::bin_shifts` and `seam::signature` on strips and bins whose
//! shifts are planted, and the predicted fields (`common::predict`)
//! against 3.4's arithmetic. They prove the instrument consistent with
//! itself; only real footage speaks for the camera.

/// A pure shift planted by resampling one strip comes back within
/// 0.02 px.
#[test]
#[ignore = "M3.4 unit (c)"]
fn a_pure_shift_comes_back_within_0_02_px() {
    todo!()
}

/// Swapping the two strips flips the sign of every term.
#[test]
#[ignore = "M3.4 unit (c)"]
fn swapping_the_strips_flips_every_term() {
    todo!()
}

/// Gains on one strip leave the geometric terms under 0.005°, and come
/// back as the log gains.
#[test]
#[ignore = "M3.4 unit (c)"]
fn gains_leave_the_geometry_under_0_005_deg() {
    todo!()
}

/// A bin without texture is marked invalid, never given a zero shift.
#[test]
#[ignore = "M3.4 unit (c)"]
fn a_textureless_bin_is_invalid() {
    todo!()
}

/// Texture along a single line gives information across the line and
/// none along it: the aperture problem, kept out of the fit by each
/// bin's normal matrix.
#[test]
#[ignore = "M3.4 unit (c)"]
fn a_line_informs_across_it_only() {
    todo!()
}

/// `signature` gives back each term of a field of bins made from known
/// terms.
#[test]
#[ignore = "M3.4 unit (c)"]
fn signature_recovers_each_term() {
    todo!()
}

/// `predict_bins`' fields for 3.4's planted differences have the terms
/// and sizes 3.4 works out.
#[test]
#[ignore = "M3.4 unit (c)"]
fn predicted_fields_match_the_error_budget() {
    todo!()
}
