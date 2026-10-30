//! The engine's rules (M1 plan sections 8.2 to 8.4 and 12.1), on
//! lavapipe and a `ManualClock` wherever wall time would make a test
//! flaky: the presenter shows the latest frame at or before the clock
//! with a variable frame rate; hitches are intervals over 1.5 times the
//! median; frames passed over while playing are counted as dropped; a
//! decoder that falls behind catches up without making the frames it
//! passes; a slot is reused only once nothing else holds its frame; a
//! seek keeps the frame on screen until the new one is ready and frees
//! the old run's slots; steps follow runs both ways, go on from where a
//! seek (keyframe ones included) or step on its way lands, and instant
//! ones from each other before a present; the A/V offset moves the frame only while playing; two
//! streams seek, step and play on three slots, with the second's decoder
//! behind, and seek with one failing while its next frame holds the
//! third slot; a keyframe seek back with the cache full lands on its
//! keyframe, on one stream and on two streams on three slots; a seek
//! back, as at a loop, leaves the frames held from before it out of the
//! slots and starts the clock once the next two frames are Ready, or at
//! once at the last frame; a fallback reaches the snapshot, from a fault and from `Auto` picking
//! Vulkan on lavapipe; the audio seam holds playback for its preroll;
//! and a 5.7K source plays inside a 256 MiB cache.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use spherecut_core::{Clock, FrameRate, ManualClock, Rational, StopToken, Time};
use spherecut_media::HwDevice;
use spherecut_media::MediaError;
use spherecut_media::backend::{
    AudioDecode, AudioRequest, DecodeMode, DecodePath, DecodeStats, DecoderEvent, Fallback,
    FallbackReason, Fault, FaultPlan, FfmpegBackend, HwKind, MediaBackend, NextFrame, SeekMode,
    SeekOutcome, VideoDecode, VideoRequest, VideoStreamInfo,
};
use spherecut_media::decode::open_hw_device;
use spherecut_media::doctor::CheckResult;
use spherecut_media::probe::{ProbeError, ProbeReport};
use spherecut_playback::{
    AudioOutput, AudioOutputFactory, CacheBudget, Counter, DecoderPreference, HwDecodeCheck,
    HwSupport, PREROLL_FRAMES, PlayState, Player, PlayerConfig, PlayerError, PlayerEvent,
    PlayerParts, PlayerThreads, Presented, SlotState,
};
use spherecut_render::readback::read_texture;
use spherecut_synth::fake::{SyntheticBackend, SyntheticClip, frame_index};
use spherecut_synth::media::{self, Codec, Gop, Spec, VFR_TIMESCALE, vfr_ticks};

use common::{gpu, player, pts, snapshot, until};

const RATE: FrameRate = FrameRate::FPS_30;
const WAIT: Duration = Duration::from_secs(20);

fn at(k: i64) -> Time {
    RATE.frame_to_time(k).unwrap()
}

fn config(mib: u64) -> PlayerConfig {
    PlayerConfig {
        cache: CacheBudget::Bytes(mib << 20),
        decoder: DecoderPreference::Software,
        ..PlayerConfig::default()
    }
}

fn clip(frames: u64) -> SyntheticBackend {
    SyntheticBackend::new(SyntheticClip {
        frames,
        ..SyntheticClip::default()
    })
}

/// Present until stream 0 shows the frame at `at(k)` (frame `k` of a
/// constant 30 fps), failing after [`WAIT`]; every frame shown on the
/// way must be at or before the clock. What changed on the way goes into
/// `seen`.
fn show_seen(player: &Player, k: i64, seen: &mut Vec<Time>) -> Presented {
    until(WAIT, || {
        let p = player.present(Instant::now())?;
        for f in &p.frames {
            assert!(
                f.frame.time <= p.time,
                "{:?} shown at {:?}",
                f.frame,
                p.time
            );
        }
        let t = p.frames[0].frame.time;
        if seen.last() != Some(&t) {
            seen.push(t);
        }
        (t == at(k)).then_some(p)
    })
    .unwrap_or_else(|| {
        panic!(
            "frame {k} never came up: {:?}",
            player.present(Instant::now()).map(|p| pts(&p))
        )
    })
}

fn show(player: &Player, k: i64) -> Presented {
    show_seen(player, k, &mut Vec::new())
}

/// Opened and showing its first frame.
fn opened(
    backend: impl MediaBackend + 'static,
    clock: Arc<ManualClock>,
    config: PlayerConfig,
) -> Player {
    let player = player(backend, clock, config);
    player.open("clip.synthetic".into());
    show(&player, 0);
    player
}

/// A test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("engine-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn the_presenter_shows_the_latest_frame_at_or_before_the_clock() {
    // A true VFR clip: frames last 1/30 to 1/15 s, in no short period.
    let dir = TempDir::new("vfr");
    let path = media::write(
        &Spec {
            name: "vfr".into(),
            vfr: true,
            frames: 24,
            ..Spec::default()
        },
        &dir.0,
    )
    .unwrap();
    let tick = |n: i64| Time(Rational::new(n, VFR_TIMESCALE).unwrap());
    let starts: Vec<i64> = (0..24u32)
        .scan(0, |t, i| {
            let s = *t;
            *t += vfr_ticks(i);
            Some(s)
        })
        .collect();
    let clock = Arc::new(ManualClock::new(tick(3000)));
    let player = player(FfmpegBackend::new(), clock.clone(), config(64));
    player.open(path);
    let first = show(&player, 0);
    let base = first.frames[0].frame.pts;
    player.play();
    // The clock at each frame's start, inside it, and a tick before the
    // next: always that frame.
    for k in 0..23usize {
        let (s, next) = (starts[k], starts[k + 1]);
        for t in [s, (s + next) / 2, next - 1] {
            clock.set(tick(t), Instant::now());
            let p = until(WAIT, || {
                let p = player.present(Instant::now())?;
                let f = &p.frames[0].frame;
                assert!(
                    f.time <= tick(t),
                    "frame at {:?} shown at {:?}",
                    f.time,
                    tick(t)
                );
                (f.time == tick(s)).then_some(p)
            });
            assert!(p.is_some(), "frame {k} never came up at tick {t}");
        }
    }
    assert_eq!(snapshot(&player).state, PlayState::Playing);
    common::join(player);
    let _ = base;
}

#[test]
fn frames_passed_over_while_playing_count_as_dropped() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(clip(120), clock.clone(), config(64));
    player.play();
    // Two frames a step, with the frame two on uploaded before the clock
    // moves: each step passes one over. Three frames a step after that,
    // without waiting: the frames between may come up first, and what is
    // counted must be what was passed.
    let mut seen = vec![at(0)];
    for k in (2..=40).step_by(2) {
        // Presenting meanwhile, as a GUI does every frame: a playhead the
        // cache's lock was too busy for reaches it at the next present,
        // and the upload thread picks the frames to upload around the
        // playhead (D34). The clock stays on k - 2, so no frame changes.
        until(WAIT, || {
            let _ = player.present(Instant::now());
            let views = player.slots().views();
            views.iter().any(|v| v.frame.time == at(k)).then_some(())
        })
        .unwrap_or_else(|| {
            panic!(
                "the frame two on, {k}, never came up; the cache's playhead is {:?}",
                player.cache().playhead()
            )
        });
        clock.set(at(k), Instant::now());
        show_seen(&player, k, &mut seen);
    }
    assert_eq!(seen.len(), 21, "{seen:?}");
    assert_eq!(player.perf().count(Counter::Dropped), 20);
    for k in (43..=70).step_by(3) {
        clock.set(at(k), Instant::now());
        show_seen(&player, k, &mut seen);
    }
    let frames: Vec<i64> = seen
        .iter()
        .map(|&t| {
            RATE.time_to_frame(t, spherecut_core::Rounding::Floor)
                .unwrap()
        })
        .collect();
    let passed: i64 = frames.windows(2).map(|w| w[1] - w[0] - 1).sum();
    eprintln!("shown {frames:?}: {passed} passed over");
    assert_eq!(player.perf().count(Counter::Dropped), passed as u64);
    common::join(player);
}

#[test]
fn hitches_are_intervals_over_one_and_a_half_times_the_median() {
    // A steady UI at 12.5 fps, slower than the present latency's 50 ms
    // cap, has no hitches; a frame that takes 200 ms is one.
    let player = player(
        clip(30),
        Arc::new(ManualClock::new(RATE.frame_duration())),
        config(64),
    );
    let start = Instant::now();
    let mut now = start;
    for _ in 0..40 {
        now += Duration::from_millis(80);
        let _ = player.present(now);
    }
    assert_eq!(player.perf().count(Counter::Hitches), 0);
    now += Duration::from_millis(200);
    let _ = player.present(now);
    for _ in 0..10 {
        now += Duration::from_millis(80);
        let _ = player.present(now);
    }
    assert_eq!(player.perf().count(Counter::Hitches), 1);
    common::join(player);
}

#[test]
fn frames_are_dropped_only_while_playing() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(clip(120), clock.clone(), config(64));
    // Paused, a seek or a step far on drops nothing.
    player.seek(at(40), SeekMode::Exact);
    show(&player, 40);
    player.step(5);
    show(&player, 45);
    assert_eq!(player.perf().count(Counter::Dropped), 0);
    common::join(player);
}

#[test]
fn a_keyframe_seek_shows_the_keyframe_at_its_own_time() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    // A keyframe every 8: the keyframe for frame 45 is 40, and the cache
    // holds nothing past the first window.
    let player = opened(clip(120), clock.clone(), config(64));
    player.seek(at(45), SeekMode::Keyframe);
    show(&player, 40);
    let snap = snapshot(&player);
    assert_eq!(snap.clock, at(40));
    assert_eq!(
        snap.streams[0].landed.unwrap().exactness,
        Some(spherecut_media::backend::Exactness::KeyframeOnly)
    );
    // Playing goes on from it, dropping nothing.
    player.play();
    clock.set(at(41), Instant::now());
    show(&player, 41);
    assert_eq!(player.perf().count(Counter::Dropped), 0);
    common::join(player);
}

#[test]
fn a_decoder_that_falls_behind_catches_up_without_making_the_frames_it_passes() {
    let backend = SyntheticBackend::new(SyntheticClip {
        frames: 600,
        keyint: 1000,
        frame_cost: Duration::from_millis(2),
        ..SyntheticClip::default()
    });
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(backend, clock.clone(), config(64));
    // Let it decode ahead as far as its window (1 s) while paused.
    until(WAIT, || {
        let held = player.cache().held();
        (held.len() >= 30).then_some(())
    })
    .expect("the window ahead never filled");
    player.play();
    // The clock leaps 10 s on: the decoder is 270 frames behind it.
    clock.set(at(300), Instant::now());
    show(&player, 300);
    let dropped = player.perf().count(Counter::DecodeDropped);
    let held: Vec<u64> = player
        .cache()
        .held()
        .iter()
        .map(|(_, f)| frame_index(f).unwrap())
        .collect();
    eprintln!("decode-dropped {dropped}, held {held:?}");
    // Catching up from the frame after the window's last to 300 made
    // none of the frames between.
    let last_ahead = held.iter().copied().filter(|&i| i < 300).max().unwrap();
    assert!(last_ahead < 60, "{held:?}");
    assert!(
        held.iter().all(|&i| i <= last_ahead || i >= 300),
        "frames passed over were made: {held:?}"
    );
    assert_eq!(dropped, 300 - last_ahead - 1);
    common::join(player);
}

#[test]
fn a_slot_is_reused_only_once_nothing_else_holds_its_frame() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(clip(120), clock.clone(), config(64));
    let p = show(&player, 0);
    let held = p.frames[0].gpu.clone();
    let slot = p.frames[0].slot;
    drop(p);
    let read = |gpu_frame: &spherecut_render::GpuFrame| {
        let t = read_texture(gpu(), gpu_frame.plane_texture(0).unwrap(), 0).unwrap();
        u64::from_le_bytes(t.data[..8].try_into().unwrap())
    };
    assert_eq!(read(&held), 0);
    player.play();
    // Twenty frames on, through four slots: the held one is freed, and
    // never written.
    for k in 1..=20 {
        clock.set(at(k), Instant::now());
        show(&player, k);
    }
    let state = player.slots().state(slot);
    assert_eq!(state, Some(SlotState::Free), "slot {slot}");
    assert_eq!(player.slots().holders(slot), Some(2));
    assert_eq!(read(&held), 0, "a held slot was written");
    assert!(player.perf().count(Counter::FramesUploaded) >= 20);
    // Let go, it is used again.
    drop(held);
    let mut reused = false;
    for k in 21..=40 {
        clock.set(at(k), Instant::now());
        show(&player, k);
        reused |= player.slots().state(slot) != Some(SlotState::Free);
    }
    assert!(reused, "slot {slot} was never used again");
    common::join(player);
}

#[test]
fn a_seek_keeps_the_frame_up_until_the_new_one_is_ready_and_frees_the_old_run() {
    // 30 ms a frame from a keyframe 8 back: a seek takes a while.
    let backend = SyntheticBackend::new(SyntheticClip {
        frames: 300,
        keyint: 8,
        frame_cost: Duration::from_millis(30),
        ..SyntheticClip::default()
    });
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(backend, clock.clone(), config(64));
    player.play();
    for k in 1..=3 {
        clock.set(at(k), Instant::now());
        show(&player, k);
    }
    player.seek(at(207), SeekMode::Exact);
    let start = Instant::now();
    let mut during = Vec::new();
    let landed = until(WAIT, || {
        let p = player.present(Instant::now());
        let shown = p.as_ref().map(pts);
        during.push(shown.clone());
        (shown == Some(vec![207])).then_some(start.elapsed())
    });
    let landed = landed.expect("the seek never landed");
    eprintln!("landed after {landed:?}");
    // Until then, the frame on screen stayed: no black flash.
    assert!(during.len() > 1);
    for s in &during[..during.len() - 1] {
        assert_eq!(s.as_deref(), Some(&[3][..]), "{during:?}");
    }
    // The old run's slots are freed for the new one's frames.
    let settled = until(WAIT, || {
        let _ = player.present(Instant::now());
        let views = player.slots().views();
        views.iter().all(|v| v.frame.time >= at(206)).then_some(())
    });
    assert!(settled.is_some(), "{:?}", player.slots().views());
    // Playback goes on from there.
    clock.set(at(209), Instant::now());
    show(&player, 209);
    common::join(player);
}

#[test]
fn a_step_past_the_window_ahead_lands() {
    // A window of a third of a second, 10 frames, and a decoder that takes
    // 10 ms a frame: a step of 40 goes well past the window, and the
    // worker moves the playhead as it goes, so its frames never wait for
    // room. The presenter must leave the playhead to it meanwhile, though
    // the clock stood between frames, where the presenter last put it.
    let backend = SyntheticBackend::new(SyntheticClip {
        frames: 120,
        frame_cost: Duration::from_millis(10),
        ..SyntheticClip::default()
    });
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(
        backend,
        clock.clone(),
        PlayerConfig {
            ahead: Duration::from_millis(333),
            ..config(64)
        },
    );
    // The window filled while paused, so playing to frame 5 needs no
    // catching up.
    until(WAIT, || (player.cache().held().len() >= 10).then_some(()))
        .expect("the window ahead never filled");
    player.play();
    let between = Time(Rational::new(11, 60).unwrap());
    clock.set(between, Instant::now());
    show(&player, 5);
    player.pause();
    let _ = player.present(Instant::now());
    player.step(40);
    // The worker is past the window before the presenter looks again.
    std::thread::sleep(Duration::from_millis(150));
    show(&player, 45);
    player.step(-35);
    show(&player, 10);
    assert_eq!(snapshot(&player).state, PlayState::Paused);
    common::join(player);
}

#[test]
fn steps_follow_runs_both_ways() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(clip(120), clock.clone(), config(64));
    // The frame after the one shown comes up while paused, so a step to
    // it is instant: no request reaches the worker.
    let requests = |player: &Player| snapshot(player).streams[0].seeks.started;
    until(WAIT, || {
        player
            .slots()
            .views()
            .iter()
            .any(|v| v.frame.pts == 1 && v.state == SlotState::Ready)
            .then_some(())
    })
    .expect("the frame after never came up");
    let before = until(WAIT, || {
        let s = snapshot(&player);
        (s.state == PlayState::Paused).then(|| s.streams[0].seeks.started)
    })
    .unwrap();
    player.step(1);
    show(&player, 1);
    player.step(-1);
    show(&player, 0);
    assert_eq!(
        requests(&player),
        before,
        "an instant step went to the worker"
    );
    // Farther, through the cache's links.
    player.step(5);
    show(&player, 5);
    player.step(-2);
    show(&player, 3);
    // Back from a seek into the cache.
    player.seek(at(20), SeekMode::Exact);
    show(&player, 20);
    player.step(-1);
    show(&player, 19);
    // Back from a seek past what the cache holds: decoded.
    player.seek(at(100), SeekMode::Exact);
    show(&player, 100);
    player.step(-3);
    show(&player, 97);
    assert_eq!(snapshot(&player).state, PlayState::Paused);
    common::join(player);
}

#[test]
fn a_step_while_a_seek_or_step_is_on_its_way_goes_on_from_where_it_lands() {
    // 30 ms a frame: each request below is still on its way when the
    // next comes, as when a key follows a click or repeats.
    let backend = SyntheticBackend::new(SyntheticClip {
        frames: 240,
        frame_cost: Duration::from_millis(30),
        ..SyntheticClip::default()
    });
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(backend, clock, config(64));
    // From the seek's frame, which never shows: the step superseded it.
    player.seek(at(207), SeekMode::Exact);
    player.step(1);
    let mut seen = Vec::new();
    show_seen(&player, 208, &mut seen);
    assert_eq!(seen, [at(0), at(208)]);
    // A step's frames add to those of the step on its way.
    player.step(5);
    player.step(5);
    show(&player, 218);
    // And come back to the seek's frame.
    player.seek(at(230), SeekMode::Exact);
    player.step(1);
    player.step(-1);
    show(&player, 230);
    let s = snapshot(&player);
    assert_eq!((s.state, s.clock), (PlayState::Paused, at(230)));
    common::join(player);
}

#[test]
fn a_step_while_a_keyframe_seek_is_on_its_way_goes_on_from_its_keyframe() {
    // A keyframe every 8 frames: the seek to 213 lands on 208, so the
    // steps on its way show 210, as they would after it, and not 215.
    let backend = SyntheticBackend::new(SyntheticClip {
        frames: 240,
        frame_cost: Duration::from_millis(30),
        ..SyntheticClip::default()
    });
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(backend, clock, config(64));
    player.seek(at(213), SeekMode::Keyframe);
    player.step(1);
    player.step(1);
    let mut seen = Vec::new();
    show_seen(&player, 210, &mut seen);
    assert_eq!(seen, [at(0), at(210)]);
    common::join(player);
}

#[test]
fn instant_steps_before_a_present_go_on_from_each_other() {
    // Two key events in one UI frame: the second step goes on from the
    // first's frame, though no present came between.
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(clip(120), clock, config(64));
    let ready = |want: &[i64]| {
        until(WAIT, || {
            let views = player.slots().views();
            want.iter()
                .all(|&k| {
                    views
                        .iter()
                        .any(|v| v.frame.pts == k && v.state == SlotState::Ready)
                })
                .then_some(())
        })
        .unwrap_or_else(|| panic!("frames {want:?} never came up"));
    };
    let requests = |player: &Player| snapshot(player).streams[0].seeks.started;
    ready(&[1, 2]);
    let before = until(WAIT, || {
        let s = snapshot(&player);
        (s.state == PlayState::Paused).then(|| s.streams[0].seeks.started)
    })
    .unwrap();
    player.step(1);
    player.step(1);
    show(&player, 2);
    assert_eq!(snapshot(&player).clock, at(2));
    // There and back is where it started.
    ready(&[1, 3]);
    player.step(1);
    player.step(-1);
    let p = show(&player, 2);
    assert!(p.changed, "the steps' frames never showed");
    assert_eq!(snapshot(&player).clock, at(2));
    assert_eq!(
        requests(&player),
        before,
        "an instant step went to the worker"
    );
    common::join(player);
}

#[test]
fn the_av_offset_moves_the_frame_only_while_playing() {
    for offset_ms in [50, -50] {
        let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
        let player = opened(
            clip(120),
            clock.clone(),
            PlayerConfig {
                av_offset_ms: offset_ms,
                ..config(64)
            },
        );
        // Paused, a frame-exact seek's frame stays up, while the upload
        // thread brings the frames beside it up into slots.
        player.seek(at(10), SeekMode::Exact);
        show(&player, 10);
        for _ in 0..100 {
            let p = player.present(Instant::now()).unwrap();
            assert_eq!((pts(&p), p.time), (vec![10], at(10)), "{offset_ms} ms");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(snapshot(&player).clock, at(10));
        // Playing, the frame due the offset after the clock's time.
        player.play();
        let offset = Time(Rational::new(i64::from(offset_ms), 1000).unwrap());
        let p = player.present(Instant::now()).unwrap();
        assert_eq!(
            p.time,
            at(10).checked_add(offset).unwrap(),
            "{offset_ms} ms"
        );
        common::join(player);
    }
}

/// Wait `d`, or until `stop` says to stop: false then.
fn wait_unless_stopped(d: Duration, stop: &StopToken) -> bool {
    let start = Instant::now();
    while start.elapsed() < d {
        if stop.is_stopped() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// The synthetic clip, with stream 1's decoder taking `seek` longer a
/// seek and `next` longer a frame: a lens decoder running behind. While
/// `fail` is set, its seeks fail.
#[derive(Debug)]
struct SlowSecond {
    inner: SyntheticBackend,
    seek: Duration,
    next: Duration,
    fail: Arc<AtomicBool>,
}

#[derive(Debug)]
struct SlowVideo {
    inner: Box<dyn VideoDecode>,
    seek: Duration,
    next: Duration,
    fail: Arc<AtomicBool>,
    path: PathBuf,
}

impl VideoDecode for SlowVideo {
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
        if !wait_unless_stopped(self.seek, stop) {
            return Ok(SeekOutcome::Stopped);
        }
        if self.fail.load(Ordering::Acquire) {
            return Err(MediaError::Decode {
                path: self.path.clone(),
                reason: "stream 1's seeks fail".into(),
            });
        }
        self.inner.seek(at, mode, stop)
    }

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        if !wait_unless_stopped(self.next, stop) {
            return Ok(NextFrame::Stopped);
        }
        self.inner.next_frame(stop)
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

impl MediaBackend for SlowSecond {
    fn name(&self) -> &'static str {
        "slow-second"
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
        Ok(match req.stream {
            Some(1) => Box::new(SlowVideo {
                inner,
                seek: self.seek,
                next: self.next,
                fail: self.fail.clone(),
                path: path.to_path_buf(),
            }),
            _ => inner,
        })
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }
}

/// The synthetic clip, its frames coming `delay_ms` apart once that is
/// set (seeks keep their own cost): a decoder that turns slow when a
/// test says so.
#[derive(Debug)]
struct Slowed {
    inner: SyntheticBackend,
    delay_ms: Arc<AtomicU64>,
}

#[derive(Debug)]
struct SlowedVideo {
    inner: Box<dyn VideoDecode>,
    delay_ms: Arc<AtomicU64>,
}

impl VideoDecode for SlowedVideo {
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

    fn next_frame(&mut self, stop: &StopToken) -> Result<NextFrame, MediaError> {
        let delay = Duration::from_millis(self.delay_ms.load(Ordering::Acquire));
        if !wait_unless_stopped(delay, stop) {
            return Ok(NextFrame::Stopped);
        }
        self.inner.next_frame(stop)
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

impl MediaBackend for Slowed {
    fn name(&self) -> &'static str {
        "slowed"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.inner.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        Ok(Box::new(SlowedVideo {
            inner: self.inner.open_video(path, req)?,
            delay_ms: self.delay_ms.clone(),
        }))
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }
}

/// A 120-frame clip of 512x256 frames through a cache of about 42, its
/// decoder slowed to `delay_ms` a frame once that is set: opened, played
/// on `clock` to frame 90 (so that the cache holds frames from about 69
/// to 111).
fn played_to_90(clock: &Arc<ManualClock>, delay_ms: &Arc<AtomicU64>) -> Player {
    let backend = Slowed {
        inner: SyntheticBackend::new(SyntheticClip {
            width: 512,
            height: 256,
            frames: 120,
            keyint: 30,
            ..SyntheticClip::default()
        }),
        delay_ms: delay_ms.clone(),
    };
    let config = PlayerConfig {
        cache: CacheBudget::Bytes(8 << 20),
        ..config(8)
    };
    let player = opened(backend, clock.clone(), config);
    player.play();
    for k in 1..=90 {
        clock.set(at(k), Instant::now());
        show(&player, k);
    }
    let held = until(WAIT, || {
        let s = player.cache().stats();
        (s.ahead >= 15).then_some(s)
    });
    assert!(held.is_some(), "{:?}", player.cache().stats());
    player
}

/// Present until stream 0 shows frame `k`, whatever the clock says (the
/// frame before a seek stays up until the seek's is ready).
fn lands_on(player: &Player, k: i64) {
    let shown = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (p.frames[0].frame.time == at(k)).then_some(())
    });
    assert!(shown.is_some(), "frame {k} never came up");
}

#[test]
fn a_seek_back_leaves_the_old_run_ahead_out_of_the_slots_and_prerolls() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let delay_ms = Arc::new(AtomicU64::new(0));
    let player = played_to_90(&clock, &delay_ms);
    // From here a frame comes every 120 ms after the seek's own, as at
    // odin's loop points, where the decoder was only just ahead of the
    // clock and the cache held frames from before the loop.
    delay_ms.store(120, Ordering::Release);
    player.seek(at(0), SeekMode::Exact);
    lands_on(&player, 0);
    // The frames held from 69 on are ahead of the playhead, but past a
    // gap: none of them takes a slot the frames after 0 need (90, on
    // screen until 0 was, is Ready until the upload thread frees it).
    // And the clock waits, the seek's frame up, until frames 1 and 2 are
    // Ready.
    let start = Instant::now();
    let mut started = None;
    while start.elapsed() < Duration::from_millis(600) {
        let p = player.present(Instant::now()).unwrap();
        assert_eq!(pts(&p), [0], "the clock hasn't moved");
        let ready: Vec<Time> = player
            .slots()
            .views()
            .iter()
            .filter(|v| v.state == SlotState::Ready)
            .map(|v| v.frame.time)
            .collect();
        assert!(
            ready.iter().all(|&t| t <= at(3) || t == at(90)),
            "a frame the seek left took a slot: {ready:?}"
        );
        if clock.is_running() {
            started.get_or_insert(ready);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let ready = started.expect("the clock never started");
    assert!(
        ready.contains(&at(1)) && ready.contains(&at(2)),
        "the clock started with {ready:?} ready"
    );
    for k in 1..=4 {
        clock.set(at(k), Instant::now());
        show(&player, k);
    }
    assert_eq!(snapshot(&player).perf.counter(Counter::Dropped), 0);
    common::join(player);
}

#[test]
fn a_preroll_at_the_last_frame_waits_for_nothing_more() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let delay_ms = Arc::new(AtomicU64::new(0));
    let player = played_to_90(&clock, &delay_ms);
    let start = Instant::now();
    player.seek(at(119), SeekMode::Exact);
    lands_on(&player, 119);
    // No frame comes after it, so the clock starts as soon as it is up,
    // not after the preroll's limit.
    let started = until(WAIT, || {
        let _ = player.present(Instant::now());
        clock.is_running().then(|| start.elapsed())
    });
    let started = started.expect("the clock never started");
    assert!(
        started < spherecut_playback::PREROLL_LIMIT / 2,
        "it started after {started:?}"
    );
    common::join(player);
}

/// Two streams of a 90-frame clip, as the X5's raw files hold one per
/// lens, on `slots` GPU slots with stream 1 slowed as [`SlowSecond`]
/// says: opened and showing both first frames.
fn two_streams(slots: u8, seek: Duration, next: Duration) -> Player {
    let backend = SlowSecond {
        inner: clip(90),
        seek,
        next,
        fail: Arc::default(),
    };
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = player(
        backend,
        clock,
        PlayerConfig {
            gpu_slots: slots,
            ..config(64)
        },
    );
    player.open_streams("two.synthetic".into(), vec![Some(0), Some(1)]);
    lands(&player, &[0, 0]);
    player
}

/// Present until the streams show the frames `want` (by pts), failing
/// after [`WAIT`] with what the slots and the snapshot hold.
fn lands(player: &Player, want: &[i64]) {
    let got = until(WAIT, || {
        let p = player.present(Instant::now())?;
        (pts(&p) == want).then_some(())
    });
    if got.is_none() {
        let views: Vec<_> = player
            .slots()
            .views()
            .iter()
            .map(|v| (v.index, v.state, v.frame.stream, v.frame.pts))
            .collect();
        let snap = snapshot(player);
        panic!(
            "{want:?} never came up: showing {:?}, {:?}, landed {:?}; slots {views:?}, {:?}; cache {:?}",
            player.present(Instant::now()).map(|p| pts(&p)),
            snap.state,
            snap.streams.iter().map(|s| s.landed).collect::<Vec<_>>(),
            player.slots().stats(),
            snap.cache
        );
    }
}

#[test]
fn two_streams_on_three_slots_seek_and_step() {
    // Two old frames on screen and two new ones to put up need four
    // slots; with three, the presenter puts each stream's up as it comes.
    let player = two_streams(3, Duration::ZERO, Duration::ZERO);
    player.seek(at(40), SeekMode::Exact);
    lands(&player, &[40, 40]);
    player.step(1);
    lands(&player, &[41, 41]);
    player.step(5);
    lands(&player, &[46, 46]);
    player.step(-1);
    lands(&player, &[45, 45]);
    assert_eq!(snapshot(&player).state, PlayState::Paused);
    common::join(player);
}

#[test]
fn two_streams_seek_with_the_second_decoder_behind() {
    // Stream 1 decodes a frame in 500 ms and seeks in 300: when the seek
    // comes, stream 0 has landed long before, and must leave stream 1 a
    // slot for its frame.
    let player = two_streams(4, Duration::from_millis(300), Duration::from_millis(500));
    for k in [40, 10, 70] {
        player.seek(at(k), SeekMode::Exact);
        lands(&player, &[k, k]);
    }
    common::join(player);
}

#[test]
fn two_streams_on_three_slots_seek_with_one_failing() {
    // The failing decoder is opened first, as player stream 0, since the
    // third slot goes to stream 0's next frame. It fails the seek, so its
    // frame on screen stays and it wants no slot: its next frame's slot
    // must go to stream 1's new one.
    let fail = Arc::new(AtomicBool::new(false));
    let backend = SlowSecond {
        inner: clip(90),
        seek: Duration::ZERO,
        next: Duration::ZERO,
        fail: fail.clone(),
    };
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = player(
        backend,
        clock,
        PlayerConfig {
            gpu_slots: 3,
            ..config(64)
        },
    );
    player.open_streams("two.synthetic".into(), vec![Some(1), Some(0)]);
    lands(&player, &[0, 0]);
    // Paused, the third slot takes stream 0's frame after the one shown.
    let third = until(WAIT, || {
        let _ = player.present(Instant::now());
        let views = player.slots().views();
        views
            .iter()
            .any(|v| v.state == SlotState::Ready && v.frame.stream == 0 && v.frame.pts == 1)
            .then_some(())
    });
    assert!(third.is_some(), "stream 0's next frame never went up");
    fail.store(true, Ordering::Release);
    player.seek(at(40), SeekMode::Exact);
    lands(&player, &[0, 40]);
    common::join(player);
}

#[test]
fn two_streams_play_on_three_slots_with_the_second_decoder_behind() {
    // Stream 0's next frame is ready long before stream 1's frame at the
    // clock, 150 ms a frame: on three slots it must leave the slot that
    // frame needs, or the two lenses show different frames until the
    // clock moves.
    let backend = SlowSecond {
        inner: clip(90),
        seek: Duration::ZERO,
        next: Duration::from_millis(150),
        fail: Arc::default(),
    };
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = player(
        backend,
        clock.clone(),
        PlayerConfig {
            gpu_slots: 3,
            ..config(64)
        },
    );
    player.open_streams("two.synthetic".into(), vec![Some(0), Some(1)]);
    lands(&player, &[0, 0]);
    player.play();
    for k in 1..=12 {
        clock.set(at(k), Instant::now());
        lands(&player, &[k, k]);
    }
    common::join(player);
}

/// Two streams on three slots and the cache at its budget: a keyframe
/// seek back shows each stream's keyframe as it comes, and the one shown
/// first must stay in the cache until the other is up, or its stream
/// would want nothing and keep the slot the other needs.
#[test]
fn two_streams_keyframe_seek_back_on_three_slots_with_the_cache_full() {
    let frame = 64 * 32 * 3 / 2;
    let backend = SyntheticBackend::new(SyntheticClip {
        frames: 120,
        frame_cost: Duration::from_millis(5),
        ..SyntheticClip::default()
    });
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = player(
        backend,
        clock.clone(),
        PlayerConfig {
            gpu_slots: 3,
            cache: CacheBudget::Bytes(16 * frame),
            ..config(64)
        },
    );
    player.open_streams("two.synthetic".into(), vec![Some(0), Some(1)]);
    lands(&player, &[0, 0]);
    player.play();
    for k in 1..=60 {
        clock.set(at(k), Instant::now());
        lands(&player, &[k, k]);
    }
    player.pause();
    // Keyframes every 8, each seek once the one before is on screen.
    for (k, key) in [(45, 40), (29, 24), (21, 16), (13, 8), (5, 0)] {
        player.seek(at(k), SeekMode::Keyframe);
        lands(&player, &[key, key]);
    }
    common::join(player);
}

/// A keyframe seek back lands on a keyframe the cache doesn't hold, and
/// decoding on from it to the time asked for fills the cache: the frame
/// it landed on must stay until it is in a slot, though it is the
/// farthest behind the playhead.
#[test]
fn a_keyframe_seek_back_with_the_cache_full_lands_on_its_keyframe() {
    let frame = 64 * 32 * 3 / 2;
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = player(
        clip(120),
        clock.clone(),
        PlayerConfig {
            cache: CacheBudget::Bytes(8 * frame),
            ..config(64)
        },
    );
    player.open("clip.synthetic".into());
    show(&player, 0);
    player.play();
    for k in 1..=60 {
        clock.set(at(k), Instant::now());
        show(&player, k);
    }
    player.pause();
    // Keyframes every 8: 45 lands on 40, then a scrub back, each seek
    // once the one before is on screen.
    for (k, key) in [(45, 40), (37, 32), (29, 24), (21, 16), (5, 0)] {
        player.seek(at(k), SeekMode::Keyframe);
        lands(&player, &[key]);
        assert_eq!(snapshot(&player).clock, at(key));
    }
    common::join(player);
}

/// FFmpeg's backend, with a device stood in for by software that fails
/// frame `plan.frame` "as hardware" (as synth's fallback tests do).
#[derive(Debug)]
struct FaultBackend {
    inner: FfmpegBackend,
    plan: FaultPlan,
}

impl MediaBackend for FaultBackend {
    fn name(&self) -> &'static str {
        "fault"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.inner.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let req = VideoRequest {
            decode: DecodeMode::Hardware {
                kind: HwKind::VaApi,
                device: None,
                fallback: Fallback::Software,
            },
            fault: Some(self.plan),
            ..req.clone()
        };
        self.inner.open_video(path, &req)
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }
}

fn h264(dir: &TempDir) -> PathBuf {
    media::write(
        &Spec {
            name: "h264".into(),
            codec: Codec::H264,
            gop: Some(Gop {
                keyint: 8,
                bframes: 3,
                open: true,
                radl: 0,
            }),
            frames: 30,
            ..Spec::default()
        },
        &dir.0,
    )
    .unwrap()
}

#[test]
fn a_fallback_from_a_failing_device_reaches_the_snapshot() {
    let dir = TempDir::new("fault");
    let path = h264(&dir);
    let backend = FaultBackend {
        inner: FfmpegBackend::new(),
        plan: FaultPlan {
            frame: 10,
            fault: Fault::Decode,
            stand_in: true,
        },
    };
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = player(backend, clock.clone(), config(64));
    player.open(path);
    let mut seen = Vec::new();
    show_seen(&player, 0, &mut seen);
    player.play();
    // Every frame in turn, the failing one and those after it included
    // (the worker decodes ahead, so it fell back by now, or soon).
    for k in 1..=20 {
        clock.set(at(k), Instant::now());
        show_seen(&player, k, &mut seen);
    }
    assert_eq!(seen, (0..=20).map(at).collect::<Vec<_>>());
    let snap = snapshot(&player);
    let s = &snap.streams[0];
    assert!(
        matches!(
            s.fallback,
            Some((HwKind::VaApi, FallbackReason::Failed { .. }))
        ),
        "{:?}",
        s.fallback
    );
    assert!(
        matches!(s.decode, Some(DecodePath::FellBack { .. })),
        "{:?}",
        s.decode
    );
    let fell: Vec<_> = player
        .events()
        .into_iter()
        .filter(|e| matches!(e, PlayerEvent::FellBack { .. }))
        .collect();
    assert_eq!(fell.len(), 1, "{fell:?}");
    assert_eq!(player.perf().count(Counter::Dropped), 0);
    common::join(player);
}

/// FFmpeg's backend, with every Vulkan decode on FFmpeg's device on
/// lavapipe, which has no video decode, whatever GPU the machine has:
/// left to itself the backend opens the GPU behind the default render
/// node, which decodes H.264 on odin. Pinned by name, as synth's fallback
/// tests do it; a render node that matches no device would not do,
/// since the backend then takes the first GPU listed.
#[derive(Debug)]
struct LavapipeBackend {
    inner: FfmpegBackend,
    lavapipe: Arc<HwDevice>,
}

impl LavapipeBackend {
    fn new() -> Self {
        let lavapipe = open_hw_device("vulkan", Some("llvmpipe")).unwrap_or_else(|e| {
            panic!(
                "{e}; Tier 1 needs FFmpeg's Vulkan device on Mesa's lavapipe \
                 (mesa-vulkan-drivers on Fedora and Ubuntu, vulkan-swrast on Arch)"
            )
        });
        LavapipeBackend {
            inner: FfmpegBackend::new(),
            lavapipe,
        }
    }
}

impl MediaBackend for LavapipeBackend {
    fn name(&self) -> &'static str {
        "lavapipe"
    }

    fn probe(&self, path: &Path) -> Result<ProbeReport, ProbeError> {
        self.inner.probe(path)
    }

    fn open_video(
        &self,
        path: &Path,
        req: &VideoRequest,
    ) -> Result<Box<dyn VideoDecode>, MediaError> {
        let decode = match &req.decode {
            DecodeMode::Hardware {
                kind: HwKind::Vulkan,
                fallback,
                ..
            } => DecodeMode::Hardware {
                kind: HwKind::Vulkan,
                device: Some(self.lavapipe.clone()),
                fallback: *fallback,
            },
            other => other.clone(),
        };
        let req = VideoRequest {
            decode,
            ..req.clone()
        };
        self.inner.open_video(path, &req)
    }

    fn open_audio(
        &self,
        path: &Path,
        req: &AudioRequest,
    ) -> Result<Box<dyn AudioDecode>, MediaError> {
        self.inner.open_audio(path, req)
    }
}

#[test]
fn auto_picks_vulkan_from_doctor_and_falls_back_on_lavapipe() {
    let dir = TempDir::new("auto");
    let path = h264(&dir);
    // Doctor's checks said Vulkan decodes H.264 here and VA-API doesn't;
    // the Vulkan device the decoder gets is lavapipe's, which decodes
    // nothing.
    let check = |kind, result| HwDecodeCheck {
        format: "H.264 High".into(),
        kind,
        result,
        reason: None,
    };
    let hw = HwSupport {
        checks: vec![
            check(HwKind::VaApi, CheckResult::Fails),
            check(HwKind::Vulkan, CheckResult::Works),
        ],
    };
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let parts = PlayerParts {
        hw: Some(hw),
        ..PlayerParts::new(Arc::new(LavapipeBackend::new()), gpu().clone(), clock)
    };
    let player = Player::new(
        parts,
        PlayerConfig {
            decoder: DecoderPreference::Auto,
            ..config(64)
        },
    )
    .unwrap();
    player.open(path);
    show(&player, 0);
    let snap = snapshot(&player);
    let s = &snap.streams[0];
    let choice = s.choice.as_deref().unwrap_or_default();
    assert!(choice.starts_with("Vulkan"), "{choice}");
    assert!(
        matches!(
            s.fallback,
            Some((
                HwKind::Vulkan,
                FallbackReason::Refused(_) | FallbackReason::DeviceUnavailable(_)
            ))
        ),
        "{:?}",
        s.fallback
    );
    assert!(player.events().iter().any(|e| matches!(
        e,
        PlayerEvent::FellBack {
            from: HwKind::Vulkan,
            ..
        }
    )));
    common::join(player);
}

/// Audio that is ready to play only when the test says so, on a clock the
/// test moves.
#[derive(Debug)]
struct FakeAudio {
    clock: Arc<ManualClock>,
    ready: AtomicBool,
}

impl AudioOutput for FakeAudio {
    fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    fn ready(&self, _ahead: Duration) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct FakeAudioFactory(Arc<FakeAudio>);

impl AudioOutputFactory for FakeAudioFactory {
    fn open(
        &self,
        _path: &Path,
        _stop: &StopToken,
        _threads: &PlayerThreads,
    ) -> Result<Option<Arc<dyn AudioOutput>>, PlayerError> {
        Ok(Some(self.0.clone()))
    }
}

#[test]
fn playback_after_a_seek_waits_for_the_audio_preroll() {
    let audio = Arc::new(FakeAudio {
        clock: Arc::new(ManualClock::new(RATE.frame_duration())),
        ready: AtomicBool::new(true),
    });
    let own = Arc::new(ManualClock::new(RATE.frame_duration()));
    let parts = PlayerParts {
        audio: Some(Arc::new(FakeAudioFactory(audio.clone()))),
        ..PlayerParts::new(Arc::new(clip(120)), gpu().clone(), own.clone())
    };
    let player = Player::new(parts, config(64)).unwrap();
    player.open("clip.synthetic".into());
    show(&player, 0);
    assert!(snapshot(&player).has_audio);
    player.play();
    // Video follows the audio's clock, not its own.
    let _ = player.present(Instant::now());
    assert!(audio.clock.is_running() && !own.is_running());
    audio.ready.store(false, Ordering::Release);
    player.seek(at(50), SeekMode::Exact);
    show(&player, 50);
    // The frame is up, and then the frames after it that the video's
    // preroll waits for (which a loaded machine takes a while to upload:
    // odin, running the engine tests side by side, took more than the
    // 40 ms this once gave); the audio isn't ready, so the clock waits.
    let prerolled = until(WAIT, || {
        let _ = player.present(Instant::now());
        let views = player.slots().views();
        (1..=PREROLL_FRAMES as i64)
            .all(|k| {
                views
                    .iter()
                    .any(|v| v.state == SlotState::Ready && v.frame.time == at(50 + k))
            })
            .then_some(())
    });
    assert!(
        prerolled.is_some(),
        "the video's preroll never came in: {:?}",
        player.slots().views()
    );
    for _ in 0..20 {
        let _ = player.present(Instant::now());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!audio.clock.is_running());
    assert_eq!(snapshot(&player).state, PlayState::Seeking);
    audio.ready.store(true, Ordering::Release);
    let _ = player.present(Instant::now());
    assert!(audio.clock.is_running());
    assert_eq!(audio.clock.now(), at(50));
    assert_eq!(snapshot(&player).state, PlayState::Playing);
    common::join(player);
}

/// Audio that takes a while to open, and gives up if its open is
/// stopped, as a real device's would.
#[derive(Debug)]
struct SlowAudioFactory(Arc<FakeAudio>);

impl AudioOutputFactory for SlowAudioFactory {
    fn open(
        &self,
        _path: &Path,
        stop: &StopToken,
        _threads: &PlayerThreads,
    ) -> Result<Option<Arc<dyn AudioOutput>>, PlayerError> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(200) {
            if stop.is_stopped() {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(Some(self.0.clone()))
    }
}

#[test]
fn a_seek_while_the_audio_opens_keeps_the_audio() {
    let audio = Arc::new(FakeAudio {
        clock: Arc::new(ManualClock::new(RATE.frame_duration())),
        ready: AtomicBool::new(true),
    });
    let parts = PlayerParts {
        audio: Some(Arc::new(SlowAudioFactory(audio.clone()))),
        ..PlayerParts::new(
            Arc::new(clip(120)),
            gpu().clone(),
            Arc::new(ManualClock::new(RATE.frame_duration())),
        )
    };
    let player = Player::new(parts, config(64)).unwrap();
    player.open("clip.synthetic".into());
    // Seeks stop the requests before them, not the open's audio.
    player.seek(at(10), SeekMode::Exact);
    player.seek(at(20), SeekMode::Exact);
    show(&player, 20);
    let has_audio = until(WAIT, || {
        let s = player.snapshot();
        s.fresh.then_some(s.has_audio)
    });
    assert_eq!(has_audio, Some(true));
    common::join(player);
}

#[test]
fn new_settings_reach_the_threads() {
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = opened(clip(30), clock, config(64));
    player.set_config(PlayerConfig {
        gpu_slots: 3,
        ..config(32)
    });
    let budget = until(WAIT, || {
        let b = player.cache().stats().budget;
        (b == 32 << 20).then_some(b)
    });
    assert!(budget.is_some());
    assert_eq!(player.slots().active(), 3);
    let snap = until(WAIT, || {
        let s = player.snapshot();
        s.budget.filter(|b| b.bytes == 32 << 20).map(|_| s)
    });
    assert!(snap.is_some());
    common::join(player);
}

/// Risk 11's acceptance (M1 plan, slice 5): 5.7K frames, 25 MB each,
/// through a 256 MiB cache and four GPU slots on lavapipe, frame by frame
/// on a `ManualClock`: every frame plays, and the cache never holds more
/// than its budget.
#[test]
fn a_5k7_source_plays_inside_a_256_mib_cache() {
    let backend = SyntheticBackend::new(SyntheticClip {
        width: 5760,
        height: 2880,
        frames: 60,
        keyint: 30,
        ..SyntheticClip::default()
    });
    let budget = 256u64 << 20;
    let clock = Arc::new(ManualClock::new(RATE.frame_duration()));
    let player = player(backend, clock.clone(), config(256));
    player.open("5k7.synthetic".into());
    let start = Instant::now();
    let mut peak = 0;
    let mut watch = |player: &Player| {
        let s = player.cache().stats();
        assert!(s.bytes <= budget, "{} bytes held of {budget}", s.bytes);
        peak = peak.max(s.bytes);
    };
    let first = until(WAIT, || {
        watch(&player);
        player.present(Instant::now())
    });
    assert_eq!(first.map(|p| pts(&p)), Some(vec![0]));
    player.play();
    for k in 1..60 {
        clock.set(at(k), Instant::now());
        let shown = until(WAIT, || {
            watch(&player);
            let p = player.present(Instant::now())?;
            (pts(&p) == [k]).then_some(())
        });
        assert!(shown.is_some(), "frame {k} never came up");
    }
    let took = start.elapsed();
    let snap = snapshot(&player);
    let stats = player.cache().stats();
    eprintln!(
        "60 frames of 5.7K in {took:?}; peak {} MiB of {}, {} frames of {} MB, upload cpu {:?}",
        peak >> 20,
        budget >> 20,
        budget / stats.frame_bytes,
        stats.frame_bytes / 1_000_000,
        snap.perf.stage(spherecut_playback::Stage::UploadCpu)
    );
    assert!(!stats.too_small);
    assert!(peak > budget / 2, "the cache never filled: {peak}");
    assert_eq!(snap.perf.counter(Counter::Dropped), 0);
    assert!(snap.perf.counter(Counter::FramesShown) >= 60);
    assert!(player.events().iter().all(|e| !matches!(
        e,
        PlayerEvent::CacheTooSmall { .. } | PlayerEvent::SlotDropped { .. }
    )));
    common::join(player);
}
