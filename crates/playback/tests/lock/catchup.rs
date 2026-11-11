//! The group's catch-up (M3 plan section 9, D67): a pair whose lenses
//! both trail the clock catches up to it together, as each stream of an
//! unlocked open catches up on its own.

use std::path::PathBuf;
use std::time::Instant;

use spherecut_core::Clock;
use spherecut_playback::{LockSpec, LockStatus, LockTest};
use spherecut_testkit::control::{at_most, must_fail};

use crate::common::per_stream::{Lens, PerStream};
use crate::common::{self, pts, snapshot, until};
use crate::held::{Held, LENSES, at, clock, config};
use crate::{WAIT, timed};

/// Clock ticks of a run, a frame each.
const TICKS: i64 = 45;

/// How far the newest frame both lenses handed out trailed the clock, by
/// frames, at each tick of a run with the lenses locked by `test` or,
/// with `None`, unlocked. The lenses hand out 6 frames ahead, then two
/// frames for every three ticks of the clock, lens 0's first and lens
/// 1's after it, each once the cache's playhead is the clock's time; the
/// frames a catch-up passes over cost nothing. So the lenses fall behind
/// the clock together, by a third of a frame a tick, and only a catch-up
/// brings them back.
fn lags(test: Option<LockTest>) -> Vec<i64> {
    let lens = || Lens::default().frames(300);
    let backend = Held::new(PerStream::pair(lens(), lens()), [6, 6]);
    let clock = clock();
    let player = common::player(backend.clone(), clock.clone(), config());
    let path = PathBuf::from(LENSES);
    match test {
        Some(test) => player.open_locked(
            path,
            [0, 1],
            LockSpec {
                test,
                ..LockSpec::default()
            },
        ),
        None => player.open_streams(path, vec![Some(0), Some(1)]),
    }
    let ready = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (pts(&p) == [0, 0] && backend.holding(0) == Some(7) && backend.holding(1) == Some(7))
            .then_some(())
    });
    assert!(
        ready.is_some(),
        "the lenses never handed out their 6 frames"
    );
    let status = snapshot(&player).lock.map(|l| l.status);
    assert_eq!(status, test.map(|_| LockStatus::Locked));
    player.play();
    let mut lags = Vec::new();
    for tick in 1..=TICKS {
        clock.set(at(tick), Instant::now());
        let moved = until(WAIT, || {
            let _ = player.present(Instant::now());
            (player.cache().stats().playhead == at(tick)).then_some(())
        });
        assert!(moved.is_some(), "the playhead never reached {tick}");
        assert!(clock.is_running());
        if tick % 3 != 0 {
            for lens in 0..2 {
                backend.step(lens, WAIT);
            }
        }
        let newest = (0..2)
            .map(|lens| backend.released(lens).expect("a frame handed out"))
            .min()
            .unwrap_or(0);
        lags.push(tick - newest);
    }
    common::join(player);
    lags
}

/// A locked pair whose lenses both trail the clock, by a third of a frame
/// a tick, catches up to it as each stream of an unlocked open does on
/// its own (V7): once every member is more than 2 frames behind, the
/// group's `until` rises to the clock's time, so that it never trails
/// further than the unlocked streams do. Its control, every rise capped
/// at the newest frame the other lens holds, never catches up: the lag
/// grows by a frame every three ticks.
#[test]
fn a_pair_trailing_the_clock_together_catches_up() {
    let _time = timed("a_pair_trailing_the_clock_together_catches_up");
    let most = |lags: &[i64]| lags.iter().copied().max().unwrap_or(0);
    let unlocked = lags(None);
    let locked = lags(Some(LockTest::default()));
    eprintln!("lag by tick, unlocked {unlocked:?}; locked {locked:?}");
    // Unlocked, each stream catches up once it is more than 2 frames
    // behind: never 4 behind.
    assert_eq!(at_most(most(&unlocked), 3).passes(), Some(true));
    assert_eq!(
        at_most(most(&locked), most(&unlocked)).passes(),
        Some(true),
        "locked {locked:?}, unlocked {unlocked:?}"
    );
    let capped = lags(Some(LockTest {
        old_until: true,
        ..LockTest::default()
    }));
    eprintln!("capped {capped:?}");
    let lag = must_fail("the until capped", at_most(most(&capped), most(&unlocked)));
    assert!(
        lag >= 8,
        "the capped pair trailed by {lag} at most: {capped:?}"
    );
}
