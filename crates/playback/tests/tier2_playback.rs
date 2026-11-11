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
//!   other picture as TSA_N, and a note says so beside its rows. P-LOCK
//!   (M3 plan 4.3) adds each mode played as the app plays it by default,
//!   unlocked and then opened as a locked pair, and each perf pair locked
//!   beside the app's run: rows of unpaired presents, pairs presented,
//!   drops, `pair_wait`, pair resyncs, slots, each lens's decodes and
//!   path B's pool, and a verdict a mode (Pass, Redirect to 8 slots, or
//!   Kill): at 8K30, 5.7K60 and 4K120 no pair apart, at least 99 % of the
//!   frames presented and at most 1 point more dropped than unlocked; at
//!   8K25 on path B no pair apart and the pacing rule met, with the pairs
//!   and `pair_wait` of each second that dropped.
//! - `bench_playback_stitched_samples`: each stitched equirect among the
//!   samples, gated as the perf clips are when it is within the criterion
//!   (5.7K at 30 fps or less) and recorded otherwise; a skip saying so
//!   while there is none (clip 12 is still to come).
//! - `bench_perf_clips_software_decode`: each perf clip's software decode
//!   rate, which hw-test's report holds against "too easy" (150 fps).
//! - `samples_lens_pairs_frame_locked` and `zero_copy_pair_matches_copy`
//!   (Tier 2h): clip 9 and the 2×3840² perf pair opened locked, landing
//!   and playing both lenses on one frame, read back from the slots by
//!   corner code, and every slot of path B's, path A's and the copy
//!   path's passes byte for byte a software decode's, with a line for each
//!   wrong one that says where and how it is wrong.
//!
//! Where the GPU, hardware decode of the format, the perf clips or the
//! samples aren't there, a benchmark records what it can and skips with
//! the reason, which never counts as a pass. A run that only records and
//! fails is a note saying why, but a stall fails the benchmark.
//! `the_bench_harness_plays_on_lavapipe` runs the same harness in Tier 1,
//! in software on a small clip, with one stream and with two, and
//! `the_bench_harness_plays_both_tracks_of_a_pair` plays a 2 × 64² pair
//! as the perf pairs are played, and `the_bench_harness_plays_a_locked_pair`
//! plays it locked, reading its corner codes and its slots back as Tier
//! 2h does, so that none can rot between hardware runs. Run the rest with
//! `cargo xtask hw-test`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::video::{
    Assumed, ChromaSiting, ColorCodes, ColorFamily, ColorInfo, Matrix, PixelLayout, PlaneDesc,
    Primaries, Range, SampleBytes, VideoPlanes,
};
use spherecut_core::{Clock, FrameRate, ManualClock, Rounding, StopToken, SystemClock, Time};
use spherecut_media::HwDevice;
use spherecut_media::backend::{
    DecodeMode, DecodePath, Fallback, FfmpegBackend, HwKind, HwOutput, MediaBackend, NextFrame,
    SeekMode, Transfer, VideoFrame, VideoRequest,
};
use spherecut_media::decode::VideoDecoder;
use spherecut_media::detect::Layout;
use spherecut_media::hw::{self, GpuIds, RenderNode};
use spherecut_media::probe::{self, ProbeReport, StreamKind};
use spherecut_playback::decoder::choose;
use spherecut_playback::{
    CacheBudget, Counter, DecoderPreference, HwSupport, LockSpec, LockStatus, PerfStats, Player,
    PlayerConfig, PlayerParts, PlayerSnapshot, Presented, SURFACE_CAP, ShownFrame, SlotFrame,
    SlotStats, Stage, StreamFormat, ZeroCopy,
};
use spherecut_render::dmabuf::{self, Importer};
use spherecut_render::vkshare::Copied;
use spherecut_render::{
    AdapterChoice, DebugMode, Encoder, Filter, Gpu, GpuFrame, GpuTimers, Image, LinearTarget,
    Placement, Projector, RenderError, SlotShape, StillOptions, StillRenderer, Uploader,
    ViewParams, upload,
};
use spherecut_render::{readback, wgpu};
use spherecut_sphere::DQuat;
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_synth::code::{self as corner, CornerCode, Luma, Samples};
use spherecut_synth::media::{self as synth_media, Codec, Spec};
use spherecut_testkit::control::must_fail;
use spherecut_testkit::samples::{self, Mode};
use spherecut_testkit::{bench, tier2};

use common::pacing;

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
                locked: false,
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
    /// The two streams are opened as a locked pair
    /// ([`Player::open_locked`]): P-LOCK's runs (M3 plan 4.3).
    locked: bool,
}

impl Run {
    /// On `kind` alone, copied into the slots.
    fn only(kind: HwKind) -> Run {
        Run {
            decoder: DecoderPreference::Only(kind),
            label: kind.to_string(),
            kind: Some(kind),
            zero_copy: ZeroCopy::Off,
            locked: false,
        }
    }

    fn software() -> Run {
        Run {
            decoder: DecoderPreference::Software,
            label: "software".into(),
            kind: None,
            zero_copy: ZeroCopy::Off,
            locked: false,
        }
    }

    /// The same run with its two streams opened as a locked pair, its
    /// rows called "…, locked".
    fn locked(&self) -> Run {
        Run {
            label: format!("{}, locked", self.label),
            locked: true,
            ..self.clone()
        }
    }

    /// Vulkan Video's frames decoded on the renderer's own device and
    /// kept there (path B), where its rig shares the device.
    fn path_b(&self) -> bool {
        self.kind == Some(HwKind::Vulkan) && self.zero_copy.vulkan()
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
    /// The longest `pair_wait` a present of a locked pair recorded in the
    /// second, in ms.
    pair_wait: f32,
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
    /// The streams' frame rate, where the first stream gives one.
    rate: Option<FrameRate>,
    /// Presents whose streams showed frames of different times, as the
    /// engine counts them (`unpaired_presents`, over every stream, locked
    /// or not), and the times a locked pair sought both lenses again to
    /// find a common frame (`pair_resyncs`).
    unpaired: u64,
    resyncs: u64,
    /// Each stream's frames decoded since the open, handed out or passed
    /// over ([`StreamStatus::decoded`]), at the end of the pass.
    ///
    /// [`StreamStatus::decoded`]: spherecut_playback::StreamStatus::decoded
    lens_decoded: Vec<u64>,
    /// The slots at the end of the pass.
    slots: SlotStats,
    /// A locked pair's lock at the end of the pass; `None` unlocked.
    lock: Option<LockStatus>,
    /// Path B's pool as the frames shown used it ([`PoolWatch`]), by
    /// stream: its distinct Vulkan images and their bytes. Empty off path
    /// B.
    pool: Vec<(usize, u64)>,
    /// Of a locked pair, each present's lenses' clip times, `None` for a
    /// lens it showed nothing of, and its [`pacing::Sample`], up to the
    /// first loop.
    presents: Vec<[Option<Time>; 2]>,
    samples: Vec<pacing::Sample>,
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
    /// Unpaired presents and pair resyncs at the start of the pass.
    pairs: [u64; 2],
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
            pairs: [0; 2],
            at: [0; 3],
            current: Second::default(),
            seen,
            pass: Pass::default(),
        };
        m.at = m.counts();
        m.base = m.at;
        m.imports = m.import_counts();
        m.pairs = m.pair_counts();
        m
    }

    fn pair_counts(&self) -> [u64; 2] {
        [Counter::UnpairedPresents, Counter::PairResyncs].map(|c| self.perf.count(c))
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
            let new = &kept[kept.len() - take..];
            if s == Stage::PairWait {
                self.current.pair_wait = new.iter().copied().fold(self.current.pair_wait, f32::max);
            }
            self.pass
                .stages
                .entry(s.name())
                .or_default()
                .extend_from_slice(new);
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
        let [unpaired, resyncs] = self.pair_counts();
        self.pass.unpaired = unpaired - self.pairs[0];
        self.pass.resyncs = resyncs - self.pairs[1];
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

/// Open `streams` of `path` on `player` as `run` asks: two streams by
/// index as a locked pair ([`Player::open_locked`]), else each stream on
/// its own.
fn open(player: &Player, path: &Path, streams: &[Option<usize>], run: &Run) -> Result<(), String> {
    match (run.locked, streams) {
        (false, _) => player.open_streams(path.to_path_buf(), streams.to_vec()),
        (true, &[Some(a), Some(b)]) => {
            player.open_locked(path.to_path_buf(), [a, b], LockSpec::default());
        }
        (true, _) => {
            return Err(format!(
                "a locked pair is two streams by index, not {streams:?}"
            ));
        }
    }
    Ok(())
}

/// What a pass's end adds to what its [`Meter`] counted: the streams'
/// decodes, the slots and the lock, and the pool `watch` saw.
fn finish_pass(mut pass: Pass, player: &Player, watch: Option<PoolWatch>) -> Pass {
    let snap = common::snapshot(player);
    pass.paths = snap.streams.iter().map(|s| s.decode.clone()).collect();
    pass.refusal = snap.import_refused.clone();
    pass.rate = stream_rate(player);
    pass.lens_decoded = snap.streams.iter().map(|s| s.decoded).collect();
    pass.slots = snap.slots;
    pass.lock = snap.lock.map(|l| l.status);
    pass.pool = watch.map_or_else(Vec::new, PoolWatch::finish);
    pass
}

/// Path B's pool, as the frames shown used it (M3 plan section 9: a pair's
/// pool may reach about 707 MB at 8K): a thread of its own, off the UI
/// thread, looks each frame shown up in the cache and keeps the Vulkan
/// images FFmpeg decoded it into, a stream at a time.
struct PoolWatch {
    shown: mpsc::Sender<(usize, Time)>,
    thread: std::thread::JoinHandle<Vec<(usize, u64)>>,
}

impl PoolWatch {
    /// A watch on `player`'s `streams` streams, for a run on path B.
    fn of(player: &Player, run: &Run, streams: usize) -> Option<PoolWatch> {
        if !run.path_b() {
            return None;
        }
        let cache = player.cache().clone();
        let (shown, frames) = mpsc::channel::<(usize, Time)>();
        let thread = std::thread::spawn(move || {
            let mut images = vec![HashSet::new(); streams];
            let mut bytes = vec![0u64; streams];
            for (stream, time) in frames {
                let Some(frame) = cache.get(stream, time) else {
                    continue;
                };
                let (Some(picture), Some(seen), Some(each)) = (
                    frame.vulkan(),
                    images.get_mut(stream),
                    bytes.get_mut(stream),
                ) else {
                    continue;
                };
                picture.with_locked(&mut |states| seen.extend(states.iter().map(|s| s.image)));
                // A picture's planes, each its size in texels by its
                // texel's bytes, over the images that hold them.
                let layout = frame.layout();
                let held: u64 = picture
                    .planes()
                    .iter()
                    .zip(&layout.planes)
                    .map(|(p, d)| {
                        let texel = u64::from(d.components)
                            * match d.bytes {
                                SampleBytes::One => 1,
                                SampleBytes::Two => 2,
                            };
                        u64::from(p.size.0) * u64::from(p.size.1) * texel
                    })
                    .sum();
                let images: HashSet<usize> = picture.planes().iter().map(|p| p.image).collect();
                *each = (*each).max(held / images.len().max(1) as u64);
            }
            images
                .iter()
                .zip(bytes)
                .map(|(i, b)| (i.len(), i.len() as u64 * b))
                .collect()
        });
        Some(PoolWatch { shown, thread })
    }

    /// Note what `p` shows.
    fn shown(&self, p: &Presented) {
        for f in &p.frames {
            let _ = self.shown.send((f.stream, f.frame.time));
        }
    }

    /// Each stream's distinct images and their bytes.
    fn finish(self) -> Vec<(usize, u64)> {
        drop(self.shown);
        self.thread.join().unwrap_or_default()
    }
}

/// What [`unthrottled_with`] does with each new frame on screen besides
/// count it: read it back, say, which waits for the GPU, so the pass isn't
/// marked as the UI thread. It stops the pass with `Break`, or fails it.
type Each<'a> = &'a mut dyn FnMut(&Presented) -> Result<ControlFlow<()>, String>;

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
    unthrottled_with(rig, path, streams, run, limit, None)
}

/// [`unthrottled`], with `each` called at every new frame on screen, the
/// first included, off the UI thread.
fn unthrottled_with(
    rig: &Rig,
    path: &Path,
    streams: &[Option<usize>],
    run: &Run,
    limit: Duration,
    mut each: Option<Each<'_>>,
) -> Result<Pass, String> {
    let clock = Arc::new(ManualClock::new(FrameRate::FPS_30.frame_duration()));
    let player = rig.player(clock.clone(), run)?;
    open(&player, path, streams, run)?;
    let watch = PoolWatch::of(&player, run, streams.len());
    let pass = (|| -> Result<Pass, String> {
        let ui = each.is_none().then(UiThread::mark);
        let first = first_frame(&player, streams.len())?;
        let rate = stream_rate(&player);
        let mut shown = first.frames[0].frame;
        if let Some(w) = &watch {
            w.shown(&first);
        }
        let mut stop = match each.as_mut() {
            Some(f) => f(&first)?.is_break(),
            None => false,
        };
        // Held, its frames' slots would stay out of use: with two streams,
        // two of the four, and no stream could show its next frame.
        drop(first);
        player.play();
        let mut meter = Meter::new(player.perf().clone(), streams.len(), Instant::now());
        let mut timeline = Timeline::new(path, run, "unthrottled", player.perf(), meter.begun);
        'pass: while !stop && meter.begun.elapsed() < limit {
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
                    if let Some(w) = &watch {
                        w.shown(&p);
                    }
                    if let Some(f) = each.as_mut() {
                        stop = f(&p)?.is_break();
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
        let pass = finish_pass(meter.finish(Instant::now()), &player, watch);
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
    open(&player, path, streams, run)?;
    let watch = PoolWatch::of(&player, run, streams.len());
    let pass = (|| -> Result<Pass, String> {
        let ui = UiThread::mark();
        let perf = player.perf().clone();
        let first = first_frame(&player, streams.len())?;
        screen.draw(&first, &perf).map_err(|e| e.to_string())?;
        let rate = stream_rate(&player);
        let mut last = first.frames[0].frame;
        drop(first);
        // A locked pair's presents, for the pacing rule, until the first
        // loop seeks back.
        let mut presents = Vec::new();
        let mut samples = Vec::new();
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
            if run.locked && meter.pass.loops == 0 {
                let time = |m: usize| {
                    presented
                        .as_ref()?
                        .frames
                        .iter()
                        .find(|f| f.stream == m)
                        .map(|f| f.frame.time)
                };
                presents.push([time(0), time(1)]);
                samples.push(pacing::Sample::of(presented.as_ref(), &snap, [0, 1]));
            }
            if let Some(p) = &presented {
                if p.changed {
                    screen.draw(p, &perf).map_err(|e| e.to_string())?;
                    if let Some(w) = &watch {
                        w.shown(p);
                    }
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
        let mut pass = finish_pass(meter.finish(Instant::now()), &player, watch);
        pass.presents = presents;
        pass.samples = samples;
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
    if r.lens_decoded.len() == 2 {
        record_pair(&name, run, &[u, r]);
    }
}

/// A two-stream run's rows of P-LOCK's (M3 plan 4.3), locked or not,
/// from its `passes`, unthrottled then real time where both ran: the
/// unpaired presents over them all, each lens's decodes since the open
/// and path B's pool in the last; of a locked run, its pair resyncs, the pairs it
/// presented, `pair_wait`, its slots and its lock.
fn record_pair(name: &str, run: &Run, passes: &[&Pass]) {
    let Some(last) = passes.last() else {
        return;
    };
    let over = |of: fn(&Pass) -> u64| passes.iter().map(|p| of(p)).sum::<u64>() as f64;
    let pass = if passes.len() > 1 {
        "real time"
    } else {
        "unthrottled"
    };
    bench::record(
        &format!("{name}: unpaired presents"),
        over(|p| p.unpaired),
        "presents",
    );
    for (k, decoded) in last.lens_decoded.iter().enumerate() {
        bench::record(
            &format!("{name}: lens {k} decoded since the open, {pass}"),
            *decoded as f64,
            "frames",
        );
    }
    for (k, (images, bytes)) in last.pool.iter().enumerate() {
        bench::record(
            &format!("{name}: path B pool, lens {k}, {pass}"),
            *bytes as f64 / f64::from(1u32 << 20),
            "MiB",
        );
        bench::note(format!(
            "{name}: lens {k}'s frames shown were in {images} Vulkan images, {pass}"
        ));
    }
    if !run.locked {
        return;
    }
    bench::record(
        &format!("{name}: pair resyncs"),
        over(|p| p.resyncs),
        "resyncs",
    );
    bench::record(
        &format!("{name}: pairs presented, {pass}"),
        last.shown as f64,
        "pairs",
    );
    if let Some(share) = presented_share(last) {
        bench::record(&format!("{name}: presented after 1 s"), share, "%");
    }
    for p in [50.0, 95.0] {
        if let Some(ms) = last.stage(Stage::PairWait, p) {
            bench::record(&format!("{name}: pair_wait p{p}, {pass}"), ms, "ms");
        }
    }
    bench::record(&format!("{name}: slots"), last.slots.active as f64, "slots");
    bench::note(format!(
        "{name}: the lock {}; {} slots in use, {} dead, at the end",
        match &last.lock {
            Some(LockStatus::Locked) => "held".to_string(),
            Some(LockStatus::Refused { field }) => format!("was refused or undone ({field})"),
            Some(LockStatus::Pending) => "never settled".to_string(),
            None => "was never asked for".to_string(),
        },
        last.slots.active,
        last.slots.dead
    ));
}

/// The frames a real-time pass's full seconds after the first had to
/// show, at its rate.
fn due_after_first(r: &Pass) -> Option<f64> {
    let fps = r.rate?.to_f64();
    let seconds = r.seconds.len().checked_sub(1).filter(|&n| n > 0)?;
    Some(seconds as f64 * fps)
}

/// Of those frames, the share shown, in %.
fn presented_share(r: &Pass) -> Option<f64> {
    Some(100.0 * r.after_first(|s| s.shown) as f64 / due_after_first(r)?)
}

/// Of those frames, the share dropped, in %.
fn dropped_share(r: &Pass) -> Option<f64> {
    Some(100.0 * r.after_first(|s| s.dropped) as f64 / due_after_first(r)?)
}

/// P-LOCK's verdict (M3 plan 4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The lock as designed.
    Pass,
    /// The lock held every pair together but fell short of the frames:
    /// `MAX_SLOTS` 6 → 8 for a locked pair, the plan's redirect.
    Redirect,
    /// The lock showed a pair apart, came undone or stalled, which more
    /// slots wouldn't mend: M3.10 waits for a design note on the upload
    /// schedule.
    Kill,
}

/// What a locked run's `passes` did that fails P-LOCK outright: a pair
/// shown apart, the lock refused or undone (the slot floor), a stall.
fn lock_broken(passes: &[&Pass]) -> Vec<String> {
    let mut broken = Vec::new();
    for (pass, p) in ["unthrottled", "real time"].iter().zip(passes) {
        if p.unpaired > 0 {
            broken.push(format!("{pass}: {} unpaired presents", p.unpaired));
        }
        if p.lock != Some(LockStatus::Locked) {
            broken.push(format!("{pass}: the lock ended {:?}", p.lock));
        }
        if let Some(why) = &p.stalled {
            broken.push(format!("{pass}: stalled: {why}"));
        }
    }
    broken
}

/// P-LOCK at 8K30, 5.7K60 and 4K120: the locked run's real-time pass
/// presents at least 99 % of its frames and drops at most 1 percentage
/// point more than `unlocked`'s, with no pair apart.
fn held_verdict(unlocked: &Pass, locked: &(Pass, Pass)) -> (Verdict, String) {
    let broken = lock_broken(&[&locked.0, &locked.1]);
    if !broken.is_empty() {
        return (Verdict::Kill, broken.join("; "));
    }
    let r = &locked.1;
    let (Some(shown), Some(dropped), Some(before)) = (
        presented_share(r),
        dropped_share(r),
        dropped_share(unlocked),
    ) else {
        return (
            Verdict::Kill,
            "the real-time passes ran no full second after the first".into(),
        );
    };
    let line = format!(
        "0 unpaired, presented {shown:.2} % of the frames after 1 s, dropped {dropped:.2} % \
         against {before:.2} % unlocked"
    );
    if shown >= 99.0 && dropped - before <= 1.0 {
        (Verdict::Pass, line)
    } else {
        (Verdict::Redirect, line)
    }
}

/// P-LOCK at 8K25 on path B, where the unlocked raw view decode-drops:
/// no pair apart and the pacing rule met over the real-time pass up to
/// its first loop, with the pairs presented and `pair_wait` in each
/// second that dropped.
fn paced_verdict(locked: &(Pass, Pass)) -> (Verdict, String) {
    let broken = lock_broken(&[&locked.0, &locked.1]);
    if !broken.is_empty() {
        return (Verdict::Kill, broken.join("; "));
    }
    let r = &locked.1;
    let drops: Vec<String> = r
        .seconds
        .iter()
        .enumerate()
        .filter(|(_, s)| s.dropped > 0)
        .map(|(i, s)| {
            format!(
                "second {i}: {} dropped ({} decode-dropped), {} pairs, pair_wait {:.1} ms at most",
                s.dropped, s.decode_dropped, s.shown, s.pair_wait
            )
        })
        .collect();
    let drops = if drops.is_empty() {
        "no second dropped".to_string()
    } else {
        drops.join(", ")
    };
    let Some(rate) = r.rate else {
        return (Verdict::Kill, "the pass has no frame rate".into());
    };
    match pacing::pacing_cut(&r.samples, rate) {
        Ok(paced) => (
            Verdict::Pass,
            format!("0 unpaired, the pacing rule met ({paced:?}); {drops}"),
        ),
        Err(why) => (
            Verdict::Redirect,
            format!("0 unpaired, the pacing rule failed: {why:?}; {drops}"),
        ),
    }
}

/// The camera's mode of a two-lens file, by its lenses' side and rate.
fn x5_mode(report: &ProbeReport) -> Option<Mode> {
    let ((w, h), fps, _) = shape(report)?;
    Mode::ALL.into_iter().find(|m| {
        let (n, d) = m.rate();
        w == m.side() && h == m.side() && (fps - f64::from(n) / f64::from(d)).abs() < 0.01
    })
}

/// P-LOCK (M3 plan 4.3) on one X5 mode: its pair played as the app plays
/// it by default (D25), unlocked and then locked, both passes each, the
/// locked rows beside the unlocked, and the verdict where the plan gives
/// one: 8K30, 5.7K60 and 4K120 held to the unlocked raw view, 8K25 to the
/// pacing rule on path B. `auto` is Auto's run, copied, and `copied` its
/// passes, the unlocked ones where the app copies too. Stalls go into
/// `stalls`; the verdict's line comes back.
fn p_lock(
    rig: &Rig,
    mode: &str,
    path: &Path,
    report: &ProbeReport,
    auto: &Run,
    copied: Option<&(Pass, Pass)>,
    stalls: &mut Vec<String>,
) -> String {
    let streams = lens_streams(report);
    let class = x5_mode(report);
    let shared = rig.as_app_plays(auto);
    let (on, base) = match &shared {
        Ok((on, kept)) => (on, kept.clone()),
        Err(why) => {
            bench::note(format!(
                "{mode}, P-LOCK: copied, as the app plays it: {why}"
            ));
            (rig, auto.clone())
        }
    };
    let fresh;
    let unlocked = if shared.is_ok() {
        fresh = record(on, mode, path, &streams, &base, RECORDED, RECORDED);
        stalls.extend(stall(mode, &base, &fresh));
        fresh.as_ref()
    } else {
        copied
    };
    let locked_run = base.locked();
    let locked = record(on, mode, path, &streams, &locked_run, RECORDED, RECORDED);
    stalls.extend(stall(mode, &locked_run, &locked));
    let name = format!("P-LOCK, {mode}, {}", locked_run.label);
    let Some(locked) = locked else {
        return format!("{name}: Kill: the locked run didn't play (see its note)");
    };
    let (verdict, why) = match class {
        Some(Mode::K8_25) if !base.path_b() => {
            return format!(
                "{name}: not decided: 8K25's verdict is on path B, and {}",
                shared.err().unwrap_or_else(|| "the run copies".into())
            );
        }
        Some(Mode::K8_25) => paced_verdict(&locked),
        Some(Mode::K8_30 | Mode::K5_7_60 | Mode::K4_120) => match unlocked {
            Some((_, r)) => held_verdict(r, &locked),
            None => {
                return format!("{name}: not decided: the unlocked run didn't play");
            }
        },
        Some(Mode::K5_7_30) | None => {
            let broken = lock_broken(&[&locked.0, &locked.1]);
            return format!(
                "{name}: recorded, P-LOCK names no verdict for this mode{}",
                if broken.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", broken.join("; "))
                }
            );
        }
    };
    format!(
        "{name}: {verdict:?}: {why}; {} slots",
        locked.1.slots.active
    )
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
                record_pair(&name, run, &[u]);
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
    // P-LOCK's rows (M3 plan 4.3): the app's run again, locked, recorded
    // beside it.
    let locked = (runs[0].0, runs[0].1.locked());
    let passes = play_pair(clip, path, &streams, &runs, GATED_UNTHROTTLED);
    let held = play_pair(
        clip,
        path,
        &streams,
        std::slice::from_ref(&locked),
        GATED_UNTHROTTLED,
    );
    for u in held.iter().flatten() {
        let broken = lock_broken(&[u]);
        bench::note(format!(
            "{clip}, {}: P-LOCK recorded{}",
            locked.1.label,
            if broken.is_empty() {
                ": 0 unpaired, the lock held".to_string()
            } else {
                format!(": {}", broken.join("; "))
            }
        ));
    }
    for ((_, run), pass) in runs.iter().chain([&locked]).zip(passes.iter().chain(&held)) {
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
    let mut verdicts = Vec::new();
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
                verdicts.push(p_lock(
                    &rig,
                    mode,
                    path,
                    report,
                    &run,
                    passes.as_ref(),
                    &mut stalls,
                ));
            }
            Err(why) => {
                verdicts.push(format!(
                    "P-LOCK, {mode}: not decided: Auto decodes it in software here"
                ));
                no_hardware.push(format!("{mode}: {why}"));
            }
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
    for verdict in &verdicts {
        bench::note(verdict);
    }
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

/// Frames the zero-copy checks decode of each clip, a lens, while they
/// hold frames as the frame cache would. On VA-API (path A) that is more
/// than the [`ZERO_COPY_SURFACES`] its pool keeps beyond the decoder's
/// own, so that the pool runs round. On Vulkan Video (path B)
/// `extra_hw_frames` isn't raised: FFmpeg's pool makes images as frames
/// are held, and what bounds it is the frames held on the device, at most
/// [`SURFACE_CAP`] a stream in the cache (and as many in the one-stream
/// check's queue), with the decoder's references and the frames on their
/// way. Past those, 48 frames have the decoder decode into images of
/// frames let go, over and over.
///
/// [`ZERO_COPY_SURFACES`]: spherecut_media::backend::ZERO_COPY_SURFACES
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
/// back while it was held would show a later picture. Every other frame
/// is also taken on the GPU right after its decode, before its download,
/// as the upload thread takes it while later decodes may still read it
/// as a reference, and must render as the copy too. Returns the frames
/// compared, and of those, the ones taken right after their decode.
fn compare_zero_copy(
    rig: &Rig,
    how: &OnDevice,
    path: &Path,
    stream: Option<usize>,
    count: usize,
) -> Result<(usize, usize), NotCompared> {
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
    let mut early = 0;
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
        // Production order: taken as soon as it is decoded.
        let first = (k % 2 == 0)
            .then(|| take(&rig.gpu, how, &frame))
            .transpose()?;
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
        if let Some(taken) = &first {
            for (i, (view, size)) in views.iter().enumerate() {
                let got = render_taken(&rig.gpu, &mut renderer, &frame, taken, view, *size, &opts)
                    .unwrap_or_else(|e| panic!("frame {k}: rendering {}: {e}", how.taken()));
                if got != want[i] {
                    let why = mismatch(
                        &rig.gpu,
                        &mut renderer,
                        how,
                        &frame,
                        taken,
                        &got,
                        &want[i],
                        view,
                        *size,
                        &opts,
                    );
                    panic!(
                        "{}: frame {k}, view {i}: {} right after the decode renders other \
                         bytes than the copy: {why}",
                        path.display(),
                        how.taken()
                    );
                }
            }
            early += 1;
        }
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
    Ok((compared, early))
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
            Ok((n, early)) => eprintln!(
                "{name}: {n} frames, {} byte-identical to the copy, {early} of them taken \
                 right after their decode too",
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

/// The corner codes (M3 plan 7.1) in the slots a pair shows, read back
/// from the GPU: a slot's four corner blocks of luma copied on the GPU
/// into one small texture and read back, then laid at their corners of a
/// picture otherwise black and read as synth reads a picture. 8-bit luma
/// only, as the perf pairs' and the 2×64² pairs' are.
#[derive(Debug)]
struct CodeReader {
    side: u32,
    block: u32,
    blocks: wgpu::Texture,
    luma: Vec<u8>,
}

impl CodeReader {
    /// A reader for lenses `side` px square.
    fn new(gpu: &Gpu, side: u32) -> CodeReader {
        let block = corner::block_side(side);
        let blocks = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("corner blocks"),
            size: wgpu::Extent3d {
                width: 2 * block,
                height: 2 * block,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        CodeReader {
            side,
            block,
            blocks,
            luma: vec![0; side as usize * side as usize],
        }
    }

    /// The code `f`'s slot holds. Waits for the GPU.
    fn read(&mut self, gpu: &Gpu, f: &ShownFrame) -> Result<CornerCode, String> {
        let texture = f
            .gpu
            .plane_texture(0)
            .ok_or_else(|| format!("stream {}'s slot has no planes", f.stream))?;
        let size = (texture.width(), texture.height());
        if texture.format() != wgpu::TextureFormat::R8Unorm || size != (self.side, self.side) {
            return Err(format!(
                "stream {}'s slot is {:?} at {}x{}, not 8-bit luma {} px square",
                f.stream,
                texture.format(),
                size.0,
                size.1,
                self.side
            ));
        }
        let (b, far) = (self.block, self.side - self.block);
        // In read_corners' order, each with its place in `blocks`.
        let corners = [(0, 0), (far, 0), (0, far), (far, far)];
        let at = |i: u32| (i % 2 * b, i / 2 * b);
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("corner blocks"),
            });
        for (i, &(x, y)) in (0u32..).zip(&corners) {
            let (bx, by) = at(i);
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.blocks,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: bx, y: by, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: b,
                    height: b,
                    depth_or_array_layers: 1,
                },
            );
        }
        gpu.queue.submit([enc.finish()]);
        let texels = readback::read_texture(gpu, &self.blocks, 0).map_err(|e| e.to_string())?;
        let (b, side) = (b as usize, self.side as usize);
        for (i, &(x, y)) in (0u32..).zip(&corners) {
            let (bx, by) = at(i);
            let (bx, by, x, y) = (bx as usize, by as usize, x as usize, y as usize);
            for row in 0..b {
                let from = (by + row) * 2 * b + bx;
                let to = (y + row) * side + x;
                self.luma[to..to + b].copy_from_slice(&texels.data[from..from + b]);
            }
        }
        let luma = Luma {
            samples: Samples::U8(&self.luma),
            width: self.side,
            height: self.side,
            stride: side,
            bits: 8,
        };
        corner::read_corner_code(&luma).map_err(|e| format!("stream {}: {e}", f.stream))
    }
}

/// The frame index `time` is at `rate`.
fn frame_index(rate: FrameRate, time: Time) -> i64 {
    rate.time_to_frame(time, Rounding::Floor)
        .unwrap_or(i64::MIN)
}

/// What `p` shows of a locked pair: the frame its slots' tags name, both
/// lenses at one time, and the corner codes `reader` reads from lens 0's
/// slot and then lens 1's; an error when the tags already show the
/// lenses apart.
fn pair_codes(
    reader: &mut CodeReader,
    gpu: &Gpu,
    p: &Presented,
    rate: FrameRate,
) -> Result<(i64, [CornerCode; 2]), String> {
    let lens = |m: usize| p.frames.iter().find(|f| f.stream == m);
    let (Some(a), Some(b)) = (lens(0), lens(1)) else {
        return Err(format!(
            "a present showed {} of the two lenses",
            p.frames.len()
        ));
    };
    if a.frame.time != b.frame.time {
        return Err(format!(
            "the tags show the lenses apart: {:?} and {:?}",
            a.frame.time, b.frame.time
        ));
    }
    Ok((
        frame_index(rate, a.frame.time),
        [reader.read(gpu, a)?, reader.read(gpu, b)?],
    ))
}

/// Whether `codes`, read from a pair's slots, lens 0's then lens 1's, are
/// the frame `tag` their tags name, each of its own lens.
fn codes_match(tag: i64, codes: [CornerCode; 2]) -> Result<(), String> {
    let frame = u32::try_from(tag.rem_euclid(1 << corner::FRAME_BITS)).unwrap_or(0);
    let want = [0, 1].map(|lens| CornerCode::of(lens, frame));
    if codes == want {
        Ok(())
    } else {
        Err(format!("frame {tag}'s pair holds {codes:?}, not {want:?}"))
    }
}

/// The presents of a pass that showed a locked pair's lenses apart by
/// their tags: one lens alone, or the two at different times.
fn apart(presents: &[[Option<Time>; 2]]) -> usize {
    presents
        .iter()
        .filter(|[a, b]| (a.is_some() || b.is_some()) && a != b)
        .count()
}

/// [`play_pair`]'s passes with the pair opened locked, in Tier 1:
/// pair64-matched played through both passes on lavapipe in software,
/// with the corner codes of both slots read back at every frame of the
/// unthrottled pass and after each of [`LOCKED_SEEKS`] exact seeks, as
/// `samples_lens_pairs_frame_locked` reads the perf pair's (M3 plan
/// section 9). Every pair is the frame its tags name, every seek lands
/// both lenses on the frame that holds it, none is shown apart by the
/// engine's count or the harness's, the lock holds on six slots, and the
/// real-time pass keeps its samples for the pacing rule; and two
/// [`slot_pass`]es of the locked pair's first pairs read every slot back
/// as a software decode of the pair makes it, and as each other, slot for
/// slot. The controls: the pair opened unlocked holds no lock, which
/// [`lock_held`] says (a failure in `zero_copy_pair_matches_copy`, never
/// a skip); the second pass's lenses swapped differ from the first's;
/// each pass's first slot, judged as its lens's next frame and as the
/// other lens's, fails as the frame it is; lens 0's slot read for both
/// fails the codes' check, lens 1 a frame late in one present fails the
/// harness's count, and lens 1 where another seek left it fails the
/// landing's.
#[test]
fn the_bench_harness_plays_a_locked_pair() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("tier2-playback-locked-{}", std::process::id()));
    let rig = Rig {
        gpu: common::gpu().clone(),
        node: hw::render_node(None, None),
        hw: HwSupport::default(),
    };
    let spec = synth_media::pairs_64()
        .into_iter()
        .find(|p| p.name == synth_media::PAIR_64_MATCHED)
        .unwrap();
    let rate = spec.lenses[0].rate;
    let path = synth_media::write_pair(&spec, &dir).unwrap();
    let streams = lens_streams(&probe_file(&path));
    let run = Run::software().locked();
    let mut reader = CodeReader::new(&rig.gpu, spec.lenses[0].width);
    let mut read = Vec::new();
    let mut each = |p: &Presented| {
        read.push(pair_codes(&mut reader, &rig.gpu, p, rate)?);
        Ok(ControlFlow::Continue(()))
    };
    let limit = Duration::from_secs(60);
    let u = unthrottled_with(&rig, &path, &streams, &run, limit, Some(&mut each)).unwrap();
    let r = realtime(&rig, &path, &streams, &run, Duration::from_millis(2500)).unwrap();
    let truth = Truth::decode(&path, &streams, 4 + REACH).unwrap();
    let passes = [(); 2].map(|_| slot_pass(&rig, &path, &streams, &run, 4, false, &truth));
    let unlocked = slot_pass(&rig, &path, &streams, &Run::software(), 4, false, &truth);
    let landings = seek_landings(&rig, &path, &streams, &run, Some((&mut reader, rate))).unwrap();
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(u.stalled, None);
    assert_eq!(u.shown, 29);
    assert_eq!(
        (u.lock, u.unpaired, u.slots.active),
        (Some(LockStatus::Locked), 0, 6)
    );
    let tags: Vec<i64> = read.iter().map(|(tag, _)| *tag).collect();
    assert_eq!(tags, (0..30).collect::<Vec<_>>());
    for (tag, codes) in &read {
        codes_match(*tag, *codes).unwrap();
    }
    assert_eq!((r.lock, r.unpaired), (Some(LockStatus::Locked), 0));
    assert_eq!(apart(&r.presents), 0);
    assert!(r.shown > 0 && !r.samples.is_empty());
    assert_eq!(r.samples.len(), r.presents.len());
    assert_eq!(u.lens_decoded.len(), 2);
    assert!(
        u.lens_decoded.iter().all(|&d| d >= 29),
        "{:?}",
        u.lens_decoded
    );
    assert_eq!(landings.len(), LOCKED_SEEKS);
    for l in &landings {
        landed_together(l).unwrap();
        assert!(l.codes.is_some(), "{l:?}");
    }
    for p in &passes {
        assert_eq!((p.broken.as_deref(), p.wrong().count()), (None, 0), "{p:?}");
        lock_held(p).unwrap();
        let pts = |lens: usize| p.keys.keys().filter(move |k| k.0 == lens).map(|k| k.1);
        assert_eq!(pts(0).count(), 4, "{:?}", p.keys.keys());
        assert!(pts(0).eq(pts(1)), "{:?}", p.keys.keys());
        assert_eq!(p.counts.downloaded(), p.counts.uploaded, "{:?}", p.counts);
    }
    // Two runs of the same path, slot for slot.
    let (first, second) = (passes[0].hashes(), passes[1].hashes());
    assert_eq!(same_slots(&first, &second), Ok(8));

    // Controls.
    let swapped = second
        .iter()
        .map(|(&(lens, pts), &h)| ((lens ^ 1, pts), h))
        .collect();
    must_fail(
        "the second run's lenses swapped",
        same_slots(&first, &swapped),
    );
    assert_eq!(unlocked.broken, None);
    let unlocked = must_fail(
        "the pair opened unlocked holds a lock",
        lock_held(&unlocked),
    );
    assert!(unlocked.contains("the lock ended None"), "{unlocked}");
    for p in &passes {
        check_controls(p.controls.as_ref().unwrap());
    }
    let (tag, codes) = read[5];
    must_fail(
        "lens 0's slot read for both",
        codes_match(tag, [codes[0], codes[0]]),
    );
    let mut late = r.presents.clone();
    let k = late.iter().position(|p| p[1].is_some()).unwrap();
    late[k][1] = late[k][1].map(|t| t.checked_add(rate.frame_duration()).unwrap());
    must_fail("lens 1 a frame late in one present", apart(&late) == 0);
    let (a, b) = (
        landings[0],
        landings[1..]
            .iter()
            .find(|b| b.lenses[0] != landings[0].lenses[0])
            .unwrap(),
    );
    must_fail(
        "lens 1 landed where another seek left it",
        landed_together(&Landing {
            lenses: [a.lenses[0], b.lenses[1]],
            ..a
        }),
    );
}

/// A file a locked check plays: its name, its path and the rate its
/// corner codes count at, where it has them.
type LockedFile = (String, PathBuf, Option<FrameRate>);

/// The two-lens files the locked Tier 2h checks play, with the rate
/// their corner codes count at where they have them: clip 9's master
/// (`handheld_flips`, handheld, where a pair shown apart would show,
/// unlike clip 8's still tripod take) and the 2×3840² perf pair. Each
/// file not here is a reason.
fn locked_files() -> (Vec<LockedFile>, Vec<String>) {
    let (mut files, mut missing) = (Vec::new(), Vec::new());
    match tier2::samples_dir() {
        Some(dir) => match samples::master_in(&dir, samples::entry("handheld_flips")) {
            Ok(path) => files.push(("clip 9 (handheld_flips)".to_string(), path, None)),
            Err(why) => missing.push(why),
        },
        None => missing.push("no samples directory (SPHERECUT_SAMPLES)".into()),
    }
    let spec = synth_media::perf_pairs()
        .into_iter()
        .find(|p| p.name == synth_media::PERF_PAIR_3840_2997)
        .expect("synth's 2×3840² perf pair");
    match tier2::perf_clip(&spec.file_name()) {
        Ok(path) => files.push((
            format!("perf clip {}", spec.name),
            path,
            Some(spec.lenses[0].rate),
        )),
        Err(why) => missing.push(why),
    }
    (files, missing)
}

/// The rig and the run a locked check plays `report`'s two lenses on, as
/// the app plays them by default (D25), locked; why not, where Auto
/// decodes them in software.
fn locked_run<'a>(
    rig: &'a Rig,
    shared: &'a mut Option<Rig>,
    report: &ProbeReport,
) -> Result<(&'a Rig, Run), String> {
    let format = format_of(report, lens_streams(report)[0]);
    let auto = rig.auto(&format)?;
    match rig.as_app_plays(&auto) {
        Ok((on, kept)) => {
            let on = shared.insert(on);
            Ok((&*on, kept.locked()))
        }
        Err(why) => {
            eprintln!("{}: copied, as the app plays it: {why}", auto.label);
            Ok((rig, auto.locked()))
        }
    }
}

/// How many exact seeks [`samples_lens_pairs_frame_locked`] makes of each
/// file.
const LOCKED_SEEKS: usize = 100;

/// Where an exact seek left a locked pair on screen.
#[derive(Debug, Clone, Copy)]
struct Landing {
    target: Time,
    /// Each lens's frame shown: its pts, its clip time and when it ends.
    lenses: [Option<(i64, Time, Option<Time>)>; 2],
    /// The frame the slots' tags name and the corner codes the slots
    /// hold, where the file has them.
    codes: Option<(i64, [CornerCode; 2])>,
}

/// Whether a seek landed both lenses on the one frame that holds its
/// target, by pts, and by corner code where the landing read them.
fn landed_together(l: &Landing) -> Result<(), String> {
    let [Some(a), Some(b)] = l.lenses else {
        return Err(format!("{l:?}: a lens shows nothing"));
    };
    if (a.0, a.1) != (b.0, b.1) {
        return Err(format!(
            "a seek to {:?} landed the lenses apart: {a:?} and {b:?}",
            l.target
        ));
    }
    if a.1 > l.target || a.2.is_some_and(|end| end <= l.target) {
        return Err(format!(
            "a seek to {:?} landed both on {a:?}, which doesn't hold it",
            l.target
        ));
    }
    match l.codes {
        Some((tag, codes)) => codes_match(tag, codes),
        None => Ok(()),
    }
}

/// [`LOCKED_SEEKS`] exact seeks of the pair at `path` opened as `run` on
/// `rig`, paused, each to the middle of a frame drawn at random (by a
/// fixed seed), and where each landed once both lenses show the frame
/// that holds it, or what they show after [`FIRST_FRAME`]; with the
/// corner codes `codes` reads, at that rate, when given.
fn seek_landings(
    rig: &Rig,
    path: &Path,
    streams: &[Option<usize>],
    run: &Run,
    mut codes: Option<(&mut CodeReader, FrameRate)>,
) -> Result<Vec<Landing>, String> {
    let clock = Arc::new(ManualClock::new(FrameRate::FPS_30.frame_duration()));
    let player = rig.player(clock, run)?;
    open(&player, path, streams, run)?;
    let landings = (|| -> Result<Vec<Landing>, String> {
        drop(first_frame(&player, streams.len())?);
        let rate = stream_rate(&player).ok_or("the lenses give no frame rate")?;
        let frames = common::snapshot(&player)
            .streams
            .iter()
            .filter_map(|s| s.info.as_ref()?.duration)
            .map(|d| frame_index(rate, d))
            .min()
            .filter(|&n| n > 1)
            .ok_or("the lenses give no length")?;
        // SplitMix64, from a fixed seed.
        let mut state: u64 = 0x5eed_0c0d_e5a1_0c4d;
        let mut next = || {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        let half = rate
            .frame_duration()
            .0
            .checked_div(spherecut_core::Rational::new(2, 1).map_err(|e| e.to_string())?)
            .map(Time)
            .map_err(|e| e.to_string())?;
        let mut landings = Vec::new();
        for _ in 0..LOCKED_SEEKS {
            let k = i64::try_from(next() % (frames - 1) as u64).unwrap_or(0);
            let target = rate
                .frame_to_time(k)
                .and_then(|t| t.checked_add(half))
                .map_err(|e| e.to_string())?;
            player.seek(target, SeekMode::Exact);
            let start = Instant::now();
            let p = loop {
                let p = player.present(Instant::now());
                let holds = |p: &Presented| {
                    p.frames.len() == 2
                        && p.frames.iter().all(|f| {
                            f.frame.time <= target && ends(&f.frame, Some(rate)) > Some(target)
                        })
                };
                match p {
                    Some(p) if holds(&p) || start.elapsed() > FIRST_FRAME => break Some(p),
                    None if start.elapsed() > FIRST_FRAME => break None,
                    _ => {}
                }
                stream_error(&player)?;
                std::thread::sleep(Duration::from_millis(1));
            };
            let lens = |m: usize| {
                let f = p.as_ref()?.frames.iter().find(|f| f.stream == m)?;
                Some((f.frame.pts, f.frame.time, ends(&f.frame, Some(rate))))
            };
            let codes = match (&mut codes, &p) {
                (Some((reader, at)), Some(p)) => pair_codes(reader, &rig.gpu, p, *at).ok(),
                _ => None,
            };
            landings.push(Landing {
                target,
                lenses: [lens(0), lens(1)],
                codes,
            });
        }
        Ok(landings)
    })();
    let joined = join(player);
    let landings = landings?;
    joined?;
    Ok(landings)
}

/// Each X5 sample's two lenses opened locked, as the app plays them by
/// default (M3 plan section 9), on clip 9 and on the 2×3840² perf pair:
/// 100 exact seeks land both lenses on one frame, by pts and, on the
/// perf pair, by corner code; both play in real time with no pair shown
/// apart, by the engine's count and the harness's; and the perf pair's
/// corner codes, read back from both presented slots on every frame of an
/// unthrottled pass, equal each other and the tags: the content check on
/// path B and its pool. Each check's control plants a mismatch it must
/// catch: lens 1 landed a frame off, the codes of lens 0's slot read for
/// both, lens 1 a frame late in one present.
#[test]
#[ignore = "tier2: needs the X5 samples or hw-test's perf pairs, and a GPU that decodes them"]
fn samples_lens_pairs_frame_locked() {
    tier2::require_hw_run();
    let (files, missing) = locked_files();
    if files.is_empty() {
        tier2::skip(missing.join("; "));
    }
    let rig = Rig::hardware();
    eprintln!("{}", rig.about());
    let mut no_hardware = Vec::new();
    for (name, path, coded) in &files {
        let report = probe_file(path);
        let streams = lens_streams(&report);
        let mut shared = None;
        let (on, run) = match locked_run(&rig, &mut shared, &report) {
            Ok(r) => r,
            Err(why) => {
                no_hardware.push(format!("{name}: Auto decodes it in software here: {why}"));
                continue;
            }
        };
        let side = shape(&report).map_or(0, |((w, _), _, _)| w);
        let mut reader = coded.map(|_| CodeReader::new(&on.gpu, side));
        let codes = reader.as_mut().zip(*coded);
        let landings = seek_landings(on, path, &streams, &run, codes)
            .unwrap_or_else(|e| panic!("{name}, {}: seeks: {e}", run.label));
        for l in &landings {
            landed_together(l).unwrap_or_else(|e| panic!("{name}, {}: {e}", run.label));
        }
        assert!(
            coded.is_none() || landings.iter().all(|l| l.codes.is_some()),
            "{name}: a landing's codes weren't read"
        );
        // Control: lens 1 as another seek left it, a frame off.
        let (a, b) = landings
            .iter()
            .zip(&landings[1..])
            .find(|(a, b)| a.lenses[0].map(|l| l.1) != b.lenses[0].map(|l| l.1))
            .expect("two seeks that landed on different frames");
        let planted = Landing {
            lenses: [a.lenses[0], b.lenses[1]],
            ..*a
        };
        must_fail("lens 1 landed elsewhere", landed_together(&planted));
        eprintln!(
            "{name}, {}: {} exact seeks landed both lenses together",
            run.label,
            landings.len()
        );

        // Real time, no pair apart.
        let r = realtime(on, path, &streams, &run, RECORDED)
            .unwrap_or_else(|e| panic!("{name}, {}: real time: {e}", run.label));
        assert_eq!(
            (r.lock, r.unpaired, apart(&r.presents)),
            (Some(LockStatus::Locked), 0, 0),
            "{name}, {}: in real time",
            run.label
        );
        let mut late = r.presents.clone();
        if let Some(k) = late.iter().position(|p| p[1].is_some()) {
            late[k][1] = late[k][1].and_then(|t| {
                t.checked_add(r.rate.map_or(Time::ZERO, FrameRate::frame_duration))
                    .ok()
            });
        }
        must_fail("lens 1 a frame late in one present", apart(&late) == 0);
        eprintln!(
            "{name}, {}: {} pairs in {:.1} s of real time, none apart; {}",
            run.label,
            r.shown,
            r.elapsed.as_secs_f64(),
            r.paths_line()
        );

        // Every frame's corner codes, unthrottled.
        let (Some(reader), Some(rate)) = (reader.as_mut(), *coded) else {
            continue;
        };
        let mut read = Vec::new();
        let mut each = |p: &Presented| {
            read.push(pair_codes(reader, &on.gpu, p, rate)?);
            Ok(ControlFlow::Continue(()))
        };
        let u = unthrottled_with(on, path, &streams, &run, GATED_UNTHROTTLED, Some(&mut each))
            .unwrap_or_else(|e| panic!("{name}, {}: unthrottled: {e}", run.label));
        assert_eq!(u.stalled, None, "{name}, {}", run.label);
        assert_eq!((u.lock, u.unpaired), (Some(LockStatus::Locked), 0));
        for (tag, codes) in &read {
            codes_match(*tag, *codes).unwrap_or_else(|e| panic!("{name}, {}: {e}", run.label));
        }
        let (tag, codes) = read[read.len() / 2];
        must_fail(
            "lens 0's slot read for both",
            codes_match(tag, [codes[0], codes[0]]),
        );
        eprintln!(
            "{name}, {}: {} pairs' corner codes read back, each its tags' frame; {}",
            run.label,
            read.len(),
            u.paths_line()
        );
    }
    if !missing.is_empty() || !no_hardware.is_empty() {
        tier2::skip(
            missing
                .iter()
                .chain(&no_hardware)
                .cloned()
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
}

/// How far either way of a wrong slot's frame [`Truth::like`] looks for a
/// frame, of either lens, whose bands the slot holds: a stale frame, or
/// one crossed from the other lens.
const REACH: usize = 16;

/// The picture rows of a band, the unit a wrong slot's place is given in:
/// an HEVC CTU row of luma, and the chroma rows beside it.
const BAND: u32 = 64;

/// The wrong frames of a pass the report lists one by one; the rest it
/// counts.
const LISTED: usize = 40;

/// One plane of a [`Yuv`]: its size in samples, and the samples, packed
/// row after row.
#[derive(Clone, PartialEq, Eq)]
struct Plane {
    width: u32,
    height: u32,
    samples: Vec<u8>,
}

impl Plane {
    /// Band `b`'s rows, `rows` a band, as far as the plane goes.
    fn band(&self, b: u32, rows: u32) -> &[u8] {
        let at = |r: u32| r.min(self.height) as usize * self.width as usize;
        &self.samples[at(b.saturating_mul(rows))..at(b.saturating_add(1).saturating_mul(rows))]
    }
}

/// A picture's 8-bit Y, U and V planes: the truth's, a slot's read back,
/// or a frame's in the cache, with interleaved chroma (NV12's) split, so
/// that any two compare plane for plane.
#[derive(Clone, PartialEq, Eq)]
struct Yuv([Plane; 3]);

impl std::fmt::Debug for Yuv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Yuv({})", self.shape())
    }
}

impl Yuv {
    /// `planes`, Y, U and V, or why they aren't three.
    fn of(planes: Vec<Plane>) -> Result<Yuv, String> {
        let n = planes.len();
        <[Plane; 3]>::try_from(planes)
            .map(Yuv)
            .map_err(|_| format!("{n} planes, not Y, U and V"))
    }

    /// Each plane's size: "3840x3840, 1920x1920, 1920x1920".
    fn shape(&self) -> String {
        self.0
            .iter()
            .map(|p| format!("{}x{}", p.width, p.height))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn same_shape(&self, other: &Yuv) -> bool {
        self.0
            .iter()
            .zip(&other.0)
            .all(|(a, b)| (a.width, a.height) == (b.width, b.height))
    }

    /// The picture's bands, [`BAND`] rows of luma each.
    fn bands(&self) -> u32 {
        self.0[0].height.div_ceil(BAND)
    }

    /// Plane `i`'s rows in a band: 64 of luma, 32 of 4:2:0 chroma.
    fn band_rows(&self, i: usize) -> u32 {
        let rows =
            u64::from(BAND) * u64::from(self.0[i].height) / u64::from(self.0[0].height.max(1));
        u32::try_from(rows).unwrap_or(BAND).max(1)
    }

    /// Whether band `b` is the same in every plane of `self` and `other`,
    /// pictures of one shape.
    fn same_band(&self, other: &Yuv, b: u32) -> bool {
        (0..3).all(|i| {
            let rows = self.band_rows(i);
            self.0[i].band(b, rows) == other.0[i].band(b, rows)
        })
    }

    /// A hash of every plane, eight bytes at a time: far quicker than
    /// testkit's `fnv64`, a byte at a time, at 8K.
    fn hash(&self) -> u64 {
        self.0
            .iter()
            .fold(0xcbf2_9ce4_8422_2325, |h, p| words_hash(h, &p.samples))
    }
}

/// `h` with `bytes` mixed in eight at a time, as FxHash mixes its words,
/// and their length.
fn words_hash(h: u64, bytes: &[u8]) -> u64 {
    let mix = |h: u64, w: u64| (h.rotate_left(5) ^ w).wrapping_mul(0x517c_c1b7_2722_0a95);
    let words = bytes.chunks_exact(8);
    let tail = words
        .remainder()
        .iter()
        .fold(0u64, |w, &b| w << 8 | u64::from(b));
    let h = words.fold(h, |h, w| mix(h, u64::from_le_bytes(w.try_into().unwrap())));
    mix(mix(h, tail), bytes.len() as u64)
}

/// A plane of `width` × `height` positions, `components` samples
/// interleaved at each, as a plane a component.
fn split(
    width: u32,
    height: u32,
    components: usize,
    samples: Vec<u8>,
) -> Result<Vec<Plane>, String> {
    match components {
        1 => Ok(vec![Plane {
            width,
            height,
            samples,
        }]),
        2 => {
            let (u, v): (Vec<u8>, Vec<u8>) = samples.chunks_exact(2).map(|c| (c[0], c[1])).unzip();
            Ok(vec![
                Plane {
                    width,
                    height,
                    samples: u,
                },
                Plane {
                    width,
                    height,
                    samples: v,
                },
            ])
        }
        n => Err(format!("a plane of {n} samples interleaved")),
    }
}

/// `frame`'s planes, planar or semi-planar 8-bit YUV, as a [`Yuv`].
fn yuv_of(frame: &dyn VideoPlanes) -> Result<Yuv, String> {
    let layout = frame.layout();
    if layout.family != ColorFamily::Yuv || layout.bit_depth != 8 {
        return Err(format!("{} isn't 8-bit YUV", layout.name));
    }
    let mut planes = Vec::new();
    for (i, desc) in layout.planes.iter().enumerate() {
        let p = frame
            .plane(i)
            .ok_or_else(|| format!("{} has no plane {i} in memory", layout.name))?;
        let row = desc.row_bytes(p.width);
        let mut samples = Vec::with_capacity(row * p.height as usize);
        for y in 0..p.height {
            let bytes = p
                .row(y, row)
                .ok_or_else(|| format!("{}'s plane {i} has no row {y}", layout.name))?;
            samples.extend_from_slice(bytes);
        }
        planes.extend(split(
            p.width,
            p.height,
            usize::from(desc.components),
            samples,
        )?);
    }
    Yuv::of(planes)
}

/// Slot `f`'s planes, read back from `gpu` by [`read_planes`].
fn read_slot(gpu: &Gpu, f: &ShownFrame) -> Result<Yuv, String> {
    read_planes(gpu, &f.gpu).map_err(|e| format!("stream {}'s slot: {e}", f.stream))
}

/// `slot`'s planes, level 0 (which the mips are made from), read back
/// from `gpu`, NV12's chroma split. Waits for the GPU.
fn read_planes(gpu: &Gpu, slot: &GpuFrame) -> Result<Yuv, String> {
    let mut planes = Vec::new();
    for (i, t) in (0..).map_while(|i| slot.plane_texture(i).map(|t| (i, t))) {
        let components = match t.format() {
            wgpu::TextureFormat::R8Unorm => 1,
            wgpu::TextureFormat::Rg8Unorm => 2,
            other => return Err(format!("plane {i} is {other:?}, not 8-bit")),
        };
        let texels = readback::read_texture(gpu, t, 0).map_err(|e| e.to_string())?;
        planes.extend(split(texels.width, texels.height, components, texels.data)?);
    }
    Yuv::of(planes)
}

/// The first `frames` frames of `stream` of `path`, decoded in software,
/// with their pts.
fn software_frames(
    path: &Path,
    stream: Option<usize>,
    frames: usize,
) -> Result<Vec<(i64, Yuv)>, String> {
    let req = VideoRequest {
        stream,
        ..VideoRequest::default()
    };
    let mut decoder = FfmpegBackend::new()
        .open_video(path, &req)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(frames);
    while out.len() < frames {
        match decoder
            .next_frame(&StopToken::never())
            .map_err(|e| e.to_string())?
        {
            NextFrame::Frame(f) => out.push((f.pts, yuv_of(f.as_ref())?)),
            NextFrame::End => break,
            NextFrame::Stopped => return Err("the decode stopped".into()),
        }
    }
    Ok(out)
}

/// What a slot pass judges its slots against: each lens's first frames
/// decoded in software, HEVC's decode being exact by the standard, so
/// that every pass, on any decoder, kept on the GPU or copied, must show
/// the same bytes. It holds [`REACH`] frames more than a pass shows, so
/// that a wrong slot can be named as a frame of either lens near its own.
struct Truth {
    lenses: Vec<TruthLens>,
}

/// A lens of the [`Truth`]: its stream, and its frames in order, with
/// their pts.
struct TruthLens {
    stream: Option<usize>,
    frames: Vec<(i64, Yuv)>,
}

impl Truth {
    /// The first `frames` frames of each of `streams` of `path`, decoded
    /// in software, a lens a thread.
    fn decode(path: &Path, streams: &[Option<usize>], frames: usize) -> Result<Truth, String> {
        let lenses = std::thread::scope(|s| {
            let decodes: Vec<_> = streams
                .iter()
                .map(|&stream| s.spawn(move || software_frames(path, stream, frames)))
                .collect();
            decodes
                .into_iter()
                .zip(streams)
                .map(|(d, &stream)| {
                    let frames = d
                        .join()
                        .map_err(|_| format!("decoding stream {stream:?} panicked"))??;
                    Ok(TruthLens { stream, frames })
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        Ok(Truth { lenses })
    }

    /// The lens `stream` is: its place among the truth's streams.
    fn lens_of(&self, stream: Option<usize>) -> Option<usize> {
        self.lenses.iter().position(|l| l.stream == stream)
    }

    /// Where lens `lens`'s frame at `pts` is among its frames.
    fn index(&self, lens: usize, pts: i64) -> Option<usize> {
        self.lenses
            .get(lens)?
            .frames
            .binary_search_by_key(&pts, |(p, _)| *p)
            .ok()
    }

    fn frame(&self, lens: usize, index: usize) -> Option<&(i64, Yuv)> {
        self.lenses.get(lens)?.frames.get(index)
    }

    /// Whether `picture` is lens `lens`'s frame `index`, byte for byte.
    fn holds(&self, picture: &Yuv, lens: usize, index: Option<usize>) -> bool {
        index
            .and_then(|i| self.frame(lens, i))
            .is_some_and(|(_, want)| want == picture)
    }

    /// Whether `slot` holds lens `lens`'s frame `index`, byte for byte;
    /// how it doesn't when not.
    fn judge(&self, slot: &Yuv, lens: usize, index: usize) -> Result<(), Wrong> {
        let Some((_, want)) = self.frame(lens, index) else {
            return Err(Wrong::NotInTruth);
        };
        if !slot.same_shape(want) {
            return Err(Wrong::Shape(format!(
                "{} against the truth's {}",
                slot.shape(),
                want.shape()
            )));
        }
        if slot == want {
            return Ok(());
        }
        let planes =
            std::array::from_fn(|i| PlaneDiff::of(&slot.0[i], &want.0[i], slot.band_rows(i)));
        let bands: Vec<u32> = (0..slot.bands())
            .filter(|&b| !slot.same_band(want, b))
            .collect();
        let like = self.like(slot, lens, index, &bands);
        Err(Wrong::Samples(Box::new(Diff {
            planes,
            bands: bands.len(),
            like,
        })))
    }

    /// Of the frames of either lens within [`REACH`] of lens `lens`'s
    /// frame `index` (that frame left out), the one that holds the most of
    /// `slot`'s bands `bands`, the nearest first and its own lens before
    /// the other: a stale frame, or one crossed from the other lens.
    /// `None` when none holds one.
    fn like(&self, slot: &Yuv, lens: usize, index: usize, bands: &[u32]) -> Option<Like> {
        let lenses: Vec<usize> = std::iter::once(lens)
            .chain((0..self.lenses.len()).filter(|&l| l != lens))
            .collect();
        let mut best: Option<Like> = None;
        for d in 0..=REACH as isize {
            let offs = if d == 0 { vec![0] } else { vec![-d, d] };
            for &l in &lenses {
                for &off in &offs {
                    if l == lens && off == 0 {
                        continue;
                    }
                    let Some((pts, other)) =
                        index.checked_add_signed(off).and_then(|j| self.frame(l, j))
                    else {
                        continue;
                    };
                    if !other.same_shape(slot) {
                        continue;
                    }
                    let held = bands.iter().filter(|&&b| slot.same_band(other, b)).count();
                    if held > best.map_or(0, |b| b.bands) {
                        best = Some(Like {
                            lens: l,
                            off,
                            pts: *pts,
                            bands: held,
                        });
                    }
                }
            }
        }
        best
    }
}

/// How a picture isn't the truth's frame.
#[derive(Debug, Clone)]
enum Wrong {
    /// The truth has no frame of its lens at its pts.
    NotInTruth,
    /// It isn't shaped as the truth's frame is.
    Shape(String),
    /// Its samples differ.
    Samples(Box<Diff>),
}

impl Wrong {
    /// The frame near it that holds the most of its bands that differ.
    fn like(&self) -> Option<Like> {
        match self {
            Wrong::Samples(d) => d.like,
            _ => None,
        }
    }
}

/// Where a picture's samples differ from the truth's frame's.
#[derive(Debug, Clone)]
struct Diff {
    /// Y's, U's and V's.
    planes: [PlaneDiff; 3],
    /// The picture's bands that differ in any plane.
    bands: usize,
    /// The frame near it of either lens that holds the most of those
    /// bands.
    like: Option<Like>,
}

/// Where one plane differs.
#[derive(Debug, Clone, Default)]
struct PlaneDiff {
    samples: usize,
    /// The largest difference of a sample.
    most: u8,
    /// x0, y0, x1, y1 of the samples that differ, inclusive, in the
    /// plane's own samples.
    bounds: Option<[u32; 4]>,
    /// The picture's bands it differs in.
    bands: Vec<u32>,
}

impl PlaneDiff {
    /// Where `got` differs from `want`, planes of one shape, `rows` rows a
    /// band.
    fn of(got: &Plane, want: &Plane, rows: u32) -> PlaneDiff {
        let mut d = PlaneDiff::default();
        let width = (got.width as usize).max(1);
        for (y, (g, w)) in (0u32..).zip(got.samples.chunks(width).zip(want.samples.chunks(width))) {
            if g == w {
                continue;
            }
            for (x, (a, b)) in (0u32..).zip(g.iter().zip(w)) {
                if a == b {
                    continue;
                }
                d.samples += 1;
                d.most = d.most.max(a.abs_diff(*b));
                d.bounds = Some(match d.bounds {
                    None => [x, y, x, y],
                    Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                });
            }
            let band = y / rows;
            if d.bands.last() != Some(&band) {
                d.bands.push(band);
            }
        }
        d
    }
}

/// A frame of the truth whose bands a wrong picture holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Like {
    lens: usize,
    /// Frames from the one the picture should be, and its pts.
    off: isize,
    pts: i64,
    /// Of the picture's bands that differ, those it holds.
    bands: usize,
}

impl std::fmt::Display for Wrong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let d = match self {
            Wrong::NotInTruth => return write!(f, "not among the truth's frames"),
            Wrong::Shape(s) => return write!(f, "shaped {s}"),
            Wrong::Samples(d) => d,
        };
        for (name, p) in ["Y", "U", "V"].iter().zip(&d.planes) {
            match p.bounds {
                None => write!(f, "{name} right; ")?,
                Some([x0, y0, x1, y1]) => write!(
                    f,
                    "{name} {} samples by up to {} in x {x0}-{x1} y {y0}-{y1}, bands {}; ",
                    p.samples,
                    p.most,
                    ranges(&p.bands)
                )?,
            }
        }
        match d.like {
            Some(l) => write!(
                f,
                "{} of its {} bands that differ are lens {}'s frame {:+} (pts {})",
                l.bands, d.bands, l.lens, l.off, l.pts
            ),
            None => write!(
                f,
                "none of its {} bands that differ is either lens's within {REACH} frames",
                d.bands
            ),
        }
    }
}

/// `[3, 4, 5, 9]` as "3-5, 9".
fn ranges(v: &[u32]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1] == v[j] + 1 {
            j += 1;
        }
        out.push(if i == j {
            v[i].to_string()
        } else {
            format!("{}-{}", v[i], v[j])
        });
        i = j + 1;
    }
    out.join(", ")
}

/// A second read of a wrong slot, still held: the same bytes as the first,
/// the truth's, or others.
#[derive(Debug)]
enum Again {
    Same,
    Right,
    Other,
    Failed(String),
}

impl std::fmt::Display for Again {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Again::Same => write!(f, "the same"),
            Again::Right => write!(f, "right"),
            Again::Other => write!(f, "other bytes"),
            Again::Failed(e) => write!(f, "failed ({e})"),
        }
    }
}

/// A wrong slot's frame as the cache holds it then, downloaded from its
/// decoder's device where it is on one: the truth's, the slot's bytes or
/// others; or gone from the cache.
#[derive(Debug)]
enum Cached {
    Right,
    AsSlot,
    Other,
    Gone,
    Failed(String),
}

impl std::fmt::Display for Cached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cached::Right => write!(f, "right"),
            Cached::AsSlot => write!(f, "as the slot"),
            Cached::Other => write!(f, "other bytes"),
            Cached::Gone => write!(f, "gone"),
            Cached::Failed(e) => write!(f, "unread ({e})"),
        }
    }
}

/// A showing of a frame whose slot wasn't the truth's.
#[derive(Debug)]
struct Showing {
    /// The slot, and the frame's run and place in it as the slot has them.
    slot: usize,
    run: u64,
    seq: u64,
    wrong: Wrong,
    again: Again,
    cached: Cached,
}

/// What a slot pass saw of one frame of one lens.
#[derive(Debug, Default)]
struct Seen {
    /// Its place among the truth's frames of its lens.
    index: Option<usize>,
    /// The times it was shown, and uploaded (the slot versions seen
    /// holding it).
    shown: u32,
    uploaded: u32,
    /// Its slot's [`Yuv::hash`] at its first showing.
    hash: u64,
    /// The showings that weren't the truth's, and the first of them.
    wrong: u32,
    first_wrong: Option<Showing>,
}

/// The engine's counts at the end of a slot pass, since its open.
#[derive(Debug, Default, Clone)]
struct PassCounts {
    resyncs: u64,
    unpaired: u64,
    /// Each lens's frames decoded.
    decoded: Vec<u64>,
    /// Frames uploaded into slots, and of those: copied out of FFmpeg's
    /// Vulkan images (path B), imported as DMA-BUFs (path A), and left on
    /// the device but refused by the GPU.
    uploaded: u64,
    copied: u64,
    imported: u64,
    refused: u64,
}

impl PassCounts {
    /// The frames uploaded through system memory: decoded there, or
    /// downloaded by the decoder or the upload thread.
    fn downloaded(&self) -> u64 {
        self.uploaded.saturating_sub(self.copied + self.imported)
    }
}

/// A pass's controls, planted at its first slot read: the slot judged as
/// its lens's next frame, and as the other lens's frame at its place.
#[derive(Debug)]
struct Controls {
    lens: usize,
    late: Result<(), Wrong>,
    crossed: Option<Result<(), Wrong>>,
}

/// What a slot pass found.
#[derive(Debug, Default)]
struct SlotPass {
    /// By lens and pts.
    keys: BTreeMap<(usize, i64), Seen>,
    counts: PassCounts,
    lock: Option<LockStatus>,
    elapsed: Duration,
    /// Why the pass is broken, a failure; or why it kept no frame on the
    /// GPU here, a skip.
    broken: Option<String>,
    not_kept: Option<String>,
    controls: Option<Controls>,
}

impl SlotPass {
    /// Each frame's slot hash, at its first showing.
    fn hashes(&self) -> BTreeMap<(usize, i64), u64> {
        self.keys.iter().map(|(&k, s)| (k, s.hash)).collect()
    }

    /// The frames shown other than the truth's, at least once.
    fn wrong(&self) -> impl Iterator<Item = (&(usize, i64), &Seen)> {
        self.keys.iter().filter(|(_, s)| s.wrong > 0)
    }
}

/// Whether a locked pass's lock held: locked at its end, no pair shown
/// apart.
fn lock_held(p: &SlotPass) -> Result<(), String> {
    if p.lock == Some(LockStatus::Locked) && p.counts.unpaired == 0 {
        Ok(())
    } else {
        Err(format!(
            "the lock ended {:?} with {} unpaired presents",
            p.lock, p.counts.unpaired
        ))
    }
}

/// `count` presents of `streams` of `path`, opened as `run` on `rig` and
/// played unthrottled, each slot shown read back from the GPU (level 0,
/// which the mips are made from) and judged against `truth`, byte for
/// byte; with `idle`, each present's reads wait first for the GPU to
/// finish the work submitted so far. A wrong slot is read again while
/// held, and its frame looked up in the cache. There is no [`PoolWatch`]:
/// it takes FFmpeg's frame lock from another thread and holds frames,
/// which moves the timing under test. Broken when the run fails, stalls
/// or ends short, a locked run's lock doesn't hold, a kept run's frame
/// came to its slot through system memory or was refused by the GPU, or
/// a copied run's came on the GPU; not kept when a kept run took no frame
/// on the GPU at all.
fn slot_pass(
    rig: &Rig,
    path: &Path,
    streams: &[Option<usize>],
    run: &Run,
    count: usize,
    idle: bool,
    truth: &Truth,
) -> SlotPass {
    let begun = Instant::now();
    let mut pass = SlotPass::default();
    let Some(lenses) = streams
        .iter()
        .map(|&s| truth.lens_of(s))
        .collect::<Option<Vec<usize>>>()
    else {
        pass.broken = Some(format!("the truth has no lens of {streams:?}"));
        return pass;
    };
    let clock = Arc::new(ManualClock::new(FrameRate::FPS_30.frame_duration()));
    let player = match rig.player(clock.clone(), run) {
        Ok(p) => p,
        Err(e) => {
            pass.broken = Some(e);
            return pass;
        }
    };
    // Each upload seen, by slot and version: the frame it holds.
    let mut uploads: HashMap<(usize, u64), (usize, i64)> = HashMap::new();
    let played = (|| -> Result<(), String> {
        open(&player, path, streams, run)?;
        let (mut next, mut rate) = (None, None);
        for n in 0..count {
            if let Some(t) = next {
                clock.set(t, Instant::now());
            }
            let (asked, wait) = (Instant::now(), if n == 0 { FIRST_FRAME } else { STALL });
            let p = loop {
                for v in player.slots().views() {
                    if let Some(&lens) = lenses.get(v.frame.stream) {
                        uploads.insert((v.index, v.word >> 3), (lens, v.frame.pts));
                    }
                }
                if let Some(p) = player.present(Instant::now())
                    && p.frames.len() == streams.len()
                    && next.is_none_or(|t| p.frames.iter().all(|f| ends(&f.frame, rate) > Some(t)))
                {
                    break p;
                }
                stream_error(&player)?;
                if player.snapshot().at_end {
                    return Err(format!("the file ended {n} presents in"));
                }
                if asked.elapsed() > wait {
                    return Err(format!("it stalled {n} presents in: none after {wait:?}"));
                }
                std::thread::sleep(Duration::from_micros(250));
            };
            look(rig, &player, truth, &lenses, idle, &p, &mut pass)?;
            if n == 0 {
                rate = stream_rate(&player);
                player.play();
            }
            let shown = p.frames[0].frame;
            next = Some(
                ends(&shown, rate)
                    .ok_or_else(|| format!("frame at {:?} has no duration", shown.time))?,
            );
        }
        Ok(())
    })();
    let snap = common::snapshot(&player);
    let perf = player.perf();
    pass.counts = PassCounts {
        resyncs: perf.count(Counter::PairResyncs),
        unpaired: perf.count(Counter::UnpairedPresents),
        decoded: snap.streams.iter().map(|s| s.decoded).collect(),
        uploaded: perf.count(Counter::FramesUploaded),
        copied: perf.count(Counter::VulkanCopies),
        imported: perf.count(Counter::FramesImported),
        refused: perf.count(Counter::ImportsRefused),
    };
    pass.lock = snap.lock.map(|l| l.status);
    for key in uploads.into_values() {
        if let Some(seen) = pass.keys.get_mut(&key) {
            seen.uploaded += 1;
        }
    }
    let joined = join(player);
    pass.elapsed = begun.elapsed();
    let c = &pass.counts;
    let on_gpu = c.copied + c.imported;
    let mut broken = played.err();
    if broken.is_none() && run.locked {
        broken = lock_held(&pass).err();
    }
    if run.zero_copy != ZeroCopy::Off {
        if on_gpu == 0 {
            pass.not_kept = Some(format!(
                "no frame taken on the GPU, {} refused{}",
                c.refused,
                because(snap.import_refused.as_ref())
            ));
        } else if broken.is_none() && (c.downloaded() > 0 || c.refused > 0) {
            broken = Some(format!(
                "{} of its {} frames uploaded came through system memory, {} refused by the \
                 GPU{}",
                c.downloaded(),
                c.uploaded,
                c.refused,
                because(snap.import_refused.as_ref())
            ));
        }
    } else if broken.is_none() && on_gpu > 0 {
        broken = Some(format!("a copied run took {on_gpu} frames on the GPU"));
    }
    if let Err(e) = joined {
        broken = Some(broken.map_or(e.clone(), |b| format!("{b}; {e}")));
    }
    pass.broken = broken;
    pass
}

/// Read back each slot `p` shows, as lens `lenses[stream]`, and judge it
/// against `truth`, into `pass`: a wrong one read again and looked up in
/// `player`'s cache. The pass's first read also plants its controls.
fn look(
    on: &Rig,
    player: &Player,
    truth: &Truth,
    lenses: &[usize],
    idle: bool,
    p: &Presented,
    pass: &mut SlotPass,
) -> Result<(), String> {
    if idle {
        on.gpu.wait_for_submitted().map_err(|e| e.to_string())?;
    }
    for f in &p.frames {
        let lens = *lenses
            .get(f.stream)
            .ok_or_else(|| format!("a present showed stream {}", f.stream))?;
        let pts = f.frame.pts;
        let slot = read_slot(&on.gpu, f)?;
        let index = truth.index(lens, pts);
        if pass.controls.is_none()
            && let Some(i) = index
        {
            let other = (lens + 1) % truth.lenses.len();
            pass.controls = Some(Controls {
                lens,
                late: truth.judge(&slot, lens, i + 1),
                crossed: (other != lens).then(|| truth.judge(&slot, other, i)),
            });
        }
        let judged = index.map_or(Err(Wrong::NotInTruth), |i| truth.judge(&slot, lens, i));
        let seen = pass.keys.entry((lens, pts)).or_default();
        seen.index = index;
        seen.shown += 1;
        if seen.shown == 1 {
            seen.hash = slot.hash();
        }
        let Err(wrong) = judged else {
            continue;
        };
        seen.wrong += 1;
        if seen.first_wrong.is_some() {
            continue;
        }
        let again = match read_slot(&on.gpu, f) {
            Ok(s) if s == slot => Again::Same,
            Ok(s) if truth.holds(&s, lens, index) => Again::Right,
            Ok(_) => Again::Other,
            Err(e) => Again::Failed(e),
        };
        seen.first_wrong = Some(Showing {
            slot: f.slot,
            run: f.frame.run,
            seq: f.frame.seq,
            wrong,
            again,
            cached: cached(player, f, &slot, truth, lens, index),
        });
    }
    Ok(())
}

/// What `player`'s cache holds now of the frame `f` shows, against the
/// truth's frame `index` of lens `lens` and against `slot`.
fn cached(
    player: &Player,
    f: &ShownFrame,
    slot: &Yuv,
    truth: &Truth,
    lens: usize,
    index: Option<usize>,
) -> Cached {
    let Some(frame) = player.cache().get(f.stream, f.frame.time) else {
        return Cached::Gone;
    };
    let yuv = match frame.to_system() {
        Some(Ok(down)) => yuv_of(&down),
        Some(Err(e)) => Err(e.to_string()),
        None => yuv_of(frame.as_ref()),
    };
    match yuv {
        Ok(y) if truth.holds(&y, lens, index) => Cached::Right,
        Ok(y) if y == *slot => Cached::AsSlot,
        Ok(_) => Cached::Other,
        Err(e) => Cached::Failed(e),
    }
}

/// The controls a pass planted (M3 plan 4.1, rule 1): its first slot
/// judged as its lens's next frame and as the other lens's frame must
/// fail, and [`Truth::like`] must name the frame it is: a frame early of
/// its own lens, and the other lens's at its place.
fn check_controls(c: &Controls) {
    let late = must_fail("a slot judged as its lens's next frame", c.late.clone());
    assert_eq!(
        late.like().map(|l| (l.lens, l.off)),
        Some((c.lens, -1)),
        "a slot judged as its lens's next frame: {late}"
    );
    if let Some(crossed) = &c.crossed {
        let crossed = must_fail("a slot judged as the other lens's frame", crossed.clone());
        assert_eq!(
            crossed.like().map(|l| (l.lens, l.off)),
            Some((c.lens, 0)),
            "a slot judged as the other lens's frame: {crossed}"
        );
    }
}

/// An 8-bit 4:2:0 layout of `components` a plane: `[1, 2]` NV12's,
/// `[1, 1, 1]` I420's.
fn layout_420(name: &str, components: &[u8]) -> PixelLayout {
    PixelLayout {
        name: name.into(),
        planes: (0..)
            .zip(components)
            .map(|(i, &components)| PlaneDesc {
                components,
                bytes: SampleBytes::One,
                log2_sub: if i == 0 { (0, 0) } else { (1, 1) },
            })
            .collect(),
        family: ColorFamily::Yuv,
        bit_depth: 8,
        msb_aligned: false,
    }
}

/// [`read_planes`] and [`yuv_of`] on the slots path A, path B and the
/// copy path fill on a hardware decoder, NV12's (R8 and Rg8), which the
/// Tier 1 players' software decode never fills: a 70×130 picture (three
/// bands, odd chroma rows) given as NV12 and as I420, rows padded, is the
/// same [`Yuv`] in memory, and uploaded to a slot of each reads back as
/// it, which the truth judges its frame. The control: the NV12 frame with
/// U and V swapped reads back with Y right and U and V wrong.
#[test]
fn nv12_and_i420_slots_read_back_as_one_picture() {
    let gpu = common::gpu();
    let size: (u32, u32) = (70, 130);
    let chroma = (size.0.div_ceil(2), size.1.div_ceil(2));
    let plane = |p: u32, (width, height): (u32, u32)| Plane {
        width,
        height,
        samples: (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    ((p * 7919 + y * 104_729 + x * 31).wrapping_mul(2_654_435_761) >> 24) as u8
                })
            })
            .collect(),
    };
    let want = Yuv([plane(0, size), plane(1, chroma), plane(2, chroma)]);
    let [y, u, v] = &want.0;
    let uv = |a: &Plane, b: &Plane| -> Vec<u8> {
        a.samples
            .iter()
            .zip(&b.samples)
            .flat_map(|(&a, &b)| [a, b])
            .collect()
    };
    let color = ColorInfo {
        codes: ColorCodes::UNSPECIFIED,
        range: Range::Limited,
        matrix: Matrix::Bt709,
        transfer: spherecut_core::video::Transfer::Sdr,
        primaries: Primaries::Bt709,
        siting: ChromaSiting::Left,
        assumed: Assumed::default(),
    };
    // Each plane given as rows of `row` bytes, then 5 bytes of padding.
    let frame = |name: &str, components: &[u8], planes: [(u32, &[u8]); 3]| {
        let (planes, strides): (Vec<Vec<u8>>, Vec<usize>) = planes[..components.len()]
            .iter()
            .map(|&(row, packed)| {
                let row = row as usize;
                let padded = packed
                    .chunks(row)
                    .flat_map(|r| r.iter().copied().chain([0xa5; 5]))
                    .collect();
                (padded, row + 5)
            })
            .unzip();
        let layout = Arc::new(layout_420(name, components));
        VideoFrame::owned(layout, size, planes, strides, color, (0, Time::ZERO)).unwrap()
    };
    let (vu, uv) = (uv(v, u), uv(u, v));
    let nv12 = frame(
        "nv12",
        &[1, 2],
        [(size.0, &y.samples), (2 * chroma.0, &uv), (0, &[])],
    );
    let i420 = frame(
        "yuv420p",
        &[1, 1, 1],
        [
            (size.0, &y.samples),
            (chroma.0, &u.samples),
            (chroma.0, &v.samples),
        ],
    );
    let nv21 = frame(
        "nv12",
        &[1, 2],
        [(size.0, &y.samples), (2 * chroma.0, &vu), (0, &[])],
    );
    let mut uploader = Uploader::new(gpu);
    let mut read_back = |f: &VideoFrame| {
        let slot = GpuFrame::new(gpu, &SlotShape::of(f.layout(), size).unwrap()).unwrap();
        uploader.upload(gpu, f, &slot).unwrap();
        let formats: Vec<_> = (0..)
            .map_while(|i| slot.plane_texture(i).map(|t| t.format()))
            .collect();
        (formats, read_planes(gpu, &slot))
    };
    let truth = Truth {
        lenses: vec![TruthLens {
            stream: Some(0),
            frames: vec![(0, want.clone())],
        }],
    };

    let (r8, rg8) = (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm);
    for (f, formats) in [(&nv12, vec![r8, rg8]), (&i420, vec![r8; 3])] {
        assert_eq!(yuv_of(f), Ok(want.clone()), "{}", f.layout().name);
        let (got, slot) = read_back(f);
        assert_eq!(got, formats, "{}", f.layout().name);
        let slot = slot.unwrap();
        truth.judge(&slot, 0, 0).unwrap();
        assert_eq!(slot, want);
    }

    // Control.
    let (_, swapped) = read_back(&nv21);
    let wrong = must_fail(
        "an NV12 slot with U and V swapped",
        truth.judge(&swapped.unwrap(), 0, 0),
    );
    let Wrong::Samples(d) = &wrong else {
        panic!("{wrong}")
    };
    assert_eq!(
        d.planes.each_ref().map(|p| p.samples > 0),
        [false, true, true],
        "{wrong}"
    );
}

/// Whether two passes' slots, `got` and `want` by lens and pts, hold the
/// same frames byte for byte; the frames that differ, or the frames one
/// showed and the other didn't. The frames compared.
fn same_slots(
    got: &BTreeMap<(usize, i64), u64>,
    want: &BTreeMap<(usize, i64), u64>,
) -> Result<usize, String> {
    if got.keys().ne(want.keys()) {
        return Err(format!(
            "the passes showed other frames: {:?} against {:?}",
            got.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>()
        ));
    }
    let differ: Vec<String> = got
        .iter()
        .filter(|(k, v)| want.get(k) != Some(v))
        .map(|((lens, pts), _)| format!("lens {lens} pts {pts}"))
        .collect();
    if differ.is_empty() {
        return Ok(got.len());
    }
    let shown = differ.len().min(8);
    Err(format!(
        "{} of {} frames differ: {}{}",
        differ.len(),
        got.len(),
        differ[..shown].join(", "),
        if differ.len() > shown { ", …" } else { "" }
    ))
}

/// `line` on stderr and as a bench note, so that it lands in hw-test's
/// report.
fn say(line: &str) {
    eprintln!("{line}");
    bench::note(line);
}

/// A way the zero-copy pair check plays a file.
struct Variant {
    name: &'static str,
    /// The lenses it opens, by their place among the file's.
    lenses: &'static [usize],
    locked: bool,
    /// Each present's reads wait first for the GPU to finish the work
    /// submitted so far.
    idle: bool,
}

/// The ways: locked, as the app plays a pair, first; then, on clip 9's
/// path B, where odin's mismatches came, unlocked, each lens alone
/// through the player, and locked with each read after the GPU idles.
const VARIANTS: [Variant; 5] = [
    Variant {
        name: "locked",
        lenses: &[0, 1],
        locked: true,
        idle: false,
    },
    Variant {
        name: "unlocked",
        lenses: &[0, 1],
        locked: false,
        idle: false,
    },
    Variant {
        name: "lens 0 alone",
        lenses: &[0],
        locked: false,
        idle: false,
    },
    Variant {
        name: "lens 1 alone",
        lenses: &[1],
        locked: false,
        idle: false,
    },
    Variant {
        name: "locked, read idle",
        lenses: &[0, 1],
        locked: true,
        idle: true,
    },
];

/// A variant of the zero-copy pair check played on one file and path,
/// and its passes in the order run, each named, and kept or copied.
struct Played {
    file: String,
    path: &'static str,
    variant: &'static str,
    passes: Vec<(&'static str, bool, SlotPass)>,
}

impl Played {
    /// What a wrong frame is over the variant's passes: kept wrong (only
    /// the kept passes showed it wrong), copy wrong, or both wrong.
    fn class(&self, key: &(usize, i64)) -> &'static str {
        let wrong = |kept: bool| {
            self.passes
                .iter()
                .any(|(_, k, p)| *k == kept && p.keys.get(key).is_some_and(|s| s.wrong > 0))
        };
        match (wrong(true), wrong(false)) {
            (true, true) => "both wrong",
            (true, false) => "kept wrong",
            (false, true) => "copy wrong",
            (false, false) => "wrong",
        }
    }

    /// A pass's row: its frames, the wrong ones, its verdict and the
    /// engine's counts.
    fn row(&self, name: &str, p: &SlotPass) -> String {
        let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
        for (k, _) in p.wrong() {
            *classes.entry(self.class(k)).or_default() += 1;
        }
        let verdict = if let Some(why) = &p.broken {
            format!("broken: {why}")
        } else if !classes.is_empty() {
            classes
                .iter()
                .map(|(c, n)| format!("{c} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        } else if let Some(why) = &p.not_kept {
            format!("not kept: {why}")
        } else {
            "right".into()
        };
        let twice: Vec<String> = p
            .keys
            .iter()
            .filter(|(_, s)| s.shown > 1)
            .map(|((lens, pts), s)| format!("lens {lens} pts {pts} x{}", s.shown))
            .collect();
        let c = &p.counts;
        format!(
            "{}, {}, {}, {name}: {} frames, {} wrong: {verdict}; resyncs {}, unpaired {}, \
             decoded {}, uploaded {} ({} copied, {} imported, {} downloaded, {} refused), \
             shown twice: {}; {:.1} s",
            self.file,
            self.path,
            self.variant,
            p.keys.len(),
            p.wrong().count(),
            c.resyncs,
            c.unpaired,
            c.decoded
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("+"),
            c.uploaded,
            c.copied,
            c.imported,
            c.downloaded(),
            c.refused,
            if twice.is_empty() {
                "none".into()
            } else {
                twice.join(", ")
            },
            p.elapsed.as_secs_f64()
        )
    }

    /// Each side's passes against each other, slot for slot: "copy 1
    /// against copy 2: the same 96 frames; kept 1 against kept 2: …".
    fn steadiness(&self) -> String {
        let side = |kept: bool| -> Vec<&(&str, bool, SlotPass)> {
            self.passes.iter().filter(|(_, k, _)| *k == kept).collect()
        };
        [false, true]
            .into_iter()
            .filter_map(|kept| match side(kept).as_slice() {
                [(a, _, p), (b, _, q)] => Some(format!(
                    "{a} against {b}: {}",
                    match same_slots(&p.hashes(), &q.hashes()) {
                        Ok(n) => format!("the same {n} frames"),
                        Err(e) => e,
                    }
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// One line for wrong frame `key` of pass `name`, `p`.
    fn key_line(&self, name: &str, (lens, pts): (usize, i64), s: &Seen) -> String {
        let at = s.index.map_or_else(|| "?".into(), |i| i.to_string());
        let head = format!(
            "{}, {}, {}, {name}: lens {lens} pts {pts} (frame {at}): {}; shown {}, uploaded {}, \
             wrong {}",
            self.file,
            self.path,
            self.variant,
            self.class(&(lens, pts)),
            s.shown,
            s.uploaded,
            s.wrong
        );
        match &s.first_wrong {
            Some(w) => format!(
                "{head}; slot {} run {} seq {}; read again: {}; in the cache: {}; {}",
                w.slot, w.run, w.seq, w.again, w.cached, w.wrong
            ),
            None => head,
        }
    }
}

/// The zero-copy pair check's report, as [`say`]s: a row a pass, a line a
/// variant on its passes' steadiness, then each wrong frame, at most
/// [`LISTED`] a pass. What failed, in words: nothing when every pass
/// showed the truth's frames and none broke.
fn pair_report(played: &[Played]) -> Vec<String> {
    say(
        "zero-copy pair check: file, path, variant, pass: frames, wrong, verdict; the engine's counts",
    );
    for g in played {
        for (name, _, p) in &g.passes {
            say(&g.row(name, p));
        }
        say(&format!(
            "{}, {}, {}: {}",
            g.file,
            g.path,
            g.variant,
            g.steadiness()
        ));
    }
    for g in played {
        for (name, _, p) in &g.passes {
            let wrong: Vec<_> = p.wrong().collect();
            for &(&key, s) in wrong.iter().take(LISTED) {
                say(&g.key_line(name, key, s));
            }
            if wrong.len() > LISTED {
                say(&format!(
                    "{}, {}, {}, {name}: {} wrong frames more",
                    g.file,
                    g.path,
                    g.variant,
                    wrong.len() - LISTED
                ));
            }
        }
    }
    let passes = || {
        played
            .iter()
            .flat_map(|g| g.passes.iter().map(move |p| (g, p)))
    };
    let wrong_in = |kept: bool| {
        passes()
            .filter(|(_, (_, k, p))| *k == kept && p.wrong().next().is_some())
            .count()
    };
    let mut failed = Vec::new();
    match wrong_in(true) {
        0 => {}
        n => failed.push(format!(
            "{n} kept passes showed frames other than the truth's"
        )),
    }
    match wrong_in(false) {
        0 => {}
        n => failed.push(format!(
            "copy wrong: {n} copied passes showed frames other than the truth's, an upstream \
             decode fault, not path B's"
        )),
    }
    let broken: Vec<String> = passes()
        .filter_map(|(g, (name, _, p))| {
            let why = p.broken.as_ref()?;
            Some(format!(
                "{}, {}, {}, {name}: {why}",
                g.file, g.path, g.variant
            ))
        })
        .collect();
    if !broken.is_empty() {
        failed.push(format!(
            "{} passes broke: {}",
            broken.len(),
            broken.join("; ")
        ));
    }
    failed
}

/// Every slot a locked pair shows, kept on the GPU (path B where the
/// device can be shared, and path A where the GPU imports VA-API's
/// frames) or copied on the same decoder, is the frame a software decode
/// of the file makes, byte for byte: the first [`ZERO_COPY_FRAMES`] pairs
/// of clip 9 and of the 2×3840² perf pair, more than path B's cache
/// holds on the device a lens (M3 plan section 9). Each variant runs a
/// copied pass, a kept one, and each again, so that each side's
/// steadiness shows; clip 9's path B also plays unlocked, each lens
/// alone, and locked with each read after the GPU idles. A wrong slot is
/// told apart: its kept or copied side, its planes' differences and
/// bands, whether those bands are a frame of either lens near it, its
/// slot, run and seq, a second read, its frame in the cache, and how
/// often it was shown and uploaded. Every pass and file runs before the
/// test fails, and the report (one row a pass, one line a wrong frame)
/// goes to stderr and the bench notes. Each pass plants controls: its
/// first slot judged as its lens's next frame, and as the other lens's,
/// must fail as the frame it is. A kept pass any of whose frames came
/// through system memory or was refused by the GPU, or a pass that
/// fails, stalls, ends short or shows the lenses apart locked, fails it;
/// one that keeps no frame on the GPU at all skips, as a file or path not
/// here does, never a pass with one left out.
#[test]
#[ignore = "tier2: needs a GPU that keeps decoded frames on it, and the X5 samples or hw-test's perf pairs"]
fn zero_copy_pair_matches_copy() {
    tier2::require_hw_run();
    let begun = Instant::now();
    let (files, mut missing) = locked_files();
    if files.is_empty() {
        tier2::skip(missing.join("; "));
    }
    let rig = Rig::hardware();
    say(&rig.about());
    let shared = rig.shared();
    let mut played: Vec<Played> = Vec::new();
    let mut failed = Vec::new();
    for (name, path, coded) in &files {
        let report = probe_file(path);
        let streams = lens_streams(&report);
        let format = format_of(&report, streams[0]);
        let check = format.check_format().unwrap_or("this format");
        let mut paths: Vec<(&str, &Rig, HwKind, ZeroCopy)> = Vec::new();
        match &shared {
            Ok(on) if rig.hw.works(check, HwKind::Vulkan) => {
                paths.push(("path B", on, HwKind::Vulkan, ZeroCopy::Vulkan));
            }
            Ok(_) => missing.push(format!(
                "{name}: path B: Vulkan Video doesn't decode {check}"
            )),
            Err(why) => missing.push(format!("{name}: path B: the device can't be shared: {why}")),
        }
        if !rig.gpu.caps.dmabuf_import {
            missing.push(format!("{name}: path A: the GPU can't import DMA-BUFs"));
        } else if !rig.hw.works(check, HwKind::VaApi) {
            missing.push(format!("{name}: path A: VA-API doesn't decode {check}"));
        } else {
            paths.push(("path A", &rig, HwKind::VaApi, ZeroCopy::All));
        }
        if paths.is_empty() {
            continue;
        }
        let decoding = Instant::now();
        let truth = match Truth::decode(path, &streams, ZERO_COPY_FRAMES + REACH) {
            Ok(t) => t,
            Err(e) => {
                failed.push(format!("{name}: decoding the truth in software: {e}"));
                continue;
            }
        };
        say(&format!(
            "{name}: the truth, {} frames a lens decoded in software in {:.1} s",
            truth
                .lenses
                .iter()
                .map(|l| l.frames.len().to_string())
                .collect::<Vec<_>>()
                .join(" and "),
            decoding.elapsed().as_secs_f64()
        ));
        for (label, on, kind, zero_copy) in paths {
            // Clip 9 is the sample, the file without corner codes.
            let variants = if coded.is_none() && kind == HwKind::Vulkan {
                &VARIANTS[..]
            } else {
                &VARIANTS[..1]
            };
            for v in variants {
                let streams: Vec<Option<usize>> = v.lenses.iter().map(|&l| streams[l]).collect();
                let as_variant = |r: Run| if v.locked { r.locked() } else { r };
                let kept = as_variant(Run {
                    label: format!("{kind}, {label}"),
                    zero_copy,
                    ..Run::only(kind)
                });
                let copy = as_variant(Run::only(kind));
                let mut g = Played {
                    file: name.clone(),
                    path: label,
                    variant: v.name,
                    passes: Vec::new(),
                };
                for (pass, is_kept) in [
                    ("copy 1", false),
                    ("kept 1", true),
                    ("copy 2", false),
                    ("kept 2", true),
                ] {
                    let (on, run) = if is_kept { (on, &kept) } else { (&rig, &copy) };
                    let p = slot_pass(on, path, &streams, run, ZERO_COPY_FRAMES, v.idle, &truth);
                    if let Some(why) = p.not_kept.as_ref().filter(|_| is_kept) {
                        let why = format!("{name}, {}: {why}", kept.label);
                        if !missing.contains(&why) {
                            missing.push(why);
                        }
                    }
                    g.passes.push((pass, is_kept, p));
                    // As it goes, should a later pass never end.
                    if let Some((pass, _, p)) = g.passes.last() {
                        eprintln!("{}", g.row(pass, p));
                    }
                }
                played.push(g);
            }
        }
    }
    failed.extend(pair_report(&played));
    say(&format!(
        "the zero-copy pair check took {:.0} s",
        begun.elapsed().as_secs_f64()
    ));
    for (_, _, p) in played.iter().flat_map(|g| &g.passes) {
        if let Some(c) = &p.controls {
            check_controls(c);
        }
    }
    if !failed.is_empty() {
        panic!(
            "{}; the rows and the frames above say where",
            failed.join("; ")
        );
    }
    if !missing.is_empty() {
        tier2::skip(missing.join("; "));
    }
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
