//! Frame-locked pairs (M3 plan section 9, DECISIONS D67): a camera's two
//! lenses, opened with [`Player::open_locked`], shown only as pairs of
//! frames at one clip time.
//!
//! The lenses lock at open if their timing tables, read through
//! [`MediaBackend::track_timing`] whatever the decoder, agree on time
//! base, start (against [`LockSpec::offset`]), and `stts` and `stss`
//! over the common prefix, and their opened streams' info on a constant
//! rate and the size; otherwise the lock is refused by the [`field`]
//! that differs and they play unlocked. Once
//! locked, the presenter shows the newest time both lenses hold at or
//! before the clock ([`pick_pair`]), both slots or neither
//! ([`show_pair`], [`show_pair_over`]); the frame cache runs from the
//! group base, the newest time both hold, so that such a time always
//! exists; one catch-up time serves the group, so that the lenses skip
//! to the same frame; and the pair ends at its shorter lens.
//!
//! The group's members are player streams, by their place in what was
//! opened (`[0, 1]` for a pair of file streams), not file streams: the
//! cache's keys stay (stream, clip time), and a group of two members
//! over one stream, each with its columns (M3.14's LRV proxy, D71), keeps
//! them apart.
//!
//! [`Player::open_locked`]: crate::Player::open_locked
//! [`MediaBackend::track_timing`]: spherecut_media::backend::MediaBackend::track_timing

use std::time::Duration;

use spherecut_core::Time;

use crate::slots::{SlotState, SlotView};

/// How a pair is locked, given to [`Player::open_locked`].
///
/// [`Player::open_locked`]: crate::Player::open_locked
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockSpec {
    /// Lens 1's first presentation time less lens 0's, in the tracks'
    /// tables, which the open requires the two to differ by (else the
    /// lock is refused with [`field::START`]): 0 for the X5, whose tracks
    /// P-TRK found to start together (M3 plan 4.3, R1). Nothing else
    /// reads it. Each lens's clip time already starts at its own first
    /// frame, and its `stts` and `stss` are compared from there, so a
    /// container offset needs nothing more; an offset in the pictures at
    /// equal times can't be given here.
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
    /// Landings that differ stay as they landed, with no exact seek of
    /// both lenses to the later: a keyframe seek's, and an exact seek's
    /// whose landings both lie at or after its time.
    pub no_later_landing: bool,
    /// No `pair_resync` backstop when every member waits for room and no
    /// common time lies past the group base.
    pub no_resync: bool,
    /// No floor of two live slots a member: the group stays locked below
    /// it.
    pub no_floor: bool,
    /// A member that fails, mid-play or in a seek or a step, doesn't end
    /// the pair.
    pub no_end_on_failure: bool,
    /// The group `until` stays capped at the newest frame the other
    /// member holds even when every member trails the clock, so that a
    /// pair that trails together never catches up (the catch-up
    /// control).
    pub old_until: bool,
    /// A member that fails a seek or a step at or past its end isn't
    /// taken as landed on its last frame: the clock lands where the live
    /// member landed.
    pub no_failed_landing: bool,
    /// How long the upload thread waits before it publishes each frame of
    /// a locked pair, to make the pair late on purpose (the pacing
    /// rule's control); `None`, the default, costs nothing.
    pub publish_delay: Option<Duration>,
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
    /// couldn't open or failed before it handed out a frame, or the slots
    /// fell below the floor: they show unlocked, as
    /// [`Player::open_streams`] shows streams.
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
    /// A member's decoder couldn't open, or the member failed before it
    /// handed out its first frame of the open, whether the group had
    /// formed or not.
    pub const OPEN: &str = "open";
    /// A slot's death left fewer live slots than two a member.
    pub const SLOTS: &str = "slots";

    /// Every field: the comparisons in the order the open checks them,
    /// then the two that aren't comparisons.
    pub const ALL: [&str; 9] = [
        TIMING, TIME_BASE, START, RATE, SIZE, STTS, STSS, OPEN, SLOTS,
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
/// leaves one that was Shown already (the frame on screen) as it is, as
/// [`show_pair_over`] does for the presenter.
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

/// [`show_pair`] over the frames a presenter has on screen: a member
/// that shows its slot in `pair` already keeps that frame (`on_screen`),
/// and the other's slot goes up through `try_show`. If the second can't,
/// the first is given back through `unshow`, but only if this call moved
/// it from Ready to Shown: never the frame on screen, nor a slot that was
/// Shown already. Both frames, member 0's first, or `None` with the slots
/// as they were. A pure function of what it is given.
pub fn show_pair_over<G>(
    pair: &[SlotView; 2],
    on_screen: [Option<G>; 2],
    mut try_show: impl FnMut(&SlotView) -> Option<G>,
    mut unshow: impl FnMut(&SlotView),
) -> Option<[G; 2]> {
    let moved = on_screen[0].is_none() && pair[0].state == SlotState::Ready;
    let mut kept = on_screen.into_iter();
    show_pair(
        pair,
        |v| kept.next().flatten().or_else(|| try_show(v)),
        |v| {
            if moved {
                unshow(v);
            }
        },
    )
}
