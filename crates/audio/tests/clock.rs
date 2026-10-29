//! The audio clock on the fake device (M1 plan sections 9.2 and 9.3):
//! it waits for audio, runs by the samples played, freezes when the
//! callbacks stall, runs through underruns skipping the debt, restarts
//! a seek at round(at × rate), and falls back to the system clock and
//! back to the audio without a jump.

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use spherecut_audio::{
    FakeAudioDevice, FakeConfig, Fallback, Output, RESYNC_LEAD, STALL, Sink, WAIT_LIMIT,
};
use spherecut_core::{Clock, FrameRate, Rational, Time};

/// A sample's value: its index, exactly, in both channels.
fn value(i: i64) -> f32 {
    (i.rem_euclid(1 << 20)) as f32
}

/// The decoder's part: pushes a ramp from where the clock's epoch wants
/// it.
struct Feeder {
    sink: Sink,
    epoch: u64,
    pos: i64,
}

impl Feeder {
    fn new(sink: Sink) -> Self {
        Feeder {
            sink,
            epoch: 0,
            pos: 0,
        }
    }

    /// Follow the epoch, as the decoder's seek does.
    fn sync(&mut self) {
        let want = self.sink.want();
        if want.epoch != self.epoch {
            self.epoch = want.epoch;
            self.pos = want.start;
        }
    }

    /// Push up to sample `to`, as far as the ring takes.
    fn fill_to(&mut self, to: i64) {
        self.sync();
        let ch = usize::from(self.sink.channels());
        while self.pos < to {
            let n = (to - self.pos).min(512);
            let data: Vec<f32> = (self.pos..self.pos + n)
                .flat_map(|i| std::iter::repeat_n(value(i), ch))
                .collect();
            let taken = self.sink.push(self.epoch, self.pos, &data);
            self.pos += taken as i64;
            if taken == 0 {
                break;
            }
        }
    }

    /// Keep the ring full.
    fn top_up(&mut self) {
        self.fill_to(i64::MAX / 4);
    }
}

fn fake(rate: u32, latency_ms: u64) -> (Output, Feeder, FakeAudioDevice, Instant) {
    let t0 = Instant::now() + Duration::from_secs(1);
    let cfg = FakeConfig {
        rate,
        channels: 2,
        period: 1024,
        latency: Duration::from_millis(latency_ms),
    };
    let (out, sink, mut dev) = FakeAudioDevice::new(cfg, t0);
    dev.set_wait(Duration::ZERO);
    (out, Feeder::new(sink), dev, t0)
}

fn secs(t: Time) -> f64 {
    t.to_secs_f64()
}

/// Every callback while the ring is kept full: the buffer holds the
/// samples from `from` on, and the clock shows `from / rate` when they
/// are heard, to within half a sample (a seek's time shows exactly as
/// its sample round(at × rate) is heard).
fn check_steady(out: &Output, feeder: &mut Feeder, dev: &mut FakeAudioDevice, n: usize) {
    let clock = out.clock();
    let rate = f64::from(dev.config().rate);
    for _ in 0..n {
        feeder.top_up();
        let p = dev.tick().unwrap();
        let from = p.rendered.from.unwrap();
        assert_eq!(p.rendered.fill.copied, 1024, "{p:?}");
        assert_eq!(dev.buffer()[0], value(from));
        assert_eq!(dev.buffer()[2 * 1023 + 1], value(from + 1023));
        let shown = secs(clock.time_at(p.heard));
        assert!(
            (shown - from as f64 / rate).abs() < 0.5 / rate + 1e-9,
            "{shown} at {from}"
        );
    }
}

#[test]
fn the_clock_waits_for_audio_then_runs_by_the_samples_played() {
    for rate in [44_100, 48_000] {
        let (out, mut feeder, mut dev, t0) = fake(rate, 20);
        let clock = out.clock();
        feeder.top_up();
        clock.play(t0 - Duration::from_millis(5));
        assert!(clock.is_running());
        assert_eq!(clock.time_at(t0), Time::ZERO, "waiting for audio");
        let p = dev.tick().unwrap();
        assert_eq!(p.rendered.from, Some(0));
        // Until the first sample is heard the clock holds 0; then it
        // runs.
        assert_eq!(clock.time_at(t0 + Duration::from_millis(19)), Time::ZERO);
        assert_eq!(clock.time_at(p.heard), Time::ZERO);
        let ten = secs(clock.time_at(p.heard + Duration::from_millis(10)));
        assert!((ten - 0.010).abs() < 1e-9, "{ten}");
        check_steady(&out, &mut feeder, &mut dev, 3000);
        assert_eq!(out.stats().underruns, 0);
    }
}

#[test]
fn the_clock_never_goes_back_and_freezes_when_callbacks_stop() {
    let (out, mut feeder, mut dev, t0) = fake(48_000, 30);
    let clock = out.clock();
    feeder.top_up();
    clock.play(t0);
    let mut last = Time::ZERO;
    let mut t = t0;
    let end = t0 + Duration::from_secs(3);
    let mut stall: Option<(i64, Instant)> = None;
    let mut most = 0.0f64;
    while t < end {
        if stall.is_none() && t > t0 + Duration::from_secs(1) {
            // 300 ms without a callback, which is less than STALL, while
            // the device plays nothing (a suspended sink).
            let p = dev.played().last().unwrap();
            stall = Some((p.rendered.from.unwrap(), dev.next_callback().unwrap()));
            dev.stall(Duration::from_millis(300));
        }
        if dev.next_callback().unwrap() <= t {
            feeder.top_up();
            dev.tick();
        }
        let now = clock.time_at(t);
        assert!(now >= last, "back from {last:?} to {now:?} at {:?}", t - t0);
        if let Some((_, due)) = stall
            && t < due + Duration::from_millis(300)
        {
            most = most.max(secs(now));
        }
        last = now;
        t += Duration::from_micros(500);
    }
    // While stalled, the clock froze at the end of the last buffer
    // written plus one more buffer.
    let (from, _) = stall.unwrap();
    let cap = (from + 2 * 1024) as f64 / 48_000.0;
    assert!((most - cap).abs() < 1e-6, "{most} vs {cap}");
    // The device played nothing for 300 ms, so the clock, which follows
    // it, is 300 ms further behind wall time than the latency.
    let behind = 3.0 - secs(clock.time_at(end));
    assert!((behind - 0.330).abs() < 1e-6, "{behind}");
    assert!(!out.stats().on_system_clock);
}

#[test]
fn an_underrun_keeps_the_clock_running_and_skips_the_debt() {
    let (out, mut feeder, mut dev, t0) = fake(48_000, 20);
    let clock = out.clock();
    feeder.fill_to(12_000);
    clock.play(t0);
    // A quarter of a second queued, then the decoder stalls for 200 ms.
    let stall_end = t0 + Duration::from_millis(450);
    dev.run_until(stall_end);
    let shown = secs(clock.time_at(stall_end + Duration::from_millis(20)));
    let wall = 0.450;
    assert!((shown - wall).abs() < 0.03, "the clock ran on: {shown}");
    assert_eq!(out.stats().underruns, 1, "one dry spell, counted once");
    // The decoder catches up from where it stopped; what it pushed late
    // is dropped, and the next sample heard is the one due.
    for _ in 0..200 {
        feeder.top_up();
        let p = dev.tick().unwrap();
        if p.rendered.fill.copied == 1024 {
            let from = p.rendered.from.unwrap();
            assert_eq!(dev.buffer()[0], value(from));
            let due = secs(clock.time_at(p.heard)) * 48_000.0;
            assert!((due - from as f64).abs() < 1.0, "{due} vs {from}");
            assert_eq!(out.stats().underruns, 1);
            return;
        }
    }
    panic!("audio never came back");
}

#[test]
fn running_out_at_the_end_of_the_audio_isnt_an_underrun() {
    let (out, mut feeder, mut dev, t0) = fake(48_000, 20);
    let clock = out.clock();
    feeder.fill_to(4800);
    feeder.sink.end(feeder.epoch, 4800);
    clock.play(t0);
    dev.run_until(t0 + Duration::from_millis(500));
    assert_eq!(out.stats().underruns, 0);
    let shown = secs(clock.time_at(t0 + Duration::from_millis(520)));
    assert!((shown - 0.5).abs() < 0.03, "{shown}");
}

#[test]
fn a_seek_restarts_audio_at_round_at_times_rate() {
    let frame = FrameRate::FPS_29_97.frame_duration();
    for rate in [44_100u32, 48_000] {
        let (out, mut feeder, mut dev, t0) = fake(rate, 25);
        let clock = out.clock();
        feeder.top_up();
        clock.play(t0);
        dev.run_until(t0 + Duration::from_millis(300));
        let at = Time(Rational::from_integer(1234).checked_mul(frame.0).unwrap());
        let now = dev.next_callback().unwrap();
        clock.pause(now);
        clock.set(at, now);
        assert_eq!(clock.time_at(now + Duration::from_secs(1)), at);
        let want = feeder.sink.want();
        assert_eq!(want.at, at);
        let expect = (secs(at) * f64::from(rate)).round() as i64;
        assert_eq!(want.start, expect);
        feeder.top_up();
        assert!(out.ready(Duration::from_millis(100)));
        let resume = now + Duration::from_millis(50);
        dev.run_until(resume);
        clock.play(resume);
        let p = dev.tick().unwrap();
        assert_eq!(p.rendered.from, Some(expect));
        assert_eq!(dev.buffer()[0], value(expect));
        assert_eq!(clock.time_at(p.heard), at, "exactly the seek's time");
        check_steady(&out, &mut feeder, &mut dev, 200);
    }
}

#[test]
fn pausing_freezes_the_clock_and_playing_resumes_at_its_sample() {
    let (out, mut feeder, mut dev, t0) = fake(48_000, 20);
    let clock = out.clock();
    feeder.top_up();
    clock.play(t0);
    dev.run_until(t0 + Duration::from_millis(400));
    let pause = t0 + Duration::from_millis(410);
    let held = clock.time_at(pause);
    clock.pause(pause);
    assert!(!clock.is_running());
    assert_eq!(clock.time_at(pause + Duration::from_secs(5)), held);
    // Paused, the callback plays silence and the ring refills from the
    // paused time.
    dev.run_until(pause + Duration::from_millis(100));
    assert!(dev.played().last().unwrap().rendered.from.is_none());
    assert_eq!(feeder.sink.want().at, held);
    feeder.top_up();
    let resume = pause + Duration::from_millis(200);
    clock.play(resume);
    let p = loop {
        let p = dev.tick().unwrap();
        if p.rendered.from.is_some() {
            break p;
        }
    };
    let expect = (secs(held) * 48_000.0).round() as i64;
    assert_eq!(p.rendered.from, Some(expect));
    assert_eq!(clock.time_at(p.heard), held);
}

#[test]
fn a_seek_past_the_end_of_the_audio_runs_at_once() {
    let (out, mut feeder, mut dev, t0) = fake(48_000, 20);
    let clock = out.clock();
    clock.set(Time::from_secs(100), t0);
    feeder.sync();
    feeder.sink.end(feeder.epoch, feeder.pos);
    assert!(out.ready(Duration::from_millis(100)));
    clock.play(t0);
    dev.run_until(t0 + Duration::from_millis(500));
    let shown = secs(clock.time_at(t0 + Duration::from_millis(500)));
    assert!((shown - 100.48).abs() < 0.03, "{shown}");
    assert_eq!(out.stats().underruns, 0);
}

#[test]
fn ready_needs_the_audio_queued() {
    let (out, mut feeder, _dev, _t0) = fake(48_000, 20);
    feeder.fill_to(2400);
    assert!(!out.ready(Duration::from_millis(100)));
    feeder.fill_to(4800);
    assert!(out.ready(Duration::from_millis(100)));
    // More than the ring holds is capped at what it holds.
    feeder.top_up();
    assert!(out.ready(Duration::from_secs(10)));
}

/// Read the clock every 0.5 ms of scripted time from `from` to `to`,
/// running the device and keeping the ring full: the largest step
/// between reads.
fn largest_step(
    out: &Output,
    feeder: &mut Feeder,
    dev: &mut FakeAudioDevice,
    from: Instant,
    to: Instant,
) -> Duration {
    let clock = out.clock();
    let step = Duration::from_micros(500);
    let mut t = from;
    let mut last = clock.time_at(t);
    let mut largest = Duration::ZERO;
    while t < to {
        t += step;
        feeder.top_up();
        dev.run_until(t);
        let now = clock.time_at(t);
        assert!(now >= last, "back at {:?}", t - from);
        let d = Duration::from_secs_f64(secs(now.checked_sub(last).unwrap()));
        largest = largest.max(d.saturating_sub(step));
        last = now;
    }
    largest
}

#[test]
fn device_loss_falls_back_without_a_jump_and_comes_back_to_the_audio() {
    let (out, mut feeder, mut dev, t0) = fake(48_000, 25);
    let clock = out.clock();
    feeder.top_up();
    clock.play(t0);
    dev.run_until(t0 + Duration::from_millis(500));
    let lost = t0 + Duration::from_millis(510);
    let before = clock.time_at(lost);
    dev.lose(lost);
    let after = clock.time_at(lost);
    let jump = (secs(after) - secs(before)).abs();
    assert!(jump < 0.001, "{jump}");
    let stats = out.stats();
    assert!(stats.on_system_clock);
    assert_eq!(stats.last_fallback, Some(Fallback::Lost));
    assert!(out.ready(Duration::from_millis(100)), "video doesn't wait");
    // The decoder is asked for audio ahead of the clock.
    let want = feeder.sink.want();
    let lead = secs(want.at) - secs(after);
    assert!((lead - RESYNC_LEAD.as_secs_f64()).abs() < 1e-6, "{lead}");
    // On the system clock for a second.
    let gap = largest_step(
        &out,
        &mut feeder,
        &mut dev,
        lost,
        lost + Duration::from_secs(1),
    );
    assert!(gap < Duration::from_micros(10), "{gap:?}");
    let shown = secs(clock.time_at(lost + Duration::from_secs(1)));
    assert!((shown - secs(after) - 1.0).abs() < 1e-6, "{shown}");
    // A new stream: the clock goes back to the audio without a jump, and
    // the samples heard are the ones due.
    let back = lost + Duration::from_secs(1);
    dev.restore(back);
    let gap = largest_step(
        &out,
        &mut feeder,
        &mut dev,
        back,
        back + Duration::from_secs(1),
    );
    assert!(gap < Duration::from_millis(1), "{gap:?}");
    assert!(!out.stats().on_system_clock);
    check_steady(&out, &mut feeder, &mut dev, 100);
}

#[test]
fn a_stall_falls_back_after_its_limit_and_the_audio_takes_over_again() {
    let (out, mut feeder, mut dev, t0) = fake(48_000, 20);
    let clock = out.clock();
    feeder.top_up();
    clock.play(t0);
    dev.run_until(t0 + Duration::from_millis(500));
    let last = dev.played().last().unwrap().at;
    let at_last = secs(clock.time_at(last));
    dev.stall(Duration::from_millis(900));
    let gap = largest_step(
        &out,
        &mut feeder,
        &mut dev,
        last,
        last + Duration::from_millis(800),
    );
    assert!(gap < Duration::from_micros(10), "no jump: {gap:?}");
    let stats = out.stats();
    assert!(stats.on_system_clock);
    assert_eq!(stats.last_fallback, Some(Fallback::Stalled));
    // The watchdog fell back STALL after the last callback, give or take
    // its 50 ms; the clock had frozen two buffers after it until then.
    let shown = secs(clock.time_at(last + Duration::from_millis(800)));
    let wall = at_last + 0.800;
    let frozen = STALL.as_secs_f64() - 2.0 * 1024.0 / 48_000.0 - 0.020;
    assert!(
        (wall - shown - frozen).abs() < 0.051,
        "{} behind",
        wall - shown
    );
    // The callbacks resume: back on the audio.
    let resumed = last + Duration::from_millis(800);
    largest_step(
        &out,
        &mut feeder,
        &mut dev,
        resumed,
        resumed + Duration::from_secs(1),
    );
    assert!(!out.stats().on_system_clock);
    check_steady(&out, &mut feeder, &mut dev, 100);
}

#[test]
fn no_audio_after_playing_starts_runs_the_clock_on_after_its_limit() {
    let (out, _feeder, mut dev, t0) = fake(48_000, 20);
    let clock = out.clock();
    clock.play(t0);
    dev.run_until(t0 + WAIT_LIMIT - Duration::from_millis(100));
    assert_eq!(clock.time_at(t0 + WAIT_LIMIT), Time::ZERO, "still waiting");
    dev.run_until(t0 + WAIT_LIMIT + Duration::from_millis(100));
    let stats = out.stats();
    assert!(stats.on_system_clock);
    assert_eq!(stats.last_fallback, Some(Fallback::Starved));
    let shown = secs(clock.time_at(t0 + WAIT_LIMIT + Duration::from_secs(1)));
    assert!((shown - 1.0).abs() < 0.06, "{shown}");
}

#[test]
fn the_null_output_keeps_time() {
    let (out, sink) = Output::null(48_000, 2);
    let clock = out.clock();
    let mut feeder = Feeder::new(sink);
    feeder.top_up();
    let start = Instant::now();
    clock.play(start);
    let end = start + Duration::from_secs(2);
    while Instant::now() < end {
        feeder.top_up();
        std::thread::sleep(Duration::from_millis(5));
    }
    let now = Instant::now();
    let ran = secs(clock.time_at(now));
    let wall = now.duration_since(start).as_secs_f64();
    assert!((ran / wall - 1.0).abs() < 0.05, "{ran} in {wall}");
    let stats = out.stats();
    assert_eq!(stats.underruns, 0);
    assert!(!stats.on_system_clock);
    assert_eq!(stats.host, "null");
}

#[test]
fn opening_the_device_here_says_there_is_none_or_opens_one() {
    // This box has no device; a machine with one opens it.
    match Output::open_device() {
        Ok((out, _sink)) => {
            let s = out.stats();
            assert!(s.rate > 0 && s.channels > 0, "{s:?}");
        }
        Err(e) => assert!(
            matches!(e, spherecut_audio::AudioError::NoDevice { .. }),
            "{e}"
        ),
    }
}
