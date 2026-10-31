//! `spherecut render`: a project's sequence exported to an MP4 (M2 plan
//! section 9, DECISIONS D51), through the export crate's one handle.
//!
//! The steps, in order:
//!
//! 1. the command line, which clap parses (exit 2);
//! 2. the output, before any work, as `frame` checks it: something there
//!    is refused unless `--overwrite` is given, and a directory, pipe or
//!    device either way;
//! 3. the project, loaded (exit 1 with its error's code), and its media:
//!    offline is exit 1, a file that changed is a warning;
//! 4. the flags' changes for this run, not saved: `--preset` and
//!    `--range` (`out_of_range` outside the sequence, exit 1) through the
//!    project's own edits, so its rules hold, and the export settings;
//! 5. the SIGINT handler and the GPU (exit 3 without one);
//! 6. `Export::start`, which refuses what needs no work (exit 1, or 3 for
//!    an encoder the run needs and hasn't);
//! 7. the `start` line, a `progress` line at most once a second, and the
//!    first Ctrl+C cancelling the export (exit 4, no file), as a panic on
//!    any thread does (exit 1);
//! 8. the result: the last line under `--json`, or a summary on stdout.
//!
//! Under `--json` every line on stdout is a whole D27 envelope whose
//! `result.event` names it, and the last is the run's document: the
//! result, or the error (D51). Without it, progress goes to stderr once a
//! second.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Args, ValueEnum};
use spherecut_agent::still::{Failure, about, failure, number, seconds};
use spherecut_cli::contract::{
    self, Envelope, Exit, FallbackInfo, Finishing, RenderAudio, RenderCheck, RenderDecode,
    RenderEvent, RenderProgress, RenderResult, RenderStages, RenderStart, Size, TimeRange, Warning,
    codes,
};
use spherecut_core::TimeSpec;
use spherecut_core::project::file::{self, FileError, MediaStatus};
use spherecut_core::project::{
    AudioChoice, Codec, Edit, EncoderChoice, Orientation, Output, Project, Quality,
};
use spherecut_core::time::Time;
use spherecut_export::{Decoder, Export, ExportError, ExportJob, Preset, Progress, Report};
use spherecut_render::{AdapterChoice, Gpu};

use crate::{DOCUMENT_STARTED, PANICKED, emit, frame, interrupt, print_json, to_stderr};

/// How often the main thread looks for a Ctrl+C while the export runs.
const POLL: Duration = Duration::from_millis(10);

/// The most often a progress line goes out (section 9).
const PROGRESS_EVERY: Duration = Duration::from_secs(1);

#[derive(Args, Debug, Clone)]
pub struct RenderArgs {
    /// The project file.
    pub project: PathBuf,
    /// The MP4 to write. Something already there is refused unless
    /// --overwrite is given.
    #[arg(short, long, value_name = "OUT.mp4")]
    pub output: PathBuf,
    /// The output's preset: flat-16x9-1080 and the rest of flat-WxH-SIDE
    /// (16x9, 9x16, 1x1 or 4x5 at 1080, 1440 or 2160), or 360-3840,
    /// 360-5760 or 360-7680 [default: the sequence's]
    #[arg(long, value_name = "NAME", value_parser = preset)]
    pub preset: Option<Preset>,
    /// [default: the sequence's]
    #[arg(long, value_enum)]
    pub codec: Option<CodecArg>,
    /// auto tries the GPU's VA-API encoder and goes on in software,
    /// saying so; vaapi fails instead (exit 3) [default: the sequence's]
    #[arg(long, value_enum)]
    pub encoder: Option<EncoderArg>,
    /// [default: the sequence's]
    #[arg(long, value_enum)]
    pub quality: Option<QualityArg>,
    /// The part of the sequence to export, in sequence time: IN..OUT,
    /// each as frame's --time takes it (12.5, 1:02.5, 370f), either left
    /// out for the sequence's start or end [default: all of it]
    #[arg(long, value_name = "IN..OUT", value_parser = range, allow_hyphen_values = true)]
    pub range: Option<RangeArg>,
    /// Half the size, rounded up to a multiple of 8, with the encoder's
    /// fastest settings: a quick look.
    #[arg(long)]
    pub draft: bool,
    /// [default: the sequence's]
    #[arg(long, value_enum)]
    pub audio: Option<AudioArg>,
    /// Blur each frame over its shutter [default: the sequence's choice]
    #[arg(long)]
    pub motion_blur: bool,
    /// Export 8-bit from a 10-bit source.
    #[arg(long)]
    pub eight_bit: bool,
    /// auto decodes on VA-API on the rendering GPU's node and goes on in
    /// software, as frame's auto does; software never tries.
    #[arg(long, value_enum, default_value_t = DecoderArg::Auto)]
    pub decoder: DecoderArg,
    /// The Vulkan adapter to render on: auto (a GPU, else lavapipe), cpu
    /// (lavapipe), or part of an adapter's name or its PCI bus id
    /// [default: $SPHERECUT_ADAPTER, else auto].
    #[arg(long, value_name = "auto|cpu|NAME")]
    pub adapter: Option<String>,
    /// Replace what is at the output, if it is a regular file, once the
    /// new file is whole and has passed its checks.
    #[arg(long)]
    pub overwrite: bool,
    /// Print one JSON envelope a line instead of text (docs/cli-json.md).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CodecArg {
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EncoderArg {
    Auto,
    Vaapi,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum QualityArg {
    High,
    Medium,
    Small,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AudioArg {
    Copy,
    Aac,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DecoderArg {
    Auto,
    Software,
}

/// `--range`: either end may be left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeArg {
    pub start: Option<TimeSpec>,
    pub end: Option<TimeSpec>,
}

fn preset(s: &str) -> Result<Preset, String> {
    Preset::parse(s).ok_or_else(|| {
        let names: Vec<String> = Preset::all().iter().map(Preset::to_string).collect();
        format!("expected one of {}", names.join(", "))
    })
}

fn range(s: &str) -> Result<RangeArg, String> {
    let (a, b) = s
        .split_once("..")
        .ok_or_else(|| "expected IN..OUT, such as 2..7.5, 1:00..90f, 5.. or ..10".to_string())?;
    let side = |v: &str| match v {
        "" => Ok(None),
        v => v
            .parse::<TimeSpec>()
            .map(Some)
            .map_err(|e: spherecut_core::TimeSpecError| e.to_string()),
    };
    Ok(RangeArg {
        start: side(a)?,
        end: side(b)?,
    })
}

fn codec_name(c: Codec) -> &'static str {
    match c {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    }
}

fn quality_name(q: Quality) -> &'static str {
    match q {
        Quality::High => "high",
        Quality::Medium => "medium",
        Quality::Small => "small",
    }
}

fn audio_name(a: AudioChoice) -> &'static str {
    match a {
        AudioChoice::Copy => "copy",
        AudioChoice::Aac => "aac",
        AudioChoice::None => "none",
    }
}

/// Run `spherecut render`, and print its lines.
pub fn run(args: RenderArgs) -> Result<Exit> {
    let json = args.json;
    let mut warnings = Vec::new();
    let outcome = render(&args, &mut warnings);
    match outcome {
        Ok(result) => {
            if json {
                let event = RenderEvent::Result(result);
                let envelope =
                    Envelope::new(contract::RENDER, Some(event), None).with_warnings(warnings);
                print_json(&envelope, Exit::Ok)
            } else {
                emit(&format!("{}\n", summary(&result)))?;
                Ok(Exit::Ok)
            }
        }
        Err(e) => {
            let exit = contract::frame_exit(e.code);
            if json {
                let envelope = Envelope::<RenderEvent>::new(contract::RENDER, None, Some(e))
                    .with_warnings(warnings);
                print_json(&envelope, exit)
            } else {
                to_stderr(&format!("error: {}\n", e.message));
                Ok(exit)
            }
        }
    }
}

/// Steps 2 to 7: the finished file's result, or why there is none.
fn render(args: &RenderArgs, warnings: &mut Vec<Warning>) -> Result<RenderResult, Failure> {
    // Step 2.
    frame::check_output(&args.output, args.overwrite)?;

    // Step 3.
    let mut project = file::load(&args.project).map_err(|e| project_error(&args.project, &e))?;
    media(&project, warnings)?;

    // Step 4.
    let range = changes(&mut project, args)?;
    let sequence = project.the_sequence();
    let preset = Preset::of(&sequence.output).ok_or_else(|| {
        let (w, h) = sequence.output.size();
        failure(
            codes::INVALID_PROJECT,
            format!("the sequence's {w}x{h} isn't one of the export presets; give --preset"),
        )
    })?;
    let mut settings = sequence.export;
    if let Some(c) = args.codec {
        settings.codec = match c {
            CodecArg::H264 => Codec::H264,
            CodecArg::Hevc => Codec::Hevc,
            CodecArg::Av1 => Codec::Av1,
        };
    }
    if let Some(e) = args.encoder {
        settings.encoder = match e {
            EncoderArg::Auto => EncoderChoice::Auto,
            EncoderArg::Vaapi => EncoderChoice::Vaapi,
            EncoderArg::Software => EncoderChoice::Software,
        };
    }
    if let Some(q) = args.quality {
        settings.quality = match q {
            QualityArg::High => Quality::High,
            QualityArg::Medium => Quality::Medium,
            QualityArg::Small => Quality::Small,
        };
    }
    if let Some(a) = args.audio {
        settings.audio = match a {
            AudioArg::Copy => AudioChoice::Copy,
            AudioArg::Aac => AudioChoice::Aac,
            AudioArg::None => AudioChoice::None,
        };
    }
    settings.motion_blur |= args.motion_blur;
    let rate = sequence.rate;

    // Step 5. Before the export's temporary files, so that the first
    // Ctrl+C never leaves one behind.
    interrupt::install().map_err(|e| {
        failure(
            codes::INTERNAL,
            format!("cannot install the SIGINT handler: {e}"),
        )
    })?;
    let adapter = match &args.adapter {
        Some(a) => AdapterChoice::parse(a),
        None => AdapterChoice::from_env().unwrap_or(AdapterChoice::Auto),
    };
    let gpu = Gpu::headless(&adapter).map_err(spherecut_agent::still::render_error)?;

    // Step 6.
    let job = ExportJob {
        output: args.output.clone(),
        overwrite: args.overwrite,
        settings,
        eight_bit: args.eight_bit,
        decoder: match args.decoder {
            DecoderArg::Auto => Decoder::Auto,
            DecoderArg::Software => Decoder::Software,
        },
        draft: args.draft,
        ..ExportJob::new(project, &args.output, gpu)
    };
    let mut export = Export::start(job).map_err(|e| export_error(&args.output, &e))?;

    // Step 7.
    let first = export.progress();
    let size = if args.draft {
        preset.draft_size()
    } else {
        preset.size()
    };
    let start = RenderStart {
        output: text(&args.output),
        project: text(&args.project),
        preset: preset.to_string(),
        size: Size {
            width: size.0,
            height: size.1,
        },
        draft: args.draft,
        rate,
        range,
        frames: first.frames_total,
        duration: i64::try_from(first.frames_total)
            .ok()
            .and_then(|n| rate.frame_to_time(n).ok())
            .unwrap_or(Time::ZERO),
        codec: codec_name(settings.codec),
        quality: quality_name(settings.quality),
        encoder: first.encoder,
        fallback: first.fallback.as_ref().map(FallbackInfo::from),
        audio: audio_name(settings.audio),
        motion_blur: settings.motion_blur,
    };
    if args.json {
        line(RenderEvent::Start(start));
    } else {
        to_stderr(&format!(
            "rendering {} frames at {}×{} in {} to {}\n",
            first.frames_total,
            size.0,
            size.1,
            first.encoder,
            args.output.display()
        ));
    }
    let mut last = Instant::now();
    let mut stopping = false;
    let report = loop {
        if let Some(done) = export.wait(POLL) {
            break done;
        }
        if interrupt::interrupted() && !stopping {
            tracing::info!("interrupted: stopping the export");
            export.cancel();
            stopping = true;
        }
        // A panic's document has gone out, and the run has failed: the
        // file it would write mustn't be left behind.
        if PANICKED.load(Ordering::SeqCst) && !stopping {
            tracing::info!("a panic: stopping the export");
            export.cancel();
            stopping = true;
        }
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            let p = export.progress();
            if args.json {
                line(RenderEvent::Progress(progress(&p)));
            } else {
                to_stderr(&format!("{}\n", progress_text(&p)));
            }
        }
    };
    let report = report.map_err(|e| match e {
        // Whatever stopped the export, the interrupt did.
        _ if interrupt::interrupted() => cancelled(),
        e => export_error(&args.output, &e),
    })?;
    warnings.extend(
        report
            .warnings
            .iter()
            .map(|w| Warning::new(w.code, w.message.clone())),
    );
    for w in &report.warnings {
        tracing::warn!("{w}");
    }
    Ok(result(&args.output, &report))
}

/// A `start` or `progress` line. The result's line is the run's
/// document, which [`print_json`] writes; these go out before it, and a
/// reader that has gone away isn't an error (the export goes on, as
/// `frame` does with a closed stdout).
fn line(event: RenderEvent) {
    // A panic's document has gone out: it is the last line.
    if DOCUMENT_STARTED.load(Ordering::SeqCst) {
        return;
    }
    let envelope = Envelope::new(contract::RENDER, Some(event), None);
    match envelope.to_line() {
        Ok(l) => {
            if let Err(e) = emit(&l) {
                tracing::debug!("a progress line wasn't written: {e:#}");
            }
        }
        Err(e) => tracing::debug!("a progress line wasn't made: {e}"),
    }
}

/// The text of `path` for the JSON: bytes that aren't UTF-8 are U+FFFD.
fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn cancelled() -> Failure {
    failure(codes::CANCELLED, "interrupted: no file written")
}

/// Step 3's error: the file's own code, D27's `io_error` for reading it.
fn project_error(path: &Path, e: &FileError) -> Failure {
    let code = match e.code() {
        "not_found" => "not_found",
        "io" => "io_error",
        "too_large" => "too_large",
        "not_project" => codes::NOT_PROJECT,
        "newer_schema" => codes::NEWER_SCHEMA,
        "malformed" => codes::MALFORMED_PROJECT,
        _ => codes::INVALID_PROJECT,
    };
    // Io and TooLarge already name the file.
    let message = match e {
        FileError::Io { .. } | FileError::TooLarge { .. } => e.to_string(),
        e => format!("{}: {e}", path.display()),
    };
    about(failure(code, message), path)
}

/// Step 3's look at the clip's media: an offline clip fails, a changed
/// file is a warning (M2 plan section 5.3).
fn media(project: &Project, warnings: &mut Vec<Warning>) -> Result<(), Failure> {
    let clip = project.the_clip();
    let status = file::media_status(project);
    let Some((m, status)) = project
        .media()
        .iter()
        .zip(status)
        .find(|(m, _)| m.id == clip.media)
    else {
        return Err(failure(
            codes::INVALID_PROJECT,
            format!("the clip's media {} isn't in the project", clip.media.0),
        ));
    };
    match status {
        MediaStatus::Present => Ok(()),
        MediaStatus::Missing => Err(about(
            failure(
                codes::MEDIA_OFFLINE,
                format!(
                    "the clip is offline: {} isn't there (relinking is M5's)",
                    m.path.display()
                ),
            ),
            &m.path,
        )),
        MediaStatus::Changed(why) => {
            let message = format!(
                "{} isn't the file the project added ({why}); exporting it anyway",
                m.path.display()
            );
            tracing::warn!("{message}");
            warnings.push(Warning::new(codes::warnings::MEDIA_CHANGED, message));
            Ok(())
        }
    }
}

/// Step 4's changes to the project, made with its own edits so that its
/// rules hold: `--preset`, and `--range` as a trim within the trim. The
/// part of the sequence exported.
fn changes(project: &mut Project, args: &RenderArgs) -> Result<TimeRange, Failure> {
    let sequence = project.the_sequence();
    let clip = project.the_clip();
    let mut edits = Vec::new();
    if let Some(p) = args.preset {
        let output = match p {
            Preset::Flat { aspect, short_side } => Output::Flat { aspect, short_side },
            Preset::Sphere { width } => Output::Equirect {
                width,
                // A sphere keeps the sequence's turn.
                orientation: match sequence.output {
                    Output::Equirect { orientation, .. } => orientation,
                    Output::Flat { .. } => Orientation::IDENTITY,
                },
            },
        };
        edits.push(Edit::SetOutput {
            sequence: sequence.id,
            output,
        });
    }
    let length = clip.duration();
    let mut range = TimeRange {
        start: Time::ZERO,
        end: length,
    };
    if let Some(r) = args.range {
        let at = |t: TimeSpec| -> Result<Time, Failure> {
            match t {
                TimeSpec::Time(t) => Ok(t),
                TimeSpec::Frame(n) => i64::try_from(n)
                    .ok()
                    .and_then(|n| sequence.rate.frame_to_time(n).ok())
                    .ok_or_else(|| out_of_range(format!("--range: frame {n} is past the end"))),
            }
        };
        if let Some(t) = r.start {
            range.start = at(t)?;
        }
        if let Some(t) = r.end {
            range.end = at(t)?;
        }
        let shown = |t: Time| format!("{} s", seconds(t));
        if range.end > length {
            return Err(out_of_range(format!(
                "--range ends at {}, past the sequence's end at {}",
                shown(range.end),
                shown(length)
            )));
        }
        if range.start >= range.end {
            return Err(out_of_range(format!(
                "--range from {} to {} has no frames",
                shown(range.start),
                shown(range.end)
            )));
        }
        let start = clip.source.start;
        let trim = |t: Time| {
            start
                .checked_add(t)
                .map_err(|e| out_of_range(format!("--range: {e}")))
        };
        edits.push(Edit::SetTrim {
            clip: clip.id,
            start: Some(trim(range.start)?),
            end: Some(trim(range.end)?),
        });
    }
    if !edits.is_empty() {
        project.apply(&edits).map_err(|e| {
            failure(
                codes::INVALID_PROJECT,
                format!("the flags' changes break a rule of the project: {e}"),
            )
        })?;
    }
    Ok(range)
}

fn out_of_range(message: String) -> Failure {
    failure(codes::OUT_OF_RANGE, message)
}

/// An export's error, with its code (D51).
fn export_error(output: &Path, e: &ExportError) -> Failure {
    let f = failure(e.code(), e.to_string());
    match e {
        ExportError::OutputExists(_) => about(
            failure(
                codes::OUTPUT_EXISTS,
                format!(
                    "{} exists; pass --overwrite to replace it",
                    output.display()
                ),
            ),
            output,
        ),
        ExportError::Cancelled => cancelled(),
        ExportError::NoSpace { folder, .. } => about(f, folder),
        ExportError::Failed { .. } if e.code() == "io_error" => about(f, output),
        _ => f,
    }
}

fn progress(p: &Progress) -> RenderProgress {
    RenderProgress {
        frames_done: p.frames_done,
        frames_total: p.frames_total,
        fps: p.fps,
        elapsed: p.elapsed.as_secs_f64(),
        eta: p.eta.map(|d| d.as_secs_f64()),
        encoder: p.encoder,
        fallback: p.fallback.as_ref().map(FallbackInfo::from),
        decode_fallback: p.decode_fallback.as_ref().map(FallbackInfo::from),
        finishing: p.finishing.map(|(bytes_done, bytes_total)| Finishing {
            bytes_done,
            bytes_total,
        }),
    }
}

/// The line a second on stderr: "120 of 600 frames, 45.2 fps, 10.6 s
/// left (hevc_vaapi)".
fn progress_text(p: &Progress) -> String {
    if let Some((done, total)) = p.finishing {
        return format!(
            "finishing the file: {} of {} MB",
            done / 1_000_000,
            total / 1_000_000
        );
    }
    let left = match p.eta {
        Some(eta) => format!(", {} s left", number(eta.as_secs_f64().round())),
        None => String::new(),
    };
    format!(
        "{} of {} frames, {:.1} fps{left} ({})",
        p.frames_done, p.frames_total, p.fps, p.encoder
    )
}

fn result(output: &Path, r: &Report) -> RenderResult {
    RenderResult {
        output: text(output),
        frames: r.frames,
        duration: r.duration,
        size: Size {
            width: r.size.0,
            height: r.size.1,
        },
        draft: r.draft,
        rate: r.rate,
        codec: codec_name(r.codec),
        encoder: r.encoder,
        fallback: r.fallback.as_ref().map(FallbackInfo::from),
        decode: RenderDecode {
            path: r.decode.path,
            fallback: r.decode.fallback.as_ref().map(FallbackInfo::from),
        },
        ten_bit: r.ten_bit,
        audio: r.audio.as_ref().map(|a| RenderAudio {
            stream: a.stream,
            encoder: a.encoder,
            rate: a.rate,
            channels: a.channels,
            ambix: a.ambix,
            samples: a.samples,
        }),
        bytes: r.bytes,
        blur_capped: r.blur_capped,
        checks: r
            .checks
            .iter()
            .map(|c| RenderCheck {
                name: c.name,
                found: c.found.clone(),
            })
            .collect(),
        stages: RenderStages {
            decode: r.stages.decode.as_secs_f64(),
            render: r.stages.render.as_secs_f64(),
            readback: r.stages.readback.as_secs_f64(),
            encode: r.stages.encode.as_secs_f64(),
            finish: r.stages.finish.as_secs_f64(),
        },
        elapsed: r.elapsed.as_secs_f64(),
    }
}

/// The summary for people: "wrote out.mp4: 600 frames (20 s) at
/// 3840×1920, HEVC 10-bit in hevc_vaapi, AAC audio, 45.1 MB, 14 checks
/// passed", and what fell back.
fn summary(r: &RenderResult) -> String {
    let codec = match r.codec {
        "h264" => "H.264",
        "hevc" => "HEVC",
        _ => "AV1",
    };
    let bits = if r.ten_bit { "10-bit" } else { "8-bit" };
    let audio = match &r.audio {
        Some(a) if a.ambix => format!("{} ambiX audio", a.encoder),
        Some(a) => format!("{} audio", a.encoder),
        None => "no audio".into(),
    };
    let draft = if r.draft { " draft" } else { "" };
    let mut s = format!(
        "wrote {}: {} frames ({} s) at {}×{}{draft}, {codec} {bits} in {}, {audio}, {:.1} MB, \
         {} checks passed",
        r.output,
        r.frames,
        seconds(r.duration),
        r.size.width,
        r.size.height,
        r.encoder,
        r.bytes as f64 / 1e6,
        r.checks.len()
    );
    if let Some(f) = &r.fallback {
        s.push_str(&format!("; {} wasn't used ({})", f.from, f.reason));
    }
    if let Some(f) = &r.decode.fallback {
        s.push_str(&format!("; decoded in software ({}: {})", f.from, f.reason));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_takes_times_or_frames_with_either_end_left_out() {
        let t = |s: &str| s.parse::<TimeSpec>().unwrap();
        assert_eq!(
            range("2..7.5"),
            Ok(RangeArg {
                start: Some(t("2")),
                end: Some(t("7.5"))
            })
        );
        assert_eq!(
            range("1:02.5..90f"),
            Ok(RangeArg {
                start: Some(t("1:02.5")),
                end: Some(t("90f"))
            })
        );
        assert_eq!(
            range("5.."),
            Ok(RangeArg {
                start: Some(t("5")),
                end: None
            })
        );
        assert_eq!(
            range("..10"),
            Ok(RangeArg {
                start: None,
                end: Some(t("10"))
            })
        );
        assert!(range("5").unwrap_err().contains("IN..OUT"));
        assert!(range("-1..5").is_err());
        assert!(range("a..5").is_err());
    }

    #[test]
    fn a_preset_is_named_as_the_table_names_it() {
        assert_eq!(preset("360-5760"), Ok(Preset::Sphere { width: 5760 }));
        let e = preset("flat-16x9-720").unwrap_err();
        assert!(
            e.contains("flat-16x9-1080") && e.contains("360-7680"),
            "{e}"
        );
    }
}
