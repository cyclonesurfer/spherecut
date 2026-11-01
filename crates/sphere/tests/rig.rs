//! The lens pair (M3 plan 6.2 and 6.3): the Euler space and its twins,
//! `Rig::build` and its frames, the weights, the stitch distance and the
//! mask. The properties run 100,000 cases in the slice's check (19.3);
//! the small enums go exhaustively and `EulerConv` is sampled.

/// `EulerConv::all` lists 1152 distinct values, each axes and order a
/// permutation.
#[test]
#[ignore = "M3.3 unit (b)"]
fn euler_space_has_1152_distinct_values() {
    todo!()
}

/// `EulerConv::rotation` takes each sign with its angle, whatever the
/// order, and adds the roll offset to the stored roll before the roll's
/// sign, as `synth::lens::TruthEuler` reads the labels (amendment 3).
#[test]
#[ignore = "M3.3 unit (b)"]
fn euler_conv_reads_signs_by_angle_and_offsets_the_roll_first() {
    todo!()
}

/// `EulerConv::PLACEHOLDER` is R_z(roll − 90°) · R_x(pitch) · R_y(yaw),
/// within 1° of the identity on Pat's V3 and V6 angles, and puts block
/// 0's lens along −Z (5.3).
#[test]
#[ignore = "M3.3 unit (b)"]
fn euler_placeholder_is_near_the_identity_on_pats_angles() {
    todo!()
}

/// The twin groups for sampled stored angles, once stage G's filter keeps
/// only values that put block 0's lens within 5° of −Z, differ by common
/// rotations that are exactly the quarter turns about Z (6.2).
#[test]
#[ignore = "M3.3 unit (b)"]
fn euler_twins_are_quarter_turns() {
    todo!()
}

/// At q = 0, unproject(texel(d)) == d for every value of the small enums
/// and sampled `EulerConv`s, the turn included.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_texel_and_unproject_invert_at_q_zero() {
    todo!()
}

/// det(lens_from_rig) = +1 for every lens of every rig.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_lens_from_rig_is_a_proper_rotation() {
    todo!()
}

/// Two different values of any field differ in texel for some ray at
/// stored angles of at least 1°, all 21 `V6Reading` values among them on
/// a calibration whose d5 to d12 are all non-zero.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_values_of_every_field_differ_in_texel() {
    todo!()
}

/// Canvas maps A, B and C invert.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_canvas_maps_invert() {
    todo!()
}

/// Flipping the binding swaps the tracks and nothing else.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_binding_flip_swaps_the_tracks_and_nothing_else() {
    todo!()
}

/// `TzSign::Negated` flips all of t_1, so a rig built with it at q is
/// the `AsStored` rig at −q, ray for ray.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_tz_negated_is_as_stored_at_negative_q() {
    todo!()
}

/// A front track that is block 1's turns both lenses 180° about +Y, and
/// `upright` turns both a quarter turn about −Z: common rotations, the
/// relative one unchanged.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_front_track_and_upright_turn_both_lenses() {
    todo!()
}

/// `Rig::build` refuses a θ_max past the limit, a missing generation, a
/// block's terms of the wrong generation, a front track past 1, a side
/// of 0 and numbers that aren't finite, each with its `RigError`.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_build_refuses_what_it_cannot_build() {
    todo!()
}

/// `Tune::default()` lies inside every bound of `validate` (11.2) on
/// Pat's V6 numbers, the feather against min(θ_max, θ_frame) − 90° among
/// them.
#[test]
#[ignore = "M3.3 unit (b)"]
fn tune_default_lies_inside_validates_bounds() {
    todo!()
}

/// The weights lie in [0, 1] and sum to 1 wherever a lens covers.
#[test]
#[ignore = "M3.3 unit (b)"]
fn weights_lie_in_the_unit_interval_and_sum_to_one() {
    todo!()
}

/// The weights are continuous in the direction.
#[test]
#[ignore = "M3.3 unit (b)"]
fn weights_are_continuous() {
    todo!()
}

/// The weights are symmetric under a lens swap with a mirrored rig.
#[test]
#[ignore = "M3.3 unit (b)"]
fn weights_are_symmetric_under_a_mirrored_swap() {
    todo!()
}

/// A point at R seen with q = 1/R puts both lenses' rays through it
/// within 1e-9 m, the error growing with |q − 1/R|.
#[test]
#[ignore = "M3.3 unit (b)"]
fn distance_puts_both_rays_through_the_scene_point() {
    todo!()
}

/// For rigs within validate's bounds, Pat's V6 numbers under map A among
/// them, every direction with a non-zero weight has its footprint inside
/// its slot by the margin; with the frame term off, some footprints fall
/// outside (the control).
#[test]
#[ignore = "M3.3 unit (b)"]
fn mask_keeps_weighted_footprints_inside_their_slots() {
    todo!()
}

/// `theta_frame` matches a scan of the slot's edges at one sample per
/// texel within 0.01°: 99.1° on lens 0 of Pat's V6 under map A.
#[test]
#[ignore = "M3.3 unit (b)"]
fn theta_frame_matches_an_edge_scan() {
    todo!()
}
