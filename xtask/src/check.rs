//! `cargo xtask check`: everything that must pass before a commit is pushed.

use std::process::Command;

use anyhow::{Result, bail};

use crate::{have_cargo_subcommand, run_step};

#[derive(clap::Args)]
pub struct Args {
    /// Rewrite files with rustfmt instead of only checking them.
    #[arg(long)]
    fix: bool,
    /// Skip cargo-deny and cargo-audit (they need the advisory database).
    #[arg(long)]
    no_supply_chain: bool,
}

pub fn run(a: Args) -> Result<()> {
    for (sub, pkg) in [("fmt", "rustfmt"), ("clippy", "clippy")] {
        if !have_cargo_subcommand(sub) {
            bail!(
                "`cargo {sub}` is not installed: run `rustup component add {pkg}` \
                 (or, with a distro toolchain, install the `{pkg}` package)"
            );
        }
    }
    // The Tier 1 tests run probe and doctor under strace to check that
    // they open no network socket (D24), and fail without it.
    let strace = Command::new("strace")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success());
    if !strace {
        bail!(
            "strace is not installed, and the Tier 1 tests need it: \
             sudo dnf install strace (Fedora), sudo pacman -S strace (Arch) \
             or sudo apt install strace (Ubuntu)"
        );
    }
    // cpal's ALSA host, always built on Linux, links libasound (D35):
    // without its headers the build fails in alsa-sys.
    let alsa = Command::new("pkg-config")
        .args(["--exists", "alsa"])
        .status()
        .is_ok_and(|s| s.success());
    if !alsa {
        bail!(
            "the ALSA development headers are not installed (pkg-config finds no `alsa`), \
             and the audio output links libasound: \
             sudo dnf install alsa-lib-devel (Fedora), sudo pacman -S alsa-lib (Arch) \
             or sudo apt install libasound2-dev (Ubuntu)"
        );
    }
    let mut fmt = Command::new("cargo");
    fmt.args(["fmt", "--all"]);
    if !a.fix {
        fmt.arg("--check");
    }
    run_step("rustfmt", &mut fmt)?;
    run_step(
        "clippy",
        Command::new("cargo").args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ]),
    )?;
    // The FFmpeg bindings differ between releases (D2).
    crate::compat::from_check()?;
    run_step(
        "tests (Tier 1)",
        Command::new("cargo").args(["test", "--workspace", "--all-targets"]),
    )?;
    run_step(
        "doc tests",
        Command::new("cargo").args(["test", "--workspace", "--doc"]),
    )?;
    if a.no_supply_chain {
        eprintln!("==> skipping cargo-deny and cargo-audit (--no-supply-chain)");
        return Ok(());
    }
    for tool in ["deny", "audit"] {
        if !have_cargo_subcommand(tool) {
            bail!("cargo-{tool} is not installed: run `cargo install --locked cargo-{tool}`");
        }
    }
    run_step("cargo-deny", Command::new("cargo").args(["deny", "check"]))?;
    run_step(
        "cargo-audit",
        Command::new("cargo").args(["audit", "--deny", "warnings"]),
    )?;
    eprintln!("==> all checks passed");
    Ok(())
}
