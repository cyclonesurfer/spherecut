//! An export job and its handle (M2 plan section 8.6, amendment 2 item
//! 32): what to export and where, and the [`Export`] that the CLI, the
//! app and agents all drive: its progress, cancel, and the result.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_core::cancel::StopSource;
use spherecut_core::project::{Codec, EncoderChoice, ExportSettings, Project};
use spherecut_core::time::{FrameRate, Time};
use spherecut_media::encode::has_encoder;
use spherecut_media::hw::{self, GpuIds, RenderNode};
use spherecut_render::Gpu;

use crate::check::Checked;
use crate::encoder::{EncoderKind, EncoderSetup};
use crate::error::ExportError;
use crate::pace::Pace;
use crate::pipeline;
use crate::plan::FramePlan;
use crate::preset::Preset;
use crate::warning::Warning;

/// What to export, and where.
#[derive(Debug, Clone)]
pub struct ExportJob {
    /// The project whose one sequence is exported.
    pub project: Project,
    /// The file to write.
    pub output: PathBuf,
    /// Replace a regular file at `output` once the new one is whole
    /// (`--overwrite`, D16); otherwise one there is refused.
    pub overwrite: bool,
    /// How to export: the sequence's own choices, or flags' overrides of
    /// them for this run.
    pub settings: ExportSettings,
    /// Export 8-bit even from a 10-bit source (section 8.3).
    pub eight_bit: bool,
    /// Render at [`Preset::draft_size`] with the encoder's fastest
    /// settings, for a quick look (`--draft`, section 9).
    pub draft: bool,
    /// Where the source is decoded (amendment 2 item 25).
    pub decoder: Decoder,
    /// Mesa's quality level for a VA-API encoder
    /// ([`quality_level`](crate::encoder::quality_level)) in place of the
    /// quality's: for the benchmark that measured them.
    pub vaapi_level: Option<u32>,
    /// The device to render on: the app's, whose submissions wait at its
    /// gate while the UI presents (D41), or a headless one.
    pub gpu: Gpu,
    /// The app's pace (amendment 2 item 22): a frame a UI pass while the
    /// viewer plays or a drag runs. None runs free.
    pub pace: Option<Arc<Pace>>,
}

/// Where an export decodes its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decoder {
    /// As `spherecut frame --decoder auto` does (D39): VA-API on the
    /// render node of the GPU that renders, going on in software if it
    /// can't, which the report says (D3); software when there is no node.
    #[default]
    Auto,
    Software,
}

/// A hardware path an export didn't take, or left: what it was, and why
/// (D3). D27's codes say which: `hw_unavailable` (no device, or no such
/// encoder in this FFmpeg), `hw_unsupported` (it turned the job down) or
/// `hw_failed` (it failed on a frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    /// FFmpeg's name for it: `vaapi` for a decoder's device,
    /// `hevc_vaapi` for an encoder.
    pub from: &'static str,
    pub code: &'static str,
    pub reason: String,
}

/// Where an export's frames were decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeReport {
    /// FFmpeg's name for the device (`vaapi`), or `software`: where the
    /// last frame was decoded.
    pub path: &'static str,
    /// The hardware decode that `auto` passed over or left for software.
    pub fallback: Option<Fallback>,
}

impl ExportJob {
    /// `project`'s sequence with its own export settings, to `output`,
    /// refusing a file there.
    pub fn new(project: Project, output: impl Into<PathBuf>, gpu: Gpu) -> ExportJob {
        let settings = project.the_sequence().export;
        ExportJob {
            project,
            output: output.into(),
            overwrite: false,
            settings,
            eight_bit: false,
            draft: false,
            decoder: Decoder::Auto,
            vaapi_level: None,
            gpu,
            pace: None,
        }
    }
}

/// How far an export has got.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// Frames muxed into the file: only ever increasing.
    pub frames_done: u64,
    pub frames_total: u64,
    /// Frames muxed a second over the last two seconds, 0 until there
    /// are two to measure between.
    pub fps: f64,
    pub elapsed: Duration,
    /// At `fps`, until the last frame is muxed.
    pub eta: Option<Duration>,
    /// FFmpeg's name for the encoder.
    pub encoder: &'static str,
    /// The hardware encoder passed over or left for this one.
    pub fallback: Option<Fallback>,
    /// The hardware decode passed over or left for software.
    pub decode_fallback: Option<Fallback>,
    /// The last pass, which writes the finished file with its metadata
    /// and the moov first: bytes written and in all, once it has started.
    pub finishing: Option<(u64, u64)>,
}

/// A finished export.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub output: PathBuf,
    pub frames: u64,
    /// `frames / rate`.
    pub duration: Time,
    pub size: (u32, u32),
    /// At the draft's size and speed.
    pub draft: bool,
    pub rate: FrameRate,
    pub codec: Codec,
    pub encoder: &'static str,
    /// The hardware encoder that the export passed over, or restarted
    /// without when it failed in its first second (section 8.3).
    pub fallback: Option<Fallback>,
    pub decode: DecodeReport,
    /// The video is 10-bit.
    pub ten_bit: bool,
    /// The audio track, if the export has one.
    pub audio: Option<AudioReport>,
    /// The file's size.
    pub bytes: u64,
    /// Frames whose motion blur needed more views than the cap and got
    /// that many (render's `FrameViews::capped`).
    pub blur_capped: u64,
    /// What the export did that the project didn't say: the left eye of
    /// a stereo source, a crop, a pose not applied.
    pub warnings: Vec<Warning>,
    /// The checks the file passed before it was put in place (section 9).
    pub checks: Vec<Checked>,
    /// Where the time went.
    pub stages: StageTimes,
    pub elapsed: Duration,
}

/// Each stage's busy time over the try that made the file. The decode,
/// render and encode stages run at once on threads of their own, so the
/// busiest sets the pace; the finish comes after them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StageTimes {
    /// Seeking to the trim's start and decoding the frames.
    pub decode: Duration,
    /// Uploading each frame and drawing its views, until the GPU had
    /// drawn them.
    pub render: Duration,
    /// Copying each frame's planes back from the GPU.
    pub readback: Duration,
    /// Encoding and muxing the frames, and the audio between them.
    pub encode: Duration,
    /// After the last frame: the encoder's flush, the last pass that
    /// writes the finished file, and the checks.
    pub finish: Duration,
}

/// An export's audio track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioReport {
    /// The source's stream it came from, by index.
    pub stream: usize,
    /// FFmpeg's name for the encoder.
    pub encoder: &'static str,
    pub rate: u32,
    pub channels: u16,
    /// First-order ambiX, with an `SA3D` box.
    pub ambix: bool,
    /// Samples a channel: the trim's length at `rate`, to within one.
    pub samples: u64,
}

/// What the threads and the handle share: the progress counters.
#[derive(Debug)]
pub(crate) struct Shared {
    started: Instant,
    total: u64,
    /// The encoder now, and the hardware one it took over from.
    encoder: Mutex<(&'static str, Option<Fallback>)>,
    decode_fallback: Mutex<Option<Fallback>>,
    done: AtomicU64,
    /// When frames were muxed over the last two seconds, oldest first,
    /// with the one before them.
    window: Mutex<VecDeque<(Instant, u64)>>,
    /// The last pass's bytes written and in all; all 0 until it starts.
    finished: AtomicU64,
    finishing_total: AtomicU64,
}

/// The span `Progress::fps` measures over.
const FPS_WINDOW: Duration = Duration::from_secs(2);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl Shared {
    pub(crate) fn new(total: u64, encoder: &'static str, fallback: Option<Fallback>) -> Shared {
        Shared {
            started: Instant::now(),
            total,
            encoder: Mutex::new((encoder, fallback)),
            decode_fallback: Mutex::new(None),
            done: AtomicU64::new(0),
            window: Mutex::new(VecDeque::new()),
            finished: AtomicU64::new(0),
            finishing_total: AtomicU64::new(0),
        }
    }

    /// The last pass has written `done` bytes of `total`.
    pub(crate) fn finishing(&self, done: u64, total: u64) {
        self.finishing_total.store(total, Ordering::Release);
        self.finished.store(done, Ordering::Release);
    }

    /// The export goes on with `encoder`, having left `fallback`'s.
    pub(crate) fn encoder(&self, encoder: &'static str, fallback: Option<Fallback>) {
        *lock(&self.encoder) = (encoder, fallback);
    }

    /// The decode left the hardware, or never had it.
    pub(crate) fn decode_fell_back(&self, fallback: Fallback) {
        lock(&self.decode_fallback).get_or_insert(fallback);
    }

    /// `done` frames are muxed. After an encoder's restart (section 8.3)
    /// the count stays where the first got to until the next passes it,
    /// since progress only goes forward.
    pub(crate) fn muxed(&self, done: u64) {
        let now = Instant::now();
        let done = self.done.fetch_max(done, Ordering::AcqRel).max(done);
        let mut w = self
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        w.push_back((now, done));
        while w.len() > 2 && w.get(1).is_some_and(|&(t, _)| now - t > FPS_WINDOW) {
            w.pop_front();
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn progress(&self) -> Progress {
        let done = self.done.load(Ordering::Acquire);
        let fps = {
            let w = self
                .window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match (w.front(), w.back()) {
                (Some(&(t0, n0)), Some(&(t1, n1))) if t1 > t0 => {
                    (n1 - n0) as f64 / (t1 - t0).as_secs_f64()
                }
                _ => 0.0,
            }
        };
        let eta = (fps > 0.0).then(|| Duration::from_secs_f64((self.total - done) as f64 / fps));
        let (encoder, fallback) = lock(&self.encoder).clone();
        Progress {
            frames_done: done,
            frames_total: self.total,
            fps,
            elapsed: self.started.elapsed(),
            eta,
            encoder,
            fallback,
            decode_fallback: lock(&self.decode_fallback).clone(),
            finishing: match self.finishing_total.load(Ordering::Acquire) {
                0 => None,
                total => Some((self.finished.load(Ordering::Acquire), total)),
            },
        }
    }
}

/// A running export. Dropping it cancels the export and waits for it.
#[derive(Debug)]
pub struct Export {
    shared: Arc<Shared>,
    stop: StopSource,
    result: mpsc::Receiver<Result<Report, ExportError>>,
    done: Option<Result<Report, ExportError>>,
    thread: Option<JoinHandle<()>>,
}

impl Export {
    /// Check `job` and start exporting it. The checks that need no file
    /// or device are made here: the sequence's output is a preset, its
    /// trim has frames, the encoder is there, nothing is at the output
    /// (unless `overwrite`). The rest fail through [`Export::wait`].
    pub fn start(job: ExportJob) -> Result<Export, ExportError> {
        let sequence = job.project.the_sequence();
        let clip = job.project.the_clip();
        if job.project.media_by_id(clip.media).is_none() {
            return Err(ExportError::Invalid(format!(
                "the clip's media {} isn't in the project",
                clip.media.0
            )));
        }
        let preset = Preset::of(&sequence.output).ok_or_else(|| {
            let (w, h) = sequence.output.size();
            ExportError::Invalid(format!("{w}x{h} isn't one of the export presets"))
        })?;
        let plan = FramePlan::new(clip.source, sequence.rate)?;
        let node = render_node(&job.gpu);
        let size = if job.draft {
            preset.draft_size()
        } else {
            preset.size()
        };
        let encoders = Encoders::choose(
            &job.settings,
            preset,
            size,
            sequence.rate,
            &node,
            &has_encoder,
        )?;
        if !job.overwrite && std::fs::symlink_metadata(&job.output).is_ok() {
            return Err(ExportError::OutputExists(job.output.clone()));
        }
        let shared = Arc::new(Shared::new(
            plan.count(),
            encoders.first_name(),
            encoders.skipped.clone(),
        ));
        let stop = StopSource::new();
        let (tx, result) = mpsc::channel();
        let thread = {
            let (shared, stop) = (shared.clone(), stop.clone());
            std::thread::Builder::new()
                .name("export".into())
                .spawn(move || {
                    let r = pipeline::run(&job, preset, plan, &node, &encoders, &shared, &stop);
                    tx.send(r).ok();
                })
                .map_err(|e| ExportError::Invalid(format!("starting the export: {e}")))?
        };
        Ok(Export {
            shared,
            stop,
            result,
            done: None,
            thread: Some(thread),
        })
    }

    pub fn progress(&self) -> Progress {
        self.shared.progress()
    }

    /// Stop the export: every stage within about one frame's work
    /// (section 8.6 asks 2 s), leaving nothing at the output.
    /// [`Export::wait`] then gives [`ExportError::Cancelled`].
    pub fn cancel(&self) {
        self.stop.shutdown();
    }

    /// The result, once the export has ended, waiting up to `timeout`
    /// for it; `None` while it runs.
    pub fn wait(&mut self, timeout: Duration) -> Option<Result<Report, ExportError>> {
        if self.done.is_none() {
            match self.result.recv_timeout(timeout) {
                Ok(r) => {
                    self.done = Some(r);
                    if let Some(t) = self.thread.take() {
                        t.join().ok();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // The thread ended without a result: it panicked.
                    let why = match self.thread.take().map(JoinHandle::join) {
                        Some(Err(p)) => panic_text(&p),
                        _ => "no result".into(),
                    };
                    self.done = Some(Err(ExportError::Invalid(format!(
                        "the export stopped: {why}"
                    ))));
                }
            }
        }
        self.done.clone()
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        if self.done.is_none() {
            self.cancel();
        }
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}

/// The render node of the GPU that renders, where VA-API decodes and
/// encodes (D30): its PCI bus id's, else the first with its PCI ids, else
/// `renderD128`. lavapipe names no GPU.
pub(crate) fn render_node(gpu: &Gpu) -> RenderNode {
    let info = &gpu.caps.info;
    let ids = (!gpu.caps.is_cpu).then(|| GpuIds {
        pci_bus_id: info.device_pci_bus_id.clone(),
        vendor: info.vendor,
        device: info.device,
    });
    hw::render_node(ids.as_ref(), None)
}

/// The encoders an export tries for its codec, in order (section 8.3),
/// and the hardware one `auto` passed over without trying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Encoders {
    pub kinds: Vec<EncoderKind>,
    pub skipped: Option<Fallback>,
    /// FFmpeg's names for them: the codec's VA-API encoder, its software
    /// one.
    pub(crate) names: (&'static str, &'static str),
}

/// Why VA-API's AV1 encoder can't take frames of `size`, if it can't
/// as they are: AMD's VCN 4 pads AV1 to a multiple of 64 × 16 (1080
/// lines to 1082) and decoders show the padding (odin, 2026-10-31: a
/// 1080 × 1080 export decoded at 1088 × 1082), so AV1 goes to VA-API
/// only at a size it needn't pad, on any driver until one is measured
/// not to. H.264 and HEVC crop theirs.
pub(crate) fn av1_vaapi_pads(size: (u32, u32)) -> Option<String> {
    let (w, h) = size;
    (w % 64 != 0 || h % 16 != 0).then(|| {
        format!(
            "av1_vaapi would pad {w}x{h}: VA-API pads AV1 to a multiple of 64x16 (AMD's VCN 4 \
             pads 1080 lines to 1082), and players show the padding"
        )
    })
}

impl Encoders {
    /// The list for `settings` at the encoded `size`: `vaapi` and
    /// `software` the one each (an error for a missing one, or for AV1 at
    /// a size VA-API pads, which the CLI's exit 3 says), and `auto`
    /// VA-API where there is a render node, this FFmpeg has its encoder
    /// and it needn't pad, then software. `has` says whether FFmpeg has
    /// an encoder.
    pub(crate) fn choose(
        settings: &ExportSettings,
        preset: Preset,
        size: (u32, u32),
        rate: FrameRate,
        node: &RenderNode,
        has: &dyn Fn(&str) -> bool,
    ) -> Result<Encoders, ExportError> {
        // The names don't depend on the bit depth, which the source sets.
        let name = |kind| {
            EncoderSetup::of(kind, settings.codec, settings.quality, preset, rate, false).name
        };
        let names = (name(EncoderKind::Vaapi), name(EncoderKind::Software));
        let (vaapi, software) = names;
        let codec = settings.codec;
        let no_device = || {
            (!node.path.exists()).then(|| {
                format!(
                    "no VA-API device: no render node at {}",
                    node.path.display()
                )
            })
        };
        let missing = |name: &str| format!("this FFmpeg has no {name}, which exports {codec:?}");
        let pads = (codec == Codec::Av1)
            .then(|| av1_vaapi_pads(size))
            .flatten();
        let (kinds, skipped) = match settings.encoder {
            EncoderChoice::Vaapi => {
                if !has(vaapi) {
                    return Err(ExportError::NoEncoder(missing(vaapi)));
                }
                if let Some(why) = no_device() {
                    return Err(ExportError::NoEncoder(format!("{vaapi}: {why}")));
                }
                if let Some(message) = pads {
                    return Err(ExportError::Hardware {
                        encoder: vaapi,
                        code: "hw_unsupported",
                        message,
                    });
                }
                (vec![EncoderKind::Vaapi], None)
            }
            EncoderChoice::Software if has(software) => (vec![EncoderKind::Software], None),
            EncoderChoice::Software => return Err(ExportError::NoEncoder(missing(software))),
            EncoderChoice::Auto => {
                let why_not = if has(vaapi) {
                    no_device()
                        .map(|why| ("hw_unavailable", why))
                        .or_else(|| pads.map(|why| ("hw_unsupported", why)))
                } else {
                    Some(("hw_unavailable", missing(vaapi)))
                };
                let mut kinds = Vec::new();
                if why_not.is_none() {
                    kinds.push(EncoderKind::Vaapi);
                }
                if has(software) {
                    kinds.push(EncoderKind::Software);
                }
                if kinds.is_empty() {
                    return Err(ExportError::NoEncoder(format!(
                        "{}, and {}",
                        why_not.map(|(_, why)| why).unwrap_or_default(),
                        missing(software)
                    )));
                }
                let skipped = why_not.map(|(code, reason)| Fallback {
                    from: vaapi,
                    code,
                    reason,
                });
                (kinds, skipped)
            }
        };
        Ok(Encoders {
            kinds,
            skipped,
            names,
        })
    }

    pub(crate) fn name(&self, kind: EncoderKind) -> &'static str {
        match kind {
            EncoderKind::Vaapi => self.names.0,
            EncoderKind::Software => self.names.1,
        }
    }

    fn first_name(&self) -> &'static str {
        self.name(self.kinds[0])
    }
}

pub(crate) fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".into())
}

#[cfg(test)]
mod tests {
    use spherecut_core::project::{Aspect, AudioChoice, Quality};
    use spherecut_media::hw::NodeChoice;

    use super::*;

    fn settings(codec: Codec, encoder: EncoderChoice) -> ExportSettings {
        ExportSettings {
            codec,
            quality: Quality::High,
            encoder,
            audio: AudioChoice::None,
            motion_blur: false,
        }
    }

    const WIDE: Preset = Preset::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };

    fn node(path: &std::path::Path) -> RenderNode {
        RenderNode {
            path: path.to_path_buf(),
            chosen: NodeChoice::Default,
        }
    }

    #[test]
    fn auto_tries_vaapi_where_there_is_a_node_and_the_encoder_then_software() {
        let rate = FrameRate::FPS_30;
        // A file stands in for the render node: it is there.
        let there = node(std::path::Path::new(env!("CARGO_MANIFEST_PATH")));
        let gone = node(std::path::Path::new("/nonexistent/renderD128"));
        let all = |_: &str| true;
        let choose = |codec, choice, node: &RenderNode, has: &dyn Fn(&str) -> bool| {
            Encoders::choose(&settings(codec, choice), WIDE, WIDE.size(), rate, node, has)
        };

        let e = choose(Codec::Hevc, EncoderChoice::Auto, &there, &all).unwrap();
        assert_eq!(e.kinds, [EncoderKind::Vaapi, EncoderKind::Software]);
        assert_eq!((&e.skipped, e.first_name()), (&None, "hevc_vaapi"));
        assert_eq!(e.name(EncoderKind::Software), "libx265");

        // No node: software, saying why.
        let e = choose(Codec::Av1, EncoderChoice::Auto, &gone, &all).unwrap();
        assert_eq!(e.kinds, [EncoderKind::Software]);
        let f = e.skipped.unwrap();
        assert_eq!((f.from, f.code), ("av1_vaapi", "hw_unavailable"));
        assert!(
            f.reason.contains("no render node at /nonexistent"),
            "{}",
            f.reason
        );

        // No VA-API encoder in this FFmpeg.
        let no_vaapi = |n: &str| !n.ends_with("_vaapi");
        let e = choose(Codec::H264, EncoderChoice::Auto, &there, &no_vaapi).unwrap();
        assert_eq!(e.kinds, [EncoderKind::Software]);
        assert!(e.skipped.unwrap().reason.contains("no h264_vaapi"));

        // No software encoder (Fedora's own FFmpeg has no x264): VA-API
        // alone.
        let no_x264 = |n: &str| n != "libx264";
        let e = choose(Codec::H264, EncoderChoice::Auto, &there, &no_x264).unwrap();
        assert_eq!(e.kinds, [EncoderKind::Vaapi]);
        // Neither: refused, with both reasons.
        let e = choose(Codec::H264, EncoderChoice::Auto, &gone, &no_x264).unwrap_err();
        let ExportError::NoEncoder(why) = e else {
            panic!("{e:?}")
        };
        assert!(
            why.contains("no render node") && why.contains("no libx264"),
            "{why}"
        );
    }

    #[test]
    fn vaapi_and_software_are_the_one_asked_for_or_refused() {
        let rate = FrameRate::FPS_30;
        let there = node(std::path::Path::new(env!("CARGO_MANIFEST_PATH")));
        let gone = node(std::path::Path::new("/nonexistent/renderD128"));
        let all = |_: &str| true;
        let none = |_: &str| false;
        let choose = |choice, node: &RenderNode, has: &dyn Fn(&str) -> bool| {
            Encoders::choose(
                &settings(Codec::Hevc, choice),
                WIDE,
                WIDE.size(),
                rate,
                node,
                has,
            )
        };
        let e = choose(EncoderChoice::Vaapi, &there, &all).unwrap();
        assert_eq!((e.kinds, e.skipped), (vec![EncoderKind::Vaapi], None));
        let e = choose(EncoderChoice::Software, &gone, &all).unwrap();
        assert_eq!((e.kinds, e.skipped), (vec![EncoderKind::Software], None));
        for (choice, node, has, want) in [
            (
                EncoderChoice::Vaapi,
                &gone,
                &all as &dyn Fn(&str) -> bool,
                "no render node",
            ),
            (EncoderChoice::Vaapi, &there, &none, "no hevc_vaapi"),
            (EncoderChoice::Software, &there, &none, "no libx265"),
        ] {
            match choose(choice, node, has) {
                Err(ExportError::NoEncoder(why)) => assert!(why.contains(want), "{why}"),
                other => panic!("{choice:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn av1_goes_to_vaapi_only_at_sizes_it_needs_no_padding_for() {
        // odin's VCN 4 decoded a 1080 x 1080 AV1 export at 1088 x 1082.
        for (size, pads) in [
            ((1080, 1080), true),
            ((1920, 1080), true),
            ((1080, 1920), true),
            ((1080, 1350), true),
            ((2560, 1440), false),
            ((3840, 2160), false),
            ((3840, 1920), false),
            ((5760, 2880), false),
            ((7680, 3840), false),
            // 1920 x 1080's draft.
            ((960, 544), false),
        ] {
            assert_eq!(av1_vaapi_pads(size).is_some(), pads, "{size:?}");
        }
        let there = node(std::path::Path::new(env!("CARGO_MANIFEST_PATH")));
        let all = |_: &str| true;
        let choose = |codec, choice, size| {
            Encoders::choose(
                &settings(codec, choice),
                WIDE,
                size,
                FrameRate::FPS_30,
                &there,
                &all,
            )
        };
        // Auto: software, saying why.
        let e = choose(Codec::Av1, EncoderChoice::Auto, (1920, 1080)).unwrap();
        assert_eq!(e.kinds, [EncoderKind::Software]);
        let f = e.skipped.unwrap();
        assert_eq!((f.from, f.code), ("av1_vaapi", "hw_unsupported"));
        assert!(f.reason.contains("would pad 1920x1080"), "{}", f.reason);
        // Its draft needs none.
        let e = choose(Codec::Av1, EncoderChoice::Auto, (960, 544)).unwrap();
        assert_eq!(
            (e.kinds, e.skipped),
            (vec![EncoderKind::Vaapi, EncoderKind::Software], None)
        );
        // Asked for: refused as the hardware's (exit 3).
        match choose(Codec::Av1, EncoderChoice::Vaapi, (1080, 1080)) {
            Err(ExportError::Hardware {
                encoder: "av1_vaapi",
                code: "hw_unsupported",
                message,
            }) => assert!(message.contains("would pad 1080x1080"), "{message}"),
            other => panic!("{other:?}"),
        }
        // H.264 and HEVC crop theirs.
        for codec in [Codec::H264, Codec::Hevc] {
            let e = choose(codec, EncoderChoice::Vaapi, (1080, 1080)).unwrap();
            assert_eq!(e.kinds, [EncoderKind::Vaapi], "{codec:?}");
        }
    }

    #[test]
    fn progress_only_goes_forward_when_an_encoder_starts_over() {
        let s = Shared::new(30, "hevc_vaapi", None);
        s.muxed(5);
        let fallback = Fallback {
            from: "hevc_vaapi",
            code: "hw_failed",
            reason: "it failed".into(),
        };
        s.encoder("libx265", Some(fallback.clone()));
        s.muxed(1);
        let p = s.progress();
        assert_eq!((p.frames_done, p.encoder), (5, "libx265"));
        assert_eq!(p.fallback, Some(fallback));
        s.muxed(6);
        assert_eq!(s.progress().frames_done, 6);
        assert!(s.progress().fps >= 0.0);
        // The decode's first fallback is the one kept.
        let first = Fallback {
            from: "vaapi",
            code: "hw_unavailable",
            reason: "none".into(),
        };
        s.decode_fell_back(first.clone());
        s.decode_fell_back(Fallback {
            reason: "another".into(),
            ..first.clone()
        });
        assert_eq!(s.progress().decode_fallback, Some(first));
    }
}
