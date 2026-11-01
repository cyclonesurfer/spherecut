//! A recording's files (M3 plan 5.4), M3.2's unit (c): `companions`
//! from names alone, and `verify_companion` by record 4.

#[test]
#[ignore = "M3.2 unit (c)"]
fn companions_pair_a_master_and_its_lrv_both_ways() {
    todo!()
}

/// Names that aren't an Insta360 camera's, or only look like one, name
/// no companions; `companions` opens nothing.
#[test]
#[ignore = "M3.2 unit (c)"]
fn companions_of_other_names_are_none() {
    todo!()
}

/// Equal exposure records are `Verified`; one byte changed, or the
/// record missing from either file, is `NameOnly`.
#[test]
#[ignore = "M3.2 unit (c)"]
fn verify_companion_compares_record_4() {
    todo!()
}

#[test]
#[ignore = "M3.2 unit (c)"]
fn verify_companion_panics_on_the_ui_thread() {
    todo!()
}
