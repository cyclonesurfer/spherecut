//! camera's reader and parser against synth's own trailer writer and
//! string printer, which share no code with them (M3 plan 4.1 rule 2,
//! 5.5, 7.2), M3.2's unit (d). These are integration tests: synth depends
//! on media, which depends on camera, so a unit test would hold a second
//! copy of camera's types. The properties run 100,000 cases at
//! `PROPTEST_CASES=100000`.

/// Property: records synth writes into an `inst` box, at any offset and
/// alignment, read back through `read_trailer` and `read_record` as
/// synth's `TrailerLayout` says, payload for payload, and the metadata
/// and exposure parse to what synth wrote.
#[test]
#[ignore = "M3.2 unit (d)"]
fn the_trailer_round_trips_through_synths_writer() {
    todo!()
}

/// Property: `calib::parse(synth::insv::print(truth))` gives each of the
/// truth's numbers back within half a unit in its last printed place,
/// for V3 and V6 truths around Pat's numbers (amendment 3).
#[test]
#[ignore = "M3.2 unit (d)"]
fn calib_parses_synths_print() {
    todo!()
}

/// Each trailer `InsvFault` (`TruncatedTrailer` to `VarintOverflow`) is
/// an error naming its record or part at the planted byte, and
/// `NoCalibration` a file with no strings.
#[test]
#[ignore = "M3.2 unit (d)"]
fn every_trailer_fault_is_a_clear_error_naming_the_record() {
    todo!()
}
