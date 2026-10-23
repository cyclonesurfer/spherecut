//! The `spherecut` command.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use spherecut_core::brand;

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
    /// and the detected camera for one or more files.
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
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Log warnings to stderr, but not FFmpeg's own messages: it logs an error
/// for every device doctor tries that isn't there, and our errors already
/// say what failed. `SPHERECUT_LOG` adds `EnvFilter` directives, e.g.
/// `debug`, `ffmpeg=debug` or `spherecut_media=trace`. FFmpeg's messages
/// show when it names the `ffmpeg` target or sets a level for everything.
fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::LevelFilter;
    const DEFAULT: &str = "warn,ffmpeg=off";
    let filter = match std::env::var("SPHERECUT_LOG") {
        Ok(v) if !v.trim().is_empty() => {
            // `ffmpeg=off` would outrank a global level (a directive naming
            // the target wins), so a global level replaces the default.
            // `ffmpeg=...` simply overrides it.
            let global = v
                .split(',')
                .any(|d| d.trim().parse::<LevelFilter>().is_ok());
            let base = if global { "warn" } else { DEFAULT };
            EnvFilter::try_new(format!("{base},{v}")).unwrap_or_else(|e| {
                eprintln!("warning: ignoring SPHERECUT_LOG={v}: {e}");
                EnvFilter::new(DEFAULT)
            })
        }
        _ => EnvFilter::new(DEFAULT),
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
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
