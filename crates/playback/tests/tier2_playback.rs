//! Tier 2: playback in real time through the engine (M1 plan sections
//! 12.2 and 12.3, risk 1).
//!
//! Each benchmark plays a file through a [`Player`] on the machine's GPU
//! as the GUI will: `present` and `snapshot` at 60 Hz deadlines on a
//! thread marked as the UI thread, where a wait panics, and each new
//! frame projected into an offscreen 1920×1080 target and encoded, as the
//! viewport will draw it (the raw view's two lenses side by side, 960×1080
//! each). A run is two passes, each on a player of its own:
//!
//! - **unthrottled**, on a [`ManualClock`] moved on to the next frame as
//!   soon as the last is on screen: what the engine can do;
//! - **real time**, on a [`SystemClock`], playing from the start again at
//!   the end (a seek every loop): what plays.
//!
//! Each full second of a pass gives the frames decoded (per stream) and
//! shown; the real-time pass also counts the frames the presenter passed
//! over (dropped), the frames a decoder behind the clock skipped
//! (decode-dropped), and the presents whose frame had already ended when
//! it went on screen (late). A note gives each stage's p50 and p95 over
//! the real-time pass, the loops, the UI ticks missed and where each
//! stream decoded. With `SPHERECUT_PLAYBACK_TIMELINE` naming a
//! directory, each pass also writes its [`Timeline`] there, tick by tick.
//! The cache's budget is Auto's, or `SPHERECUT_FRAME_CACHE_MIB`'s, as the
//! GUI's would be.
//!
//! What gates, and what only records:
//!
//! - `bench_playback_realtime_5k7_8bit` and `_10bit` play hw-test's 20 s
//!   perf clip on the hardware decoder [`DecoderPreference::Auto`] picks
//!   from doctor's checks, as the app plays it by default (D25): Vulkan
//!   Video's frames decoded on the renderer's own device and kept there
//!   (path B) where the device can be shared, else copied. They pass only
//!   with no drop and no decode-drop after the first second of 60 s in
//!   real time, every unthrottled second decoding at least 1.2 × 29.97
//!   frames, and every stream on that hardware, and every frame kept on
//!   the GPU where they are kept, throughout (section 12.3). Auto's copy
//!   path where that differs, the other hardware kind where doctor says
//!   it decodes the clip, and software are recorded.
//! - `bench_playback_realtime_8k_8bit`: the 8K perf clip, recorded.
//! - `bench_playback_x5_raw`: the longest file of each X5 recording mode,
//!   its two lenses as two streams: M3's 8K budget, recorded, failing
//!   only when a pass stalls (no frame for 5 s), as it does when the
//!   engine stops. Then hw-test's two-track perf clips, both tracks
//!   opened as a sample's are, unthrottled only: on Auto's hardware as
//!   the app plays it by default, and again with `SPHERECUT_ZERO_COPY=1`
//!   where that imports VA-API's frames (path A), with notes on path A
//!   against the copy and on K2's engine half (M3 plan 4.3), from the
//!   2×2880² pair at 29.97, as this machine measures it: every full
//!   second's frames decoded, the mean over the lenses, at 36 fps or
//!   more. The 2×2880² pair at 59.94 is P only, where the X5 codes every
//!   other picture as TSA_N, and a note says so beside its rows.
//! - `bench_playback_stitched_samples`: each stitched equirect among the
//!   samples, gated as the perf clips are when it is within the criterion
//!   (5.7K at 30 fps or less) and recorded otherwise; a skip saying so
//!   while there is none (clip 12 is still to come).
//! - `bench_perf_clips_software_decode`: each perf clip's software decode
//!   rate, which hw-test's report holds against "too easy" (150 fps).
//!
//! Where the GPU, hardware decode of the format, the perf clips or the
//! samples aren't there, a benchmark records what it can and skips with
//! the reason, which never counts as a pass. A run that only records and
//! fails is a note saying why, but a stall fails the benchmark.
//! `the_bench_harness_plays_on_lavapipe` runs the same harness in Tier 1,
//! in software on a small clip, with one stream and with two, and
//! `the_bench_harness_plays_both_tracks_of_a_pair` plays a 2 × 64² pair
//! as the perf pairs are played, so that neither can rot between hardware
//! runs. Run the rest with `cargo xtask hw-test`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::video::VideoPlanes;
use spherecut_core::{Clock, FrameRate, ManualClock, StopToken, SystemClock, Time};
use spherecut_media::HwDevice;
use spherecut_media::backend::{
    DecodeMode, DecodePath, Fallback, FfmpegBackend, HwKind, HwOutput, MediaBackend, NextFrame,
    Transfer, VideoFrame, VideoRequest,
};
use spherecut_media::decode::VideoDecoder;
use spherecut_media::detect::Layout;
use spherecut_media::hw::{self, GpuIds, RenderNode};
use spherecut_media::probe::{self, ProbeReport, StreamKind};
use spherecut_playback::decoder::choose;
use spherecut_playback::{
    CacheBudget, Counter, DecoderPreference, HwSupport, PerfStats, Player, PlayerConfig,
    PlayerParts, PlayerSnapshot, Presented, SURFACE_CAP, SlotFrame, Stage, StreamFormat, ZeroCopy,
};
use spherecut_render::dmabuf::{self, Importer};
use spherecut_render::vkshare::Copied;
use spherecut_render::wgpu;
use spherecut_render::{
    AdapterChoice, DebugMode, Encoder, Filter, Gpu, GpuTimers, Image, LinearTarget, Placement,
    Projector, RenderError, SlotShape, StillOptions, StillRenderer, ViewParams, upload,
};
use spherecut_sphere::DQuat;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_synth::media::{self as synth_media, Codec, Spec};
use spherecut_testkit::control::must_fail;
use spherecut_testkit::{bench, tier2};

/// One UI frame at 60 Hz.
const TICK: Duration = Duration::from_nanos(16_666_667);

/// The real-time pass of a gated run (section 12.3), and of one that
/// records.
const GATED: Duration = Duration::from_secs(60);
const RECORDED: Duration = Duration::from_secs(20);

/// The longest an unthrottled pass of a gated run goes on: long enough
/// to show its slowest second; a 20 s clip at 36 fps takes 17.
const GATED_UNTHROTTLED: Duration = Duration::from_secs(30);

/// How long a file may take to show its first frame (opening an 8K
/// stream's hardware device included), and an unthrottled pass its next.
const FIRST_FRAME: Duration = Duration::from_secs(30);
const STALL: Duration = Duration::from_secs(5);

/// What every unthrottled second of a gated run must decode: 1.2 × the
/// camera's 29.97 (section 12.3).
const GATE_FPS: f64 = 1.2 * 29.97;

/// The offscreen target, as a 1080p viewport.
const OUTPUT: (u32, u32) = (1920, 1080);

/// Where hw-test says the render node VA-API is tested on is; unset, the
/// node is the rendering GPU's own.
const RENDER_NODE_ENV: &str = "SPHERECUT_RENDER_NODE";

/// A directory each pass writes its [`Timeline`] to; unset, none is
/// kept.
const TIMELINE_ENV: &str = "SPHERECUT_PLAYBACK_TIMELINE";

/// The stages a timeline row gives, each as the samples taken since the
/// row before and the longest of them.
const TIMELINE_STAGES: [Stage; 4] = [
    Stage::Decode,
    Stage::Download,
    Stage::UploadCpu,
    Stage::UploadGpu,
];

/// This thread marked as the UI thread, as the GUI's is, until dropped: a
/// wait in the engine's calls from it panics.
struct UiThread;

impl UiThread {
    fn mark() -> UiThread {
        mark_ui_thread_with(OnBlock::Panic);
        UiThread
    }
}

impl Drop for UiThread {
    fn drop(&mut self) {
        unmark_ui_thread();
    }
}

/// The GPU a benchmark renders on, opened by `open` (`Gpu::headless`, or
/// `headless_shared` for slice 5b's path B): the adapter
/// `SPHERECUT_ADAPTER` names, else the best one there is, or a skip when
/// that is a CPU or there is none. An adapter that is there but won't
/// give a device fails the test: that is a fault to report, not missing
/// hardware.
fn hardware_gpu(open: fn(&AdapterChoice) -> Result<Gpu, RenderError>) -> Gpu {
    tier2::require_hw_run();
    let choice = AdapterChoice::from_env().unwrap_or(AdapterChoice::Auto);
    let env = AdapterChoice::ENV;
    match open(&choice) {
        Ok(gpu) if gpu.caps.is_cpu => tier2::skip(format!(
            "no GPU: the adapter {env}={choice} picks is a CPU, {}",
            gpu.caps.info.name
        )),
        Ok(gpu) => gpu,
        Err(e @ (RenderError::NoAdapter { .. } | RenderError::AdapterNotFound { .. })) => {
            tier2::skip(format!("no GPU for {env}={choice}: {e}"))
        }
        Err(e) => panic!("{env}={choice} won't open a device: {e}"),
    }
}

/// The GPU, the render node its decoders open on, and doctor's checks
/// there: made once per benchmark, and shared by its players.
struct Rig {
    gpu: Gpu,
    node: RenderNode,
    hw: HwSupport,
}

impl Rig {
    /// On the machine's GPU, decoding on its render node (or
    /// `SPHERECUT_RENDER_NODE`'s), with doctor's checks run there.
    fn hardware() -> Rig {
        let gpu = hardware_gpu(Gpu::headless);
        let info = &gpu.caps.info;
        let ids = GpuIds {
            pci_bus_id: info.device_pci_bus_id.clone(),
            vendor: info.vendor,
            device: info.device,
        };
        let given = std::env::var_os(RENDER_NODE_ENV).map(PathBuf::from);
        let node = hw::render_node(Some(&ids), given.as_deref());
        let hw = HwSupport::check(&node.path);
        Rig { gpu, node, hw }
    }

    /// The same GPU opened again with its device shared with FFmpeg's
    /// Vulkan Video (slice 5b's path B), decoding on the same node with
    /// the same checks; why not, when the device can't be shared.
    fn shared(&self) -> Result<Rig, String> {
        let gpu = hardware_gpu(Gpu::headless_shared);
        gpu.shared_video()?;
        Ok(Rig {
            gpu,
            node: self.node.clone(),
            hw: self.hw.clone(),
        })
    }

    /// Where and how the app plays `auto`, Auto's run, by default (D25):
    /// on this GPU opened again with its device shared, Vulkan Video's
    /// frames kept there (path B). Why not, when Auto picks VA-API, whose
    /// frames the app copies, or the device can't be shared: the app
    /// copies then too, as `auto` does here.
    fn as_app_plays(&self, auto: &Run) -> Result<(Rig, Run), String> {
        match auto.kind {
            Some(HwKind::Vulkan) => {}
            Some(kind) => return Err(format!("the app copies {kind}'s frames")),
            None => return Err("software's frames are copied".into()),
        }
        let shared = self
            .shared()
            .map_err(|why| format!("{} can't share its device: {why}", self.gpu.caps.info.name))?;
        let run = Run {
            label: format!("{} (Auto, zero copy)", HwKind::Vulkan),
            zero_copy: ZeroCopy::default(),
            ..auto.clone()
        };
        Ok((shared, run))
    }

    fn discrete(&self) -> bool {
        self.gpu.caps.info.device_type == wgpu::DeviceType::DiscreteGpu
    }

    fn about(&self) -> String {
        let info = &self.gpu.caps.info;
        format!(
            "rendered on {} ({} {}), decoded on {}",
            info.name, info.driver, info.driver_info, self.node
        )
    }

    fn player(&self, clock: Arc<dyn Clock>, run: &Run) -> Result<Player, String> {
        let mut parts = PlayerParts::new(
            Arc::new(FfmpegBackend::on_node(self.node.clone())),
            self.gpu.clone(),
            clock,
        );
        parts.hw = Some(self.hw.clone());
        parts.render_node = Some(self.node.path.clone());
        let config = PlayerConfig {
            decoder: run.decoder,
            zero_copy: run.zero_copy,
            cache: CacheBudget::choose(
                None,
                CacheBudget::env_value().as_deref(),
                CacheBudget::Auto,
            )
            .budget,
            ..PlayerConfig::default()
        };
        Player::new(parts, config).map_err(|e| e.to_string())
    }

    /// The hardware kind Auto picks for a stream of `format`, its frames
    /// copied, or why it picks software.
    fn auto(&self, format: &StreamFormat) -> Result<Run, String> {
        let auto = choose(
            DecoderPreference::Auto,
            Some(&self.hw),
            Some(format),
            self.discrete(),
        );
        match auto.mode {
            DecodeMode::Hardware { kind, .. } => Ok(Run {
                decoder: DecoderPreference::Auto,
                label: format!("{kind} (Auto)"),
                kind: Some(kind),
                zero_copy: ZeroCopy::Off,
            }),
            DecodeMode::Software => Err(auto.why),
        }
    }

    /// The decoders to run a stream of `format` on: the hardware kind
    /// Auto picks, or why it picks software; then the other kind where
    /// doctor says it decodes the format, and software. A kind left out
    /// gets a note under `label`.
    fn decoders(&self, format: &StreamFormat, label: &str) -> (Result<Run, String>, Vec<Run>) {
        let gated = self.auto(format);
        let picked = gated.as_ref().ok().and_then(|run| run.kind);
        let mut others = Vec::new();
        for kind in [HwKind::VaApi, HwKind::Vulkan] {
            if picked == Some(kind) {
                continue;
            }
            let check = format.check_format().unwrap_or("this format");
            if self.hw.works(check, kind) {
                others.push(Run::only(kind));
            } else {
                let why = self
                    .hw
                    .get(check, kind)
                    .and_then(|c| c.reason.clone())
                    .unwrap_or_else(|| format!("doctor didn't check {check} on {kind}"));
                bench::not_measured(&format!("{label}, {kind}"), why);
            }
        }
        others.push(Run::software());
        (gated, others)
    }
}

/// A decoder to play a file on, and what its rows are called.
#[derive(Debug, Clone)]
struct Run {
    decoder: DecoderPreference,
    label: String,
    /// The hardware it decodes on, when it is hardware.
    kind: Option<HwKind>,
    /// Which frames are left on the decoder's device and taken into the
    /// slots on the GPU (slice 5b): VA-API's imported as DMA-BUFs, Vulkan
    /// Video's decoded on the renderer's own device and copied out there;
    /// the rest are copied.
    zero_copy: ZeroCopy,
}

impl Run {
    /// On `kind` alone, copied into the slots.
    fn only(kind: HwKind) -> Run {
        Run {
            decoder: DecoderPreference::Only(kind),
            label: kind.to_string(),
            kind: Some(kind),
            zero_copy: ZeroCopy::Off,
        }
    }

    fn software() -> Run {
        Run {
            decoder: DecoderPreference::Software,
            label: "software".into(),
            kind: None,
            zero_copy: ZeroCopy::Off,
        }
    }
}

/// A target per stream, side by side in one 1920×1080 output, drawn as
/// the GUI's viewport is: projected, then encoded.
struct Screen {
    gpu: Gpu,
    projector: Projector,
    encoder: Encoder,
    targets: Vec<LinearTarget>,
    output: wgpu::TextureView,
    timers: Option<GpuTimers>,
    view: ViewParams,
}

impl Screen {
    fn new(gpu: &Gpu, streams: usize) -> Result<Screen, RenderError> {
        let n = u32::try_from(streams.max(1)).unwrap_or(1);
        let size = (OUTPUT.0 / n, OUTPUT.1);
        let targets = (0..n)
            .map(|_| LinearTarget::new(gpu, size))
            .collect::<Result<_, _>>()?;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let output = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewport"),
            size: wgpu::Extent3d {
                width: OUTPUT.0,
                height: OUTPUT.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let projector = Projector::new(gpu);
        projector.prepare(Filter::Anisotropic, DebugMode::Off);
        Ok(Screen {
            gpu: gpu.clone(),
            projector,
            encoder: Encoder::new(gpu, format),
            targets,
            output: output.create_view(&wgpu::TextureViewDescriptor::default()),
            timers: GpuTimers::new(gpu, "viewport"),
            view: ViewParams::new(
                DQuat::IDENTITY,
                Lens::new(Projection::Rectilinear, 90f64.to_radians()),
            ),
        })
    }

    /// Draw what was presented, recording the CPU's time to record and
    /// submit it and the GPU's latest.
    fn draw(&mut self, p: &Presented, perf: &PerfStats) -> Result<(), RenderError> {
        let start = Instant::now();
        let gpu = &self.gpu;
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("viewport"),
            });
        let span = self.timers.as_mut().and_then(|t| t.begin(&mut enc));
        for f in &p.frames {
            if let Some(target) = self.targets.get(f.stream) {
                self.projector.record(
                    &mut enc,
                    &f.gpu,
                    &self.view,
                    target,
                    Filter::Anisotropic,
                    DebugMode::Off,
                    None,
                )?;
            }
        }
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for (i, target) in (0u32..).zip(&self.targets) {
                self.encoder
                    .draw(&mut pass, target, [i * target.extent().0, 0]);
            }
        }
        if let (Some(t), Some(span)) = (self.timers.as_mut(), span) {
            t.end(&mut enc, span);
        }
        gpu.queue.submit([enc.finish()]);
        perf.record_since(Stage::RenderCpu, start);
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        if let Some(ms) = self.timers.as_mut().and_then(GpuTimers::poll) {
            perf.record(Stage::RenderGpu, ms);
        }
        gpu.check()
    }
}

/// One full second of a pass.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Second {
    /// Frames decoded, per stream.
    decoded: f64,
    /// New frames on screen, of the first stream.
    shown: u64,
    dropped: u64,
    decode_dropped: u64,
    late: u64,
}

/// What a pass found.
#[derive(Debug, Default)]
struct Pass {
    seconds: Vec<Second>,
    /// Over the whole pass, partial seconds included.
    shown: u64,
    decoded: f64,
    elapsed: Duration,
    loops: u64,
    /// UI ticks missed by taking longer than one.
    missed: u64,
    /// Why an unthrottled pass stopped before the end, if it did.
    stalled: Option<String>,
    /// Every sample each stage recorded, by [`Stage::name`].
    stages: BTreeMap<&'static str, Vec<f32>>,
    /// Where each stream decoded at the end, and whether it fell back.
    paths: Vec<Option<DecodePath>>,
    /// Frames taken into slots on the GPU (zero copy: imported, or
    /// copied out of FFmpeg's Vulkan images), and frames the GPU refused,
    /// with why the first was.
    imported: u64,
    refused: u64,
    refusal: Option<String>,
}

impl Pass {
    /// Each full second's decoded fps, or the whole pass's when it was
    /// shorter than a second.
    fn decoded_fps(&self) -> Vec<f64> {
        if self.seconds.is_empty() {
            let s = self.elapsed.as_secs_f64();
            return if s > 0.0 {
                vec![self.decoded / s]
            } else {
                Vec::new()
            };
        }
        self.seconds.iter().map(|s| s.decoded).collect()
    }

    fn shown_fps(&self) -> Vec<f64> {
        self.seconds.iter().map(|s| s.shown as f64).collect()
    }

    /// `of` summed over the seconds after the first.
    fn after_first(&self, of: impl Fn(&Second) -> u64) -> u64 {
        self.seconds.iter().skip(1).map(of).sum()
    }

    fn stage(&self, stage: Stage, p: f64) -> Option<f64> {
        let v: Vec<f64> = self
            .stages
            .get(stage.name())?
            .iter()
            .map(|&ms| f64::from(ms))
            .collect();
        bench::percentile(&v, p)
    }

    /// "decode 4.1/6.0, download …" in ms, for the stages with samples.
    fn stages_line(&self) -> String {
        Stage::ALL
            .iter()
            .filter_map(|&s| {
                let (p50, p95) = (self.stage(s, 50.0)?, self.stage(s, 95.0)?);
                Some(format!("{} {p50:.2}/{p95:.2}", s.name()))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn paths_line(&self) -> String {
        self.paths
            .iter()
            .enumerate()
            .map(|(i, p)| match p {
                Some(DecodePath::Hardware { kind }) => format!("stream {i} on {kind}"),
                Some(DecodePath::Software { .. }) => format!("stream {i} in software"),
                Some(DecodePath::FellBack { from, reason }) => {
                    format!("stream {i} fell back from {from} to software: {reason}")
                }
                None => format!("stream {i} never decoded"),
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Counts a pass's seconds from the engine's counters, and gathers each
/// stage's samples as they come (the engine keeps the last 256).
struct Meter {
    perf: Arc<PerfStats>,
    streams: f64,
    begun: Instant,
    second: Instant,
    /// Counts at the start of the pass, and of the second: decoded,
    /// dropped, decode-dropped.
    base: [u64; 3],
    /// Frames imported and imports refused at the start of the pass.
    imports: [u64; 2],
    at: [u64; 3],
    current: Second,
    seen: [u64; Stage::ALL.len()],
    pass: Pass,
}

impl Meter {
    fn new(perf: Arc<PerfStats>, streams: usize, now: Instant) -> Meter {
        let seen = Stage::ALL.map(|s| perf.summary(s).recorded);
        let mut m = Meter {
            perf,
            streams: streams.max(1) as f64,
            begun: now,
            second: now,
            base: [0; 3],
            imports: [0; 2],
            at: [0; 3],
            current: Second::default(),
            seen,
            pass: Pass::default(),
        };
        m.at = m.counts();
        m.base = m.at;
        m.imports = m.import_counts();
        m
    }

    fn import_counts(&self) -> [u64; 2] {
        let imported = self.perf.count(Counter::FramesImported);
        [
            imported + self.perf.count(Counter::VulkanCopies),
            self.perf.count(Counter::ImportsRefused),
        ]
    }

    fn counts(&self) -> [u64; 3] {
        [
            Counter::FramesDecoded,
            Counter::Dropped,
            Counter::DecodeDropped,
        ]
        .map(|c| self.perf.count(c))
    }

    fn gather(&mut self) {
        for (i, &s) in Stage::ALL.iter().enumerate() {
            let recorded = self.perf.summary(s).recorded;
            let new = recorded.saturating_sub(self.seen[i]);
            if new == 0 {
                continue;
            }
            self.seen[i] = recorded;
            let kept = self.perf.samples(s);
            let take = usize::try_from(new).unwrap_or(usize::MAX).min(kept.len());
            self.pass
                .stages
                .entry(s.name())
                .or_default()
                .extend_from_slice(&kept[kept.len() - take..]);
        }
    }

    /// Note a new frame on screen, and a late present.
    fn shown(&mut self) {
        self.current.shown += 1;
        self.pass.shown += 1;
    }

    fn late(&mut self) {
        self.current.late += 1;
    }

    /// Close every second that has passed by `now`.
    fn tick(&mut self, now: Instant) {
        self.gather();
        while now.saturating_duration_since(self.second) >= Duration::from_secs(1) {
            let c = self.counts();
            let s = Second {
                decoded: (c[0] - self.at[0]) as f64 / self.streams,
                dropped: c[1] - self.at[1],
                decode_dropped: c[2] - self.at[2],
                ..self.current
            };
            self.pass.seconds.push(s);
            self.at = c;
            self.current = Second::default();
            self.second += Duration::from_secs(1);
        }
    }

    fn finish(mut self, now: Instant) -> Pass {
        self.tick(now);
        let decoded = self.perf.count(Counter::FramesDecoded) - self.base[0];
        self.pass.decoded = decoded as f64 / self.streams;
        self.pass.elapsed = now.saturating_duration_since(self.begun);
        let [imported, refused] = self.import_counts();
        self.pass.imported = imported - self.imports[0];
        self.pass.refused = refused - self.imports[1];
        self.pass
    }
}

/// A pass as CSV, for a gate that fails at a moment (a loop point) or
/// for a while (as the cache fills): a row per UI tick in real time, per
/// frame shown unthrottled, with the clock, the frame on screen, the
/// engine's counters, the cache and the slots, the stages' samples since
/// the row before, and the process's minor page faults. Written to
/// `<file>-<run>-<pass>.csv` in [`TIMELINE_ENV`]'s directory.
struct Timeline {
    path: PathBuf,
    begun: Instant,
    seen: [u64; TIMELINE_STAGES.len()],
    csv: String,
}

impl Timeline {
    /// `pass` of `run` on `file`'s timeline, when [`TIMELINE_ENV`] asks
    /// for one.
    fn new(file: &Path, run: &Run, pass: &str, perf: &PerfStats, begun: Instant) -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os(TIMELINE_ENV)?);
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let name: String = format!("{stem}-{}-{pass}", run.label)
            .chars()
            .map(|c| match c {
                'a'..='z' | '0'..='9' | '-' => c,
                'A'..='Z' => c.to_ascii_lowercase(),
                _ => '_',
            })
            .collect();
        let mut csv = String::from(
            "t_ms,clock_s,shown_s,playing,at_end,loops,decoded,uploaded,shown,dropped,\
             decode_dropped,cache_waits,cache_frames,cache_ahead,cache_mib,slots_ready,\
             slots_writing,slots_free,minflt",
        );
        for s in TIMELINE_STAGES {
            csv.push_str(&format!(",{0}_n,{0}_max_ms", s.name()));
        }
        csv.push('\n');
        Some(Timeline {
            path: dir.join(format!("{name}.csv")),
            begun,
            seen: TIMELINE_STAGES.map(|s| perf.summary(s).recorded),
            csv,
        })
    }

    fn row(&mut self, now: Instant, snap: &PlayerSnapshot, loops: u64, perf: &PerfStats) {
        use std::fmt::Write as _;
        let shown = snap
            .shown
            .first()
            .copied()
            .flatten()
            .map(|f| format!("{:.4}", f.time.to_secs_f64()))
            .unwrap_or_default();
        let count = |c| snap.perf.counter(c);
        let (frames, ahead, bytes) = snap
            .cache
            .as_ref()
            .map_or((0, 0, 0), |c| (c.frames, c.ahead, c.bytes));
        let slots = &snap.slots;
        let _ = write!(
            self.csv,
            "{:.2},{:.4},{shown},{},{},{loops},{},{},{},{},{},{},{frames},{ahead},{},{},{},{},{}",
            now.saturating_duration_since(self.begun).as_secs_f64() * 1000.0,
            snap.clock.to_secs_f64(),
            u8::from(snap.playing),
            u8::from(snap.at_end),
            count(Counter::FramesDecoded),
            count(Counter::FramesUploaded),
            count(Counter::FramesShown),
            count(Counter::Dropped),
            count(Counter::DecodeDropped),
            count(Counter::CacheWaits),
            bytes >> 20,
            slots.ready,
            slots.writing,
            slots.free,
            minor_faults(),
        );
        for (seen, s) in self.seen.iter_mut().zip(TIMELINE_STAGES) {
            let recorded = perf.summary(s).recorded;
            let new = recorded.saturating_sub(*seen);
            *seen = recorded;
            let kept = perf.samples(s);
            let take = usize::try_from(new).unwrap_or(usize::MAX).min(kept.len());
            let longest = kept[kept.len() - take..]
                .iter()
                .copied()
                .fold(0.0, f32::max);
            let _ = write!(self.csv, ",{new},{longest:.2}");
        }
        self.csv.push('\n');
    }

    fn write(self) {
        let written = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&self.path, &self.csv));
        match written {
            Ok(()) => bench::note(format!("timeline: {}", self.path.display())),
            Err(e) => bench::note(format!("timeline {}: {e}", self.path.display())),
        }
    }
}

/// The process's minor page faults so far: the 10th field of
/// `/proc/self/stat`, counted from after the command's name (which may
/// hold spaces); 0 where it can't be read.
fn minor_faults() -> u64 {
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            let (_, rest) = stat.rsplit_once(')')?;
            rest.split_whitespace().nth(7)?.parse().ok()
        })
        .unwrap_or(0)
}

/// The first present with a frame of every stream, failing on an error
/// the streams report.
fn first_frame(player: &Player, streams: usize) -> Result<Presented, String> {
    let start = Instant::now();
    loop {
        if let Some(p) = player.present(Instant::now())
            && p.frames.len() == streams
        {
            return Ok(p);
        }
        stream_error(player)?;
        if start.elapsed() > FIRST_FRAME {
            return Err(format!(
                "no first frame after {FIRST_FRAME:?}: {:?}",
                player.snapshot().streams
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn stream_error(player: &Player) -> Result<(), String> {
    match player
        .snapshot()
        .streams
        .iter()
        .enumerate()
        .find_map(|(i, s)| s.error.as_ref().map(|e| (i, e.clone())))
    {
        Some((i, e)) => Err(format!("stream {i}: {e}")),
        None => Ok(()),
    }
}

/// When `f` stops being on screen.
fn ends(f: &SlotFrame, rate: Option<FrameRate>) -> Option<Time> {
    let d = f.duration.or_else(|| rate.map(FrameRate::frame_duration))?;
    f.time.checked_add(d).ok()
}

fn stream_rate(player: &Player) -> Option<FrameRate> {
    common::snapshot(player)
        .streams
        .first()
        .and_then(|s| s.info.as_ref())
        .and_then(|i| i.frame_rate())
}

/// Shut `player` down, failing when it doesn't go cleanly.
fn join(player: Player) -> Result<(), String> {
    let report = player.join(Duration::from_secs(10));
    if report.is_clean() {
        Ok(())
    } else {
        Err(format!("the player didn't shut down cleanly: {report:?}"))
    }
}

fn paths(player: &Player) -> Vec<Option<DecodePath>> {
    common::snapshot(player)
        .streams
        .iter()
        .map(|s| s.decode.clone())
        .collect()
}

/// The unthrottled pass: the clock moved on to the next frame as soon as
/// every stream shows the one it is at, through the file once or for
/// `limit`.
fn unthrottled(
    rig: &Rig,
    path: &Path,
    streams: &[Option<usize>],
    run: &Run,
    limit: Duration,
) -> Result<Pass, String> {
    let clock = Arc::new(ManualClock::new(FrameRate::FPS_30.frame_duration()));
    let player = rig.player(clock.clone(), run)?;
    player.open_streams(path.to_path_buf(), streams.to_vec());
    let pass = (|| -> Result<Pass, String> {
        let ui = UiThread::mark();
        let first = first_frame(&player, streams.len())?;
        let rate = stream_rate(&player);
        let mut shown = first.frames[0].frame;
        // Held, its frames' slots would stay out of use: with two streams,
        // two of the four, and no stream could show its next frame.
        drop(first);
        player.play();
        let mut meter = Meter::new(player.perf().clone(), streams.len(), Instant::now());
        let mut timeline = Timeline::new(path, run, "unthrottled", player.perf(), meter.begun);
        'pass: while meter.begun.elapsed() < limit {
            let Some(next) = ends(&shown, rate) else {
                return Err(format!("frame at {:?} has no duration", shown.time));
            };
            clock.set(next, Instant::now());
            let asked = Instant::now();
            loop {
                let now = Instant::now();
                if let Some(p) = player.present(now)
                    && p.frames.len() == streams.len()
                    && p.frames.iter().all(|f| ends(&f.frame, rate) > Some(next))
                {
                    shown = p.frames[0].frame;
                    meter.shown();
                    meter.tick(now);
                    if let Some(t) = &mut timeline {
                        t.row(now, &player.snapshot(), 0, player.perf());
                    }
                    break;
                }
                stream_error(&player)?;
                if player.snapshot().at_end {
                    break 'pass;
                }
                if asked.elapsed() > STALL {
                    meter.pass.stalled = Some(format!(
                        "no frame at {:.3} s after {STALL:?}",
                        next.to_secs_f64()
                    ));
                    break 'pass;
                }
                meter.tick(now);
                std::thread::sleep(Duration::from_micros(250));
            }
        }
        let mut pass = meter.finish(Instant::now());
        pass.paths = paths(&player);
        pass.refusal = player.snapshot().import_refused;
        drop(ui);
        if let Some(t) = timeline {
            t.write();
        }
        Ok(pass)
    })();
    let joined = join(player);
    let pass = pass?;
    joined?;
    Ok(pass)
}

/// The real-time pass: `present`, `snapshot` and a draw at 60 Hz
/// deadlines for `length`, playing from the start again at the end.
fn realtime(
    rig: &Rig,
    path: &Path,
    streams: &[Option<usize>],
    run: &Run,
    length: Duration,
) -> Result<Pass, String> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    let player = rig.player(clock, run)?;
    let mut screen = Screen::new(&rig.gpu, streams.len()).map_err(|e| e.to_string())?;
    player.open_streams(path.to_path_buf(), streams.to_vec());
    let pass = (|| -> Result<Pass, String> {
        let ui = UiThread::mark();
        let perf = player.perf().clone();
        let first = first_frame(&player, streams.len())?;
        screen.draw(&first, &perf).map_err(|e| e.to_string())?;
        let rate = stream_rate(&player);
        let mut last = first.frames[0].frame;
        drop(first);
        player.play();
        let start = Instant::now();
        let mut meter = Meter::new(perf.clone(), streams.len(), start);
        let mut timeline = Timeline::new(path, run, "realtime", &perf, start);
        let mut deadline = start;
        while start.elapsed() < length {
            deadline += TICK;
            let now = Instant::now();
            if now < deadline {
                std::thread::sleep(deadline - now);
            } else if now - deadline >= TICK {
                // The loop took more than a tick: start again from now.
                let behind = (now - deadline).as_nanos() / TICK.as_nanos();
                meter.pass.missed += u64::try_from(behind).unwrap_or(u64::MAX);
                deadline = now;
            }
            let now = Instant::now();
            let presented = player.present(now);
            let snap = player.snapshot();
            if let Some(t) = &mut timeline {
                t.row(now, &snap, meter.pass.loops, &perf);
            }
            if let Some(p) = &presented {
                if p.changed {
                    screen.draw(p, &perf).map_err(|e| e.to_string())?;
                }
                if let Some(f) = p.frames.first()
                    && f.frame != last
                {
                    last = f.frame;
                    meter.shown();
                }
                if snap.playing
                    && p.frames
                        .iter()
                        .any(|f| ends(&f.frame, rate).is_some_and(|e| e <= p.time))
                {
                    meter.late();
                }
            }
            if let Some((i, e)) = snap
                .streams
                .iter()
                .enumerate()
                .find_map(|(i, s)| s.error.as_ref().map(|e| (i, e)))
            {
                return Err(format!("stream {i}: {e}"));
            }
            if snap.at_end {
                player.play();
                meter.pass.loops += 1;
            }
            meter.tick(now);
        }
        let mut pass = meter.finish(Instant::now());
        pass.paths = paths(&player);
        pass.refusal = player.snapshot().import_refused;
        drop(ui);
        if let Some(t) = timeline {
            t.write();
        }
        Ok(pass)
    })();
    let joined = join(player);
    let pass = pass?;
    joined?;
    Ok(pass)
}

/// Both passes of `run` on `path`, recorded under "`clip`, `run.label`";
/// a failure is a note saying why.
fn record(
    rig: &Rig,
    clip: &str,
    path: &Path,
    streams: &[Option<usize>],
    run: &Run,
    length: Duration,
    limit: Duration,
) -> Option<(Pass, Pass)> {
    let name = format!("{clip}, {}", run.label);
    let passes = unthrottled(rig, path, streams, run, limit)
        .map_err(|e| format!("unthrottled: {e}"))
        .and_then(|u| {
            realtime(rig, path, streams, run, length)
                .map(|r| (u, r))
                .map_err(|e| format!("real time: {e}"))
        });
    let (u, r) = match passes {
        Ok(p) => p,
        Err(why) => {
            bench::not_measured(&name, why);
            return None;
        }
    };
    record_passes(clip, run, &u, &r);
    Some((u, r))
}

/// What a gated run must show (section 12.3): on `run`'s hardware
/// throughout, its frames kept on the GPU throughout where `run` keeps
/// them there, no drop or decode-drop after the first second of
/// `length`, and every unthrottled second at `fps` or more.
fn gate(name: &str, (u, r): &(Pass, Pass), run: &Run, length: Duration, fps: f64) {
    let kind = run.kind.expect("a gated run is on hardware");
    let mut failed = Vec::new();
    let on_kind = |p: &Pass| {
        p.paths
            .iter()
            .all(|d| *d == Some(DecodePath::Hardware { kind }))
    };
    if !on_kind(u) || !on_kind(r) {
        failed.push(format!(
            "not on {kind} throughout: unthrottled {}; real time {}",
            u.paths_line(),
            r.paths_line()
        ));
    }
    let (taken, refused, why) = on_gpu(u, r);
    if run.zero_copy != ZeroCopy::Off && (taken == 0 || refused > 0) {
        failed.push(format!(
            "not kept on the GPU throughout: {taken} frames taken there, {refused} refused{}",
            because(why)
        ));
    }
    let dropped = r.after_first(|s| s.dropped);
    let decode_dropped = r.after_first(|s| s.decode_dropped);
    if dropped > 0 || decode_dropped > 0 {
        failed.push(format!(
            "{dropped} dropped and {decode_dropped} decode-dropped after the first second, \
             by second: {:?}",
            r.seconds
                .iter()
                .map(|s| (s.dropped, s.decode_dropped))
                .collect::<Vec<_>>()
        ));
    }
    let full = (length.as_secs() as usize).saturating_sub(1);
    if r.seconds.len() < full {
        failed.push(format!(
            "the real-time pass ran {} full seconds of {}",
            r.seconds.len(),
            length.as_secs()
        ));
    }
    if let Some(why) = &u.stalled {
        failed.push(format!("the unthrottled pass stalled: {why}"));
    }
    let decoded = u.decoded_fps();
    let slowest = decoded.iter().copied().fold(f64::INFINITY, f64::min);
    if decoded.is_empty() || slowest < fps {
        failed.push(format!(
            "unthrottled seconds decoded {decoded:.1?} fps, the slowest under {fps:.3}"
        ));
    }
    assert!(failed.is_empty(), "{name}: {}", failed.join("; "));
}

/// A recorded run whose unthrottled pass stalled: the engine stopped
/// bringing frames up, which no speed of decoder explains.
fn stall(clip: &str, run: &Run, passes: &Option<(Pass, Pass)>) -> Option<String> {
    let (u, _) = passes.as_ref()?;
    let why = u.stalled.as_ref()?;
    Some(format!(
        "{clip}, {}: the unthrottled pass stalled: {why}",
        run.label
    ))
}

/// The video streams of a probed file, by index.
fn video_streams(r: &ProbeReport) -> Vec<usize> {
    r.streams
        .iter()
        .filter(|s| s.kind == StreamKind::Video)
        .map(|s| s.index)
        .collect()
}

fn format_of(r: &ProbeReport, index: Option<usize>) -> StreamFormat {
    StreamFormat::of(r, index)
        .unwrap_or_else(|| panic!("{}: no video stream's format", r.path.display()))
}

fn probe_file(path: &Path) -> ProbeReport {
    probe::probe(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A perf clip's path, or a skip until hw-test has made it.
fn perf_clip(name: &str) -> (Spec, PathBuf) {
    let spec = synth_media::perf_clips()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("synth has no perf clip {name}"));
    let path = tier2::require_perf_clip(&spec.file_name());
    (spec, path)
}

/// One perf clip: Auto's hardware gated when `gated`, then the rest
/// recorded; a skip when Auto picks software, after the rest.
fn play_perf_clip(name: &str, gated: bool) {
    let (_, path) = perf_clip(name);
    let rig = Rig::hardware();
    bench::note(format!("{name}: {}", rig.about()));
    let report = probe_file(&path);
    let format = format_of(&report, None);
    let (auto, mut others) = rig.decoders(&format, name);
    let streams = [None];
    let (length, limit) = if gated {
        (GATED, GATED_UNTHROTTLED)
    } else {
        (RECORDED, RECORDED)
    };
    // A gated run plays as the app does by default; where that keeps
    // the frames on the GPU, Auto's copy path is recorded with the rest.
    let mut shared = None;
    let auto = auto.map(|run| match rig.as_app_plays(&run) {
        Ok((on, kept)) if gated => {
            shared = Some(on);
            others.insert(0, run);
            kept
        }
        Err(why) if gated => {
            bench::note(format!(
                "{name}, {}: gated on the copy path: {why}",
                run.label
            ));
            run
        }
        _ => run,
    });
    let on = shared.as_ref().unwrap_or(&rig);
    let auto = auto.map(|run| {
        let passes = if gated {
            // A gated run that fails says why in the assertion.
            let u = unthrottled(on, &path, &streams, &run, limit)
                .unwrap_or_else(|e| panic!("{name}, {}: unthrottled: {e}", run.label));
            let r = realtime(on, &path, &streams, &run, length)
                .unwrap_or_else(|e| panic!("{name}, {}: real time: {e}", run.label));
            record_passes(name, &run, &u, &r);
            Some((u, r))
        } else {
            record(on, name, &path, &streams, &run, length, limit)
        };
        (run, passes)
    });
    for run in &others {
        record(&rig, name, &path, &streams, run, RECORDED, RECORDED);
    }
    match auto {
        Err(why) => tier2::skip(format!("{name}: Auto decodes in software here: {why}")),
        Ok((run, Some(passes))) if gated => gate(
            &format!("{name}, {}", run.label),
            &passes,
            &run,
            length,
            GATE_FPS,
        ),
        Ok(_) => {}
    }
}

/// The frames a run's passes took on the GPU, those the GPU refused, and
/// the first reason it gave.
fn on_gpu<'a>(u: &'a Pass, r: &'a Pass) -> (u64, u64, Option<&'a String>) {
    (
        u.imported + r.imported,
        u.refused + r.refused,
        u.refusal.as_ref().or(r.refusal.as_ref()),
    )
}

/// ` (why)`, or nothing.
fn because(why: Option<&String>) -> String {
    why.map_or(String::new(), |w| format!(" ({w})"))
}

/// [`record`]'s rows for passes already run.
fn record_passes(clip: &str, run: &Run, u: &Pass, r: &Pass) {
    let name = format!("{clip}, {}", run.label);
    bench::record_runs(
        &format!("{name}: decoded per second, unthrottled"),
        &u.decoded_fps(),
        "fps",
    );
    bench::record_runs(
        &format!("{name}: decoded per second, real time"),
        &r.decoded_fps(),
        "fps",
    );
    bench::record_runs(
        &format!("{name}: shown per second, real time"),
        &r.shown_fps(),
        "fps",
    );
    let after = |of: fn(&Second) -> u64| r.after_first(of) as f64;
    bench::record(
        &format!("{name}: dropped after 1 s"),
        after(|s| s.dropped),
        "frames",
    );
    bench::record(
        &format!("{name}: decode-dropped after 1 s"),
        after(|s| s.decode_dropped),
        "frames",
    );
    bench::record(
        &format!("{name}: late presents after 1 s"),
        after(|s| s.late),
        "presents",
    );
    bench::note(format!(
        "{name}: {:.0} s in real time, {} loops, {} UI ticks missed; {}; \
         stage p50/p95 ms: {}",
        r.elapsed.as_secs_f64(),
        r.loops,
        r.missed,
        r.paths_line(),
        r.stages_line()
    ));
    if let Some(why) = &u.stalled {
        bench::note(format!("{name}: the unthrottled pass stalled: {why}"));
    }
    if run.zero_copy != ZeroCopy::Off {
        let (n, k, why) = on_gpu(u, r);
        bench::note(format!(
            "{name}: {n} frames taken on the GPU, {k} refused{}",
            because(why)
        ));
    }
}

#[test]
#[ignore = "tier2: benchmark, needs a GPU that decodes HEVC Main and hw-test's perf clips"]
fn bench_playback_realtime_5k7_8bit() {
    play_perf_clip(synth_media::PERF_5K7_8BIT, true);
}

#[test]
#[ignore = "tier2: benchmark, needs a GPU that decodes HEVC Main 10 and hw-test's perf clips"]
fn bench_playback_realtime_5k7_10bit() {
    play_perf_clip(synth_media::PERF_5K7_10BIT, true);
}

#[test]
#[ignore = "tier2: benchmark, needs a GPU that decodes HEVC Main and hw-test's perf clips"]
fn bench_playback_realtime_8k_8bit() {
    play_perf_clip(synth_media::PERF_8K_8BIT, false);
}

const EXTENSIONS: &[&str] = &["insv", "lrv", "mp4", "mov", "360", "osv"];

/// The camera files under the samples directory, probed, sorted by path.
fn samples() -> (PathBuf, Vec<(PathBuf, ProbeReport)>) {
    let dir = tier2::require_samples_dir();
    let probed = probe_samples(&dir);
    (dir, probed)
}

/// The camera files under `dir`, probed, sorted by path.
fn probe_samples(dir: &Path) -> Vec<(PathBuf, ProbeReport)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| {
                EXTENSIONS.contains(&x.to_string_lossy().to_ascii_lowercase().as_str())
            }) {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|p| probe::probe(&p).ok().map(|r| (p, r)))
        .collect()
}

fn layout(r: &ProbeReport) -> Option<Layout> {
    r.camera.as_ref().map(|c| c.layout)
}

/// A file's first video stream: its size, its rate in fps and its length
/// in seconds.
fn shape(r: &ProbeReport) -> Option<((u32, u32), f64, f64)> {
    let s = r.streams.iter().find(|s| s.kind == StreamKind::Video)?;
    let v = s.video.as_ref()?;
    let fps = s.avg_frame_rate?.to_f64();
    let len = s.duration.map_or(0.0, |d| d.to_secs_f64());
    Some(((v.width, v.height), fps, len))
}

/// The longest file of each X5 raw mode (two lenses of a size at a
/// rate) among `probed`, by mode.
fn x5_modes(probed: Vec<(PathBuf, ProbeReport)>) -> BTreeMap<String, (PathBuf, ProbeReport)> {
    let mut modes: BTreeMap<String, (PathBuf, ProbeReport, f64)> = BTreeMap::new();
    for (p, r) in probed {
        let streams = video_streams(&r);
        if layout(&r) != Some(Layout::Insta360DualFisheye) || streams.len() != 2 {
            continue;
        }
        let Some(((w, h), fps, len)) = shape(&r) else {
            continue;
        };
        let mode = format!("X5 raw 2× {w}x{h} at {fps:.2} fps");
        if modes.get(&mode).is_none_or(|m| len > m.2) {
            modes.insert(mode, (p, r, len));
        }
    }
    modes
        .into_iter()
        .map(|(mode, (p, r, _))| (mode, (p, r)))
        .collect()
}

/// [`x5_modes`] of the samples, or a skip when there is none.
fn require_x5_modes() -> BTreeMap<String, (PathBuf, ProbeReport)> {
    let (dir, probed) = samples();
    let modes = x5_modes(probed);
    if modes.is_empty() {
        tier2::skip(format!(
            "no Insta360 dual-fisheye file with two lenses under {}",
            dir.display()
        ));
    }
    modes
}

/// What K2's engine half asks of each lens of the 2×2880² pair at 29.97
/// (M3 plan 4.3): 30 × 1.2 fps, as `bench_export_pair_decode` asks of the
/// export's pair decode.
const K2_FPS: f64 = 30.0 * 1.2;

/// Where the perf pair called `name` is coded unlike the X5's mode, for
/// a note beside its rows: the 59.94 pair is P only (M3 plan 7.2,
/// `synth::media::PERF_PAIR_P_ONLY`, which the export's pair bench and
/// hw-test's report say too).
fn coded_unlike_x5(name: &str) -> Option<&'static str> {
    (name == synth_media::PERF_PAIR_2880_5994).then_some(synth_media::PERF_PAIR_P_ONLY)
}

/// A probed file's video streams, each opened by its index, as a
/// sample's two lenses are.
fn lens_streams(r: &ProbeReport) -> Vec<Option<usize>> {
    video_streams(r).into_iter().map(Some).collect()
}

/// Both lenses of the two-track file at `path`, as `streams`, played
/// unthrottled on each of `runs` on its rig, for up to `limit`: each
/// run's rows under `clip`, and its pass, or why it didn't play, which a
/// note says too.
fn play_pair(
    clip: &str,
    path: &Path,
    streams: &[Option<usize>],
    runs: &[(&Rig, Run)],
    limit: Duration,
) -> Vec<Result<Pass, String>> {
    let mut passes = Vec::new();
    for (rig, run) in runs {
        let name = format!("{clip}, {}", run.label);
        let pass = unthrottled(rig, path, streams, run, limit);
        match &pass {
            Ok(u) => {
                bench::record_runs(
                    &format!("{name}: decoded per second, unthrottled"),
                    &u.decoded_fps(),
                    "fps",
                );
                bench::note(format!(
                    "{name}: {} frames a lens shown in {:.1} s unthrottled; {}; \
                     stage p50/p95 ms: {}",
                    u.shown + 1,
                    u.elapsed.as_secs_f64(),
                    u.paths_line(),
                    u.stages_line()
                ));
                if let Some(why) = &u.stalled {
                    bench::note(format!("{name}: the unthrottled pass stalled: {why}"));
                }
                if run.zero_copy != ZeroCopy::Off {
                    bench::note(format!(
                        "{name}: {} frames taken on the GPU, {} refused{}",
                        u.imported,
                        u.refused,
                        because(u.refusal.as_ref())
                    ));
                }
            }
            Err(why) => bench::not_measured(&name, format!("unthrottled: {why}")),
        }
        passes.push(pass);
    }
    passes
}

/// Auto's run with `SPHERECUT_ZERO_COPY=1`: VA-API's frames imported as
/// DMA-BUFs (path A), on the GPU the copy runs on. Why it doesn't apply,
/// where Auto decodes on Vulkan Video, whose frames the default already
/// keeps on the GPU where it can, or where the GPU can't import them.
fn path_a(rig: &Rig, auto: &Run) -> Result<Run, String> {
    if auto.kind != Some(HwKind::VaApi) {
        return Err(format!(
            "the same as the default: it imports only VA-API's frames, and Auto decodes on {}",
            auto.kind.map_or("software".into(), |k| k.to_string())
        ));
    }
    if !rig.gpu.caps.dmabuf_import {
        return Err(format!(
            "{} can't import DMA-BUFs: it lacks VK_EXT_external_memory_dma_buf or \
             VK_EXT_image_drm_format_modifier",
            rig.gpu.caps.info.name
        ));
    }
    Ok(Run {
        label: format!("{} (Auto, SPHERECUT_ZERO_COPY=1)", HwKind::VaApi),
        zero_copy: ZeroCopy::All,
        ..auto.clone()
    })
}

/// K2's engine half (M3 plan 4.3) from `run`'s unthrottled pass over the
/// 2×2880² pair at 29.97: decided only on VA-API with its frames copied,
/// as the plan names it. It passes when every stream decoded on VA-API
/// throughout, the pass didn't stall, and every full second decoded
/// [`K2_FPS`] frames a lens or more, as section 12.3's gate holds a
/// perf clip's seconds; the error says what fell short. A second's count
/// is the mean over the lenses, since the engine counts the frames
/// decoded of every stream together, so a lens slower than the other by
/// a few frames a second hides in it.
fn k2_engine(run: &Run, u: &Pass) -> Option<Result<String, String>> {
    if run.kind != Some(HwKind::VaApi) || run.zero_copy.dmabuf() {
        return None;
    }
    let mut short = Vec::new();
    let on_va = Some(DecodePath::Hardware {
        kind: HwKind::VaApi,
    });
    if u.paths.is_empty() || u.paths.iter().any(|d| *d != on_va) {
        short.push(format!("not on VA-API throughout: {}", u.paths_line()));
    }
    if let Some(why) = &u.stalled {
        short.push(format!("the pass stalled: {why}"));
    }
    let fps: Vec<f64> = u.seconds.iter().map(|s| s.decoded).collect();
    let slowest = fps.iter().copied().fold(f64::INFINITY, f64::min);
    if fps.is_empty() || slowest < K2_FPS {
        short.push(format!(
            "its {} full seconds decoded {fps:.1?} fps a lens (the mean over the lenses), the \
             slowest under {K2_FPS:.0}",
            fps.len()
        ));
    }
    if !short.is_empty() {
        return Some(Err(short.join("; ")));
    }
    let median = bench::percentile(&fps, 50.0).unwrap_or(f64::NAN);
    Some(Ok(format!(
        "its {} full seconds decoded {slowest:.1} fps a lens at the slowest, {median:.1} at \
         the median (the mean over the lenses), against {K2_FPS:.0}",
        fps.len()
    )))
}

/// The median of a pass's full seconds, in fps a lens.
fn median_fps(u: &Pass) -> f64 {
    bench::percentile(&u.decoded_fps(), 50.0).unwrap_or(f64::NAN)
}

/// A two-track perf clip's runs in [`bench_playback_x5_raw`] (M3 plan
/// 4.3's K2, the engine half): Auto as the app plays it by default (D25),
/// then again with `SPHERECUT_ZERO_COPY=1` where that imports the frames
/// (path A), each unthrottled; notes compare path A with the copy and,
/// on the 2×2880² pair at 29.97, give K2's verdict. Stalls go into
/// `stalls`; the clip goes into `no_hardware` when Auto decodes it in
/// software, which isn't played.
fn bench_perf_pair(
    rig: &Rig,
    clip: &str,
    path: &Path,
    k2: bool,
    stalls: &mut Vec<String>,
    no_hardware: &mut Vec<String>,
) -> Option<String> {
    let report = probe_file(path);
    let streams = lens_streams(&report);
    let format = format_of(&report, streams[0]);
    let auto = match rig.auto(&format) {
        Ok(run) => run,
        Err(why) => {
            bench::not_measured(clip, format!("Auto decodes it in software here: {why}"));
            no_hardware.push(format!("{clip}: {why}"));
            return k2.then(|| format!("not decided here: Auto decodes {clip} in software"));
        }
    };
    let shared = rig.as_app_plays(&auto);
    let mut runs: Vec<(&Rig, Run)> = Vec::new();
    match &shared {
        Ok((on, kept)) => runs.push((on, kept.clone())),
        Err(why) => {
            bench::note(format!(
                "{clip}, {}: copied, as the app plays it: {why}",
                auto.label
            ));
            runs.push((rig, auto.clone()));
        }
    }
    match path_a(rig, &auto) {
        Ok(run) => runs.push((rig, run)),
        Err(why) => bench::not_measured(&format!("{clip}, SPHERECUT_ZERO_COPY=1"), why),
    }
    let passes = play_pair(clip, path, &streams, &runs, GATED_UNTHROTTLED);
    for ((_, run), pass) in runs.iter().zip(&passes) {
        if let Ok(Pass {
            stalled: Some(why), ..
        }) = pass
        {
            stalls.push(format!(
                "{clip}, {}: the unthrottled pass stalled: {why}",
                run.label
            ));
        }
    }
    if let [Ok(copy), Ok(a)] = &passes[..] {
        let (fa, fc) = (median_fps(a), median_fps(copy));
        bench::note(format!(
            "{clip}, path A against the copy: {fa:.1} fps a lens at the median against {fc:.1}: \
             {}",
            if fa > fc {
                "path A is the faster: if zero_copy_matches_copy_path passes here, D25's \
                 revisit rule makes it the default where Vulkan Video can't decode"
            } else {
                "the copy is as fast or faster, so D25 stands"
            }
        ));
    }
    if !k2 {
        return None;
    }
    let (run, pass) = (&runs[0].1, &passes[0]);
    Some(match pass {
        Err(why) => format!("not decided: {}: {why}", run.label),
        Ok(u) => match k2_engine(run, u) {
            None => format!(
                "not decided here: Auto plays {clip} on {}, and K2 asks VA-API's with its \
                 frames copied",
                run.label
            ),
            Some(Ok(why)) => format!("{clip}, {}: Pass: {why}", run.label),
            Some(Err(why)) => format!(
                "{clip}, {}: Fail: {why}; path A's row and zero_copy_matches_copy_path here \
                 decide D25's revisit",
                run.label
            ),
        },
    })
}

/// Each X5 raw mode among the samples, its two lenses as two streams, on
/// Auto's hardware and the rest, in real time and unthrottled; then each
/// of hw-test's two-track perf clips, both tracks opened as a sample's
/// are, unthrottled ([`bench_perf_pair`]). Recorded; a stall fails it.
#[test]
#[ignore = "tier2: benchmark, needs camera samples or hw-test's perf pairs, and a GPU that decodes them"]
fn bench_playback_x5_raw() {
    tier2::require_hw_run();
    // The samples and the perf pairs each give rows of their own; with
    // neither, this skips with both reasons.
    let mut missing = Vec::new();
    let modes = match tier2::samples_dir() {
        Some(dir) => {
            let modes = x5_modes(probe_samples(&dir));
            if modes.is_empty() {
                missing.push(format!(
                    "no Insta360 dual-fisheye file with two lenses under {}",
                    dir.display()
                ));
            }
            modes
        }
        None => {
            missing.push("no samples directory (SPHERECUT_SAMPLES)".into());
            BTreeMap::new()
        }
    };
    let mut pairs = Vec::new();
    for spec in synth_media::perf_pairs() {
        let clip = format!("perf clip {}", spec.name);
        match tier2::perf_clip(&spec.file_name()) {
            Ok(path) => pairs.push((
                clip,
                path,
                spec.name == synth_media::PERF_PAIR_2880_2997,
                coded_unlike_x5(&spec.name),
            )),
            Err(why) => {
                bench::not_measured(&clip, &why);
                missing.push(why);
            }
        }
    }
    missing.dedup();
    if modes.is_empty() && pairs.is_empty() {
        tier2::skip(missing.join("; "));
    }
    let rig = Rig::hardware();
    bench::note(format!("X5 raw: {}", rig.about()));
    let mut no_hardware = Vec::new();
    let mut stalls = Vec::new();
    for (mode, (path, report)) in &modes {
        let streams = lens_streams(report);
        let format = format_of(report, streams[0]);
        bench::note(format!(
            "{mode}: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        let (auto, others) = rig.decoders(&format, mode);
        match auto {
            Ok(run) => {
                let passes = record(&rig, mode, path, &streams, &run, RECORDED, RECORDED);
                stalls.extend(stall(mode, &run, &passes));
            }
            Err(why) => no_hardware.push(format!("{mode}: {why}")),
        }
        for run in &others {
            let passes = record(&rig, mode, path, &streams, run, RECORDED, RECORDED);
            stalls.extend(stall(mode, run, &passes));
        }
    }
    let mut k2 = None;
    for (clip, path, gated, unlike) in &pairs {
        if let Some(unlike) = unlike {
            bench::note(format!("{clip}: {unlike}"));
        }
        let verdict = bench_perf_pair(&rig, clip, path, *gated, &mut stalls, &mut no_hardware);
        k2 = k2.or(verdict);
    }
    bench::note(format!(
        "K2's rule for its engine half (huginn's 5.7K30), on this machine: {}",
        k2.unwrap_or_else(|| format!(
            "not decided: perf clip {} isn't here",
            synth_media::PERF_PAIR_2880_2997
        ))
    ));
    assert!(stalls.is_empty(), "{}", stalls.join("; "));
    if no_hardware.len() == modes.len() + pairs.len() {
        tier2::skip(format!(
            "Auto decodes in software here: {}",
            no_hardware.join("; ")
        ));
    }
}

#[test]
#[ignore = "tier2: benchmark, needs a stitched equirect sample and a GPU that decodes it"]
fn bench_playback_stitched_samples() {
    let (dir, probed) = samples();
    let stitched: Vec<_> = probed
        .into_iter()
        .filter(|(_, r)| {
            matches!(
                layout(r),
                Some(Layout::EquirectTagged | Layout::EquirectLikely)
            )
        })
        .collect();
    if stitched.is_empty() {
        tier2::skip(format!(
            "no stitched equirect among the samples under {}: clip 12 is still to come \
             (docs/SAMPLES.md)",
            dir.display()
        ));
    }
    let rig = Rig::hardware();
    bench::note(format!("stitched samples: {}", rig.about()));
    let mut gated = Vec::new();
    let mut no_hardware = Vec::new();
    let mut stalls = Vec::new();
    for (path, report) in &stitched {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let format = format_of(report, None);
        let ((w, h), fps, _) = shape(report).unwrap_or(((0, 0), 0.0, 0.0));
        // Within the criterion: 5.7K at 30 fps or less (section 12.3).
        let within = u64::from(w) * u64::from(h) <= 5760 * 2880 && fps <= 30.0 && fps > 0.0;
        let (auto, mut others) = rig.decoders(&format, &name);
        match auto {
            Ok(run) => {
                let (length, limit) = if within {
                    (GATED, GATED_UNTHROTTLED)
                } else {
                    (RECORDED, RECORDED)
                };
                // Gated as the app plays it by default, as the perf clips
                // are.
                let shared;
                let (on, run) = match rig.as_app_plays(&run) {
                    Ok((s, kept)) if within => {
                        shared = s;
                        others.insert(0, run);
                        (&shared, kept)
                    }
                    Err(why) if within => {
                        bench::note(format!(
                            "{name}, {}: gated on the copy path: {why}",
                            run.label
                        ));
                        (&rig, run)
                    }
                    _ => (&rig, run),
                };
                match record(on, &name, path, &[None], &run, length, limit) {
                    Some(passes) if within => gated.push((name.clone(), run, passes, fps)),
                    None if within => panic!("{name}, {}: see the note", run.label),
                    _ => {}
                }
            }
            Err(why) => no_hardware.push(format!("{name}: {why}")),
        }
        for run in &others {
            let passes = record(&rig, &name, path, &[None], run, RECORDED, RECORDED);
            stalls.extend(stall(&name, run, &passes));
        }
    }
    assert!(stalls.is_empty(), "{}", stalls.join("; "));
    for (name, run, passes, fps) in &gated {
        gate(
            &format!("{name}, {}", run.label),
            passes,
            run,
            GATED,
            1.2 * fps,
        );
    }
    if no_hardware.len() == stitched.len() {
        tier2::skip(format!(
            "Auto decodes in software here: {}",
            no_hardware.join("; ")
        ));
    }
}

/// Frames the zero-copy check decodes of each clip: more than the
/// decoder's surfaces beyond [`SURFACE_CAP`], so that the pool runs
/// round while frames are held.
const ZERO_COPY_FRAMES: usize = 48;

/// The views the zero-copy check renders each frame through: magnified,
/// which reads the slot's level 0, and a 360° fisheye, which minifies and
/// reads its mips.
fn zero_copy_views() -> [(ViewParams, (u32, u32)); 2] {
    [
        (
            ViewParams::new(
                Ypr::from_degrees(30.0, 10.0, 0.0).to_quat(),
                Lens::new(Projection::Rectilinear, 60f64.to_radians()),
            ),
            (1280, 720),
        ),
        (
            ViewParams::new(
                DQuat::IDENTITY,
                Lens::new(Projection::Fisheye, 360f64.to_radians()),
            ),
            (720, 720),
        ),
    ]
}

/// The zero-copy path a check takes.
enum OnDevice {
    /// VA-API's frames exported as DMA-BUFs and imported (path A).
    DmaBuf,
    /// Vulkan Video's frames decoded on the renderer's own device, through
    /// FFmpeg's context on it, and copied out there (path B).
    Shared(Arc<HwDevice>),
}

impl OnDevice {
    fn decoder(&self) -> &'static str {
        match self {
            OnDevice::DmaBuf => "VA-API",
            OnDevice::Shared(_) => "Vulkan Video",
        }
    }

    /// What a frame taken this way is called.
    fn taken(&self) -> &'static str {
        match self {
            OnDevice::DmaBuf => "the import",
            OnDevice::Shared(_) => "the copy out of FFmpeg's image",
        }
    }
}

/// Why a clip's frames couldn't be compared: the decoder or the GPU
/// turned zero copy down here.
enum NotCompared {
    /// The decoder won't decode the stream.
    Decode(String),
    /// It decoded it but didn't leave the frame on its device.
    Export(String),
    /// The GPU wouldn't take the frame: its driver refused the planes, or
    /// the copier the image.
    Import(String),
}

/// A frame taken on the GPU, as the upload thread takes it.
enum Taken {
    Imported(Vec<wgpu::Texture>),
    Copied(Copied),
}

/// `frame` taken on `gpu` as `how` takes it.
fn take(gpu: &Gpu, how: &OnDevice, frame: &VideoFrame) -> Result<Taken, NotCompared> {
    Ok(match how {
        OnDevice::DmaBuf => Taken::Imported(import_planes(gpu, frame)?),
        OnDevice::Shared(_) => Taken::Copied(copy_planes(gpu, frame)?),
    })
}

/// `frame`, taken as `taken`, rendered at `view`.
fn render_taken(
    gpu: &Gpu,
    renderer: &mut StillRenderer,
    frame: &VideoFrame,
    taken: &Taken,
    view: &ViewParams,
    size: (u32, u32),
    opts: &StillOptions,
) -> Result<Image, RenderError> {
    let views = std::slice::from_ref(view);
    match taken {
        Taken::Imported(planes) => renderer.render_imported(gpu, frame, planes, views, size, opts),
        Taken::Copied(c) => {
            renderer.render_copied(gpu, frame, &c.buffer, &c.planes, views, size, opts)
        }
    }
}

/// What a zero-copy render `got` that differs from the copy's `want`
/// says about why: how much differs; whether the same take, rendered
/// again, and a fresh take render as `got` or as `want`; and whether
/// the frame downloads now as it did before (if not, its surface changed
/// under the frame that holds it).
#[allow(clippy::too_many_arguments)]
fn mismatch(
    gpu: &Gpu,
    renderer: &mut StillRenderer,
    how: &OnDevice,
    frame: &VideoFrame,
    taken: &Taken,
    got: &Image,
    want: &Image,
    view: &ViewParams,
    size: (u32, u32),
    opts: &StillOptions,
) -> String {
    let (g, w) = (got.unit_rgb(), want.unit_rgb());
    let differ = g.iter().zip(&w).filter(|(a, b)| a != b).count();
    let most = g
        .iter()
        .zip(&w)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max);
    let like = |image: Result<Image, String>| match image {
        Ok(i) if i == *want => "as the copy".to_string(),
        Ok(i) if i == *got => "as the first time".to_string(),
        Ok(_) => "a third way".to_string(),
        Err(e) => format!("not at all ({e})"),
    };
    let again = like(
        render_taken(gpu, renderer, frame, taken, view, size, opts).map_err(|e| e.to_string()),
    );
    let fresh = like(match take(gpu, how, frame) {
        Ok(t) => {
            render_taken(gpu, renderer, frame, &t, view, size, opts).map_err(|e| e.to_string())
        }
        Err(NotCompared::Decode(e) | NotCompared::Export(e) | NotCompared::Import(e)) => Err(e),
    });
    let downloaded = like(match frame.to_system() {
        Some(Ok(copy)) => renderer
            .render(gpu, &copy, view, size, opts)
            .map_err(|e| e.to_string()),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("not on its device".into()),
    });
    format!(
        "{differ} of {} channel values differ, by {most:.4} at most; rendered again it \
         renders {again}, taken again {fresh}, and downloaded again {downloaded}",
        w.len()
    )
}

/// `frame`'s planes imported into `gpu`, as `frame --zero-copy` and the
/// upload thread import them.
fn import_planes(gpu: &Gpu, frame: &VideoFrame) -> Result<Vec<wgpu::Texture>, NotCompared> {
    let layers = frame
        .dmabuf()
        .ok_or_else(|| NotCompared::Export(format!("the frame came as {:?}", frame.transfer)))?;
    let whole = SlotShape::of(frame.layout(), frame.size())
        .map_err(|e| NotCompared::Import(e.to_string()))?;
    let planes = dmabuf::plan(&whole, layers).map_err(NotCompared::Import)?;
    Importer::new().import(gpu, &planes).map_err(refused)
}

/// `frame`'s planes copied out of its Vulkan image on `gpu`, waited for
/// and ordered before wgpu's next submit, as `frame --zero-copy --decoder
/// vulkan` and the upload thread copy them.
fn copy_planes(gpu: &Gpu, frame: &VideoFrame) -> Result<Copied, NotCompared> {
    let picture = frame
        .vulkan()
        .ok_or_else(|| NotCompared::Export(format!("the frame came as {:?}", frame.transfer)))?;
    let video = gpu.shared_video().map_err(NotCompared::Import)?;
    let regions = upload::shown_regions(frame, &Placement::FULL).map_err(refused)?;
    let copied = video.copier.copy(picture, &regions).map_err(refused)?;
    video.copier.wait(&copied).map_err(refused)?;
    video.copier.order_before(&gpu.queue, &copied);
    Ok(copied)
}

/// A refusal of the GPU's as one; any other error fails the test.
fn refused(e: RenderError) -> NotCompared {
    match e {
        RenderError::Import(why) => NotCompared::Import(why),
        e => panic!("taking a frame on the GPU: {e}"),
    }
}

/// Decodes `count` frames of `stream` of `path` on the device `how` says,
/// leaving them there, and renders each through the copy (the frame
/// downloaded as it comes) and, [`SURFACE_CAP`] − 1 frames later, as the
/// GPU takes it, holding the frames in between as the frame cache would:
/// the two renders must be the same bytes. A surface the decoder took
/// back while it was held would show a later picture. Returns the frames
/// compared.
fn compare_zero_copy(
    rig: &Rig,
    how: &OnDevice,
    path: &Path,
    stream: Option<usize>,
    count: usize,
) -> Result<usize, NotCompared> {
    let backend = FfmpegBackend::on_node(rig.node.clone());
    let (kind, device, output) = match how {
        OnDevice::DmaBuf => (HwKind::VaApi, None, HwOutput::DrmPrime),
        OnDevice::Shared(dev) => (HwKind::Vulkan, Some(dev.clone()), HwOutput::Vulkan),
    };
    let req = VideoRequest {
        stream,
        decode: DecodeMode::Hardware {
            kind,
            device,
            fallback: Fallback::Fail,
        },
        output,
        ..VideoRequest::default()
    };
    let mut decoder = backend
        .open_video(path, &req)
        .map_err(|e| NotCompared::Decode(e.to_string()))?;
    let views = zero_copy_views();
    let opts = StillOptions::default();
    let mut renderer = StillRenderer::new(&rig.gpu);
    // Each held frame with its copy path's renders.
    let mut held: VecDeque<(usize, Arc<VideoFrame>, Vec<Image>)> = VecDeque::new();
    let mut compared = 0;
    let mut moved = false;
    let mut last: Option<Vec<Image>> = None;
    let mut check = |renderer: &mut StillRenderer,
                     (k, frame, want): (usize, Arc<VideoFrame>, Vec<Image>)|
     -> Result<(), NotCompared> {
        let taken = take(&rig.gpu, how, &frame)?;
        for (i, (view, size)) in views.iter().enumerate() {
            let got = render_taken(&rig.gpu, renderer, &frame, &taken, view, *size, &opts)
                .unwrap_or_else(|e| panic!("frame {k}: rendering {}: {e}", how.taken()));
            if got != want[i] {
                let why = mismatch(
                    &rig.gpu, renderer, how, &frame, &taken, &got, &want[i], view, *size, &opts,
                );
                panic!(
                    "{}: frame {k}, view {i}: {} renders other bytes than the copy: {why}",
                    path.display(),
                    how.taken()
                );
            }
        }
        compared += 1;
        Ok(())
    };
    for k in 0..count {
        let frame = match decoder
            .next_frame(&StopToken::never())
            .map_err(|e| NotCompared::Decode(e.to_string()))?
        {
            NextFrame::Frame(f) => f,
            NextFrame::End => break,
            NextFrame::Stopped => panic!("never stopped"),
        };
        if let Transfer::Downloaded { not_exported } = &frame.transfer {
            return Err(NotCompared::Export(
                not_exported
                    .clone()
                    .unwrap_or_else(|| "no reason given".into()),
            ));
        }
        let copied = frame
            .to_system()
            .expect("a frame on its device")
            .unwrap_or_else(|e| panic!("frame {k}: downloading it: {e}"));
        let want: Vec<Image> = views
            .iter()
            .map(|(view, size)| {
                renderer
                    .render(&rig.gpu, &copied, view, *size, &opts)
                    .unwrap_or_else(|e| panic!("frame {k}: rendering the copy: {e}"))
            })
            .collect();
        moved |= last.as_ref().is_some_and(|l| *l != want);
        last = Some(want.clone());
        held.push_back((k, frame, want));
        if held.len() == SURFACE_CAP {
            check(&mut renderer, held.pop_front().expect("held"))?;
        }
    }
    while let Some(h) = held.pop_front() {
        check(&mut renderer, h)?;
    }
    // Otherwise a frame from the wrong surface would match too.
    assert!(moved, "{}: every frame renders the same", path.display());
    Ok(compared)
}

/// The zero-copy checks' clips: synthetic HEVC Main and Main 10 (NV12
/// and P010) written to `dir`, and an X5 lens when the samples are there.
fn zero_copy_clips(dir: &Path) -> Vec<(String, PathBuf, Option<usize>)> {
    let mut clips: Vec<(String, PathBuf, Option<usize>)> = [("hevc-main", 8), ("hevc-main10", 10)]
        .into_iter()
        .map(|(name, bits)| {
            let spec = Spec {
                name: name.into(),
                width: 1920,
                height: 960,
                frames: ZERO_COPY_FRAMES as u32,
                codec: Codec::Hevc,
                bits,
                spherical: true,
                ..Spec::default()
            };
            (
                name.to_owned(),
                synth_media::write(&spec, dir).unwrap(),
                None,
            )
        })
        .collect();
    let samples = tier2::samples_dir().map(|d| x5_modes(probe_samples(&d)));
    match samples.as_ref().and_then(|m| m.iter().next()) {
        Some((mode, (path, report))) => {
            clips.push((
                mode.clone(),
                path.clone(),
                video_streams(report).first().copied(),
            ));
        }
        None => eprintln!("no X5 raw sample: synthetic clips only"),
    }
    clips
}

/// [`compare_zero_copy`] over the clips of [`zero_copy_clips`], `how`
/// says; a skip naming each clip left out, and why, never a pass.
fn compare_clips(rig: &Rig, how: &OnDevice) {
    eprintln!("{}", rig.about());
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("tier2-zero-copy-{}", std::process::id()));
    let clips = zero_copy_clips(&dir);
    let decoder = how.decoder();
    let mut left_out = Vec::new();
    for (name, path, stream) in &clips {
        match compare_zero_copy(rig, how, path, *stream, ZERO_COPY_FRAMES) {
            Ok(n) => eprintln!(
                "{name}: {n} frames, {} byte-identical to the copy",
                how.taken()
            ),
            Err(NotCompared::Decode(why)) => left_out.push(format!("{name}: {decoder}: {why}")),
            Err(NotCompared::Export(why)) => left_out.push(format!(
                "{name}: {decoder} didn't leave it on the device: {why}"
            )),
            Err(NotCompared::Import(why)) => {
                left_out.push(format!("{name}: the GPU won't take it: {why}"))
            }
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    if !left_out.is_empty() {
        tier2::skip(format!(
            "{} of {} clips not compared: {}",
            left_out.len(),
            clips.len(),
            left_out.join("; ")
        ));
    }
}

/// Zero copy against the copy (slice 5b): synthetic HEVC Main and Main
/// 10 clips (NV12 and P010), and an X5 lens when the samples are there,
/// each frame byte-identical through its import and its download, with
/// as many frames held as the frame cache holds. Skips, with the reason,
/// when the GPU can't import, or VA-API won't decode or export a clip,
/// or the driver refuses its planes; never a pass with a clip left out.
#[test]
#[ignore = "tier2: needs a VA-API GPU that imports DMA-BUFs"]
fn zero_copy_matches_copy_path() {
    let rig = Rig::hardware();
    if !rig.gpu.caps.dmabuf_import {
        tier2::skip(format!(
            "{} can't import DMA-BUFs: it lacks VK_EXT_external_memory_dma_buf or \
             VK_EXT_image_drm_format_modifier",
            rig.gpu.caps.info.name
        ));
    }
    compare_clips(&rig, &OnDevice::DmaBuf);
}

/// Path B against the copy (slice 5b): the clips of
/// [`zero_copy_matches_copy_path`] decoded by Vulkan Video on the
/// renderer's own device, each frame byte-identical through the GPU's
/// copy out of FFmpeg's image and through its download, as many frames
/// held as the frame cache holds. Skips, with the reason, when the device
/// can't be shared or FFmpeg won't take it, or Vulkan Video won't decode
/// a clip, or the copier refuses its frames; never a pass with a clip
/// left out.
#[test]
#[ignore = "tier2: needs a GPU with Vulkan Video decode"]
fn shared_vulkan_matches_copy_path() {
    let plain = Rig::hardware();
    let name = plain.gpu.caps.info.name.clone();
    let rig = plain
        .shared()
        .unwrap_or_else(|why| tier2::skip(format!("{name} can't share its device: {why}")));
    drop(plain);
    let video = rig.gpu.shared_video().expect("a shared device");
    let device = HwDevice::vulkan_shared(&video.handles)
        .unwrap_or_else(|e| tier2::skip(format!("FFmpeg won't decode on {name}'s device: {e}")));
    compare_clips(&rig, &OnDevice::Shared(Arc::new(device)));
}

/// A run's real-time upload stages, p50 and p99.
fn record_uploads(clip: &str, run: &Run, r: &Pass) {
    let name = format!("{clip}, {}", run.label);
    for stage in [Stage::UploadCpu, Stage::UploadGpu] {
        for p in [50.0, 99.0] {
            if let Some(ms) = r.stage(stage, p) {
                bench::record(&format!("{name}: {} p{p}", stage.name()), ms, "ms");
            }
        }
    }
}

/// Each X5 raw mode through VA-API with the copy, VA-API with zero copy,
/// Vulkan Video with the copy and Vulkan Video with zero copy (path B, on
/// the renderer's device opened shared), where doctor says each decodes
/// the mode: decoded and shown fps, drops, and the upload stages' p50 and
/// p99, the rows D25's choice before M3 takes. Recorded; a stall fails
/// it. It skips when the GPU can't import or share its device, and when
/// a frame was refused, saying why.
#[test]
#[ignore = "tier2: benchmark, needs camera samples and a GPU that imports DMA-BUFs or shares its device"]
fn bench_x5_8k30_zero_copy_vs_copy() {
    let modes = require_x5_modes();
    let rig = Rig::hardware();
    let name = &rig.gpu.caps.info.name;
    // Each zero-copy path's rig, or why it can't run here.
    let dmabuf: Result<&Rig, String> = if rig.gpu.caps.dmabuf_import {
        Ok(&rig)
    } else {
        Err(format!(
            "{name} can't import DMA-BUFs: it lacks VK_EXT_external_memory_dma_buf or \
             VK_EXT_image_drm_format_modifier"
        ))
    };
    let shared = rig
        .shared()
        .map_err(|why| format!("{name} can't share its device: {why}"));
    bench::note(format!("X5 zero copy against the copy: {}", rig.about()));
    let mut stalls = Vec::new();
    let mut refused = Vec::new();
    // Frames each path took on the GPU, VA-API's then Vulkan Video's.
    let mut taken = [0u64; 2];
    for (mode, (path, report)) in &modes {
        let streams: Vec<Option<usize>> = video_streams(report).into_iter().map(Some).collect();
        let format = format_of(report, streams[0]);
        let check = format.check_format().unwrap_or("this format");
        let mut runs = Vec::new();
        for kind in [HwKind::VaApi, HwKind::Vulkan] {
            if !rig.hw.works(check, kind) {
                let why = rig
                    .hw
                    .get(check, kind)
                    .and_then(|c| c.reason.clone())
                    .unwrap_or_else(|| format!("doctor didn't check {check} on {kind}"));
                bench::not_measured(&format!("{mode}, {kind}"), why);
                continue;
            }
            runs.push((&rig, Run::only(kind)));
            let on = match kind {
                HwKind::VaApi => dmabuf.clone(),
                HwKind::Vulkan => shared.as_ref().map_err(String::clone),
            };
            let label = format!("{kind}, zero copy");
            match on {
                Ok(on) => runs.push((
                    on,
                    Run {
                        label,
                        zero_copy: ZeroCopy::All,
                        ..Run::only(kind)
                    },
                )),
                Err(why) => bench::not_measured(&format!("{mode}, {label}"), why),
            }
        }
        for (on, run) in &runs {
            let passes = record(on, mode, path, &streams, run, RECORDED, RECORDED);
            stalls.extend(stall(mode, run, &passes));
            let Some((u, r)) = &passes else {
                continue;
            };
            record_uploads(mode, run, r);
            if run.zero_copy != ZeroCopy::Off {
                let (n, k, why) = on_gpu(u, r);
                if k > 0 || n == 0 {
                    refused.push(format!(
                        "{mode}, {}: {n} taken, {k} refused: {}",
                        run.label,
                        why.map_or("no frame was tried", String::as_str)
                    ));
                }
                taken[usize::from(run.kind == Some(HwKind::Vulkan))] += n;
            }
        }
    }
    assert!(stalls.is_empty(), "{}", stalls.join("; "));
    let mut short = refused;
    for (path, (kind, on)) in [
        (HwKind::VaApi, dmabuf.map(|_| ())),
        (HwKind::Vulkan, shared.map(|_| ())),
    ]
    .into_iter()
    .enumerate()
    {
        match on {
            Err(why) => short.push(why),
            Ok(()) if taken[path] == 0 => {
                short.push(format!("{kind} took no X5 mode's frames on the GPU here"))
            }
            Ok(()) => {}
        }
    }
    if !short.is_empty() {
        tier2::skip(format!(
            "zero copy didn't take every frame: {}",
            short.join("; ")
        ));
    }
}

/// How fast software decodes up to `frames` of `path`'s stream `stream`
/// (FFmpeg's best with `None`), or for `limit`.
fn software_fps(
    path: &Path,
    stream: Option<usize>,
    frames: usize,
    limit: Duration,
) -> Result<f64, String> {
    let mut d = VideoDecoder::open(path, stream, None).map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut n = 0;
    while n < frames && start.elapsed() < limit {
        match d.next_frame().map_err(|e| e.to_string())? {
            Some(_) => n += 1,
            None => break,
        }
    }
    Ok(n as f64 / start.elapsed().as_secs_f64())
}

#[test]
#[ignore = "tier2: benchmark, needs hw-test's perf clips"]
fn bench_perf_clips_software_decode() {
    tier2::require_hw_run();
    // A clip hw-test couldn't make leaves the others to measure; with
    // none, this skips with the reason.
    // A two-track clip is measured a lens at a time, so that hw-test
    // flags each track on its own.
    let mut missing = None;
    let mut found = 0;
    let clips = synth_media::perf_clips()
        .into_iter()
        .map(|s| (s.file_name(), s.name, s.frames, vec![None]));
    let pairs = synth_media::perf_pairs().into_iter().map(|p| {
        (
            p.file_name(),
            p.name,
            p.lenses[0].frames,
            vec![Some(0), Some(1)],
        )
    });
    for (file, clip, frames, streams) in clips.chain(pairs) {
        let names: Vec<String> = streams
            .iter()
            .map(|s| match s {
                None => format!("perf clip {clip}: software decode"),
                Some(k) => format!("perf clip {clip} lens {k}: software decode"),
            })
            .collect();
        let path = match tier2::perf_clip(&file) {
            Ok(path) => path,
            Err(why) => {
                for name in &names {
                    bench::not_measured(name, &why);
                }
                missing = Some(why);
                continue;
            }
        };
        found += 1;
        let frames = usize::try_from(frames).unwrap_or(usize::MAX).min(300);
        for (name, stream) in names.iter().zip(streams) {
            let measured = bench::try_measure(name, "fps", || {
                software_fps(&path, stream, frames, Duration::from_secs(10))
            });
            if let Err(why) = measured {
                bench::not_measured(name, why);
            }
        }
    }
    if let (0, Some(why)) = (found, missing) {
        tier2::skip(why);
    }
}

/// The harness of the benchmarks above, in Tier 1: a second of 30 fps
/// played through both passes on lavapipe in software, on a thread marked
/// as the UI thread. It checks what the harness counts, not how fast:
/// every frame of the unthrottled pass came up, the real-time pass looped
/// and counted its seconds, and the stages were gathered.
#[test]
fn the_bench_harness_plays_on_lavapipe() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("tier2-playback-{}", std::process::id()));
    let path = synth_media::write(
        &Spec {
            name: "harness".into(),
            width: 256,
            height: 128,
            codec: Codec::H264,
            frames: 30,
            ..Spec::default()
        },
        &dir,
    )
    .unwrap();
    let rig = Rig {
        gpu: common::gpu().clone(),
        node: hw::render_node(None, None),
        hw: HwSupport::default(),
    };
    let run = Run::software();
    let u = unthrottled(&rig, &path, &[None], &run, Duration::from_secs(60)).unwrap();
    let r = realtime(&rig, &path, &[None], &run, Duration::from_millis(2500)).unwrap();
    // Two streams, as an X5's two lenses: the stream twice. Each needs a
    // slot on screen and one for its next frame, all four of them.
    let both = [Some(0), Some(0)];
    let u2 = unthrottled(&rig, &path, &both, &run, Duration::from_secs(60)).unwrap();
    let r2 = realtime(&rig, &path, &both, &run, Duration::from_millis(2500)).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(u2.stalled, None, "two streams");
    assert_eq!(u2.shown, 29, "two streams");
    assert!(r2.shown > 0, "two streams");
    eprintln!(
        "unthrottled: {} shown in {:?}, {:?} fps; real time: {:?}, {} loops, {} missed; {}",
        u.shown,
        u.elapsed,
        u.decoded_fps(),
        r.seconds,
        r.loops,
        r.missed,
        r.stages_line()
    );
    assert_eq!(u.stalled, None);
    // The first frame is shown before the pass starts.
    assert_eq!(u.shown, 29);
    assert!(!u.decoded_fps().is_empty());
    assert!(r.seconds.len() >= 2, "{:?}", r.seconds);
    assert!(r.loops >= 1, "a second of footage in 2.5 s never looped");
    assert!(r.shown > 0);
    for p in [&u, &r] {
        assert!(
            p.paths
                .iter()
                .all(|d| matches!(d, Some(DecodePath::Software { .. }))),
            "{}",
            p.paths_line()
        );
    }
    // A second of footage fits in the cache, so the loops are decoded
    // once, before the pass: decode's samples may all come before it.
    for stage in [Stage::UploadCpu, Stage::RenderCpu, Stage::UiFrame] {
        assert!(
            r.stage(stage, 50.0).is_some(),
            "no {} samples: {}",
            stage.name(),
            r.stages_line()
        );
    }
}

/// [`play_pair`], the perf pairs' half of `bench_playback_x5_raw`, in
/// Tier 1: pair64-matched's two tracks, opened as a sample's lenses are,
/// played unthrottled on lavapipe in software to the end of both. The
/// control is pair64-lens-short, whose lens 1 is a frame short: were the
/// second stream lens 0 again, it would play to the end as well. It
/// stalls instead, for 5 s.
#[test]
fn the_bench_harness_plays_both_tracks_of_a_pair() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("tier2-playback-pair-{}", std::process::id()));
    let rig = Rig {
        gpu: common::gpu().clone(),
        node: hw::render_node(None, None),
        hw: HwSupport::default(),
    };
    let play = |name: &str| {
        let spec = synth_media::pairs_64()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap();
        let path = synth_media::write_pair(&spec, &dir).unwrap();
        let streams = lens_streams(&probe_file(&path));
        let runs = [(&rig, Run::software())];
        let passes = play_pair(name, &path, &streams, &runs, Duration::from_secs(60));
        let [pass] = <[_; 1]>::try_from(passes).unwrap();
        (path, streams, pass)
    };
    // Every frame of both lenses after the first came up, each lens
    // decoded in software.
    let whole = |p: &Pass| {
        p.stalled.is_none()
            && p.shown == 29
            && p.paths.len() == 2
            && p.paths
                .iter()
                .all(|d| matches!(d, Some(DecodePath::Software { .. })))
    };
    let (path, streams, matched) = play(synth_media::PAIR_64_MATCHED);
    let (_, _, short) = play(synth_media::PAIR_64_LENS_SHORT);
    // The bench's own rows end before any pass where Auto decodes in
    // software, as it does with no doctor's checks.
    let (mut stalls, mut no_hardware) = (Vec::new(), Vec::new());
    let k2 = bench_perf_pair(&rig, "pair64", &path, true, &mut stalls, &mut no_hardware);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        k2.as_deref(),
        Some("not decided here: Auto decodes pair64 in software")
    );
    assert_eq!((stalls.len(), no_hardware.len()), (0, 1), "{no_hardware:?}");
    assert_eq!(streams, [Some(0), Some(1)]);
    let matched = matched.unwrap();
    assert!(
        whole(&matched),
        "{} shown, stalled {:?}; {}",
        matched.shown,
        matched.stalled,
        matched.paths_line()
    );
    assert!(!matched.decoded_fps().is_empty());
    must_fail(
        "a pair whose lens 1 is a frame short",
        short.as_ref().is_ok_and(whole),
    );
}

/// K2's engine half holds VA-API's copied pass to 36 fps a lens in every
/// full second, on VA-API throughout and without a stall, and decides
/// nothing on any other run.
#[test]
fn k2s_engine_half_holds_every_second_of_va_api_copied_to_36_a_lens() {
    let va = Run {
        label: "VA-API (Auto)".into(),
        decoder: DecoderPreference::Auto,
        ..Run::only(HwKind::VaApi)
    };
    let on_va = Some(DecodePath::Hardware {
        kind: HwKind::VaApi,
    });
    let pass = |fps: &[f64]| Pass {
        seconds: fps
            .iter()
            .map(|&decoded| Second {
                decoded,
                ..Second::default()
            })
            .collect(),
        paths: vec![on_va.clone(); 2],
        ..Pass::default()
    };
    let fast = pass(&[41.0, 38.5, 36.0, 40.0]);
    let why = k2_engine(&va, &fast).unwrap().unwrap();
    assert!(
        why.contains(
            "4 full seconds decoded 36.0 fps a lens at the slowest, 38.5 at the median (the mean \
             over the lenses)"
        ),
        "{why}"
    );
    // The controls: each shortfall fails it.
    let slow = must_fail(
        "a second under 36",
        k2_engine(&va, &pass(&[41.0, 35.9, 40.0])).unwrap(),
    );
    assert!(slow.contains("35.9"), "{slow}");
    let fell = Pass {
        paths: vec![on_va.clone(), Some(DecodePath::Software { threads: None })],
        ..pass(&[41.0])
    };
    let off = must_fail("a lens in software", k2_engine(&va, &fell).unwrap());
    assert!(off.contains("stream 1 in software"), "{off}");
    let stalled = Pass {
        stalled: Some("no frame at 3.000 s after 5s".into()),
        ..pass(&[41.0])
    };
    must_fail("a pass that stalled", k2_engine(&va, &stalled).unwrap());
    let brief = Pass {
        decoded: 30.0,
        elapsed: Duration::from_millis(500),
        ..pass(&[])
    };
    must_fail("no full second", k2_engine(&va, &brief).unwrap());
    // Only VA-API with its frames copied decides it.
    let path_a = Run {
        zero_copy: ZeroCopy::All,
        ..va.clone()
    };
    for run in [Run::only(HwKind::Vulkan), path_a, Run::software()] {
        assert!(k2_engine(&run, &fast).is_none(), "{}", run.label);
    }
}

/// The 59.94 perf pair's rows say it is P only, where the X5 codes
/// TSA_N (M3 plan 7.2); the 29.97 pairs, coded P only as the X5 codes
/// them, say nothing.
#[test]
fn only_the_5994_perf_pair_says_it_is_p_only() {
    let said: Vec<(String, Option<&str>)> = synth_media::perf_pairs()
        .into_iter()
        .map(|p| {
            let unlike = coded_unlike_x5(&p.name);
            (p.name, unlike)
        })
        .collect();
    assert_eq!(said.len(), 3);
    for (name, unlike) in &said {
        if name == synth_media::PERF_PAIR_2880_5994 {
            assert_eq!(*unlike, Some(synth_media::PERF_PAIR_P_ONLY), "{name}");
        } else {
            must_fail(&format!("{name} said to be P only"), unlike.is_some());
        }
    }
}
