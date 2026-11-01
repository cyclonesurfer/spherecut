//! The stitching lens models (M3 plan 6.1 and 6.3): `fisheye::Unified`
//! and its inverse, the canonical distortion and every reading that maps
//! onto it, the 21 V6 readings, and `KbLens`. The properties run 100,000
//! cases in the slice's check (19.3).

/// unproject(project(d)) is within 1e-7 rad of d for ξ in [0, 2.5] and
/// θ up to min(100°, limit_theta − 1°), the coefficients drawn around
/// Pat's V6 and scaled to ξ (6.3).
#[test]
#[ignore = "M3.3 unit (a)"]
fn unified_unproject_inverts_project() {
    todo!()
}

/// project(unproject(px)) is within 1e-6 px of px, in the lens's own
/// pixels, over the same draws.
#[test]
#[ignore = "M3.3 unit (a)"]
fn unified_project_inverts_unproject() {
    todo!()
}

/// With no distortion, ξ = 0 gives r = tan θ and ξ = 1 gives tan(θ/2).
#[test]
#[ignore = "M3.3 unit (a)"]
fn unified_is_tan_at_xi_0_and_tan_half_at_xi_1() {
    todo!()
}

/// r(θ) rises all the way up to limit_theta.
#[test]
#[ignore = "M3.3 unit (a)"]
fn unified_radius_rises_up_to_limit_theta() {
    todo!()
}

/// unproject is `None` past limit_theta.
#[test]
#[ignore = "M3.3 unit (a)"]
fn unified_unproject_is_none_past_limit_theta() {
    todo!()
}

/// limit_theta is acos(−1/ξ) for ξ > 1 (120° at ξ = 2), acos(−ξ) for
/// ξ ≤ 1 (90° at ξ = 0), and earlier where the polynomial folds: 120.1°
/// on Pat's V6.
#[test]
#[ignore = "M3.3 unit (a)"]
fn unified_limit_theta_is_where_the_model_folds() {
    todo!()
}

/// The inverse meets 6.3's tolerances on Pat's V3 and V6 lenses up to
/// 119°, where three passes would not (6.1).
#[test]
#[ignore = "M3.3 unit (a)"]
fn unified_inverts_pats_lenses_to_119_degrees() {
    todo!()
}

/// V3's formula, written term by term, equals the canonical evaluation
/// of `Canonical::v3`.
#[test]
#[ignore = "M3.3 unit (a)"]
fn canonical_v3_is_its_own_formula() {
    todo!()
}

/// Each of the 21 readings' own formula, written term by term from its
/// fields, equals the canonical evaluation of `Canonical::v6`.
#[test]
#[ignore = "M3.3 unit (a)"]
fn canonical_each_v6_reading_is_its_own_formula() {
    todo!()
}

/// The two sourced readings, written out as their sources state them
/// (orbis's where d4 = 0), equal {D5D6 with extension, D9 ByAxis} and
/// {D5D6, D7 ByAxis} (6.1), so a slip in a permutation is caught.
#[test]
#[ignore = "M3.3 unit (a)"]
fn canonical_sourced_readings_are_the_values_named() {
    todo!()
}

/// `V6Reading::all` lists 21 distinct values, each reading every term at
/// most once.
#[test]
#[ignore = "M3.3 unit (a)"]
fn v6_space_has_21_distinct_readings() {
    todo!()
}

/// On terms whose d5 to d12 are all non-zero, the 21 readings give 21
/// different canonical forms.
#[test]
#[ignore = "M3.3 unit (a)"]
fn v6_readings_give_distinct_canonical_forms() {
    todo!()
}

/// `KbLens` round-trips on each base, its tangential and prism terms
/// included.
#[test]
#[ignore = "M3.3 unit (a)"]
fn kb_lens_unproject_inverts_project() {
    todo!()
}

/// `KbLens` applies its base after the polynomial in θ, as
/// `fisheye::Lens::radius` does.
#[test]
#[ignore = "M3.3 unit (a)"]
fn kb_lens_applies_its_base_after_the_polynomial() {
    todo!()
}
