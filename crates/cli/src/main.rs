//! The `spherecut` command.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use spherecut_core::brand;
use spherecut_media::probe::StreamKind;

mod show;

#[derive(Parser)]
#[command(name = brand::APP_SLUG, version = brand::VERSION, about = "Native Linux 360° video editor")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show container, streams, codecs, color, timing, spherical metadata
    /// and the detected camera for one or more files. Fails if any file
    /// cannot be read, is not MP4/MOV or Matroska/WebM, is truncated or
    /// corrupt, or has no video or audio.
    Probe {
        files: Vec<PathBuf>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Check codecs and hardware decode/encode support, and print the
    /// command that fixes anything missing.
    Doctor {
        /// DRM render node to test for VA-API.
        #[arg(long, default_value = "/dev/dri/renderD128")]
        render_node: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    init_logging();
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            to_stderr(&format!("error: {e:#}\n"));
            ExitCode::FAILURE
        }
    }
}

/// What the CLI logs by default: warnings, but not FFmpeg's own messages.
/// FFmpeg logs an error for every device doctor tries that isn't there,
/// and our errors already say what failed.
const DEFAULT_LOG: &str = "warn,ffmpeg=off";

/// Log to stderr as [`log_directives`] says for `SPHERECUT_LOG`.
fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    let filter = match std::env::var("SPHERECUT_LOG") {
        Ok(v) if !v.trim().is_empty() => {
            EnvFilter::try_new(log_directives(&v)).unwrap_or_else(|e| {
                to_stderr(&format!("warning: ignoring SPHERECUT_LOG={v}: {e}\n"));
                EnvFilter::new(DEFAULT_LOG)
            })
        }
        _ => EnvFilter::new(DEFAULT_LOG),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        // It would report a failed write with eprintln!, which panics on
        // a closed stderr; see to_stderr.
        .log_internal_errors(false)
        .init();
}

/// The `EnvFilter` directives for a `SPHERECUT_LOG` value, e.g. `debug`,
/// `ffmpeg=debug` or `spherecut_media=trace`, which add to the default.
/// FFmpeg's messages show when the value names the `ffmpeg` target, or
/// sets a level for everything that is more verbose than the default's
/// (`info`, `debug`, `trace`); `warn` and `error` leave them off.
fn log_directives(v: &str) -> String {
    use tracing_subscriber::filter::LevelFilter;
    // EnvFilter splits on commas too, but takes spaces as part of a
    // directive. An empty directive would parse as a level (ERROR).
    let directives: Vec<&str> = v
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .collect();
    // The last global level wins in EnvFilter. `ffmpeg=off` would outrank
    // it (a directive naming the target wins), so a more verbose one
    // replaces the default. A directive naming `ffmpeg` comes later and
    // simply overrides it.
    let global = directives
        .iter()
        .rev()
        .find_map(|d| d.parse::<LevelFilter>().ok());
    let base = match global {
        Some(level) if level > LevelFilter::WARN => "warn",
        _ => DEFAULT_LOG,
    };
    std::iter::once(base)
        .chain(directives)
        .collect::<Vec<_>>()
        .join(",")
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Probe { files, json } => {
            if files.is_empty() {
                anyhow::bail!("give at least one file to probe");
            }
            let mut failed = false;
            let mut reports = Vec::new();
            // Cleared when the reader goes away; the rest are still probed
            // for the exit code and the errors on stderr.
            let mut reading = true;
            for f in &files {
                match spherecut_media::probe::probe(f) {
                    Ok(r) => {
                        if !json && reading {
                            reading = emit(&show::probe(&r))?;
                        }
                        if let Some(first) = r.damage.first() {
                            failed = true;
                            let more = match r.damage.len() - 1 {
                                0 => String::new(),
                                1 => " (and 1 more problem)".into(),
                                n => format!(" (and {n} more problems)"),
                            };
                            to_stderr(&format!(
                                "error: {} is damaged: {first}{more}\n",
                                f.display()
                            ));
                        } else if !r
                            .streams
                            .iter()
                            .any(|s| matches!(s.kind, StreamKind::Video | StreamKind::Audio))
                        {
                            // An MP4 or Matroska file can hold only
                            // subtitles or data.
                            failed = true;
                            to_stderr(&format!(
                                "error: {} has no video or audio stream; FFmpeg reads it as {}\n",
                                f.display(),
                                r.container.format_long
                            ));
                        }
                        reports.push(serde_json::to_value(&r)?);
                    }
                    Err(e) => {
                        failed = true;
                        // FFmpeg's own word on why it could not open the
                        // file, under ours.
                        if json {
                            to_stderr(&format!("error: {e}\n"));
                        } else {
                            to_stderr(&format!(
                                "error: {e}\n{}",
                                show::ffmpeg_lines(&e.ffmpeg_log)
                            ));
                        }
                        // Lossy, as in the reports: Linux file names need
                        // not be UTF-8.
                        reports.push(serde_json::json!({
                            "path": f.to_string_lossy(),
                            "error": e.to_string(),
                            "ffmpeg_log": e.ffmpeg_log,
                        }));
                    }
                }
            }
            if json {
                let v = if reports.len() == 1 {
                    reports.remove(0)
                } else {
                    serde_json::Value::Array(reports)
                };
                emit(&format!("{}\n", serde_json::to_string_pretty(&v)?))?;
            }
            Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Cmd::Doctor { render_node, json } => {
            let r = spherecut_media::doctor::run(&render_node);
            if json {
                emit(&format!("{}\n", serde_json::to_string_pretty(&r)?))?;
            } else {
                emit(&show::doctor(&r))?;
            }
            let errors = r
                .issues
                .iter()
                .any(|i| i.severity == spherecut_media::doctor::Severity::Error);
            Ok(if errors {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

/// Write `s` to stderr. Errors are ignored: with nowhere to report them,
/// a closed stderr (`spherecut probe *.mp4 2>&1 | head`) must not turn the
/// exit code into a panic's.
fn to_stderr(s: &str) {
    use std::io::Write;
    let _ = std::io::stderr().lock().write_all(s.as_bytes());
}

/// Write `s` to stdout. Returns false when the reader has gone away, as in
/// `spherecut probe *.mp4 | head`, which is not an error.
fn emit(s: &str) -> Result<bool> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(s.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(anyhow::Error::new(e).context("cannot write to stdout")),
    }
}
