//! The group base (M3 plan section 9): the cache runs from the newest
//! time both lenses hold, one `until` serves the group, seeks land both
//! lenses on one frame, the pair ends at its shorter lens, and the
//! `pair_resync` backstop holds when a shrinking budget takes the frame
//! the trailing lens was catching up to.

/// A seek forward while lens 0 leads, to a time past every frame either
/// lens holds and more than 1 s past the pair on screen, lands both
/// lenses within `WAIT`: the base is the clock's time until both land.
/// Its control, the old common time kept as the base, deadlocks.
#[test]
#[ignore = "M3.8 unit (c)"]
fn a_seek_forward_while_lens_0_leads_lands_both() {
    todo!()
}

/// The group's `until` is raised to the clock's time by a member more
/// than 2 frames behind it, but never past the newest frame every other
/// member holds, so the trailing lens's next frame always pairs.
#[test]
#[ignore = "M3.8 unit (c)"]
fn the_group_until_stays_within_the_other_lens() {
    todo!()
}

/// Keyframe seeks whose landings differ (tables planted to agree over
/// GOPs that don't) go exact to the later landing on both lenses, and
/// the clock takes the group's landing.
#[test]
#[ignore = "M3.8 unit (c)"]
fn keyframe_landings_that_differ_go_exact_to_the_later() {
    todo!()
}

/// A locked pair with lens 1 a frame short ends at lens 1's last frame,
/// and play stops there.
#[test]
#[ignore = "M3.8 unit (c)"]
fn a_locked_pair_ends_at_its_shorter_lens() {
    todo!()
}

/// A budget that shrinks mid-play evicts the frame the trailing lens was
/// catching up to: with both workers waiting for room and no common time
/// past the base, the group seeks both lenses to the trailing lens's
/// newest frame and counts a `pair_resync`, and the pair goes on. Its
/// control, the backstop off, stops.
#[test]
#[ignore = "M3.8 unit (c)"]
fn a_shrinking_budget_mid_play_resyncs_the_pair() {
    todo!()
}
