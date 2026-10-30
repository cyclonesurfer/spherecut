//! Tier 2: the default output device plays (M1 plan sections 9.3 and
//! 12.2, DECISIONS D35).
//!
//! `audio_output_device_plays` plays a 440 Hz tone at −12 dBFS on the
//! default device, from a thread that keeps the ring full as the decoder
//! does, and watches the clock and the callbacks for 2 s of `Instant`
//! from the moment the clock starts to move. It passes when:
//!
//! - the clock advances 2.0 s ± 50 ms and never goes back;
//! - no gap between callbacks is over twice the callback's buffer;
//! - the median reported latency is 5 to 200 ms;
//! - the ring never ran dry and the clock never left the device's.
//!
//! The host, device, format, buffer and latency go into the report's
//! notes, and the numbers into its benchmark table.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use spherecut_audio::{AudioError, Output, OutputStats, Sink};
use spherecut_core::Clock;
use spherecut_testkit::{bench, tier2};

/// How long the clock is watched.
const RUN: Duration = Duration::from_secs(2);
/// How far the clock may be from [`RUN`] after it.
const TOLERANCE: Duration = Duration::from_millis(50);
/// How long the clock may take to start moving after `play`.
const START_LIMIT: Duration = Duration::from_secs(2);

const TONE_HZ: f64 = 440.0;
/// −12 dBFS.
const AMPLITUDE: f64 = 0.25;

/// Push the tone into `sink` from sample 0, keeping the ring full, until
/// `stop`.
fn tone(sink: Sink, stop: Arc<AtomicBool>) {
    let rate = f64::from(sink.rate());
    let ch = usize::from(sink.channels());
    let epoch = sink.want().epoch;
    let mut next = 0i64;
    let mut buf = Vec::new();
    while !stop.load(Ordering::Acquire) {
        buf.clear();
        for i in next..next + 1024 {
            let v = (AMPLITUDE * (std::f64::consts::TAU * TONE_HZ * i as f64 / rate).sin()) as f32;
            buf.extend(std::iter::repeat_n(v, ch));
        }
        let mut done = 0;
        while done < 1024 && !stop.load(Ordering::Acquire) {
            let n = sink.push(epoch, next + done as i64, &buf[done * ch..]);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            done += n;
        }
        next += 1024;
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// One look at the output while it plays.
struct Look {
    at: Instant,
    time: f64,
    stats: OutputStats,
}

#[test]
#[ignore = "tier2: the default audio output device"]
fn audio_output_device_plays() {
    let (out, sink) = tier2::require_audio_device(|| match Output::open_device() {
        Ok(o) => Ok(o),
        Err(AudioError::NoDevice { reason }) => Err(reason),
        Err(e) => panic!("{e}"),
    });
    let rate = out.rate();
    let clock = out.clock();
    let stop = Arc::new(AtomicBool::new(false));
    let producer = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("tone".into())
            .spawn(move || tone(sink, stop))
            .unwrap()
    };
    // The ring full before it plays, as the engine's preroll has it.
    let filling = Instant::now();
    while !out.ready(Duration::from_millis(400)) {
        assert!(
            filling.elapsed() < Duration::from_secs(2),
            "the tone thread didn't fill the ring"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let played = Instant::now();
    clock.play(played);
    let look = || {
        let at = Instant::now();
        Look {
            at,
            time: clock.time_at(at).to_secs_f64(),
            stats: out.stats(),
        }
    };
    let first = loop {
        let l = look();
        if l.time > 0.0 {
            break l;
        }
        assert!(
            played.elapsed() < START_LIMIT,
            "the clock didn't start within {START_LIMIT:?} of play ({:?})",
            l.stats
        );
        std::thread::sleep(Duration::from_micros(500));
    };
    let mut looks = vec![first];
    while looks.last().unwrap().at < looks[0].at + RUN {
        std::thread::sleep(Duration::from_micros(500));
        looks.push(look());
    }
    stop.store(true, Ordering::Release);
    producer.join().unwrap();
    let (first, last) = (&looks[0], looks.last().unwrap());
    let stats = &last.stats;

    // The callbacks: each look's last interval against that callback's
    // buffer, a callback at a time.
    let mut worst = 0.0f64;
    let mut late = Vec::new();
    for w in looks.windows(2) {
        let s = &w[1].stats;
        if s.callbacks == w[0].stats.callbacks || s.period == 0 {
            continue;
        }
        let period = f64::from(s.period) / f64::from(rate) * 1e3;
        let interval = ms(s.interval);
        worst = worst.max(interval / period);
        if interval > 2.0 * period {
            late.push(format!("{interval:.1} ms for {} frames", s.period));
        }
    }
    let mut latencies: Vec<f64> = looks.iter().map(|l| ms(l.stats.latency)).collect();
    latencies.sort_by(f64::total_cmp);
    let latency = latencies[latencies.len() / 2];
    let elapsed = last.at - first.at;
    let advanced = last.time - first.time;
    let back = looks.windows(2).filter(|w| w[1].time < w[0].time).count();

    bench::note(format!(
        "audio output: host {}, device {}, {} at {} Hz, {} channels, buffer asked {}, \
         last period {} frames, latency {:.1} ms (median; {:.1} to {:.1}), \
         {} callbacks, {} host xruns",
        stats.host,
        stats.device.as_deref().unwrap_or("unknown"),
        stats.sample_format.as_deref().unwrap_or("unknown format"),
        stats.rate,
        stats.channels,
        stats
            .buffer
            .map_or("the host's default".into(), |b| format!("{b} frames")),
        stats.period,
        latency,
        latencies[0],
        latencies[latencies.len() - 1],
        stats.callbacks - first.stats.callbacks,
        stats.xruns,
    ));
    bench::record(
        "audio clock over 2 s of Instant",
        advanced / elapsed.as_secs_f64() * 2.0,
        "s",
    );
    bench::record("audio latency (median)", latency, "ms");
    bench::record("audio callback gap, worst (× its buffer)", worst, "x");

    assert_eq!(back, 0, "the clock went back {back} times");
    let off = (advanced - elapsed.as_secs_f64()).abs();
    assert!(
        off <= TOLERANCE.as_secs_f64(),
        "the clock advanced {advanced:.4} s in {:.4} s of Instant",
        elapsed.as_secs_f64()
    );
    assert!(
        late.is_empty(),
        "callbacks over twice their buffer apart: {late:?}"
    );
    assert!(
        (5.0..=200.0).contains(&latency),
        "the median latency is {latency:.1} ms, not 5 to 200"
    );
    assert_eq!(stats.underruns, 0, "the ring ran dry: {stats:?}");
    assert_eq!(stats.fallbacks, 0, "the clock left the device's: {stats:?}");
}
