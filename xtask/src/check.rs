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
