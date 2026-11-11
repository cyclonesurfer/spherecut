//! The present (M3 plan section 9): `pick_pair` takes the newest time
//! both lenses hold, `show_pair` shows both slots or neither, and the
//! counters say what was shown: `unpaired_presents` over every stream,
//! locked or not, `pair_wait`, and dropped frames once a pair.
//!
//! The players here are on six GPU slots, a locked pair's, and on
//! `ManualClock`s the tests move themselves, most of them never past
//! lens 1's newest frame, so that both lenses always hold a pair at or
//! before the clock.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::sync::Arc;
use std::time::{Duration, Instant};

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{TestError, TestRng, TestRunner};
use spherecut_core::{Clock, FrameRate, ManualClock, Rational, Rounding, Time};
use spherecut_media::backend::SeekMode;
use spherecut_playback::lock::{pick_pair, show_pair};
use spherecut_playback::{
    CacheBudget, Counter, DecoderPreference, LockSpec, LockStatus, LockTest, PerfStats, Player,
    PlayerConfig, Presented, SlotFrame, SlotState, SlotView, Stage,
};
use spherecut_testkit::control::{above, at_least, must_fail};

use crate::common::pacing::{self, Pace, Unpaced};
use crate::common::per_stream::{Lens, PerStream};
use crate::common::{self, snapshot, until};
use crate::{WAIT, timed};

/// What `PerStream` opens: any name will do.
const LENSES: &str = "lenses.synthetic";

/// The clips' rate.
const RATE: FrameRate = FrameRate::FPS_30;

/// The clip time of frame `k`.
fn at(k: i64) -> Time {
    RATE.frame_to_time(k).unwrap()
}

/// The frame at or before `t`.
fn frame_of(t: Time) -> i64 {
    RATE.time_to_frame(t, Rounding::Floor).unwrap()
}

/// The clip time of each stream's frame `p` shows, by stream.
fn times(p: &Presented) -> Vec<Time> {
    p.frames.iter().map(|f| f.frame.time).collect()
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

/// The most slots a case has.
const SLOTS: usize = 12;

/// A case of `pick_pair`'s property: Ready and Shown slots of streams 0
/// to 2 in opens 1 and 2, at the first 8 frames' times, many at a time
/// another slot of their stream holds too, in no order; the members and
/// the open asked for; the clock's time, on frames and between them; and
/// which slots' `try_show` fails.
#[derive(Debug, Clone)]
struct Case {
    views: Vec<SlotView>,
    members: [usize; 2],
    epoch: u64,
    t: Time,
    fails: Vec<bool>,
}

fn case() -> impl Strategy<Value = Case> {
    let slot = (any::<bool>(), 0..3usize, 1..=2u64, 0..8i64, 0..4u64);
    let views = vec(slot, 0..=SLOTS)
        .prop_map(|slots| {
            slots
                .into_iter()
                .enumerate()
                .map(|(index, (shown, stream, epoch, k, seq))| SlotView {
                    index,
                    state: if shown {
                        SlotState::Shown
                    } else {
                        SlotState::Ready
                    },
                    word: index as u64,
                    frame: SlotFrame {
                        stream,
                        epoch,
                        time: at(k),
                        pts: k,
                        run: 0,
                        seq,
                        duration: Some(RATE.frame_duration()),
                    },
                })
                .collect::<Vec<_>>()
        })
        .prop_shuffle();
    (
        views,
        0..3usize,
        1..3usize,
        1..=2u64,
        0..18i64,
        vec(any::<bool>(), SLOTS),
    )
        .prop_map(|(views, first, apart, epoch, halves, fails)| Case {
            views,
            members: [first, (first + apart) % 3],
            epoch,
            t: Time(Rational::new(halves, 60).unwrap()),
            fails,
        })
}

/// A `pick_pair`.
type Pick = fn(&[SlotView], [usize; 2], u64, Time) -> Option<[SlotView; 2]>;

/// The check of `pick` on `case`, and of `show_pair` on what it took,
/// with each slot's `try_show` failing where the case says and the first
/// slot given back when the second fails if `give_back`: never a mixed
/// pair; the members' slots in the open at the largest time at or before
/// the clock at which both have one, found by trying every time, each
/// the slot the tie rule takes; and the pair up whole, or the slots as
/// they were. The error says what broke.
fn holds(case: &Case, pick: Pick, give_back: bool) -> Result<(), String> {
    let [m0, m1] = case.members;
    let got = pick(&case.views, case.members, case.epoch, case.t);
    if let Some(p) = got {
        if p[0].frame.time != p[1].frame.time {
            return Err(format!("a mixed pair: {p:?}"));
        }
        let theirs = (0..2).all(|k| {
            case.views.contains(&p[k])
                && p[k].frame.stream == case.members[k]
                && p[k].frame.epoch == case.epoch
                && p[k].frame.time <= case.t
        });
        if !theirs {
            return Err(format!("not the members' slots in the open: {p:?}"));
        }
    }
    // Member `m`'s slots at exactly `time`, best by the tie rule first:
    // the Shown one, then the higher place in its run, then the lower
    // slot.
    let at_time = |m: usize, time: Time| {
        let mut held: Vec<SlotView> = case
            .views
            .iter()
            .filter(|v| v.frame.stream == m && v.frame.epoch == case.epoch && v.frame.time == time)
            .copied()
            .collect();
        held.sort_by_key(|v| (v.state != SlotState::Shown, Reverse(v.frame.seq), v.index));
        held
    };
    let largest = (0..8)
        .rev()
        .map(at)
        .filter(|&time| time <= case.t)
        .find(|&time| !at_time(m0, time).is_empty() && !at_time(m1, time).is_empty());
    let (pair, time) = match (got, largest) {
        (None, None) => return Ok(()),
        (Some(p), Some(time)) if p[0].frame.time == time => (p, time),
        (got, _) => {
            return Err(format!(
                "took {:?}, not the largest common time {largest:?}",
                got.map(|p| p[0].frame.time)
            ));
        }
    };
    let best = [at_time(m0, time)[0], at_time(m1, time)[0]];
    if pair != best {
        return Err(format!("{pair:?}, not {best:?} by the tie rule"));
    }

    // The slots' states by index, through `show_pair`.
    let mut states = vec![SlotState::Free; SLOTS];
    for v in &case.views {
        states[v.index] = v.state;
    }
    let before = states.clone();
    let states = RefCell::new(states);
    let up = show_pair(
        &pair,
        |v| {
            if case.fails[v.index] {
                return None;
            }
            states.borrow_mut()[v.index] = SlotState::Shown;
            Some(v.index)
        },
        |v| {
            if give_back && v.state == SlotState::Ready {
                states.borrow_mut()[v.index] = SlotState::Ready;
            }
        },
    );
    let after = states.into_inner();
    match up {
        Some(up) => {
            let whole = up == [pair[0].index, pair[1].index]
                && up
                    .iter()
                    .all(|&i| after[i] == SlotState::Shown && !case.fails[i]);
            if !whole {
                return Err(format!("{up:?} shown for {pair:?}"));
            }
        }
        None if after != before => {
            let alone: Vec<usize> = (0..SLOTS).filter(|&i| after[i] != before[i]).collect();
            return Err(format!(
                "a pair that failed left slots {alone:?} shown alone"
            ));
        }
        None => {}
    }
    Ok(())
}

/// [`holds`] over `n` cases of [`case`], from a seeded generator when
/// `seeded`: the controls, which must fail the same way every run.
fn run(n: u32, seeded: bool, pick: Pick, give_back: bool) -> Result<(), TestError<Case>> {
    let config = ProptestConfig {
        cases: n,
        failure_persistence: None,
        ..ProptestConfig::default()
    };
    let mut runner = if seeded {
        let rng = TestRng::deterministic_rng(config.rng_algorithm);
        TestRunner::new_with_rng(config, rng)
    } else {
        TestRunner::new(config)
    };
    runner.run(&case(), |c| {
        holds(&c, pick, give_back).map_err(TestCaseError::fail)
    })
}

/// `pick_pair`'s control: each member's newest slot at or before `t`
/// alone, by the same tie rule.
fn newest_alone(
    views: &[SlotView],
    members: [usize; 2],
    epoch: u64,
    t: Time,
) -> Option<[SlotView; 2]> {
    let newest = |m: usize| pick_pair(views, [m, m], epoch, t).map(|p| p[0]);
    Some([newest(members[0])?, newest(members[1])?])
}

/// `pick_pair`'s property, at `PROPTEST_CASES=100000` apart: over random
/// slot states, epochs, times and failed `try_show`s, never a mixed pair,
/// and always the largest common time at or before the clock.
#[test]
fn pick_pair_takes_the_largest_common_time() {
    let _time = timed("pick_pair_takes_the_largest_common_time");
    if let Err(e) = run(cases(), false, pick_pair, true) {
        panic!("{e}");
    }

    // Controls: each member's newest frame alone mixes the pair, and a
    // first slot not given back is left shown alone.
    let mixed = must_fail(
        "each member's newest alone",
        run(256, true, newest_alone, true),
    );
    assert!(mixed.to_string().contains("a mixed pair"), "{mixed}");
    let alone = must_fail("nothing given back", run(256, true, pick_pair, false));
    assert!(alone.to_string().contains("shown alone"), "{alone}");
}

/// A player of `backend` on a paused `ManualClock`, decoding in software
/// as far as `ahead` past the playhead, on six GPU slots.
fn player(backend: PerStream, ahead: Duration) -> (Player, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = common::player(
        backend,
        clock.clone(),
        PlayerConfig {
            cache: CacheBudget::Bytes(64 << 20),
            ahead,
            gpu_slots: 6,
            decoder: DecoderPreference::Software,
            ..PlayerConfig::default()
        },
    );
    (player, clock)
}

/// Lenses of `frames` frames, lens 0 at `costs[0]` a frame and lens 1 at
/// `costs[1]`.
fn lenses(frames: u64, costs: [Duration; 2]) -> PerStream {
    let lens = |cost| Lens::default().frames(frames).cost(cost);
    PerStream::pair(lens(costs[0]), lens(costs[1]))
}

/// Open both lenses, locked by `lock` or, with `None`, through
/// `open_streams` (a locked pair must lock), and show both at frame 1
/// once every slot holds a frame: paused there, each lens wants its
/// frames 0 to 2 up. A slot is made the first time a frame goes in,
/// which on lavapipe takes long enough to hold up the first frames
/// played.
fn open(player: &Player, lock: Option<LockSpec>) {
    match lock {
        Some(spec) => player.open_locked(LENSES.into(), [0, 1], spec),
        None => player.open_streams(LENSES.into(), vec![Some(0), Some(1)]),
    }
    until(WAIT, || {
        player
            .present(Instant::now())
            .filter(|p| p.frames.len() == 2)
    })
    .expect("the lenses never both showed");
    let status = snapshot(player).lock.map(|l| l.status);
    assert_eq!(status, lock.map(|_| LockStatus::Locked));
    player.seek(at(1), SeekMode::Exact);
    until(WAIT, || {
        let p = player.present(Instant::now())?;
        (times(&p) == [at(1); 2] && player.slots().views().len() == 6).then_some(())
    })
    .expect("frame 1 never showed with every slot full");
}

/// Seek to frame `from` and play from there to frame `to`, then pause,
/// presenting about every millisecond and calling `each` with what each
/// present showed, failing after [`WAIT`]. The seek's preroll lets the
/// slots fill before the clock runs: a player's first uploads include
/// slow ones (on lavapipe its first and its ninth took 33 ms each),
/// which would stall the first frames played. While it runs, the clock
/// moves on to lens `lead`'s newest frame, never past `to`, and with
/// `keep_up`, never more than a frame past the pair on screen. Led by
/// lens 1, the slower, the clock never leaves it behind, and with
/// `keep_up` the pair isn't either.
fn play_to(
    player: &Player,
    clock: &ManualClock,
    [from, to]: [i64; 2],
    lead: usize,
    keep_up: bool,
    mut each: impl FnMut(&Presented),
) {
    player.seek(at(from), SeekMode::Exact);
    player.play();
    let began = Instant::now();
    let mut shown = None;
    loop {
        if clock.is_running()
            && (shown.is_some() || !keep_up)
            && let Some(newest) = snapshot(player).streams.get(lead).and_then(|s| s.newest)
        {
            let mut next = newest.min(at(to));
            if keep_up && let Some(t) = shown {
                next = next.min(at(frame_of(t) + 1));
            }
            if clock.now() < next {
                clock.set(next, Instant::now());
            }
        }
        if let Some(p) = player.present(Instant::now()) {
            shown = p.frames.first().map(|f| f.frame.time);
            each(&p);
        }
        if clock.is_running() && frame_of(clock.now()) >= to {
            break;
        }
        assert!(
            began.elapsed() < WAIT,
            "play never reached frame {to}: {:?}",
            clock.now()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    player.pause();
}

/// The check: every present showed both lenses at one clip time. The
/// pairs shown, or the first present that showed a lens's frame without
/// its pair, by its place and what it showed.
fn all_or_none(seen: &[Vec<Time>]) -> Result<usize, (usize, Vec<Time>)> {
    let mut pairs = 0;
    let mut last = None;
    for (place, shown) in seen.iter().enumerate() {
        match shown.as_slice() {
            [a, b] if a == b => {
                if last != Some(*a) {
                    pairs += 1;
                    last = Some(*a);
                }
            }
            _ => return Err((place, shown.clone())),
        }
    }
    Ok(pairs)
}

/// Lens 1 at three times lens 0's cost.
const SLOW_LENS_1: [Duration; 2] = [Duration::from_millis(1), Duration::from_millis(3)];

/// Two frames past the playhead: the window, so that a lens hands out
/// each frame just before a clock its newest frame leads reaches it.
const TWO_FRAMES: Duration = Duration::from_millis(67);

/// A locked pair's present shows both slots or neither: when the second
/// `try_show` fails, the first is given back, and the frame on screen
/// stays.
#[test]
fn a_locked_pair_is_shown_all_or_none() {
    let _time = timed("a_locked_pair_is_shown_all_or_none");
    let (player, clock) = player(lenses(40, SLOW_LENS_1), TWO_FRAMES);
    // What each present showed, played through `segments`, each from a
    // seek to a frame, the clock led by lens `lead`.
    let run = |lock: Option<LockSpec>, lead: usize, segments: &[[i64; 2]]| {
        open(&player, lock);
        let mut seen = Vec::new();
        for &segment in segments {
            play_to(&player, &clock, segment, lead, false, |p| {
                seen.push(times(p))
            });
        }
        seen
    };
    // The seeks after the first land on frames not yet decoded, so that
    // lens 1's newest frame is where it landed.
    let seen = run(Some(LockSpec::default()), 1, &[[2, 12], [30, 38], [18, 26]]);
    let pairs = all_or_none(&seen).unwrap_or_else(|(place, shown)| {
        panic!("present {place} of {} showed {shown:?}", seen.len())
    });
    let enough = at_least(pairs, 20);
    assert_eq!(enough.passes(), Some(true), "pairs shown: {enough}");

    // Control: unlocked, with the clock led by lens 0, lens 0's frames
    // show before lens 1's come.
    let unlocked = run(None, 0, &[[2, 12]]);
    let (_, shown) = must_fail("the lenses unlocked", all_or_none(&unlocked));
    assert_eq!(shown.len(), 2, "{shown:?}");
    common::join(player);
}

/// `unpaired_presents` counts the presents at which the streams shown
/// are at different clip times, over every stream, through
/// `open_streams` as through `open_locked`.
#[test]
fn unpaired_presents_count_over_every_stream() {
    let _time = timed("unpaired_presents_count_over_every_stream");
    let (player, clock) = player(lenses(30, SLOW_LENS_1), TWO_FRAMES);
    // The presents of a play from frame 2 to 12, the clock led by lens
    // 0, that showed the lenses at different times, as seen and as
    // counted.
    let run = |lock: Option<LockSpec>| {
        open(&player, lock);
        let before = player.perf().count(Counter::UnpairedPresents);
        let mut apart = 0u64;
        play_to(&player, &clock, [2, 12], 0, false, |p| {
            apart += u64::from(times(p).windows(2).any(|w| w[0] != w[1]));
        });
        (
            apart,
            player.perf().count(Counter::UnpairedPresents) - before,
        )
    };
    let (apart, counted) = run(None);
    assert_eq!(counted, apart, "counted, and seen");
    let some = above(counted, 0);
    assert_eq!(some.passes(), Some(true), "unpaired presents: {some}");

    // Control: a locked pair's are none.
    let (apart, counted) = run(Some(LockSpec::default()));
    assert_eq!(counted, apart, "counted, and seen, locked");
    must_fail("a locked pair's unpaired presents", above(counted, 0));
    common::join(player);
}

/// `pair_wait`'s samples, read after each present: each must be the
/// clock's time less the time of the pair that present showed, in
/// milliseconds.
#[derive(Debug, Default)]
struct Waits {
    seen: u64,
    max: f32,
    wrong: Option<String>,
}

impl Waits {
    /// From the samples `perf` has recorded so far on.
    fn from(perf: &PerfStats) -> Waits {
        Waits {
            seen: perf.samples_after(Stage::PairWait, 0).0,
            ..Waits::default()
        }
    }

    /// The samples since the present before, recorded by present `p`
    /// (`None`: nothing presented).
    fn after(&mut self, perf: &PerfStats, p: Option<&Presented>) {
        let (seen, new) = perf.samples_after(Stage::PairWait, self.seen);
        self.seen = seen;
        let want = p.and_then(|p| {
            let d = p.time.checked_sub(p.frames.first()?.frame.time).ok()?;
            Some(d.to_secs_f64() * 1000.0)
        });
        let right = match (new.as_slice(), want) {
            ([], _) => true,
            ([ms], Some(want)) => (f64::from(*ms) - want).abs() < 1e-3,
            _ => false,
        };
        if right {
            self.max = new.iter().fold(self.max, |m, &ms| m.max(ms));
        } else {
            self.wrong
                .get_or_insert(format!("samples {new:?} at a present {want:?} ms behind"));
        }
    }
}

/// `pair_wait` is how far the pair shown trails the clock.
#[test]
fn pair_wait_is_the_pairs_lag() {
    let _time = timed("pair_wait_is_the_pairs_lag");
    // Lens 1 at four frames of the lagging run's clock a frame, and
    // fast at its second open, both lenses decoding two frames ahead.
    let backend = PerStream::pair(
        Lens::default().frames(12),
        Lens::default()
            .frames(12)
            .cost(Duration::from_millis(12))
            .cost_at_open(1, Duration::from_millis(2)),
    );
    let (player, clock) = player(backend, TWO_FRAMES);
    let perf = player.perf().clone();
    // The check: the most a pair trailed the clock, over a frame and a
    // half.
    let frame_ms = RATE.frame_duration().to_secs_f64() * 1000.0;
    let lags = |max: f32| above(f64::from(max), 1.5 * frame_ms);

    // The clock a frame every 3 ms: the pair trails.
    open(&player, Some(LockSpec::default()));
    let mut waits = Waits::from(&perf);
    let pace = Pace {
        rate: RATE,
        period: Duration::from_millis(3),
        frames: 12,
    };
    pacing::play(&player, &clock, pace, [0, 1], WAIT, |p, _| {
        waits.after(&perf, p)
    });
    assert_eq!(waits.wrong, None);
    let lagged = lags(waits.max);
    assert_eq!(lagged.passes(), Some(true), "pair_wait: {lagged}");

    // Control: a pair the clock never leads by more than a frame.
    open(&player, Some(LockSpec::default()));
    let mut waits = Waits::from(&perf);
    play_to(&player, &clock, [2, 10], 1, true, |p| {
        waits.after(&perf, Some(p))
    });
    assert_eq!(waits.wrong, None);
    must_fail("a pair that keeps up", lags(waits.max));
    common::join(player);
}

/// The jumps of two frames [`drops_count_once_for_a_pair`] makes.
const JUMPS: u64 = 3;

/// A pair passed over counts as one drop, not one a lens.
#[test]
fn drops_count_once_for_a_pair() {
    let _time = timed("drops_count_once_for_a_pair");
    let (player, clock) = player(lenses(30, [Duration::ZERO; 2]), Duration::from_secs(1));
    // The frames counted dropped over [`JUMPS`] jumps of the clock two
    // frames on, each once both lenses' frames there are up in slots.
    let run = |lock: Option<LockSpec>| {
        open(&player, lock);
        player.play();
        until(WAIT, || {
            player.present(Instant::now());
            clock.is_running().then_some(())
        })
        .expect("play never began");
        let epoch = snapshot(&player).epoch;
        let before = player.perf().count(Counter::Dropped);
        for jump in 1..=JUMPS as i64 {
            let to = at(1 + 2 * jump);
            until(WAIT, || {
                let views = player.slots().views();
                let up = |s: usize| {
                    views.iter().any(|v| {
                        v.state == SlotState::Ready
                            && v.frame.epoch == epoch
                            && v.frame.stream == s
                            && v.frame.time == to
                    })
                };
                (up(0) && up(1)).then_some(())
            })
            .expect("the frames two on never went up");
            clock.set(to, Instant::now());
            until(WAIT, || {
                player
                    .present(Instant::now())
                    .filter(|p| times(p) == [to, to])
            })
            .expect("the frames two on never showed");
        }
        player.pause();
        player.perf().count(Counter::Dropped) - before
    };
    // The check: one frame passed over a jump.
    let once_a_pair = |dropped: u64| {
        if dropped == JUMPS {
            Ok(dropped)
        } else {
            Err(dropped)
        }
    };
    once_a_pair(run(Some(LockSpec::default()))).unwrap_or_else(|d| panic!("{d} dropped"));

    // Control: unlocked, each lens counts its own.
    let dropped = must_fail("the lenses unlocked", once_a_pair(run(None)));
    assert_eq!(dropped, 2 * JUMPS);
    common::join(player);
}

/// The pacing rule fails a run that freezes: the check that
/// `a_locked_pair_paces_to_its_slowest_lens` applies, held to a run in
/// which the pair stops coming.
#[test]
fn the_pacing_rule_fails_a_frozen_run() {
    let _time = timed("the_pacing_rule_fails_a_frozen_run");
    const FRAMES: i64 = 33;
    // Lens 1 decodes for nothing at its first open, and at 2.5 frames of
    // the frozen run's clock a frame at its second.
    let lens = Lens::default().frames(FRAMES as u64);
    let backend = PerStream::pair(
        lens.clone(),
        lens.cost_at_open(1, Duration::from_micros(7500)),
    );
    let (player, clock) = player(backend, Duration::from_secs(1));
    let run = |lock: LockSpec, period: Duration| {
        open(&player, Some(lock));
        let pace = Pace {
            rate: RATE,
            period,
            frames: FRAMES,
        };
        pacing::play(&player, &clock, pace, [0, 1], WAIT, |_, _| {})
    };

    // A steady run keeps to the rule.
    let steady = run(LockSpec::default(), Duration::from_millis(3));
    pacing::pacing(&steady, RATE).unwrap_or_else(|e| panic!("a steady run: {e:?}"));

    // Control: lens 1 behind the clock, and the group base off, freezes
    // the pair while lens 1 hands out frames.
    let frozen = LockSpec {
        test: LockTest {
            no_group_base: true,
            ..LockTest::default()
        },
        ..LockSpec::default()
    };
    let samples = run(frozen, Duration::from_millis(3));
    let e = must_fail("a frozen run", pacing::pacing(&samples, RATE));
    assert!(matches!(e, Unpaced::Trailed { .. }), "{e:?}");
    common::join(player);
}
