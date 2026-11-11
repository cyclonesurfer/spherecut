//! The pacing rule (M3 plan section 9): a locked pair plays at its
//! slowest lens's pace, never freezing while the clock runs on. Over
//! what a run showed at each present, after the first [`WARM_UP`]
//! frames:
//!
//! - the pairs presented are at least [`SHARE`] of the frames the
//!   slowest lens hands out;
//! - the time presented trails that lens's newest frame by at most
//!   [`TRAIL`] frames, once the frame has had one present to go up: its
//!   newest as the present before saw it, and never past the clock's
//!   frame then, since a pair can't show ahead of the clock;
//! - play reaches the end.
//!
//! [`pacing`] holds a run's [`Sample`]s to it and names the part that
//! failed, and [`pacing_cut`] one that its own length ends; [`play`] drives a run on a `ManualClock` and gathers them,
//! [`play_steady`] drives one that keeps to the rule, and [`play_with`]
//! one whose clock a test moves itself.

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
    /// The pairs presented after the warm-up, and the frames the slowest
    /// lens handed out meanwhile.
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

/// The pacing rule, with or without its last part.
fn pacing_to(samples: &[Sample], rate: FrameRate, to_end: bool) -> Result<Paced, Unpaced> {
    let frame = |t: Time| rate.time_to_frame(t, Rounding::Floor).unwrap_or(i64::MIN);
    let start = samples
        .iter()
        .position(|s| frame(s.clock) >= WARM_UP)
        .ok_or(Unpaced::Short {
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
    let base = &samples[start.saturating_sub(1)];
    let pairs = (start..samples.len())
        .filter(|&at| {
            let s = &samples[at];
            s.shown.is_some() && (at == 0 || samples[at - 1].shown != s.shown)
        })
        .count() as u64;
    let last = samples.last().unwrap_or(base);
    let handed_out = (0..2)
        .map(|m| last.handed_out[m].saturating_sub(base.handed_out[m]))
        .min()
        .unwrap_or(0);
    if (pairs as f64) < SHARE * handed_out as f64 {
        return Err(Unpaced::FewPairs { pairs, handed_out });
    }
    if to_end && !samples.iter().any(|s| s.end) {
        return Err(Unpaced::NoEnd);
    }
    Ok(Paced {
        pairs,
        handed_out,
        trailed,
    })
}

/// How a paced run moves its clock: a frame of `rate` every `period` of
/// wall time, from frame 0 to `frames`, the clip's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pace {
    pub rate: FrameRate,
    pub period: Duration,
    pub frames: i64,
}

/// Play `player`, opened on `clock` and showing its first pair, at
/// `pace`, presenting about every millisecond, until play reaches the
/// end or `wait` passes: each present's [`Sample`] of `members`, with
/// `each` called after it with what it presented (to read the slots
/// back, or change the budget mid-play).
pub fn play(
    player: &Player,
    clock: &ManualClock,
    pace: Pace,
    members: [usize; 2],
    wait: Duration,
    each: impl FnMut(Option<&Presented>, &Sample),
) -> Vec<Sample> {
    let began = Instant::now();
    play_with(player, members, wait, each, || {
        let k = (began.elapsed().as_nanos() / pace.period.as_nanos().max(1)) as i64;
        let at = pace.rate.frame_to_time(k.min(pace.frames)).unwrap();
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
