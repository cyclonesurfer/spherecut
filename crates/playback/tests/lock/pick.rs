//! The present (M3 plan section 9): `pick_pair` takes the newest time
//! both lenses hold, `show_pair` shows both slots or neither, and the
//! counters say what was shown: `unpaired_presents` over every stream,
//! locked or not, `pair_wait`, and dropped frames once a pair.

/// `pick_pair`'s property, at `PROPTEST_CASES=100000` apart: over random
/// slot states, epochs, times and failed `try_show`s, never a mixed pair,
/// and always the largest common time at or before the clock.
#[test]
#[ignore = "M3.8 unit (b)"]
fn pick_pair_takes_the_largest_common_time() {
    todo!()
}

/// A locked pair's present shows both slots or neither: when the second
/// `try_show` fails, the first is given back, and the frame on screen
/// stays.
#[test]
#[ignore = "M3.8 unit (b)"]
fn a_locked_pair_is_shown_all_or_none() {
    todo!()
}

/// `unpaired_presents` counts the presents at which the streams shown
/// are at different clip times, over every stream, through
/// `open_streams` as through `open_locked`.
#[test]
#[ignore = "M3.8 unit (b)"]
fn unpaired_presents_count_over_every_stream() {
    todo!()
}

/// `pair_wait` is how far the pair shown trails the clock.
#[test]
#[ignore = "M3.8 unit (b)"]
fn pair_wait_is_the_pairs_lag() {
    todo!()
}

/// A pair passed over counts as one drop, not one a lens.
#[test]
#[ignore = "M3.8 unit (b)"]
fn drops_count_once_for_a_pair() {
    todo!()
}

/// The pacing rule fails a run that freezes: the check that
/// `a_locked_pair_paces_to_its_slowest_lens` applies, held to a run in
/// which the pair stops coming.
#[test]
#[ignore = "M3.8 unit (b)"]
fn the_pacing_rule_fails_a_frozen_run() {
    todo!()
}
