//! The stored calibration as sphere's types and the built-in geometry
//! table (M3 plan 5.2 and 5.3; M3.5's unit (b)): `insta360::to_calibration`,
//! `insta360::known` and `insta360::stitching`, held to Pat's truths in
//! `spherecut-boundary` through synth's printer, which shares no code
//! with camera's parser (4.1 rule 2).
//!
//! M3.5's types stage names the tests; unit (b) writes them and the
//! suite's budget.

/// `to_calibration` of camera's parse of synth's print of Pat's V3 and
/// V6 truths, alone and together, with field 27's window, is
/// `spherecut_boundary::calibration` of the truths: src the canvas's
/// height, dst the window's, block 1's cx local to its square, and every
/// other number as stored (5.2, and 5.5's cross-implementation rule).
#[test]
#[ignore = "M3.5 unit (b)"]
fn to_calibration_gives_the_truths_numbers() {
    todo!()
}

/// Strings that parse but can't be a calibration (two generations on
/// different canvases, a canvas that isn't two squares side by side, no
/// window) are `CameraError::NotCalibration`, naming the field.
#[test]
#[ignore = "M3.5 unit (b)"]
fn to_calibration_refuses_what_it_cant_hold() {
    todo!()
}

/// `known::lookup("Insta360 X5", "v1.1.22_build1")` is 5.3's entry: the
/// placeholder `EulerConv` with "placeholder until the fit" for its
/// evidence, provisional, the LRV's left half track 1, no screen side
/// yet, stitchable and no notice.
#[test]
#[ignore = "M3.5 unit (b)"]
fn known_x5_is_the_placeholder() {
    todo!()
}

/// A firmware the X5's `measured_on` doesn't list gets its entry as
/// provisional, with evidence that says so; a model with no entry gets
/// the X5's conventions, provisional, with the notice "geometry assumed
/// from the X5" (5.3).
#[test]
#[ignore = "M3.5 unit (b)"]
fn known_unlisted_firmware_and_model_are_provisional() {
    todo!()
}

/// `stitching` reads the generation field 136 names into the entry's
/// geometry (4 for V6, 3 for V3), gives `None` for metadata with neither
/// a V3 nor a V6 string, and builds a rig from a trailer with V3 alone,
/// which the table's V6 row refuses (`RigError::NoGeneration`).
#[test]
#[ignore = "M3.5 unit (b)"]
fn stitching_follows_field_136() {
    todo!()
}

/// Tier 2c: each of Pat's 14 files gives a `stitching` whose
/// calibration is `spherecut_boundary::calibration` of Pat's truths and
/// which `Rig::build` builds under the default tune.
#[test]
#[ignore = "M3.5 unit (b)"]
fn samples_calibrations_build_rigs() {
    todo!()
}
