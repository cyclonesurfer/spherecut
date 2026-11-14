//! Developer tasks. Run with `cargo xtask <command>`.

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use spherecut_testkit::bless;

mod check;
mod compat;
mod controls;
mod gen_media;
mod gui_report;
mod hw_test;
mod machine;
mod perf_clips;

#[derive(Parser)]
#[command(about = "Spherecut developer tasks")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// fmt, clippy (-D warnings, also against the FFmpeg builds listed in
    /// SPHERECUT_FFMPEG_COMPAT, with the tests linked against each),
    /// rustdoc (-D warnings), the MSRV build (`rust-version`'s toolchain
    /// through rustup; skipped without it, except under CI), Tier 1 tests,
    /// doc tests, cargo-deny and cargo-audit. Clippy's network lints,
    /// rustdoc, the MSRV build and cargo-deny's network bans each also run
    /// on scratch crates made to fail them.
    Check(check::Args),
    /// Clippy and a link of the tests against other FFmpeg builds, each in
    /// its own target directory; --test also runs the Tier 1 tests against
    /// one, and refuses to start while a variable that blesses is on.
    ///
    /// Each build's target directory is `spherecut-compat-<label>`, `<label>`
    /// being the prefix's last component, in the directory
    /// SPHERECUT_FFMPEG_COMPAT_TARGET_DIR names, or else in compat/ in
    /// cargo's target directory. compat stamps each one it makes, and
    /// deletes one (built against another FFmpeg, or with --clean) only
    /// while it holds that stamp: a directory there without it is refused
    /// and left, as is a target directory that is a prefix, holds one or
    /// lies inside one (or inside any directory holding
    /// include/libavcodec/avcodec.h), symlinks and `..` resolved.
    ///
    /// A build passes only if pkg-config finds each library
    /// ffmpeg-sys-next binds in the prefix, before the build and after it,
    /// and the build searched for libraries only inside the prefix.
    Compat(compat::Args),
    /// Write the synthetic test-media set to test-media/.
    GenMedia(gen_media::Args),
    /// Tier 2 run on real hardware; writes `hw-reports/<host>-<commit>.md`.
    HwTest(hw_test::Args),
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Check(a) => check::run(a),
        Cmd::Compat(a) => compat::run(a),
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

/// Refuses to start while a variable that blesses is on
/// ([`spherecut_testkit::bless`]): `cargo xtask check`, `hw-test` and
/// `compat --test` hold the code to the goldens, floors and fixtures, and
/// a run with one on would rewrite them instead (the project review's
/// F055; compat's since the M3.7 review's R2, through
/// [`compat::refuse_blessing_for`]). `var` looks a variable up.
pub fn refuse_blessing(var: &dyn Fn(&str) -> Option<OsString>) -> Result<()> {
    let on = bless::on_in(var);
    let (names, verb, them) = match on.as_slice() {
        [] => return Ok(()),
        [one] => (one.to_string(), "is", "it"),
        _ => (on.join(" and "), "are", "them"),
    };
    bail!(
        "{names} {verb} on, so the tests this runs would bless rather than check: \
         unset {them}, or set {them} to 0, and run again"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use spherecut_testkit::bless::{BLESS_PROJECT, LOWER, UPDATE_GOLDENS};
    use spherecut_testkit::control::must_fail;

    type Refuse = fn(&dyn Fn(&str) -> Option<OsString>) -> Result<()>;

    /// Whether `refuse` refuses each blessing variable on alone, naming
    /// it, and lets a run start with both unset, empty or "0": the first
    /// case it gets wrong, if any.
    fn refuses_each_variable(refuse: Refuse) -> Result<(), String> {
        let run = |pairs: &[(&str, &str)]| {
            refuse(&|name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            })
        };
        for name in [UPDATE_GOLDENS, BLESS_PROJECT] {
            for value in ["1", LOWER] {
                match run(&[(name, value)]) {
                    Ok(()) => return Err(format!("{name}={value} isn't refused")),
                    Err(e) if !e.to_string().contains(name) => {
                        return Err(format!("{name}={value} is refused without its name: {e}"));
                    }
                    Err(_) => {}
                }
            }
        }
        for value in [None, Some(""), Some("0")] {
            let pairs: Vec<(&str, &str)> = value
                .map(|v| vec![(UPDATE_GOLDENS, v), (BLESS_PROJECT, v)])
                .unwrap_or_default();
            if let Err(e) = run(&pairs) {
                return Err(format!("both at {value:?} are refused: {e}"));
            }
        }
        Ok(())
    }

    #[test]
    fn compat_refuses_to_run_the_tier1_tests_while_blessing() {
        // `cargo xtask compat --test PREFIX` runs the Tier 1 suite (R2).
        refuses_each_variable(|var| {
            compat::refuse_blessing_for(Some(Path::new("/ff/n8.1.3")), var)
        })
        .unwrap();
        let e = must_fail(
            "compat as it was, refusing nothing",
            refuses_each_variable(|_| Ok(())),
        );
        assert!(e.contains(UPDATE_GOLDENS), "{e}");
        // Without --test it runs clippy and a link, which bless nothing.
        let on = |name: &str| (name == UPDATE_GOLDENS).then(|| OsString::from("1"));
        compat::refuse_blessing_for(None, &on).unwrap();
        // Control: a copy that refuses whatever the run, which refuses a
        // lint alone.
        fn always(_test: Option<&Path>, var: &dyn Fn(&str) -> Option<OsString>) -> Result<()> {
            refuse_blessing(var)
        }
        must_fail("refusing without --test", always(None, &on));
    }

    #[test]
    fn a_run_refuses_to_start_while_blessing() {
        refuses_each_variable(refuse_blessing).unwrap();
        // Controls: copies broken to read only SPHERECUT_UPDATE_GOLDENS,
        // and to read the variables by presence, as two blessings did
        // before F055.
        fn only_goldens(var: &dyn Fn(&str) -> Option<OsString>) -> Result<()> {
            refuse_blessing(&|name: &str| (name == UPDATE_GOLDENS).then(|| var(name)).flatten())
        }
        fn by_presence(var: &dyn Fn(&str) -> Option<OsString>) -> Result<()> {
            refuse_blessing(&|name: &str| var(name).map(|_| OsString::from("1")))
        }
        let e = must_fail(
            "reading only SPHERECUT_UPDATE_GOLDENS",
            refuses_each_variable(only_goldens),
        );
        assert!(e.contains(BLESS_PROJECT), "{e}");
        let e = must_fail(
            "reading the variables by presence",
            refuses_each_variable(by_presence),
        );
        assert!(e.contains("Some(\"\")"), "{e}");
    }
}
