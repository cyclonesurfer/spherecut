//! The pacing rule (M3 plan section 9): a locked pair plays at its
//! slowest lens's pace, never freezing while the clock runs on. Over
//! what a run showed at each present, after the first [`WARM_UP`]
//! frames:
//!
//! - the pairs presented are at least [`SHARE`] of the frames the
//!   slowest lens hands out, counted alike: pairs of frames it handed out
//!   after the warm-up began, since a lens that leads the clock handed
//!   its window out before;
//! - the time presented trails that lens's newest frame by at most
//!   [`TRAIL`] frames, once the frame has had one present to go up: its
//!   newest as the present before saw it, and never past the clock's
//!   frame then, since a pair can't show ahead of the clock;
//! - play reaches the end.
//!
//! [`pacing`] holds a run's [`Sample`]s to it and names the part that
//! failed, [`pacing_cut`] one that its own length ends, and [`pooled`]
//! several runs whose share is judged together, over at least
//! [`JUDGED`] frames; [`handed_out`] counts what a run's slowest lens
//! handed out after the warm-up, and [`freezes`] how long its pair stood
//! still against the clock, from how far behind it. [`play`] drives a
//! run on a `ManualClock` and gathers them, [`play_steady`] drives one
//! that keeps to the rule, and [`play_with`] one whose clock a test
//! moves itself.

use std::cell::Cell;
use std::time::{Duration, Instant};

use spherecut_core::{Clock, FrameRate, ManualClock, Rounding, Time};
use spherecut_playback::{Player, PlayerSnapshot, Presented};

use super::snapshot;

/// The frames at the start of a run the rule passes over, while the
/// cache and the slots fill.
pub const WARM_UP: i64 = 30;

/// The least share of the slowest lens's frames that must be presented
/// as pairs.
pub const SHARE: f64 = 0.8;

/// The most frames the time presented may trail the slowest lens's
/// newest frame.
pub const TRAIL: i64 = 2;

/// The fewest frames the slowest lenses of [`pooled`]'s runs must hand
/// out after the warm-up between them, for [`SHARE`] to judge by: over
/// fewer, one pair a held-up present missed fails it.
pub const JUDGED: u64 = 20;

/// The most frames [`play`] moves the clock on at a present, for a
/// [`Pace`] whose frames are shorter than a present: a test thread held
/// up between two presents moves it on two frames, not past frames no
/// present was there to show, which would count against the engine. A
/// pace whose frames are longer than a present moves it on one.
pub const MOST_A_PRESENT: i64 = 2;

/// What one present of a run showed, and where its lenses were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// The clock's time the present was for.
    pub clock: Time,
    /// The pair on screen: the members' frames' clip time when both show
    /// one and the two are at one time, else `None`.
    pub shown: Option<Time>,
    /// Each member's frames handed out so far, and the clip time of its
    /// newest.
    pub handed_out: [u64; 2],
    pub newest: [Option<Time>; 2],
    /// Play had reached the end, and paused there.
    pub end: bool,
}

impl Sample {
    /// What `presented` showed of `members` (nothing, when `None`), at
    /// the time it was for, with the lenses where `snap`, read after it,
    /// has them.
    pub fn of(presented: Option<&Presented>, snap: &PlayerSnapshot, members: [usize; 2]) -> Sample {
        let time = |m: usize| {
            presented?
                .frames
                .iter()
                .find(|f| f.stream == m)
                .map(|f| f.frame.time)
        };
        let shown = match members.map(time) {
            [Some(a), Some(b)] if a == b => Some(a),
            _ => None,
        };
        let stream = |m: usize| snap.streams.get(m);
        Sample {
            clock: presented.map_or(snap.clock, |p| p.time),
            shown,
            handed_out: members.map(|m| stream(m).map_or(0, |s| s.handed_out)),
            newest: members.map(|m| stream(m).and_then(|s| s.newest)),
            end: snap.at_end,
        }
    }
}

/// How a run kept to the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paced {
    /// The pairs presented after the warm-up of frames the slowest lens
    /// handed out meanwhile (past its newest at the present before the
    /// warm-up), and those frames.
    pub pairs: u64,
    pub handed_out: u64,
    /// The most frames the time presented trailed the slowest lens's
    /// newest frame.
    pub trailed: i64,
}

/// The part of the rule a run failed: the first in the order [`pacing`]
/// checks them, which is `Short`, `Trailed`, `FewPairs`, then `NoEnd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unpaced {
    /// The clock never passed the first [`WARM_UP`] frames: at most
    /// `clock`.
    Short { clock: Option<Time> },
    /// At sample `at`, the pair presented (`None`: no pair) trailed the
    /// slowest lens's newest frame as the sample before saw it, or the
    /// clock's frame then where that is earlier (`target`), by `frames`,
    /// more than [`TRAIL`].
    Trailed {
        at: usize,
        shown: Option<Time>,
        target: Time,
        frames: i64,
    },
    /// Fewer pairs presented than [`SHARE`] of the frames the slowest
    /// lens handed out after the warm-up.
    FewPairs { pairs: u64, handed_out: u64 },
    /// Play never reached the end.
    NoEnd,
    /// [`pooled`]'s runs handed out fewer than [`JUDGED`] frames after
    /// the warm-up between them: too few to judge the share by.
    Unjudged { handed_out: u64 },
}

/// `samples`, a run's presents in order, of a clip at `rate`, held to
/// the pacing rule.
pub fn pacing(samples: &[Sample], rate: FrameRate) -> Result<Paced, Unpaced> {
    pacing_to(samples, rate, true)
}

/// [`pacing`] for a run that a length of its own may end before the
/// clip's end, as a real-time benchmark's: the rule but its last part,
/// play reaching the end, which a freeze fails anyway by trailing.
pub fn pacing_cut(samples: &[Sample], rate: FrameRate) -> Result<Paced, Unpaced> {
    pacing_to(samples, rate, false)
}

/// Several runs of one test held to the pacing rule, the share judged
/// over them together: each run passes the warm-up, never trails and
/// reaches the end, and between them the slowest lenses hand out at
/// least [`JUDGED`] frames after the warm-up, of which the pairs
/// presented are at least [`SHARE`]. Each run's count, or the part the
/// runs failed: a run's own part, in [`pacing`]'s order, before
/// `Unjudged`, then `FewPairs` over them all.
pub fn pooled(runs: &[&[Sample]], rate: FrameRate) -> Result<Vec<Paced>, Unpaced> {
    let each = runs
        .iter()
        .map(|samples| {
            let paced = counted(samples, rate)?;
            if samples.iter().any(|s| s.end) {
                Ok(paced)
            } else {
                Err(Unpaced::NoEnd)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let pairs = each.iter().map(|p| p.pairs).sum::<u64>();
    let handed_out = each.iter().map(|p| p.handed_out).sum::<u64>();
    if handed_out < JUDGED {
        return Err(Unpaced::Unjudged { handed_out });
    }
    if (pairs as f64) < SHARE * handed_out as f64 {
        return Err(Unpaced::FewPairs { pairs, handed_out });
    }
    Ok(each)
}

/// A stand-still of a run's pair: from the present that put the pair up
/// (sample `at`), at the clock's frame `clock` and `lag` frames behind
/// it, for `frames` frames of the clock, to the present that put the next
/// pair up or to the run's last (sample `until`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freeze {
    pub at: usize,
    pub until: usize,
    pub clock: i64,
    pub lag: i64,
    pub frames: i64,
}

/// The stand-stills of a run's pair that reach past the warm-up, each in
/// full from the present that put its pair up (a present with no pair
/// stands still too). A pair behind the clock stands still while its
/// slowest lens catches up, so how long it may stand depends on how far
/// behind it was: a pair that slides into freezes longer than its
/// catch-ups take outgrows any bound on that, and one that freezes
/// outgrows it at once.
pub fn freezes(samples: &[Sample], rate: FrameRate) -> Vec<Freeze> {
    let frame = |t: Time| rate.time_to_frame(t, Rounding::Floor).unwrap_or(i64::MIN);
    let mut ups = Vec::new();
    let mut up: Option<Time> = None;
    for (at, s) in samples.iter().enumerate() {
        if s.shown.is_some() && s.shown != up {
            ups.push(at);
            up = s.shown;
        }
    }
    let last = samples.len().saturating_sub(1);
    (ups.iter().enumerate())
        .filter_map(|(i, &at)| {
            let clock = frame(samples[at].clock);
            let until = ups.get(i + 1).copied().unwrap_or(last);
            let shown = frame(samples[at].shown?);
            Some(Freeze {
                at,
                until,
                clock,
                lag: clock.saturating_sub(shown),
                frames: frame(samples[until].clock).saturating_sub(clock),
            })
        })
        .filter(|f| f.clock.saturating_add(f.frames) >= WARM_UP)
        .collect()
}

/// The frames the slowest lens of a run handed out after the warm-up, as
/// [`pacing`] counts them, whatever else the run did: 0 for a run that
/// never passed the warm-up.
pub fn handed_out(samples: &[Sample], rate: FrameRate) -> u64 {
    warm(samples, rate).map_or(0, |start| handed_out_from(samples, start).0)
}

/// The first sample past the warm-up.
fn warm(samples: &[Sample], rate: FrameRate) -> Option<usize> {
    let frame = |t: Time| rate.time_to_frame(t, Rounding::Floor).unwrap_or(i64::MIN);
    samples.iter().position(|s| frame(s.clock) >= WARM_UP)
}

/// The frames the slowest lens handed out from the sample before `start`
/// to the last, and that lens's newest frame at that sample: the frames
/// it handed out are those past it. The slowest lens is the one behind
/// there, its newest frame the earlier (or none), whose frames are the
/// pairs' to come, or where the two are level the one that hands out
/// fewer: a lens that leads, held back by its window, may hand out
/// catch-up landings that the lens behind passes over on its way to a
/// later one.
fn handed_out_from(samples: &[Sample], start: usize) -> (u64, Option<Time>) {
    let base = &samples[start.saturating_sub(1)];
    let last = samples.last().unwrap_or(base);
    let each = [0, 1].map(|m| last.handed_out[m].saturating_sub(base.handed_out[m]));
    let slowest = match base.newest {
        [a, b] if a != b => usize::from(b < a),
        _ => usize::from(each[1] < each[0]),
    };
    (each[slowest], base.newest[slowest])
}

/// The pacing rule, with or without its last part.
fn pacing_to(samples: &[Sample], rate: FrameRate, to_end: bool) -> Result<Paced, Unpaced> {
    let paced = counted(samples, rate)?;
    if (paced.pairs as f64) < SHARE * paced.handed_out as f64 {
        return Err(Unpaced::FewPairs {
            pairs: paced.pairs,
            handed_out: paced.handed_out,
        });
    }
    if to_end && !samples.iter().any(|s| s.end) {
        return Err(Unpaced::NoEnd);
    }
    Ok(paced)
}

/// A run past the warm-up that never trailed, and its counts; the share
/// and the end are its callers'.
fn counted(samples: &[Sample], rate: FrameRate) -> Result<Paced, Unpaced> {
    let frame = |t: Time| rate.time_to_frame(t, Rounding::Floor).unwrap_or(i64::MIN);
    let start = warm(samples, rate).ok_or(Unpaced::Short {
        clock: samples.iter().map(|s| s.clock).max(),
    })?;
    let mut trailed = 0;
    for at in start.max(1)..samples.len() {
        let (before, s) = (&samples[at - 1], &samples[at]);
        // The slowest lens's newest frame, once both have handed one out,
        // and the clock's, as the present before saw them: a present
        // late by frames gives the slots no time to fill.
        let [Some(n0), Some(n1)] = before.newest else {
            continue;
        };
        let target = n0.min(n1).min(before.clock);
        let frames = s
            .shown
            .map_or(i64::MAX, |t| frame(target).saturating_sub(frame(t)).max(0));
        if frames > TRAIL {
            return Err(Unpaced::Trailed {
                at,
                shown: s.shown,
                target,
                frames,
            });
        }
        trailed = trailed.max(frames);
    }
    // The pairs of frames the slowest lens handed out after the warm-up
    // began, as its count is: a pair of a frame it handed out before
    // counts against nothing.
    let (handed_out, past) = handed_out_from(samples, start);
    let pairs = (start..samples.len())
        .filter(|&at| {
            let s = &samples[at];
            s.shown.is_some_and(|t| past.is_none_or(|p| t > p))
                && (at == 0 || samples[at - 1].shown != s.shown)
        })
        .count() as u64;
    Ok(Paced {
        pairs,
        handed_out,
        trailed,
    })
}

/// How a paced run moves its clock: a frame of `rate` every `period` of
/// wall time, from frame 0 to `frames`, the clip's end, and at most
/// `most` frames at a present ([`MOST_A_PRESENT`], or 1 where a frame
/// lasts longer than a present, so that a present held up never passes
/// a frame over).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pace {
    pub rate: FrameRate,
    pub period: Duration,
    pub frames: i64,
    pub most: i64,
}

/// Play `player`, opened on `clock` and showing its first pair, at
/// `pace`, presenting about every millisecond, until play reaches the
/// end or `wait` passes: each present's [`Sample`] of `members`, with
/// `each` called after it with what it presented (to read the slots
/// back, or change the budget mid-play). `each` returns how long of its
/// own time the clock leaves out: time it held the presents up that
/// isn't the engine's, which would have the clock run on past frames
/// no present was there to show. The clock keeps to the wall clock's
/// frame, less that, but moves on at most `pace.most` frames a present.
pub fn play(
    player: &Player,
    clock: &ManualClock,
    pace: Pace,
    members: [usize; 2],
    wait: Duration,
    mut each: impl FnMut(Option<&Presented>, &Sample) -> Duration,
) -> Vec<Sample> {
    let began = Instant::now();
    let left_out = Cell::new(Duration::ZERO);
    let each = |p: Option<&Presented>, s: &Sample| left_out.set(left_out.get() + each(p, s));
    play_with(player, members, wait, each, || {
        let played = began.elapsed().saturating_sub(left_out.get());
        let k = (played.as_nanos() / pace.period.as_nanos().max(1)) as i64;
        let from = pace
            .rate
            .time_to_frame(clock.now(), Rounding::Floor)
            .unwrap();
        let k = k.min(from + pace.most).min(pace.frames);
        let at = pace.rate.frame_to_time(k).unwrap();
        if clock.is_running() && clock.now() < at {
            clock.set(at, Instant::now());
        }
    })
}

/// Play `player` as [`play`] does, from the clock's frame to `frames`,
/// the end of a clip at `rate`, but moving the clock on a frame only
/// once both members' frames there are up in slots, never on the wall
/// clock: a run that keeps to the rule however slowly the slots fill,
/// the counterpart to one that freezes.
pub fn play_steady(
    player: &Player,
    clock: &ManualClock,
    rate: FrameRate,
    frames: i64,
    members: [usize; 2],
    wait: Duration,
    each: impl FnMut(Option<&Presented>, &Sample),
) -> Vec<Sample> {
    let epoch = snapshot(player).epoch;
    let up = |t: Time| {
        let views = player.slots().views();
        members.iter().all(|&m| {
            views
                .iter()
                .any(|v| v.frame.epoch == epoch && v.frame.stream == m && v.frame.time == t)
        })
    };
    play_with(player, members, wait, each, || {
        if !clock.is_running() {
            return;
        }
        let k = rate.time_to_frame(clock.now(), Rounding::Floor).unwrap();
        let next = rate.frame_to_time((k + 1).min(frames)).unwrap();
        if k < frames && (k + 1 >= frames || up(next)) {
            clock.set(next, Instant::now());
        }
    })
}

/// Play `player` as [`play`] does, but calling `step` before each
/// present to move the clock, and whatever else the run changes (a
/// lens's hold, say).
pub fn play_with(
    player: &Player,
    members: [usize; 2],
    wait: Duration,
    mut each: impl FnMut(Option<&Presented>, &Sample),
    mut step: impl FnMut(),
) -> Vec<Sample> {
    let mut samples = Vec::new();
    player.play();
    let began = Instant::now();
    while began.elapsed() < wait {
        step();
        let p = player.present(Instant::now());
        let s = Sample::of(p.as_ref(), &snapshot(player), members);
        each(p.as_ref(), &s);
        samples.push(s);
        if s.end {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    samples
}
