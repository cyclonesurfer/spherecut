//! The check's own controls (DECISIONS D32): a lint or a ban that can't
//! fail proves nothing, so `cargo xtask check` also runs clippy's network
//! lints and cargo-deny's network bans (D8, D24), rustdoc's gate and the
//! MSRV build (D1) on scratch crates made to fail them, and on clean ones
//! that must pass. The scratch crates go under the target directory's
//! `xtask-controls/`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use spherecut_testkit::control::Outcome;

/// Where the scratch crates go.
const DIR: &str = "xtask-controls";

/// Writes a scratch crate (or workspace) `name` for a control under
/// `<target>/xtask-controls/`, emptied first: each of `files` is a path
/// relative to it and its text. Its `Cargo.toml` must have a
/// `[workspace]` of its own, so that cargo never takes it for part of
/// ours. Returns its directory.
pub fn scratch_crate(name: &str, files: &[(&str, &str)]) -> Result<PathBuf> {
    scratch_crate_in(&base(), name, files)
}

/// `<target>/xtask-controls/`.
fn base() -> PathBuf {
    crate::hw_test::target_dir(&crate::workspace_root()).join(DIR)
}

fn scratch_crate_in(base: &Path, name: &str, files: &[(&str, &str)]) -> Result<PathBuf> {
    let own = files
        .iter()
        .any(|(path, text)| *path == "Cargo.toml" && text.contains("[workspace]"));
    if !own {
        bail!("scratch crate {name}: its Cargo.toml must have a [workspace] of its own");
    }
    let dir = base.join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("emptying {}", dir.display()))?;
    }
    for (path, text) in files {
        let at = dir.join(path);
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&at, text).with_context(|| format!("writing {}", at.display()))?;
    }
    Ok(dir)
}

/// What `cmd` came to: its stderr, `Ok` when it passed and `Err` when it
/// failed, as [`must_fail`] reads a check.
fn outcome(cmd: &mut Command) -> Result<Result<String, String>> {
    let out = cmd
        .output()
        .with_context(|| format!("could not start {cmd:?}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok(if out.status.success() {
        Ok(stderr)
    } else {
        Err(stderr)
    })
}

/// The failure of control `name`, which must fail; a control that
/// passes fails the check, in D32's words.
pub fn must_fail<O: Outcome>(name: &str, outcome: O) -> Result<O::Failure> {
    outcome.into_failure().map_err(|why| {
        anyhow!(
            "control `{name}` did not fail ({why}): a control that stops failing fails the \
             check (DECISIONS D32)"
        )
    })
}

/// A package's manifest: `name`, private, MIT, edition 2021, its own
/// workspace if `alone`, and `rest` after.
fn manifest(name: &str, version: &str, alone: bool, rest: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n\
         publish = false\nlicense = \"MIT\"\n{}{rest}",
        if alone { "\n[workspace]\n" } else { "" }
    )
}

/// Clippy with the workspace's `clippy.toml` (in `root`) on two scratch
/// crates: a clean one passes, and one that opens a TCP stream and looks
/// up a name must fail on both, `disallowed_types` and
/// `disallowed_methods`.
pub fn clippy_controls(root: &Path) -> Result<()> {
    let lint = |dir: &Path| {
        outcome(
            Command::new("cargo")
                .arg("clippy")
                .arg("--manifest-path")
                .arg(dir.join("Cargo.toml"))
                .args(["--offline", "--quiet", "--", "-D", "warnings"])
                .env("CLIPPY_CONF_DIR", root)
                .env("CARGO_TARGET_DIR", dir.join("target")),
        )
    };
    let clean = scratch_crate(
        "clippy-clean",
        &[
            ("Cargo.toml", &manifest("control-clean", "0.1.0", true, "")),
            (
                "src/lib.rs",
                "//! A clean crate.\n\npub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n",
            ),
        ],
    )?;
    if let Err(stderr) = lint(&clean)? {
        bail!("clippy fails a clean scratch crate, so its controls say nothing:\n{stderr}");
    }
    let net = scratch_crate(
        "clippy-net",
        &[
            ("Cargo.toml", &manifest("control-net", "0.1.0", true, "")),
            (
                "src/lib.rs",
                "//! A crate that reaches the network.\n\n\
                 use std::net::{TcpStream, ToSocketAddrs};\n\n\
                 pub fn reach() -> std::io::Result<()> {\n    \
                 let _addrs = \"h:1\".to_socket_addrs()?;\n    \
                 let _stream = TcpStream::connect(\"h:1\")?;\n    \
                 Ok(())\n}\n",
            ),
        ],
    )?;
    let stderr = must_fail(
        "clippy on a crate that opens a TCP stream and looks up a name",
        lint(&net)?,
    )?;
    for lint in ["disallowed_types", "disallowed_methods"] {
        if !stderr.contains(lint) {
            bail!("clippy failed the network control, but not on {lint}:\n{stderr}");
        }
    }
    eprintln!("==> clippy's controls failed as they must");
    Ok(())
}

/// cargo-deny's bans from `root`'s `deny.toml` on two scratch
/// workspaces: one without dependencies passes, and one whose member
/// depends on crates named socket2 0.5.0, reqwest 0.12.0 and tokio 1.0.0
/// with its `net` feature must fail, naming all three.
pub fn deny_controls(root: &Path) -> Result<()> {
    let deny = |dir: &Path| {
        outcome(
            Command::new("cargo")
                .arg("deny")
                .arg("--manifest-path")
                .arg(dir.join("Cargo.toml"))
                .arg("--config")
                .arg(root.join("deny.toml"))
                .args(["--offline", "check", "bans"]),
        )
    };
    let banned = ["socket2", "reqwest", "tokio"];
    let workspace = |deps: &str| -> Vec<(String, String)> {
        let mut files = vec![
            (
                "Cargo.toml".to_string(),
                format!(
                    "[workspace]\nmembers = [\"app\"]\nexclude = {banned:?}\nresolver = \"2\"\n"
                ),
            ),
            (
                "app/Cargo.toml".into(),
                manifest(
                    "control-app",
                    "0.1.0",
                    false,
                    &format!("\n[dependencies]\n{deps}"),
                ),
            ),
            ("app/src/lib.rs".into(), String::new()),
        ];
        for (name, version) in banned.into_iter().zip(["0.5.0", "0.12.0", "1.0.0"]) {
            let features = if name == "tokio" {
                "\n[features]\nnet = []\n"
            } else {
                ""
            };
            files.push((
                format!("{name}/Cargo.toml"),
                manifest(name, version, false, features),
            ));
            files.push((format!("{name}/src/lib.rs"), String::new()));
        }
        files
    };
    let write = |name: &str, files: &[(String, String)]| {
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        scratch_crate(name, &files)
    };
    let clean = write("deny-clean", &workspace(""))?;
    if let Err(stderr) = deny(&clean)? {
        bail!(
            "cargo-deny fails a workspace without dependencies, so its controls say nothing:\n{stderr}"
        );
    }
    let net = write(
        "deny-net",
        &workspace(
            "socket2 = { path = \"../socket2\", version = \"0.5.0\" }\n\
             reqwest = { path = \"../reqwest\", version = \"0.12.0\" }\n\
             tokio = { path = \"../tokio\", version = \"1.0.0\", features = [\"net\"] }\n",
        ),
    )?;
    let stderr = must_fail(
        "cargo-deny on a workspace that depends on socket2, reqwest and tokio's net",
        deny(&net)?,
    )?;
    for name in banned {
        if !stderr.contains(&format!("'{name} = ")) {
            bail!("cargo-deny failed the network control, but didn't name {name}:\n{stderr}");
        }
    }
    eprintln!("==> cargo-deny's controls failed as they must");
    Ok(())
}

/// A scratch crate for rustdoc's controls, `rustdoc/<name>`: a lib
/// `doc_gate_control` from `lib` and a bin `doc-gate-control-bin` from
/// `bin`, no dependencies, and an empty `[workspace]` of its own.
fn doc_crate(name: &str, lib: &str, bin: &str) -> Result<PathBuf> {
    let toml = manifest(
        "doc-gate-control",
        "0.1.0",
        true,
        "\n[[bin]]\nname = \"doc-gate-control-bin\"\npath = \"src/main.rs\"\n",
    );
    scratch_crate(
        &format!("rustdoc/{name}"),
        &[
            ("Cargo.toml", &toml),
            ("src/lib.rs", lib),
            ("src/main.rs", bin),
        ],
    )
}

/// Rustdoc as the check runs it ([`crate::check::rustdoc_command`], the
/// check's flags) on scratch crates built offline into
/// `xtask-controls/target`: a clean one passes, and four must fail, each
/// naming its fault: a link to nothing, a link to a name that is both a
/// module and a macro, an unclosed HTML tag in a bin's private item's
/// docs, and the first again under `RUSTDOCFLAGS=-A warnings`, which the
/// check's `-D warnings` comes after.
pub fn rustdoc_controls() -> Result<()> {
    let target = base().join("target");
    let doc = |dir: &Path, flags: OsString| {
        outcome(
            crate::check::rustdoc_command(Some(&dir.join("Cargo.toml")), flags)
                .arg("--offline")
                .env("CARGO_TARGET_DIR", &target),
        )
    };
    let ours = || crate::check::rustdocflags(&[]);
    let lib =
        "//! A clean crate.\n\n/// Adds.\npub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n";
    let bin = "//! Its bin.\n\n/// Says hello.\nfn hello() -> &'static str {\n    \"hello\"\n}\n\n\
               fn main() {\n    println!(\"{}\", hello());\n}\n";
    let clean = doc_crate("clean", lib, bin)?;
    if let Err(stderr) = doc(&clean, ours())? {
        bail!("rustdoc fails a clean scratch crate, so its controls say nothing:\n{stderr}");
    }
    let nowhere = "//! See [`nowhere`].\n";
    let both = "//! [`file`] saves it.\n\n/// Files.\npub mod file {}\n";
    let html = "//! Its bin.\n\n/// spherecut-look-<ms>-<n>.jpg\nfn hello() -> &'static str {\n    \
                \"hello\"\n}\n\nfn main() {\n    println!(\"{}\", hello());\n}\n";
    let allowed = vec![(
        OsString::from("RUSTDOCFLAGS"),
        OsString::from("-A warnings"),
    )];
    let controls = [
        (
            "a link to nothing",
            "nowhere",
            nowhere,
            bin,
            ours(),
            "unresolved link",
        ),
        (
            "a link to both a module and a macro",
            "ambiguous",
            both,
            bin,
            ours(),
            "both a module and a macro",
        ),
        (
            "an unclosed HTML tag in a bin's private docs",
            "html",
            lib,
            html,
            ours(),
            "unclosed HTML tag `ms`",
        ),
        (
            "a link to nothing under RUSTDOCFLAGS=-A warnings",
            "allowed",
            nowhere,
            bin,
            crate::check::rustdocflags(&allowed),
            "unresolved link",
        ),
    ];
    let count = controls.len();
    for (what, name, lib, bin, flags, says) in controls {
        let dir = doc_crate(name, lib, bin)?;
        let stderr = must_fail(&format!("rustdoc on {what}"), doc(&dir, flags)?)?;
        if !stderr.contains(says) {
            bail!("rustdoc failed the control `{what}`, but not with \"{says}\":\n{stderr}");
        }
    }
    eprintln!("==> rustdoc's {count} controls failed as they must");
    Ok(())
}

/// The MSRV build's command ([`crate::check::msrv_command`]) on two
/// scratch crates built offline into `xtask-controls/target`: one whose
/// `rust-version` is `version` passes, and one a minor release past it
/// must fail, cargo saying that it requires that rustc.
pub fn msrv_controls(version: &str) -> Result<()> {
    let (major, minor) = crate::machine::rustc_version(&format!("rustc {version}"))
        .with_context(|| format!("{version:?} isn't a version"))?;
    let past = format!("{major}.{}", minor + 1);
    let target = base().join("target");
    let at = |name: &str, rust: &str| -> Result<Result<String, String>> {
        let dir = scratch_crate(
            &format!("msrv/{name}"),
            &[
                (
                    "Cargo.toml",
                    &format!(
                        "[package]\nname = \"msrv-control\"\nversion = \"0.1.0\"\n\
                         edition = \"2021\"\nrust-version = \"{rust}\"\npublish = false\n\
                         license = \"MIT\"\n\n[workspace]\n"
                    ),
                ),
                ("src/lib.rs", "//! A crate for the MSRV build's controls.\n"),
            ],
        )?;
        outcome(
            crate::check::msrv_command(version, Some(&dir.join("Cargo.toml")), &target)
                .arg("--offline"),
        )
    };
    if let Err(stderr) = at("at", version)? {
        bail!(
            "the MSRV build fails a crate whose rust-version is {version}, so its controls \
             say nothing:\n{stderr}"
        );
    }
    let stderr = must_fail(
        &format!("the MSRV build of a crate that needs Rust {past}"),
        at("past", &past)?,
    )?;
    let says = format!("requires rustc {past}");
    if !stderr.contains(&says) {
        bail!("the MSRV build failed its control, but not with \"{says}\":\n{stderr}");
    }
    eprintln!("==> the MSRV build's controls failed as they must");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_crate_is_written_afresh_with_a_workspace_of_its_own() {
        let base = std::env::temp_dir().join(format!("xtask-scratch-{}", std::process::id()));
        let files = [
            ("Cargo.toml", "[package]\nname = \"x\"\n\n[workspace]\n"),
            ("src/lib.rs", "pub fn x() {}\n"),
        ];
        let dir = scratch_crate_in(&base, "one", &files).unwrap();
        assert_eq!(dir, base.join("one"));
        std::fs::write(dir.join("left-over"), "").unwrap();
        let dir = scratch_crate_in(&base, "one", &files).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
            "pub fn x() {}\n"
        );
        assert!(!dir.join("left-over").exists());
        // The control: a crate cargo would take for one of ours.
        let e = must_fail(
            "a scratch crate without a workspace of its own",
            scratch_crate_in(&base, "two", &[("Cargo.toml", "[package]\nname = \"y\"\n")]),
        )
        .unwrap();
        assert!(e.to_string().contains("[workspace] of its own"), "{e}");
        assert!(!base.join("two").exists());
        // A control that passes fails the check, in D32's words.
        let e = must_fail("a check that passes", true).unwrap_err();
        assert_eq!(
            e.to_string(),
            "control `a check that passes` did not fail (it passed): a control that stops \
             failing fails the check (DECISIONS D32)"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
