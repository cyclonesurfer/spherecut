//! `pair_wait` (M3 plan section 9, D67): how far the clock's time,
//! without the A/V offset, is past the end of the pair on screen,
//! recorded at each present while the clock runs and the pair is locked.

use std::path::PathBuf;
use std::time::Instant;

use spherecut_core::Clock;
use spherecut_playback::{LockSpec, LockStatus, PerfStats, Player, PlayerConfig, SlotState, Stage};
use spherecut_testkit::control::{above, at_most, must_fail};

use crate::common::per_stream::{Lens, PerStream};
use crate::common::{self, pts, snapshot, until};
use crate::held::{Held, LENSES, RATE, at, clock, config};
use crate::{WAIT, timed};

/// The `pair_wait` samples recorded since the `seen`th, and how many have
/// been recorded.
fn since(perf: &PerfStats, seen: &mut u64) -> Vec<f32> {
    let (recorded, new) = perf.samples_after(Stage::PairWait, *seen);
    *seen = recorded;
    new
}

/// Wait until stream `stream`'s frame `k` of the open is Ready in a slot,
/// without presenting.
fn ready(player: &Player, stream: usize, k: i64) {
    let epoch = snapshot(player).epoch;
    let up = until(WAIT, || {
        let views = player.slots().views();
        views
            .iter()
            .any(|v| {
                v.state == SlotState::Ready
                    && v.frame.epoch == epoch
                    && v.frame.stream == stream
                    && v.frame.pts == k
            })
            .then_some(())
    });
    assert!(up.is_some(), "stream {stream}'s frame {k} never went up");
}

/// `pair_wait` is 0 while the clock's time lies in the pair's interval
/// or before it, with an A/V offset of 40 ms showing the pair a frame
/// ahead of the clock, and how far the clock is past the pair's end once
/// a lens is late: lens 1 hands out frames 1 to 5 and holds 6, and with
/// the clock at 8 the pair at 5 is 2 frames, 66.7 ms, late. Nothing is
/// recorded while paused, nor for an unlocked open. Each check's control
/// is the other's case.
#[test]
fn pair_wait_is_how_far_the_clock_is_past_the_pair() {
    let _time = timed("pair_wait_is_how_far_the_clock_is_past_the_pair");
    let lens = || Lens::default().frames(90);
    let backend = Held::new(PerStream::pair(lens(), lens()), [u64::MAX, 5]);
    let clock = clock();
    let player = common::player(
        backend.clone(),
        clock.clone(),
        PlayerConfig {
            av_offset_ms: 40,
            ..config()
        },
    );
    let perf = player.perf().clone();
    let mut seen = 0;
    let path = PathBuf::from(LENSES);
    player.open_locked(path.clone(), [0, 1], LockSpec::default());
    let first = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (pts(&p) == [0, 0] && backend.holding(1) == Some(6)).then_some(())
    });
    assert!(
        first.is_some(),
        "the pair never showed with lens 1 holding 6"
    );
    assert_eq!(
        snapshot(&player).lock.map(|l| l.status),
        Some(LockStatus::Locked)
    );
    // Paused, nothing is recorded.
    since(&perf, &mut seen);
    for _ in 0..10 {
        let _ = player.present(Instant::now());
    }
    let paused = since(&perf, &mut seen);

    // On time: at each frame k of the clock the pair at k + 1 shows, as
    // the offset says, once both lenses' are up.
    player.play();
    for k in 1..=4 {
        clock.set(at(k), Instant::now());
        for lens in 0..2 {
            ready(&player, lens, k + 1);
        }
        let p = player.present(Instant::now()).map(|p| pts(&p));
        assert_eq!(p, Some(vec![k + 1, k + 1]));
    }
    let on_time = since(&perf, &mut seen);
    assert!(!on_time.is_empty());
    // Late: the clock at 8, past the pair at 5, which ends at 6.
    clock.set(at(8), Instant::now());
    let p = player.present(Instant::now()).map(|p| pts(&p));
    assert_eq!(p, Some(vec![5, 5]));
    let late = since(&perf, &mut seen);
    player.pause();
    for _ in 0..10 {
        let _ = player.present(Instant::now());
    }
    let paused_again = since(&perf, &mut seen);

    // Unlocked, playing: nothing is recorded.
    backend.free(1);
    player.open_streams(path, vec![Some(0), Some(1)]);
    let opened = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (pts(&p) == [0, 0]).then_some(())
    });
    assert!(opened.is_some(), "the unlocked open never showed");
    since(&perf, &mut seen);
    player.play();
    for k in 1..=4 {
        clock.set(at(k), Instant::now());
        for _ in 0..3 {
            let _ = player.present(Instant::now());
        }
    }
    assert!(clock.is_running());
    let unlocked = since(&perf, &mut seen);
    common::join(player);

    let most = |s: &[f32]| s.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let two_frames = (2.0 * RATE.frame_duration().to_secs_f64() * 1000.0) as f32;
    assert_eq!(
        at_most(most(&on_time), 0.0).passes(),
        Some(true),
        "{on_time:?}"
    );
    assert_eq!(late.len(), 1, "{late:?}");
    assert!((late[0] - two_frames).abs() < 1e-3, "{late:?}");
    assert_eq!(above(late[0], 0.0).passes(), Some(true));
    assert_eq!(
        (paused.len(), paused_again.len(), unlocked.len()),
        (0, 0, 0)
    );
    // Controls: the late pair isn't 0, the pairs on time aren't late, and
    // the presents while the locked pair played recorded.
    must_fail("the late pair on time", at_most(most(&late), 0.0));
    must_fail("the pairs on time late", above(most(&on_time), 0.0));
    must_fail("playing locked records nothing", on_time.is_empty());
}
