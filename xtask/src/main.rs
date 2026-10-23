//! Developer tasks. Run with `cargo xtask <command>`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

mod check;
mod gen_media;
mod hw_test;

#[derive(Parser)]
#[command(about = "Spherecut developer tasks")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// fmt, clippy (-D warnings), Tier 1 tests, cargo-deny and cargo-audit.
    Check(check::Args),
    /// Write the synthetic test-media set to test-media/.
    GenMedia(gen_media::Args),
    /// Tier 2 run on real hardware; writes hw-reports/<host>-<commit>.md.
    HwTest(hw_test::Args),
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Check(a) => check::run(a),
        Cmd::GenMedia(a) => gen_media::run(a),
        Cmd::HwTest(a) => hw_test::run(a),
    }
}

/// The workspace root (the parent of this crate's directory).
pub fn workspace_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap_or(here).to_path_buf()
}

/// Run a command in the workspace root, echoing it, and fail if it fails.
pub fn run_step(name: &str, cmd: &mut Command) -> Result<()> {
    eprintln!("==> {name}");
    let status = cmd
        .current_dir(workspace_root())
        .status()
        .with_context(|| format!("could not start {name}"))?;
    if !status.success() {
        bail!("{name} failed ({status})");
    }
    Ok(())
}

/// Whether a cargo subcommand such as `deny` is installed.
pub fn have_cargo_subcommand(sub: &str) -> bool {
    Command::new("cargo")
        .args([sub, "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
