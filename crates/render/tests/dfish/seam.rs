//! The seam's dual-fisheye goldens (M3 plan 10.5), a lens pair stitched
//! by the shader across its seam: both seams of the far `SeamLines` pair,
//! the registration the GPU's strips measure, the stitch distance, the
//! exposure match, the feather, the X5's own coding and the layouts.
//! Rows of the goldens binary, blessed into `goldens/dfish_seam/` with a
//! manifest of their own (see the binary's doc).

use super::{Mode, Row, Run, UPDATE, bless, diff_dir_beside, gpu, run_these};

/// Where these rows' images and manifest live.
pub(super) const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/dfish_seam");

/// What their manifest says it is.
pub(super) const ABOUT: &str = "The seam's dual-fisheye goldens (M3 plan 10.5, DECISIONS D32): \
                                lens pairs rendered from synth's planes and stitched by the \
                                shader on lavapipe, with each check's thresholds and what it \
                                measured when blessed.";

/// The rows, in 10.5's order.
pub(super) const ROWS: &[(&str, Row)] = &[
    ("dfish_rect_seam_left", dfish_rect_seam_left),
    ("dfish_rect_seam_right", dfish_rect_seam_right),
    ("dfish_seam_registration", dfish_seam_registration),
    ("dfish_stitch_distance", dfish_stitch_distance),
    ("dfish_exposure_match", dfish_exposure_match),
    ("dfish_feather", dfish_feather),
    ("dfish_fullrange_left", dfish_fullrange_left),
    ("dfish_layout_invariance", dfish_layout_invariance),
];

#[test]
#[ignore = "M3.7 unit (b)"]
fn dfish_seam_rows() {
    let gpu = gpu();
    assert_eq!(gpu.caps.info.device_type, wgpu::DeviceType::Cpu);
    let diffs = diff_dir_beside("dfish-seam");
    let _ = std::fs::remove_dir_all(&diffs);
    let mode = Mode::Golden {
        bless: bless::on(UPDATE),
    };
    let mut run = Run::at(gpu, mode, std::path::Path::new(DIR), &diffs);
    run.about = ABOUT;
    run.lower_floors = bless::lower(UPDATE);
    run.tell = true;
    run_these(&mut run, ROWS, None);
    run.finish();
}

fn dfish_rect_seam_left(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}

fn dfish_rect_seam_right(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}

fn dfish_seam_registration(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}

fn dfish_stitch_distance(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}

fn dfish_exposure_match(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}

fn dfish_feather(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}

fn dfish_fullrange_left(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}

fn dfish_layout_invariance(_: &mut Run<'_>) {
    todo!("M3.7 unit (b)")
}
