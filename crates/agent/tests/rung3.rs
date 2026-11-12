//! Rung 3 of the round-trip ladder (M3 plan 4.2; M3.5's unit (c)): the
//! lossless synthetic `.insv` (`InsvSpec::rung3()`) through probe, the
//! camera crate and `Still::open_pair` to the CPU reference, held to the
//! planes synth coded and to rung 1's render of them, within 3 s
//! (amendment 5), and `StillPair`'s seek and refusals.
//!
//! M3.5's types stage names the tests; unit (c) writes them and the
//! suite's budget.

/// The planes `StillPair::seek` decodes for each track and frame hash
/// equal to `InsvSpec::planes`: the decode is lossless.
#[test]
#[ignore = "M3.5 unit (c)"]
fn rung3_planes_hash_equal() {
    todo!()
}

/// `StillPair::render` of the file equals rung 1's `render_rig` of the
/// same planes and rig, the one through the file and the camera crate,
/// the other through synth and the boundary.
#[test]
#[ignore = "M3.5 unit (c)"]
fn rung3_render_equals_rung1() {
    todo!()
}

/// `StillPair::seek` lands both lenses on the frame asked for, at one
/// time with equal pts, from any frame before or after it.
#[test]
#[ignore = "M3.5 unit (c)"]
fn open_pair_seeks_both_lenses_to_one_time() {
    todo!()
}

/// `Still::open_pair` refuses a file it can't stitch, each with a clear
/// failure: one with no calibration (`InsvFault::NoCalibration`), and a
/// geometry `Rig::build` refuses.
#[test]
#[ignore = "M3.5 unit (c)"]
fn open_pair_refuses_a_file_it_cant_stitch() {
    todo!()
}
