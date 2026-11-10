//! The CPU reference's stitch (M3 plan 10.3 and 10.4):
//! `reference::render_rig` and its debug modes 4 to 7, and what samples
//! through it, the strips (8.2) and the masks (8.1). No GPU: the shader's
//! stitch is M3.7's, and is held to this.

/// Each lens's footprint is its texel's change across the pixel's four
/// ±½ px rays, projected through that lens, and its LOD the log2 of the
/// footprint's longer side in texels (10.3).
#[test]
#[ignore = "M3.4 unit (b)"]
fn rig_footprint_is_the_projected_half_pixel_rays() {
    todo!()
}

/// The mask and the blend are the rig's own, at each lens's own LOD:
/// `Rig::covers` and `Rig::weights`, with the frame term and the feather's
/// shrink (6.2).
#[test]
#[ignore = "M3.4 unit (b)"]
fn rig_mask_and_weights_are_the_rigs() {
    todo!()
}

/// Each lens's gain multiplies its linear light, after its EOTF and
/// before the blend.
#[test]
#[ignore = "M3.4 unit (b)"]
fn rig_gains_apply_in_linear_light() {
    todo!()
}

/// Modes 4 and 5 draw each lens alone, 6 the weights (red w0, blue w1)
/// and 7 the seam difference where both cover.
#[test]
#[ignore = "M3.4 unit (b)"]
fn rig_debug_modes_4_to_7() {
    todo!()
}

/// `lenses` go by track and the rig's lenses by block: block b samples
/// its own track's picture, under a reversed binding too.
#[test]
#[ignore = "M3.4 unit (b)"]
fn rig_lenses_come_from_their_tracks() {
    todo!()
}

/// A strip's sample outside its lens's mask is marked invalid, never
/// clamped to the slot's edge (8.2, 10.3).
#[test]
#[ignore = "M3.4 unit (b)"]
fn strips_mark_samples_outside_invalid() {
    todo!()
}

/// `band_weights` and `lens_coverage` are the pixels `render_rig` draws:
/// each lens's coverage is where modes 4 and 5 draw it (8.1).
#[test]
#[ignore = "M3.4 unit (b)"]
fn masks_match_what_render_rig_draws() {
    todo!()
}
