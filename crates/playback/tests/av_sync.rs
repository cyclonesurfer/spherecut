//! A/V sync (M1 plan section 9.3): the player on the fake audio device,
//! whose callbacks come at scripted instants with the real callback
//! code, over a synthetic clip whose audio says which sample it is.
//!
//! Each combination of 29.97 or 59.94 fps with 44.1 or 48 kHz plays 60 s
//! of scripted time, presenting at 60 Hz: at every present the frame
//! shown is the one whose interval holds the audio clock's time. A seek
//! restarts the audio at sample round(at × rate); a decode stall leaves
//! the clock and video running, and the sample heard after it is the
//! one the clock is at; losing the device moves to the system clock
//! without a jump; and the null output keeps time in real time.
//!
//! Scripted time runs far ahead of real time, while the decoders run in
//! real time: the fake device waits for each callback's audio, and each
//! present for the frame due, so that only what a test stalls is late.
//! The player is played and sought at scripted instants too
//! (`Player::play_at`, `seek_at`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use spherecut_audio::{AudioClock, FakeAudioDevice, FakeConfig, OutputKind, OutputStats, Played};
use spherecut_core::{Clock, FrameRate, Rounding, SystemClock, Time};
use spherecut_media::backend::SeekMode;
use spherecut_playback::{
    AudioDevice, CacheBudget, DecoderPreference, DeviceAudio, PREROLL_AUDIO, PREROLL_LIMIT,
    PlayState, Player, PlayerConfig, PlayerEvent, PlayerParts, SlotState, Stage,
};
use spherecut_synth::fake::{SyntheticAudio, SyntheticBackend, SyntheticClip, audio_index};

use common::{gpu, pts, snapshot, until};

const WAIT: Duration = Duration::from_secs(20);
/// A 60 Hz display.
const REFRESH: Duration = Duration::from_nanos(16_666_667);
const LATENCY: Duration = Duration::from_millis(20);
/// More presents than the presenter keeps intervals of.
const WARM_UP: u32 = 64;

fn config() -> PlayerConfig {
    PlayerConfig {
        cache: CacheBudget::Bytes(64 << 20),
        decoder: DecoderPreference::Software,
        ..PlayerConfig::default()
    }
}

fn backend(fps: FrameRate, seconds: u32) -> (SyntheticBackend, SyntheticAudio) {
    let frames = (f64::from(seconds) * fps.to_f64()).ceil() as u64 + 2;
    let audio = SyntheticAudio::new(Time::from_secs(i64::from(seconds) + 1));
    let clip = SyntheticClip {
        width: 64,
        height: 32,
        rate: fps,
        frames,
        keyint: 30,
        frame_cost: Duration::ZERO,
    };
    (SyntheticBackend::with_audio(clip, audio.clone()), audio)
}

/// What a present showed.
#[derive(Debug, Clone, Copy)]
struct Seen {
    /// The clock's time it was presented for.
    time: Time,
    frame: i64,
}

/// A player on the fake device, presenting at scripted instants.
struct Rig {
    player: Player,
    dev: FakeAudioDevice,
    clock: Arc<AudioClock>,
    fps: FrameRate,
    frames: i64,
    rate: u32,
    audio: SyntheticAudio,
    /// The scripted instant.
    t: Instant,
    /// The frame shown last.
    shown: i64,
    /// Whether the device and the presents wait for the audio decoder
    /// (not while a test stalls it).
    patient: bool,
    /// Callbacks looked at, and the ones checked against the clock.
    looked: usize,
    checked: usize,
    /// How far ahead of a sample heard the clock may be: after a late
    /// callback its floor holds it until the audio catches up.
    hold: Duration,
}

impl Rig {
    fn new(fps: FrameRate, rate: u32, seconds: u32) -> Rig {
        let (backend, audio) = backend(fps, seconds);
        let frames = backend.clip.frames as i64;
        let backend = Arc::new(backend);
        // Scripted time stays ahead of real time, as the decode workers
        // read the clock at real instants: before its anchor's, the
        // clock shows the anchor's time, so they see it no further on
        // than the presents do, and don't skip frames to catch up with
        // it.
        let t0 = Instant::now() + Duration::from_secs(3600);
        // Before the device's first callback, a display's worth of
        // presents, paused, so that the presenter's latency (the median
        // interval between presents) is a refresh and not the zeros of
        // the wait for the first frame.
        let start = t0 + REFRESH * WARM_UP;
        let cfg = FakeConfig {
            rate,
            channels: 2,
            period: 1024,
            latency: LATENCY,
        };
        let (out, sink, mut dev) = FakeAudioDevice::new(cfg, start);
        dev.set_wait(WAIT);
        let clock = out.clock();
        let parts = PlayerParts {
            audio: Some(Arc::new(DeviceAudio::given(backend.clone(), out, sink))),
            ..PlayerParts::new(backend, gpu().clone(), Arc::new(SystemClock::new()))
        };
        let player = Player::new(parts, config()).unwrap();
        player.open("av.synthetic".into());
        let first = until(WAIT, || player.present(t0).filter(|p| pts(p) == [0]));
        assert!(first.is_some(), "the first frame never came up");
        assert!(snapshot(&player).has_audio);
        let mut t = t0;
        while t < start {
            t += REFRESH;
            let _ = player.present(t);
        }
        Rig {
            player,
            dev,
            clock,
            fps,
            frames,
            rate,
            audio,
            t,
            shown: 0,
            patient: true,
            looked: 0,
            checked: 0,
            hold: Duration::ZERO,
        }
    }

    /// Stall the audio decoder, or let it go again.
    fn stall_audio(&mut self, stall: bool) {
        if stall {
            self.audio.valve.close();
        } else {
            self.audio.valve.release();
        }
        self.patient = !stall;
        self.dev.set_wait(if stall { Duration::ZERO } else { WAIT });
    }

    /// The frame whose interval holds `t`.
    fn due(&self, t: Time) -> i64 {
        self.fps
            .time_to_frame(t, Rounding::Floor)
            .unwrap()
            .clamp(0, self.frames - 1)
    }

    /// The next refresh: run the device up to it, checking its callbacks
    /// against the clock, and present, waiting (in real time) for the
    /// frame due to be ready, and while prerolling for the audio. The
    /// frame shown is the one whose interval holds the time presented
    /// for, and video never goes back. What was presented is dropped,
    /// so that its slot can go.
    fn refresh(&mut self) -> Seen {
        if self.patient
            && !self.clock.is_running()
            && self.player.snapshot().state == PlayState::Seeking
        {
            let ready = until(WAIT, || self.clock.ready(PREROLL_AUDIO).then_some(()));
            assert!(ready.is_some(), "the audio never came for the preroll");
        }
        self.t += REFRESH;
        self.dev.run_until(self.t);
        self.check_callbacks();
        // One present, and if its frame isn't up yet, another once it is:
        // presenting again and again at one instant would shrink the
        // presenter's latency (the median interval between presents) and
        // move its playhead back.
        let t = self.t;
        let mut p = until(WAIT, || self.player.present(t)).unwrap();
        if pts(&p) != [self.due(p.time)] {
            let want = self.fps.frame_to_time(self.due(p.time)).unwrap();
            let playhead = p.time;
            drop(p);
            let up = until(WAIT, || {
                // A present whose playhead found the cache's lock taken
                // asks for a repaint, which a GUI's next frame gives.
                if self.player.cache().stats().playhead != playhead {
                    let _ = self.player.present(t);
                }
                self.player
                    .slots()
                    .views()
                    .iter()
                    .any(|v| {
                        v.frame.time == want
                            && matches!(v.state, SlotState::Ready | SlotState::Shown)
                    })
                    .then_some(())
            });
            p = self.player.present(t).unwrap();
            if up.is_none() || pts(&p) != [self.due(p.time)] {
                let snap = self.player.snapshot();
                let views: Vec<_> = self
                    .player
                    .slots()
                    .views()
                    .iter()
                    .map(|v| (v.state, v.frame.time))
                    .collect();
                panic!(
                    "frame {:?} shown for {:?}, frame {} due; slots {views:?}; cache {:?}; counters {:?}",
                    pts(&p),
                    p.time,
                    self.due(p.time),
                    self.player.cache().stats(),
                    snap.perf.counters,
                );
            }
        }
        let k = pts(&p)[0];
        assert!(k >= self.shown, "back from frame {} to {k}", self.shown);
        self.shown = k;
        Seen {
            time: p.time,
            frame: k,
        }
    }

    /// The latest callback anchors the clock: at its first frame's
    /// instant, the clock shows that sample's time (to within half a
    /// sample, or up to [`Rig::hold`] ahead), and the buffer holds that
    /// sample. Earlier callbacks of
    /// the same run were checked as the latest, or are past.
    fn check_callbacks(&mut self) {
        let played = self.dev.played();
        if played.len() == self.looked {
            return;
        }
        self.looked = played.len();
        let p = *played.last().unwrap();
        if p.rendered.from.is_none() {
            return;
        }
        self.check(&p);
        self.checked += 1;
    }

    fn check(&self, p: &Played) {
        let from = p.rendered.from.unwrap();
        let shown = self.clock.time_at(p.heard).to_secs_f64();
        let rate = f64::from(self.rate);
        let ahead = shown - from as f64 / rate;
        let slack = 0.5 / rate + 1e-9;
        assert!(
            (-slack..=self.hold.as_secs_f64() + slack).contains(&ahead),
            "{shown} as sample {from} is heard"
        );
        if p.rendered.fill.copied > 0 && p.rendered.fill.gap == 0 {
            assert_eq!(audio_index(p.first), from % (1 << 20), "{p:?}");
        }
    }

    fn run_for(&mut self, d: Duration) -> Vec<Seen> {
        let end = self.t + d;
        let mut out = Vec::new();
        while self.t < end {
            out.push(self.refresh());
        }
        out
    }

    /// Refresh until a callback after the `skip`th plays audio; that
    /// callback, the latest.
    fn until_heard(&mut self, skip: usize) -> Played {
        for _ in 0..1000 {
            self.refresh();
            let played = self.dev.played();
            if played.len() > skip
                && let Some(p) = played.last()
                && p.rendered.from.is_some()
            {
                return *p;
            }
        }
        panic!("no audio was heard");
    }

    fn output_stats(&self) -> OutputStats {
        self.player.audio_stats().expect("the player has audio")
    }

    fn finish(self) {
        common::join(self.player);
    }
}

#[test]
fn video_follows_the_audio_clock_at_every_rate() {
    for fps in [FrameRate::FPS_29_97, FrameRate::FPS_59_94] {
        for rate in [44_100, 48_000] {
            let mut rig = Rig::new(fps, rate, 61);
            rig.player.play_at(rig.t);
            let presents = rig.run_for(Duration::from_secs(60));
            // Every frame of the minute came up, but for those a 60 Hz
            // display had no present for (at 59.94 fps, one in a
            // thousand).
            let shown: std::collections::BTreeSet<i64> = presents.iter().map(|p| p.frame).collect();
            let last = *shown.last().unwrap();
            let expect = (60.0 * fps.to_f64()) as i64;
            assert!((last - expect).abs() <= 3, "{last} of {expect}");
            assert!(
                shown.len() as i64 >= last - last / 500,
                "{} of {last} frames shown",
                shown.len()
            );
            // The audio: each callback played the samples after the last
            // one's, and the ring never ran dry.
            let played: Vec<Played> = rig
                .dev
                .played()
                .iter()
                .filter(|p| p.rendered.from.is_some())
                .copied()
                .collect();
            for w in played.windows(2) {
                assert_eq!(
                    w[1].rendered.from.unwrap(),
                    w[0].rendered.from.unwrap() + 1024
                );
                assert_eq!(w[1].rendered.fill.copied, 1024);
            }
            assert!(rig.checked as f64 > 59.0 * f64::from(rate) / 1024.0);
            let stats = rig.output_stats();
            assert_eq!(stats.underruns, 0, "{fps:?} at {rate}");
            assert_eq!(stats.fallbacks, 0, "{fps:?} at {rate}");
            // Each frame shown within its interval is in sync: the A/V
            // offset is 0 but where the rig presented before its frame
            // was up (it decodes in step with the presents), late by
            // under a refresh.
            let av = rig.player.perf().samples(Stage::AvOffset);
            let off: Vec<f32> = av.iter().copied().filter(|v| *v != 0.0).collect();
            eprintln!(
                "{fps:?} at {rate}: {} of {} A/V samples off: {off:?}",
                off.len(),
                av.len()
            );
            assert!(av.len() >= 200, "{} A/V samples", av.len());
            // The median is 0 (frame by frame, it was half a frame).
            assert!(off.len() * 2 < av.len(), "{fps:?} at {rate}: {off:?}");
            let refresh_ms = REFRESH.as_secs_f32() * 1000.0;
            assert!(off.iter().all(|v| *v > 0.0 && *v <= refresh_ms), "{off:?}");
            rig.finish();
        }
    }
}

#[test]
fn a_seek_restarts_the_audio_at_its_sample_and_the_video_at_its_frame() {
    for (fps, rate) in [
        (FrameRate::FPS_29_97, 44_100),
        (FrameRate::FPS_59_94, 48_000),
    ] {
        let mut rig = Rig::new(fps, rate, 20);
        rig.player.play_at(rig.t);
        rig.run_for(Duration::from_secs(2));
        let at = fps.frame_to_time(300).unwrap();
        rig.player.seek_at(at, SeekMode::Exact, rig.t);
        rig.shown = 0;
        let before = rig.dev.played().len();
        let first = rig.until_heard(before);
        let sample = (at.to_secs_f64() * f64::from(rate)).round() as i64;
        assert_eq!(first.rendered.from, Some(sample));
        assert_eq!(audio_index(first.first), sample % (1 << 20));
        assert_eq!(
            rig.clock.time_at(first.heard),
            at,
            "exactly the seek's time"
        );
        // Video resumes at the seek's frame, then follows.
        assert_eq!(rig.shown, 300);
        rig.run_for(Duration::from_secs(1));
        assert!(rig.shown > 320);
        assert_eq!(rig.output_stats().fallbacks, 0);
        rig.finish();
    }
}

#[test]
fn a_decode_stall_keeps_the_clock_and_video_running_and_skips_the_debt() {
    let mut rig = Rig::new(FrameRate::FPS_29_97, 48_000, 20);
    rig.player.play_at(rig.t);
    rig.run_for(Duration::from_secs(2));
    // The audio decoder stalls; the ring drains, then runs dry.
    rig.stall_audio(true);
    let start = rig.t;
    let dry_at = loop {
        let p = rig.refresh();
        if rig.output_stats().underruns > 0 {
            break p.time;
        }
        assert!(rig.t.duration_since(start) < Duration::from_secs(2));
    };
    // 200 ms dry: video and the clock go on.
    let during = rig.run_for(Duration::from_millis(200));
    let ran = during.last().unwrap().time.checked_sub(dry_at).unwrap();
    assert!(
        (ran.to_secs_f64() - 0.2).abs() < 0.03,
        "the clock ran {ran:?} in 200 ms"
    );
    assert!(during.last().unwrap().frame >= during[0].frame + 5);
    // The decoder comes back: the next sample heard is the one due (the
    // rig checks each callback against the clock and the sample).
    rig.stall_audio(false);
    let before = rig.dev.played().len();
    let checked = rig.checked;
    rig.run_for(Duration::from_secs(1));
    let back = rig.dev.played()[before..]
        .iter()
        .find(|p| p.rendered.fill.copied == 1024)
        .copied()
        .expect("audio came back");
    assert!(back.rendered.from.unwrap() > 0);
    assert!(rig.checked > checked + 30);
    let stats = rig.output_stats();
    assert_eq!(stats.underruns, 1, "one dry spell");
    assert_eq!(stats.fallbacks, 0);
    rig.finish();
}

#[test]
fn a_late_callback_never_takes_the_video_back() {
    // A callback 30 ms late makes the clock correct its rate, which can
    // bring a time read a present's latency ahead of it below the one
    // read at the present before (D35).
    const LATE: Duration = Duration::from_millis(30);
    let mut rig = Rig::new(FrameRate::FPS_29_97, 48_000, 20);
    rig.hold = LATE;
    rig.player.play_at(rig.t);
    let mut last = rig.run_for(Duration::from_secs(1)).last().unwrap().time;
    for _ in 0..20 {
        rig.dev.stall(LATE);
        for p in rig.run_for(Duration::from_millis(250)) {
            assert!(p.time >= last, "back from {last:?} to {:?}", p.time);
            last = p.time;
        }
    }
    assert!(rig.shown > 150);
    let stats = rig.output_stats();
    assert_eq!(stats.underruns, 0);
    assert_eq!(stats.fallbacks, 0);
    rig.finish();
}

#[test]
fn losing_the_device_moves_to_the_system_clock_without_a_jump() {
    let mut rig = Rig::new(FrameRate::FPS_29_97, 48_000, 20);
    rig.player.play_at(rig.t);
    rig.run_for(Duration::from_secs(2));
    let t = rig.t + Duration::from_millis(5);
    let before = rig.clock.time_at(t);
    rig.dev.lose(t);
    let after = rig.clock.time_at(t);
    let jump = (after.to_secs_f64() - before.to_secs_f64()).abs();
    assert!(jump < 0.001, "{jump} s");
    assert!(rig.output_stats().on_system_clock);
    // Video plays on, on the system clock.
    let presents = rig.run_for(Duration::from_secs(1));
    let ran = presents
        .last()
        .unwrap()
        .time
        .checked_sub(presents[0].time)
        .unwrap()
        .to_secs_f64();
    assert!((ran - 1.0).abs() < 0.02, "{ran}");
    // A new stream takes the clock back to the audio, with no jump.
    rig.dev.restore(rig.t);
    let mut last = presents.last().unwrap().time;
    let checked = rig.checked;
    for _ in 0..60 {
        let p = rig.refresh();
        let step = p.time.checked_sub(last).unwrap().to_secs_f64();
        assert!(step < REFRESH.as_secs_f64() + 0.001, "a jump of {step} s");
        last = p.time;
    }
    let stats = rig.output_stats();
    assert!(!stats.on_system_clock);
    assert_eq!(stats.fallbacks, 1);
    assert!(rig.checked > checked + 30);
    rig.finish();
}

#[test]
fn a_preroll_waits_its_limit_for_the_audio_and_then_plays_without_it() {
    let mut rig = Rig::new(FrameRate::FPS_29_97, 48_000, 20);
    rig.player.play_at(rig.t);
    rig.run_for(Duration::from_secs(1));
    rig.stall_audio(true);
    let at = FrameRate::FPS_29_97.frame_to_time(200).unwrap();
    rig.player.seek_at(at, SeekMode::Exact, rig.t);
    rig.shown = 0;
    let start = rig.t;
    // The seek's frame comes up; the clock waits for the audio.
    let mut late = None;
    while rig.t < start + PREROLL_LIMIT + Duration::from_millis(500) {
        let p = rig.refresh();
        if late.is_none()
            && rig
                .player
                .events()
                .iter()
                .any(|e| matches!(e, PlayerEvent::AudioLate))
        {
            late = Some((rig.t, p.time));
        }
    }
    let (when, time) = late.expect("the preroll never gave up");
    let waited = when.duration_since(start);
    assert!(
        waited >= PREROLL_LIMIT && waited < PREROLL_LIMIT + Duration::from_millis(100),
        "{waited:?}"
    );
    // It played on from the seek's time, on the system clock.
    assert!(
        time.checked_sub(at).unwrap().to_secs_f64() < 0.05,
        "{time:?}"
    );
    assert!(rig.output_stats().on_system_clock);
    assert!(rig.clock.is_running());
    rig.stall_audio(false);
    rig.finish();
}

fn player_on(backend: SyntheticBackend, device: AudioDevice) -> Player {
    let backend = Arc::new(backend);
    let parts = PlayerParts {
        audio: Some(Arc::new(DeviceAudio::new(backend.clone(), device))),
        ..PlayerParts::new(backend, gpu().clone(), Arc::new(SystemClock::new()))
    };
    Player::new(parts, config()).unwrap()
}

fn null_player(backend: SyntheticBackend) -> Player {
    player_on(
        backend,
        AudioDevice::Null {
            rate: 48_000,
            channels: 2,
        },
    )
}

/// The output `AudioOpened` names, once frame 0 is up.
fn opened_on(player: &Player) -> Option<OutputKind> {
    assert!(
        until(WAIT, || player
            .present(Instant::now())
            .filter(|p| pts(p) == [0]))
        .is_some()
    );
    until(WAIT, || {
        player.events().into_iter().find_map(|e| match e {
            PlayerEvent::AudioOpened { output } => output.map(|s| s.kind),
            _ => None,
        })
    })
}

/// `AudioDevice::Default` plays on the default device when it opens,
/// and on the null output when it doesn't, as here.
#[test]
fn the_default_device_is_the_null_output_without_one() {
    let expected = match spherecut_audio::Output::open_device() {
        Ok(_) => OutputKind::Device,
        Err(_) => OutputKind::Null,
    };
    let (backend, _audio) = backend(FrameRate::FPS_29_97, 2);
    let player = player_on(backend, AudioDevice::Default);
    player.open("default.synthetic".into());
    assert_eq!(opened_on(&player), Some(expected));
    common::join(player);
}

#[test]
fn the_null_output_keeps_time() {
    let (backend, _audio) = backend(FrameRate::FPS_29_97, 10);
    let player = null_player(backend);
    player.open("null.synthetic".into());
    assert_eq!(opened_on(&player), Some(OutputKind::Null));
    player.play();
    // Playing starts once the first audio is heard.
    let start = until(WAIT, || {
        let now = Instant::now();
        let p = player.present(now)?;
        (p.time > Time::ZERO).then_some((now, p.time))
    })
    .unwrap();
    let mut last = start;
    while last.0.duration_since(start.0) < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(16));
        let now = Instant::now();
        if let Some(p) = player.present(now) {
            last = (now, p.time);
        }
    }
    let wall = last.0.duration_since(start.0).as_secs_f64();
    let ran = last.1.checked_sub(start.1).unwrap().to_secs_f64();
    assert!((ran / wall - 1.0).abs() < 0.05, "{ran} s in {wall} s");
    common::join(player);
}

/// The audio's threads are the player's: a newer open ends the last
/// file's, and the player's join waits for them, a decoder stuck waiting
/// for its read included, and reports them.
#[test]
fn the_audio_threads_end_with_their_open_and_the_players_join() {
    let (backend, audio) = backend(FrameRate::FPS_29_97, 10);
    let player = null_player(backend);
    for name in ["first.synthetic", "second.synthetic"] {
        player.open(name.into());
        assert_eq!(opened_on(&player), Some(OutputKind::Null), "{name}");
        player.play();
        let playing = until(WAIT, || {
            player
                .present(Instant::now())
                .filter(|p| p.time > Time::ZERO)
        });
        assert!(playing.is_some(), "{name} never played");
    }
    audio.valve.close();
    std::thread::sleep(Duration::from_millis(50));
    let report = player.join(common::JOIN);
    assert!(report.is_clean(), "{report:?}");
    let count = |name: &str| report.joined.iter().filter(|j| *j == name).count();
    assert_eq!(
        (count("audio-decode"), count("audio-null")),
        (2, 2),
        "{report:?}"
    );
}

#[test]
fn a_file_without_audio_plays_on_the_players_clock() {
    let player = null_player(SyntheticBackend::new(SyntheticClip {
        frames: 30,
        ..SyntheticClip::default()
    }));
    player.open("silent.synthetic".into());
    assert!(
        until(WAIT, || player
            .present(Instant::now())
            .filter(|p| pts(p) == [0]))
        .is_some()
    );
    assert!(!snapshot(&player).has_audio);
    assert!(
        player
            .events()
            .iter()
            .any(|e| matches!(e, PlayerEvent::NoAudio))
    );
    assert!(player.audio_stats().is_none());
    common::join(player);
}
