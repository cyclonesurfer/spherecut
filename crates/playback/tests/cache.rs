//! The frame cache's rules (M1 plan sections 8.3 and 12.1): it never
//! holds more than its budget; frames in the window ahead of the playhead
//! and the frame at it are never evicted; a decode worker waits at the
//! limit and goes on when the playhead moves; run links survive
//! evictions and are never false. A proptest checks them against a model
//! after every step of random decode runs, seeks and budget changes.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use proptest::prelude::*;
use spherecut_core::blocking::{OnBlock, mark_ui_thread_with};
use spherecut_core::video::{
    Assumed, ChromaSiting, ColorCodes, ColorFamily, ColorInfo, Matrix, PixelLayout, PlaneDesc,
    Primaries, Range, SampleBytes, Transfer,
};
use spherecut_core::{FrameRate, Rational, StopSource, StopToken, Time};
use spherecut_media::backend::VideoFrame;
use spherecut_playback::{FrameCache, Insert, MIN_FRAMES};

const RATE: FrameRate = FrameRate::FPS_30;

fn at(k: i64) -> Time {
    RATE.frame_to_time(k).unwrap()
}

/// Half-frame steps, so that a playhead can fall between frames.
fn half(k2: i64) -> Time {
    Time(Rational::new(k2, 60).unwrap())
}

fn layout() -> Arc<PixelLayout> {
    Arc::new(PixelLayout {
        name: "gray".into(),
        planes: vec![PlaneDesc {
            components: 1,
            bytes: SampleBytes::One,
            log2_sub: (0, 0),
        }],
        family: ColorFamily::Gray,
        bit_depth: 8,
        msb_aligned: false,
    })
}

fn color() -> ColorInfo {
    ColorInfo {
        codes: ColorCodes::UNSPECIFIED,
        range: Range::Full,
        matrix: Matrix::Bt709,
        transfer: Transfer::Sdr,
        primaries: Primaries::Bt709,
        siting: ChromaSiting::Center,
        assumed: Assumed::default(),
    }
}

/// Frame `k` of a 30 fps stream, `bytes` big, handed out as place `seq`
/// of run `run`.
fn frame(k: i64, run: u64, seq: u64, bytes: usize) -> Arc<VideoFrame> {
    let width = u32::try_from(bytes).unwrap();
    let mut f = VideoFrame::owned(
        layout(),
        (width, 1),
        vec![vec![0; bytes]],
        vec![bytes],
        color(),
        (k, at(k)),
    )
    .unwrap();
    assert_eq!(f.bytes(), bytes);
    f.duration = Some(RATE.frame_duration());
    f.run = run;
    f.seq = seq;
    Arc::new(f)
}

/// The frame index of a frame made by [`frame`].
fn index(f: &VideoFrame) -> i64 {
    f.pts
}

#[test]
fn frames_link_by_run_and_place() {
    let c = FrameCache::new(1 << 20, Time::from_secs(1));
    for k in 0..5 {
        assert_eq!(c.try_insert(0, frame(k, 1, k as u64, 10)), Insert::Stored);
    }
    // Frame 7 of another run: held, not linked to frame 4.
    assert_eq!(c.try_insert(0, frame(7, 2, 0, 10)), Insert::Stored);
    assert_eq!(index(&c.next_after(0, at(2)).unwrap()), 3);
    assert_eq!(index(&c.prev_before(0, at(2)).unwrap()), 1);
    assert!(c.prev_before(0, at(0)).is_none());
    assert!(c.next_after(0, at(4)).is_none());
    assert!(c.next_after(0, at(7)).is_none());
    assert!(c.next_after(0, at(9)).is_none(), "nothing held there");
    let (f, linked) = c.following(0, at(4)).unwrap();
    assert_eq!((index(&f), linked), (7, false));
    let (f, linked) = c.following(0, at(1)).unwrap();
    assert_eq!((index(&f), linked), (2, true));
    assert_eq!(index(&c.run_end(0, at(1)).unwrap()), 4);
    // The frame shown at a time between frames 2 and 3 is frame 2: its
    // successor is held and later. After frame 4 nothing says what
    // follows, and frame 7 has no successor.
    assert_eq!(index(&c.frame_at(0, half(5)).unwrap()), 2);
    assert_eq!(index(&c.frame_at(0, at(3)).unwrap()), 3);
    assert!(c.frame_at(0, half(9)).is_none());
    assert!(c.frame_at(0, at(7)).is_none());
    assert_eq!(index(&c.at_or_before(0, half(17)).unwrap()), 7);
    // Frame 5 of run 1 links to frame 4, and not to frame 7, which is
    // place 0 of run 2.
    assert_eq!(c.try_insert(0, frame(5, 1, 5, 10)), Insert::Stored);
    assert_eq!(index(&c.next_after(0, at(4)).unwrap()), 5);
    assert_eq!(index(&c.run_end(0, at(0)).unwrap()), 5);
    // Another run decoding frames 5 to 7 over them links 5 to 7 through
    // run 3, and keeps the frames already held.
    for (seq, k) in (5..8).enumerate() {
        let r = c.try_insert(0, frame(k, 3, seq as u64, 10));
        assert_eq!(
            r,
            if k == 6 {
                Insert::Stored
            } else {
                Insert::Known
            }
        );
    }
    assert_eq!(index(&c.run_end(0, at(0)).unwrap()), 7);
    assert_eq!(index(&c.prev_before(0, at(7)).unwrap()), 6);
    // Another stream's frames are its own.
    assert!(c.get(1, at(2)).is_none());
    assert_eq!(c.try_insert(1, frame(2, 1, 0, 10)), Insert::Stored);
    assert!(c.next_after(1, at(2)).is_none());
    assert_eq!(c.stats().frames, 9);
    assert_eq!(c.stats().bytes, 90);
}

#[test]
fn tied_frames_keep_the_later_and_links_pass_over_the_tie() {
    let c = FrameCache::new(1 << 20, Time::from_secs(1));
    // Frames 3 and 4 of the run have the same time (frame 3's): the
    // later one shows.
    c.try_insert(0, frame(2, 1, 0, 10));
    let first = frame(3, 1, 1, 10);
    c.try_insert(0, first.clone());
    let mut tied = VideoFrame::owned(
        layout(),
        (12, 1),
        vec![vec![1; 12]],
        vec![12],
        color(),
        (3, at(3)),
    )
    .unwrap();
    tied.run = 1;
    tied.seq = 2;
    let tied = Arc::new(tied);
    assert_eq!(c.try_insert(0, tied.clone()), Insert::Stored);
    c.try_insert(0, frame(4, 1, 3, 10));
    assert!(Arc::ptr_eq(&c.get(0, at(3)).unwrap(), &tied));
    assert_eq!(c.stats().bytes, 32);
    assert_eq!(index(&c.next_after(0, at(2)).unwrap()), 3);
    assert_eq!(index(&c.next_after(0, at(3)).unwrap()), 4);
    assert_eq!(index(&c.prev_before(0, at(4)).unwrap()), 3);
    assert_eq!(index(&c.prev_before(0, at(3)).unwrap()), 2);
    assert!(Arc::ptr_eq(&c.frame_at(0, at(3)).unwrap(), &tied));
}

#[test]
fn a_decoder_that_numbers_runs_afresh_makes_no_false_link() {
    let c = FrameCache::new(1 << 20, Time::from_secs(1));
    c.try_insert(0, frame(10, 1, 0, 10));
    c.try_insert(0, frame(11, 1, 1, 10));
    // A new decoder's run 1 starts at frame 2: its place 0 isn't frame 10.
    c.try_insert(0, frame(2, 1, 0, 10));
    c.try_insert(0, frame(3, 1, 1, 10));
    assert_eq!(index(&c.next_after(0, at(2)).unwrap()), 3);
    assert!(c.next_after(0, at(10)).is_none());
    assert!(c.prev_before(0, at(11)).is_none());
    // forget_runs drops every link of the stream and keeps its frames.
    c.forget_runs(0);
    assert!(c.next_after(0, at(2)).is_none());
    assert_eq!(c.stats().frames, 4);
}

#[test]
fn eviction_takes_the_farthest_behind_first_then_the_farthest_past_the_window() {
    // Room for 10 frames of 10 bytes; a window of 2 s holds 5 (half the
    // budget).
    let c = FrameCache::new(100, Time::from_secs(2));
    c.set_playhead(at(20));
    for k in 10..=20 {
        c.try_insert(0, frame(k, 1, k as u64, 10));
    }
    // Eleven frames: frame 10, the farthest behind, went.
    let held: Vec<i64> = c.held().iter().map(|(_, f)| index(f)).collect();
    assert_eq!(held, (11..=20).collect::<Vec<_>>());
    assert_eq!(c.stats().evicted, 1);
    // Five ahead fit the window; each evicts history.
    for k in 21..=25 {
        assert_eq!(c.try_insert(0, frame(k, 1, k as u64, 10)), Insert::Stored);
    }
    assert_eq!(c.try_insert(0, frame(26, 1, 26, 10)), Insert::Full);
    let held: Vec<i64> = c.held().iter().map(|(_, f)| index(f)).collect();
    assert_eq!(held, (16..=25).collect::<Vec<_>>());
    // A frame older than every held one makes no room for itself.
    assert_eq!(c.try_insert(0, frame(3, 2, 0, 10)), Insert::Dropped);
    // A seek back to 5: frames 16 to 25 are now ahead, past a window that
    // holds only the nearest; new frames from 5 on push the farthest out.
    c.set_playhead(at(5));
    for (seq, k) in (5..10).enumerate() {
        assert_eq!(
            c.try_insert(0, frame(k, 3, seq as u64, 10)),
            Insert::Stored,
            "{k}"
        );
    }
    let held: Vec<i64> = c.held().iter().map(|(_, f)| index(f)).collect();
    assert_eq!(held, [5, 6, 7, 8, 9, 16, 17, 18, 19, 20]);
    let s = c.stats();
    assert_eq!((s.bytes, s.ahead, s.behind), (100, 9, 1));
}

#[test]
fn the_window_is_half_the_budget_and_ahead_seconds() {
    // 1 s ahead at 30 fps is 30 frames; half of a 1000-byte budget is 50
    // frames of 10 bytes: the time limits it.
    let c = FrameCache::new(1000, Time::from_secs(1));
    let mut stored = 0;
    for k in 1..100 {
        if c.try_insert(0, frame(k, 1, k as u64, 10)) == Insert::Full {
            break;
        }
        stored += 1;
    }
    assert_eq!(stored, 30);
    assert_eq!(c.stats().ahead_span, Time::from_secs(1));
    // With a longer reach, half the budget limits it, and two streams
    // share that half.
    let c = FrameCache::new(1000, Time::from_secs(10));
    let fill = |s: usize| {
        (1..100)
            .take_while(|&k| c.try_insert(s, frame(k, 1, k as u64, 10)) != Insert::Full)
            .count()
    };
    assert_eq!(fill(0), 50);
    c.clear();
    assert_eq!(c.try_insert(1, frame(1, 1, 1, 10)), Insert::Stored);
    assert_eq!(fill(0), 25);
}

#[test]
fn a_budget_too_small_still_plays_and_says_so() {
    // Room for 2 frames of 100 bytes: under MIN_FRAMES.
    let c = FrameCache::new(250, Time::from_secs(1));
    c.set_playhead(at(4));
    assert_eq!(c.try_insert(0, frame(4, 1, 0, 100)), Insert::Stored);
    assert_eq!(c.try_insert(0, frame(5, 1, 1, 100)), Insert::Stored);
    assert_eq!(c.try_insert(0, frame(6, 1, 2, 100)), Insert::Full);
    let s = c.stats();
    assert!(s.too_small, "{s:?}");
    assert_eq!(s.frame_bytes * MIN_FRAMES, 600);
    // Half a frame: the frame shown and the next are held all the same,
    // over the budget, and playing on works.
    c.set_budget(50);
    assert_eq!(c.stats().bytes, 200);
    c.set_playhead(at(5));
    assert_eq!(c.try_insert(0, frame(6, 1, 2, 100)), Insert::Stored);
    let held: Vec<i64> = c.held().iter().map(|(_, f)| index(f)).collect();
    assert_eq!(held, [5, 6]);
    assert_eq!(c.try_insert(0, frame(7, 1, 3, 100)), Insert::Full);
    // A budget of six frames is enough.
    c.set_budget(600);
    assert!(!c.stats().too_small);
}

/// A decode worker on its own thread inserts frames 1 to 40 one after
/// another, and says after each insert which it was.
fn producer(
    c: Arc<FrameCache>,
    stop: StopToken,
) -> (mpsc::Receiver<(i64, Insert)>, std::thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || {
        for k in 1..=40 {
            let r = c.insert(0, frame(k, 1, k as u64, 10), &stop);
            if tx.send((k, r)).is_err() || r == Insert::Stopped {
                return;
            }
        }
    });
    (rx, h)
}

/// The producer's next inserts are `want`, and then it waits: nothing
/// more comes for 300 ms. Each insert may take up to 10 s on a loaded
/// machine; a producer that a load holds back past 300 ms would only make
/// the last check pass when it shouldn't, never fail when it should pass.
fn expect(rx: &mpsc::Receiver<(i64, Insert)>, want: impl IntoIterator<Item = (i64, Insert)>) {
    for w in want {
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(w));
    }
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(300)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "the producer didn't wait"
    );
}

fn stored(ks: std::ops::RangeInclusive<i64>, r: Insert) -> impl Iterator<Item = (i64, Insert)> {
    ks.map(move |k| (k, r))
}

#[test]
fn the_producer_waits_at_the_limit_and_goes_on_when_the_playhead_moves() {
    // A window of 10 frames (1/3 s at 30 fps).
    let c = Arc::new(FrameCache::new(1 << 20, Time(Rational::new(1, 3).unwrap())));
    let stops = StopSource::new();
    let (rx, h) = producer(c.clone(), stops.next());
    expect(&rx, stored(1..=10, Insert::Stored));
    assert!(c.stats().waits >= 1);
    // Moving the playhead five frames on lets five more in.
    c.set_playhead(at(5));
    expect(&rx, stored(11..=15, Insert::Stored));
    // As the UI moves it, without waiting for the lock (free here).
    assert!(c.try_set_playhead(at(29)));
    expect(&rx, stored(16..=39, Insert::Stored));
    assert!(c.try_set_playhead(at(30)));
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)),
        Ok((40, Insert::Stored))
    );
    h.join().unwrap();
}

#[test]
fn a_waiting_producer_stops_with_its_token_and_at_close() {
    let c = Arc::new(FrameCache::new(1 << 20, Time(Rational::new(1, 3).unwrap())));
    let stops = StopSource::new();
    let (rx, h) = producer(c.clone(), stops.next());
    expect(&rx, stored(1..=10, Insert::Stored));
    // A seek stops the run: the wait ends within a slice or two, without
    // a wake-up.
    let started = Instant::now();
    stops.next();
    let (k, r) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!((k, r), (11, Insert::Stopped));
    assert!(started.elapsed() < Duration::from_secs(5));
    h.join().unwrap();
    assert!(c.get(0, at(11)).is_none());
    // Closing the cache ends a wait at once, and every insert after.
    let (rx, h) = producer(c.clone(), stops.next());
    // Frames 1 to 10 are held: Known, and frame 11 waits.
    expect(&rx, stored(1..=10, Insert::Known));
    c.close();
    let (k, r) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!((k, r), (11, Insert::Stopped));
    h.join().unwrap();
    assert_eq!(c.try_insert(0, frame(1, 9, 0, 10)), Insert::Stopped);
}

#[test]
fn a_change_wakes_a_waiter() {
    let c = Arc::new(FrameCache::new(1 << 20, Time::from_secs(1)));
    let seen = c.generation();
    let c2 = c.clone();
    let h = std::thread::spawn(move || c2.wait_for_change(seen, Duration::from_secs(30)));
    std::thread::sleep(Duration::from_millis(50));
    c.try_insert(0, frame(0, 1, 0, 10));
    let started = Instant::now();
    let now = h.join().unwrap();
    assert!(now > seen);
    assert!(started.elapsed() < Duration::from_secs(10));
    // Without a change, it returns when the time is up.
    let t = Instant::now();
    assert_eq!(c.wait_for_change(now, Duration::from_millis(30)), now);
    assert!(t.elapsed() >= Duration::from_millis(30));
}

/// The negative control of M1 plan section 8.6 for the cache: on a thread
/// marked as the UI thread, every call that takes the lock panics, and
/// the `try_` calls the UI makes don't.
#[test]
fn the_ui_thread_only_tries_the_lock() {
    let c = Arc::new(FrameCache::new(1 << 20, Time::from_secs(1)));
    c.try_insert(0, frame(0, 1, 0, 10));
    let ui = |f: fn(&FrameCache)| {
        let c = c.clone();
        std::thread::spawn(move || {
            mark_ui_thread_with(OnBlock::Panic);
            f(&c);
        })
        .join()
        .map_err(|e| e.downcast_ref::<String>().cloned().unwrap_or_default())
    };
    assert_eq!(ui(|c| assert!(c.try_stats().is_some())), Ok(()));
    assert_eq!(ui(|c| assert!(c.try_set_playhead(at(1)))), Ok(()));
    let lock = "taking the frame cache's lock";
    for (f, what) in [
        (
            (|c: &FrameCache| {
                let _ = c.stats();
            }) as fn(&FrameCache),
            lock,
        ),
        (|c| c.set_playhead(at(2)), lock),
        (|c| drop(c.get(0, at(0))), lock),
        (|c| drop(c.held()), lock),
        (
            |c| {
                let _ = c.try_insert(0, frame(1, 1, 1, 10));
            },
            lock,
        ),
        (
            |c| {
                let _ = c.insert(0, frame(1, 1, 1, 10), &StopToken::never());
            },
            "waiting for room in the frame cache",
        ),
        (
            |c| {
                let _ = c.wait_for_change(0, Duration::ZERO);
            },
            "waiting for the frame cache to change",
        ),
    ] {
        let e = ui(f).unwrap_err();
        assert!(
            e.starts_with(&format!("{what} would block the UI thread")),
            "{e}"
        );
    }
    // Nothing those calls would have done was done.
    assert_eq!(c.stats().frames, 1);
    assert_eq!(c.playhead(), at(1));
}

// The model-checked property test.

const FRAMES: i64 = 24;
const STREAMS: usize = 2;
const MAX_BYTES: u64 = 64;

#[derive(Debug, Clone)]
enum Op {
    /// A decode run on stream `s`: frames `from` on, one after another,
    /// until `count` are in or the cache is full.
    Run {
        s: usize,
        from: i64,
        count: i64,
        run: u64,
    },
    /// One frame on its own.
    One {
        s: usize,
        k: i64,
        run: u64,
    },
    /// The playhead, in half frames.
    Playhead(i64),
    Budget(u64),
    Clear,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..STREAMS, 0..FRAMES, 1..12i64, 0..3u64).prop_map(|(s, from, count, run)| Op::Run { s, from, count, run }),
        3 => (0..STREAMS, 0..FRAMES, 0..3u64).prop_map(|(s, k, run)| Op::One { s, k, run }),
        4 => (0..2 * FRAMES).prop_map(Op::Playhead),
        1 => (2 * STREAMS as u64 * MAX_BYTES..2048).prop_map(Op::Budget),
        1 => Just(Op::Clear),
    ]
}

/// What the test knows: the runs each held frame was handed out in
/// while held, the streams offered frames since the last clear, and the
/// cache's settings.
#[derive(Debug)]
struct Model {
    runs: BTreeMap<(usize, i64), BTreeSet<u64>>,
    seen: BTreeSet<usize>,
    budget: u64,
    ahead: Time,
    playhead: Time,
}

type Held = BTreeMap<(usize, i64), u64>;

fn held(c: &FrameCache) -> Held {
    c.held()
        .into_iter()
        .map(|(s, f)| ((s, index(&f)), f.bytes() as u64))
        .collect()
}

impl Model {
    fn cap(&self) -> u64 {
        self.budget / 2 / self.seen.len().max(1) as u64
    }

    /// Stream `s`'s window: the frames ahead of the playhead, nearest
    /// first, while within `ahead` and half the budget, and the nearest
    /// whatever.
    fn window(&self, held: &Held, s: usize) -> BTreeSet<i64> {
        let limit = self.playhead.checked_add(self.ahead).unwrap();
        let mut out = BTreeSet::new();
        let mut bytes = 0;
        for (&(_, k), &b) in held.range((s, 0)..(s + 1, 0)) {
            if at(k) <= self.playhead {
                continue;
            }
            bytes += b;
            if !out.is_empty() && (at(k) > limit || bytes > self.cap()) {
                break;
            }
            out.insert(k);
        }
        out
    }

    fn current(&self, held: &Held, s: usize) -> Option<i64> {
        held.range((s, 0)..(s + 1, 0))
            .map(|(&(_, k), _)| k)
            .rfind(|&k| at(k) <= self.playhead)
    }

    /// Whether frame `k` of `bytes`, ahead of the playhead and not held,
    /// would be let in.
    fn admits(&self, held: &Held, s: usize, k: i64, bytes: u64) -> bool {
        let nearer: Vec<u64> = held
            .range((s, 0)..(s, k))
            .filter(|&(&(_, j), _)| at(j) > self.playhead)
            .map(|(_, &b)| b)
            .collect();
        let limit = self.playhead.checked_add(self.ahead).unwrap();
        nearer.is_empty() || (at(k) <= limit && nearer.iter().sum::<u64>() + bytes <= self.cap())
    }

    /// What is left of `held` once the rules have evicted down to the
    /// budget: the frame farthest behind its stream's current frame goes
    /// first, whichever stream it is in (the lower stream on a tie), and
    /// only when none is left the frame farthest past its stream's window
    /// (the higher stream on a tie). Current frames and windows stay.
    fn shrink(&self, held: &Held) -> Held {
        let mut left = held.clone();
        while left.values().sum::<u64>() > self.budget {
            let behind = (0..STREAMS)
                .filter_map(|s| {
                    let current = self.current(&left, s)?;
                    let (&(_, k), _) = left.range((s, 0)..(s, current)).next()?;
                    Some((k, s))
                })
                .min();
            let past = || {
                (0..STREAMS)
                    .filter_map(|s| {
                        let window = self.window(&left, s);
                        let (&(_, k), _) = left.range((s, 0)..(s + 1, 0)).next_back()?;
                        (at(k) > self.playhead && !window.contains(&k)).then_some((k, s))
                    })
                    .max()
            };
            match behind.or_else(past) {
                Some((k, s)) => left.remove(&(s, k)),
                None => break,
            };
        }
        left
    }
}

/// Every check after a step. `start` is what the cache held when it
/// began to evict: what it held before, with the frame the step offered
/// when it took it in. `inserted` is that frame, when the step offered
/// one, and whether its stream was new to the cache.
fn check(
    c: &FrameCache,
    m: &mut Model,
    (before, start, after): (&Held, &Held, &Held),
    inserted: Option<(usize, i64, bool)>,
    moved: bool,
) -> Result<(), TestCaseError> {
    let s = c.stats();
    // Bytes and frames are counted exactly, and never pass the budget.
    prop_assert_eq!(s.bytes, after.values().sum::<u64>());
    prop_assert_eq!(s.frames, after.len());
    prop_assert!(s.bytes <= s.budget, "{} > {}", s.bytes, s.budget);
    // The frames evicted are the ones the rules pick, in their order, and
    // no more than the budget needs.
    let kept = m.shrink(start);
    prop_assert_eq!(
        &kept,
        after,
        "the rules keep {:?} of {:?}",
        kept.keys().collect::<Vec<_>>(),
        start.keys().collect::<Vec<_>>()
    );
    // A frame in the window, or the one at the playhead, stays unless the
    // playhead, the budget or the streams moved, or a frame nearer the
    // playhead came in.
    if !moved {
        for st in 0..STREAMS {
            let mut kept: Vec<i64> = m.window(before, st).into_iter().collect();
            kept.extend(m.current(before, st));
            for k in kept {
                let excused = match inserted {
                    // A new stream halves each stream's window; a frame
                    // nearer the playhead pushes the farthest out of it,
                    // and a newer one at or before the playhead takes over
                    // as the frame shown.
                    Some((is, ik, new_stream)) => {
                        new_stream
                            || (is == st && at(ik) > m.playhead && ik < k)
                            || (is == st && at(ik) <= m.playhead && ik > k)
                    }
                    None => false,
                };
                prop_assert!(
                    after.contains_key(&(st, k)) || excused,
                    "frame {} of stream {} evicted from the window or the playhead",
                    k,
                    st
                );
            }
        }
    }
    // Runs: forget the runs of frames no longer held.
    m.runs.retain(|key, _| after.contains_key(key));
    // Links are exactly the shared runs of adjacent frames held.
    for &(st, k) in after.keys() {
        let shared = |j: i64| {
            let (a, b) = (m.runs.get(&(st, k)), m.runs.get(&(st, j)));
            a.zip(b).is_some_and(|(a, b)| !a.is_disjoint(b))
        };
        let next = c.next_after(st, at(k)).map(|f| index(&f));
        prop_assert_eq!(
            next,
            shared(k + 1).then_some(k + 1),
            "next of {} in {}",
            k,
            st
        );
        let prev = c.prev_before(st, at(k)).map(|f| index(&f));
        prop_assert_eq!(
            prev,
            shared(k - 1).then_some(k - 1),
            "prev of {} in {}",
            k,
            st
        );
        // Shown at a time inside frame k's interval: frame k, when the
        // frame after it is linked.
        let inside = half(2 * k + 1);
        let shown = c.frame_at(st, inside).map(|f| index(&f));
        prop_assert_eq!(shown, shared(k + 1).then_some(k));
    }
    Ok(())
}

/// Offer frame `k` of stream `s` in run `run`, and check what came of it.
fn offer(
    c: &FrameCache,
    m: &mut Model,
    sizes: &[u64],
    (s, k, run): (usize, i64, u64),
) -> Result<Insert, TestCaseError> {
    let before = held(c);
    let bytes = sizes[s * FRAMES as usize + k as usize];
    let new_stream = m.seen.insert(s);
    let was_held = before.contains_key(&(s, k));
    let ahead = at(k) > m.playhead;
    let admitted = was_held || !ahead || m.admits(&before, s, k, bytes);
    // Places follow the frame index, so that a run's places that follow
    // on are frames that follow on.
    let r = c.try_insert(s, frame(k, run, k as u64 + 100, bytes as usize));
    let after = held(c);
    match r {
        Insert::Full => {
            prop_assert!(!admitted, "frame {} turned away though it fits", k);
            prop_assert_eq!(&before, &after);
        }
        Insert::Stored | Insert::Known => {
            prop_assert!(admitted, "frame {} let in past the window", k);
            prop_assert!(after.contains_key(&(s, k)));
            prop_assert_eq!(r == Insert::Known, was_held);
            m.runs.entry((s, k)).or_default().insert(run);
        }
        Insert::Dropped => {
            prop_assert!(!ahead && !was_held, "frame {} dropped", k);
            prop_assert!(!after.contains_key(&(s, k)));
        }
        Insert::Stopped => prop_assert!(false, "stopped"),
    }
    let mut start = before.clone();
    if r != Insert::Full {
        start.insert((s, k), bytes);
    }
    check(
        c,
        m,
        (&before, &start, &after),
        Some((s, k, new_stream)),
        false,
    )?;
    Ok(r)
}

proptest! {
    #[test]
    fn the_cache_keeps_its_rules(
        budget in 2 * STREAMS as u64 * MAX_BYTES..2048,
        ahead_frames in 0..12i64,
        sizes in proptest::collection::vec(1..=MAX_BYTES, FRAMES as usize * STREAMS),
        ops in proptest::collection::vec(op(), 1..40),
    ) {
        let ahead = at(ahead_frames);
        let c = FrameCache::new(budget, ahead);
        let mut m = Model {
            runs: BTreeMap::new(),
            seen: BTreeSet::new(),
            budget,
            ahead,
            playhead: Time::ZERO,
        };
        for op in ops {
            match op {
                Op::Run { s, from, count, run } => {
                    for k in from..(from + count).min(FRAMES) {
                        if offer(&c, &mut m, &sizes, (s, k, run))? == Insert::Full {
                            break;
                        }
                    }
                }
                Op::One { s, k, run } => {
                    offer(&c, &mut m, &sizes, (s, k, run))?;
                }
                Op::Playhead(k2) => {
                    let before = held(&c);
                    m.playhead = half(k2);
                    c.set_playhead(m.playhead);
                    // Moving the playhead evicts nothing.
                    prop_assert_eq!(&before, &held(&c));
                    check(&c, &mut m, (&before, &before, &before), None, true)?;
                }
                Op::Budget(b) => {
                    let before = held(&c);
                    m.budget = b;
                    c.set_budget(b);
                    check(&c, &mut m, (&before, &before, &held(&c)), None, true)?;
                }
                Op::Clear => {
                    c.clear();
                    m.seen.clear();
                    let after = held(&c);
                    prop_assert!(after.is_empty());
                    check(&c, &mut m, (&Held::new(), &Held::new(), &after), None, true)?;
                }
            }
        }
    }
}
