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
//! over, up to the presents' cadence (skipped: a clip faster than the
//! presents passes them over by design) and beyond it (dropped), the
//! frames a decoder behind the clock skipped (decode-dropped), and the
//! presents whose frame had already ended when it went on screen (late). A note gives each stage's p50 and p95 over
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
//!   when an unthrottled pass stalls (no frame for 5 s), as it does when
//!   the engine stops, or P-LOCK's verdict is Kill. Then hw-test's
//!   two-track perf clips, both tracks opened as a sample's are,
//!   unthrottled only: on Auto's hardware as
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
//!   path B's pool, a note beside each run, locked or not, of
//!   `MemAvailable` as each of its passes began, and a verdict a mode
//!   (Pass, Redirect to 8 slots, or Kill): at 8K30, 5.7K60 and 4K120 no
//!   pair apart, a new pair at 99 % or more of the real-time presents
//!   that owed one, and at most 1 point more dropped a lens than
//!   unlocked; at 8K25 on path B no pair apart and the pacing rule met,
//!   with the pairs and `pair_wait` of each second that dropped. A
//!   present owes a frame when its clock ran into a later one since the
//!   present before, short of the end, or when it is a loop's first after
//!   the seek back, so a rate above the presents' and the loops of a
//!   short master owe only what can be shown ([`Owed`]).
//!   The engine counts a locked pair's drops (and skips) once a pair and
//!   an unlocked player's for each stream, so the unlocked count is
//!   divided by its streams; the frames the presents' cadence passes over
//!   (4K120 at 60 Hz, one a present) are skipped, not dropped. A
//!   real-time pass whose frame stands still for 5 s while its clock runs
//!   has stalled, which with a pair apart or the lock undone is Kill; a
//!   note sums every mode's verdict up.
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
//!   wrong one that says where and how it is wrong, and whether it is on
//!   the fault odin's GFX11 RADV showed with DCC on (the check's docs say
//!   what each outcome means).
//!
//! Where the GPU, hardware decode of the format, the perf clips or the
//! samples aren't there, a benchmark records what it can and skips with
//! the reason, which never counts as a pass. A run that only records and
//! fails is a note saying why, but an unthrottled pass's stall, or
//! P-LOCK's Kill, fails the benchmark.
//! `the_bench_harness_plays_on_lavapipe` runs the same harness in Tier 1,
//! in software on a small clip, with one stream and with two, and
//! `the_bench_harness_plays_both_tracks_of_a_pair` plays a 2 × 64² pair
//! as the perf pairs are played, and `the_bench_harness_plays_a_locked_pair`
//! plays it locked, reading its corner codes and its slots back as Tier
//! 2h does, so that none can rot between hardware runs;
//! `p_lock_scores_the_presents_that_owe_a_frame_and_drops_a_lens` scores
//! synthetic passes as P-LOCK does; and
//! `a_zero_copy_mismatch_names_the_stage_that_parts_from_the_download`
//! holds the zero-copy checks' account of a wrong frame (its take, the
//! slot, or the render) to planes and a slot made on lavapipe. Run the
//! rest with `cargo xtask hw-test`.

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
use spherecut_core::{
    Clock, FrameRate, ManualClock, Rational, Rounding, StopToken, SystemClock, Time,
};
use spherecut_media::HwDevice;
use spherecut_media::backend::{
    DecodeMode, DecodePath, Fallback, FfmpegBackend, HwKind, HwOutput, MediaBackend, NextFrame,
    SeekMode, Transfer, VideoFrame, VideoRequest,
};
use spherecut_media::decode::VideoDecoder;
use spherecut_media::detect::Layout;
use spherecut_media::hw::{self, GpuIds, RenderNode};
use spherecut_media::probe::{self, ProbeReport, StreamKind};
use spherecut_playback::budget::mem_available;
use spherecut_playback::decoder::choose;
use spherecut_playback::{
    CacheBudget, Counter, DecoderPreference, HwSupport, LockSpec, LockStatus, PerfStats, Player,
    PlayerConfig, PlayerParts, PlayerSnapshot, Presented, SURFACE_CAP, ShownFrame, SlotFrame,
    SlotState, SlotStats, Stage, StreamFormat, ZeroCopy,
};
use spherecut_render::dmabuf::{self, Importer};
use spherecut_render::vkshare::Copied;
use spherecut_render::{
    AdapterChoice, DebugMode, Encoder, Filter, Gpu, GpuFrame, GpuTimers, Image, LinearTarget,
    Placement, Projector, RenderError, SlotShape, StillOptions, StillRenderer, Uploader,
    ViewParams, upload,
};
use spherecut_render::{readback, wgpu, yuv};
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
    /// New frames on screen, of the first stream: of a locked pair, new
    /// pairs.
    shown: u64,
    /// Presents of the real-time pass that owed a new frame ([`Owed`]).
    due: u64,
    /// Frames passed over beyond the presents' cadence, as the engine
    /// counts them: once a pair for a locked pair, summed over the streams
    /// otherwise ([`Pass::drops_over`]).
    dropped: u64,
    /// Frames passed over up to the presents' cadence (a clip faster than
    /// the presents), counted as `dropped` is.
    skipped: u64,
    /// Frames a decoder behind the clock skipped, summed over the
    /// streams: each worker counts its own.
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
    /// Why an unthrottled pass stopped before the end, or why a real-time
    /// pass stood still while its clock ran ([`Standing`]), if it did.
    stalled: Option<String>,
    /// The streams played, and the streams `dropped` and `skipped` sum
    /// over: 1 for a locked pair, whose frames passed over count once a
    /// pair (D67), else every stream's. A share a lens divides by it.
    streams: usize,
    drops_over: usize,
    /// `MemAvailable` as the pass began, in bytes, where `/proc/meminfo`
    /// says.
    mem_available: Option<u64>,
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
    /// dropped, decode-dropped, skipped.
    base: [u64; 4],
    /// Frames imported and imports refused at the start of the pass.
    imports: [u64; 2],
    /// Unpaired presents and pair resyncs at the start of the pass.
    pairs: [u64; 2],
    at: [u64; 4],
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
            base: [0; 4],
            imports: [0; 2],
            pairs: [0; 2],
            at: [0; 4],
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

    fn counts(&self) -> [u64; 4] {
        [
            Counter::FramesDecoded,
            Counter::Dropped,
            Counter::DecodeDropped,
            Counter::Skipped,
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

    /// Note a present that owed a new frame ([`Owed`]).
    fn due(&mut self) {
        self.current.due += 1;
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
                skipped: c[3] - self.at[3],
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

/// Which presents of a real-time pass owe a new frame (P-LOCK, M3 plan
/// 4.3): a present owes one when the clock ran into a later frame since
/// the present before, short of the clip's end, and the first present
/// after the seek back to the start that a loop makes owes the play's
/// first frame. So a present owes at most one, however many frames its
/// clock passed (119.88 fps shown at 60 Hz owes 60 a second), and none
/// while the clock stands: at the end, and through the seek, the landing
/// and the preroll of a loop, which a short master makes every 3 to 5 s.
/// The share presented is the new frames put up (new pairs, for a locked
/// pair), at whatever present, over the presents that owed one
/// ([`presented_share`]).
#[derive(Debug, Default)]
struct Owed {
    /// The time the present before was for.
    last: Option<Time>,
}

impl Owed {
    /// Whether the present for `time` (the clock one UI frame ahead, as
    /// [`Presented::time`] is), at `rate`, owes a new frame, with the
    /// clock `playing` or not and play `at_end` or not as the snapshot
    /// after it says, of a clip that ends at `end` where that is known.
    fn at(
        &mut self,
        time: Time,
        rate: FrameRate,
        playing: bool,
        at_end: bool,
        end: Option<Time>,
    ) -> bool {
        let frame = |t: Time| rate.time_to_frame(t, Rounding::Floor).unwrap_or(i64::MIN);
        let owed = match self.last {
            // Back to the start: a loop's seek, which owes the play's
            // first frame; or the end's present, which set the clock back
            // to the last frame and owes nothing.
            Some(before) if time < before => !at_end,
            Some(before) => playing && end.is_none_or(|e| time < e) && frame(time) > frame(before),
            None => false,
        };
        self.last = Some(time);
        owed
    }
}

/// How long a real-time pass's first stream has shown one frame while
/// its clock ran short of the end: a pass standing still for [`STALL`]
/// has stalled, as an unthrottled one with no frame for as long has, and
/// a locked pair's verdict is then Kill, since more slots wouldn't mend
/// it (P-LOCK, M3 plan 4.3).
#[derive(Debug)]
struct Standing {
    since: Instant,
}

impl Standing {
    fn new(now: Instant) -> Self {
        Standing { since: now }
    }

    /// How long the frame has stood at the present at `now`, which put a
    /// new frame up or not, with the clock `playing` short of the end
    /// (`at_end` false) or not: zero once it moves, or while the clock
    /// stands.
    fn at(&mut self, now: Instant, new: bool, playing: bool, at_end: bool) -> Duration {
        if new || !playing || at_end {
            self.since = now;
        }
        now.saturating_duration_since(self.since)
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
             skipped,decode_dropped,cache_waits,cache_frames,cache_ahead,cache_mib,\
             slots_ready,slots_writing,slots_free,minflt",
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
            "{:.2},{:.4},{shown},{},{},{loops},{},{},{},{},{},{},{},{frames},{ahead},{},{},{},{},{}",
            now.saturating_duration_since(self.begun).as_secs_f64() * 1000.0,
            snap.clock.to_secs_f64(),
            u8::from(snap.playing),
            u8::from(snap.at_end),
            count(Counter::FramesDecoded),
            count(Counter::FramesUploaded),
            count(Counter::FramesShown),
            count(Counter::Dropped),
            count(Counter::Skipped),
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

/// Where play ends: the earliest stream's end, once its decoding reached
/// it, else its length.
fn clip_end(snap: &PlayerSnapshot) -> Option<Time> {
    snap.streams
        .iter()
        .filter_map(|s| s.end.or_else(|| s.info.as_ref()?.duration))
        .min()
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
/// decodes, the slots and the lock, the pair resyncs, and the pool
/// `watch` saw. Off the UI thread: it reads the cache.
fn finish_pass(mut pass: Pass, player: &Player, watch: Option<PoolWatch>) -> Pass {
    let snap = common::snapshot(player);
    pass.paths = snap.streams.iter().map(|s| s.decode.clone()).collect();
    pass.refusal = snap.import_refused.clone();
    pass.rate = stream_rate(player);
    pass.lens_decoded = snap.streams.iter().map(|s| s.decoded).collect();
    pass.slots = snap.slots;
    pass.lock = snap.lock.map(|l| l.status);
    // From the cache, which counts each at once, where the engine's
    // counter takes them in every 250 ms; the pass's player is its own,
    // so these are the pass's since the open.
    pass.resyncs = player.cache().stats().resyncs;
    pass.streams = snap.streams.len();
    pass.drops_over = if pass.lock == Some(LockStatus::Locked) {
        1
    } else {
        pass.streams
    };
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
                let locked =
                    picture.with_locked(&mut |states| seen.extend(states.iter().map(|s| s.image)));
                assert!(
                    locked,
                    "a cached frame of stream {stream} couldn't be locked"
                );
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
        let mem_available = mem_available_now();
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
        let pass = meter.finish(Instant::now());
        // The cache's counts take its lock, which the UI thread mustn't.
        drop(ui);
        let mut pass = finish_pass(pass, &player, watch);
        pass.mem_available = mem_available;
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
/// deadlines for `length`, playing from the start again at the end. Each
/// present is scored as [`Owed`] says, and a pass whose frame stands
/// still for [`STALL`] while its clock runs is marked stalled
/// ([`Standing`]) and plays on.
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
        let mem_available = mem_available_now();
        player.play();
        let start = Instant::now();
        let mut meter = Meter::new(perf.clone(), streams.len(), start);
        let mut owed = Owed::default();
        let mut standing = Standing::new(start);
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
            let mut new = false;
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
                    new = true;
                    meter.shown();
                }
                if snap.playing
                    && p.frames
                        .iter()
                        .any(|f| ends(&f.frame, rate).is_some_and(|e| e <= p.time))
                {
                    meter.late();
                }
                if let Some(rate) = rate
                    && owed.at(p.time, rate, snap.playing, snap.at_end, clip_end(&snap))
                {
                    meter.due();
                }
            }
            let stood = standing.at(now, new, snap.playing, snap.at_end);
            if stood >= STALL && meter.pass.stalled.is_none() {
                meter.pass.stalled = Some(format!(
                    "no new frame for {stood:?} while the clock ran, at {:.3} s",
                    snap.clock.to_secs_f64()
                ));
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
        let pass = meter.finish(Instant::now());
        drop(ui);
        let mut pass = finish_pass(pass, &player, watch);
        pass.presents = presents;
        pass.samples = samples;
        pass.mem_available = mem_available;
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
        &format!("{name}: skipped after 1 s"),
        after(|s| s.skipped),
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
    if let Some(why) = &r.stalled {
        bench::note(format!("{name}: the real-time pass stood still: {why}"));
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

/// `MemAvailable` now, in bytes, where `/proc/meminfo` says.
fn mem_available_now() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| mem_available(&m))
}

/// The note beside a run's rows of `MemAvailable` as each of its
/// `passes` began, unthrottled then real time where both ran, so that a
/// locked run's rows and the unlocked run's beside them can be read
/// against the memory each had (copy-path slumps follow it, D42).
fn mem_note(name: &str, passes: &[&Pass]) -> String {
    let each: Vec<String> = passes
        .iter()
        .zip(["unthrottled", "real time"])
        .map(|(p, pass)| match p.mem_available {
            Some(bytes) => format!("{} MiB {pass}", bytes >> 20),
            None => format!("not read {pass}"),
        })
        .collect();
    format!(
        "{name}: MemAvailable as each pass began: {}",
        each.join(", ")
    )
}

/// A two-stream run's rows of P-LOCK's (M3 plan 4.3), locked or not,
/// from its `passes`, unthrottled then real time where both ran: the
/// unpaired presents over them all, each lens's decodes since the open,
/// path B's pool and the slots in the last, and a note of `MemAvailable`
/// as each pass began ([`mem_note`]); of a real-time pass, the share of
/// the presents that owed a frame that put one up, the frames dropped and
/// skipped a lens and `MemAvailable` as it began; of a locked run, its
/// pair resyncs, the pairs it presented, `pair_wait` and its lock. Both
/// runs record their slots, since the lock takes all of `MAX_SLOTS` and
/// the unlocked player its default.
fn record_pair(name: &str, run: &Run, passes: &[&Pass]) {
    let Some(last) = passes.last() else {
        return;
    };
    bench::note(mem_note(name, passes));
    let over = |of: fn(&Pass) -> u64| passes.iter().map(|p| of(p)).sum::<u64>() as f64;
    let real_time = passes.len() > 1;
    let pass = if real_time {
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
    bench::record(&format!("{name}: slots"), last.slots.active as f64, "slots");
    // An unthrottled pass owes no frame at a present: its clock waits for
    // every stream's.
    if real_time {
        if let Some(share) = presented_share(last) {
            bench::record(&format!("{name}: presented after 1 s"), share, "%");
        }
        if let Some(share) = dropped_share(last) {
            bench::record(&format!("{name}: dropped a lens after 1 s"), share, "%");
        }
        if let Some(share) = skipped_share(last) {
            bench::record(&format!("{name}: skipped a lens after 1 s"), share, "%");
        }
        if let Some(bytes) = last.mem_available {
            bench::record(
                &format!("{name}: MemAvailable as the real-time pass began"),
                bytes as f64 / f64::from(1u32 << 20),
                "MiB",
            );
        }
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
    for p in [50.0, 95.0] {
        if let Some(ms) = last.stage(Stage::PairWait, p) {
            bench::record(&format!("{name}: pair_wait p{p}, {pass}"), ms, "ms");
        }
    }
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

/// The presents of a real-time pass's full seconds after the first that
/// owed a new frame ([`Owed`]); `None` for none, as of an unthrottled
/// pass, which counts none.
fn due_after_first(r: &Pass) -> Option<f64> {
    let due = r.after_first(|s| s.due);
    (due > 0).then_some(due as f64)
}

/// The new frames put up in those seconds (new pairs, of a locked pair),
/// at whatever present, over those presents, in %: a frame put up a
/// present late, at a present that owed none (a frame lasts two presents
/// at 8K30), still counts: a pair a present late shows in the late
/// presents' row, not here. A loop's seek owes the play's first frame and its
/// landing puts it up unowed, so the two even out.
fn presented_share(r: &Pass) -> Option<f64> {
    Some(100.0 * r.after_first(|s| s.shown) as f64 / due_after_first(r)?)
}

/// The frames passed over a lens in those seconds beyond the presents'
/// cadence, as a share of those presents, in %: the engine counts a
/// locked pair's once a pair and an unlocked player's for every stream,
/// so the count is divided by [`Pass::drops_over`]. The frames a clip
/// faster than the presents passes over at each one by design are
/// skipped ([`skipped_share`]), on both sides of the comparison.
fn dropped_share(r: &Pass) -> Option<f64> {
    let a_lens = r.after_first(|s| s.dropped) as f64 / r.drops_over.max(1) as f64;
    Some(100.0 * a_lens / due_after_first(r)?)
}

/// The frames passed over a lens in those seconds up to the presents'
/// cadence, as [`dropped_share`] counts those beyond it: about 100 % for
/// 4K120 at 60 Hz, a frame a present, and 0 at or below the present
/// rate.
fn skipped_share(r: &Pass) -> Option<f64> {
    let a_lens = r.after_first(|s| s.skipped) as f64 / r.drops_over.max(1) as f64;
    Some(100.0 * a_lens / due_after_first(r)?)
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
/// shown apart, the lock refused or undone (the slot floor), a stall of
/// either pass ([`Standing`] for the real-time one's).
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
/// put a new pair up at 99 % or more of the presents that owed one
/// ([`presented_share`]) and dropped at most 1 percentage point more a
/// lens than `unlocked`'s real-time pass ([`dropped_share`]), with no
/// pair apart. The line gives both runs' shares.
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
            "the real-time passes owed no frame after the first second".into(),
        );
    };
    let was = presented_share(unlocked).unwrap_or(f64::NAN);
    let skips = [r, unlocked].map(|p| skipped_share(p).unwrap_or(f64::NAN));
    let line = format!(
        "0 unpaired, presented {shown:.2} % of the presents that owed a frame after 1 s \
         against {was:.2} % unlocked, dropped {dropped:.2} % a lens against {before:.2} % \
         (skipped {:.2} % against {:.2} %)",
        skips[0], skips[1]
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
/// second that dropped, and the drops and decode drops of each a lens. A
/// pair that trailed by [`STALL`]'s worth of frames or more, or showed
/// none, froze, which more slots wouldn't mend: Kill, as a stall is.
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
                "second {i}: {:.1} dropped a lens ({:.1} decode-dropped a lens), {} pairs, \
                 pair_wait {:.1} ms at most",
                s.dropped as f64 / r.drops_over.max(1) as f64,
                s.decode_dropped as f64 / r.streams.max(1) as f64,
                s.shown,
                s.pair_wait
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
    let frozen = (STALL.as_secs_f64() * rate.to_f64()).ceil() as i64;
    match pacing::pacing_cut(&r.samples, rate) {
        Ok(paced) => (
            Verdict::Pass,
            format!("0 unpaired, the pacing rule met ({paced:?}); {drops}"),
        ),
        Err(why @ pacing::Unpaced::Trailed { frames, .. }) if frames >= frozen => (
            Verdict::Kill,
            format!("0 unpaired, the pair froze: {why:?}; {drops}"),
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
/// `stalls`; the verdict comes back, `None` where none was decided, with
/// its line, which gives both runs' slots.
fn p_lock(
    rig: &Rig,
    mode: &str,
    path: &Path,
    report: &ProbeReport,
    auto: &Run,
    copied: Option<&(Pass, Pass)>,
    stalls: &mut Vec<String>,
) -> (Option<Verdict>, String) {
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
        return (
            Some(Verdict::Kill),
            format!("{name}: Kill: the locked run didn't play (see its note)"),
        );
    };
    // The baseline's slots beside the lock's: the lock takes all of
    // MAX_SLOTS and the unlocked player its default.
    let slots = format!(
        "{} slots locked, {} unlocked",
        locked.1.slots.active,
        unlocked.map_or_else(|| "?".into(), |(_, r)| r.slots.active.to_string())
    );
    let (verdict, why) = match class {
        Some(Mode::K8_25) if !base.path_b() => {
            return (
                None,
                format!(
                    "{name}: not decided: 8K25's verdict is on path B, and {}",
                    shared.err().unwrap_or_else(|| "the run copies".into())
                ),
            );
        }
        Some(Mode::K8_25) => paced_verdict(&locked),
        Some(Mode::K8_30 | Mode::K5_7_60 | Mode::K4_120) => match unlocked {
            Some((_, r)) => held_verdict(r, &locked),
            None => {
                return (
                    None,
                    format!("{name}: not decided: the unlocked run didn't play"),
                );
            }
        },
        Some(Mode::K5_7_30) | None => {
            let broken = lock_broken(&[&locked.0, &locked.1]);
            return (
                None,
                format!(
                    "{name}: recorded, P-LOCK names no verdict for this mode; {slots}{}",
                    if broken.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", broken.join("; "))
                    }
                ),
            );
        }
    };
    (
        Some(verdict),
        format!("{name}: {verdict:?}: {why}; {slots}"),
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

/// P-LOCK's summary line: every mode's verdict, by mode, "not decided"
/// where it has none.
fn p_lock_summary(verdicts: &[(String, Option<Verdict>, String)]) -> String {
    let each: Vec<String> = verdicts
        .iter()
        .map(|(mode, v, _)| match v {
            Some(v) => format!("{mode} {v:?}"),
            None => format!("{mode} not decided"),
        })
        .collect();
    format!(
        "P-LOCK on this machine: {}",
        if each.is_empty() {
            "no X5 raw mode among the samples".to_string()
        } else {
            each.join(", ")
        }
    )
}

/// Each X5 raw mode among the samples, its two lenses as two streams, on
/// Auto's hardware and the rest, in real time and unthrottled; then each
/// of hw-test's two-track perf clips, both tracks opened as a sample's
/// are, unthrottled ([`bench_perf_pair`]). Recorded; a stall of an
/// unthrottled pass fails it, and so does P-LOCK's Kill (a pair apart, the
/// lock refused or undone, a stall of either pass). A Redirect is a note,
/// and so is the summary line that names every mode's verdict.
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
                let (verdict, line) =
                    p_lock(&rig, mode, path, report, &run, passes.as_ref(), &mut stalls);
                verdicts.push((mode.clone(), verdict, line));
            }
            Err(why) => {
                verdicts.push((
                    mode.clone(),
                    None,
                    format!("P-LOCK, {mode}: not decided: Auto decodes it in software here"),
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
    for (_, _, line) in &verdicts {
        bench::note(line);
    }
    let summary = p_lock_summary(&verdicts);
    bench::note(&summary);
    eprintln!("{summary}");
    let killed: Vec<&str> = verdicts
        .iter()
        .filter(|(_, v, _)| *v == Some(Verdict::Kill))
        .map(|(_, _, line)| line.as_str())
        .collect();
    let failed: Vec<&str> = stalls.iter().map(String::as_str).chain(killed).collect();
    assert!(failed.is_empty(), "{}", failed.join("; "));
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
/// says about why. First, where the frame's bytes part from its
/// download's ([`stage`]), read before anything renders again: in the
/// take's planes; in `renderer`'s slot as the take's last render left it
/// (`view`'s for a held frame, the last view's for one rendered right
/// after its decode); or in neither, so in the render. Then how much
/// differs; whether the same take, rendered again, and a fresh take
/// render as `got` or as `want`; and whether the frame downloads now as
/// it did before (if not, its surface changed under the frame that holds
/// it).
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
    let slot = renderer.slot().map(|s| read_planes(gpu, s));
    let held = taken_planes(gpu, taken);
    let copy = frame.to_system();
    let stage = match &copy {
        Some(Ok(c)) => stage(how, held, slot, c),
        Some(Err(e)) => format!("no stage named, the download failing: {e}"),
        None => "no stage named, the frame not on its device".to_string(),
    };
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
    let downloaded = like(match copy {
        Some(Ok(copy)) => renderer
            .render(gpu, &copy, view, size, opts)
            .map_err(|e| e.to_string()),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("not on its device".into()),
    });
    format!(
        "{stage}; {differ} of {} channel values differ, by {most:.4} at most; rendered \
         again it renders {again}, taken again {fresh}, and downloaded again {downloaded}",
        w.len()
    )
}

/// Where a frame taken on the GPU first parts from its download, `copy`:
/// in the take's planes read back (`held`: an import's textures, or the
/// copy's buffer), plane by plane ([`same_planes`]); in the renderer's
/// slot, level 0, read back (`slot`), Y's samples that differ held to
/// odin's GFX11 R8 signature ([`r8_cells`]); or in neither, so in the
/// render. A deep frame's slot holds the ingest pass's halves, not the
/// planes' bytes, so it isn't compared.
fn stage(
    how: &OnDevice,
    held: Result<Vec<readback::Texels>, String>,
    slot: Option<Result<Yuv, String>>,
    copy: &VideoFrame,
) -> String {
    let taken = how.taken();
    match held.and_then(|h| Ok((h, planes_of(copy)?))) {
        Ok((h, c)) => {
            if let Err(why) = same_planes(&h, &c) {
                return format!("{taken} already differs from the download: {why}");
            }
        }
        Err(e) => return format!("no stage named, reading the planes failing: {e}"),
    }
    let Some(slot) = slot else {
        return format!("{taken} is the download's bytes, and the renderer has no slot");
    };
    match slot.and_then(|s| Ok((s, yuv_of(copy)?))) {
        Err(e) => format!("{taken} is the download's bytes; the slot isn't compared: {e}"),
        Ok((s, c)) if !s.same_shape(&c) => format!(
            "{taken} is the download's bytes; the slot is {}, the download {}",
            s.shape(),
            c.shape()
        ),
        Ok((s, c)) if s == c => {
            format!("{taken} and the slot are the download's bytes, so the render differs")
        }
        Ok((s, c)) => {
            let diffs: Vec<PlaneDiff> = (0..3)
                .map(|i| PlaneDiff::of(&s.0[i], &c.0[i], s.band_rows(i)))
                .collect();
            let planes = ["Y", "U", "V"]
                .iter()
                .zip(&diffs)
                .filter(|(_, d)| d.samples > 0)
                .map(|(name, d)| format!("{name} in {} samples, by {} at most", d.samples, d.most))
                .collect::<Vec<_>>()
                .join(", ");
            let r8 = match r8_cells(&diffs[0]) {
                Ok(on) => format!("odin's GFX11 R8 signature: {on}"),
                Err(off) => format!("not odin's GFX11 R8 signature: {off}"),
            };
            format!("{taken} is the download's bytes, but the slot differs: {planes}; {r8}")
        }
    }
}

/// The planes `taken` holds, read back from `gpu`: an import's textures,
/// whole, or the planes in the copy's buffer. Waits for the GPU.
fn taken_planes(gpu: &Gpu, taken: &Taken) -> Result<Vec<readback::Texels>, String> {
    match taken {
        Taken::Imported(planes) => planes
            .iter()
            .map(|t| readback::read_texture(gpu, t, 0).map_err(|e| e.to_string()))
            .collect(),
        Taken::Copied(c) => {
            let end = c
                .planes
                .iter()
                .map(|p| p.offset + u64::from(p.bytes_per_row) * u64::from(p.size.1))
                .max()
                .unwrap_or(0);
            let bytes = yuv::read_buffer(gpu, &c.buffer, end).map_err(|e| e.to_string())?;
            c.planes
                .iter()
                .map(|p| {
                    let row = p.size.0 as usize * p.bytes_per_texel as usize;
                    let mut data = Vec::with_capacity(row * p.size.1 as usize);
                    for y in 0..p.size.1 as usize {
                        let at = p.offset as usize + y * p.bytes_per_row as usize;
                        let bytes = bytes.get(at..at + row).ok_or_else(|| {
                            format!(
                                "the copy's buffer ends before row {y} of its plane at {}",
                                p.offset
                            )
                        })?;
                        data.extend_from_slice(bytes);
                    }
                    Ok(readback::Texels {
                        width: p.size.0,
                        height: p.size.1,
                        bytes_per_texel: p.bytes_per_texel,
                        data,
                    })
                })
                .collect()
        }
    }
}

/// `frame`'s planes in memory, each row without its padding, as a take's
/// planes read back are.
fn planes_of(frame: &dyn VideoPlanes) -> Result<Vec<readback::Texels>, String> {
    let layout = frame.layout();
    let mut planes = Vec::new();
    for (i, desc) in layout.planes.iter().enumerate() {
        let p = frame
            .plane(i)
            .ok_or_else(|| format!("{} has no plane {i} in memory", layout.name))?;
        let row = desc.row_bytes(p.width);
        let mut data = Vec::with_capacity(row * p.height as usize);
        for y in 0..p.height {
            let bytes = p
                .row(y, row)
                .ok_or_else(|| format!("{}'s plane {i} has no row {y}", layout.name))?;
            data.extend_from_slice(bytes);
        }
        planes.push(readback::Texels {
            width: p.width,
            height: p.height,
            bytes_per_texel: u32::try_from(row / p.width.max(1) as usize).unwrap_or(0),
            data,
        });
    }
    Ok(planes)
}

/// Whether the planes a take holds, read back (`taken`), are its
/// download's (`copy`) byte for byte; or, for each plane that isn't, how
/// many of its texels differ and the first of them in raster order.
fn same_planes(taken: &[readback::Texels], copy: &[readback::Texels]) -> Result<(), String> {
    if taken.len() != copy.len() {
        return Err(format!(
            "{} planes, the download {}",
            taken.len(),
            copy.len()
        ));
    }
    let mut off = Vec::new();
    for (i, (t, c)) in taken.iter().zip(copy).enumerate() {
        if (t.width, t.height, t.bytes_per_texel) != (c.width, c.height, c.bytes_per_texel) {
            off.push(format!(
                "plane {i} is {}x{} of {} bytes a texel, the download's {}x{} of {}",
                t.width, t.height, t.bytes_per_texel, c.width, c.height, c.bytes_per_texel
            ));
            continue;
        }
        let b = t.bytes_per_texel.max(1) as usize;
        let mut differ = t
            .data
            .chunks_exact(b)
            .zip(c.data.chunks_exact(b))
            .enumerate()
            .filter(|(_, (g, w))| g != w)
            .map(|(n, _)| n);
        if let Some(n) = differ.next() {
            let w = t.width.max(1) as usize;
            off.push(format!(
                "plane {i}: {} of {} texels differ, the first at x {} y {}",
                1 + differ.count(),
                t.data.len() / b,
                n % w,
                n / w
            ));
        }
    }
    if off.is_empty() {
        Ok(())
    } else {
        Err(off.join("; "))
    }
}

/// Whether a frame's renders through the zero-copy views, `got`, are
/// `want`'s bytes, view for view; or the first view whose aren't.
fn same(got: &[Image], want: &[Image]) -> Result<(), usize> {
    got.iter()
        .zip(want)
        .position(|(g, w)| g != w)
        .map_or(Ok(()), Err)
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
/// back while it was held would show a later picture.
///
/// Every other frame is also taken on the GPU and rendered through every
/// view right after its decode, before anything downloads it, as the
/// upload thread takes a frame and the viewer draws it, and those renders
/// must be the copy's too. On path B the copier's barrier then starts
/// from the decoder's layout and its wait meets the decode's semaphore;
/// on path A the GPU's first read of the import comes before VA-API's
/// download reads the surface. No later decode runs meanwhile (one test
/// thread), as one may on the player's decode thread. Once a clip, on
/// the first such frame that moved from the one before, two controls
/// run: the renders' check must refuse the frame before's renders, and
/// the planes' check of [`mismatch`] ([`same_planes`]), which the take
/// must pass against its own download, must refuse the frame before's
/// download.
///
/// The copy renders through a renderer of its own, so that the zero-copy
/// renderer's slot holds the take's when a render differs. Returns the
/// frames compared, and of those, the ones taken right after their
/// decode.
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
    let mut copies = StillRenderer::new(&rig.gpu);
    // Each held frame with its copy path's renders.
    let mut held: VecDeque<(usize, Arc<VideoFrame>, Vec<Image>)> = VecDeque::new();
    let mut compared = 0;
    let mut early = 0;
    let mut moved = false;
    let mut controlled = false;
    let mut last: Option<Vec<Image>> = None;
    // The frame before's download, kept until the controls have run.
    let mut last_copy: Option<VideoFrame> = None;
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
        // Production order: taken as soon as it is decoded, and rendered
        // before anything downloads it.
        let first = if k % 2 == 0 {
            let taken = take(&rig.gpu, how, &frame)?;
            let got: Vec<Image> = views
                .iter()
                .map(|(view, size)| {
                    render_taken(&rig.gpu, &mut renderer, &frame, &taken, view, *size, &opts)
                        .unwrap_or_else(|e| panic!("frame {k}: rendering {}: {e}", how.taken()))
                })
                .collect();
            Some((taken, got))
        } else {
            None
        };
        let copied = frame
            .to_system()
            .expect("a frame on its device")
            .unwrap_or_else(|e| panic!("frame {k}: downloading it: {e}"));
        let want: Vec<Image> = views
            .iter()
            .map(|(view, size)| {
                copies
                    .render(&rig.gpu, &copied, view, *size, &opts)
                    .unwrap_or_else(|e| panic!("frame {k}: rendering the copy: {e}"))
            })
            .collect();
        if let Some((taken, got)) = &first {
            if let Err(i) = same(got, &want) {
                let (view, size) = &views[i];
                let why = mismatch(
                    &rig.gpu,
                    &mut renderer,
                    how,
                    &frame,
                    taken,
                    &got[i],
                    &want[i],
                    view,
                    *size,
                    &opts,
                );
                panic!(
                    "{}: frame {k}, view {i}: {} taken and rendered before its download \
                     renders other bytes than the copy: {why}",
                    path.display(),
                    how.taken()
                );
            }
            if !controlled
                && let (Some(before), Some(before_copy)) = (&last, &last_copy)
                && *before != want
            {
                must_fail("an early render of another frame", same(got, before));
                let planes = taken_planes(&rig.gpu, taken)
                    .unwrap_or_else(|e| panic!("frame {k}: reading {} back: {e}", how.taken()));
                let copy_of = |f: &VideoFrame| {
                    planes_of(f).unwrap_or_else(|e| panic!("frame {k}: a download's planes: {e}"))
                };
                if let Err(why) = same_planes(&planes, &copy_of(&copied)) {
                    panic!(
                        "{}: frame {k}: {} holds other bytes than its download, though every \
                         view renders as the copy: {why}",
                        path.display(),
                        how.taken()
                    );
                }
                let why = must_fail(
                    "a plane of another frame",
                    same_planes(&planes, &copy_of(before_copy)),
                );
                assert!(why.starts_with("plane "), "{why}");
                controlled = true;
            }
            early += 1;
        }
        moved |= last.as_ref().is_some_and(|l| *l != want);
        last = Some(want.clone());
        last_copy = (!controlled).then_some(copied);
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
    assert!(
        controlled,
        "{}: no frame taken right after its decode moved from the one before, so the \
         controls never ran",
        path.display()
    );
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
                 and rendered right after their decode too",
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
/// as many frames held as the frame cache holds, and every other frame
/// imported and rendered before its download too ([`compare_zero_copy`]).
/// Skips, with the reason, when the GPU can't import, or VA-API won't
/// decode or export a clip, or the driver refuses its planes; never a
/// pass with a clip left out.
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
/// held as the frame cache holds, and every other frame copied out and
/// rendered before its download too. Skips, with the reason, when the
/// device can't be shared or FFmpeg won't take it, or Vulkan Video won't
/// decode a clip, or the copier refuses its frames; never a pass with a
/// clip left out.
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
    // Unlocked, the engine counts drops for each stream, which P-LOCK's
    // share a lens divides by (the locked pair's test sees 1).
    assert_eq!((matched.streams, matched.drops_over), (2, 2));
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

/// Whether `p` shows a locked pair's lenses apart by their tags: one
/// lens alone, or the two at different times.
fn tags_apart(p: &Presented) -> bool {
    let time = |m: usize| {
        p.frames
            .iter()
            .find(|f| f.stream == m)
            .map(|f| f.frame.time)
    };
    time(0) != time(1)
}

/// What an unthrottled pass of a coded pair read back, as a table: the
/// presents read, those apart by their tags (not read), those whose two
/// slots' corner codes name different frames or a lens's slot holds the
/// other lens's code (apart by code), and those whose codes agree but
/// name another frame than the tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeTable {
    read: usize,
    apart_by_tag: usize,
    apart_by_code: usize,
    not_the_tag: usize,
}

impl CodeTable {
    fn of(read: &[(i64, [CornerCode; 2])], apart_by_tag: usize) -> CodeTable {
        let apart = |[a, b]: [CornerCode; 2]| a.frame != b.frame || (a.lens, b.lens) != (0, 1);
        CodeTable {
            read: read.len(),
            apart_by_tag,
            apart_by_code: read.iter().filter(|(_, c)| apart(*c)).count(),
            not_the_tag: read
                .iter()
                .filter(|&&(tag, c)| !apart(c) && codes_match(tag, c).is_err())
                .count(),
        }
    }
}

impl std::fmt::Display for CodeTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} pairs' corner codes read back; unpaired by tag {}, by corner code {}, \
             paired on another frame than the tags {}",
            self.read, self.apart_by_tag, self.apart_by_code, self.not_the_tag
        )
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
/// slot, as does a third whose every present read while the slots stood
/// still ([`Before::Quiet`]). The controls: the pair opened unlocked holds
/// no lock, which [`lock_held`] says (a failure in
/// `zero_copy_pair_matches_copy`, never a skip); the second pass's lenses
/// swapped differ from the first's, and it a frame short names the frame;
/// each pass's first right slot, judged as its lens's next frame and as
/// the other lens's, fails as the frame it is; a pass judged against the
/// truth with its lenses' frames crossed finds every slot wrong, tells
/// each apart as the other lens's frame at its place (the same read
/// again, each lens's first in the cache as in the slot and the slot's
/// own frame, its Y samples its own lens's beside the crossed truth's,
/// off the GFX11 R8 signature), and fails the report, which counts them
/// by path and side; counts planted break [`path_rule`] (a kept pass with
/// a frame downloaded or refused, a copied one with a frame on the GPU)
/// or skip it (a kept pass with none on the GPU); the note of
/// `MemAvailable` beside P-LOCK's rows gives both passes', and says when
/// a pass didn't read it; [`stood_still`] fails with a slot free before
/// the reads or claimed during them; lens 0's slot read for both fails
/// the codes' check, lens 1 a frame late in one present fails the
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
    let passes = [(); 2].map(|_| slot_pass(&rig, &path, &streams, &run, 4, Reads::NOW, &truth));
    let quiet = Reads {
        before: Before::Quiet,
        watched: false,
    };
    let still = slot_pass(&rig, &path, &streams, &run, 4, quiet, &truth);
    let unlocked = slot_pass(
        &rig,
        &path,
        &streams,
        &Run::software(),
        4,
        Reads::NOW,
        &truth,
    );
    // The truth with each lens's frames given as the other's.
    let crossed = Truth {
        lenses: (0..2)
            .map(|l| TruthLens {
                stream: truth.lenses[l].stream,
                frames: truth.lenses[1 - l].frames.clone(),
            })
            .collect(),
    };
    let misjudged = slot_pass(&rig, &path, &streams, &run, 4, Reads::NOW, &crossed);
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
    // Tier 2h's table of the presents read, and its control: lens 0's
    // slot read for both, and a present apart by tag.
    let none = CodeTable {
        read: 30,
        apart_by_tag: 0,
        apart_by_code: 0,
        not_the_tag: 0,
    };
    assert_eq!(CodeTable::of(&read, 0), none);
    let crossed: Vec<_> = read.iter().map(|&(tag, [a, _])| (tag, [a, a])).collect();
    let mut late = read.clone();
    late[5].1 = [late[5].1[0], late[6].1[1]];
    let mut stale = read.clone();
    stale[5].1 = stale[4].1;
    assert_eq!(
        [
            CodeTable::of(&crossed, 1),
            CodeTable::of(&late, 0),
            CodeTable::of(&stale, 0)
        ]
        .map(|t| (t.apart_by_tag, t.apart_by_code, t.not_the_tag)),
        [(1, 30, 0), (0, 1, 0), (0, 0, 1)]
    );
    must_fail("a table with a pair apart", CodeTable::of(&late, 0) == none);
    assert_eq!((r.lock, r.unpaired), (Some(LockStatus::Locked), 0));
    assert_eq!(apart(&r.presents), 0);
    assert!(r.shown > 0 && !r.samples.is_empty());
    assert_eq!(r.samples.len(), r.presents.len());
    // MemAvailable as each pass began, as the note beside P-LOCK's rows
    // gives it; a pass that didn't read it says so.
    let mem = mem_note("pair64", &[&u, &r]);
    assert!(
        u.mem_available.is_some() && r.mem_available.is_some(),
        "{mem}"
    );
    assert!(
        mem.starts_with("pair64: MemAvailable as each pass began: ")
            && mem.contains(" MiB unthrottled, ")
            && mem.ends_with(" MiB real time"),
        "{mem}"
    );
    let unread = mem_note("pair64", &[&Pass::default(), &r]);
    must_fail(
        "a pass that didn't read MemAvailable noted as read",
        unread.contains(" MiB unthrottled"),
    );
    assert!(unread.contains("not read unthrottled, "), "{unread}");
    // Its presents scored as P-LOCK scores them: some owed a pair, and
    // its drops count once a pair.
    assert!(r.seconds.iter().any(|s| s.due > 0), "{:?}", r.seconds);
    assert_eq!((r.streams, r.drops_over), (2, 1));
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
    // Two runs of the same path, slot for slot, and a third read while
    // the slots stood still.
    let (first, second) = (passes[0].hashes(), passes[1].hashes());
    assert_eq!(same_slots(&first, &second), Ok(8));
    assert_eq!((still.broken.as_deref(), still.wrong().count()), (None, 0));
    assert_eq!((still.quiet, still.unquiet), (4, 0), "{still:?}");
    assert_eq!(same_slots(&first, &still.hashes()), Ok(8));
    // The path rules, on counts planted: a kept pass all on the GPU and a
    // copied one all through system memory keep to them.
    let kept = Run {
        zero_copy: ZeroCopy::Vulkan,
        ..Run::only(HwKind::Vulkan)
    };
    let copied = Run::only(HwKind::Vulkan);
    let counts = |copied, refused| PassCounts {
        uploaded: 8,
        copied,
        refused,
        ..PassCounts::default()
    };
    path_rule(&kept, &counts(8, 0), None).unwrap();
    path_rule(&copied, &counts(0, 0), None).unwrap();

    // Controls.
    let swapped = second
        .iter()
        .map(|(&(lens, pts), &h)| ((lens ^ 1, pts), h))
        .collect();
    must_fail(
        "the second run's lenses swapped",
        same_slots(&first, &swapped),
    );
    let mut short = second.clone();
    short.pop_last();
    let short = must_fail("the second run a frame short", same_slots(&first, &short));
    assert!(
        short.contains("8 and 7 frames; only the first: lens 1 pts"),
        "{short}"
    );
    for (name, run, c) in [
        ("a kept pass with a frame downloaded", &kept, counts(7, 0)),
        ("a kept pass with a frame refused", &kept, counts(8, 1)),
        ("a copied pass with a frame copied", &copied, counts(1, 0)),
    ] {
        let off = must_fail(name, path_rule(run, &c, None));
        assert!(matches!(off, Off::Broken(_)), "{name}: {off:?}");
    }
    let off = must_fail(
        "a kept pass with no frame on the GPU",
        path_rule(&kept, &counts(0, 8), None),
    );
    assert!(matches!(off, Off::NotKept(_)), "{off:?}");
    let words: Vec<(usize, u64)> = (0..6).map(|i| (i, 16 * i as u64 + 2)).collect();
    assert!(stood_still(6, &words, &words));
    must_fail(
        "a slot free before the reads",
        stood_still(6, &words[1..], &words[1..]),
    );
    let mut claimed = words.clone();
    claimed[3].1 += 8;
    must_fail(
        "a slot claimed while they read",
        stood_still(6, &words, &claimed),
    );
    assert_eq!(unlocked.broken, None);
    let unlocked = must_fail(
        "the pair opened unlocked holds a lock",
        lock_held(&unlocked),
    );
    assert!(unlocked.contains("the lock ended None"), "{unlocked}");
    for p in passes.iter().chain([&still]) {
        check_controls(p.controls.as_ref().unwrap());
    }
    // Judged against the crossed truth, every slot is wrong, told apart
    // as the other lens's frame at its place and the same when read
    // again, each lens's first in the cache as in the slot (the frame the
    // slot was filled from) and the rest not looked up, its Y samples
    // its own lens's frame's beside the crossed truth's, and off the
    // GFX11 R8 signature; and the report fails it, by path and side.
    assert_eq!(misjudged.broken, None);
    assert_eq!(misjudged.wrong().count(), 8, "{misjudged:?}");
    assert!(misjudged.controls.is_none(), "{misjudged:?}");
    let y_at = |lens: usize, index: usize, s: &YSample| {
        let y = &truth.frame(lens, index).unwrap().1.0[0];
        y.samples[(s.y * y.width + s.x) as usize]
    };
    for (&(lens, pts), s) in misjudged.wrong() {
        let w = s.first_wrong.as_ref().unwrap();
        assert_eq!(
            w.wrong.like().map(|l| (l.lens, l.off)),
            Some((1 - lens, 0)),
            "{}",
            w.wrong
        );
        assert!(matches!(w.again, Again::Same), "{}", w.again);
        let first = misjudged.keys.keys().find(|k| k.0 == lens) == Some(&(lens, pts));
        if first {
            assert!(
                matches!(w.cached, Cached::AsSlot(Source::TheSlots)),
                "{}",
                w.cached
            );
        } else {
            assert!(matches!(w.cached, Cached::NotLooked), "{}", w.cached);
        }
        let index = s.index.unwrap();
        assert!(
            !w.samples.is_empty() && w.samples.len() <= SAMPLES,
            "{}",
            w.samples()
        );
        for y in &w.samples {
            assert_eq!(
                (y.slot, y.truth),
                (y_at(lens, index, y), y_at(1 - lens, index, y)),
                "{}",
                w.samples()
            );
        }
        if let Some(b) = w.before {
            assert!(b.pts < pts, "{b:?}");
        }
        must_fail(
            "the crossed truth's frame on the GFX11 R8 signature",
            gfx11_r8(w, s.shown, &misjudged.counts),
        );
    }
    let report = pair_report(&[Played {
        file: "pair64-matched".into(),
        path: "software",
        variant: "crossed truth",
        passes: vec![("copy 1", false, misjudged)],
    }]);
    let failed = must_fail(
        "a pass judged against the crossed truth",
        if report.is_empty() {
            Ok(())
        } else {
            Err(report)
        },
    );
    assert!(
        failed[0].starts_with(
            "8 frames shown wrong (software copy 8 in 1 pass); of the 8 told apart, 0 on the \
             GFX11 R8 signature and 8 off it; the cache, looked up for 2 frames: as the slot with \
             the slot's own frame 2; where it held the slot's own frame as the slot"
        ),
        "{failed:?}"
    );
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

/// A file a locked check plays: its name, its path, and the rate its
/// corner codes count at and its frames a lens, where it has them.
type LockedFile = (String, PathBuf, Option<(FrameRate, i64)>);

/// The two-lens files the locked Tier 2h checks play, with the rate
/// their corner codes count at and their frames where they have them:
/// clip 9's master
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
            Some((spec.lenses[0].rate, i64::from(spec.lenses[0].frames))),
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
/// unthrottled pass, every frame of the clip once and in order, equal
/// each other and the tags: the content check on path B and its pool. A
/// table gives the presents unpaired by tag and by corner code. Each
/// check's control plants a mismatch it must catch: lens 1 landed a frame
/// off, the codes of lens 0's slot read for both (which the table counts
/// too), lens 1 a frame late in one present, a pass a frame short.
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
        let codes = reader.as_mut().zip(coded.map(|(rate, _)| rate));
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

        // Every frame's corner codes, unthrottled: a present whose tags
        // show the lenses apart is counted, not read, so that the table
        // below gives both counts.
        let (Some(reader), Some((rate, frames))) = (reader.as_mut(), *coded) else {
            continue;
        };
        let (mut read, mut apart_by_tag) = (Vec::new(), 0);
        let mut each = |p: &Presented| {
            if tags_apart(p) {
                apart_by_tag += 1;
            } else {
                read.push(pair_codes(reader, &on.gpu, p, rate)?);
            }
            Ok(ControlFlow::Continue(()))
        };
        let u = unthrottled_with(on, path, &streams, &run, GATED_UNTHROTTLED, Some(&mut each))
            .unwrap_or_else(|e| panic!("{name}, {}: unthrottled: {e}", run.label));
        let table = CodeTable::of(&read, apart_by_tag);
        eprintln!("{name}, {}: {table}; {}", run.label, u.paths_line());
        assert_eq!(u.stalled, None, "{name}, {}", run.label);
        assert_eq!((u.lock, u.unpaired), (Some(LockStatus::Locked), 0));
        assert_eq!(
            (table.apart_by_tag, table.apart_by_code, table.not_the_tag),
            (0, 0, 0),
            "{name}, {}: {table}",
            run.label
        );
        for (tag, codes) in &read {
            codes_match(*tag, *codes).unwrap_or_else(|e| panic!("{name}, {}: {e}", run.label));
        }
        // Every frame of the clip, each once, in order: a pass cut off at
        // its limit, or ended early, reads fewer.
        let tags: Vec<i64> = read.iter().map(|(tag, _)| *tag).collect();
        assert!(
            tags.iter().copied().eq(0..frames),
            "{name}, {}: {} frames read of {frames}, from {:?} to {:?}",
            run.label,
            tags.len(),
            tags.first(),
            tags.last()
        );
        let (tag, codes) = read[read.len() / 2];
        must_fail(
            "lens 0's slot read for both",
            codes_match(tag, [codes[0], codes[0]]),
        );
        must_fail(
            "a pass a frame short",
            tags[..tags.len() - 1].iter().copied().eq(0..frames),
        );
        let crossed: Vec<(i64, [CornerCode; 2])> =
            read.iter().map(|&(tag, [a, _])| (tag, [a, a])).collect();
        let off = CodeTable::of(&crossed, 1);
        must_fail(
            "lens 0's slot read for both, and a present apart by tag, in the table",
            (off.apart_by_tag, off.apart_by_code) == (0, 0),
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
    /// The aligned 8×4 cells it differs in, and the first [`CELLS`] of
    /// them in raster order, each with its samples that differ.
    cells: usize,
    first_cells: Vec<Cell>,
}

/// The most aligned 8×4 cells a [`PlaneDiff`] keeps, and that odin's
/// GFX11 R8 signature allows a frame ([`gfx11_r8`]): PR #32's wrong
/// frames differed in one cell or two, and damage in more is of another
/// scale.
const CELLS: usize = 16;

/// An aligned 8×4 cell of a plane in which samples differ: its corner,
/// and its samples that differ as bits, bit `y%4 * 8 + x%8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    x: u32,
    y: u32,
    mask: u32,
}

impl PlaneDiff {
    /// Where `got` differs from `want`, planes of one shape, `rows` rows a
    /// band.
    fn of(got: &Plane, want: &Plane, rows: u32) -> PlaneDiff {
        let mut d = PlaneDiff::default();
        let width = (got.width as usize).max(1);
        // The cells of the row of cells under way, a mask each.
        let mut masks = vec![0u32; width.div_ceil(8)];
        let mut cell_row = 0;
        for (y, (g, w)) in (0u32..).zip(got.samples.chunks(width).zip(want.samples.chunks(width))) {
            if g == w {
                continue;
            }
            if y / 4 != cell_row {
                d.add_cells(&mut masks, cell_row);
                cell_row = y / 4;
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
                masks[x as usize / 8] |= 1 << (y % 4 * 8 + x % 8);
            }
            let band = y / rows;
            if d.bands.last() != Some(&band) {
                d.bands.push(band);
            }
        }
        d.add_cells(&mut masks, cell_row);
        d
    }

    /// The cells of row of cells `row` that `masks` has samples in, a
    /// mask a cell of the row, counted and kept up to [`CELLS`]; `masks`
    /// left clear.
    fn add_cells(&mut self, masks: &mut [u32], row: u32) {
        for (x, mask) in (0u32..).zip(masks.iter_mut()) {
            if *mask == 0 {
                continue;
            }
            self.cells += 1;
            if self.first_cells.len() < CELLS {
                self.first_cells.push(Cell {
                    x: x * 8,
                    y: row * 4,
                    mask: *mask,
                });
            }
            *mask = 0;
        }
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
/// others, and whether it is the frame the slot was filled from; or gone
/// from the cache. Only each lens's first wrong frame of a pass is looked
/// up: the download is FFmpeg's, under its frame lock, on the test's
/// thread while the decoders run, which moves the timing under test for
/// the frames after it.
#[derive(Debug)]
enum Cached {
    Right(Source),
    AsSlot(Source),
    Other(Source),
    Gone,
    Failed(String),
    NotLooked,
}

impl Cached {
    /// What it found, in a few words, for the report's sum.
    fn kind(&self) -> &'static str {
        match self {
            Cached::Right(Source::TheSlots) => "right with the slot's own frame",
            Cached::Right(Source::Another { .. }) => "right with another decode",
            Cached::AsSlot(Source::TheSlots) => "as the slot with the slot's own frame",
            Cached::AsSlot(Source::Another { .. }) => "as the slot with another decode",
            Cached::Other(_) => "other bytes",
            Cached::Gone => "gone",
            Cached::Failed(_) => "unread",
            Cached::NotLooked => "not looked up",
        }
    }
}

impl std::fmt::Display for Cached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (what, source) = match self {
            Cached::Right(s) => ("right", s),
            Cached::AsSlot(s) => ("as the slot", s),
            Cached::Other(s) => ("other bytes", s),
            Cached::Gone => return write!(f, "gone"),
            Cached::Failed(e) => return write!(f, "unread ({e})"),
            Cached::NotLooked => return write!(f, "not looked up"),
        };
        write!(f, "{what}, {source}")
    }
}

/// Whether the frame the cache holds at a slot's time is the one the
/// slot was filled from: the same run and seq, which the slot keeps from
/// the decoder's frame (it keeps no `Arc` of the frame to compare), or
/// another decode of that time, whose bytes say nothing of the slot's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    TheSlots,
    Another { run: u64, seq: u64 },
}

impl Source {
    /// The cache's frame of run `run` and place `seq` in it, against the
    /// frame `slot` holds.
    fn of(slot: &SlotFrame, run: u64, seq: u64) -> Source {
        if (slot.run, slot.seq) == (run, seq) {
            Source::TheSlots
        } else {
            Source::Another { run, seq }
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::TheSlots => write!(f, "the frame the slot was filled from"),
            Source::Another { run, seq } => write!(
                f,
                "but another decode of its time (run {run} seq {seq}), not the slot's frame"
            ),
        }
    }
}

/// A frame a slot showed: its lens, pts and place among the truth's
/// frames of that lens, and its run and seq.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    lens: usize,
    pts: i64,
    index: Option<usize>,
    run: u64,
    seq: u64,
}

/// The most Y samples a wrong slot's line gives: a whole 8×4 cell's.
const SAMPLES: usize = 32;

/// A Y sample where a wrong slot differs from the truth's frame: its
/// place, the slot's sample, the truth's, and the sample there of the
/// frame the slot showed before, where it showed one the truth has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct YSample {
    x: u32,
    y: u32,
    slot: u8,
    truth: u8,
    before: Option<u8>,
}

/// The first [`SAMPLES`] of `slot`'s Y samples that differ from `want`'s,
/// in raster order, each beside `before`'s there: `slot` and `want` of
/// one shape, and `before` left out where it isn't theirs.
fn y_samples(slot: &Yuv, want: &Yuv, before: Option<&Yuv>) -> Vec<YSample> {
    let before = before.filter(|b| b.same_shape(slot)).map(|b| &b.0[0]);
    let (got, want) = (&slot.0[0], &want.0[0]);
    let width = (got.width as usize).max(1);
    let mut out = Vec::new();
    let rows = got.samples.chunks(width).zip(want.samples.chunks(width));
    for (y, (g, w)) in (0u32..).zip(rows) {
        if g == w {
            continue;
        }
        for (x, (&a, &b)) in (0u32..).zip(g.iter().zip(w)) {
            if a == b {
                continue;
            }
            if out.len() == SAMPLES {
                return out;
            }
            let at = y as usize * width + x as usize;
            out.push(YSample {
                x,
                y,
                slot: a,
                truth: b,
                before: before.and_then(|p| p.samples.get(at).copied()),
            });
        }
    }
    out
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
    /// The frame the slot showed before this one in the pass, where it
    /// showed one: the frame it held before, unless one written to it
    /// in between was never shown.
    before: Option<Held>,
    /// Its first Y samples that differ, beside the truth's and the frame
    /// before's ([`y_samples`]).
    samples: Vec<YSample>,
}

impl Showing {
    /// Its Y samples that differ, "x,y slot/truth/before" each, and how
    /// many of them are the frame before's.
    fn samples(&self) -> String {
        if self.samples.is_empty() {
            return "no Y sample differs".into();
        }
        let before = match self.before {
            Some(b) => format!(
                "the slot showed lens {} pts {} (frame {}) before",
                b.lens,
                b.pts,
                b.index.map_or_else(|| "?".into(), |i| i.to_string())
            ),
            None => "the slot showed nothing before in the pass".into(),
        };
        let each: Vec<String> = self
            .samples
            .iter()
            .map(|s| {
                let was = s.before.map_or_else(|| "-".into(), |b| b.to_string());
                format!("{},{} {}/{}/{was}", s.x, s.y, s.slot, s.truth)
            })
            .collect();
        let stale = self
            .samples
            .iter()
            .filter(|s| s.before == Some(s.slot))
            .count();
        format!(
            "Y slot/truth/before at x,y, the first {} that differ ({before}): {}; {stale} of \
             them the frame before's",
            self.samples.len(),
            each.join(" ")
        )
    }
}

/// Where a wrong showing falls against odin's GFX11 R8 signature, the
/// fault PR #32 told apart on the RX 7600 under RADV with DCC on (the
/// check's docs give the evidence): Y only, U and V right; Y's samples
/// that differ in at most [`CELLS`] aligned 8×4 cells, each cell's one of
/// its blocks ([`r8_block`]: the 4×2 at x%8 4..7, y%4 2..3, the right 4×4
/// or the whole cell); the same when read again; the cache right or not
/// looked up; the pass with no resync and no unpaired present; and the
/// frame shown once. `shown` is how often the frame was shown and `c` its
/// pass's counts. On it, each cell's block; off it, every way it is off.
/// It labels a line and never excuses one: every wrong frame fails the
/// check either way.
fn gfx11_r8(w: &Showing, shown: u32, c: &PassCounts) -> Result<String, String> {
    let mut off = Vec::new();
    let mut shape = None;
    match &w.wrong {
        Wrong::Samples(d) => {
            match r8_cells(&d.planes[0]) {
                Ok(s) => shape = Some(s),
                Err(why) => off.push(why),
            }
            for (name, p) in ["U", "V"].iter().zip(&d.planes[1..]) {
                if p.bounds.is_some() {
                    off.push(format!("{name} differs"));
                }
            }
        }
        other => off.push(other.to_string()),
    }
    if !matches!(w.again, Again::Same) {
        off.push(format!("read again: {}", w.again));
    }
    if !matches!(w.cached, Cached::Right(_) | Cached::NotLooked) {
        off.push(format!("in the cache: {}", w.cached));
    }
    if c.resyncs > 0 {
        off.push(format!("pair resyncs in its pass: {}", c.resyncs));
    }
    if c.unpaired > 0 {
        off.push(format!("unpaired presents in its pass: {}", c.unpaired));
    }
    if shown > 1 {
        off.push(format!("shown {shown} times"));
    }
    match shape {
        Some(s) if off.is_empty() => Ok(s),
        _ => Err(off.join(", ")),
    }
}

/// Where Y's samples that differ, `y`, lie in their aligned 8×4 cells, as
/// [`gfx11_r8`] has it: each cell's block ([`r8_block`]), in at most
/// [`CELLS`] cells; or why they don't, each cell off it named.
fn r8_cells(y: &PlaneDiff) -> Result<String, String> {
    if y.cells == 0 {
        return Err("Y right".into());
    }
    if y.cells > CELLS {
        return Err(format!(
            "Y differs in {} aligned 8x4 cells, more than the {CELLS} the signature allows",
            y.cells
        ));
    }
    let (mut on, mut off) = (Vec::new(), Vec::new());
    for &c in &y.first_cells {
        match r8_block(c) {
            Ok(block) => on.push(block),
            Err(why) => off.push(why),
        }
    }
    if off.is_empty() {
        Ok(on.join(" and "))
    } else {
        Err(off.join(", "))
    }
}

/// Which of odin's GFX11 R8 blocks Y's samples that differ in `cell` are:
/// the 4×2 at x%8 4..7, y%4 2..3, the right 4×4 (x%8 4..7, y%4 0..3) or
/// the whole cell, their box the block's, corner to corner (a sample
/// inside it may be right, where the wrong byte happens to be the
/// truth's); or where in the cell they are instead.
fn r8_block(cell: Cell) -> Result<String, String> {
    let of = format!("of the 8x4 cell at x {} y {}", cell.x, cell.y);
    if cell.mask == 0 {
        return Err(format!("no sample differs {of}"));
    }
    let row = |r: u32| cell.mask >> (r * 8) & 0xff;
    let cols = (0..4).fold(0, |c, r| c | row(r));
    let rows = (0..4).fold(0u32, |b, r| if row(r) != 0 { b | 1 << r } else { b });
    let (c0, c1) = (cols.trailing_zeros(), 31 - cols.leading_zeros());
    let (r0, r1) = (rows.trailing_zeros(), 31 - rows.leading_zeros());
    let off = |why: &str| {
        Err(format!(
            "Y differs at x%8 {c0}..{c1}, y%4 {r0}..{r1} {of}: {why}"
        ))
    };
    match (c0, c1, r0, r1) {
        (4, 7, 2, 3) => Ok(format!("the 4x2 at x%8 4..7, y%4 2..3 {of}")),
        (4, 7, 0, 3) => Ok(format!("the right 4x4 (x%8 4..7) {of}")),
        (0, 7, 0, 3) => Ok(format!("the whole {of}")),
        _ if c0 >= 4 => off("its right half, but neither its 4x2 at y%4 2..3 nor its whole 4x4"),
        _ if c1 < 4 => off("its left half alone"),
        _ => off("across its halves, short of the whole cell"),
    }
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
    /// The showings that weren't the truth's, and the first of them,
    /// told apart, for the first [`LISTED`] frames of a pass that are
    /// wrong (telling one apart reads it again, downloads its frame in the
    /// cache and looks through the truth's frames near it).
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

/// A pass's controls, planted at its first slot the truth judges right:
/// the slot judged as its lens's next frame, and as the other lens's
/// frame at its place.
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
    /// With [`Before::Quiet`], the presents whose reads the slots stood
    /// still over, and those they didn't (the wait for them ran out, or
    /// a slot changed while they read).
    quiet: u32,
    unquiet: u32,
    /// With [`Reads::watched`], each stream's distinct images the
    /// [`PoolWatch`] saw, and their bytes.
    pool: Vec<(usize, u64)>,
    /// The lenses whose first wrong frame was looked up in the cache.
    looked: HashSet<usize>,
    /// By slot, the frame it showed last and the one it showed before
    /// that.
    held: HashMap<usize, (Held, Option<Held>)>,
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

    /// The wrong frames told apart so far: the first [`LISTED`], whose
    /// lines the report gives.
    fn told(&self) -> usize {
        self.keys
            .values()
            .filter(|s| s.first_wrong.is_some())
            .count()
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

/// How long a [`Before::Quiet`] read waits for the slots to stand still
/// before it reads anyway, and counts the present as not quiet.
const QUIET: Duration = Duration::from_secs(2);

/// What a slot pass does before each present's reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Before {
    /// Reads at once.
    #[default]
    Nothing,
    /// Waits for the GPU to finish the work submitted so far. That moves
    /// the reads' timing, and nothing more: the upload thread may still be
    /// between staging the copier's wait on wgpu's queue and its own
    /// submit, which a read's submit then takes.
    Idle,
    /// Waits, at most [`QUIET`], until every live slot is Ready or Shown:
    /// none is free for the upload thread to claim and none is being
    /// written, so that it is copying nothing out and has staged no wait
    /// that a read's submit could take. A present counts as quiet when no
    /// slot changed by the end of its reads ([`stood_still`]).
    Quiet,
}

/// How a slot pass reads its slots.
#[derive(Debug, Clone, Copy, Default)]
struct Reads {
    before: Before,
    /// A [`PoolWatch`] fed every present, as [`unthrottled_with`]'s path B
    /// passes feed one, and as this check's passes had when odin's
    /// mismatches came: it takes FFmpeg's frame lock from a thread of its
    /// own and holds frames while it looks.
    watched: bool,
}

impl Reads {
    /// Reads at once, with no watch.
    const NOW: Reads = Reads {
        before: Before::Nothing,
        watched: false,
    };
}

/// Why a pass's frames didn't come the way its run says.
#[derive(Debug)]
enum Off {
    /// A kept run's frame came through system memory or was refused by
    /// the GPU, or a copied run's came on the GPU: the pass didn't play
    /// the path it names, a failure.
    Broken(String),
    /// A kept run took no frame on the GPU at all: the decoder or the GPU
    /// turned zero copy down here, a skip.
    NotKept(String),
}

/// Whether the frames `run` uploaded, `c`, came the way it says: every
/// one of a kept run's on the GPU (copied out or imported), none of a
/// copied run's. `refusal` is the player's reason the GPU turned the
/// decoder's frames down, where it has one.
fn path_rule(run: &Run, c: &PassCounts, refusal: Option<&String>) -> Result<(), Off> {
    let on_gpu = c.copied + c.imported;
    if run.zero_copy == ZeroCopy::Off {
        return match on_gpu {
            0 => Ok(()),
            n => Err(Off::Broken(format!(
                "a copied run took {n} frames on the GPU"
            ))),
        };
    }
    if on_gpu == 0 {
        Err(Off::NotKept(format!(
            "no frame taken on the GPU, {} refused{}",
            c.refused,
            because(refusal)
        )))
    } else if c.downloaded() > 0 || c.refused > 0 {
        Err(Off::Broken(format!(
            "{} of its {} frames uploaded came through system memory, {} refused by the GPU{}",
            c.downloaded(),
            c.uploaded,
            c.refused,
            because(refusal)
        )))
    } else {
        Ok(())
    }
}

/// The words of `player`'s Ready and Shown slots, by slot: a claim, a
/// discard, a publish or a show changes a slot's.
fn slot_words(player: &Player) -> Vec<(usize, u64)> {
    player
        .slots()
        .views()
        .iter()
        .map(|v| (v.index, v.word))
        .collect()
}

/// Waits, at most [`QUIET`], until the upload thread has nothing left to
/// do while `p` is on screen: every live slot of `player`'s Ready or
/// Shown, and each Ready one ahead of the frame its stream shows (one
/// behind it, a frame the presenter gave back, the upload thread frees
/// and writes again). The live slots, and the words then.
fn settle(player: &Player, p: &Presented) -> (usize, Vec<(usize, u64)>) {
    let until = Instant::now() + QUIET;
    loop {
        let active = player.slots().stats().active;
        let views = player.slots().views();
        let ahead = views.iter().all(|v| {
            v.state != SlotState::Ready
                || p.frames
                    .iter()
                    .any(|f| f.stream == v.frame.stream && f.frame.time < v.frame.time)
        });
        if (views.len() == active && ahead) || Instant::now() >= until {
            return (active, views.iter().map(|v| (v.index, v.word)).collect());
        }
        std::thread::sleep(Duration::from_micros(250));
    }
}

/// Whether the slots stood still over a present's reads: every one of
/// the `active` live slots Ready or Shown before them (`before`), so that
/// none was free for the upload thread to claim or being written, and
/// none changed by their end (`after`), a claim moving a slot's version
/// on. The upload thread then copied nothing out while they read, and
/// had staged no wait of the copier's that their submits could take.
fn stood_still(active: usize, before: &[(usize, u64)], after: &[(usize, u64)]) -> bool {
    before.len() == active && before == after
}

/// `count` presents of `streams` of `path`, opened as `run` on `rig` and
/// played unthrottled, each slot shown read back from the GPU (level 0,
/// which the mips are made from) as `reads` says and judged against
/// `truth`, byte for byte. A wrong slot is read again while held, and a
/// lens's first wrong frame is looked up in the cache. There is no
/// [`PoolWatch`] unless `reads` asks for one: it takes FFmpeg's frame
/// lock from another thread and holds frames, which moves the timing
/// under test. Broken when the run fails, stalls or ends short, a locked
/// run's lock doesn't hold, or its frames break [`path_rule`]; not kept
/// when a kept run took no frame on the GPU at all.
fn slot_pass(
    rig: &Rig,
    path: &Path,
    streams: &[Option<usize>],
    run: &Run,
    count: usize,
    reads: Reads,
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
    let mut watch = None;
    let played = (|| -> Result<(), String> {
        open(&player, path, streams, run)?;
        if reads.watched {
            watch = PoolWatch::of(&player, run, streams.len());
        }
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
            if let Some(w) = &watch {
                w.shown(&p);
            }
            look(rig, &player, truth, &lenses, reads.before, &p, &mut pass)?;
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
    pass.pool = watch.map_or_else(Vec::new, PoolWatch::finish);
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
    let mut broken = played.err();
    if broken.is_none() && run.locked {
        broken = lock_held(&pass).err();
    }
    match path_rule(run, &pass.counts, snap.import_refused.as_ref()) {
        Ok(()) => {}
        Err(Off::NotKept(why)) => pass.not_kept = Some(why),
        Err(Off::Broken(why)) => {
            broken.get_or_insert(why);
        }
    }
    if let Err(e) = joined {
        broken = Some(broken.map_or(e.clone(), |b| format!("{b}; {e}")));
    }
    pass.broken = broken;
    pass
}

/// Read back each slot `p` shows, as lens `lenses[stream]`, after what
/// `before` waits for, and judge it against `truth`, into `pass`: a wrong
/// one read again, its Y samples that differ listed beside the truth's
/// and those of the frame its slot showed before, and a lens's first
/// wrong one looked up in `player`'s cache. The pass's first slot the
/// truth judges right plants its controls.
fn look(
    on: &Rig,
    player: &Player,
    truth: &Truth,
    lenses: &[usize],
    before: Before,
    p: &Presented,
    pass: &mut SlotPass,
) -> Result<(), String> {
    let settled = match before {
        Before::Nothing => None,
        Before::Idle => {
            on.gpu.wait_for_submitted().map_err(|e| e.to_string())?;
            None
        }
        Before::Quiet => Some(settle(player, p)),
    };
    for f in &p.frames {
        let lens = *lenses
            .get(f.stream)
            .ok_or_else(|| format!("a present showed stream {}", f.stream))?;
        let pts = f.frame.pts;
        let slot = read_slot(&on.gpu, f)?;
        let index = truth.index(lens, pts);
        let right = truth.holds(&slot, lens, index);
        if pass.controls.is_none()
            && right
            && let Some(i) = index
        {
            let other = (lens + 1) % truth.lenses.len();
            pass.controls = Some(Controls {
                lens,
                late: truth.judge(&slot, lens, i + 1),
                crossed: (other != lens).then(|| truth.judge(&slot, other, i)),
            });
        }
        let now = Held {
            lens,
            pts,
            index,
            run: f.frame.run,
            seq: f.frame.seq,
        };
        let before = match pass.held.get(&f.slot) {
            Some(&(last, earlier)) if last == now => earlier,
            Some(&(last, _)) => Some(last),
            None => None,
        };
        pass.held.insert(f.slot, (now, before));
        let told = pass.told();
        let seen = pass.keys.entry((lens, pts)).or_default();
        seen.index = index;
        seen.shown += 1;
        if seen.shown == 1 {
            seen.hash = slot.hash();
        }
        if right {
            continue;
        }
        seen.wrong += 1;
        if seen.first_wrong.is_some() || told >= LISTED {
            continue;
        }
        let Err(wrong) = index.map_or(Err(Wrong::NotInTruth), |i| truth.judge(&slot, lens, i))
        else {
            continue;
        };
        let again = match read_slot(&on.gpu, f) {
            Ok(s) if s == slot => Again::Same,
            Ok(s) if truth.holds(&s, lens, index) => Again::Right,
            Ok(_) => Again::Other,
            Err(e) => Again::Failed(e),
        };
        let cached = if pass.looked.insert(lens) {
            cached(player, f, &slot, truth, lens, index)
        } else {
            Cached::NotLooked
        };
        let picture = |h: Option<Held>| {
            h.and_then(|h| truth.frame(h.lens, h.index?))
                .map(|(_, y)| y)
        };
        let samples = picture(Some(now))
            .filter(|want| want.same_shape(&slot))
            .map_or_else(Vec::new, |want| y_samples(&slot, want, picture(before)));
        if let Some(seen) = pass.keys.get_mut(&(lens, pts)) {
            seen.first_wrong = Some(Showing {
                slot: f.slot,
                run: f.frame.run,
                seq: f.frame.seq,
                wrong,
                again,
                cached,
                before,
                samples,
            });
        }
    }
    if let Some((active, words)) = settled {
        if stood_still(active, &words, &slot_words(player)) {
            pass.quiet += 1;
        } else {
            pass.unquiet += 1;
        }
    }
    Ok(())
}

/// What `player`'s cache holds now of the frame `f` shows, against the
/// truth's frame `index` of lens `lens` and against `slot`, and whether
/// it is the frame the slot was filled from ([`Source`]).
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
    let source = Source::of(&f.frame, frame.run, frame.seq);
    let yuv = match frame.to_system() {
        Some(Ok(down)) => yuv_of(&down),
        Some(Err(e)) => Err(e.to_string()),
        None => yuv_of(frame.as_ref()),
    };
    match yuv {
        Ok(y) if truth.holds(&y, lens, index) => Cached::Right(source),
        Ok(y) if y == *slot => Cached::AsSlot(source),
        Ok(_) => Cached::Other(source),
        Err(e) => Cached::Failed(e),
    }
}

/// The controls a pass planted (M3 plan 4.1, rule 1): its first right
/// slot judged as its lens's next frame and as the other lens's frame
/// must fail, and [`Truth::like`] must name the frame it is: a frame
/// early of its own lens, and the other lens's at its place.
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

/// A 70×130 NV12 frame in memory, its samples hashed from their place
/// and `seed`; with U and V swapped when `swapped`.
fn nv12_frame(seed: u32, swapped: bool) -> VideoFrame {
    let size: (u32, u32) = (70, 130);
    let chroma = (size.0.div_ceil(2), size.1.div_ceil(2));
    let sample = move |p: u32, x: u32, y: u32| {
        ((seed + p * 7919 + y * 104_729 + x * 31).wrapping_mul(2_654_435_761) >> 24) as u8
    };
    let (u, v) = if swapped { (2, 1) } else { (1, 2) };
    let luma = (0..size.1)
        .flat_map(|y| (0..size.0).map(move |x| sample(0, x, y)))
        .collect();
    let uv = (0..chroma.1)
        .flat_map(|y| (0..chroma.0).flat_map(move |x| [sample(u, x, y), sample(v, x, y)]))
        .collect();
    let color = ColorInfo {
        codes: ColorCodes::UNSPECIFIED,
        range: Range::Limited,
        matrix: Matrix::Bt709,
        transfer: spherecut_core::video::Transfer::Sdr,
        primaries: Primaries::Bt709,
        siting: ChromaSiting::Left,
        assumed: Assumed::default(),
    };
    let layout = Arc::new(layout_420("nv12", &[1, 2]));
    let strides = vec![size.0 as usize, 2 * chroma.0 as usize];
    VideoFrame::owned(
        layout,
        size,
        vec![luma, uv],
        strides,
        color,
        (0, Time::ZERO),
    )
    .unwrap()
}

/// `frame`'s planes as textures, as `dmabuf::Importer` makes them: whole,
/// in the import's format and usage (and `COPY_DST`, to fill them here).
fn as_imported(gpu: &Gpu, frame: &VideoFrame) -> Vec<wgpu::Texture> {
    let shape = SlotShape::of(frame.layout(), frame.size()).unwrap();
    let planes = planes_of(frame).unwrap();
    shape
        .planes
        .iter()
        .zip(&planes)
        .map(|(p, texels)| {
            let size = wgpu::Extent3d {
                width: p.size.0,
                height: p.size.1,
                depth_or_array_layers: 1,
            };
            let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("plane as imported"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: dmabuf::imported_format(p.format),
                usage: dmabuf::USAGE | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.queue.write_texture(
                t.as_image_copy(),
                &texels.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(texels.width * texels.bytes_per_texel),
                    rows_per_image: None,
                },
                size,
            );
            t
        })
        .collect()
}

/// [`stage`], which names where a frame taken on the GPU first parts
/// from its download when a zero-copy render differs ([`mismatch`]), on
/// lavapipe: an NV12 frame's planes made textures as an import's are,
/// and a renderer's slot once it has rendered them, are the frame's
/// bytes, so the render is named. The controls: the take of the frame
/// with U and V swapped is named, plane 1 from its first texel; and with
/// this frame's take, a slot that rendered the swapped frame last is
/// named, U and V wrong, Y right and so off odin's GFX11 R8 signature.
#[test]
fn a_zero_copy_mismatch_names_the_stage_that_parts_from_the_download() {
    let gpu = common::gpu();
    let frame = nv12_frame(0, false);
    let swapped = nv12_frame(0, true);
    let mut renderer = StillRenderer::new(gpu);
    let (view, size) = &zero_copy_views()[0];
    let opts = StillOptions::default();
    let mut slot_after = |f: &VideoFrame, taken: &Taken| {
        render_taken(gpu, &mut renderer, f, taken, view, *size, &opts).unwrap();
        renderer.slot().map(|s| read_planes(gpu, s))
    };
    let taken = Taken::Imported(as_imported(gpu, &frame));
    let other = Taken::Imported(as_imported(gpu, &swapped));
    let named =
        |taken: &Taken, slot| stage(&OnDevice::DmaBuf, taken_planes(gpu, taken), slot, &frame);

    let slot = slot_after(&frame, &taken);
    assert_eq!(
        named(&taken, slot),
        "the import and the slot are the download's bytes, so the render differs"
    );

    let slot = slot_after(&swapped, &other);
    let import = named(&other, slot.clone());
    assert!(
        import.starts_with(
            "the import already differs from the download: plane 1: 2275 of 2275 texels \
             differ, the first at x 0 y 0"
        ),
        "{import}"
    );
    let at_slot = named(&taken, slot);
    assert!(
        at_slot.starts_with("the import is the download's bytes, but the slot differs: U in ")
            && at_slot.contains(", V in ")
            && !at_slot.contains("Y in ")
            && at_slot.ends_with("not odin's GFX11 R8 signature: Y right"),
        "{at_slot}"
    );
}

/// Whether two passes' slots, `got` and `want` by lens and pts, hold the
/// same frames byte for byte; the frames that differ, or how many frames
/// each showed and the first few that only one did. The frames compared.
fn same_slots(
    got: &BTreeMap<(usize, i64), u64>,
    want: &BTreeMap<(usize, i64), u64>,
) -> Result<usize, String> {
    if got.keys().ne(want.keys()) {
        let only = |a: &BTreeMap<(usize, i64), u64>, b: &BTreeMap<(usize, i64), u64>| {
            let keys: Vec<String> = a
                .keys()
                .filter(|k| !b.contains_key(k))
                .map(|(lens, pts)| format!("lens {lens} pts {pts}"))
                .collect();
            match keys.len() {
                0 => "none".to_string(),
                n if n <= 4 => keys.join(", "),
                n => format!("{} and {} more", keys[..4].join(", "), n - 4),
            }
        };
        return Err(format!(
            "the passes showed {} and {} frames; only the first: {}; only the second: {}",
            got.len(),
            want.len(),
            only(got, want),
            only(want, got)
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
    reads: Reads,
}

/// The ways: locked, as the app plays a pair, first; then, on clip 9's
/// path B, where odin's mismatches came, unlocked, each lens alone
/// through the player, and locked three more ways: each read after the
/// GPU idles, each read while the slots stand still, and with a
/// [`PoolWatch`] fed every present, as when the mismatches came.
const VARIANTS: [Variant; 7] = [
    Variant {
        name: "locked",
        lenses: &[0, 1],
        locked: true,
        reads: Reads::NOW,
    },
    Variant {
        name: "unlocked",
        lenses: &[0, 1],
        locked: false,
        reads: Reads::NOW,
    },
    Variant {
        name: "lens 0 alone",
        lenses: &[0],
        locked: false,
        reads: Reads::NOW,
    },
    Variant {
        name: "lens 1 alone",
        lenses: &[1],
        locked: false,
        reads: Reads::NOW,
    },
    Variant {
        name: "locked, read idle",
        lenses: &[0, 1],
        locked: true,
        reads: Reads {
            before: Before::Idle,
            watched: false,
        },
    },
    Variant {
        name: "locked, read quiet",
        lenses: &[0, 1],
        locked: true,
        reads: Reads {
            before: Before::Quiet,
            watched: false,
        },
    },
    Variant {
        name: "locked, pool watched",
        lenses: &[0, 1],
        locked: true,
        reads: Reads {
            before: Before::Nothing,
            watched: true,
        },
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
        let mut also = String::new();
        if p.quiet + p.unquiet > 0 {
            also += &format!(", quiet {} of {}", p.quiet, p.quiet + p.unquiet);
        }
        if !p.pool.is_empty() {
            also += &format!(
                ", pool {} images",
                p.pool
                    .iter()
                    .map(|(images, _)| images.to_string())
                    .collect::<Vec<_>>()
                    .join("+")
            );
        }
        format!(
            "{}, {}, {}, {name}: {} frames, {} wrong: {verdict}; resyncs {}, unpaired {}, \
             decoded {}, uploaded {} ({} copied, {} imported, {} downloaded, {} refused), \
             shown twice: {}{also}; {:.1} s",
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

    /// One line for wrong frame `key` of pass `name`, whose counts are
    /// `c`: where told apart, how it is wrong, its Y samples that differ,
    /// and whether it is on odin's GFX11 R8 signature ([`gfx11_r8`]).
    fn key_line(&self, name: &str, (lens, pts): (usize, i64), s: &Seen, c: &PassCounts) -> String {
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
                "{head}; slot {} run {} seq {}; read again: {}; in the cache: {}; {}; {}; {}",
                w.slot,
                w.run,
                w.seq,
                w.again,
                w.cached,
                w.wrong,
                w.samples(),
                match gfx11_r8(w, s.shown, c) {
                    Ok(shape) => format!("on the GFX11 R8 signature: {shape}"),
                    Err(why) => format!("off the GFX11 R8 signature: {why}"),
                }
            ),
            None => head,
        }
    }
}

/// The zero-copy pair check's report, as [`say`]s: a row a pass, a line a
/// variant on its passes' steadiness, then each wrong frame, at most
/// [`LISTED`] a pass, labelled on odin's GFX11 R8 signature or off it
/// ([`gfx11_r8`]), and a line that sums the labels up and calls out each
/// frame off it. What failed, in words: nothing when every pass showed the
/// truth's frames and none broke; else the wrong frames by path and side
/// and what the cache found of them ([`Wrongs`]), and the passes that
/// broke. A frame on the signature fails as any other does.
fn pair_report(played: &[Played]) -> Vec<String> {
    say(
        "zero-copy pair check: file, path, variant, pass: frames, wrong, verdict; the engine's counts",
    );
    for g in played {
        for (name, _, p) in &g.passes {
            say(&g.row(name, p));
        }
        let steady = g.steadiness();
        if !steady.is_empty() {
            say(&format!("{}, {}, {}: {steady}", g.file, g.path, g.variant));
        }
    }
    for g in played {
        for (name, _, p) in &g.passes {
            let (told, more): (Vec<_>, Vec<_>) =
                p.wrong().partition(|(_, s)| s.first_wrong.is_some());
            for (&key, s) in told {
                say(&g.key_line(name, key, s, &p.counts));
            }
            if !more.is_empty() {
                say(&format!(
                    "{}, {}, {}, {name}: {} wrong frames more, not told apart: {}",
                    g.file,
                    g.path,
                    g.variant,
                    more.len(),
                    more.iter()
                        .map(|&(&(lens, pts), s)| format!(
                            "lens {lens} pts {pts} ({}, wrong {})",
                            g.class(&(lens, pts)),
                            s.wrong
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    let wrongs = Wrongs::of(played);
    if wrongs.frames > 0 {
        say(&wrongs.signature());
    }
    let mut failed = Vec::new();
    if wrongs.frames > 0 {
        failed.push(wrongs.to_string());
    }
    let broken: Vec<String> = played
        .iter()
        .flat_map(|g| g.passes.iter().map(move |p| (g, p)))
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

/// `n` and `one` or `many` after it: "1 pass", "3 passes".
fn count_of(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The wrong frames of the zero-copy pair check's passes, summed up.
#[derive(Debug, Default)]
struct Wrongs {
    frames: usize,
    /// By path and side (kept or copy), in the order played: the wrong
    /// frames, and the passes that showed them.
    by_path: Vec<(String, usize, usize)>,
    /// Those told apart, those of them on the GFX11 R8 signature, and
    /// each one off it, where and why.
    told: usize,
    on: usize,
    off: Vec<String>,
    /// What each lookup in the cache found ([`Cached::kind`]), by kind.
    cache: BTreeMap<&'static str, usize>,
}

impl Wrongs {
    fn of(played: &[Played]) -> Wrongs {
        let mut w = Wrongs::default();
        for g in played {
            for (name, kept, p) in &g.passes {
                let n = p.wrong().count();
                if n == 0 {
                    continue;
                }
                w.frames += n;
                let side = format!("{} {}", g.path, if *kept { "kept" } else { "copy" });
                match w.by_path.iter_mut().find(|(s, _, _)| *s == side) {
                    Some((_, frames, passes)) => {
                        *frames += n;
                        *passes += 1;
                    }
                    None => w.by_path.push((side, n, 1)),
                }
                for (&(lens, pts), s) in p.wrong() {
                    let Some(shown) = &s.first_wrong else {
                        continue;
                    };
                    w.told += 1;
                    if !matches!(shown.cached, Cached::NotLooked) {
                        *w.cache.entry(shown.cached.kind()).or_default() += 1;
                    }
                    match gfx11_r8(shown, s.shown, &p.counts) {
                        Ok(_) => w.on += 1,
                        Err(why) => w.off.push(format!(
                            "{}, {}, {}, {name}: lens {lens} pts {pts} ({why})",
                            g.file, g.path, g.variant
                        )),
                    }
                }
            }
        }
        w
    }

    /// The report's line on the GFX11 R8 signature: how many of the
    /// frames told apart are on it, and each one off it, called out.
    fn signature(&self) -> String {
        let mut line = format!(
            "GFX11 R8 signature: {} of the {} wrong frames told apart on it, {} off it; {} not \
             told apart",
            self.on,
            self.told,
            self.off.len(),
            self.frames - self.told
        );
        if !self.off.is_empty() {
            line += &format!("; off it: {}", self.off.join("; "));
        }
        line
    }
}

impl std::fmt::Display for Wrongs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let by_path: Vec<String> = self
            .by_path
            .iter()
            .map(|(side, frames, passes)| {
                format!("{side} {frames} in {}", count_of(*passes, "pass", "passes"))
            })
            .collect();
        write!(
            f,
            "{} shown wrong ({}); of the {} told apart, {} on the GFX11 R8 signature and {} off \
             it",
            count_of(self.frames, "frame", "frames"),
            by_path.join(", "),
            self.told,
            self.on,
            self.off.len()
        )?;
        if self.cache.is_empty() {
            return write!(f, "; none looked up in the cache");
        }
        let found: Vec<String> = self
            .cache
            .iter()
            .map(|(kind, n)| format!("{kind} {n}"))
            .collect();
        write!(
            f,
            "; the cache, looked up for {}: {}",
            count_of(self.cache.values().sum(), "frame", "frames"),
            found.join(", ")
        )?;
        let has = |kind: &str| self.cache.contains_key(kind);
        if has(Cached::Right(Source::TheSlots).kind()) {
            write!(
                f,
                "; where the cache held the slot's own frame right, the decode was right and the \
                 slot's write or read went wrong"
            )?;
        }
        if has(Cached::AsSlot(Source::TheSlots).kind()) {
            write!(
                f,
                "; where it held the slot's own frame as the slot, the frame was wrong before the \
                 slot: the decode, or its download"
            )?;
        }
        Ok(())
    }
}

/// The zero-copy pair check's report on mismatches planted in pictures of
/// noise, judged as the check judges a slot. On odin's GFX11 R8 signature:
/// a 4×2 of the frame the slot held before at x%8 4..7, y%4 2..3 of an
/// aligned 8×4 cell (its eight samples listed beside the truth's and the
/// frame before's, all eight the frame before's), the same with a sample
/// inside it right, the cell's right 4×4, its whole 8×4, and two 4×2s in
/// distant cells, with the cache right (the slot's own frame, or another
/// decode) or not looked up; odin's three lines whose Y box spans cells,
/// as two 4×2s at its corners on a plane of clip 9's; and [`CELLS`] 4×2s.
/// The report fails on them all the same, a frame on the signature alone
/// too, and counts them by path and side, the frames told apart on the
/// signature and off it, and what the cache found. The controls, each off
/// the signature and saying why: the cell's left half alone, a 4×2 across
/// two cells, a 4×2 at y%4 0..1, one sample, a 4×2 across the cell's
/// halves, two samples across them, a 4×2 short of its corners, a 4×2 on
/// it beside a cell off it, Y wrong in every cell, one cell more than
/// [`CELLS`], U wrong too, a second read with other bytes, the cache with
/// other bytes, as the slot or gone, a pass that resynced or showed a
/// pair apart, a frame shown twice, a frame the truth hasn't, and the
/// whole frame before shown; fresh bytes where the frame before's were
/// planted aren't counted as its, the samples stop at [`SAMPLES`], and
/// the cache's frame of another run or seq isn't the slot's.
#[test]
fn the_pair_report_labels_odins_gfx11_r8_signature() {
    let (width, height) = (64u32, 16u32);
    let noise = |seed: u32, (w, h): (u32, u32)| Plane {
        width: w,
        height: h,
        samples: (0..w * h)
            .map(|i| ((i ^ seed.wrapping_mul(0x9e37_79b9)).wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect(),
    };
    let picture = |seed: u32| {
        let chroma = (width / 2, height / 2);
        Yuv([
            noise(3 * seed, (width, height)),
            noise(3 * seed + 1, chroma),
            noise(3 * seed + 2, chroma),
        ])
    };
    // Frame 2 is the slot's; frame 0, every sample of it frame 2's with
    // bits flipped, is the one its slot showed before.
    let want = picture(7);
    let stale = Yuv(want.0.clone().map(|p| Plane {
        samples: p.samples.iter().map(|s| s ^ 0x5a).collect(),
        ..p
    }));
    let truth = Truth {
        lenses: vec![TruthLens {
            stream: Some(0),
            frames: vec![
                (0, stale.clone()),
                (1, picture(8)),
                (2, want.clone()),
                (3, picture(9)),
            ],
        }],
    };
    let before = Held {
        lens: 0,
        pts: 0,
        index: Some(0),
        run: 0,
        seq: 0,
    };
    // `slot` with Y in `xs` × `ys` taken from `from`.
    let put = |mut slot: Yuv, from: &Yuv, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>| {
        for y in ys {
            for x in xs.clone() {
                let at = (y * width + x) as usize;
                slot.0[0].samples[at] = from.0[0].samples[at];
            }
        }
        slot
    };
    let plant = |from: &Yuv, xs, ys| put(want.clone(), from, xs, ys);
    let showing = |slot: &Yuv, again: Again, cached: Cached| Showing {
        slot: 1,
        run: 0,
        seq: 2,
        wrong: truth.judge(slot, 0, 2).unwrap_err(),
        again,
        cached,
        before: Some(before),
        samples: y_samples(slot, &want, truth.frame(0, 0).map(|(_, y)| y)),
    };
    let own = || Cached::Right(Source::TheSlots);
    let calm = PassCounts::default();
    let on = |slot: &Yuv| showing(slot, Again::Same, own());

    let block = on(&plant(&stale, 12..16, 6..8));
    assert_eq!(
        gfx11_r8(&block, 1, &calm),
        Ok("the 4x2 at x%8 4..7, y%4 2..3 of the 8x4 cell at x 8 y 4".into())
    );
    let places: Vec<(u32, u32)> = block.samples.iter().map(|s| (s.x, s.y)).collect();
    let want_places: Vec<(u32, u32)> = (6..8).flat_map(|y| (12..16).map(move |x| (x, y))).collect();
    assert_eq!(places, want_places);
    for s in &block.samples {
        let at = (s.y * width + s.x) as usize;
        assert_eq!(
            (s.slot, s.truth, s.before),
            (
                stale.0[0].samples[at],
                want.0[0].samples[at],
                Some(stale.0[0].samples[at])
            )
        );
    }
    let words = block.samples();
    assert!(
        words.contains("the first 8 that differ (the slot showed lens 0 pts 0 (frame 0) before)")
            && words.ends_with("8 of them the frame before's"),
        "{words}"
    );
    for (slot, cached, shape) in [
        (
            put(plant(&stale, 12..16, 6..8), &want, 13..14, 6..7),
            Cached::NotLooked,
            "the 4x2 at x%8 4..7, y%4 2..3 of the 8x4 cell at x 8 y 4",
        ),
        (
            plant(&stale, 4..8, 0..4),
            Cached::Right(Source::Another { run: 1, seq: 0 }),
            "the right 4x4 (x%8 4..7) of the 8x4 cell at x 0 y 0",
        ),
        (
            plant(&stale, 24..32, 12..16),
            own(),
            "the whole of the 8x4 cell at x 24 y 12",
        ),
        (
            put(plant(&stale, 12..16, 2..4), &stale, 60..64, 14..16),
            Cached::NotLooked,
            "the 4x2 at x%8 4..7, y%4 2..3 of the 8x4 cell at x 8 y 0 and the 4x2 at x%8 4..7, \
             y%4 2..3 of the 8x4 cell at x 56 y 12",
        ),
    ] {
        let s = showing(&slot, Again::Same, cached);
        assert_eq!(gfx11_r8(&s, 1, &calm), Ok(shape.into()), "{}", s.wrong);
    }

    // On a plane of clip 9's lens: odin's three lines whose 16 Y samples'
    // box spans cells (PR #32's logs give the box alone, its corners at
    // x%8 4 and 7, y%4 2 and 3), as two 4×2s at its corners; and the most
    // cells the signature allows, 4×2s in a row of cells.
    let lens = Plane {
        width: 3840,
        height: 3840,
        samples: vec![0; 3840 * 3840],
    };
    let blocks = |at: &[(u32, u32)]| {
        let mut got = lens.clone();
        for &(x, y) in at {
            for y in y..y + 2 {
                for x in x..x + 4 {
                    got.samples[(y * lens.width + x) as usize] = 0x80;
                }
            }
        }
        let y = PlaneDiff::of(&got, &lens, BAND);
        let s = Showing {
            wrong: Wrong::Samples(Box::new(Diff {
                planes: [y.clone(), PlaneDiff::default(), PlaneDiff::default()],
                bands: y.bands.len(),
                like: None,
            })),
            ..on(&plant(&stale, 12..16, 6..8))
        };
        (y, s)
    };
    let r8 = |x: u32, y: u32| {
        format!(
            "the 4x2 at x%8 4..7, y%4 2..3 of the 8x4 cell at x {} y {}",
            x / 8 * 8,
            y / 4 * 4
        )
    };
    for [x0, y0, x1, y1] in [
        [3116, 298, 3295, 1539],
        [2380, 1038, 2903, 1075],
        [372, 1050, 1935, 2355],
    ] {
        let (y, s) = blocks(&[(x0, y0), (x1 - 3, y1 - 1)]);
        assert_eq!((y.samples, y.bounds), (16, Some([x0, y0, x1, y1])));
        assert_eq!(
            gfx11_r8(&s, 1, &calm),
            Ok(format!("{} and {}", r8(x0, y0), r8(x1 - 3, y1 - 1))),
            "{}",
            s.wrong
        );
    }
    let row: Vec<(u32, u32)> = (0..=CELLS as u32).map(|i| (4 + 64 * i, 2)).collect();
    let (_, most) = blocks(&row[..CELLS]);
    let most = gfx11_r8(&most, 1, &calm);
    assert_eq!(
        most,
        Ok(row[..CELLS]
            .iter()
            .map(|&(x, y)| r8(x, y))
            .collect::<Vec<_>>()
            .join(" and "))
    );
    let (_, beyond) = blocks(&row);

    // Off the signature, each saying why.
    let mut chroma = plant(&stale, 12..16, 6..8);
    chroma.0[1].samples[0] ^= 1;
    let resynced = PassCounts {
        resyncs: 1,
        ..PassCounts::default()
    };
    let apart = PassCounts {
        unpaired: 2,
        ..PassCounts::default()
    };
    let not_in_truth = Showing {
        wrong: Wrong::NotInTruth,
        ..on(&plant(&stale, 12..16, 6..8))
    };
    for (name, s, shown, counts, why) in [
        (
            "the cell's left half alone",
            on(&plant(&stale, 8..12, 6..8)),
            1,
            &calm,
            "Y differs at x%8 0..3, y%4 2..3 of the 8x4 cell at x 8 y 4: its left half alone",
        ),
        (
            "a 4x2 across two cells",
            on(&plant(&stale, 14..18, 6..8)),
            1,
            &calm,
            "Y differs at x%8 6..7, y%4 2..3 of the 8x4 cell at x 8 y 4: its right half, but \
             neither its 4x2 at y%4 2..3 nor its whole 4x4, Y differs at x%8 0..1, y%4 2..3 of \
             the 8x4 cell at x 16 y 4: its left half alone",
        ),
        (
            "a 4x2 at y%4 0..1",
            on(&plant(&stale, 20..24, 8..10)),
            1,
            &calm,
            "Y differs at x%8 4..7, y%4 0..1 of the 8x4 cell at x 16 y 8: its right half, but \
             neither its 4x2 at y%4 2..3 nor its whole 4x4",
        ),
        (
            "one sample",
            on(&plant(&stale, 13..14, 7..8)),
            1,
            &calm,
            "Y differs at x%8 5..5, y%4 3..3 of the 8x4 cell at x 8 y 4: its right half",
        ),
        (
            "a 4x2 across the cell's halves",
            on(&plant(&stale, 10..14, 6..8)),
            1,
            &calm,
            "Y differs at x%8 2..5, y%4 2..3 of the 8x4 cell at x 8 y 4: across its halves, \
             short of the whole cell",
        ),
        (
            "two samples across the halves",
            on(&plant(&stale, 11..13, 4..5)),
            1,
            &calm,
            "Y differs at x%8 3..4, y%4 0..0 of the 8x4 cell at x 8 y 4: across its halves",
        ),
        (
            "a 4x2 short of its corners",
            on(&plant(&stale, 13..16, 6..8)),
            1,
            &calm,
            "Y differs at x%8 5..7, y%4 2..3 of the 8x4 cell at x 8 y 4: its right half",
        ),
        (
            "a 4x2 on it beside a cell off it",
            on(&put(plant(&stale, 12..16, 6..8), &stale, 40..44, 8..10)),
            1,
            &calm,
            "Y differs at x%8 0..3, y%4 0..1 of the 8x4 cell at x 40 y 8: its left half alone",
        ),
        (
            "Y wrong in every cell",
            on(&plant(&stale, 0..width, 0..height)),
            1,
            &calm,
            "Y differs in 32 aligned 8x4 cells, more than the 16 the signature allows",
        ),
        (
            "one cell more than the signature allows",
            beyond,
            1,
            &calm,
            "Y differs in 17 aligned 8x4 cells, more than the 16 the signature allows",
        ),
        ("U wrong too", on(&chroma), 1, &calm, "U differs"),
        (
            "a second read with other bytes",
            showing(&plant(&stale, 12..16, 6..8), Again::Other, own()),
            1,
            &calm,
            "read again: other bytes",
        ),
        (
            "the cache with other bytes",
            showing(
                &plant(&stale, 12..16, 6..8),
                Again::Same,
                Cached::Other(Source::TheSlots),
            ),
            1,
            &calm,
            "in the cache: other bytes",
        ),
        (
            "the cache as the slot",
            showing(
                &plant(&stale, 12..16, 6..8),
                Again::Same,
                Cached::AsSlot(Source::TheSlots),
            ),
            1,
            &calm,
            "in the cache: as the slot",
        ),
        (
            "the cache gone",
            showing(&plant(&stale, 12..16, 6..8), Again::Same, Cached::Gone),
            1,
            &calm,
            "in the cache: gone",
        ),
        (
            "a pass that resynced",
            on(&plant(&stale, 12..16, 6..8)),
            1,
            &resynced,
            "pair resyncs in its pass: 1",
        ),
        (
            "a pass with a pair apart",
            on(&plant(&stale, 12..16, 6..8)),
            1,
            &apart,
            "unpaired presents in its pass: 2",
        ),
        (
            "a frame shown twice",
            on(&plant(&stale, 12..16, 6..8)),
            2,
            &calm,
            "shown 2 times",
        ),
        (
            "a frame the truth hasn't",
            not_in_truth,
            1,
            &calm,
            "not among the truth's frames",
        ),
        (
            "the whole frame before",
            on(&stale),
            1,
            &calm,
            "Y differs in 32 aligned 8x4 cells, more than the 16 the signature allows, U \
             differs, V differs",
        ),
    ] {
        let off = must_fail(name, gfx11_r8(&s, shown, counts));
        assert!(off.contains(why), "{name}: {off}");
    }
    let whole = on(&stale);
    let places: Vec<(u32, u32)> = whole.samples.iter().map(|s| (s.x, s.y)).collect();
    assert_eq!(
        places,
        (0..width).take(SAMPLES).map(|x| (x, 0)).collect::<Vec<_>>()
    );
    let fresh = on(&plant(&picture(10), 12..16, 6..8));
    assert!(!fresh.samples.is_empty(), "{}", fresh.wrong);
    must_fail(
        "fresh bytes counted as the frame before's",
        fresh.samples.iter().all(|s| s.before == Some(s.slot)),
    );
    let slot = SlotFrame {
        stream: 0,
        epoch: 1,
        time: Time::ZERO,
        pts: 0,
        run: 4,
        seq: 7,
        duration: None,
    };
    assert_eq!(Source::of(&slot, 4, 7), Source::TheSlots);
    for (name, run, seq) in [
        ("the cache's frame of another run", 5, 7),
        ("the cache's frame at another seq", 4, 8),
    ] {
        must_fail(name, Source::of(&slot, run, seq) == Source::TheSlots);
    }

    // The report: wrong frames fail it on the signature or off it, a
    // frame on it alone too.
    let seen = |first_wrong: Option<Showing>| Seen {
        index: Some(2),
        shown: 1,
        uploaded: 1,
        hash: 0,
        wrong: 1,
        first_wrong,
    };
    let pass = |keys: Vec<((usize, i64), Seen)>| SlotPass {
        keys: keys.into_iter().collect(),
        ..SlotPass::default()
    };
    let played = |passes| Played {
        file: "planted".into(),
        path: "path B",
        variant: "locked",
        passes,
    };
    let as_slot = showing(&chroma, Again::Same, Cached::AsSlot(Source::TheSlots));
    let report = pair_report(&[played(vec![
        (
            "kept 1",
            true,
            pass(vec![
                ((0, 2), seen(Some(on(&plant(&stale, 12..16, 6..8))))),
                ((0, 3), seen(Some(as_slot))),
            ]),
        ),
        ("copy 1", false, pass(vec![((0, 2), seen(None))])),
    ])]);
    let failed = must_fail(
        "wrong frames on the signature and off it",
        if report.is_empty() {
            Ok(())
        } else {
            Err(report)
        },
    );
    assert_eq!(
        failed,
        [
            "3 frames shown wrong (path B kept 2 in 1 pass, path B copy 1 in 1 pass); of the 2 \
             told apart, 1 on the GFX11 R8 signature and 1 off it; the cache, looked up for 2 \
             frames: as the slot with the slot's own frame 1, right with the slot's own frame 1; \
             where the cache held the slot's own frame right, the decode was right and the slot's \
             write or read went wrong; where it held the slot's own frame as the slot, the frame \
             was wrong before the slot: the decode, or its download"
        ]
    );
    let alone = pair_report(&[played(vec![(
        "kept 1",
        true,
        pass(vec![((0, 2), seen(Some(on(&plant(&stale, 12..16, 6..8)))))]),
    )])]);
    let alone = must_fail(
        "a frame on the signature alone",
        if alone.is_empty() { Ok(()) } else { Err(alone) },
    );
    assert!(
        alone[0].starts_with(
            "1 frame shown wrong (path B kept 1 in 1 pass); of the 1 told apart, 1 on the GFX11 \
             R8 signature and 0 off it"
        ),
        "{alone:?}"
    );
    let g = played(Vec::new());
    let line = g.key_line(
        "kept 1",
        (0, 2),
        &seen(Some(on(&plant(&stale, 12..16, 6..8)))),
        &calm,
    );
    assert!(
        line.ends_with(
            "on the GFX11 R8 signature: the 4x2 at x%8 4..7, y%4 2..3 of the 8x4 cell at x 8 y 4"
        ),
        "{line}"
    );
    let line = g.key_line("kept 1", (0, 3), &seen(Some(on(&chroma))), &calm);
    assert!(
        line.ends_with("off the GFX11 R8 signature: U differs"),
        "{line}"
    );
}

/// Every slot a locked pair shows, kept on the GPU (path B where the
/// device can be shared, and path A where the GPU imports VA-API's
/// frames) or copied on the same decoder, is the frame a software decode
/// of the file makes, byte for byte: the first [`ZERO_COPY_FRAMES`] pairs
/// of clip 9 and of the 2×3840² perf pair, more than path B's cache
/// holds on the device a lens (M3 plan section 9). Each variant runs a
/// copied pass, a kept one, and each again, so that each side's
/// steadiness shows; clip 9's path B also plays unlocked, each lens
/// alone, and locked three more ways ([`VARIANTS`]): each read after the
/// GPU idles, each read while the slots stand still, and with a
/// [`PoolWatch`]. A wrong slot is told apart: its kept or copied side,
/// its planes' differences and bands, whether those bands are a frame of
/// either lens near it, its slot, run and seq, a second read, its frame
/// in the cache (each lens's first wrong one a pass) and whether that is
/// the frame the slot was filled from, its first [`SAMPLES`] Y samples
/// that differ beside the truth's and those of the frame the slot showed
/// before, how often it was shown and uploaded, and whether it is on
/// odin's GFX11 R8 signature ([`gfx11_r8`]). Every pass and file runs
/// before the test fails, and the report (one row a pass, one line a
/// wrong frame, one line on the signature) goes to stderr and the bench
/// notes; the failure gives the wrong frames by path and side and what
/// the cache found ([`Wrongs`]). Each pass plants controls: its first
/// right slot judged as its lens's next frame, and as the other lens's,
/// must fail as the frame it is. A pass that breaks [`path_rule`] (a kept
/// one with a frame through system memory or refused by the GPU, a copied
/// one with a frame on the GPU), or fails, stalls, ends short or shows
/// the lenses apart locked, fails it; a kept one that keeps no frame on
/// the GPU at all skips, as a file or path not here does, never a pass
/// with one left out. Every wrong frame fails it, on the signature or
/// off it: there is no tolerance, retry, skip or list of frames let off.
///
/// What its outcomes mean:
///
/// - Every pass right: each path's slots are the decode's, byte for byte.
/// - A pass broken: its row says why (its path, its lock, or its run);
///   its frames say nothing until it plays as its run says.
/// - Kept and copied passes wrong alike, every line on the GFX11 R8
///   signature, the cache right with the slot's own frame: the slot-DCC
///   outcome, the fault odin's RX 7600 (Navi 33, GFX11; Mesa 26.2.5's
///   RADV, FFmpeg 8.1.3, kernel 7.2.9) showed in M3.8. PR #32 (this
///   check's diagnostic, at 8ebe9f7) failed all three runs on clip 9 and
///   none on the perf pair: 141 frames wrong, path B kept 118, path A
///   kept 16, path B copy 6 and path A copy 1, a kept frame 3.3 % of the
///   time and a copied one 0.17 %; every one Y only (the R8Unorm plane,
///   never the Rg8Unorm chroma); the 4×2 at x%8 4..7 and y%4 2..3 of an
///   aligned 8×4 cell in 134 lines, the cell's right 4×4 in three and its
///   whole 8×4 in one, and in three two such 4×2s in distant cells (16
///   samples whose box runs from one's corner to the other's: the logs
///   give the box alone); the same read again, the cache right wherever
///   looked up (on the copied passes too, whose cached frame is the very
///   bytes uploaded), never on a slot's first write, and no place twice.
///   PR #33 ran it with `RADV_DEBUG=nodcc`: it passed twice, every frame
///   right on every path, and a plain run between the two failed (45
///   wrong). The fault is in the slot's write on the GPU, or in every
///   read of it, and needs RADV's DCC (delta colour compression); it
///   isn't in the decoder, FFmpeg, the cache, the Copier or the import.
///   Whether it is the write or the read, and the driver or the hardware,
///   isn't known; DECISIONS has the fault and what keeps the slots out of
///   DCC. Where a line's samples are the frame before's (its before
///   column the slot's), the slot kept part of the frame it held before.
/// - Wrong lines off the signature, called out: another fault, the line
///   saying how it is off. Kept passes wrong and copied ones right off
///   the signature point at path A or B's own steps (the import and its
///   acquire, the Copier and its barrier); the cache as the slot with the
///   slot's own frame says the frame was wrong before its slot (the
///   decode, or its download); the cache right with another decode says
///   nothing of the slot's own frame.
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
                    let p = slot_pass(on, path, &streams, run, ZERO_COPY_FRAMES, v.reads, &truth);
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

/// A synthetic real-time pass of 20 s for P-LOCK's scoring: a clip of
/// `frames` frames of `mode`'s rate played as the engine plays it at
/// [`TICK`]'s presents, each scored as [`realtime`] scores it ([`Owed`]).
/// A present is for the clock one tick ahead. The one whose time reaches
/// the clip's end puts nothing new up and has the clock set back to the
/// last frame; then the loop's seek to the start stands for `stand`
/// presents (the seek, the landing, which puts frame 0 up at the last of
/// them, and the preroll) and plays on. A present `held` (by its index and
/// the frame its clock is in) keeps the frame it owes back, as a pair
/// waiting on a lens would; a `jumped` one finds the clock a frame
/// further on, as after a tick the UI thread missed, and passes a frame
/// over. The engine counts frames passed over once for a locked pair and
/// for each of `streams` otherwise: at each present, as skipped up to the
/// presents' cadence (one fewer than the clip's frames in a tick, rounded
/// up, so 1 at 119.88 fps and 0 at 59.94) and as dropped beyond it.
fn simulated(
    mode: Mode,
    frames: i64,
    stand: u64,
    (streams, locked): (usize, bool),
    held: impl Fn(u64, i64) -> bool,
    jumped: impl Fn(u64) -> bool,
) -> Pass {
    let (n, d) = mode.rate();
    let rate = FrameRate::new(Rational::new(n.into(), d.into()).unwrap()).unwrap();
    let tick = i64::try_from(TICK.as_nanos()).unwrap();
    let ns = |n: i64| Time::seconds(Rational::new(n, 1_000_000_000).unwrap());
    let at = |f: i64| rate.frame_to_time(f).unwrap();
    let end = at(frames);
    let counted = if locked { 1 } else { streams as u64 };
    let cadence = (TICK.as_secs_f64() * rate.to_f64() - 1e-3).ceil() as u64 - 1;
    let mut owed = Owed::default();
    let mut pass = Pass {
        seconds: vec![Second::default(); 20],
        rate: Some(rate),
        streams,
        drops_over: if locked { 1 } else { streams },
        lock: locked.then_some(LockStatus::Locked),
        elapsed: Duration::from_secs(20),
        ..Pass::default()
    };
    // The frame on screen; the wall time the clock was at 0, playing,
    // and the frames it jumped since; the presents a loop has left to
    // stand.
    let (mut shown, mut began, mut ahead, mut standing) = (0, 0, 0, None::<u64>);
    for k in 0..1200u64 {
        let w = k as i64 * tick;
        let (time, playing, at_end, new, passed) = match standing {
            Some(left) => {
                standing = left.checked_sub(1).filter(|&n| n > 0);
                let landed = standing.is_none();
                if landed {
                    (shown, began, ahead) = (0, w, 0);
                }
                (Time::ZERO, false, false, landed, 0)
            }
            None => {
                ahead += i64::from(jumped(k));
                let time = ns(w - began + tick).checked_add(at(ahead)).unwrap();
                if time >= end {
                    standing = Some(stand);
                    pass.loops += 1;
                    (at(frames - 1), false, true, false, 0)
                } else {
                    let f = rate.time_to_frame(time, Rounding::Floor).unwrap();
                    if f > shown && !held(k, f) {
                        let passed = f - shown - 1;
                        shown = f;
                        (time, true, false, true, passed as u64)
                    } else {
                        (time, true, false, false, 0)
                    }
                }
            }
        };
        let second = &mut pass.seconds[usize::try_from(w / 1_000_000_000).unwrap()];
        second.due += u64::from(owed.at(time, rate, playing, at_end, Some(end)));
        second.shown += u64::from(new);
        second.skipped += passed.min(cadence) * counted;
        second.dropped += (passed - passed.min(cadence)) * counted;
        pass.shown += u64::from(new);
    }
    pass
}

/// The share of a pass's frames presented as P-LOCK counted it before
/// presents owed frames: wall seconds after the first times the rate,
/// which a loop's restarts and a rate above the presents' fail.
fn wall_second_share(r: &Pass) -> f64 {
    let due = (r.seconds.len() - 1) as f64 * r.rate.map_or(f64::NAN, FrameRate::to_f64);
    100.0 * r.after_first(|s| s.shown) as f64 / due
}

/// P-LOCK's scoring (M3 plan 4.3) on synthetic passes: frames owed
/// count per present, so 4K120 at 60 Hz presents and a 5.7K60 master
/// that loops every 3.17 s present all they owe and pass; drops compare
/// a lens to a lens, the unlocked count divided by its streams; a stall
/// is Kill. Each has a control that must fail: a real shortfall of pairs
/// (a pair held back, which the present after passes over, and one that
/// stands still at each loop's end, which passes nothing over and fails
/// the share alone), a real excess of drops, a pass that stands still
/// while its clock runs.
#[test]
fn p_lock_scores_the_presents_that_owe_a_frame_and_drops_a_lens() {
    let unthrottled = || Pass {
        lock: Some(LockStatus::Locked),
        ..Pass::default()
    };
    let never = |_: u64| false;
    let kept = |_: u64, _: i64| false;
    let pair = (2, true);
    let apart = (2, false);
    // 4K120 at 60 Hz: 4.89 s of 586 frames, every present owing one and
    // passing one over, which the presents' cadence skips on both sides;
    // dropped only at each loop's restart, whose first present after the
    // landing finds the clock and its latency two ticks on, at frame 3.
    let k4 = |held: &dyn Fn(u64, i64) -> bool, (streams, locked): (usize, bool)| {
        simulated(Mode::K4_120, 586, 4, (streams, locked), held, never)
    };
    let unlocked = k4(&kept, apart);
    let locked = (unthrottled(), k4(&kept, pair));
    let (verdict, line) = held_verdict(&unlocked, &locked);
    assert_eq!(verdict, Verdict::Pass, "4K120: {line}");
    let r = &locked.1;
    assert!(r.loops >= 3, "4K120 looped {} times", r.loops);
    assert!(presented_share(r).unwrap() >= 99.9, "{line}");
    assert!(wall_second_share(r) < 51.0, "{}", wall_second_share(r));
    let (dropped, before) = (dropped_share(r).unwrap(), dropped_share(&unlocked).unwrap());
    assert_eq!(dropped, before, "{line}");
    assert!(r.after_first(|s| s.dropped) <= r.loops, "{line}");
    let skipped = skipped_share(r).unwrap();
    assert!((99.0..=100.0).contains(&skipped), "{skipped:.2} % skipped");
    // The control: a pair held back at one present in 20, short of the
    // presents owed (and dropping its frame at the present after).
    let short = (unthrottled(), k4(&|k, _| k % 20 == 7, pair));
    let (verdict, line) = held_verdict(&unlocked, &short);
    must_fail(
        "4K120 holding a pair back at 1 present in 20",
        verdict == Verdict::Pass,
    );
    assert_eq!(verdict, Verdict::Redirect, "{line}");
    assert!(presented_share(&short.1).unwrap() < 99.0, "{line}");

    // 5.7K60: 3.17 s of 190 frames, five or six loops in 20 s, each
    // standing four presents that owe nothing but the seek's.
    let k57 = |held: &dyn Fn(u64, i64) -> bool,
               jumped: &dyn Fn(u64) -> bool,
               (streams, locked): (usize, bool)| {
        simulated(Mode::K5_7_60, 190, 4, (streams, locked), held, jumped)
    };
    let unlocked = k57(&kept, &never, apart);
    let locked = (unthrottled(), k57(&kept, &never, pair));
    let (verdict, line) = held_verdict(&unlocked, &locked);
    assert_eq!(verdict, Verdict::Pass, "5.7K60: {line}");
    let r = &locked.1;
    assert!(r.loops >= 5, "5.7K60 looped {} times", r.loops);
    assert_eq!(skipped_share(r), Some(0.0), "5.7K60 skips nothing at 60 Hz");
    assert!(wall_second_share(r) < 99.0, "{}", wall_second_share(r));
    eprintln!(
        "5.7K60 locked: {line}; wall seconds would say {:.2} %",
        wall_second_share(r)
    );
    let short = (unthrottled(), k57(&|k, _| k % 40 == 7, &never, pair));
    let (verdict, line) = held_verdict(&unlocked, &short);
    must_fail(
        "5.7K60 holding a pair back at 1 present in 40",
        verdict == Verdict::Pass,
    );
    assert_eq!(verdict, Verdict::Redirect, "{line}");
    assert!(presented_share(&short.1).unwrap() < 99.0, "{line}");
    // The control for the share alone: a pair that stands still for the
    // last 4 frames of each loop, which the loop's seek leaves unshown
    // and passes nothing over, short of the presents owed by 2 % and
    // dropping no more than the unlocked run.
    let stood = (unthrottled(), k57(&|_, f| f >= 186, &never, pair));
    let (verdict, line) = held_verdict(&unlocked, &stood);
    must_fail(
        "5.7K60 standing still for each loop's last 4 frames",
        verdict == Verdict::Pass,
    );
    assert_eq!(verdict, Verdict::Redirect, "{line}");
    let (dropped, before) = (
        dropped_share(&stood.1).unwrap(),
        dropped_share(&unlocked).unwrap(),
    );
    assert!(dropped - before <= 0.0, "{line}");
    assert!(presented_share(&stood.1).unwrap() < 99.0, "{line}");

    // Drops a lens: the unlocked baseline passes a frame over on each
    // stream at 1 present in 100, which its counter sums to 2 %, and
    // the locked pair at 1 in 70 (1.4 %) passes at 0.4 points more a lens.
    let unlocked = k57(&kept, &|k| k % 100 == 50, apart);
    let a_lens = dropped_share(&unlocked).unwrap();
    // About 1 %: a jump that falls on a loop's restart passes nothing over.
    assert!((0.8..=1.1).contains(&a_lens), "{a_lens:.2} % a lens");
    let locked = (unthrottled(), k57(&kept, &|k| k % 70 == 50, pair));
    let (verdict, line) = held_verdict(&unlocked, &locked);
    assert_eq!(verdict, Verdict::Pass, "drops a lens: {line}");
    // The control: 1 in 40 (2.5 %) is 1.5 points more a lens, which the
    // summed baseline's 2 % would have let pass.
    let more = (unthrottled(), k57(&kept, &|k| k % 40 == 20, pair));
    let (verdict, line) = held_verdict(&unlocked, &more);
    must_fail(
        "a pair dropping 1.5 points more a lens",
        verdict == Verdict::Pass,
    );
    assert_eq!(verdict, Verdict::Redirect, "{line}");
    let summed = dropped_share(&unlocked).unwrap() * unlocked.drops_over as f64;
    assert!(dropped_share(&more.1).unwrap() - summed <= 1.0, "{line}");

    // A real-time pass that stood still while its clock ran is Kill.
    let stood = Pass {
        stalled: Some("no new frame for 5s while the clock ran, at 2.000 s".into()),
        ..k57(&kept, &never, pair)
    };
    let (verdict, line) = held_verdict(&unlocked, &(unthrottled(), stood));
    assert_eq!(verdict, Verdict::Kill, "{line}");
    assert!(line.contains("real time: stalled"), "{line}");
}

/// A real-time pass stands still once its frame has stayed up for
/// [`STALL`] while its clock ran short of the end; a clock paused, or at
/// the end, never stands it still.
#[test]
fn a_real_time_pass_stands_still_only_while_its_clock_runs() {
    let t0 = Instant::now();
    let later = |s: u64| t0 + Duration::from_millis(s);
    let mut standing = Standing::new(t0);
    assert_eq!(standing.at(later(1000), true, true, false), Duration::ZERO);
    assert_eq!(
        standing.at(later(4000), false, true, false),
        Duration::from_secs(3)
    );
    assert!(standing.at(later(6000), false, true, false) >= STALL);
    // The controls: the same wait with the clock paused, or at the end.
    for (playing, at_end, why) in [(false, false, "paused"), (true, true, "at the end")] {
        let mut standing = Standing::new(t0);
        standing.at(later(1000), true, true, false);
        let stood = standing.at(later(6000), false, playing, at_end);
        must_fail(&format!("a pass {why} standing still"), stood >= STALL);
    }
    // paced_verdict's line gives drops and decode drops a lens.
    let r = Pass {
        seconds: vec![Second {
            dropped: 3,
            decode_dropped: 4,
            shown: 20,
            ..Second::default()
        }],
        rate: Some(FrameRate::FPS_25),
        streams: 2,
        drops_over: 1,
        lock: Some(LockStatus::Locked),
        ..Pass::default()
    };
    let unthrottled = Pass {
        lock: Some(LockStatus::Locked),
        ..Pass::default()
    };
    let (_, line) = paced_verdict(&(unthrottled, r));
    assert!(
        line.contains("3.0 dropped a lens (2.0 decode-dropped a lens)"),
        "{line}"
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
