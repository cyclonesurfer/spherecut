//! `spherecut frame` on a camera's two-lens file (M3 plan 14's frame row;
//! M3.5's unit (d)): stitched with the file's calibration and the
//! `Known` geometry through `agent::still::Still::open_pair`, on the CPU
//! reference until M3.7, with `--lens`, `--seam`, `--geometry`,
//! `--stitch-distance` and `--projection equirect`.
//!
//! M3.5's types stage names the tests; unit (d) writes them and the
//! suite's budget.

/// `frame <insv> --reference` writes the picture the library makes
/// (`Still::open_pair`, `StillPair::seek` and `render`), byte for byte.
#[test]
#[ignore = "M3.5 unit (d)"]
fn frame_insv_reference_matches_the_library() {
    todo!()
}

/// Without `--reference`, `frame <insv>` fails with a clear error until
/// M3.7 stitches on the GPU.
#[test]
#[ignore = "M3.5 unit (d)"]
fn frame_insv_needs_reference_until_m3_7() {
    todo!()
}

/// `--lens 0` and `--lens 1` show one lens raw, and `--seam weights` and
/// `--seam difference` the stitch's weights and seam difference
/// (`RigDebug`'s modes 6 and 7).
#[test]
#[ignore = "M3.5 unit (d)"]
fn frame_insv_shows_a_lens_and_the_seam() {
    todo!()
}

/// `--geometry binding=…,canvas=…` and `--stitch-distance 2m` change the
/// rig as the library's `PairOptions` do, starting from the file's own
/// geometry.
#[test]
#[ignore = "M3.5 unit (d)"]
fn frame_insv_takes_geometry_and_distance() {
    todo!()
}

/// Each trailer `InsvFault` is a clear error from `frame` that names the
/// record (5.5), with no picture written.
#[test]
#[ignore = "M3.5 unit (d)"]
fn frame_insv_trailer_faults_are_clear_errors() {
    todo!()
}

/// `--projection equirect`, which M3.5 adds for `.insv` files, on a file
/// that is already a stitched equirect renders the whole sphere through
/// frame's own path.
#[test]
#[ignore = "M3.5 unit (d)"]
fn frame_projection_equirect_on_an_equirect_file() {
    todo!()
}
