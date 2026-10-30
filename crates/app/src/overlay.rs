//! The performance overlay (M1 plan section 10): F3 or View ▸ Overlay
//! puts what the engine is doing over the viewer's top-left corner, read
//! every UI frame from the player's numbers ([`PerfStats`]), its
//! snapshot and its audio output. Nothing here waits: the numbers are
//! atomics, and the snapshot and the audio's are `try_lock` copies.
//!
//! Rates (decoded frames, draws that projected) are over the last second
//! of UI frames, from the counters as each frame read them; the A/V
//! offset is over the last second's samples; the rest are over the
//! stages' rings ([`spherecut_playback::perf::RING`] samples each).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use spherecut_audio::OutputStats;
use spherecut_core::video::{Matrix, Range};
use spherecut_media::backend::VideoStreamInfo;
use spherecut_playback::{
    CacheStats, Counter, MemoryStats, PerfSnapshot, PerfStats, SlotStats, Stage, StageSummary,
};
use spherecut_render::Filter;

use crate::app::AppState;

/// The window the rates are over.
const SECOND: Duration = Duration::from_secs(1);

/// UI frame intervals in its sparkline: 4 s at 60 Hz.
const SPARK_UI: usize = 240;

/// The overlay's own state: the counters over the last second, for the
/// rates, and what it showed last.
#[derive(Debug, Default)]
pub struct OverlayState {
    /// Each UI frame's reading, oldest first: the last second's, and the
    /// one before it.
    seen: VecDeque<Seen>,
    /// What the overlay showed last, while it is on.
    pub last: Option<Readings>,
}

#[derive(Debug, Clone, Copy)]
struct Seen {
    at: Instant,
    decoded: u64,
    projected: u64,
    encoded_only: u64,
}

impl OverlayState {
    /// Take this UI frame's counters. Every frame, the overlay shown or
    /// not, so that its rates are ready when it is turned on.
    pub fn tick(&mut self, now: Instant, perf: &PerfSnapshot) {
        self.seen.push_back(Seen {
            at: now,
            decoded: perf.counter(Counter::FramesDecoded),
            projected: perf.counter(Counter::Projected),
            encoded_only: perf.counter(Counter::EncodedOnly),
        });
        while self
            .seen
            .get(1)
            .is_some_and(|s| now.saturating_duration_since(s.at) >= SECOND)
        {
            self.seen.pop_front();
        }
    }

    /// The first and last readings, a quarter second apart at least.
    fn span(&self) -> Option<(Seen, Seen, f64)> {
        let (a, b) = (*self.seen.front()?, *self.seen.back()?);
        let dt = b.at.saturating_duration_since(a.at).as_secs_f64();
        (dt >= 0.25).then_some((a, b, dt))
    }

    /// Frames decoded per second, every stream's.
    fn decoded_fps(&self) -> Option<f64> {
        let (a, b, dt) = self.span()?;
        Some(b.decoded.saturating_sub(a.decoded) as f64 / dt)
    }

    /// Of the viewer's draws in the last second, those that only encoded
    /// (the projection skipped), and all of them.
    fn draws(&self) -> Option<(u64, u64)> {
        let (a, b, _) = self.span()?;
        let encoded = b.encoded_only.saturating_sub(a.encoded_only);
        let projected = b.projected.saturating_sub(a.projected);
        Some((encoded, encoded + projected))
    }
}

/// What the overlay shows, read once a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Readings {
    /// Where the first stream decodes.
    pub path: Option<String>,
    pub gpu: String,
    /// The first stream's codec, size, format and colour.
    pub stream: Option<String>,
    /// Its rate, and how many video streams there are.
    pub stream_fps: Option<f64>,
    pub streams: usize,
    pub state: String,
    pub decode: StageSummary,
    pub download: StageSummary,
    pub decoded_fps: Option<f64>,
    pub catching_up: bool,
    pub upload_cpu: StageSummary,
    pub upload_gpu: StageSummary,
    /// Frames into slots: imported as DMA-BUFs, copied out of Vulkan
    /// images, refused, and uploaded in all.
    pub imported: u64,
    pub vulkan: u64,
    pub refused: u64,
    pub uploaded: u64,
    pub render_cpu: StageSummary,
    pub render_gpu: StageSummary,
    /// The device writes timestamps between passes, and is a CPU (whose
    /// "GPU" times are CPU times).
    pub timestamps: bool,
    pub cpu_adapter: bool,
    /// The viewer, in physical pixels, at the scale.
    pub size: (u32, u32),
    pub ppp: f32,
    pub filter: Filter,
    /// The last second's draws that skipped the projection, and all.
    pub skipped: Option<(u64, u64)>,
    pub ui: StageSummary,
    pub hitches: u64,
    pub shown: u64,
    pub dropped: u64,
    pub decode_dropped: u64,
    /// The A/V offset's median and p95 over the last second.
    pub av: Option<(f32, f32)>,
    pub cache: Option<CacheStats>,
    pub slots: SlotStats,
    pub has_audio: bool,
    pub audio: Option<OutputStats>,
    pub memory: Option<MemoryStats>,
}

impl Readings {
    /// The numbers now, for a viewer of `size` physical pixels at `ppp`.
    pub fn read(s: &AppState, size: (u32, u32), ppp: f32) -> Readings {
        let snap = &s.snapshot;
        let p = &snap.perf;
        let info = s.stream0();
        let av = s
            .player
            .as_ref()
            .and_then(|pl| last_second(pl.perf(), p.stage(Stage::UiFrame)));
        Readings {
            path: snap
                .streams
                .first()
                .and_then(|st| st.decode.as_ref())
                .map(crate::panes::decode_path),
            gpu: s.gpu.caps.info.name.clone(),
            stream: info.map(stream_line),
            stream_fps: info.and_then(|i| i.frame_rate()).map(|r| r.to_f64()),
            streams: snap.streams.len(),
            state: format!("{:?}", snap.state),
            decode: *p.stage(Stage::Decode),
            download: *p.stage(Stage::Download),
            decoded_fps: s.overlay.decoded_fps(),
            catching_up: snap.streams.iter().any(|st| st.catching_up),
            upload_cpu: *p.stage(Stage::UploadCpu),
            upload_gpu: *p.stage(Stage::UploadGpu),
            imported: p.counter(Counter::FramesImported),
            vulkan: p.counter(Counter::VulkanCopies),
            refused: p.counter(Counter::ImportsRefused),
            uploaded: p.counter(Counter::FramesUploaded),
            render_cpu: *p.stage(Stage::RenderCpu),
            render_gpu: *p.stage(Stage::RenderGpu),
            timestamps: s.gpu.caps.timestamps_in_encoders,
            cpu_adapter: s.gpu.caps.is_cpu,
            size,
            ppp,
            filter: s.view.filter,
            skipped: s.overlay.draws(),
            ui: *p.stage(Stage::UiFrame),
            hitches: p.counter(Counter::Hitches),
            shown: p.counter(Counter::FramesShown),
            dropped: p.counter(Counter::Dropped),
            decode_dropped: p.counter(Counter::DecodeDropped),
            av,
            cache: snap.cache,
            slots: snap.slots,
            has_audio: snap.has_audio,
            audio: s.player.as_ref().and_then(|pl| pl.audio_stats()),
            memory: snap.memory,
        }
    }

    /// The overlay's lines, as section 10 lays them out.
    pub fn lines(&self) -> Vec<String> {
        let mut head = vec![self.path.clone().unwrap_or_else(|| "no decoder".into())];
        head.push(self.gpu.clone());
        if let Some(st) = &self.stream {
            head.push(st.clone());
        }
        if let Some(fps) = self.stream_fps {
            head.push(format!("{fps:.2} fps"));
        }
        head.push(self.state.clone());
        let decoded = self.decoded_fps.map_or_else(
            || "-".into(),
            |fps| {
                let needed = self
                    .stream_fps
                    .map(|r| r * self.streams.max(1) as f64)
                    .filter(|n| *n > 0.0);
                match needed {
                    Some(n) => format!("{fps:.1} fps ({:.2}× needed)", fps / n),
                    None => format!("{fps:.1} fps"),
                }
            },
        );
        let mut upload = format!(
            "upload    CPU {}   GPU {}",
            ms(self.upload_cpu.p50),
            self.gpu_ms(&self.upload_gpu)
        );
        if self.imported + self.vulkan + self.refused > 0 {
            upload.push_str(&format!(
                "   dmabuf {}  vulkan {}  copied {}  refused {}",
                self.imported,
                self.vulkan,
                self.uploaded.saturating_sub(self.imported + self.vulkan),
                self.refused
            ));
        } else {
            upload.push_str("   copied");
        }
        let skipped = self
            .skipped
            .map_or_else(|| "-".into(), |(s, n)| format!("{s}/{n}"));
        let drops = self.shown + self.dropped;
        let pct = if drops > 0 {
            100.0 * self.dropped as f64 / drops as f64
        } else {
            0.0
        };
        let av = self.av.map_or_else(
            || "-".into(),
            |(m, p95)| format!("{m:+.0} ms (p95 {p95:.0})"),
        );
        let cache = self.cache.as_ref().map_or_else(
            || "-".into(),
            |c| {
                format!(
                    "{:.2} / {:.2} GiB   ahead {} ({:.2} s)   behind {}",
                    gib(c.bytes),
                    gib(c.budget),
                    c.ahead,
                    c.ahead_span.to_secs_f64(),
                    c.behind
                )
            },
        );
        vec![
            head.join(" · "),
            format!(
                "decode    {} p95 {}   download {}   decoded {decoded}   catch-up {}",
                ms(self.decode.p50),
                num(self.decode.p95),
                num(self.download.p50),
                if self.catching_up { "on" } else { "off" }
            ),
            upload,
            format!(
                "render    CPU {}   GPU {}   {}×{} @{}   {}   projection skipped {skipped}",
                ms(self.render_cpu.p50),
                self.gpu_ms(&self.render_gpu),
                self.size.0,
                self.size.1,
                self.ppp,
                match self.filter {
                    Filter::Anisotropic => "aniso",
                    Filter::Isotropic => "iso",
                }
            ),
            format!(
                "UI        {} p99 {}   hitches {}",
                ms(self.ui.p50),
                num(self.ui.p99),
                self.hitches
            ),
            format!(
                "video     shown {}   dropped {} ({pct:.2}%)   decode-dropped {}   A/V {av}",
                self.shown, self.dropped, self.decode_dropped
            ),
            format!(
                "cache     {cache}   slots {}/{} ready",
                self.slots.ready, self.slots.active
            ),
            format!("audio     {}", self.audio_text()),
            format!("memory    {}", self.memory_text()),
        ]
    }

    /// A GPU time: "n/a" without timestamps, and marked as the CPU's on
    /// a CPU adapter.
    fn gpu_ms(&self, s: &StageSummary) -> String {
        if !self.timestamps {
            "n/a".into()
        } else if self.cpu_adapter {
            format!("{} (CPU timing)", ms(s.p50))
        } else {
            ms(s.p50)
        }
    }

    fn audio_text(&self) -> String {
        match &self.audio {
            Some(a) => {
                let period = if a.rate > 0 {
                    f64::from(a.period) * 1000.0 / f64::from(a.rate)
                } else {
                    0.0
                };
                format!(
                    "{} {:.1} kHz {} ch   period {period:.0} ms   latency {:.0} ms   underruns {}{}",
                    a.host,
                    f64::from(a.rate) / 1000.0,
                    a.channels,
                    a.latency.as_secs_f64() * 1000.0,
                    a.underruns,
                    if a.on_system_clock {
                        "   on the system clock"
                    } else {
                        ""
                    }
                )
            }
            None if self.has_audio => "starting".into(),
            None => "none".into(),
        }
    }

    fn memory_text(&self) -> String {
        let Some(m) = &self.memory else {
            return "-".into();
        };
        let rss = m
            .rss
            .map_or_else(|| "-".into(), |b| format!("{:.2} GiB", gib(b)));
        let gpu = m
            .gpu_allocated
            .map_or_else(|| "n/a".into(), |b| format!("{:.2} GiB", gib(b)));
        let faults = m
            .minor_faults_per_s
            .map_or_else(|| "-".into(), |n| format!("{n:.0}/s"));
        format!("RSS {rss}   GPU alloc {gpu}   minflt {faults}")
    }
}

/// The A/V offset's median and p95 over the last second: as many of the
/// last samples as UI frames fit in a second, since the presenter
/// records one a frame while the clock runs.
/// The p95 is of the offset's size, early or late.
fn last_second(perf: &PerfStats, ui: &StageSummary) -> Option<(f32, f32)> {
    let per_s = ui.p50.filter(|v| *v > 0.0).map_or(60, |v| {
        (1000.0 / v)
            .round()
            .clamp(1.0, spherecut_playback::perf::RING as f32) as usize
    });
    let mut signed = perf.samples(Stage::AvOffset);
    let mut signed = signed.split_off(signed.len().saturating_sub(per_s));
    let mut size: Vec<f32> = signed.iter().map(|v| v.abs()).collect();
    Some((rank(&mut signed, 0.5)?, rank(&mut size, 0.95)?))
}

/// The `p` quantile of `v` by the nearest rank, as the stages' are.
fn rank(v: &mut [f32], p: f32) -> Option<f32> {
    v.sort_by(f32::total_cmp);
    let n = v.len();
    (n > 0).then(|| v[((p * n as f32).ceil() as usize).clamp(1, n) - 1])
}

/// "hevc 5760×2880 nv12 full/709".
fn stream_line(info: &VideoStreamInfo) -> String {
    let c = &info.color;
    format!(
        "{} {}×{} {} {}/{}",
        info.codec,
        info.size.0,
        info.size.1,
        info.source_pix_fmt,
        match c.range {
            Range::Full => "full",
            Range::Limited => "limited",
        },
        match c.matrix {
            Matrix::Bt709 => "709",
            Matrix::Bt601 => "601",
            Matrix::Bt2020Ncl => "2020",
            Matrix::Smpte240m => "240m",
            Matrix::Fcc => "fcc",
            Matrix::Identity => "rgb",
        }
    )
}

/// Milliseconds to a tenth, or a hundredth under a tenth, so that a
/// short step doesn't read as none.
fn num(v: Option<f32>) -> String {
    v.map_or_else(
        || "-".into(),
        |v| {
            if v.abs() < 0.1 {
                format!("{v:.2}")
            } else {
                format!("{v:.1}")
            }
        },
    )
}

fn ms(v: Option<f32>) -> String {
    match v {
        Some(_) => format!("{} ms", num(v)),
        None => "-".into(),
    }
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1u32 << 30)
}

/// The overlay over `rect`, the viewer's, with the sparklines of the UI
/// frame interval and the shown video frame interval under its lines.
pub fn overlay_ui(s: &mut AppState, ui: &egui::Ui, rect: egui::Rect) {
    let ppp = ui.ctx().pixels_per_point();
    let size = (
        (rect.width() * ppp).round() as u32,
        (rect.height() * ppp).round() as u32,
    );
    let readings = Readings::read(s, size, ppp);
    let lines = readings.lines();
    s.overlay.last = Some(readings);
    let (ui_ms, shown_ms) = s.player.as_ref().map_or_else(Default::default, |p| {
        let mut ui_ms = p.perf().samples(Stage::UiFrame);
        let ui_ms = ui_ms.split_off(ui_ms.len().saturating_sub(SPARK_UI));
        (ui_ms, p.perf().samples(Stage::ShownInterval))
    });
    egui::Area::new(egui::Id::unique("spherecut-overlay"))
        .order(egui::Order::Middle)
        .fixed_pos(rect.left_top() + egui::vec2(8.0, 8.0))
        .constrain_to(rect)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(170))
                .corner_radius(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for line in lines {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(line)
                                    .monospace()
                                    .size(12.0)
                                    .color(egui::Color32::WHITE),
                            )
                            .extend(),
                        );
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        sparkline(ui, "UI frame", &ui_ms);
                        ui.add_space(12.0);
                        sparkline(ui, "video frame", &shown_ms);
                    });
                });
        });
}

/// Intervals in milliseconds as a line, scaled to their largest or 1.5×
/// their median, whichever is more, with the scale beside it.
fn sparkline(ui: &mut egui::Ui, what: &str, ms: &[f32]) {
    let mut sorted = ms.to_vec();
    sorted.sort_by(f32::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let top = sorted
        .last()
        .copied()
        .unwrap_or(0.0)
        .max(1.5 * median)
        .max(1.0);
    ui.vertical(|ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("{what} ms, 0–{top:.0}"))
                    .monospace()
                    .size(11.0)
                    .color(egui::Color32::LIGHT_GRAY),
            )
            .extend(),
        );
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(SPARK_UI as f32, 28.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(40)),
            egui::StrokeKind::Inside,
        );
        if ms.len() < 2 {
            return;
        }
        let dx = rect.width() / (SPARK_UI - 1) as f32;
        let x0 = rect.right() - dx * (ms.len() - 1) as f32;
        let points = ms
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let y = rect.bottom() - rect.height() * (v / top).clamp(0.0, 1.0);
                egui::pos2(x0 + dx * i as f32, y)
            })
            .collect();
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 220, 120)),
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readings() -> Readings {
        let at = |v: f32| StageSummary {
            recorded: 10,
            last: Some(v),
            mean: Some(v),
            p50: Some(v),
            p95: Some(v * 1.5),
            p99: Some(v * 2.0),
            max: Some(v * 3.0),
        };
        Readings {
            path: Some("VA-API".into()),
            gpu: "AMD Radeon RX 7600 (RADV NAVI33)".into(),
            stream: Some("hevc 5760×2880 nv12 full/709".into()),
            stream_fps: Some(30_000.0 / 1001.0),
            streams: 1,
            state: "Playing".into(),
            decode: at(6.0),
            download: at(2.3),
            decoded_fps: Some(61.2),
            catching_up: false,
            upload_cpu: at(1.4),
            upload_gpu: at(0.6),
            imported: 0,
            vulkan: 0,
            refused: 0,
            uploaded: 100,
            render_cpu: at(0.04),
            render_gpu: at(0.5),
            timestamps: true,
            cpu_adapter: false,
            size: (2560, 1440),
            ppp: 1.25,
            filter: Filter::Anisotropic,
            skipped: Some((112, 144)),
            ui: at(16.7),
            hitches: 0,
            shown: 1798,
            dropped: 2,
            decode_dropped: 0,
            av: Some((4.0, 9.0)),
            cache: None,
            slots: SlotStats {
                active: 4,
                ready: 2,
                ..SlotStats::default()
            },
            has_audio: false,
            audio: None,
            memory: Some(MemoryStats {
                rss: Some(5 << 29),
                minor_faults: Some(1000),
                minor_faults_per_s: Some(12.0),
                gpu_allocated: Some(1 << 29),
                gpu_reserved: Some(1 << 30),
            }),
        }
    }

    #[test]
    fn the_lines_read_as_section_10_lays_them_out() {
        let lines = readings().lines();
        assert_eq!(
            lines[0],
            "VA-API · AMD Radeon RX 7600 (RADV NAVI33) · hevc 5760×2880 nv12 full/709 · 29.97 fps · Playing"
        );
        assert_eq!(
            lines[1],
            "decode    6.0 ms p95 9.0   download 2.3   decoded 61.2 fps (2.04× needed)   catch-up off"
        );
        assert_eq!(lines[2], "upload    CPU 1.4 ms   GPU 0.6 ms   copied");
        assert_eq!(
            lines[3],
            "render    CPU 0.04 ms   GPU 0.5 ms   2560×1440 @1.25   aniso   projection skipped 112/144"
        );
        assert_eq!(lines[4], "UI        16.7 ms p99 33.4   hitches 0");
        assert_eq!(
            lines[5],
            "video     shown 1798   dropped 2 (0.11%)   decode-dropped 0   A/V +4 ms (p95 9)"
        );
        assert_eq!(lines[6], "cache     -   slots 2/4 ready");
        assert_eq!(lines[7], "audio     none");
        assert_eq!(
            lines[8],
            "memory    RSS 2.50 GiB   GPU alloc 0.50 GiB   minflt 12/s"
        );
    }

    #[test]
    fn gpu_times_say_when_they_arent() {
        let mut r = readings();
        r.timestamps = false;
        let lines = r.lines();
        assert!(lines[2].contains("GPU n/a"), "{}", lines[2]);
        assert!(lines[3].contains("GPU n/a"), "{}", lines[3]);
        r.timestamps = true;
        r.cpu_adapter = true;
        assert!(r.lines()[3].contains("GPU 0.5 ms (CPU timing)"));
        // Two lenses need twice the rate.
        r.streams = 2;
        assert!(r.lines()[1].contains("(1.02× needed)"), "{}", r.lines()[1]);
        r.has_audio = true;
        assert_eq!(r.lines()[7], "audio     starting");
        r.memory = None;
        assert_eq!(r.lines()[8], "memory    -");
    }

    #[test]
    fn rates_are_over_the_last_second() {
        let mut o = OverlayState::default();
        let t0 = Instant::now();
        let mut perf = PerfSnapshot::default();
        assert_eq!(o.decoded_fps(), None);
        for i in 0..=120u64 {
            // 60 UI frames a second, 30 decoded frames, one projection in
            // two draws.
            perf.counters[Counter::FramesDecoded as usize] = i / 2;
            perf.counters[Counter::Projected as usize] = i / 2;
            perf.counters[Counter::EncodedOnly as usize] = i - i / 2;
            o.tick(t0 + Duration::from_secs_f64(i as f64 / 60.0), &perf);
        }
        let fps = o.decoded_fps().unwrap();
        assert!((fps - 30.0).abs() < 1.0, "{fps}");
        let (skipped, all) = o.draws().unwrap();
        assert!((59..=61).contains(&all), "{all}");
        assert!(skipped.abs_diff(all / 2) <= 1, "{skipped} of {all}");
        // A second's readings, and the one before.
        assert!(o.seen.len() <= 62, "{}", o.seen.len());
    }
}
