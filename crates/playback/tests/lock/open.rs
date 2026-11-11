//! The open (M3 plan section 9): a pair locks only when its lenses'
//! timing tables agree, read through `MediaBackend::track_timing`
//! whatever the decoder; otherwise it is refused by the field that
//! differs and plays unlocked.

/// Opened locked, the pair shows nothing while its tables and decoders
/// are being read (`LockStatus::Pending`, lens 1's open held by a gate),
/// and both lenses together once it has locked.
#[test]
#[ignore = "M3.8 unit (a)"]
fn nothing_of_the_pair_shows_until_it_locks() {
    todo!()
}

/// Planted faults through `PerStream`'s tables (start pts apart by more
/// than `LockSpec.offset`, rates, sizes, `stts` and `stss` that differ,
/// the last as GOPs of 8 and 10, and no tables) are each refused by the
/// field that differs, with one `LockRefused`, and the lenses play
/// unlocked; an offset equal to the planted start locks.
#[test]
#[ignore = "M3.8 unit (a)"]
fn each_planted_fault_is_refused_by_name() {
    todo!()
}

/// A member whose open fails before the group forms refuses the lock
/// with field `"open"`, and the other lens plays unlocked.
#[test]
#[ignore = "M3.8 unit (a)"]
fn a_member_that_cant_open_refuses_the_lock() {
    todo!()
}

/// Lens 1 a frame short locks (the count isn't compared), and the player
/// says so once, with `LensShort` naming the stream and its frames.
#[test]
#[ignore = "M3.8 unit (a)"]
fn a_lens_a_frame_short_locks_and_says_so() {
    todo!()
}

/// A 2×64² two-track MP4 from M3.1's writer with the GOP fault is refused
/// by `"stss"` through `FfmpegBackend::track_timing`, so the file path is
/// tested too.
#[test]
#[ignore = "M3.8 unit (a)"]
fn the_gop_fault_file_is_refused_through_ffmpeg() {
    todo!()
}

/// The open reads both lenses' tables once each through the backend
/// under every decoder preference, Software included, where no probe
/// runs.
#[test]
#[ignore = "M3.8 unit (a)"]
fn timing_is_read_whatever_the_decoder() {
    todo!()
}
