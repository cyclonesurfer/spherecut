//! synth's lens truth against sphere's lens maths (M3 plan 4.1 rule 2,
//! 4.2's rung 0 and 6.3's Against synth): the two share no code, and
//! these tests convert between their types only here, at the boundary.

/// Against synth (6.3): at the default tune, `Rig::build` agrees with
/// `synth::lens::compose` on every value of the small enums and on
/// sampled `EulerConv`s: lens_from_rig is the inverse of to_rig within
/// 1e-12, the models within 1e-9 px and the centres within 1e-12 m.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rig_build_agrees_with_synth_compose() {
    todo!()
}

/// Rung 0 (4.2): sphere's forward of synth's inverse is the identity on
/// 10k seeded points per truth model, within 1e-6 px and 1e-7 rad.
#[test]
#[ignore = "M3.3 unit (b)"]
fn rung0_sphere_forward_of_synth_inverse_is_the_identity() {
    todo!()
}
