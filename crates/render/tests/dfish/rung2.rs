//! Rung 2 (M3 plan 4.2): rung 1's planes stitched by the shader on
//! lavapipe, held to rung 1's pictures (`reference::render_rig_of`,
//! through `tests/common/rung1.rs`) by D32's reference rule, over the
//! shots rung 1 holds: the 512 × 256 equirect and the four band stripes,
//! the stitch and each block alone. A module of the goldens binary, whose
//! pairs it shares; it stores no PNG, and blesses its floors into
//! `goldens/rung2.json` by a ledger of its own tests.

/// The X5-like camera's far `SeamLines` and `Grid` pairs.
#[test]
#[ignore = "M3.7 unit (c)"]
fn rung2_x5_like_matches_rung1() {
    todo!()
}

/// The near `SeamLines` pair, stitched at q = 1/1.5 m⁻¹.
#[test]
#[ignore = "M3.7 unit (c)"]
fn rung2_near_matches_rung1() {
    todo!()
}

/// The Kannala–Brandt camera's `SeamLines` pair, through the shader's
/// model 1.
#[test]
#[ignore = "M3.7 unit (c)"]
fn rung2_kb_rig_matches_rung1() {
    todo!()
}

/// The weights and the seam difference (debug modes 6 and 7), read back
/// in linear light.
#[test]
#[ignore = "M3.7 unit (c)"]
fn rung2_modes_6_and_7_match_rung1() {
    todo!()
}

/// One stitch and each block alone under unlike gains, by block, with a
/// binding that puts block 0 on track 1.
#[test]
#[ignore = "M3.7 unit (c)"]
fn rung2_unlike_gains_under_a_reversed_binding() {
    todo!()
}

/// Rung 7's rung 2: the same pictures on each hardware GPU, against
/// lavapipe's by D32's other-driver rule and against rung 1 by the
/// reference rule.
#[test]
#[ignore = "M3.7 unit (c)"]
fn rung2_on_radv() {
    todo!()
}
