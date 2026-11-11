//! Frame-locked pairs (M3 plan section 9, DECISIONS D67): a camera's two
//! lenses, opened with [`Player::open_locked`], shown only as pairs of
//! frames at one clip time.
//!
//! The lenses lock at open if their timing tables, read through
//! [`MediaBackend::track_timing`] whatever the decoder, agree on time
//! base, start (after [`LockSpec::offset`]), constant rate, size, and
//! `stts` and `stss` over the common prefix; otherwise the lock is
//! refused by the [`field`] that differs and they play unlocked. Once
//! locked, the presenter shows the newest time both lenses hold at or
//! before the clock ([`pick_pair`]), both slots or neither
//! ([`show_pair`]); the frame cache runs from the group base, the newest
//! time both hold, so that such a time always exists; one catch-up time
//! serves the group, so that the lenses skip to the same frame; and the
//! pair ends at its shorter lens.
//!
//! The group's members are player streams, by their place in what was
//! opened (`[0, 1]` for a pair of file streams), not file streams: the
//! cache's keys stay (stream, clip time), and a group of two members
//! over one stream, each with its columns (M3.14's LRV proxy, D71), keeps
//! them apart.
//!
//! [`Player::open_locked`]: crate::Player::open_locked
//! [`MediaBackend::track_timing`]: spherecut_media::backend::MediaBackend::track_timing

use spherecut_core::Time;

use crate::slots::{SlotState, SlotView};

/// How a pair is locked, given to [`Player::open_locked`].
///
/// [`Player::open_locked`]: crate::Player::open_locked
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockSpec {
    /// Lens 1's start less lens 0's, which the open checks and takes off
    /// lens 1's times before it compares the two lenses' tables: 0 for
    /// the X5, whose tracks P-TRK found to start together (M3 plan 4.3,
    /// R1).
    pub offset: Time,
    /// The tests' switches, all off but in a control.
    #[doc(hidden)]
    pub test: LockTest,
}

impl Default for LockSpec {
    /// No offset, and every rule on.
    fn default() -> Self {
        LockSpec {
            offset: Time::ZERO,
            test: LockTest::default(),
        }
    }
}

/// Rules of the lock a control turns off, to show that the test it
/// controls fails without them (M3 plan 4.1 rule 1). Each is off
/// (`false`) by default, which is the lock as built.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockTest {
    /// No group base and no group `until`: the cache runs from the
    /// presenter's playhead and each member catches up on its own, as
    /// unlocked streams do (the pacing rule's control).
    pub no_group_base: bool,
    /// A seek or a step keeps the old common time as the group base
    /// instead of the clock's time until every member lands.
    pub old_base_in_seeks: bool,
    /// Keyframe landings that differ stay as they landed, with no exact
    /// seek of both lenses to the later.
    pub no_later_landing: bool,
    /// No `pair_resync` backstop when every member waits for room and no
    /// common time lies past the group base.
    pub no_resync: bool,
    /// No floor of two live slots a member: the group stays locked below
    /// it.
    pub no_floor: bool,
    /// A member that fails mid-play doesn't end the pair.
    pub no_end_on_failure: bool,
}

/// Where a locked pair is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockStatus {
    /// Opening: the lenses' tables and decoders aren't all read yet, and
    /// nothing of the pair shows.
    Pending,
    /// The lenses agreed: they show only as pairs.
    Locked,
    /// They didn't agree on `field` (one of [`field`]'s), or a member
    /// couldn't open, or the slots fell below the floor: they show
    /// unlocked, as [`Player::open_streams`] shows streams.
    ///
    /// [`Player::open_streams`]: crate::Player::open_streams
    Refused { field: &'static str },
}

/// A locked pair's state, for the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockSnapshot {
    pub status: LockStatus,
    /// The group base: the newest clip time both lenses hold at or
    /// before the playhead, which the cache runs from.
    pub base: Option<Time>,
    /// The time the group catches up to, if it does.
    pub until: Option<Time>,
}

/// The names [`LockStatus::Refused`] and `PlayerEvent::LockRefused` give
/// for why a pair didn't lock, or unlocked.
pub mod field {
    /// The tracks' time bases differ.
    pub const TIME_BASE: &str = "time_base";
    /// The lenses' first frames aren't at the same clip time, after the
    /// spec's offset.
    pub const START: &str = "start";
    /// A lens's frame rate isn't constant, or the two differ.
    pub const RATE: &str = "rate";
    /// The lenses' pictures differ in size.
    pub const SIZE: &str = "size";
    /// The samples' decode times differ over the common prefix.
    pub const STTS: &str = "stts";
    /// The sync samples differ over the common prefix.
    pub const STSS: &str = "stss";
    /// A track has no timing tables to compare.
    pub const TIMING: &str = "timing";
    /// A member's decoder couldn't open before the group formed.
    pub const OPEN: &str = "open";
    /// A slot's death left fewer live slots than two a member.
    pub const SLOTS: &str = "slots";

    /// Every field, in the order the open checks them, the two that
    /// aren't about the tables last.
    pub const ALL: [&str; 9] = [
        TIME_BASE, START, RATE, SIZE, STTS, STSS, TIMING, OPEN, SLOTS,
    ];
}

/// The pair to show at the clock's time `t`: of the Ready and Shown slots
/// in `views` of open `epoch`, the two of `members` (player streams) at
/// the largest clip time T ≤ `t` at which both have one at exactly T,
/// member 0's first; `None` when no such time exists. A pure function of
/// what it is given (M3 plan section 9; the plan's slot ids are the
/// views here).
///
/// Where a member has two slots at T (its frame uploaded again after a
/// seek back while the old one is on screen), it takes the Shown one, so
/// that the frame on screen stays up, else the one with the higher place
/// in its run, else the one in the lower slot.
pub fn pick_pair(
    views: &[SlotView],
    members: [usize; 2],
    epoch: u64,
    t: Time,
) -> Option<[SlotView; 2]> {
    let held = |m: usize| {
        views.iter().filter(move |v| {
            matches!(v.state, SlotState::Ready | SlotState::Shown)
                && v.frame.epoch == epoch
                && v.frame.stream == m
                && v.frame.time <= t
        })
    };
    // Member `m`'s slot at exactly `time`, by the rule above.
    let at = |m: usize, time: Time| {
        held(m).filter(|v| v.frame.time == time).max_by(|a, b| {
            let rank = |v: &SlotView| (v.state == SlotState::Shown, v.frame.seq);
            rank(a).cmp(&rank(b)).then_with(|| b.index.cmp(&a.index))
        })
    };
    let common = held(members[0])
        .map(|v| v.frame.time)
        .filter(|&time| at(members[1], time).is_some())
        .max()?;
    Some([*at(members[0], common)?, *at(members[1], common)?])
}

/// Show both slots of `pair` or neither: each through `try_show`, and if
/// the second fails, the first given back through `give_back`. Both
/// frames, member 0's first, or `None` with nothing left shown.
///
/// `give_back` gets the view of the first slot as it was read: a caller
/// gives back a slot its `try_show` moved from Ready to Shown, and
/// leaves one that was Shown already (the frame on screen) as it is.
pub fn show_pair<G>(
    pair: &[SlotView; 2],
    mut try_show: impl FnMut(&SlotView) -> Option<G>,
    mut give_back: impl FnMut(&SlotView),
) -> Option<[G; 2]> {
    let first = try_show(&pair[0])?;
    match try_show(&pair[1]) {
        Some(second) => Some([first, second]),
        None => {
            give_back(&pair[0]);
            None
        }
    }
}
