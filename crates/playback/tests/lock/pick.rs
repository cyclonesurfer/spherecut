//! The present (M3 plan section 9): `pick_pair` takes the newest time
//! both lenses hold, `show_pair` shows both slots or neither, and the
//! counters say what was shown: `unpaired_presents` over every stream,
//! locked or not, `pair_wait`, and dropped frames once a pair.
//!
//! The players here are on six GPU slots, a locked pair's, and on
//! `ManualClock`s the tests move themselves, most of them by what the
//! slots hold rather than by the wall clock, so that what they check
//! doesn't hang on how fast lavapipe uploads: a frame on once both
//! lenses show the frame at the clock, or once their next frames are up.
//! Where a run needs lens 1 behind, the test holds it back ([`Held`])
//! rather than slowing its decoder, which would lag it only while the
//! test's loop outran it.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::{TestError, TestRng, TestRunner};
use spherecut_core::{Clock, FrameRate, ManualClock, Rational, Rounding, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioRequest, DecodePath, DecodeStats, DecoderEvent, MediaBackend, NextFrame,
    SeekMode, SeekOutcome, VideoDecode, VideoFrame, VideoRequest, VideoStreamInfo,
};
use spherecut_media::mp4::TrackTiming;
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_playback::lock::{pick_pair, show_pair, show_pair_over};
use spherecut_playback::{
    CacheBudget, Counter, DecoderPreference, LockSpec, LockStatus, LockTest, PREROLL_FRAMES,
    PerfStats, Player, PlayerConfig, Presented, SlotFrame, SlotState, SlotView, Stage,
};
use spherecut_testkit::control::{above, at_least, must_fail};

use crate::common::pacing::{self, Paced, Sample, Unpaced};
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
/// the open asked for; the clock's time, on frames and between them;
/// which slots' `try_show` fails; and whether each member's slot in the
/// pair, if it is Shown, is the frame the presenter has on screen.
#[derive(Debug, Clone)]
struct Case {
    views: Vec<SlotView>,
    members: [usize; 2],
    epoch: u64,
    t: Time,
    fails: Vec<bool>,
    on_screen: [bool; 2],
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
        any::<[bool; 2]>(),
    )
        .prop_map(
            |(views, first, apart, epoch, halves, fails, on_screen)| Case {
                views,
                members: [first, (first + apart) % 3],
                epoch,
                t: Time(Rational::new(halves, 60).unwrap()),
                fails,
                on_screen,
            },
        )
}

/// A `pick_pair`.
type Pick = fn(&[SlotView], [usize; 2], u64, Time) -> Option<[SlotView; 2]>;

/// How a pair's first slot is given back when the second can't go up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GiveBack {
    /// As the presenter does, through `show_pair_over`: only a slot the
    /// show moved from Ready to Shown.
    Moved,
    /// Never: a control.
    Never,
    /// Always, the frame on screen too: a control.
    Always,
}

/// The check of `pick` on `case`, and of showing what it took over the
/// frames on screen, with each slot's `try_show` failing where the case
/// says and the first slot given back as `give_back` says: never a mixed
/// pair; the members' slots in the open at the largest time at or before
/// the clock at which both have one, found by trying every time, each
/// the slot the tie rule takes; and the pair up whole, or the slots as
/// they were, none left shown alone and none on screen given back. The
/// error says what broke.
fn holds(case: &Case, pick: Pick, give_back: GiveBack) -> Result<(), String> {
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

    // The slots' states by index, as `SlotRing` keeps them: `try_show`
    // moves a Ready slot to Shown and leaves a Shown one, and `unshow`
    // moves a Shown slot back to Ready. A member's Shown slot is on
    // screen where the case says.
    let mut states = vec![SlotState::Free; SLOTS];
    for v in &case.views {
        states[v.index] = v.state;
    }
    let before = states.clone();
    let screen = [0, 1]
        .map(|k| (case.on_screen[k] && pair[k].state == SlotState::Shown).then_some(pair[k].index));
    let states = RefCell::new(states);
    let try_show = |v: &SlotView| {
        if case.fails[v.index] {
            return None;
        }
        states.borrow_mut()[v.index] = SlotState::Shown;
        Some(v.index)
    };
    let unshow = |v: &SlotView| {
        let mut states = states.borrow_mut();
        if states[v.index] == SlotState::Shown {
            states[v.index] = SlotState::Ready;
        }
    };
    let up = match give_back {
        GiveBack::Moved => show_pair_over(&pair, screen, try_show, unshow),
        GiveBack::Never => show_pair_over(&pair, screen, try_show, |_| {}),
        GiveBack::Always => {
            let mut kept = screen.into_iter();
            show_pair(
                &pair,
                |v| kept.next().flatten().or_else(|| try_show(v)),
                unshow,
            )
        }
    };
    let after = states.into_inner();
    match up {
        Some(up) => {
            let whole = up == [pair[0].index, pair[1].index]
                && (0..2).all(|k| {
                    after[up[k]] == SlotState::Shown && (screen[k].is_some() || !case.fails[up[k]])
                });
            if !whole {
                return Err(format!("{up:?} shown for {pair:?}"));
            }
        }
        None => {
            let moved = |from: SlotState, to: SlotState| {
                (0..SLOTS)
                    .filter(|&i| before[i] == from && after[i] == to)
                    .collect::<Vec<_>>()
            };
            let alone = moved(SlotState::Ready, SlotState::Shown);
            if !alone.is_empty() {
                return Err(format!(
                    "a pair that failed left slots {alone:?} shown alone"
                ));
            }
            let back = moved(SlotState::Shown, SlotState::Ready);
            if !back.is_empty() {
                return Err(format!(
                    "a pair that failed gave back slots {back:?}, Shown before it"
                ));
            }
        }
    }
    Ok(())
}

/// [`holds`] over `n` cases of [`case`], from a seeded generator when
/// `seeded`: the controls, which must fail the same way every run.
fn run(n: u32, seeded: bool, pick: Pick, give_back: GiveBack) -> Result<(), TestError<Case>> {
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
/// and always the largest common time at or before the clock; and
/// `show_pair_over`, as the presenter shows a pair, leaves no slot shown
/// alone when the second `try_show` fails, and gives back no frame on
/// screen.
#[test]
fn pick_pair_takes_the_largest_common_time() {
    let _time = timed("pick_pair_takes_the_largest_common_time");
    if let Err(e) = run(cases(), false, pick_pair, GiveBack::Moved) {
        panic!("{e}");
    }

    // Controls: each member's newest frame alone mixes the pair, a first
    // slot not given back is left shown alone, and one given back
    // whatever it was takes the frame on screen down.
    let mixed = must_fail(
        "each member's newest alone",
        run(256, true, newest_alone, GiveBack::Moved),
    );
    assert!(mixed.to_string().contains("a mixed pair"), "{mixed}");
    let alone = must_fail(
        "nothing given back",
        run(256, true, pick_pair, GiveBack::Never),
    );
    assert!(alone.to_string().contains("shown alone"), "{alone}");
    let down = must_fail(
        "every first slot given back",
        run(256, true, pick_pair, GiveBack::Always),
    );
    assert!(down.to_string().contains("gave back"), "{down}");
}

/// A player of `backend` on a paused `ManualClock`, decoding in software
/// as far as `ahead` past the playhead, on six GPU slots.
fn player(backend: impl MediaBackend + 'static, ahead: Duration) -> (Player, Arc<ManualClock>) {
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

/// How far a [`Held`] backend's lens 1 may go: the last frame it hands
/// out, by pts (a frame's index). Clones share it.
#[derive(Debug, Clone)]
struct Hold(Arc<AtomicI64>);

impl Hold {
    /// Lens 1 held at frame `frame`.
    fn at(frame: i64) -> Self {
        Hold(Arc::new(AtomicI64::new(frame)))
    }

    /// Let lens 1 hand out its frames up to `frame`, and hold those
    /// after it.
    fn release(&self, frame: i64) {
        self.0.store(frame, Ordering::Release);
    }

    /// Whether lens 1 may hand out the frame at pts `pts`.
    fn lets(&self, pts: i64) -> bool {
        pts <= self.0.load(Ordering::Acquire)
    }
}

/// [`PerStream`]'s lenses with lens 1 held back: it hands out no frame
/// past the one its [`Hold`] lets go, so that it lags lens 0 by as much
/// as a test says, whatever the machine's load. Clones share the hold.
#[derive(Debug, Clone)]
struct Held {
    lenses: PerStream,
    hold: Hold,
}

impl MediaBackend for Held {
    fn name(&self) -> &'static str {
        "held"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.lenses.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let inner = self.lenses.open_video(path, req)?;
        if req.stream != Some(1) {
            return Ok(inner);
        }
        Ok(Box::new(HeldVideo {
            inner,
            hold: self.hold.clone(),
            pending: None,
        }))
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.lenses.open_audio(path, req)
    }

    fn track_timing(&self, path: &Path, stream: usize) -> Result<Option<TrackTiming>, MediaError> {
        self.lenses.track_timing(path, stream)
    }
}

/// Lens 1 being decoded behind its hold: a frame past it waits, decoded,
/// until the hold lets it go or the decoder is told to stop; stopped, it
/// is kept for the next `next_frame`, so that none is lost, and a seek
/// drops it.
#[derive(Debug)]
struct HeldVideo {
    inner: Box<dyn VideoDecode>,
    hold: Hold,
    pending: Option<Arc<VideoFrame>>,
}

impl HeldVideo {
    /// Wait until the hold lets the frame at `pts` go (true), or `stop`
    /// says to stop (false), looking every millisecond.
    fn wait(&self, pts: i64, stop: &StopToken) -> bool {
        spherecut_core::may_block("a lens held back by its test");
        while !self.hold.lets(pts) {
            if stop.is_stopped() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }
}

impl VideoDecode for HeldVideo {
    fn info(&self) -> &VideoStreamInfo {
        self.inner.info()
    }

    fn decode_path(&self) -> &DecodePath {
        self.inner.decode_path()
    }

    fn seek(
        &mut self,
        at: Time,
        mode: SeekMode,
        stop: &StopToken,
    ) -> Result<SeekOutcome, MediaError> {
        self.pending = None;
        let got = self.inner.seek(at, mode, stop)?;
        if let SeekOutcome::Frame { frame, .. } = &got
            && !self.wait(frame.pts, stop)
        {
            return Ok(SeekOutcome::Stopped);
        }
        Ok(got)
    }

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        let frame = match self.pending.take() {
            Some(f) => f,
            None => match self.inner.next_frame(stop)? {
                NextFrame::Frame(f) => f,
                other => return Ok(other),
            },
        };
        if !self.wait(frame.pts, stop) {
            self.pending = Some(frame);
            return Ok(NextFrame::Stopped);
        }
        Ok(NextFrame::Frame(frame))
    }

    fn catch_up(&mut self, until: Option<Time>) {
        self.inner.catch_up(until);
    }

    fn take_events(&mut self) -> Vec<DecoderEvent> {
        self.inner.take_events()
    }

    fn stats(&self) -> DecodeStats {
        self.inner.stats()
    }
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

/// Whether stream `s`'s frame at `t`, of open `epoch`, is up in a slot.
fn up(player: &Player, epoch: u64, s: usize, t: Time) -> bool {
    player
        .slots()
        .views()
        .iter()
        .any(|v| v.frame.epoch == epoch && v.frame.stream == s && v.frame.time == t)
}

/// Seek to frame `from` and play from there to frame `to` a frame at a
/// time, then pause, presenting about every millisecond and calling
/// `each` with what each present showed, failing after [`WAIT`]. The
/// seek's preroll lets the slots fill before the clock runs: a player's
/// first uploads include slow ones (on lavapipe its first and its ninth
/// took 33 ms each). The clock moves on a frame once both lenses show
/// the frame at it, never on the wall clock, so that the pair can't
/// fall behind however slowly the slots fill. With `hold`, lens 1 is
/// held at the clock's frame past the preroll's frames, and the clock
/// moves on only once lens 0's next frame is up: one present comes there
/// before lens 1's is let go, so that each frame played past the preroll
/// is first a time at which lens 0's frame is up and lens 1's is not.
fn step_through(
    player: &Player,
    clock: &ManualClock,
    [from, to]: [i64; 2],
    hold: Option<&Hold>,
    mut each: impl FnMut(&Presented),
) {
    // The preroll waits for each lens's next frames.
    if let Some(hold) = hold {
        hold.release(from + PREROLL_FRAMES as i64);
    }
    let epoch = snapshot(player).epoch;
    player.seek(at(from), SeekMode::Exact);
    player.play();
    let began = Instant::now();
    let mut present = || {
        let p = player.present(Instant::now());
        if let Some(p) = &p {
            each(p);
        }
        p
    };
    loop {
        let k = frame_of(clock.now());
        let paired = present().is_some_and(|p| times(&p) == [at(k); 2]);
        if clock.is_running() && paired {
            if k >= to {
                break;
            }
            match hold {
                None => clock.set(at(k + 1), Instant::now()),
                Some(hold) if up(player, epoch, 0, at(k + 1)) => {
                    let held = !hold.lets(k + 1);
                    assert!(
                        !held || !up(player, epoch, 1, at(k + 1)),
                        "lens 1's frame {} went up through its hold",
                        k + 1
                    );
                    clock.set(at(k + 1), Instant::now());
                    if held {
                        present();
                        hold.release(k + 1);
                    }
                }
                Some(_) => {}
            }
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

/// Two frames past the playhead: the window, so that the lenses decode
/// on past the warm-up of a run's clock instead of all at once.
const TWO_FRAMES: Duration = Duration::from_millis(67);

/// Lenses of `frames` frames, decoded at once, lens 1 held at frame 2,
/// which the opens' slots take; and the hold. Lens 1 is slow by its
/// hold rather than by its decode cost, so that it is behind lens 0 at
/// every frame however busy the machine: a cost lags it only while the
/// test's loop is faster than its decoder.
fn held_lenses(frames: u64) -> (Held, Hold) {
    let hold = Hold::at(2);
    let backend = Held {
        lenses: lenses(frames, [Duration::ZERO; 2]),
        hold: hold.clone(),
    };
    (backend, hold)
}

/// A locked pair's present shows both lenses' frames or neither: at
/// every frame played, a present comes while lens 0's frame is up and
/// lens 1's is held back, and the pair on screen stays until lens 1's
/// comes. That a pair whose second `try_show` fails gives the first back
/// is `pick_pair_takes_the_largest_common_time`'s, through
/// `show_pair_over`.
#[test]
fn a_locked_pair_is_shown_all_or_none() {
    let _time = timed("a_locked_pair_is_shown_all_or_none");
    let (backend, hold) = held_lenses(40);
    let (player, clock) = player(backend, TWO_FRAMES);
    // What each present showed, played through `segments`, each from a
    // seek to a frame.
    let run = |lock: Option<LockSpec>, segments: &[[i64; 2]]| {
        hold.release(2);
        open(&player, lock);
        let mut seen = Vec::new();
        for &segment in segments {
            step_through(&player, &clock, segment, Some(&hold), |p| {
                seen.push(times(p))
            });
        }
        seen
    };
    // The seeks after the first land on frames not yet decoded.
    let seen = run(Some(LockSpec::default()), &[[2, 10], [30, 36], [18, 24]]);
    let pairs = all_or_none(&seen).unwrap_or_else(|(place, shown)| {
        panic!("present {place} of {} showed {shown:?}", seen.len())
    });
    let enough = at_least(pairs, 20);
    assert_eq!(enough.passes(), Some(true), "pairs shown: {enough}");

    // Control: unlocked, lens 0's frames show before lens 1's come.
    let unlocked = run(None, &[[2, 10]]);
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
    let (backend, hold) = held_lenses(30);
    let (player, clock) = player(backend, TWO_FRAMES);
    // The presents of a play from frame 2 to 8, lens 1 held a frame
    // behind lens 0 at each, that showed the lenses at different times,
    // as seen and as counted.
    let run = |lock: Option<LockSpec>| {
        hold.release(2);
        open(&player, lock);
        let before = player.perf().count(Counter::UnpairedPresents);
        let mut apart = 0u64;
        step_through(&player, &clock, [2, 8], Some(&hold), |p| {
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

/// `pair_wait`'s samples, read after each present: each must be how far
/// the clock's time is past the end of the pair that present showed
/// (its time and duration), never less than 0, in milliseconds.
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
            let frame = p.frames.first()?.frame;
            let end = frame
                .time
                .checked_add(frame.duration.unwrap_or(Time::ZERO))
                .ok()?;
            let d = p.time.checked_sub(end).ok()?;
            Some(d.max(Time::ZERO).to_secs_f64() * 1000.0)
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

/// The check that `pair_wait` recorded nothing since `seen` samples, the
/// count before a stretch: the samples recorded since, if any.
fn none_since(perf: &PerfStats, seen: u64) -> Result<(), u64> {
    match perf.samples_after(Stage::PairWait, seen).0 - seen {
        0 => Ok(()),
        n => Err(n),
    }
}

/// `pair_wait` is how far the clock is past the end of the pair shown,
/// recorded at each present while the clock runs and the pair is
/// locked, and at no other.
#[test]
fn pair_wait_is_the_pairs_lag() {
    let _time = timed("pair_wait_is_the_pairs_lag");
    let (backend, hold) = held_lenses(12);
    let (player, clock) = player(backend, TWO_FRAMES);
    let perf = player.perf().clone();
    // The check: the most a pair trailed the clock, over a frame and a
    // half.
    let frame_ms = RATE.frame_duration().to_secs_f64() * 1000.0;
    let lags = |max: f32| above(f64::from(max), 1.5 * frame_ms);
    let samples = || perf.samples_after(Stage::PairWait, 0).0;

    // Lens 1 held at frame 3, the last the preroll from frame 1 waits
    // for, and the clock moved on to frame 6: the pair at 3 trails it by
    // three frames, whatever the machine's load.
    open(&player, Some(LockSpec::default()));
    hold.release(1 + PREROLL_FRAMES as i64);
    let mut waits = Waits::from(&perf);
    let mut present = || {
        let p = player.present(Instant::now());
        waits.after(&perf, p.as_ref());
    };
    player.play();
    until(WAIT, || {
        present();
        clock.is_running().then_some(())
    })
    .expect("play never began");
    let running = samples();
    clock.set(at(6), Instant::now());
    for _ in 0..4 {
        present();
        std::thread::sleep(Duration::from_millis(1));
    }
    // Control of the check below: played locked, each present recorded.
    must_fail("a locked pair played", none_since(&perf, running));
    player.pause();
    assert_eq!(waits.wrong, None);
    let lagged = lags(waits.max);
    assert_eq!(lagged.passes(), Some(true), "pair_wait: {lagged}");
    // Paused, with the pair still three frames behind the clock, nothing
    // is recorded.
    let paused = samples();
    for _ in 0..4 {
        player.present(Instant::now());
        std::thread::sleep(Duration::from_millis(1));
    }
    none_since(&perf, paused).unwrap_or_else(|n| panic!("{n} recorded while paused"));

    // Control: a pair the clock never leads by more than a frame, which
    // moves on once the pair at it shows.
    hold.release(i64::MAX);
    open(&player, Some(LockSpec::default()));
    let mut waits = Waits::from(&perf);
    step_through(&player, &clock, [2, 10], None, |p| {
        waits.after(&perf, Some(p))
    });
    assert_eq!(waits.wrong, None);
    must_fail("a pair that keeps up", lags(waits.max));

    // Unlocked, the lenses played record nothing.
    open(&player, None);
    let unlocked = samples();
    step_through(&player, &clock, [2, 6], None, |_| ());
    none_since(&perf, unlocked).unwrap_or_else(|n| panic!("{n} recorded unlocked"));
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
            // Presenting meanwhile, which shows nothing new: a present
            // that found the cache's lock busy left the playhead behind
            // for the next to give, and the slots fill around it.
            until(WAIT, || {
                player.present(Instant::now());
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
    const FRAMES: i64 = 40;
    // Both lenses decode two frames ahead, so that they hand frames out
    // past the warm-up.
    let (backend, hold) = held_lenses(FRAMES as u64);
    let (player, clock) = player(backend, TWO_FRAMES);

    // A steady run keeps to the rule, the share of pairs counted over
    // frames handed out: the clock moves on once the lenses' next frames
    // are up, so that the pair keeps up however slowly the slots fill.
    hold.release(i64::MAX);
    open(&player, Some(LockSpec::default()));
    let steady = pacing::play_steady(&player, &clock, RATE, FRAMES, [0, 1], WAIT, |_, _| {});
    let paced = pacing::pacing(&steady, RATE).unwrap_or_else(|e| panic!("a steady run: {e:?}"));
    assert!(paced.handed_out > 0, "a steady run: {paced:?}");

    // Control: lens 1 held three frames behind a clock that moves on a
    // frame a present, past the window, and the group base off: the pair
    // freezes while lens 1 hands out frames, whatever the machine's load.
    let frozen = LockSpec {
        test: LockTest {
            no_group_base: true,
            ..LockTest::default()
        },
        ..LockSpec::default()
    };
    hold.release(2);
    open(&player, Some(frozen));
    hold.release(1 + PREROLL_FRAMES as i64);
    let samples = pacing::play_with(
        &player,
        [0, 1],
        WAIT,
        |_, _| {},
        || {
            let k = frame_of(clock.now()) + 1;
            if clock.is_running() && k <= FRAMES {
                clock.set(at(k), Instant::now());
                hold.release((k - 3).max(1 + PREROLL_FRAMES as i64));
            }
        },
    );
    let e = must_fail("a frozen run", pacing::pacing(&samples, RATE));
    assert!(matches!(e, Unpaced::Trailed { .. }), "{e:?}");
    common::join(player);
}

/// A run's present at frame `k` as [`pacing::pacing`] sees it, both
/// lenses alike: the pair at `shown` on screen, `handed_out` frames each
/// handed out, the newest at `newest`, and play at the end if `end`.
fn sample(k: i64, shown: Option<i64>, handed_out: u64, newest: i64, end: bool) -> Sample {
    Sample {
        clock: at(k),
        shown: shown.map(at),
        handed_out: [handed_out; 2],
        newest: [Some(at(newest)); 2],
        end,
    }
}

/// The pacing rule names the part a run fails, on runs made by hand: one
/// that keeps to it, and its controls, each failing one part and keeping
/// to the others.
#[test]
fn the_pacing_rule_names_the_part_a_run_fails() {
    let _time = timed("the_pacing_rule_names_the_part_a_run_fails");
    const END: i64 = 40;
    // A run that keeps to the rule: a present a frame, each showing the
    // pair at the clock's frame, each lens a frame on.
    let run = |shown: &dyn Fn(i64) -> Option<i64>, handed: &dyn Fn(i64) -> u64, end: bool| {
        (0..=END)
            .map(|k| {
                let last = k.min(END - 1);
                sample(k, shown(last), handed(last), last, end && k == END)
            })
            .collect::<Vec<_>>()
    };
    let paced = pacing::pacing(&run(&Some, &|k| k as u64 + 1, true), RATE);
    // The frames 30 to 39 shown after the warm-up, and handed out since
    // the present before it (29) to the end (39).
    let want = Paced {
        pairs: 10,
        handed_out: 10,
        trailed: 0,
    };
    assert_eq!(paced, Ok(want));

    // The clock never past the warm-up.
    let short = run(&Some, &|k| k as u64 + 1, true)[..30].to_vec();
    let e = must_fail("a run cut short", pacing::pacing(&short, RATE));
    assert_eq!(
        e,
        Unpaced::Short {
            clock: Some(at(29))
        }
    );
    // No pair at one present after the warm-up.
    let gap = run(&|k| (k != 35).then_some(k), &|k| k as u64 + 1, true);
    let e = must_fail("a present with no pair", pacing::pacing(&gap, RATE));
    assert!(
        matches!(
            e,
            Unpaced::Trailed {
                at: 35,
                shown: None,
                ..
            }
        ),
        "{e:?}"
    );
    // Each lens two frames on a present: too few pairs for its frames.
    let few = run(&Some, &|k| 2 * k as u64 + 1, true);
    let e = must_fail(
        "frames handed out twice as fast",
        pacing::pacing(&few, RATE),
    );
    assert_eq!(
        e,
        Unpaced::FewPairs {
            pairs: 10,
            handed_out: 20
        }
    );
    // Play never at the end.
    let endless = run(&Some, &|k| k as u64 + 1, false);
    let e = must_fail("a run that never ends", pacing::pacing(&endless, RATE));
    assert_eq!(e, Unpaced::NoEnd);
}
