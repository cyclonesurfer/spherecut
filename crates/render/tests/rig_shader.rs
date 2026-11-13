//! The shader's stitch on lavapipe (M3 plan 10.1 to 10.4): `fs_rig`
//! held to `reference::render_rig` and its debug modes 4 to 7, the
//! weights, ties, mask and level 0 as amendment 5 corrects them, a pair
//! blurred and coded to YUV, what `render_from` and `rig::pack` refuse,
//! and the strip pass against `seam::strips_of`. M3.7 unit (a) writes
//! them.

/// The stitch of rung 1's scenes against `reference::render_rig_of`.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_matches_render_rig_of() {
    todo!()
}

/// Debug modes 4 to 7 draw each block alone, the weights and the seam
/// difference, by calibration block whatever the binding.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_modes_4_to_7_follow_the_blocks() {
    todo!()
}

/// Mode 6 lights the ring where no feather weighs as the reference does.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_weights_light_the_ring_no_feather_weighs() {
    todo!()
}

/// A tie between the lenses falls as it does for the mirrored pair.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_ties_like_the_mirror_pair() {
    todo!()
}

/// The mask is tested in angle, against θ_max, not by its cosine.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_tests_the_mask_in_angle() {
    todo!()
}

/// Level 0 keeps the mask its footprint's LOD would give.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_level0_keeps_the_footprints_mask() {
    todo!()
}

/// A lens whose ray lands outside its picture doesn't cover the pixel.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_lens_without_an_image_doesnt_cover() {
    todo!()
}

/// Motion blur sums a pair's stitched pictures in linear light.
#[test]
#[ignore = "M3.7 unit (a)"]
fn fs_rig_blurs_a_pair_in_linear_light() {
    todo!()
}

/// `StillRenderer::render_yuv` codes a pair's stitch as it codes an
/// equirect's.
#[test]
#[ignore = "M3.7 unit (a)"]
fn render_yuv_takes_a_pair() {
    todo!()
}

/// `render_from` refuses what has no form on the GPU: a pair drawn flat,
/// and an equirect drawn in a pair's debug modes, 4 to 7.
#[test]
#[ignore = "M3.7 unit (a)"]
fn render_from_refuses_what_has_no_gpu_form() {
    todo!()
}

/// `rig::pack` refuses a Mei lens beside a Kannala–Brandt one, a gain
/// that isn't finite and 0 or more, and a binding that isn't tracks 0
/// and 1, one each.
#[test]
#[ignore = "M3.7 unit (a)"]
fn pack_refuses_what_the_gpu_cant_draw() {
    todo!()
}

/// `rig::pack` keeps a `RigLens::side` unlike its slot's luma size, so
/// the frame term reads the side and the uv the slot.
#[test]
#[ignore = "M3.7 unit (a)"]
fn pack_keeps_a_side_unlike_its_slot() {
    todo!()
}

/// `StillRenderer::strips` matches `seam::strips_of` on the strips'
/// grid.
#[test]
#[ignore = "M3.7 unit (a)"]
fn gpu_strips_match_strips_of() {
    todo!()
}

/// `reference::rig_lod_map` is what debug mode 3 draws for a pair.
#[test]
#[ignore = "M3.7 unit (a)"]
fn rig_lod_map_matches_mode_3() {
    todo!()
}
