//! Play (M3 plan section 9): a locked pair never shows a mixed pair, by
//! tag or by content, paces to its slowest lens, asks for six slots and
//! unlocks below its floor of two a member, and ends when a member
//! fails.

/// Lens 1 2.5 to 3 times slower than lens 0 with jitter, 64 seeds of
/// random seeks, plays, steps, pauses and rates: at every present both
/// slots are read back and hold the same frame, the one their tags name,
/// and after every seek and step both lenses land within `WAIT`. Its
/// control, the same run through `open_streams`, counts mismatches by
/// tag and by content.
#[test]
#[ignore = "M3.8 unit (d)"]
fn a_locked_pair_never_shows_mismatched_frames() {
    todo!()
}

/// On a 90-frame clip whose clock moves a frame every 3 ms, with lens 1
/// at 2.5 to 3 times the frame period, both lenses slower than the clock,
/// and the first case with the budget halved mid-play, the pacing rule
/// holds after the first 30 frames. Its control, the group base and the
/// group `until` off, freezes and fails it.
#[test]
#[ignore = "M3.8 unit (d)"]
fn a_locked_pair_paces_to_its_slowest_lens() {
    todo!()
}

/// A locked pair asks for six slots, whatever the setting.
#[test]
#[ignore = "M3.8 unit (d)"]
fn a_locked_pair_asks_for_six_slots() {
    todo!()
}

/// Slots killed to three, mid-play and mid-seek, unlock the pair with
/// `LockRefused { field: "slots" }` within `WAIT`, never stuck. Its
/// control, the lock without the floor, times out.
#[test]
#[ignore = "M3.8 unit (d)"]
fn the_slot_floor_unlocks_the_pair() {
    todo!()
}

/// A read error on lens 1 mid-play ends the pair within `WAIT`. Its
/// control, the rule off, neither ends nor shows a new pair within
/// `WAIT`.
#[test]
#[ignore = "M3.8 unit (d)"]
fn a_member_that_fails_mid_play_ends_the_pair() {
    todo!()
}
