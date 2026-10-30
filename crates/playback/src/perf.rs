//! The engine's numbers (M1 plan section 10): what the performance
//! overlay shows, what the smoke run writes to its JSON and what the
//! Tier 2 benchmarks report, from one source, so that they agree.
//!
//! Each [`Stage`] keeps its last [`RING`] samples in milliseconds, and
//! each [`Counter`] a running count. Every thread records into its own
//! stages (the decode worker its decode and download times, the upload
//! thread its upload times, the UI thread its frame and render times),
//! and anyone reads them, at any time. Recording and reading are atomic
//! stores and loads, never a lock: the UI thread records too, and reads
//! once a frame.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Samples kept per stage: about 4 s of UI frames at 60 Hz, 8 s of
/// video frames at 30 fps.
pub const RING: usize = 256;

/// A timed step, in milliseconds per sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Decoding a frame, without its download (decode worker).
    Decode,
    /// Copying a frame from the device (decode worker; 0 in software).
    Download,
    /// Converting a frame swscale converts (decode worker).
    Convert,
    /// Copying planes in, recording and submitting (upload thread).
    UploadCpu,
    /// Ingest and mips on the GPU, from timestamps (upload thread).
    UploadGpu,
    /// Recording the projection (UI thread).
    RenderCpu,
    /// The projection on the GPU, from timestamps.
    RenderGpu,
    /// Between one UI frame and the next (UI thread).
    UiFrame,
    /// Between one video frame shown and the next (presenter).
    ShownInterval,
    /// The clock's time less the shown frame's, signed (presenter).
    AvOffset,
}

impl Stage {
    pub const ALL: [Stage; 10] = [
        Stage::Decode,
        Stage::Download,
        Stage::Convert,
        Stage::UploadCpu,
        Stage::UploadGpu,
        Stage::RenderCpu,
        Stage::RenderGpu,
        Stage::UiFrame,
        Stage::ShownInterval,
        Stage::AvOffset,
    ];

    /// Its name in the smoke run's JSON and the benchmark reports.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Decode => "decode",
            Stage::Download => "download",
            Stage::Convert => "convert",
            Stage::UploadCpu => "upload_cpu",
            Stage::UploadGpu => "upload_gpu",
            Stage::RenderCpu => "render_cpu",
            Stage::RenderGpu => "render_gpu",
            Stage::UiFrame => "ui_frame",
            Stage::ShownInterval => "shown_interval",
            Stage::AvOffset => "av_offset",
        }
    }
}

/// A running count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Counter {
    /// Frames the decoders made.
    FramesDecoded,
    /// Frames uploaded into GPU slots.
    FramesUploaded,
    /// Video frames shown.
    FramesShown,
    /// Frames passed over between two shown ones while playing (from run
    /// links, else from gaps in pts).
    Dropped,
    /// Frames catch-up skipped in the decoder.
    DecodeDropped,
    /// UI frames over 1.5 times the median interval.
    Hitches,
    /// Seeks started, and of those, ones a newer one stopped.
    Seeks,
    SeeksSuperseded,
    /// Times a decode worker waited for room in the frame cache.
    CacheWaits,
    /// Times the audio ring ran dry while playing (slice 6).
    AudioUnderruns,
    /// Of the frames uploaded, those whose planes were imported as
    /// DMA-BUFs and copied on the GPU (slice 5b's path A), those copied
    /// out of the Vulkan decoder's images on the GPU (path B), and those
    /// left on the decoder's device that the GPU refused, which the
    /// upload thread downloaded instead.
    FramesImported,
    VulkanCopies,
    ImportsRefused,
    /// Draws of the viewer that projected its frame, and those that only
    /// encoded the projection already in its target (UI thread, section
    /// 7.3).
    Projected,
    EncodedOnly,
}

impl Counter {
    pub const ALL: [Counter; 15] = [
        Counter::FramesDecoded,
        Counter::FramesUploaded,
        Counter::FramesShown,
        Counter::Dropped,
        Counter::DecodeDropped,
        Counter::Hitches,
        Counter::Seeks,
        Counter::SeeksSuperseded,
        Counter::CacheWaits,
        Counter::AudioUnderruns,
        Counter::FramesImported,
        Counter::VulkanCopies,
        Counter::ImportsRefused,
        Counter::Projected,
        Counter::EncodedOnly,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Counter::FramesDecoded => "frames_decoded",
            Counter::FramesUploaded => "frames_uploaded",
            Counter::FramesShown => "frames_shown",
            Counter::Dropped => "dropped",
            Counter::DecodeDropped => "decode_dropped",
            Counter::Hitches => "hitches",
            Counter::Seeks => "seeks",
            Counter::SeeksSuperseded => "seeks_superseded",
            Counter::CacheWaits => "cache_waits",
            Counter::AudioUnderruns => "audio_underruns",
            Counter::FramesImported => "frames_imported",
            Counter::VulkanCopies => "vulkan_copies",
            Counter::ImportsRefused => "imports_refused",
            Counter::Projected => "projected",
            Counter::EncodedOnly => "encoded_only",
        }
    }
}

/// One stage's last samples: f32 bits in atomics, and how many were ever
/// recorded, whose remainder by [`RING`] is where the next one goes.
#[derive(Debug)]
struct Ring {
    samples: [AtomicU32; RING],
    recorded: AtomicU64,
}

impl Ring {
    fn new() -> Self {
        Ring {
            samples: std::array::from_fn(|_| AtomicU32::new(f32::NAN.to_bits())),
            recorded: AtomicU64::new(0),
        }
    }

    fn push(&self, v: f32) {
        let i = self.recorded.fetch_add(1, Ordering::AcqRel);
        self.samples[(i % RING as u64) as usize].store(v.to_bits(), Ordering::Release);
    }

    /// The samples kept, oldest first. A sample recorded while this reads
    /// may stand in for the one it replaced; one being recorded, whose
    /// slot is still empty, is left out.
    fn samples(&self) -> (u64, Vec<f32>) {
        let recorded = self.recorded.load(Ordering::Acquire);
        let kept = recorded.min(RING as u64);
        let samples = (recorded - kept..recorded)
            .map(|i| {
                f32::from_bits(self.samples[(i % RING as u64) as usize].load(Ordering::Acquire))
            })
            .filter(|v| !v.is_nan())
            .collect();
        (recorded, samples)
    }
}

/// The engine's stages and counters. Shared in an `Arc` by every thread
/// that records or reads.
#[derive(Debug)]
pub struct PerfStats {
    rings: [Ring; Stage::ALL.len()],
    counters: [AtomicU64; Counter::ALL.len()],
}

impl Default for PerfStats {
    fn default() -> Self {
        Self::new()
    }
}

impl PerfStats {
    pub fn new() -> Self {
        PerfStats {
            rings: std::array::from_fn(|_| Ring::new()),
            counters: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Record `ms` for `stage`. A NaN isn't recorded.
    pub fn record(&self, stage: Stage, ms: f32) {
        if !ms.is_nan() {
            self.rings[stage as usize].push(ms);
        }
    }

    /// Record the milliseconds since `start` for `stage`, and return them.
    pub fn record_since(&self, stage: Stage, start: Instant) -> f32 {
        let ms = start.elapsed().as_secs_f32() * 1000.0;
        self.record(stage, ms);
        ms
    }

    pub fn add(&self, counter: Counter, n: u64) {
        self.counters[counter as usize].fetch_add(n, Ordering::Relaxed);
    }

    pub fn count(&self, counter: Counter) -> u64 {
        self.counters[counter as usize].load(Ordering::Relaxed)
    }

    /// The samples `stage` keeps, oldest first: for sparklines.
    pub fn samples(&self, stage: Stage) -> Vec<f32> {
        self.rings[stage as usize].samples().1
    }

    pub fn summary(&self, stage: Stage) -> StageSummary {
        let (recorded, samples) = self.rings[stage as usize].samples();
        StageSummary::of(recorded, samples)
    }

    /// Every stage's summary and every count, read now.
    pub fn snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            stages: Stage::ALL.map(|s| self.summary(s)),
            counters: Counter::ALL.map(|c| self.count(c)),
        }
    }
}

/// A stage's samples, summed up. The percentiles are of the samples kept
/// (the last [`RING`]), by the nearest rank; `None` with none.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StageSummary {
    /// Samples ever recorded.
    pub recorded: u64,
    pub last: Option<f32>,
    pub mean: Option<f32>,
    pub p50: Option<f32>,
    pub p95: Option<f32>,
    pub p99: Option<f32>,
    pub max: Option<f32>,
}

impl StageSummary {
    fn of(recorded: u64, samples: Vec<f32>) -> StageSummary {
        let last = samples.last().copied();
        let mut sorted = samples;
        sorted.sort_by(f32::total_cmp);
        let rank = |p: f32| {
            let n = sorted.len();
            (n > 0).then(|| sorted[((p * n as f32).ceil() as usize).clamp(1, n) - 1])
        };
        let mean = (!sorted.is_empty()).then(|| {
            (sorted.iter().map(|&v| f64::from(v)).sum::<f64>() / sorted.len() as f64) as f32
        });
        StageSummary {
            recorded,
            last,
            mean,
            p50: rank(0.50),
            p95: rank(0.95),
            p99: rank(0.99),
            max: sorted.last().copied(),
        }
    }
}

/// [`PerfStats`] read at one moment, to show or write out.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PerfSnapshot {
    /// In [`Stage::ALL`]'s order.
    pub stages: [StageSummary; Stage::ALL.len()],
    /// In [`Counter::ALL`]'s order.
    pub counters: [u64; Counter::ALL.len()],
}

impl PerfSnapshot {
    pub fn stage(&self, stage: Stage) -> &StageSummary {
        &self.stages[stage as usize]
    }

    pub fn counter(&self, counter: Counter) -> u64 {
        self.counters[counter as usize]
    }
}

/// The process's memory and the GPU's, read by the upload thread once a
/// second (M1 plan section 10).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemoryStats {
    /// The resident set: `VmRSS` in `/proc/self/status`.
    pub rss: Option<u64>,
    /// Minor page faults since the process started (`/proc/self/stat`),
    /// and per second since the reading before.
    pub minor_faults: Option<u64>,
    pub minor_faults_per_s: Option<f64>,
    /// What wgpu's allocator has handed out of its blocks, and the
    /// blocks' size (`Device::generate_allocator_report`); `None` when
    /// the backend keeps no report.
    pub gpu_allocated: Option<u64>,
    pub gpu_reserved: Option<u64>,
}

/// `VmRSS` from the text of `/proc/self/status`, in bytes.
pub fn rss_of(status: &str) -> Option<u64> {
    let kb = status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?;
    let kb: u64 = kb.trim().strip_suffix("kB")?.trim_end().parse().ok()?;
    kb.checked_mul(1024)
}

/// The minor faults, the 10th field, from the text of `/proc/self/stat`.
/// The 2nd, the command's name, is in parentheses and may hold spaces
/// and parentheses itself, so the fields are counted from its last `)`.
pub fn minor_faults_of(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_ascii_whitespace().nth(7)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn the_order_of_all_is_the_order_of_the_variants() {
        for (i, s) in Stage::ALL.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
        for (i, c) in Counter::ALL.iter().enumerate() {
            assert_eq!(*c as usize, i);
        }
        let names: std::collections::BTreeSet<_> = Stage::ALL.iter().map(|s| s.name()).collect();
        assert_eq!(names.len(), Stage::ALL.len());
        let names: std::collections::BTreeSet<_> = Counter::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), Counter::ALL.len());
    }

    #[test]
    fn nothing_recorded_says_nothing() {
        let p = PerfStats::new();
        let s = p.summary(Stage::Decode);
        assert_eq!(s, StageSummary::default());
        assert!(p.samples(Stage::Decode).is_empty());
        assert_eq!(p.snapshot(), PerfSnapshot::default());
    }

    #[test]
    fn percentiles_by_nearest_rank() {
        let p = PerfStats::new();
        for v in 1..=100 {
            p.record(Stage::Decode, v as f32);
        }
        p.record(Stage::Decode, f32::NAN);
        let s = p.summary(Stage::Decode);
        assert_eq!(s.recorded, 100);
        assert_eq!(s.last, Some(100.0));
        assert_eq!(s.p50, Some(50.0));
        assert_eq!(s.p95, Some(95.0));
        assert_eq!(s.p99, Some(99.0));
        assert_eq!(s.max, Some(100.0));
        assert_eq!(s.mean, Some(50.5));
        // One sample is every percentile.
        p.record(Stage::AvOffset, -4.0);
        let s = p.summary(Stage::AvOffset);
        assert_eq!(
            (s.p50, s.p99, s.max, s.last),
            (Some(-4.0), Some(-4.0), Some(-4.0), Some(-4.0))
        );
    }

    #[test]
    fn the_ring_keeps_the_last_samples_in_order() {
        let p = PerfStats::new();
        for v in 0..(RING as u32 * 3 + 7) {
            p.record(Stage::UiFrame, v as f32);
        }
        let s = p.samples(Stage::UiFrame);
        assert_eq!(s.len(), RING);
        assert_eq!(s.first(), Some(&((RING * 2 + 7) as f32)));
        assert_eq!(s.last(), Some(&((RING * 3 + 6) as f32)));
        assert!(s.windows(2).all(|w| w[1] == w[0] + 1.0));
        assert_eq!(p.summary(Stage::UiFrame).recorded, RING as u64 * 3 + 7);
    }

    #[test]
    fn counters_add_up_across_threads() {
        let p = Arc::new(PerfStats::new());
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        p.add(Counter::FramesShown, 1);
                        p.record(Stage::Decode, 1.0);
                    }
                })
            })
            .collect();
        // Reading while they record never blocks or tears.
        for _ in 0..1000 {
            let s = p.snapshot();
            assert!(s.stage(Stage::Decode).max.is_none_or(|m| m == 1.0));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(p.count(Counter::FramesShown), 40_000);
        assert_eq!(p.snapshot().counter(Counter::FramesShown), 40_000);
        assert_eq!(p.summary(Stage::Decode).recorded, 40_000);
        assert_eq!(p.samples(Stage::Decode).len(), RING);
    }

    #[test]
    fn record_since_returns_what_it_recorded() {
        let p = PerfStats::new();
        let start = Instant::now() - std::time::Duration::from_millis(5);
        let ms = p.record_since(Stage::UploadCpu, start);
        assert!(ms >= 5.0, "{ms}");
        assert_eq!(p.summary(Stage::UploadCpu).last, Some(ms));
    }

    #[test]
    fn memory_from_procs_text() {
        let status = "Name:\tspherecut\nVmPeak:\t 9000 kB\nVmRSS:\t   2048 kB\nRssAnon:\t 1 kB\n";
        assert_eq!(rss_of(status), Some(2048 * 1024));
        assert_eq!(rss_of("Name:\tx\n"), None);
        assert_eq!(rss_of("VmRSS:\t lots kB\n"), None);
        // A name with spaces and parentheses of its own.
        let stat = "4242 (a (b) c) S 1 4242 4242 0 -1 4194560 12345 0 7 0 3 1 0 0 20 0 9 0 100";
        assert_eq!(minor_faults_of(stat), Some(12345));
        assert_eq!(minor_faults_of("4242 (x) S 1 2"), None);
        assert_eq!(minor_faults_of("no name"), None);
        // This process's own.
        let read = |p| std::fs::read_to_string(p).unwrap();
        assert!(rss_of(&read("/proc/self/status")).is_some_and(|b| b > 0));
        assert!(minor_faults_of(&read("/proc/self/stat")).is_some_and(|n| n > 0));
    }
}
