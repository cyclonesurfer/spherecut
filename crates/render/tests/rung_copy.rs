//! Rung 1's fixtures where rung 3 reaches them (M3 plan amendment 5;
//! M3.5's unit (c)): the camera, pair and equirect that
//! `tests/instrument.rs` keeps private, as unit (c) puts them in a
//! library both rungs reach, render the same pictures as rung 1's own.
//!
//! M3.5's types stage names the test; unit (c) writes it and the suite's
//! budget.

mod common;

/// The copy of rung 1 that rung 3 uses renders, through `render_rig`,
/// the pictures this crate's rung 1 fixtures do, bit for bit.
#[test]
#[ignore = "M3.5 unit (c)"]
fn synth_rung_matches_rung1() {
    todo!()
}
