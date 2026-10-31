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
use spherecut_render::Gpu;

use crate::encoder::EncoderSetup;
use crate::error::ExportError;
use crate::pipeline;
use crate::plan::FramePlan;
use crate::preset::Preset;

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
    /// The device to render on: the app's, whose submissions wait at its
    /// gate while the UI presents (D41), or a headless one.
    pub gpu: Gpu,
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
            gpu,
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
}

/// A finished export.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub output: PathBuf,
    pub frames: u64,
    /// `frames / rate`.
    pub duration: Time,
    pub size: (u32, u32),
    pub rate: FrameRate,
    pub codec: Codec,
    pub encoder: &'static str,
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
    pub warnings: Vec<String>,
    pub elapsed: Duration,
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
    encoder: &'static str,
    done: AtomicU64,
    /// When frames were muxed over the last two seconds, oldest first,
    /// with the one before them.
    window: Mutex<VecDeque<(Instant, u64)>>,
}

/// The span `Progress::fps` measures over.
const FPS_WINDOW: Duration = Duration::from_secs(2);

impl Shared {
    fn new(total: u64, encoder: &'static str) -> Shared {
        Shared {
            started: Instant::now(),
            total,
            encoder,
            done: AtomicU64::new(0),
            window: Mutex::new(VecDeque::new()),
        }
    }

    /// `done` frames are muxed.
    pub(crate) fn muxed(&self, done: u64) {
        let now = Instant::now();
        self.done.store(done, Ordering::Release);
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

    fn progress(&self) -> Progress {
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
        Progress {
            frames_done: done,
            frames_total: self.total,
            fps,
            elapsed: self.started.elapsed(),
            eta,
            encoder: self.encoder,
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
        if job.settings.encoder == EncoderChoice::Vaapi {
            return Err(ExportError::NoEncoder(
                "the VA-API encoders aren't in this build yet: export with the software ones"
                    .into(),
            ));
        }
        // The name doesn't depend on the bit depth, which the source sets.
        let s = &job.settings;
        let name = EncoderSetup::software(s.codec, s.quality, preset, sequence.rate, false).name;
        if !spherecut_media::encode::has_encoder(name) {
            return Err(ExportError::NoEncoder(format!(
                "this FFmpeg has no {name}, which exports {:?}",
                s.codec
            )));
        }
        if !job.overwrite && std::fs::symlink_metadata(&job.output).is_ok() {
            return Err(ExportError::OutputExists(job.output.clone()));
        }
        let shared = Arc::new(Shared::new(plan.count(), name));
        let stop = StopSource::new();
        let (tx, result) = mpsc::channel();
        let thread = {
            let (shared, stop) = (shared.clone(), stop.clone());
            std::thread::Builder::new()
                .name("export".into())
                .spawn(move || {
                    let r = pipeline::run(&job, preset, plan, &shared, &stop);
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

pub(crate) fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".into())
}
