//! The side panes (M1 plan sections 7.2 and 7.6): Info, Doctor and
//! Files, and the overlay. What reads the file system or runs FFmpeg runs
//! on a worker ([`Job`]).

use std::path::{Path, PathBuf};

use spherecut_core::video::Primaries;
use spherecut_media::backend::{DecodePath, VideoStreamInfo};
use spherecut_media::doctor::Severity;
use spherecut_media::spherical::{Projection, StereoMode};
use spherecut_playback::{Counter, Stage};

use crate::app::{AppState, DoctorState};
use crate::jobs::{Job, Poll, Threads};
use crate::notices::{Notice, NoticeKind};

/// The extensions the Files pane lists: MP4 and MOV, Matroska and WebM,
/// and the Insta360 X5's own (`.insv`, and `.lrv` previews).
pub const VIDEO_EXTENSIONS: [&str; 7] = ["mp4", "m4v", "mov", "mkv", "webm", "insv", "lrv"];

/// One row of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
}

type Listing = Result<Vec<Entry>, String>;

/// `dir`'s subdirectories and video files, directories first, then by
/// name; hidden ones left out. On the Files pane's worker.
#[expect(
    clippy::disallowed_methods,
    reason = "runs on the Files pane's worker thread, never the UI thread"
)]
pub fn list_dir(dir: &Path) -> Listing {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out: Vec<Entry> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = e.path();
            // Follows symlinks, so a link to a directory lists as one.
            let is_dir = path.is_dir();
            let video = path
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| VIDEO_EXTENSIONS.contains(&x.to_ascii_lowercase().as_str()));
            (is_dir || video).then_some(Entry {
                name,
                path,
                dir: is_dir,
            })
        })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// The Files pane, in place of a file dialog (section 7.2).
#[derive(Debug)]
pub struct FilesPane {
    dir: Option<PathBuf>,
    listing: Option<Listing>,
    job: Option<Job<(PathBuf, Listing)>>,
    /// Listings given up for a newer one.
    threads: Threads,
    enabled: bool,
    /// Bring the pane's tab forward on the next frame (File, Open…).
    pub focus: bool,
    edit: String,
}

impl FilesPane {
    /// The pane at `dir`, else the home directory. With `enabled` false
    /// it lists nothing (tests that keep off the file system).
    pub fn new(dir: Option<PathBuf>, enabled: bool) -> FilesPane {
        let dir = dir.or_else(|| std::env::var_os("HOME").map(PathBuf::from));
        let mut pane = FilesPane {
            edit: dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            dir: None,
            listing: None,
            job: None,
            threads: Threads::default(),
            enabled,
            focus: false,
        };
        if let Some(d) = dir {
            pane.list(d);
        }
        pane
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn pending(&self) -> bool {
        self.job.is_some()
    }

    /// List `dir`, on a worker.
    pub fn list(&mut self, dir: PathBuf) {
        self.edit = dir.display().to_string();
        self.dir = Some(dir.clone());
        self.listing = None;
        if self.enabled {
            let job = Job::spawn("files", move || {
                let l = list_dir(&dir);
                (dir, l)
            });
            if let Some(old) = self.job.replace(job) {
                self.threads.keep(old);
            }
        }
    }

    /// The listings' threads still running, for the join at exit.
    pub fn take_threads(&mut self) -> Vec<std::thread::JoinHandle<()>> {
        if let Some(job) = self.job.take() {
            self.threads.keep(job);
        }
        self.threads.take()
    }

    /// Take a listing the worker finished.
    pub fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        match job.poll() {
            Poll::Pending => {}
            Poll::Done((dir, listing)) => {
                // A listing for a directory left since is dropped.
                if self.dir.as_deref() == Some(dir.as_path()) {
                    self.listing = Some(listing);
                }
                self.job = None;
            }
            Poll::Lost => {
                self.listing = Some(Err("the listing stopped".into()));
                self.job = None;
            }
        }
    }

    /// The pane; returns the file clicked, to open.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        let mut go: Option<PathBuf> = None;
        ui.horizontal(|ui| {
            let up = self
                .dir
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            if ui
                .add_enabled(up.is_some(), egui::Button::new("Up"))
                .on_hover_text("The parent directory")
                .clicked()
            {
                go = up;
            }
            if ui.button("Refresh").on_hover_text("List again").clicked() {
                go = self.dir.clone();
            }
            let r = ui.add(egui::TextEdit::singleline(&mut self.edit).desired_width(f32::INFINITY));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                go = Some(PathBuf::from(self.edit.trim()));
            }
        });
        ui.separator();
        let mut open = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| match &self.listing {
                None if self.job.is_some() => {
                    ui.spinner();
                }
                None => {
                    ui.weak("Nothing listed");
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                Some(Ok(entries)) if entries.is_empty() => {
                    ui.weak("No videos or folders here");
                }
                Some(Ok(entries)) => {
                    for e in entries {
                        let label = if e.dir {
                            format!("{}/", e.name)
                        } else {
                            e.name.clone()
                        };
                        if ui.selectable_label(false, label).clicked() {
                            if e.dir {
                                go = Some(e.path.clone());
                            } else {
                                open = Some(e.path.clone());
                            }
                        }
                    }
                }
            });
        if let Some(d) = go {
            self.list(d);
        }
        open
    }
}

/// The Info pane's probe of the open file.
#[derive(Debug, Default)]
pub struct InfoPane {
    path: Option<PathBuf>,
    job: Option<Job<Result<String, String>>>,
    /// Probes given up for a newer file's.
    threads: Threads,
    text: Option<Result<String, String>>,
}

impl InfoPane {
    /// Probe `path`, on a worker.
    pub fn open(&mut self, path: &Path) {
        let p = path.to_path_buf();
        self.path = Some(p.clone());
        self.text = None;
        let job = Job::spawn("probe", move || {
            spherecut_media::probe::probe(&p)
                .map(|r| spherecut_media::probe::render_text(&r))
                .map_err(|e| e.error.to_string())
        });
        if let Some(old) = self.job.replace(job) {
            self.threads.keep(old);
        }
    }

    /// The probes' threads still running, for the join at exit.
    pub fn take_threads(&mut self) -> Vec<std::thread::JoinHandle<()>> {
        if let Some(job) = self.job.take() {
            self.threads.keep(job);
        }
        self.threads.take()
    }

    pub fn pending(&self) -> bool {
        self.job.is_some()
    }

    pub fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        match job.poll() {
            Poll::Pending => {}
            Poll::Done(t) => {
                self.text = Some(t);
                self.job = None;
            }
            Poll::Lost => {
                self.text = Some(Err("the probe stopped".into()));
                self.job = None;
            }
        }
    }
}

fn decode_path(p: &DecodePath) -> String {
    match p {
        DecodePath::Software { threads } => match threads {
            Some(n) => format!("software, {n} threads"),
            None => "software".into(),
        },
        DecodePath::Hardware { kind } => format!("{kind}"),
        DecodePath::FellBack { from, reason } => format!("software, after {from}: {reason}"),
    }
}

fn mark(assumed: bool) -> &'static str {
    if assumed { " (assumed)" } else { "" }
}

/// The colour and mapping of a stream, as the renderer takes them.
pub fn stream_text(info: &VideoStreamInfo) -> String {
    let c = &info.color;
    let a = &c.assumed;
    let mut s = format!(
        "{} {}x{}, {}\n",
        info.codec, info.size.0, info.size.1, info.source_pix_fmt
    );
    s.push_str(&format!(
        "coded: primaries {}, transfer {}, matrix {}, range {}, chroma location {}\n",
        c.codes.primaries, c.codes.transfer, c.codes.matrix, c.codes.range, c.codes.chroma_location
    ));
    s.push_str(&format!(
        "shown as: matrix {:?}{}, range {:?}{}, transfer {:?}{}, primaries {:?}{}, siting {:?}{}\n",
        c.matrix,
        mark(a.matrix),
        c.range,
        mark(a.range),
        c.transfer,
        mark(a.transfer),
        c.primaries,
        mark(a.primaries),
        c.siting,
        mark(a.siting)
    ));
    let m = &info.mapping;
    s.push_str(&format!(
        "mapping: {}, cover {:?}, {:?}{}\n",
        match &m.projection {
            None => "no spherical metadata (shown as equirect)".to_owned(),
            Some(p) => format!("{p:?}"),
        },
        m.cover,
        m.stereo,
        if m.pose_present {
            ", a pose (not applied)"
        } else {
            ""
        }
    ));
    s
}

/// The notices a stream's info calls for (section 7.8).
pub fn stream_notices(info: &VideoStreamInfo) -> Vec<Notice> {
    use NoticeKind as K;
    let mut v = Vec::new();
    let m = &info.mapping;
    match &m.projection {
        Some(Projection::Equirect { .. }) | None => {}
        Some(p) => v.push(
            Notice::new(K::NotEquirect, Severity::Warning, "Not an equirect source").impact(
                format!(
                    "it is {p:?}, projected as if it were equirect; its raw view comes in M1's \
                     next slice, and stitching arrives in M3"
                ),
            ),
        ),
    }
    if !matches!(m.stereo, StereoMode::Mono) {
        v.push(Notice::new(
            K::Stereo,
            Severity::Info,
            "Stereo 3D: showing the left (or top) eye",
        ));
    }
    if info.color.transfer.is_hdr() || info.color.primaries == Primaries::Bt2020 {
        v.push(
            Notice::new(K::Hdr, Severity::Warning, "HDR or BT.2020 colour")
                .impact("shown as SDR BT.709 until M7's colour pipeline"),
        );
    }
    if m.pose_present {
        v.push(
            Notice::new(K::Pose, Severity::Info, "This file has an sv3d pose")
                .impact("M1 shows the picture as stored, without it"),
        );
    }
    let a = &info.color.assumed;
    if a.any() {
        let fields: Vec<&str> = [
            (a.matrix, "matrix"),
            (a.range, "range"),
            (a.transfer, "transfer"),
            (a.primaries, "primaries"),
            (a.siting, "chroma siting"),
        ]
        .into_iter()
        .filter_map(|(on, n)| on.then_some(n))
        .collect();
        v.push(
            Notice::new(K::ColorAssumed, Severity::Info, "Colour fields assumed")
                .impact(format!("the file doesn't say its {}", fields.join(", "))),
        );
    }
    v
}

pub fn info_ui(s: &mut AppState, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            if s.snapshot.streams.is_empty() {
                ui.weak("Nothing open");
                return;
            }
            for (i, st) in s.snapshot.streams.iter().enumerate() {
                ui.strong(format!("Stream {i}"));
                if let Some(c) = &st.choice {
                    ui.label(format!("decoder: {c}"));
                }
                if let Some(d) = &st.decode {
                    ui.label(format!("decoding on: {}", decode_path(d)));
                }
                if let Some(e) = &st.error {
                    ui.colored_label(ui.visuals().error_fg_color, &e.message);
                }
                if let Some(Some(info)) = s.streams.get(i) {
                    ui.monospace(stream_text(info));
                }
            }
            ui.separator();
            match &s.info.text {
                None if s.info.pending() => {
                    ui.spinner();
                }
                None => {}
                Some(Ok(t)) => {
                    ui.monospace(t);
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
        });
}

pub fn doctor_ui(s: &mut AppState, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            if !s.app_issues.is_empty() {
                ui.strong("Spherecut found");
                for i in &s.app_issues {
                    ui.label(format!("{}: {}", i.what, i.impact));
                    for line in i.fix.lines() {
                        ui.monospace(format!("  {}", line.trim()));
                    }
                }
                ui.separator();
            }
            match &s.doctor {
                DoctorState::Pending(_) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Checking codecs and hardware…");
                    });
                }
                DoctorState::Lost => {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "The checks stopped without a report: run `spherecut doctor` in a terminal",
                    );
                }
                DoctorState::Done(r) => {
                    let text = spherecut_media::doctor::render_text(r);
                    if ui.button("Copy the report").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                    ui.monospace(text);
                }
            }
        });
}

fn ms(v: Option<f32>) -> String {
    v.map_or_else(|| "-".into(), |v| format!("{v:.1}"))
}

/// The overlay (F3): what the engine is doing, in the viewer's corner.
/// Slice 8 adds the GPU timestamps and sparklines.
pub fn overlay_ui(s: &AppState, ui: &egui::Ui, rect: egui::Rect) {
    let snap = &s.snapshot;
    let p = &snap.perf;
    let shown = p.stage(Stage::ShownInterval).p50;
    let mut lines = vec![
        format!(
            "{:?}{}",
            snap.state,
            if snap.playing { ", playing" } else { "" }
        ),
        format!(
            "fps {}  dropped {}  decode-dropped {}  hitches {}",
            shown.map_or_else(|| "-".into(), |v| format!("{:.1}", 1000.0 / v.max(0.001))),
            p.counter(Counter::Dropped),
            p.counter(Counter::DecodeDropped),
            p.counter(Counter::Hitches)
        ),
        format!(
            "decode {}  download {}  upload {} ms (p50)",
            ms(p.stage(Stage::Decode).p50),
            ms(p.stage(Stage::Download).p50),
            ms(p.stage(Stage::UploadCpu).p50)
        ),
        format!(
            "slots {} ready {} shown {} dead {}",
            snap.slots.active, snap.slots.ready, snap.slots.shown, snap.slots.dead
        ),
    ];
    if let Some(c) = &snap.cache {
        lines.push(format!(
            "cache {} / {} MiB, {} frames ({} ahead)",
            c.bytes >> 20,
            c.budget >> 20,
            c.frames,
            c.ahead
        ));
    }
    for st in &snap.streams {
        if let Some(d) = &st.decode {
            lines.push(format!("decoding on {}", decode_path(d)));
        }
    }
    // Zero copy (slice 5b): how the frames reached their slots.
    let imported = p.counter(Counter::FramesImported);
    let vulkan = p.counter(Counter::VulkanCopies);
    let refused = p.counter(Counter::ImportsRefused);
    if imported + vulkan + refused > 0 {
        lines.push(format!(
            "into slots: dmabuf {imported}  vulkan {vulkan}  copy {}  refused {refused}",
            p.counter(Counter::FramesUploaded)
                .saturating_sub(imported + vulkan)
        ));
    }
    lines.push(format!(
        "{}  {}x{} px",
        spherecut_render::gpu::describe(&s.gpu.caps.info),
        (rect.width() * ui.ctx().pixels_per_point()).round(),
        (rect.height() * ui.ctx().pixels_per_point()).round()
    ));
    let text = lines.join("\n");
    let painter = ui.painter_at(rect);
    let font = egui::FontId::monospace(12.0);
    let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
    let at = rect.left_top() + egui::vec2(8.0, 8.0);
    painter.rect_filled(
        egui::Rect::from_min_size(at, galley.size()).expand(4.0),
        4.0,
        egui::Color32::from_black_alpha(160),
    );
    painter.galley(at, galley, egui::Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_has_folders_first_and_only_videos() {
        let root = &std::env::temp_dir().join(format!("spherecut-files-{}", std::process::id()));
        std::fs::create_dir_all(root).unwrap();
        std::fs::create_dir(root.join("b-folder")).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        for f in ["z.MP4", "a.mkv", "notes.txt", "c.insv", ".x.mp4"] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let names: Vec<String> = list_dir(root)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["b-folder", "a.mkv", "c.insv", "z.MP4"]);
        assert!(list_dir(&root.join("missing")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
