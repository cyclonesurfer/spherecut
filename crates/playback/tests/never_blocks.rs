//! The UI thread never waits on the engine (M1 plan section 8.6): a fake
//! UI loop on a marked thread drives a player whose backend never
//! answers, and one whose decoder takes 50 ms a frame through a storm of
//! seeks; a player shuts down while its backend is stuck; and the guard
//! itself is live, so a wait on the marked thread would have panicked.
//!
//! **How "under 2 ms" is judged.** The plan asks that every UI iteration
//! (a snapshot, a present, the commands, the events) take under 2 ms. The
//! guard already makes any wait on a lock, a channel or a join a panic on
//! the marked thread; the times here catch what it can't, work that grows
//! with what the threads do. Wall time alone can't be judged at 2 ms on
//! this box: its 4 CPUs run the engine's threads, lavapipe, the other
//! tests and whatever else the machine runs, and with 4 busy loops beside
//! it the storm's 99th percentile came to 4 ms of wall time, every one of
//! those iterations a preemption. So each iteration's time is its wall
//! time less the time the kernel kept it waiting to run (the run queue
//! wait of `/proc/thread-self/schedstat`, which counts preemption and
//! not a wait on a lock or a sleep): the 99th percentile of that must be
//! under 2 ms, and the slowest under [`CEILING`], which work per
//! iteration or a wait on a 50 ms decode would pass. The wall times'
//! median must be under 2 ms too, which preemption doesn't move. Where
//! the kernel keeps no such statistics, wall time is judged as it is.
//! The tests that time iterations take [`TIMING`] so that they don't run
//! alongside each other.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::{FrameRate, ManualClock, SystemClock, Time};
use spherecut_media::backend::SeekMode;
use spherecut_playback::{CacheBudget, PlayState, Player, PlayerConfig, PlayerParts};
use spherecut_synth::fake::{GatedBackend, SyntheticBackend, SyntheticClip};

use common::{percentile, player, pts, snapshot, until};

/// The most a UI iteration may take, less its waits to run.
const CEILING: Duration = Duration::from_millis(10);

/// What the 99th percentile UI iteration, and the median one, must stay
/// under.
const UNDER: Duration = Duration::from_millis(2);

/// One UI frame at 60 Hz.
const FRAME: Duration = Duration::from_micros(16_667);

/// Held by the tests that time UI iterations.
static TIMING: Mutex<()> = Mutex::new(());

fn timing() -> std::sync::MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn config() -> PlayerConfig {
    PlayerConfig {
        cache: CacheBudget::Bytes(64 << 20),
        ..PlayerConfig::default()
    }
}

/// Run `ui` on a thread marked as the UI thread, with a panic on any
/// wait, and give back what it returns; fail if it takes longer than
/// `timeout` (a hang) or panics (a wait).
fn on_ui_thread<T: Send + 'static>(
    timeout: Duration,
    ui: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("fake-ui".into())
        .spawn(move || {
            mark_ui_thread_with(OnBlock::Panic);
            let out = ui();
            unmark_ui_thread();
            let _ = tx.send(out);
        })
        .unwrap();
    match rx.recv_timeout(timeout) {
        Ok(out) => {
            handle.join().unwrap();
            out
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the UI loop hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // It panicked: say why.
            match handle.join() {
                Err(e) => std::panic::resume_unwind(e),
                Ok(()) => panic!("the UI loop ended without an answer"),
            }
        }
    }
}

/// Sleep to the next 60 Hz tick after `start`.
fn tick(start: Instant, i: u32) {
    let next = start + FRAME * (i + 1);
    std::thread::sleep(next.saturating_duration_since(Instant::now()));
}

/// How long the thread that made it has waited to run, from the
/// scheduler's statistics: the second field of
/// `/proc/thread-self/schedstat`, in nanoseconds, read from a file opened
/// once on that thread. `None` where the kernel doesn't keep them.
struct RunQueue(Option<std::fs::File>);

impl RunQueue {
    /// For the calling thread.
    fn open() -> Self {
        RunQueue(std::fs::File::open("/proc/thread-self/schedstat").ok())
    }

    fn waited(&self) -> Option<Duration> {
        use std::os::unix::fs::FileExt;
        let f = self.0.as_ref()?;
        let mut buf = [0u8; 128];
        let n = f.read_at(&mut buf, 0).ok()?;
        let text = std::str::from_utf8(&buf[..n]).ok()?;
        text.split_whitespace()
            .nth(1)?
            .parse()
            .ok()
            .map(Duration::from_nanos)
    }
}

/// UI iterations' wall times, and those less their waits to run.
#[derive(Default)]
struct Times {
    wall: Vec<Duration>,
    own: Vec<Duration>,
}

impl Times {
    /// Time `f`.
    fn time<T>(&mut self, rq: &RunQueue, f: impl FnOnce() -> T) -> T {
        let w0 = rq.waited();
        let t0 = Instant::now();
        let out = f();
        let wall = t0.elapsed();
        let waited = rq
            .waited()
            .zip(w0)
            .map_or(Duration::ZERO, |(b, a)| b.saturating_sub(a));
        self.wall.push(wall);
        self.own.push(wall.saturating_sub(waited));
        out
    }
}

fn judge(what: &str, t: &Times) {
    let (p50, p99, max) = (
        percentile(&t.own, 50.0),
        percentile(&t.own, 99.0),
        percentile(&t.own, 100.0),
    );
    let wall = |p| percentile(&t.wall, p);
    eprintln!(
        "{what}: {} iterations; less waits to run: median {p50:?}, p99 {p99:?}, max {max:?}; \
         wall: median {:?}, p99 {:?}, max {:?}",
        t.own.len(),
        wall(50.0),
        wall(99.0),
        wall(100.0)
    );
    assert!(
        p99 < UNDER,
        "{what}: the 99th percentile UI iteration took {p99:?}"
    );
    assert!(max < CEILING, "{what}: a UI iteration took {max:?}");
    assert!(
        wall(50.0) < UNDER,
        "{what}: the median UI iteration took {:?}",
        wall(50.0)
    );
}

#[test]
fn the_ui_never_waits_on_a_backend_that_never_answers() {
    let _t = timing();
    let backend = GatedBackend::new(SyntheticBackend::new(SyntheticClip {
        frames: 300,
        ..SyntheticClip::default()
    }));
    let gate = backend.gate.clone();
    let player = player(backend, Arc::new(SystemClock::new()), config());
    let (player, times, states, first) = on_ui_thread(Duration::from_secs(30), move || {
        player.open("gated.synthetic".into());
        let rq = RunQueue::open();
        let mut times = Times::default();
        let mut states = Vec::new();
        let start = Instant::now();
        let mut i = 0u32;
        while start.elapsed() < Duration::from_secs(3) {
            let snap = times.time(&rq, || {
                let snap = player.snapshot();
                let _ = player.present(Instant::now());
                if i.is_multiple_of(5) {
                    player.seek(
                        FrameRate::FPS_30.frame_to_time(i64::from(i % 300)).unwrap(),
                        SeekMode::Exact,
                    );
                }
                if i.is_multiple_of(20) {
                    player.play();
                } else if i % 20 == 10 {
                    player.pause();
                }
                let _ = player.events();
                snap
            });
            states.push(snap.state);
            tick(start, i);
            i += 1;
        }
        // Released, the frame of the last seek comes up.
        gate.release();
        let released = Instant::now();
        let first = until(Duration::from_secs(10), || {
            player.present(Instant::now()).map(|_| released.elapsed())
        });
        (player, times, states, first)
    });
    judge("gated", &times);
    let other: Vec<_> = states
        .iter()
        .filter(|s| !matches!(s, PlayState::Opening | PlayState::Seeking))
        .collect();
    assert!(
        other.is_empty(),
        "states other than Opening or Seeking: {other:?}"
    );
    let first = first.expect("no frame was presented within 10 s of the release");
    eprintln!("first frame {first:?} after the release");
    assert!(
        first < Duration::from_millis(500),
        "the first frame took {first:?}"
    );
    common::join(player);
}

/// The storm's decoder's time a frame.
const FRAME_COST: Duration = Duration::from_millis(50);

#[test]
fn a_seek_storm_decodes_little_and_lands_on_the_last_seek() {
    let _t = timing();
    let rate = FrameRate::FPS_30;
    let backend = SyntheticBackend::new(SyntheticClip {
        frames: 600,
        keyint: 8,
        frame_cost: FRAME_COST,
        ..SyntheticClip::default()
    });
    let clock = Arc::new(ManualClock::new(rate.frame_duration()));
    let player = player(backend, clock, config());
    let last = 437i64;
    let (player, times, landed, gaps) = on_ui_thread(Duration::from_secs(60), move || {
        player.open("storm.synthetic".into());
        let rq = RunQueue::open();
        let mut times = Times::default();
        let mut sent = Vec::with_capacity(1001);
        sent.push(Instant::now());
        let start = Instant::now();
        // 1000 seeks in 2 s: one every 2 ms, scattered over the clip, the
        // last to frame `last`.
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        for i in 0..1000u32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let k = if i == 999 { last } else { (x % 600) as i64 };
            times.time(&rq, || {
                let _ = player.snapshot();
                let _ = player.present(Instant::now());
                sent.push(Instant::now());
                player.seek(rate.frame_to_time(k).unwrap(), SeekMode::Exact);
                let _ = player.events();
            });
            let next = start + Duration::from_millis(2) * (i + 1);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        let storm = start.elapsed();
        let landed = until(Duration::from_secs(20), || {
            let p = player.present(Instant::now())?;
            (pts(&p) == [last]).then_some(())
        });
        eprintln!("storm took {storm:?}");
        // A seek can run to its end only if nothing supersedes it for a
        // frame's decode, 50 ms: the gaps that long, which a preempted
        // UI thread makes.
        let gaps = sent
            .windows(2)
            .filter(|w| w[1] - w[0] >= FRAME_COST)
            .count() as u64;
        (player, times, landed, gaps)
    });
    judge("storm", &times);
    assert!(landed.is_some(), "the last seek's frame never came up");
    let snap = snapshot(&player);
    let seeks = snap.streams[0].seeks;
    eprintln!("{seeks:?}, {gaps} gaps of a frame's decode between seeks");
    // At most one frame decoded per superseded seek (plan 8.4): a worker
    // stops a seek within the frame it is decoding, so only the open's
    // first frame, the last seek and a seek with a gap after it run to
    // their end.
    assert!(
        seeks.decoded <= 2 + gaps,
        "{} seeks ran to completion, with {gaps} gaps",
        seeks.decoded
    );
    assert_eq!(snap.state, PlayState::Paused);
    common::join(player);
}

#[test]
fn a_player_stuck_in_its_backend_shuts_down_at_once() {
    let backend = GatedBackend::new(SyntheticBackend::default());
    let player = player(backend, Arc::new(SystemClock::new()), config());
    player.open("gated.synthetic".into());
    player.seek(Time::from_secs(0), SeekMode::Exact);
    player.play();
    // Its threads are all waiting by now: the decoder at the gate.
    std::thread::sleep(Duration::from_millis(100));
    // The plan's 2 s, not the other tests' [`common::JOIN`]: this is the
    // test of it, and stuck threads end at once.
    let start = Instant::now();
    let report = player.join(Duration::from_secs(2));
    let took = start.elapsed();
    eprintln!("{report:?}");
    assert!(report.is_clean(), "{report:?}");
    assert!(took < Duration::from_secs(2), "join took {took:?}");
    for name in ["upload", "open", "decode-v0"] {
        assert!(
            report.joined.iter().any(|j| j == name),
            "{name}: {report:?}"
        );
    }
}

#[test]
fn a_player_dropped_while_stuck_stops_its_threads() {
    let backend = Arc::new(GatedBackend::new(SyntheticBackend::default()));
    let gate = backend.gate.clone();
    // Only the player and its open and decode threads hold the backend.
    let held = Arc::downgrade(&backend);
    let parts = PlayerParts::new(backend, common::gpu().clone(), Arc::new(SystemClock::new()));
    let player = Player::new(parts, config()).unwrap();
    player.open("gated.synthetic".into());
    let cache = player.cache().clone();
    // The open is at the gate, waiting for it or for the shutdown.
    std::thread::sleep(Duration::from_millis(100));
    drop(player);
    // Dropped, it shuts down: the upload thread closes the cache as it
    // ends, and the open gives up at the gate, which is still shut, and
    // its thread ends with the decode worker's.
    let closed = until(Duration::from_secs(5), || {
        (cache.insert(0, test_frame(), &spherecut_core::StopToken::never())
            == spherecut_playback::Insert::Stopped)
            .then_some(())
    });
    assert!(closed.is_some(), "the cache was never closed");
    let ended = until(common::JOIN, || (held.strong_count() == 0).then_some(()));
    assert!(
        ended.is_some(),
        "{} holders of the backend are still running",
        held.strong_count()
    );
    assert!(!gate.is_open());
}

/// A frame of the synthetic clip, for the cache.
fn test_frame() -> Arc<spherecut_media::backend::VideoFrame> {
    use spherecut_core::StopToken;
    use spherecut_media::backend::{MediaBackend, NextFrame, VideoRequest};
    let mut dec = SyntheticBackend::default()
        .open_video("one.synthetic".as_ref(), &VideoRequest::default())
        .unwrap();
    match dec.next_frame(&StopToken::never()).unwrap() {
        NextFrame::Frame(f) => f,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_blocking_snapshot_on_the_ui_thread_panics() {
    let player = player(
        SyntheticBackend::default(),
        Arc::new(SystemClock::new()),
        config(),
    );
    let player = Arc::new(Mutex::new(Some(player)));
    let p = player.clone();
    let got = std::thread::spawn(move || {
        mark_ui_thread_with(OnBlock::Panic);
        let guard = p.lock().unwrap();
        let player: &Player = guard.as_ref().unwrap();
        // The snapshot that doesn't wait is fine on the UI thread.
        let _ = player.snapshot();
        let panics = |f: &dyn Fn()| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .err()
                .map(|e| {
                    e.downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default()
                })
        };
        let r = [
            panics(&|| drop(player.snapshot_blocking())),
            panics(&|| drop(player.slots().gpu_frame(0))),
        ];
        unmark_ui_thread();
        r
    })
    .join()
    .unwrap();
    let [snapshot, slot] = got;
    let msg = snapshot.expect("snapshot_blocking didn't panic on the UI thread");
    assert!(msg.contains("would block the UI thread"), "{msg}");
    // Naming the caller's line, which a release build logs, not the
    // player's.
    assert!(
        msg.contains("(at crates/playback/tests/never_blocks.rs:"),
        "{msg}"
    );
    let msg = slot.expect("a slot's lock didn't panic on the UI thread");
    assert!(
        msg.contains("(at crates/playback/tests/never_blocks.rs:"),
        "{msg}"
    );
    // Off the UI thread it is an ordinary snapshot.
    let player = player.lock().unwrap().take().unwrap();
    assert_eq!(player.snapshot_blocking().state, PlayState::Empty);
    common::join(player);
}
