//! The gate probe: a diagnostic of the VA-API copy path's stall on AMD,
//! off by default (the smoke run starts it with `SPHERECUT_GATE_PROBE`).
//!
//! The stall's likely cause is a page-table gate in the amdgpu VM that
//! the UI's RADV device shares with radeonsi's VA-API: when a download
//! frees its staging buffer, every later submission in the VM waits for
//! the decodes then queued, the UI's frame among them. The probe asks
//! whether such a gate holds up the UI's queue, apart from the
//! compositor: every period, a thread of its own (`gate-probe`) records
//! a clear of a 256-byte buffer of its own, waits at the device's submit
//! gate (D41) as the uploader does right before it submits, submits it,
//! and registers wgpu's work-done callback right after, which stamps the
//! job's completion. A second thread (`gate-probe-poll`) polls the device
//! every [`POLL`] so that the callback runs within about that of the
//! job's end. Each job's latency is from its submit to its callback.
//!
//! wgpu ties the callback to the latest submission on the queue, so a
//! job another thread submitted between the probe's submit and its
//! registering counts the probe's job done only once that one is too: a
//! latency is at most that much long. The probe submits about 250 small
//! jobs a second at 4 ms, and its waits at the gate count in the gate's
//! own [`GateStats`](spherecut_core::gate::GateStats).
//!
//! Nothing here runs unless [`GateProbe::start`] is called.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::gpu::Gpu;

/// How often the probe's second thread polls the device.
pub const POLL: Duration = Duration::from_millis(1);

/// How long, once stopped, the probe still polls for the jobs it
/// submitted to be done; those that aren't by then are
/// [`ProbeReport::unfinished`].
pub const DRAIN_AT_MOST: Duration = Duration::from_secs(1);

/// The buffer each job clears: tiny, and the probe's own.
const BUFFER_BYTES: u64 = 256;

/// One job the probe submitted, once done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeSample {
    /// Right before its submit, after the submit gate let it through.
    pub submitted: Instant,
    /// From then until wgpu's work-done callback ran, within about
    /// [`POLL`] of the job's end.
    pub latency: Duration,
}

/// What a probe saw, from [`GateProbe::stop`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeReport {
    /// How often it submitted a job.
    pub period: Duration,
    /// Every job done, in the order submitted.
    pub samples: Vec<ProbeSample>,
    /// Jobs submitted and not done [`DRAIN_AT_MOST`] after the probe
    /// stopped: 0 unless the queue was held up that long, or the device
    /// was lost.
    pub unfinished: u64,
}

/// A running probe: [`GateProbe::stop`] ends it and says what it saw.
/// Dropping it stops it too, without waiting for its threads, which end
/// by themselves.
pub struct GateProbe {
    period: Duration,
    shared: Arc<Shared>,
    submitter: Option<JoinHandle<()>>,
    poller: Option<JoinHandle<Vec<ProbeSample>>>,
}

/// What the probe's threads and the callbacks share.
#[derive(Debug, Default)]
struct Shared {
    /// Asked to stop.
    stop: AtomicBool,
    /// The submitting thread has ended: no job comes after.
    submitted_all: AtomicBool,
    /// Jobs submitted whose callback hasn't run.
    pending: AtomicU64,
}

impl std::fmt::Debug for GateProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateProbe")
            .field("period", &self.period)
            .field("shared", &self.shared)
            .finish_non_exhaustive()
    }
}

impl GateProbe {
    /// Starts probing `gpu`'s queue every `period` (at least 1 ms), from
    /// now. Fails only if a thread can't be started.
    pub fn start(gpu: &Gpu, period: Duration) -> std::io::Result<GateProbe> {
        let period = period.max(Duration::from_millis(1));
        let shared = Arc::new(Shared::default());
        let (done_tx, done_rx) = mpsc::channel();
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gate probe"),
            size: BUFFER_BYTES,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let poller = {
            let (gpu, shared) = (gpu.clone(), shared.clone());
            std::thread::Builder::new()
                .name("gate-probe-poll".into())
                .spawn(move || poll(&gpu, &shared, &done_rx))?
        };
        let submitter = {
            let (gpu, shared) = (gpu.clone(), shared.clone());
            std::thread::Builder::new()
                .name("gate-probe".into())
                .spawn(move || submit(&gpu, &shared, &buffer, period, &done_tx))
        };
        let submitter = match submitter {
            Ok(s) => s,
            Err(e) => {
                shared.submitted_all.store(true, Ordering::Release);
                shared.stop.store(true, Ordering::Release);
                return Err(e);
            }
        };
        tracing::info!(
            period_ms = period.as_secs_f64() * 1e3,
            "the gate probe runs"
        );
        Ok(GateProbe {
            period,
            shared,
            submitter: Some(submitter),
            poller: Some(poller),
        })
    }

    /// How often it submits a job.
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Stops submitting, waits at most [`DRAIN_AT_MOST`] for the jobs
    /// submitted to be done, and says what it saw. It waits for the
    /// probe's threads, so never on the UI thread.
    pub fn stop(mut self) -> ProbeReport {
        spherecut_core::may_block("stopping the gate probe");
        self.shared.stop.store(true, Ordering::Release);
        if let Some(s) = self.submitter.take()
            && s.join().is_err()
        {
            tracing::warn!("the gate probe's submitting thread panicked");
        }
        let mut samples = match self.poller.take().map(JoinHandle::join) {
            Some(Ok(samples)) => samples,
            _ => {
                tracing::warn!("the gate probe's polling thread panicked");
                Vec::new()
            }
        };
        samples.sort_by_key(|s| s.submitted);
        ProbeReport {
            period: self.period,
            samples,
            unfinished: self.shared.pending.load(Ordering::Acquire),
        }
    }
}

impl Drop for GateProbe {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
    }
}

/// The `gate-probe` thread: a job every `period`, on a fixed grid from
/// its start; a slot missed while it waited at the gate, or for the
/// scheduler, is skipped rather than made up.
fn submit(
    gpu: &Gpu,
    shared: &Arc<Shared>,
    buffer: &wgpu::Buffer,
    period: Duration,
    done: &mpsc::Sender<ProbeSample>,
) {
    // Set when the thread ends, by a panic too, so that the poller does.
    struct Ended<'a>(&'a AtomicBool);
    impl Drop for Ended<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _ended = Ended(&shared.submitted_all);
    let mut next = Instant::now();
    while !shared.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
            continue;
        }
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gate probe"),
            });
        enc.clear_buffer(buffer, 0, None);
        let job = enc.finish();
        gpu.submit_gate().wait();
        shared.pending.fetch_add(1, Ordering::AcqRel);
        let submitted = Instant::now();
        gpu.queue.submit([job]);
        let (done, shared) = (done.clone(), Arc::clone(shared));
        gpu.queue.on_submitted_work_done(move || {
            let latency = submitted.elapsed();
            let _ = done.send(ProbeSample { submitted, latency });
            shared.pending.fetch_sub(1, Ordering::AcqRel);
        });
        let now = Instant::now();
        while next <= now {
            next += period;
        }
    }
}

/// The `gate-probe-poll` thread: polls the device every [`POLL`], which
/// runs the jobs' callbacks, and gathers what they send. Once the
/// submitting thread has ended, it goes on until every job is done, or
/// for [`DRAIN_AT_MOST`].
fn poll(gpu: &Gpu, shared: &Shared, done: &mpsc::Receiver<ProbeSample>) -> Vec<ProbeSample> {
    let mut samples = Vec::new();
    let mut draining: Option<Instant> = None;
    loop {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        samples.extend(done.try_iter());
        if shared.submitted_all.load(Ordering::Acquire) {
            let since = *draining.get_or_insert_with(Instant::now);
            if shared.pending.load(Ordering::Acquire) == 0 || since.elapsed() >= DRAIN_AT_MOST {
                break;
            }
        }
        std::thread::sleep(POLL);
    }
    // A callback that ran on another thread's poll since the last look.
    samples.extend(done.try_iter());
    samples
}
