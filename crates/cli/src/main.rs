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
    /// cannot be read, is truncated or corrupt, or has no video or audio.
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SPHERECUT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Probe { files, json } => {
            if files.is_empty() {
                anyhow::bail!("give at least one file to probe");
            }
            let mut failed = false;
            let mut reports = Vec::new();
            for f in &files {
                match spherecut_media::probe::probe(f) {
                    Ok(r) => {
                        if !json {
                            print!("{}", show::probe(&r));
                        }
                        if let Some(first) = r.damage.first() {
                            failed = true;
                            let more = match r.damage.len() - 1 {
                                0 => String::new(),
                                1 => " (and 1 more problem)".into(),
                                n => format!(" (and {n} more problems)"),
                            };
                            eprintln!("error: {} is damaged: {first}{more}", f.display());
                        } else if !r
                            .streams
                            .iter()
                            .any(|s| matches!(s.kind, StreamKind::Video | StreamKind::Audio))
                        {
                            // FFmpeg opens text files as subtitles or lyrics.
                            failed = true;
                            eprintln!(
                                "error: {} has no video or audio stream; FFmpeg reads it as {}",
                                f.display(),
                                r.container.format_long
                            );
                        }
                        reports.push(serde_json::to_value(&r)?);
                    }
                    Err(e) => {
                        failed = true;
                        eprintln!("error: {e}");
                        reports.push(serde_json::json!({
                            "path": f,
                            "error": e.to_string(),
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
                println!("{}", serde_json::to_string_pretty(&v)?);
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
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print!("{}", show::doctor(&r));
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
