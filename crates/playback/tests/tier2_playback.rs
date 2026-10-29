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
//! stream decoded.
//!
//! What gates, and what only records:
//!
//! - `bench_playback_realtime_5k7_8bit` and `_10bit` play hw-test's 20 s
//!   perf clip on the hardware decoder [`DecoderPreference::Auto`] picks
//!   from doctor's checks, and pass only with no drop and no decode-drop
//!   after the first second of 60 s in real time, every unthrottled
//!   second decoding at least 1.2 × 29.97 frames, and every stream on
//!   that hardware throughout (section 12.3). The other hardware kind,
//!   where doctor says it decodes the clip, and software are recorded.
//! - `bench_playback_realtime_8k_8bit`: the 8K perf clip, recorded.
//! - `bench_playback_x5_raw`: the longest file of each X5 recording mode,
//!   its two lenses as two streams: M3's 8K budget, recorded.
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
//! fails is a note saying why. `the_bench_harness_plays_on_lavapipe` runs
//! the same harness in Tier 1, in software on a small clip, so that it
//! can't rot between hardware runs. Run the rest with
//! `cargo xtask hw-test`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use spherecut_core::blocking::{OnBlock, mark_ui_thread_with, unmark_ui_thread};
use spherecut_core::{Clock, FrameRate, ManualClock, SystemClock, Time};
use spherecut_media::backend::{DecodeMode, DecodePath, FfmpegBackend, HwKind};
use spherecut_media::decode::VideoDecoder;
use spherecut_media::detect::Layout;
use spherecut_media::hw::{self, GpuIds, RenderNode};
use spherecut_media::probe::{self, ProbeReport, StreamKind};
use spherecut_playback::decoder::choose;
use spherecut_playback::{
    Counter, DecoderPreference, HwSupport, PerfStats, Player, PlayerConfig, PlayerParts, Presented,
    SlotFrame, Stage, StreamFormat,
};
use spherecut_render::wgpu;
use spherecut_render::{
    AdapterChoice, DebugMode, Encoder, Filter, Gpu, GpuTimers, LinearTarget, Projector,
    RenderError, ViewParams,
};
use spherecut_sphere::DQuat;
use spherecut_sphere::perspective::{Lens, Projection};
use spherecut_synth::media::{self as synth_media, Codec, Spec};
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

/// The GPU a benchmark renders on: the adapter `SPHERECUT_ADAPTER` names,
/// else the best one there is, or a skip when that is a CPU or there is
/// none. An adapter that is there but won't give a device fails the
/// test: that is a fault to report, not missing hardware.
fn hardware_gpu() -> Gpu {
    tier2::require_hw_run();
    let choice = AdapterChoice::from_env().unwrap_or(AdapterChoice::Auto);
    let env = AdapterChoice::ENV;
    match Gpu::headless(&choice) {
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
        let gpu = hardware_gpu();
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

    fn player(&self, clock: Arc<dyn Clock>, decoder: DecoderPreference) -> Result<Player, String> {
        let mut parts = PlayerParts::new(
            Arc::new(FfmpegBackend::on_node(self.node.clone())),
            self.gpu.clone(),
            clock,
        );
        parts.hw = Some(self.hw.clone());
        parts.render_node = Some(self.node.path.clone());
        let config = PlayerConfig {
            decoder,
            ..PlayerConfig::default()
        };
        Player::new(parts, config).map_err(|e| e.to_string())
    }

    /// The decoders to run a stream of `format` on: the hardware kind
    /// Auto picks, or why it picks software; then the other kind where
    /// doctor says it decodes the format, and software. A kind left out
    /// gets a note under `label`.
    fn decoders(&self, format: &StreamFormat, label: &str) -> (Result<Run, String>, Vec<Run>) {
        let auto = choose(
            DecoderPreference::Auto,
            Some(&self.hw),
            Some(format),
            self.discrete(),
        );
        let picked = match auto.mode {
            DecodeMode::Hardware { kind, .. } => Some(kind),
            DecodeMode::Software => None,
        };
        let gated = match picked {
            Some(kind) => Ok(Run {
                decoder: DecoderPreference::Auto,
                label: format!("{kind} (Auto)"),
                kind: Some(kind),
            }),
            None => Err(auto.why),
        };
        let mut others = Vec::new();
        for kind in [HwKind::VaApi, HwKind::Vulkan] {
            if picked == Some(kind) {
                continue;
            }
            let check = format.check_format().unwrap_or("this format");
            if self.hw.works(check, kind) {
                others.push(Run {
                    decoder: DecoderPreference::Only(kind),
                    label: kind.to_string(),
                    kind: Some(kind),
                });
            } else {
                let why = self
                    .hw
                    .get(check, kind)
                    .and_then(|c| c.reason.clone())
                    .unwrap_or_else(|| format!("doctor didn't check {check} on {kind}"));
                bench::not_measured(&format!("{label}, {kind}"), why);
            }
        }
        others.push(Run {
            decoder: DecoderPreference::Software,
            label: "software".into(),
            kind: None,
        });
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
            at: [0; 3],
            current: Second::default(),
            seen,
            pass: Pass::default(),
        };
        m.at = m.counts();
        m.base = m.at;
        m
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
        self.pass
    }
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
    player
        .snapshot()
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
    player
        .snapshot()
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
    let player = rig.player(clock.clone(), run.decoder)?;
    player.open_streams(path.to_path_buf(), streams.to_vec());
    let pass = (|| -> Result<Pass, String> {
        let ui = UiThread::mark();
        let first = first_frame(&player, streams.len())?;
        let rate = stream_rate(&player);
        let mut shown = first.frames[0].frame;
        player.play();
        let mut meter = Meter::new(player.perf().clone(), streams.len(), Instant::now());
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
        drop(ui);
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
    let player = rig.player(clock, run.decoder)?;
    let mut screen = Screen::new(&rig.gpu, streams.len()).map_err(|e| e.to_string())?;
    player.open_streams(path.to_path_buf(), streams.to_vec());
    let pass = (|| -> Result<Pass, String> {
        let ui = UiThread::mark();
        let perf = player.perf().clone();
        let first = first_frame(&player, streams.len())?;
        screen.draw(&first, &perf).map_err(|e| e.to_string())?;
        let rate = stream_rate(&player);
        let mut last = first.frames[0].frame;
        player.play();
        let start = Instant::now();
        let mut meter = Meter::new(perf.clone(), streams.len(), start);
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
        drop(ui);
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

/// What a gated run must show (section 12.3): on `kind` throughout, no
/// drop or decode-drop after the first second of `length`, and every
/// unthrottled second at `fps` or more.
fn gate(name: &str, (u, r): &(Pass, Pass), kind: HwKind, length: Duration, fps: f64) {
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
    let (auto, others) = rig.decoders(&format, name);
    let streams = [None];
    let (length, limit) = if gated {
        (GATED, GATED_UNTHROTTLED)
    } else {
        (RECORDED, RECORDED)
    };
    let auto = auto.map(|run| {
        let passes = if gated {
            // A gated run that fails says why in the assertion.
            let u = unthrottled(&rig, &path, &streams, &run, limit)
                .unwrap_or_else(|e| panic!("{name}, {}: unthrottled: {e}", run.label));
            let r = realtime(&rig, &path, &streams, &run, length)
                .unwrap_or_else(|e| panic!("{name}, {}: real time: {e}", run.label));
            record_passes(name, &run, &u, &r);
            Some((u, r))
        } else {
            record(&rig, name, &path, &streams, &run, length, limit)
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
            run.kind.expect("Auto's run is on hardware"),
            length,
            GATE_FPS,
        ),
        Ok(_) => {}
    }
}

/// [`record`]'s rows for passes already run.
fn record_passes(clip: &str, run: &Run, u: &Pass, r: &Pass) {
    let name = format!("{clip}, {}", run.label);
    bench::record_runs(
        &format!("{name}: decoded fps, unthrottled"),
        &u.decoded_fps(),
        "fps",
    );
    bench::record_runs(
        &format!("{name}: decoded fps, real time"),
        &r.decoded_fps(),
        "fps",
    );
    bench::record_runs(
        &format!("{name}: shown fps, real time"),
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
    let dir = tier2::require_samples_dir();
    let mut files = Vec::new();
    walk(&dir, &mut files);
    files.sort();
    let probed = files
        .into_iter()
        .filter_map(|p| probe::probe(&p).ok().map(|r| (p, r)))
        .collect();
    (dir, probed)
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

#[test]
#[ignore = "tier2: benchmark, needs camera samples and a GPU that decodes them"]
fn bench_playback_x5_raw() {
    let (dir, probed) = samples();
    // The longest file of each mode: two lenses of a size at a rate.
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
    if modes.is_empty() {
        tier2::skip(format!(
            "no Insta360 dual-fisheye file with two lenses under {}",
            dir.display()
        ));
    }
    let rig = Rig::hardware();
    bench::note(format!("X5 raw: {}", rig.about()));
    let mut no_hardware = Vec::new();
    for (mode, (path, report, _)) in &modes {
        let streams: Vec<Option<usize>> = video_streams(report).into_iter().map(Some).collect();
        let format = format_of(report, streams[0]);
        bench::note(format!(
            "{mode}: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        let (auto, others) = rig.decoders(&format, mode);
        match auto {
            Ok(run) => {
                record(&rig, mode, path, &streams, &run, RECORDED, RECORDED);
            }
            Err(why) => no_hardware.push(format!("{mode}: {why}")),
        }
        for run in &others {
            record(&rig, mode, path, &streams, run, RECORDED, RECORDED);
        }
    }
    if no_hardware.len() == modes.len() {
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
        let (auto, others) = rig.decoders(&format, &name);
        match auto {
            Ok(run) => {
                let (length, limit) = if within {
                    (GATED, GATED_UNTHROTTLED)
                } else {
                    (RECORDED, RECORDED)
                };
                match record(&rig, &name, path, &[None], &run, length, limit) {
                    Some(passes) if within => gated.push((name.clone(), run, passes, fps)),
                    None if within => panic!("{name}, {}: see the note", run.label),
                    _ => {}
                }
            }
            Err(why) => no_hardware.push(format!("{name}: {why}")),
        }
        for run in &others {
            record(&rig, &name, path, &[None], run, RECORDED, RECORDED);
        }
    }
    for (name, run, passes, fps) in &gated {
        let kind = run.kind.expect("Auto's run is on hardware");
        gate(
            &format!("{name}, {}", run.label),
            passes,
            kind,
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

/// How fast software decodes up to `frames` of `path`, or for `limit`.
fn software_fps(path: &Path, frames: usize, limit: Duration) -> Result<f64, String> {
    let mut d = VideoDecoder::open(path, None, None).map_err(|e| e.to_string())?;
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
    for spec in synth_media::perf_clips() {
        let path = tier2::require_perf_clip(&spec.file_name());
        let name = format!("perf clip {}: software decode", spec.name);
        let frames = usize::try_from(spec.frames).unwrap_or(usize::MAX).min(300);
        let measured = bench::try_measure(&name, "fps", || {
            software_fps(&path, frames, Duration::from_secs(10))
        });
        if let Err(why) = measured {
            bench::not_measured(&name, why);
        }
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
    let run = Run {
        decoder: DecoderPreference::Software,
        label: "software".into(),
        kind: None,
    };
    let u = unthrottled(&rig, &path, &[None], &run, Duration::from_secs(60)).unwrap();
    let r = realtime(&rig, &path, &[None], &run, Duration::from_millis(2500)).unwrap();
    std::fs::remove_dir_all(&dir).ok();
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
