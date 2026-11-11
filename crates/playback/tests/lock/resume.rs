//! Requests the cache answers, and landings a failure changes (M3 plan
//! section 9, D67): a pair whose decoders reached the end, or whose
//! member failed, plays on from a seek the cache answers; a member that
//! fails a seek or a step past its end lands the pair on its last frame,
//! as a shorter lens does; and an exact seek whose two landings differ,
//! both after its time, lands both lenses on the later.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spherecut_core::{Clock, ManualClock};
use spherecut_media::backend::SeekMode;
use spherecut_playback::{LockSpec, LockStatus, LockTest, PlayState, Player};
use spherecut_testkit::control::must_fail;

use crate::common::per_stream::{Lens, PerStream};
use crate::common::{self, pts, snapshot, until};
use crate::held::{Held, LENSES, at, clock, config, frame_of};
use crate::{WAIT, timed};

/// How long a control's wait sees no stream hand out a frame, with not
/// every worker waiting for room in the cache (so idle), before it calls
/// the player stuck. A check waits for all of [`WAIT`].
const STILL: Duration = Duration::from_millis(300);

/// Open streams 0 and 1 of `backend`'s lenses, locked by `spec` or, with
/// `None`, unlocked, on a player of `clock`, and wait until both show
/// their first frames.
fn opened(
    backend: impl spherecut_media::backend::MediaBackend + 'static,
    clock: &Arc<ManualClock>,
    spec: Option<LockSpec>,
    old_idle: bool,
) -> Player {
    let player = common::player(backend, clock.clone(), config());
    player.old_idle_rule(old_idle);
    let path = PathBuf::from(LENSES);
    match spec {
        Some(spec) => player.open_locked(path, [0, 1], spec),
        None => player.open_streams(path, vec![Some(0), Some(1)]),
    }
    lands(&player, &[0, 0]);
    if spec.is_some() {
        assert_eq!(
            snapshot(&player).lock.map(|l| l.status),
            Some(LockStatus::Locked)
        );
    }
    player
}

/// Present until the streams show `want` (by pts), failing after
/// [`WAIT`].
fn lands(player: &Player, want: &[i64]) {
    let got = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (pts(&p) == want).then_some(())
    });
    assert!(
        got.is_some(),
        "{want:?} never came up: showing {:?}, {:?}",
        player.present(Instant::now()).map(|p| pts(&p)),
        snapshot(player)
            .streams
            .iter()
            .map(|s| s.newest)
            .collect::<Vec<_>>()
    );
}

/// Present until every one of `n` streams shows a frame at or after
/// `goal`: what shows then. Giving up (a control's wait) once no stream
/// has handed out a frame for [`STILL`] while not every worker waits for
/// room in the cache, or after [`WAIT`], what shows then is the error.
fn reaches(player: &Player, n: usize, goal: i64, give_up: bool) -> Result<Vec<i64>, Vec<i64>> {
    let start = Instant::now();
    let mut still: Option<(Instant, u64)> = None;
    let mut seen = Vec::new();
    loop {
        if let Some(p) = player.present(Instant::now()) {
            seen = pts(&p);
            if seen.len() == n && seen.iter().all(|&f| f >= goal) {
                return Ok(seen);
            }
        }
        if start.elapsed() >= WAIT {
            return Err(seen);
        }
        let s = player.snapshot();
        if give_up && s.fresh {
            let handed: u64 = s.streams.iter().map(|x| x.handed_out).sum();
            let waiting = s.cache.map_or(0, |c| c.waiting);
            match still {
                Some((since, h)) if h == handed && waiting < n => {
                    if since.elapsed() >= STILL {
                        return Err(seen);
                    }
                }
                _ => still = Some((Instant::now(), handed)),
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The skeptic's sequence after V18: with the window from 0 full, a seek
/// to 110 played to 115, by when decoding has reached the end; paused, a
/// seek back to 0, which the cache answers for every stream; played with
/// the clock at 60. What every one of `n` streams shows by frame 55, as
/// [`reaches`] says.
pub fn play_again_after_the_end(
    player: &Player,
    clock: &ManualClock,
    n: usize,
    give_up: bool,
) -> Result<Vec<i64>, Vec<i64>> {
    let full = until(WAIT, || {
        let _ = player.present(Instant::now());
        (snapshot(player).cache?.waiting == n).then_some(())
    });
    assert!(full.is_some(), "the window from 0 never filled");
    player.seek(at(110), SeekMode::Exact);
    lands(player, &vec![110; n]);
    player.play();
    clock.set(at(115), Instant::now());
    lands(player, &vec![115; n]);
    let ended = until(WAIT, || {
        let _ = player.present(Instant::now());
        let s = snapshot(player);
        s.streams
            .iter()
            .all(|x| x.last == Some(at(119)))
            .then_some(())
    });
    assert!(ended.is_some(), "decoding never reached the end");
    player.pause();
    let cached = |p: &Player| -> Vec<u64> {
        let s = snapshot(p);
        s.streams.iter().map(|x| x.seeks.from_cache).collect()
    };
    let before = cached(player);
    player.seek(at(0), SeekMode::Exact);
    lands(player, &vec![0; n]);
    let after = cached(player);
    assert!(
        before.iter().zip(&after).all(|(b, a)| a > b),
        "the seek back to 0 wasn't answered from the cache: {before:?}, then {after:?}"
    );
    player.play();
    clock.set(at(60), Instant::now());
    reaches(player, n, 55, give_up)
}

/// A locked pair whose decoders have reached the end plays on from a seek
/// the cache answers, past the end of the run the cache holds (V18):
/// the request's place to go on from takes each worker on from the end
/// or a failure. Its control, the old idle rule, freezes at the end of
/// the run from 0.
#[test]
fn a_pair_plays_on_from_a_seek_the_cache_answers_after_the_end() {
    let _time = timed("a_pair_plays_on_from_a_seek_the_cache_answers_after_the_end");
    let run = |old_idle: bool| {
        let lens = || Lens::default().frames(120);
        let clock = clock();
        let player = opened(
            PerStream::pair(lens(), lens()),
            &clock,
            Some(LockSpec::default()),
            old_idle,
        );
        let got = play_again_after_the_end(&player, &clock, 2, old_idle);
        common::join(player);
        got
    };
    let shown = run(false).unwrap_or_else(|e| panic!("the pair froze at {e:?}"));
    assert!(shown.iter().all(|&f| f >= 55), "{shown:?}");
    let frozen = must_fail("the old idle rule", run(true));
    assert!(frozen.iter().all(|&f| f <= 31), "{frozen:?}");
}

/// The clock and the pair shown once play has ended, played from frame
/// `from` with the clock moved on a frame at a time, each frame's pair
/// waited for, as long as [`WAIT`] at most, so that the lenses never trail
/// the clock enough to catch up.
fn end_pair(player: &Player, clock: &ManualClock, from: i64) -> (i64, Vec<i64>) {
    player.play();
    for k in from + 1..120 {
        clock.set(at(k), Instant::now());
        let ended = until(WAIT, || {
            let p = player.present(Instant::now()).map(|p| pts(&p));
            if snapshot(player).at_end {
                Some(true)
            } else {
                (p == Some(vec![k, k])).then_some(false)
            }
        });
        match ended {
            Some(true) => break,
            Some(false) => {}
            None => panic!("neither the pair at {k} nor the end came"),
        }
    }
    // Its last pair comes up once uploaded.
    let s = snapshot(player);
    assert!(s.at_end, "play never ended");
    let end = frame_of(s.clock);
    let last = until(WAIT, || {
        let shown = pts(&player.present(Instant::now())?);
        (shown == [end, end]).then_some(shown)
    });
    (end, last.unwrap_or_default())
}

/// A locked member that fails a seek, whose next request the cache
/// answers, decodes on past the frames the cache holds (V11): its
/// failure no longer keeps it idle, so that the end the failure set is
/// put back once it decodes past it, and the pair plays to its last
/// frame, 59. Lens 1 hands out frames 1 to 20 for permits; its seek to
/// 20, its newest frame, which the cache can't answer (no frame after it
/// is held), fails; a seek back to 10 is answered from the cache. Its
/// control, the old idle rule, ends the pair at frame 20.
#[test]
fn a_member_that_fails_a_seek_decodes_on_from_the_cache() {
    let _time = timed("a_member_that_fails_a_seek_decodes_on_from_the_cache");
    let run = |old_idle: bool| {
        let lens = || Lens::default().frames(60);
        let backend = Held::new(PerStream::pair(lens(), lens()), [u64::MAX, 20]);
        let clock = clock();
        let player = opened(backend.clone(), &clock, Some(LockSpec::default()), old_idle);
        let held = until(WAIT, || (backend.holding(1) == Some(21)).then_some(()));
        assert!(held.is_some(), "lens 1 never held frame 21");
        backend.fail_next_seek(1);
        player.seek(at(20), SeekMode::Exact);
        let failed = until(WAIT, || {
            let _ = player.present(Instant::now());
            snapshot(&player).streams[1].error.is_some().then_some(())
        });
        assert!(failed.is_some(), "lens 1's seek never failed");
        // The clock lands at 20: lens 0 landed there, and lens 1's
        // failure set its end past it.
        assert_eq!(landed(&player).0, 20);
        backend.free(1);
        let cached = snapshot(&player).streams[1].seeks.from_cache;
        player.seek(at(10), SeekMode::Exact);
        lands(&player, &[10, 10]);
        assert!(snapshot(&player).streams[1].seeks.from_cache > cached);
        let ended = end_pair(&player, &clock, 10);
        let status = snapshot(&player).lock.map(|l| l.status);
        common::join(player);
        assert_eq!(status, Some(LockStatus::Locked));
        ended
    };
    let truth = (59, vec![59, 59]);
    assert_eq!(run(false), truth);
    let stale = must_fail("the old idle rule", is(run(true), &truth));
    assert_eq!(stale, (20, vec![20, 20]));
}

/// `got`, if it is `want`; else the error.
fn is<T: PartialEq + std::fmt::Debug>(got: T, want: &T) -> Result<(), T> {
    if got == *want { Ok(()) } else { Err(got) }
}

/// The clock and the pair shown once a request has landed: the player
/// paused, and what it shows the same at two presents.
fn landed(player: &Player) -> (i64, Vec<i64>) {
    until(WAIT, || {
        let p = player.present(Instant::now())?;
        let s = snapshot(player);
        let shown = pts(&p);
        let again = player.present(Instant::now()).map(|p| pts(&p));
        (s.state == PlayState::Paused && again.as_ref() == Some(&shown))
            .then(|| (frame_of(s.clock), shown))
    })
    .expect("the request never landed")
}

/// A seek and a step past the end a member's failure set land the pair
/// on that member's last frame, with the clock there, as they do at a
/// shorter lens's last frame (V8): lens 1's read error at frame 20 ends
/// the pair at 19, and an exact seek to 50, then a step of one frame,
/// each show [19, 19] with the clock at 19, as a lens of 20 frames does.
/// Its control, a failed member not taken as landed on its last frame,
/// leaves the clock where lens 0 landed, at 50, where no pair can show.
#[test]
fn a_seek_past_a_failed_members_end_lands_on_its_last_frame() {
    let _time = timed("a_seek_past_a_failed_members_end_lands_on_its_last_frame");
    let run = |lens1: Lens, test: LockTest| {
        let clock = clock();
        let spec = LockSpec {
            test,
            ..LockSpec::default()
        };
        let player = opened(
            PerStream::pair(Lens::default().frames(90), lens1),
            &clock,
            Some(spec),
            false,
        );
        // Lens 1's end, its read error's or its last frame's, is known.
        let ended = until(WAIT, || {
            let _ = player.present(Instant::now());
            (snapshot(&player).streams[1].last == Some(at(19))).then_some(())
        });
        assert!(ended.is_some(), "lens 1's end never came");
        player.seek(at(50), SeekMode::Exact);
        let sought = landed(&player);
        player.step(1);
        let stepped = landed(&player);
        common::join(player);
        (sought, stepped)
    };
    let short = run(Lens::default().frames(20), LockTest::default());
    assert_eq!(short, ((19, vec![19, 19]), (19, vec![19, 19])));
    let failed = Lens::default().frames(90).read_error_at(20);
    assert_eq!(run(failed.clone(), LockTest::default()), short);
    // Control: the clock stays where lens 0 landed, at 50.
    let off = LockTest {
        no_failed_landing: true,
        ..LockTest::default()
    };
    let (sought, stepped) = must_fail("no failed landing", is(run(failed, off), &short));
    assert_eq!(sought.0, 50, "{sought:?}, then {stepped:?}");
}

/// An exact seek whose two landings differ, both at or after its time
/// (each lens's damage lands it after the time asked for, lens 0 on frame
/// 52 and lens 1 on 54 for a seek to 50), seeks both lenses exactly to
/// the later, and the pair lands there with the clock on it (N9). Its
/// control, such landings kept as they came, shows them mixed.
#[test]
fn an_exact_seek_landing_both_lenses_late_lands_on_the_later() {
    let _time = timed("an_exact_seek_landing_both_lenses_late_lands_on_the_later");
    let run = |test: LockTest| {
        let backend = Held::with_gaps(
            PerStream::pair(Lens::default().frames(90), Lens::default().frames(90)),
            [Some((50, 52)), Some((50, 54))],
        );
        let clock = clock();
        let spec = LockSpec {
            test,
            ..LockSpec::default()
        };
        let player = opened(backend, &clock, Some(spec), false);
        player.seek(at(50), SeekMode::Exact);
        let got = landed(&player);
        common::join(player);
        got
    };
    let later = (54, vec![54, 54]);
    assert_eq!(run(LockTest::default()), later);
    // Control: the clock lands on the earlier, lens 0's 52.
    let kept = LockTest {
        no_later_landing: true,
        ..LockTest::default()
    };
    let (clock, shown) = must_fail("landings kept", is(run(kept), &later));
    assert_eq!(clock, 52, "{shown:?}");
}
