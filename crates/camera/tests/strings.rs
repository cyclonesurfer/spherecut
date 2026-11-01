//! The protobuf walker and the calibration strings (M3 plan 5.1, 5.2 and
//! 5.5), M3.2's unit (b). The properties run 100,000 cases at
//! `PROPTEST_CASES=100000`.

/// Property: any u64 written as a varint, in any field, walks back to
/// itself; an eleventh byte or a value past 64 bits is `BadVarint`.
#[test]
#[ignore = "M3.2 unit (b)"]
fn varints_round_trip() {
    todo!()
}

/// Property: fields `parse_metadata` doesn't read, of every wire type,
/// anywhere among the ones it does, are skipped and counted, and change
/// nothing else.
#[test]
#[ignore = "M3.2 unit (b)"]
fn unknown_fields_are_skipped() {
    todo!()
}

/// Property: `parse(g, &print(c)) == c` for calibrations of every
/// generation, and `print(&parse(g, s)?) == s` for every string printed.
#[test]
#[ignore = "M3.2 unit (b)"]
fn strings_round_trip() {
    todo!()
}

/// Property: a string with a token dropped or added is `TokenCount`, with
/// its generation, the count found and the byte where it went wrong.
#[test]
#[ignore = "M3.2 unit (b)"]
fn a_wrong_token_count_is_an_error() {
    todo!()
}
