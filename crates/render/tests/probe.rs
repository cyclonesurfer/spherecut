//! Tier 1, on lavapipe: the gate probe (`spherecut_render::probe`, a
//! diagnostic of the VA-API copy path's stall, off by default) times a
//! tiny job on the device's queue every period, sees the queue held up by
//! work queued ahead of it, and waits at the device's submit gate (D41)
//! as every other thread that submits does.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use spherecut_render::{GateProbe, Gpu, ProbeReport};
use spherecut_testkit::control::{Comparison, at_least, at_most, below, must_fail};

/// A probe on `gpu` every `period`, stopped after `run`.
fn probe(gpu: &Gpu, period: Duration, run: Duration) -> ProbeReport {
    let p = GateProbe::start(gpu, period).unwrap();
    std::thread::sleep(run);
    p.stop()
}

/// The `p` quantile of the report's latencies in ms, by the nearest
/// rank; NaN with none, which decides no comparison.
fn quantile_ms(r: &ProbeReport, p: f64) -> f64 {
    let mut v: Vec<f64> = r
        .samples
        .iter()
        .map(|s| s.latency.as_secs_f64() * 1e3)
        .collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    v[((p * n as f64).ceil() as usize).clamp(1, n) - 1]
}

/// Test 2's check: 300 jobs or more timed in 2 s, and a p99 under
/// 100 ms (lavapipe runs on the CPU, so the bound is loose).
fn timed(r: &ProbeReport) -> (Comparison<usize>, Comparison<f64>) {
    (
        at_least(r.samples.len(), 300),
        below(quantile_ms(r, 0.99), 100.0),
    )
}

/// Clears of a 64 MiB buffer to a submission that takes `busy` or more
/// on this host: one submission of 16 timed after one of a clear, which
/// faults the buffer's pages in.
fn clears_for(gpu: &Gpu, big: &wgpu::Buffer, busy: Duration) -> u32 {
    let submit = |clears: u32| {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        for _ in 0..clears {
            enc.clear_buffer(big, 0, None);
        }
        let t = Instant::now();
        gpu.queue.submit([enc.finish()]);
        gpu.wait_for_submitted().unwrap();
        t.elapsed()
    };
    submit(1);
    let each = submit(16) / 16;
    (busy.as_secs_f64() / each.as_secs_f64().max(1e-6)).ceil() as u32
}

/// Every 4 ms for 2 s on an idle queue: 300 jobs or more, each submitted
/// within the run and listed in order, all done by the stop, and the
/// p99 under 100 ms.
///
/// Controls: every 20 ms, the same 2 s times too few jobs; and with
/// another thread keeping the queue busy with clears sized to take
/// 300 ms a submission on this host, which the probe's jobs wait behind,
/// the same p99 is 100 ms or more.
#[test]
fn the_probe_times_a_tiny_job_every_period() {
    let gpu = common::gpu();
    let period = Duration::from_millis(4);
    let run = Duration::from_secs(2);
    let start = Instant::now();
    let r = probe(gpu, period, run);
    let took = start.elapsed();
    let (n, p99) = timed(&r);
    eprintln!(
        "every {period:?} for {took:?}: {} jobs, p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms, {} \
         unfinished",
        r.samples.len(),
        quantile_ms(&r, 0.5),
        quantile_ms(&r, 0.99),
        quantile_ms(&r, 1.0),
        r.unfinished
    );
    assert_eq!(n.passes(), Some(true), "{n}");
    assert_eq!(p99.passes(), Some(true), "{p99}");
    assert_eq!(r.period, period);
    assert_eq!(r.unfinished, 0);
    assert!(
        r.samples
            .windows(2)
            .all(|w| w[0].submitted < w[1].submitted),
        "the samples in the order submitted"
    );
    let first = r.samples.first().unwrap().submitted;
    let last = r.samples.last().unwrap().submitted;
    assert!(first >= start && last <= start + took, "{first:?} {last:?}");

    // Control: every 20 ms, about 100 jobs in the same time.
    let slow = probe(gpu, Duration::from_millis(20), run);
    let n = must_fail("a probe every 20 ms", timed(&slow).0);
    assert!(n > 50, "{n} jobs every 20 ms");

    // Control: the queue kept busy by another thread, a submission at a
    // time, each clearing 64 MiB as many times over as takes 300 ms on
    // this host (lavapipe runs a submission on the thread that submits
    // it, so the probe's submit waits there too).
    let big = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("busy"),
        size: 64 << 20,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let clears = clears_for(gpu, &big, Duration::from_millis(300));
    let stop = Arc::new(AtomicBool::new(false));
    let busy = {
        let gpu = gpu.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut jobs = 0u32;
            let mut job_took = Duration::ZERO;
            while !stop.load(Ordering::Acquire) {
                let mut enc = gpu.device.create_command_encoder(&Default::default());
                for _ in 0..clears {
                    enc.clear_buffer(&big, 0, None);
                }
                let t = Instant::now();
                gpu.queue.submit([enc.finish()]);
                gpu.wait_for_submitted().unwrap();
                job_took = job_took.max(t.elapsed());
                jobs += 1;
            }
            (jobs, job_took)
        })
    };
    let loaded = probe(gpu, period, run);
    stop.store(true, Ordering::Release);
    let (jobs, job_took) = busy.join().unwrap();
    eprintln!(
        "behind {jobs} busy jobs of {clears} clears (the longest {job_took:?}): {} jobs, p50 \
         {:.2} ms, p99 {:.2} ms",
        loaded.samples.len(),
        quantile_ms(&loaded, 0.5),
        quantile_ms(&loaded, 0.99)
    );
    assert!(
        job_took >= Duration::from_millis(150),
        "the busy jobs took at most {job_took:?}: too short to hold the probe up"
    );
    let held = must_fail("a probe behind a busy queue", timed(&loaded).1);
    assert!(held >= 100.0, "{held}");
}

/// While the device's gate is held closed, the probe submits nothing;
/// it waited at the gate, and once the gate opens it goes on.
///
/// Control: a probe on a device of its own on the same GPU, whose gate
/// stays open, submits while the first device's is closed.
#[test]
fn the_probe_waits_at_the_submit_gate() {
    let gpu = common::gpu();
    let beside = gpu.beside("the control's probe").unwrap();
    let gate = gpu.submit_gate().clone();
    let waits = gate.stats().waits;
    let period = Duration::from_millis(4);
    let gated = GateProbe::start(gpu, period).unwrap();
    let open = GateProbe::start(&beside, period).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // Closed for 300 ms: a close holds the gate CLOSED_AT_MOST (20 ms),
    // so it is closed again every 2 ms.
    let closed = Instant::now();
    gate.close();
    while closed.elapsed() < Duration::from_millis(300) {
        std::thread::sleep(Duration::from_millis(2));
        gate.close();
    }
    let opened = Instant::now();
    gate.open();
    std::thread::sleep(Duration::from_millis(100));
    let (gated, open) = (gated.stop(), open.stop());
    // A job that had passed the gate as it closed may still be stamped
    // just after: those of the first 10 ms aren't counted.
    let shut = closed + Duration::from_millis(10);
    let while_closed = |r: &ProbeReport| {
        let n = r
            .samples
            .iter()
            .filter(|s| s.submitted > shut && s.submitted < opened)
            .count();
        at_most(n, 0)
    };
    let before = |r: &ProbeReport| r.samples.iter().filter(|s| s.submitted < closed).count();
    let after = |r: &ProbeReport| r.samples.iter().filter(|s| s.submitted >= opened).count();
    eprintln!(
        "gated: {} before, {} after; open: {} in all",
        before(&gated),
        after(&gated),
        open.samples.len()
    );
    let held = while_closed(&gated);
    assert_eq!(held.passes(), Some(true), "{held}");
    assert!(before(&gated) > 5 && after(&gated) > 5);
    assert!(
        gate.stats().waits > waits,
        "the probe didn't wait at the gate"
    );
    let n = must_fail(
        "a probe on a device whose gate stayed open",
        while_closed(&open),
    );
    assert!(n > 20, "{n} jobs while the other gate was closed");
}
