//! `cargo xtask check`: everything that must pass before a commit is pushed.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

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
    // The network lints must fail a crate that reaches the network (D32).
    crate::controls::clippy_controls(&crate::workspace_root())?;
    // Every crate's docs, with their intra-doc links and HTML, and a
    // warning an error even under the caller's `-A warnings` (D8).
    let vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    run_step("rustdoc", &mut rustdoc_command(None, rustdocflags(&vars)))?;
    crate::controls::rustdoc_controls()?;
    msrv(&crate::workspace_root())?;
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
    // And the network bans a workspace that depends on it (D32).
    crate::controls::deny_controls(&crate::workspace_root())?;
    run_step(
        "cargo-audit",
        Command::new("cargo").args(["audit", "--deny", "warnings"]),
    )?;
    eprintln!("==> all checks passed");
    Ok(())
}

/// `cargo doc` of every crate in the workspace, or in the one whose
/// manifest is `manifest`, without dependencies, with all features and
/// past a crate that fails, rustdoc taking `flags` (cargo's encoded form,
/// `CARGO_ENCODED_RUSTDOCFLAGS`, with `RUSTDOCFLAGS` removed so that
/// cargo reads nothing else). Cargo documents a bin's private items too.
pub(crate) fn rustdoc_command(manifest: Option<&Path>, flags: OsString) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("doc");
    if let Some(manifest) = manifest {
        cmd.arg("--manifest-path").arg(manifest);
    }
    cmd.args(["--workspace", "--no-deps", "--all-features", "--keep-going"])
        .env("CARGO_ENCODED_RUSTDOCFLAGS", flags)
        .env_remove("RUSTDOCFLAGS");
    cmd
}

/// Rustdoc's flags for the check, in cargo's encoded form (separated by
/// 0x1f): the caller's own (`CARGO_ENCODED_RUSTDOCFLAGS` as given, else
/// `RUSTDOCFLAGS` split on spaces as cargo splits it), then `-D warnings`
/// last. A later lint level wins, so the caller's `-A warnings` can't
/// turn the gate off; a lint the caller allows by name, or
/// `--cap-lints`, still can.
pub(crate) fn rustdocflags(vars: &[(OsString, OsString)]) -> OsString {
    let get = |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let mut flags: Vec<OsString> = Vec::new();
    match (get("CARGO_ENCODED_RUSTDOCFLAGS"), get("RUSTDOCFLAGS")) {
        (Some(encoded), _) => {
            if !encoded.is_empty() {
                flags.push(encoded);
            }
        }
        (None, Some(plain)) => flags.extend(
            plain
                .to_string_lossy()
                .split(' ')
                .filter(|f| !f.trim().is_empty())
                .map(|f| OsString::from(f.trim())),
        ),
        (None, None) => {}
    }
    flags.push("-D".into());
    flags.push("warnings".into());
    let mut out = OsString::new();
    for (i, f) in flags.iter().enumerate() {
        if i > 0 {
            out.push("\x1f");
        }
        out.push(f);
    }
    out
}

/// The workspace's `rust-version`, from the `[workspace.package]` table
/// of the root manifest's text.
pub(crate) fn workspace_rust_version(manifest: &str) -> Option<String> {
    let mut table = "";
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            table = line;
            continue;
        }
        if table != "[workspace.package]" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "rust-version" {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

/// `cargo check` of every target and feature in the workspace, or in the
/// one whose manifest is `manifest`, with the toolchain rustup knows as
/// `version`, into `target`. `RUSTC`, `RUSTDOC` and
/// `RUSTC_WORKSPACE_WRAPPER` are removed: an inherited `RUSTC` would build
/// with another compiler and check nothing.
pub(crate) fn msrv_command(version: &str, manifest: Option<&Path>, target: &Path) -> Command {
    let mut cmd = Command::new("rustup");
    cmd.args(["run", version, "cargo", "check"]);
    if let Some(manifest) = manifest {
        cmd.arg("--manifest-path").arg(manifest);
    }
    cmd.args(["--workspace", "--all-targets", "--all-features"])
        .env("CARGO_TARGET_DIR", target)
        .env_remove("RUSTC")
        .env_remove("RUSTDOC")
        .env_remove("RUSTC_WORKSPACE_WRAPPER");
    cmd
}

/// The MSRV build (D1): the workspace checked with the toolchain its
/// `rust-version` names, in `msrv/` in cargo's target directory, after
/// its controls. Without that toolchain it is skipped with a line saying
/// how to install it, as compat is skipped without its prefixes, except
/// under CI (`CI` set), where it fails.
fn msrv(root: &Path) -> Result<()> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))
        .with_context(|| format!("reading {}", root.join("Cargo.toml").display()))?;
    let version = workspace_rust_version(&manifest)
        .context("the workspace's Cargo.toml has no rust-version in [workspace.package]")?;
    let want = crate::machine::rustc_version(&format!("rustc {version}"))
        .with_context(|| format!("the workspace's rust-version {version:?} isn't a version"))?;
    let have = Command::new("rustup")
        .args(["run", &version, "rustc", "--version"])
        .output();
    let missing = match have {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            if crate::machine::rustc_version(&text) != Some(want) {
                bail!(
                    "rustup's {version} toolchain says it is {}, not {version}",
                    text.trim()
                );
            }
            None
        }
        Ok(_) => Some(format!(
            "rustup has no {version} toolchain \
             (rustup toolchain install {version} --profile minimal)"
        )),
        Err(_) => Some(format!(
            "rustup isn't installed, and the build runs Rust {version} through it"
        )),
    };
    if let Some(why) = missing {
        if std::env::var_os("CI").is_some_and(|v| !v.is_empty()) {
            bail!("the MSRV build must run under CI, but {why}");
        }
        eprintln!("==> skipping the MSRV build: {why}");
        return Ok(());
    }
    crate::controls::msrv_controls(&version)?;
    let target = crate::hw_test::target_dir(root).join("msrv");
    run_step(
        &format!("MSRV build (Rust {version})"),
        &mut msrv_command(&version, None, &target),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use spherecut_testkit::control::must_fail;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    /// Whether `flags`, in cargo's encoded form, end in `-D warnings`.
    fn ends_in_deny_warnings(flags: &OsString) -> bool {
        let text = flags.to_string_lossy();
        let parts: Vec<&str> = text.split('\x1f').collect();
        parts.len() >= 2 && parts[parts.len() - 2..] == ["-D", "warnings"]
    }

    #[test]
    fn rustdocflags_put_deny_warnings_last() {
        let cases = [
            (vars(&[]), "-D\x1fwarnings"),
            (
                vars(&[("RUSTDOCFLAGS", " --cfg x -A warnings ")]),
                "--cfg\x1fx\x1f-A\x1fwarnings\x1f-D\x1fwarnings",
            ),
            // Cargo takes the encoded form over the plain one, as given.
            (
                vars(&[
                    ("RUSTDOCFLAGS", "-Aignored"),
                    ("CARGO_ENCODED_RUSTDOCFLAGS", "--cfg\x1fa b"),
                ]),
                "--cfg\x1fa b\x1f-D\x1fwarnings",
            ),
            (
                vars(&[("CARGO_ENCODED_RUSTDOCFLAGS", "")]),
                "-D\x1fwarnings",
            ),
        ];
        for (vars, want) in cases {
            let flags = rustdocflags(&vars);
            assert_eq!(flags, OsString::from(want), "{vars:?}");
            assert!(ends_in_deny_warnings(&flags), "{flags:?}");
        }
        // The control: flags that let warnings through.
        must_fail(
            "flags ending in -A warnings",
            ends_in_deny_warnings(&OsString::from("-A\x1fwarnings")),
        );
    }

    #[test]
    fn the_rust_version_is_read_from_the_workspace_package() {
        let root = std::fs::read_to_string(crate::workspace_root().join("Cargo.toml")).unwrap();
        assert_eq!(workspace_rust_version(&root).as_deref(), Some("1.95"));
        assert_eq!(
            crate::machine::rustc_version("rustc 1.95"),
            Some(crate::machine::MIN_RUST),
            "machine::MIN_RUST and the workspace's rust-version differ"
        );
        // The control: a rust-version in a package's own table isn't the
        // workspace's.
        must_fail(
            "a rust-version outside [workspace.package]",
            workspace_rust_version("[package]\nrust-version = \"1.90\"\n").ok_or(()),
        );
    }
}
