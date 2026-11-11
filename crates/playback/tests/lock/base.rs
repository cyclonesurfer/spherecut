//! The group base (M3 plan section 9): the cache runs from the newest
//! time both lenses hold, one `until` serves the group, seeks land both
//! lenses on one frame, the pair ends at its shorter lens, and the
//! `pair_resync` backstop holds when a shrinking budget takes the frame
//! the trailing lens was catching up to.
//!
//! The controls that wait for what never comes (a deadlock) wait until
//! the player is stuck ([`Wait::StuckFor`] [`STALL`]) instead of for all
//! of `WAIT`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use spherecut_core::{Clock, FrameRate, ManualClock, Rounding, StopToken, Time};
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioRequest, DecodePath, DecodeStats, DecoderEvent, MediaBackend, NextFrame,
    SeekMode, SeekOutcome, VideoDecode, VideoRequest, VideoStreamInfo,
};
use spherecut_media::mp4::TrackTiming;
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_playback::{
    CacheBudget, Counter, LockSpec, LockStatus, LockTest, PlayState, Player, PlayerConfig,
    PlayerEvent,
};
use spherecut_testkit::control::must_fail;

use crate::common::per_stream::{Lens, PerStream};
use crate::common::{self, pts, snapshot, until};
use crate::{GaveUp, WAIT, Wait, timed, wait_for};

const RATE: FrameRate = FrameRate::FPS_30;

/// What `PerStream` opens: any name will do.
const LENSES: &str = "lenses.synthetic";

/// The bytes of a frame of the default clip: 64x32, 4:2:0, 8-bit.
const FRAME: u64 = 64 * 32 * 3 / 2;

fn at(k: i64) -> Time {
    RATE.frame_to_time(k).unwrap()
}

/// The frame shown at clip time `t`.
fn frame_of(t: Time) -> i64 {
    RATE.time_to_frame(t, Rounding::Floor).unwrap()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// How long a control sees nothing move before it calls the player
/// stuck, in place of `STUCK`: each control's freeze is for good, the
/// old base the playhead or no resync to move it, and the clock is paused
/// or set once, so a shorter wait loses nothing, as with `play.rs`'s
/// controls. The resync check waits on the cache's own count, which
/// moves at once, not on the perf counter the upload thread folds in
/// every 250 ms: a window this short would call a resync not yet folded
/// in stuck.
const STALL: Duration = Duration::from_millis(500);

/// A paused `ManualClock`.
fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(RATE.frame_duration()))
}

/// The config of a player holding `budget` bytes of frames, on six GPU
/// slots.
fn config(budget: u64) -> PlayerConfig {
    PlayerConfig {
        cache: CacheBudget::Bytes(budget),
        gpu_slots: 6,
        ..PlayerConfig::default()
    }
}

/// Open streams 0 and 1 of the player's backend, locked by `spec` or,
/// with `None`, unlocked as `open_streams` opens them, and wait until
/// both show their first frames: locked, the lock has formed by then.
fn open(player: &Player, spec: Option<LockSpec>) {
    let path = PathBuf::from(LENSES);
    match spec {
        Some(spec) => player.open_locked(path, [0, 1], spec),
        None => player.open_streams(path, vec![Some(0), Some(1)]),
    }
    let shown = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (p.frames.len() == 2).then(|| pts(&p))
    });
    assert_eq!(shown, Some(vec![0, 0]), "the pair never showed");
    let status = snapshot(player).lock.map(|l| l.status);
    assert_eq!(status, spec.map(|_| LockStatus::Locked));
}

/// A spec with `test`'s switches.
fn spec(test: LockTest) -> Option<LockSpec> {
    Some(LockSpec {
        test,
        ..LockSpec::default()
    })
}

/// [`PerStream`], whose lenses hand each frame they decode on out only
/// for a permit, so that a test knows which frame each lens holds, and
/// when it goes in. Seeks pass.
#[derive(Debug, Clone)]
struct Permits {
    inner: PerStream,
    /// Frames each lens may still hand out: `u64::MAX` for any.
    left: Arc<[AtomicU64; 2]>,
    /// The frame each lens holds for a permit, -1 for none.
    holding: Arc<[AtomicI64; 2]>,
}

impl Permits {
    /// `inner`, its lenses let hand out `left` frames to begin with.
    fn new(inner: PerStream, left: [u64; 2]) -> Self {
        Permits {
            inner,
            left: Arc::new(left.map(AtomicU64::new)),
            holding: Arc::new([AtomicI64::new(-1), AtomicI64::new(-1)]),
        }
    }

    /// Let lens `stream` hand out `n` more frames (`u64::MAX` for any).
    fn permit(&self, stream: usize, n: u64) {
        self.left[stream].store(n, Ordering::Release);
    }

    /// The frame lens `stream` holds for a permit.
    fn holding(&self, stream: usize) -> Option<i64> {
        Some(self.holding[stream].load(Ordering::Acquire)).filter(|&f| f >= 0)
    }
}

impl MediaBackend for Permits {
    fn name(&self) -> &'static str {
        "permits"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.inner.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let inner = self.inner.open_video(path, req)?;
        Ok(Box::new(Held {
            stream: inner.info().index,
            inner,
            permits: self.clone(),
        }))
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }

    fn track_timing(&self, path: &Path, stream: usize) -> Result<Option<TrackTiming>, MediaError> {
        self.inner.track_timing(path, stream)
    }
}

/// A lens's decoder under [`Permits`].
#[derive(Debug)]
struct Held {
    stream: usize,
    inner: Box<dyn VideoDecode>,
    permits: Permits,
}

impl VideoDecode for Held {
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
        self.inner.seek(at, mode, stop)
    }

    /// The next frame, once a permit lets it out; stopped meanwhile, the
    /// frame is lost, as at shutdown.
    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        let got = self.inner.next_frame(stop)?;
        if let NextFrame::Frame(f) = &got {
            spherecut_core::may_block("a frame held for its permit");
            let (left, holding) = (
                &self.permits.left[self.stream],
                &self.permits.holding[self.stream],
            );
            holding.store(f.pts, Ordering::Release);
            loop {
                let n = left.load(Ordering::Acquire);
                if n == u64::MAX
                    || (n > 0
                        && left
                            .compare_exchange(n, n - 1, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok())
                {
                    break;
                }
                if stop.is_stopped() {
                    return Ok(NextFrame::Stopped);
                }
                std::thread::sleep(ms(1));
            }
            holding.store(-1, Ordering::Release);
        }
        Ok(got)
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

/// A seek forward while lens 0 leads, to a time past every frame either
/// lens holds and more than 1 s past the pair on screen, lands both
/// lenses within `WAIT`: the base is the clock's time until both land.
/// Its control, the old common time kept as the base, deadlocks.
#[test]
fn a_seek_forward_while_lens_0_leads_lands_both() {
    let _time = timed("a_seek_forward_while_lens_0_leads_lands_both");
    // Lens 1 hands out frames 1 to 10 only. Paused at 0, lens 0 fills its
    // window (1 s, 30 frames) and waits with frame 31 while lens 1 holds
    // 11 for a permit; then the seek, to frame 100, which stops lens 1
    // waiting, and lens 1 is let go. Each lens's newest frame at the
    // seek, and whether both showed frame 100.
    let seek = |test: LockTest, wait: Wait| {
        let backend = Permits::new(
            PerStream::pair(Lens::default().frames(150), Lens::default().frames(150)),
            [u64::MAX, 10],
        );
        let player = common::player(backend.clone(), clock(), config(64 << 20));
        open(&player, spec(test));
        let newest = until(WAIT, || {
            let n: Vec<_> = snapshot(&player)
                .streams
                .iter()
                .map(|s| s.newest.map(frame_of))
                .collect();
            (n[0] == Some(31) && backend.holding(1) == Some(11)).then_some(n)
        })
        .expect("lens 0 never filled its window");
        player.seek(at(100), SeekMode::Exact);
        backend.permit(1, u64::MAX);
        let landed = wait_for(&player, wait, || {
            let p = player.present(Instant::now())?;
            (pts(&p) == [100, 100]).then_some(())
        });
        common::join(player);
        (newest, landed)
    };
    let (newest, landed) = seek(LockTest::default(), Wait::Full);
    assert_eq!(newest, [Some(31), Some(10)]);
    landed.unwrap();

    // Control: the old common time as the base, lens 1's newest frame
    // there, lens 0's landing lies past its window, and it waits there
    // while lens 1 waits with the frame after its own.
    let old = LockTest {
        old_base_in_seeks: true,
        ..LockTest::default()
    };
    let (newest, landed) = seek(old, Wait::StuckFor(STALL));
    assert_eq!(newest, [Some(31), Some(10)]);
    assert_eq!(must_fail("the old base in seeks", landed), GaveUp::Stuck);
}

/// The group's `until` is raised to the clock's time by a member more
/// than 2 frames behind it, but, while the other lens is within 2 frames
/// of the clock, never past the newest frame that lens holds, so the
/// trailing lens's next frame always pairs.
#[test]
fn the_group_until_stays_within_the_other_lens() {
    let _time = timed("the_group_until_stays_within_the_other_lens");
    // Paused at the pair on frame 0, lens 0 hands out frames 1 to 31,
    // all it may, and the window (1 s, 30 frames) holds 1 to 30, while
    // lens 1 holds its frame 1 for a permit. Played with the clock at
    // frame 33, 2 frames past lens 0's newest, and lens 1's frame 1 let
    // go, lens 1 is far behind the clock and catches up. The frame lens 1
    // caught up to, the group's `until` then, the frames lens 0 held, and
    // the one it holds for a permit.
    let catch_up = |test: LockTest| {
        let backend = Permits::new(
            PerStream::pair(Lens::default().frames(90), Lens::default().frames(90)),
            [31, 0],
        );
        let clock = clock();
        let player = common::player(backend.clone(), clock.clone(), config(64 << 20));
        open(&player, spec(test));
        let filled = until(WAIT, || {
            let newest = snapshot(&player).streams[0].newest;
            (newest == Some(at(31)) && backend.holding(1) == Some(1)).then_some(())
        });
        assert!(filled.is_some(), "lens 0 never filled its window");
        player.play();
        clock.set(at(33), Instant::now());
        // Until lens 0 goes no further: it holds a frame for a permit, or
        // waits for room behind a group base at 0.
        let still = until(WAIT, || {
            let _ = player.present(Instant::now());
            let c = player.cache().stats();
            let stuck = c.waiting == 1 && c.base == Some(Time::ZERO);
            (c.playhead == at(33) && (backend.holding(0).is_some() || stuck)).then_some(())
        });
        assert!(still.is_some(), "lens 0 never settled at frame 33");
        backend.permit(1, 1);
        let caught =
            until(WAIT, || backend.holding(1).filter(|&f| f > 1)).expect("lens 1 never caught up");
        let next = until(WAIT, || backend.holding(0));
        let until = snapshot(&player).lock.and_then(|l| l.until);
        let held: BTreeSet<i64> = (player.cache().held().iter())
            .filter(|(s, _)| *s == 0)
            .map(|(_, f)| f.pts)
            .collect();
        common::join(player);
        (caught, until.map(frame_of), held, next)
    };

    // Lens 1 catches up to lens 0's newest frame, which lens 0 holds.
    let (caught, until, held, next) = catch_up(LockTest::default());
    assert!((30..=31).contains(&caught), "lens 1 caught up to {caught}");
    assert_eq!(until, Some(caught));
    assert!(held.contains(&caught), "lens 0 holds {held:?}");
    assert_eq!(next, Some(32));

    // Control: no group base and no group `until`. Lens 0, 2 frames
    // behind, holds frame 32 for a permit, and lens 1 catches up to the
    // clock at 33, a frame lens 0 hasn't got to.
    let no_base = LockTest {
        no_group_base: true,
        ..LockTest::default()
    };
    let (caught, until, held, next) = catch_up(no_base);
    must_fail("no group base or until", held.contains(&caught));
    assert_eq!(
        (caught, until, held.last(), next),
        (33, None, Some(&31), Some(32))
    );
}

/// Keyframe seeks whose landings differ (tables planted to agree over
/// GOPs that don't) go exact to the later landing on both lenses, and
/// the clock takes the group's landing. The exact seek is part of the
/// one asked for, and counts no seek of its own.
#[test]
fn keyframe_landings_that_differ_go_exact_to_the_later() {
    let _time = timed("keyframe_landings_that_differ_go_exact_to_the_later");
    // Lens 1's keyframes come every 10 frames and lens 0's every 8, with
    // lens 1's tables planted as lens 0's, so that the pair locks. A
    // keyframe seek to frame 35 lands lens 0 on 32 and lens 1 on 30.
    // Where the lenses landed, what shows and the clock, by frame, once
    // the seek is shown, and the seeks counted.
    let lens0 = Lens::default().frames(90);
    let tables = lens0.derived_tables();
    let lens1 = Lens::default()
        .frames(90)
        .keyint(10)
        .plant(move |t| *t = tables);
    let seek = |test: LockTest| {
        let player = common::player(
            PerStream::pair(lens0.clone(), lens1.clone()),
            clock(),
            config(64 << 20),
        );
        open(&player, spec(test));
        player.seek(at(35), SeekMode::Keyframe);
        let s = until(WAIT, || {
            let _ = player.present(Instant::now());
            let s = snapshot(&player);
            (s.state == PlayState::Paused).then_some(s)
        })
        .expect("the seek never showed");
        common::join(player);
        let landed: Vec<_> = s
            .streams
            .iter()
            .map(|x| x.landed.map(|l| frame_of(l.time)))
            .collect();
        let shown: Vec<_> = s.shown.iter().map(|f| f.map(|f| f.pts)).collect();
        let seeks = s.perf.counter(Counter::Seeks);
        (landed, shown, frame_of(s.clock), seeks)
    };
    let both = |k| (vec![Some(k); 2], vec![Some(k); 2], k, 1);
    assert_eq!(seek(LockTest::default()), both(32));

    // Control: the landings kept as they came, each lens on its own
    // keyframe, and the clock at the earlier.
    let kept = LockTest {
        no_later_landing: true,
        ..LockTest::default()
    };
    let got = seek(kept);
    must_fail("keyframe landings kept", got == both(32));
    let apart = vec![Some(32), Some(30)];
    assert_eq!(got, (apart.clone(), apart, 30, 1));
}

/// What a player said of a lock in `events`: a refusal by its field, or
/// a lens short by its frames.
fn of_lock(events: Vec<PlayerEvent>) -> Vec<(&'static str, usize, u64)> {
    events
        .into_iter()
        .filter_map(|e| match e {
            PlayerEvent::LockRefused { field } => Some((field, 0, 0)),
            PlayerEvent::LensShort { stream, frames } => Some(("short", stream, frames)),
            _ => None,
        })
        .collect()
}

/// A locked pair with lens 1 a frame short ends at lens 1's last frame,
/// and play stops there.
#[test]
fn a_locked_pair_ends_at_its_shorter_lens() {
    let _time = timed("a_locked_pair_ends_at_its_shorter_lens");
    // Lens 1 has 29 frames to lens 0's 30. Opened, and played with the
    // clock set past both ends: the player, the frame play ended on, and
    // what the player said of the lock.
    let end = |spec: Option<LockSpec>| {
        let clock = clock();
        let player = common::player(
            PerStream::pair(Lens::default(), Lens::default().frames(29)),
            clock.clone(),
            config(64 << 20),
        );
        open(&player, spec);
        let said = of_lock(player.events());
        player.play();
        clock.set(at(40), Instant::now());
        let s = until(WAIT, || {
            let _ = player.present(Instant::now());
            Some(snapshot(&player)).filter(|s| s.at_end)
        })
        .expect("play never ended");
        (player, frame_of(s.clock), said)
    };
    let (player, ended, said) = end(Some(LockSpec::default()));
    assert_eq!((ended, said), (28, vec![("short", 1, 1)]));
    let shown = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (pts(&p) == [28, 28]).then_some(())
    });
    assert!(shown.is_some(), "the pair at frame 28 never showed");
    common::join(player);

    // Control: unlocked, play ends at stream 0's last frame.
    let (player, ended, said) = end(None);
    common::join(player);
    must_fail("unlocked", ended == 28);
    assert_eq!((ended, said), (29, vec![]));
}

/// A budget that shrinks mid-play evicts the frame the trailing lens was
/// catching up to: with both workers waiting for room and no common time
/// past the base, the group seeks both lenses to the trailing lens's
/// newest frame and counts a `pair_resync`, and the pair goes on. Its
/// control, the backstop off, stops.
#[test]
fn a_shrinking_budget_mid_play_resyncs_the_pair() {
    let _time = timed("a_shrinking_budget_mid_play_resyncs_the_pair");
    // A budget of 32 frames, so a window of 8 frames a lens (half of it,
    // shared by the two), and 280 ms ahead, 8 frames too, so that lens 0's
    // window is 8 frames before lens 1's first frame comes. Paused at the
    // pair on frame 0, lens 0 holds 1 to 8 and waits with 9, and lens 1
    // holds its frame 1 for a permit. Played with the clock at frame 20,
    // lens 1's frame 1 goes in, which moves the base to 1 and lets lens
    // 0's 9 in; lens 1, far behind the clock, catches up to the newest
    // frame lens 0 holds, and holds it. The budget falls to a quarter, 8
    // frames, a window of 2: lens 0's frame there is evicted, and lens 1
    // is let go. The frame lens 1 caught up to, and the group base once
    // it moved past 1 with a resync counted.
    let config = |frames: u64| PlayerConfig {
        ahead: ms(280),
        ..config(frames * FRAME)
    };
    let resync = |test: LockTest, wait: Wait| {
        let backend = Permits::new(
            PerStream::pair(Lens::default().frames(150), Lens::default().frames(150)),
            [u64::MAX, 0],
        );
        let clock = clock();
        let player = common::player(backend.clone(), clock.clone(), config(32));
        open(&player, spec(test));
        let filled = until(WAIT, || {
            let newest = snapshot(&player).streams[0].newest;
            (newest == Some(at(9)) && backend.holding(1) == Some(1)).then_some(())
        });
        assert!(filled.is_some(), "lens 0 never filled its window");
        player.play();
        clock.set(at(20), Instant::now());
        let given = until(WAIT, || {
            let _ = player.present(Instant::now());
            (player.cache().stats().playhead == at(20)).then_some(())
        });
        assert!(given.is_some(), "the playhead never reached the cache");
        backend.permit(1, 1);
        let caught =
            until(WAIT, || backend.holding(1).filter(|&f| f > 1)).expect("lens 1 never caught up");
        player.set_config(config(8));
        let evicted = until(WAIT, || {
            let _ = player.present(Instant::now());
            let held = player.cache().held();
            (!held.iter().any(|(s, f)| *s == 0 && f.pts == caught)).then_some(())
        });
        assert!(evicted.is_some(), "lens 0 kept frame {caught}");
        backend.permit(1, u64::MAX);
        let base = wait_for(&player, wait, || {
            let _ = player.present(Instant::now());
            let base = player.snapshot().lock?.base.filter(|&b| b > at(1))?;
            (player.cache().stats().resyncs >= 1).then_some(frame_of(base))
        });
        // The resync as the perf counters show it, once the upload thread
        // folds the cache's counts in.
        let counted = base.is_ok().then(|| {
            until(WAIT, || {
                let n = player.perf().count(Counter::PairResyncs);
                (n >= 1).then_some(n)
            })
        });
        common::join(player);
        (caught, base, counted.flatten())
    };
    let (caught, base, counted) = resync(LockTest::default(), Wait::Full);
    assert!(
        caught > 2,
        "lens 1 skipped nothing: it holds frame {caught}"
    );
    let base = base.unwrap();
    assert!(base >= 10, "the base moved only to {base}");
    assert!(
        counted.is_some(),
        "the perf counters never showed the resync"
    );

    // Control: no backstop, and both lenses wait for room for good.
    let off = LockTest {
        no_resync: true,
        ..LockTest::default()
    };
    let (_, base, _) = resync(off, Wait::StuckFor(STALL));
    assert_eq!(must_fail("no backstop", base), GaveUp::Stuck);
}
