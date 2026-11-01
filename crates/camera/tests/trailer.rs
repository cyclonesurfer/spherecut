//! The trailer reader (M3 plan 5.1 and 5.5), M3.2's unit (a): a trailer
//! cut at every point is an error, arbitrary bytes never panic, each
//! limit is an error at its byte, and the readers refuse the UI thread.
//! The properties run 100,000 cases at `PROPTEST_CASES=100000`.

/// Property: every cut of a whole trailer, from its first byte to its
/// last, is an error or no trailer, never a panic or a trailer.
#[test]
#[ignore = "M3.2 unit (a)"]
fn a_trailer_cut_at_every_point_is_an_error() {
    todo!()
}

/// Property: `read_trailer` and `read_record` on arbitrary bytes, with
/// and without the tail's magic at their end, return and never panic.
#[test]
#[ignore = "M3.2 unit (a)"]
fn arbitrary_bytes_never_panic() {
    todo!()
}

/// Each limit of `insta360::limits`, and a record over `read_record`'s
/// `max`, is its error at the byte that says so.
#[test]
#[ignore = "M3.2 unit (a)"]
fn every_limit_is_an_error_with_its_offset() {
    todo!()
}

#[test]
#[ignore = "M3.2 unit (a)"]
fn read_trailer_panics_on_the_ui_thread() {
    todo!()
}

#[test]
#[ignore = "M3.2 unit (a)"]
fn read_record_panics_on_the_ui_thread() {
    todo!()
}
