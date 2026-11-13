//! The views' dual-fisheye goldens (M3 plan 10.5), a lens pair stitched
//! by the shader and seen whole: the equirect, the poles against their
//! twins, each lens alone, a minified tiny planet, the LOD map, the mask's
//! margin and frame term, and the Kannala–Brandt rig under each base.
//! Rows of the goldens binary, blessed into `goldens/dfish_views/` with a
//! manifest of their own (see the binary's doc).

use super::{Mode, Row, Run, UPDATE, bless, diff_dir_beside, gpu, run_these};

/// Where these rows' images and manifest live.
pub(super) const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/dfish_views");

/// What their manifest says it is.
pub(super) const ABOUT: &str = "The views' dual-fisheye goldens (M3 plan 10.5, DECISIONS D32): \
                                lens pairs rendered from synth's planes and stitched by the \
                                shader on lavapipe, with each check's thresholds and what it \
                                measured when blessed.";

/// The rows, in 10.5's order.
pub(super) const ROWS: &[(&str, Row)] = &[
    ("dfish_equirect_identity", dfish_equirect_identity),
    ("dfish_zenith", dfish_zenith),
    ("dfish_nadir", dfish_nadir),
    ("dfish_lens_only_0", dfish_lens_only_0),
    ("dfish_lens_only_1", dfish_lens_only_1),
    ("dfish_hifreq_tinyplanet", dfish_hifreq_tinyplanet),
    ("dfish_lod_heatmap", dfish_lod_heatmap),
    ("dfish_mask_margin", dfish_mask_margin),
    ("dfish_kb_rig", dfish_kb_rig),
];

#[test]
#[ignore = "M3.7 unit (c)"]
fn dfish_view_rows() {
    let gpu = gpu();
    assert_eq!(gpu.caps.info.device_type, wgpu::DeviceType::Cpu);
    let diffs = diff_dir_beside("dfish-views");
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

fn dfish_equirect_identity(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_zenith(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_nadir(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_lens_only_0(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_lens_only_1(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_hifreq_tinyplanet(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_lod_heatmap(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_mask_margin(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}

fn dfish_kb_rig(_: &mut Run<'_>) {
    todo!("M3.7 unit (c)")
}
