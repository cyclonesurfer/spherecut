//! `cargo xtask hw-test`: the Tier 2 run on real hardware.
//!
//! Runs without sudo from a fresh clone. It checks its own prerequisites
//! (printing the exact install command for anything missing), records the
//! environment, builds in release mode, runs the Tier 2 tests and the
//! benchmarks, and writes one self-contained Markdown report to
//! `hw-reports/<host>-<commit>.md`.
//!
//! Tier 2 tests are `#[ignore]`d so plain `cargo test` skips them. They call
//! `spherecut_testkit::tier2::require_*`, which fails the test with a
//! `TIER2-SKIP:` marker when a prerequisite (device, sample) is missing; this
//! module reports those as skipped with the reason, never as passed.
//!
//! The Tier 2 tests are listed (`cargo test -- --ignored --list`) before
//! they run, and the report has a line for every listed test. A test that
//! never reports a result, because its binary did not start or crashed
//! first, shows up as "did not run" and fails the run.
//!
//! The command exits non-zero when the build fails or a Tier 2 test fails or
//! does not run; skips alone are not a failure. The report is written either
//! way.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::workspace_root;

/// What a skipped test's panic message starts with
/// (`spherecut_testkit::tier2::SKIP_MARKER`).
const SKIP_MARKER: &str = "TIER2-SKIP:";

/// `--quick` leaves out tests whose names contain this: debug-build timings
/// would only mislead.
const BENCH: &str = "bench_";

#[derive(clap::Args)]
pub struct Args {
    /// Directory with camera samples (default: ./samples).
    #[arg(long)]
    samples: Option<PathBuf>,
    /// Build in debug mode and skip the benchmarks (environment and tests only).
    #[arg(long)]
    quick: bool,
    /// Where to write the report (default: hw-reports/<host>-<commit>.md).
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Output of one external command.
struct Captured {
    cmdline: String,
    ok: bool,
    text: String,
    elapsed: Duration,
}

fn capture(program: &str, args: &[&str]) -> Captured {
    capture_env(program, args, &[])
}

fn cmdline(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn capture_env(program: &str, args: &[&str], env: &[(&str, &str)]) -> Captured {
    let cmdline = cmdline(program, args);
    let start = Instant::now();
    let out = Command::new(program)
        .args(args)
        .envs(env.iter().copied())
        .current_dir(workspace_root())
        .stdin(Stdio::null())
        .output();
    let elapsed = start.elapsed();
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            Captured {
                cmdline,
                ok: o.status.success(),
                text,
                elapsed,
            }
        }
        Err(e) => Captured {
            cmdline,
            ok: false,
            text: format!("could not run `{program}`: {e}"),
            elapsed,
        },
    }
}

/// Like `capture_env`, but stdout and stderr share one pipe, so the text is
/// in the order it was written. `cargo test` names each test binary on
/// stderr and the binary prints its results on stdout; only the merged
/// stream says which binary a result came from.
fn capture_merged(program: &str, args: &[&str], env: &[(&str, &str)]) -> Captured {
    let cmdline = cmdline(program, args);
    let start = Instant::now();
    let run = || -> std::io::Result<(bool, String)> {
        let (mut reader, writer) = std::io::pipe()?;
        // The Command is a temporary, so its copies of the write end close
        // once the child has started and the read below ends at EOF.
        let mut child = Command::new(program)
            .args(args)
            .envs(env.iter().copied())
            .current_dir(workspace_root())
            .stdin(Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            .spawn()?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        let status = child.wait()?;
        Ok((
            status.success(),
            String::from_utf8_lossy(&bytes).into_owned(),
        ))
    };
    let (ok, text) = run().unwrap_or_else(|e| (false, format!("could not run `{program}`: {e}")));
    Captured {
        cmdline,
        ok,
        text,
        elapsed: start.elapsed(),
    }
}

fn have(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Distro {
    Fedora,
    Arch,
    Other,
}

fn os_release() -> BTreeMap<String, String> {
    std::fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
        .collect()
}

fn distro(os: &BTreeMap<String, String>) -> Distro {
    let ids = format!(
        "{} {}",
        os.get("ID").map(String::as_str).unwrap_or(""),
        os.get("ID_LIKE").map(String::as_str).unwrap_or("")
    );
    if ids.contains("fedora") {
        Distro::Fedora
    } else if ids.contains("arch") {
        Distro::Arch
    } else {
        Distro::Other
    }
}

/// A missing prerequisite and how to fix it.
struct Missing {
    what: String,
    fix: String,
    blocks: &'static str,
}

fn pkg_cmd(d: Distro, fedora: &str, arch: &str) -> String {
    match d {
        Distro::Fedora => format!("sudo dnf install {fedora}"),
        Distro::Arch => format!("sudo pacman -S --needed {arch}"),
        Distro::Other => format!("install the equivalent of Fedora's `{fedora}` / Arch's `{arch}`"),
    }
}

fn check_prereqs(d: Distro, ffmpeg_info: &str, vainfo: &str) -> Vec<Missing> {
    let mut m = Vec::new();
    if !have("vulkaninfo") {
        m.push(Missing {
            what: "vulkaninfo".into(),
            fix: pkg_cmd(d, "vulkan-tools", "vulkan-tools"),
            blocks: "recording the Vulkan device in the report",
        });
    }
    if !have("vainfo") {
        m.push(Missing {
            what: "vainfo".into(),
            fix: pkg_cmd(d, "libva-utils", "libva-utils"),
            blocks: "recording VA-API profiles in the report",
        });
    }
    if !have("clang") {
        m.push(Missing {
            what: "clang (FFmpeg bindings are generated with bindgen)".into(),
            fix: pkg_cmd(d, "clang-devel", "clang"),
            blocks: "building Spherecut",
        });
    }
    if !have("pkg-config") && !have("pkgconf") {
        m.push(Missing {
            what: "pkg-config".into(),
            fix: pkg_cmd(d, "pkgconf-pkg-config", "pkgconf"),
            blocks: "building Spherecut",
        });
    }
    let headers = capture("pkg-config", &["--exists", "libavcodec", "libavformat"]);
    if !headers.ok {
        m.push(Missing {
            what: "FFmpeg development headers".into(),
            fix: match d {
                // ffmpeg-devel comes from RPM Fusion; see the codec note below.
                Distro::Fedora => "sudo dnf install ffmpeg-devel   (from RPM Fusion; enable it first if needed, see below)".into(),
                Distro::Arch => "sudo pacman -S --needed ffmpeg".into(),
                Distro::Other => "install FFmpeg's development headers (libavcodec-dev etc.)".into(),
            },
            blocks: "building Spherecut",
        });
    }
    // Fedora's ffmpeg-free and stock Mesa leave out H.264/HEVC.
    let has_hevc_decoder = ffmpeg_info.contains("hevc");
    if d == Distro::Fedora {
        let rpm = capture("rpm", &["-q", "ffmpeg-free"]);
        if rpm.ok {
            m.push(Missing {
                what: "full FFmpeg (ffmpeg-free is installed, which has no H.264/HEVC)".into(),
                fix: "sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm && sudo dnf swap ffmpeg-free ffmpeg --allowerasing".into(),
                blocks: "H.264/HEVC decode and encode",
            });
        }
        let amd = gpu_vendor().as_deref() == Some("0x1002");
        if amd && !capture("rpm", &["-q", "mesa-va-drivers-freeworld"]).ok {
            let fix = if capture("rpm", &["-q", "mesa-va-drivers"]).ok {
                "sudo dnf swap mesa-va-drivers mesa-va-drivers-freeworld   (RPM Fusion free)"
            } else {
                "sudo dnf install mesa-va-drivers-freeworld   (RPM Fusion free)"
            };
            m.push(Missing {
                what: "Mesa VA-API driver with H.264/HEVC (Fedora's own build leaves them out)"
                    .into(),
                fix: fix.into(),
                blocks: "hardware H.264/HEVC decode and encode on AMD",
            });
        }
    }
    if !has_hevc_decoder {
        m.push(Missing {
            what: "an FFmpeg build with the HEVC decoder".into(),
            fix: match d {
                Distro::Fedora => "see the RPM Fusion ffmpeg swap above".into(),
                Distro::Arch => "sudo pacman -S --needed ffmpeg".into(),
                Distro::Other => "install a full FFmpeg build".into(),
            },
            blocks: "decoding camera files",
        });
    }
    if !Path::new("/dev/dri/renderD128").exists() {
        m.push(Missing {
            what: "a GPU render node (/dev/dri/renderD128)".into(),
            fix: "check that the GPU driver is loaded and that your user is in the `render` group (`groups`; `sudo usermod -aG render $USER`, then log in again)".into(),
            blocks: "all hardware tests",
        });
    }
    for (sub, pkg) in [("fmt", "rustfmt"), ("clippy", "clippy")] {
        if !crate::have_cargo_subcommand(sub) {
            m.push(Missing {
                what: format!("cargo {sub}"),
                fix: if have("rustup") {
                    format!("rustup component add {pkg}")
                } else {
                    format!(
                        "{}   (or install rustup and use its toolchain)",
                        pkg_cmd(d, pkg, "rust")
                    )
                },
                blocks: "`cargo xtask check`",
            });
        }
    }
    if !vainfo.is_empty() && !vainfo.contains("VAProfileHEVCMain") {
        m.push(Missing {
            what: "VA-API HEVC profiles (vainfo lists none)".into(),
            fix: match d {
                Distro::Fedora => "AMD: install mesa-va-drivers-freeworld (above); Intel: sudo dnf install intel-media-driver   (RPM Fusion nonfree)".into(),
                Distro::Arch => "sudo pacman -S --needed mesa   (AMD) or intel-media-driver (Intel)".into(),
                Distro::Other => "install a VA-API driver with HEVC enabled".into(),
            },
            blocks: "hardware HEVC decode and encode",
        });
    }
    m
}

/// PCI vendor id of the first render node's GPU, e.g. "0x1002" (AMD) or
/// "0x8086" (Intel).
fn gpu_vendor() -> Option<String> {
    std::fs::read_to_string("/sys/class/drm/renderD128/device/vendor")
        .ok()
        .map(|s| s.trim().to_string())
}

/// How one Tier 2 test ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Passed,
    /// With the test's output.
    Failed(String),
    /// With the `TIER2-SKIP:` reason.
    Skipped(String),
    /// A benchmark, left out by `--quick`.
    LeftOut,
    /// Listed, but no result came back.
    DidNotRun,
}

/// Splits `cargo test` output into one part per test binary. A binary is
/// named after its executable without Cargo's hash (`tier2_samples`), and
/// doc tests as `<crate> (doc-tests)`. Text before the first binary
/// (compiler output) is dropped.
fn split_by_binary(output: &str) -> Vec<(String, String)> {
    let mut parts: Vec<(String, String)> = Vec::new();
    // Short name -> the executable that has it.
    let mut taken: BTreeMap<&str, &str> = BTreeMap::new();
    for line in output.lines() {
        // Cargo indents its status lines; test output that happens to say
        // "Running" usually isn't.
        let l = line.trim_start();
        let indented = l.len() < line.len();
        let exe = l
            .strip_prefix("Running ")
            .and_then(|r| r.strip_suffix(')'))
            .and_then(|r| r.rsplit_once(" ("))
            .map(|(_, exe)| exe)
            .filter(|_| indented);
        let name = if let Some(exe) = exe {
            let file = exe.rsplit('/').next().unwrap_or(exe);
            let short = strip_hash(file);
            match taken.get(short) {
                // Two packages can each have a `tests/foo.rs`.
                Some(&other) if other != exe => Some(file.to_string()),
                _ => {
                    taken.insert(short, exe);
                    Some(short.to_string())
                }
            }
        } else {
            l.strip_prefix("Doc-tests ")
                .filter(|_| indented)
                .map(|c| format!("{} (doc-tests)", c.trim()))
        };
        match name {
            Some(name) => parts.push((name, String::new())),
            None => {
                if let Some((_, text)) = parts.last_mut() {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
    }
    parts
}

/// `tier2_samples-2dfb113bee85abda` -> `tier2_samples`.
fn strip_hash(file: &str) -> &str {
    match file.rsplit_once('-') {
        Some((name, hash)) if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            name
        }
        _ => file,
    }
}

/// The tests in each binary, from `cargo test -- --ignored --list`.
fn parse_list(output: &str) -> BTreeMap<String, Vec<String>> {
    split_by_binary(output)
        .into_iter()
        .map(|(bin, text)| {
            let tests = text
                .lines()
                .filter_map(|l| l.strip_suffix(": test"))
                .map(str::to_string)
                .collect();
            (bin, tests)
        })
        .collect()
}

/// Results from one binary's libtest output, run with `--test-threads=1`.
///
/// Libraries such as libva write straight to stderr, which can split a
/// `test name ... ok` line, so a result is also taken from a bare `ok` or
/// `FAILED` line, from the failure sections and from the `failures:` list
/// at the end. A test that started but has no result failed: its binary
/// stopped (crashed, aborted, was killed) while it ran, and what follows in
/// the output says how.
fn parse_libtest(output: &str) -> BTreeMap<String, Outcome> {
    let lines: Vec<&str> = output.lines().collect();
    // (name, status, line index), in the order they started.
    let mut started: Vec<(String, String, usize)> = Vec::new();
    let mut sections: BTreeMap<String, String> = BTreeMap::new();
    let mut listed: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut in_list = false;
    let mut finished = false;
    for (i, &line) in lines.iter().enumerate() {
        if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|l| l.strip_suffix(" stdout ----"))
        {
            sections.entry(name.to_string()).or_default();
            current = Some(name.to_string());
        } else if line == "failures:" {
            current = None;
            in_list = true;
        } else if line.starts_with("test result:") {
            current = None;
            in_list = false;
            finished = true;
        } else if let Some(name) = &current {
            let text = sections.entry(name.clone()).or_default();
            text.push_str(line);
            text.push('\n');
        } else if let Some((name, status)) = line
            .strip_prefix("test ")
            .and_then(|l| l.split_once(" ... "))
        {
            started.push((name.trim().to_string(), status.trim().to_string(), i));
        } else if matches!(line, "ok" | "FAILED") {
            if let Some((_, status, _)) = started.last_mut()
                && !matches!(status.as_str(), "ok" | "FAILED")
            {
                *status = line.to_string();
            }
        } else if in_list && let Some(name) = line.strip_prefix("    ") {
            listed.push(name.trim().to_string());
        }
    }
    let mut out = BTreeMap::new();
    for (name, status, at) in &started {
        let outcome = if let Some(text) = sections.get(name) {
            failure(text)
        } else if status == "FAILED" || listed.contains(name) {
            Outcome::Failed(String::new())
        } else if status.starts_with("ignored") {
            // Not run, so no result (`--ignored` runs only these, though).
            continue;
        } else if status == "ok" || finished {
            // A failed test would be in the `failures:` list.
            Outcome::Passed
        } else {
            let rest = std::iter::once(status.as_str()).chain(lines[at + 1..].iter().copied());
            Outcome::Failed(format!(
                "The test binary stopped before this test reported a result:\n\n{}",
                rest.collect::<Vec<_>>().join("\n")
            ))
        };
        out.insert(name.clone(), outcome);
    }
    for (name, text) in &sections {
        out.entry(name.clone()).or_insert_with(|| failure(text));
    }
    for name in listed {
        out.entry(name).or_insert(Outcome::Failed(String::new()));
    }
    out
}

/// A failed test's outcome: skipped if its panic message starts with the
/// skip marker.
fn failure(text: &str) -> Outcome {
    let mut lines = text.lines();
    // libtest prints `thread '…' panicked at file:line:col:`, then the message.
    if lines.any(|l| l.contains(" panicked at "))
        && let Some(reason) = lines.next().and_then(|m| m.strip_prefix(SKIP_MARKER))
    {
        return Outcome::Skipped(reason.trim().to_string());
    }
    Outcome::Failed(text.to_string())
}

/// Every listed test exactly once, as `binary::test` in name order, with how
/// it ended. A listed test without a result did not run, unless it is a
/// benchmark that `--quick` leaves out. Results for tests that were not
/// listed are kept too.
fn merge(
    listed: &BTreeMap<String, Vec<String>>,
    ran: &BTreeMap<String, BTreeMap<String, Outcome>>,
    quick: bool,
) -> Vec<(String, Outcome)> {
    let mut all: BTreeMap<(&str, &str), Outcome> = BTreeMap::new();
    for (bin, tests) in listed {
        for t in tests {
            let missing = if quick && t.contains(BENCH) {
                Outcome::LeftOut
            } else {
                Outcome::DidNotRun
            };
            let o = ran.get(bin).and_then(|r| r.get(t)).cloned();
            all.insert((bin.as_str(), t.as_str()), o.unwrap_or(missing));
        }
    }
    for (bin, results) in ran {
        for (t, o) in results {
            all.entry((bin.as_str(), t.as_str()))
                .or_insert_with(|| o.clone());
        }
    }
    all.into_iter()
        .map(|((bin, t), o)| (format!("{bin}::{t}"), o))
        .collect()
}

/// The Tier 2 part of a run.
struct Tier2 {
    /// `cargo test -- --ignored --list`: what should run.
    list: Captured,
    run: Captured,
    /// The run's output per test binary.
    output: BTreeMap<String, String>,
    /// Every Tier 2 test once.
    results: Vec<(String, Outcome)>,
}

impl Tier2 {
    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.results.iter().filter(|(_, o)| f(o)).count()
    }

    /// How many tests did not run, per binary.
    fn did_not_run(&self) -> BTreeMap<&str, usize> {
        let mut by_bin = BTreeMap::new();
        for (name, o) in &self.results {
            if *o == Outcome::DidNotRun {
                let bin = name.split_once("::").map_or(name.as_str(), |(b, _)| b);
                *by_bin.entry(bin).or_default() += 1;
            }
        }
        by_bin
    }

    /// `cargo test` failed, but no test failed or skipped (a skip is a
    /// failure to cargo), so the failure is somewhere else.
    fn unexplained_failure(&self) -> bool {
        !self.run.ok && self.count(|o| matches!(o, Outcome::Failed(_) | Outcome::Skipped(_))) == 0
    }

    /// Why the Tier 2 part failed; empty if it did not. Skips alone are fine.
    fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        if !self.list.ok {
            p.push("listing the Tier 2 tests failed, so some may be missing".to_string());
        }
        let failed = self.count(|o| matches!(o, Outcome::Failed(_)));
        if failed > 0 {
            p.push(format!("{failed} Tier 2 test(s) failed"));
        }
        let missing = self.count(|o| *o == Outcome::DidNotRun);
        if missing > 0 {
            p.push(format!("{missing} Tier 2 test(s) did not run"));
        }
        if self.unexplained_failure() {
            p.push("`cargo test` failed without a failing test".to_string());
        }
        p
    }

    /// One line with every count, so nothing missing can look clean.
    fn counts(&self, quick: bool) -> String {
        let left_out = if quick {
            format!(
                "{} left out by `--quick`, ",
                self.count(|o| *o == Outcome::LeftOut)
            )
        } else {
            String::new()
        };
        format!(
            "Tier 2: {} passed, {} failed, {} skipped, {left_out}{} did not run ({} tests)",
            self.count(|o| *o == Outcome::Passed),
            self.count(|o| matches!(o, Outcome::Failed(_))),
            self.count(|o| matches!(o, Outcome::Skipped(_))),
            self.count(|o| *o == Outcome::DidNotRun),
            self.results.len()
        )
    }
}

/// Lists the Tier 2 tests, runs them all, and pairs every listed test with
/// its outcome.
fn run_tier2(quick: bool, samples: &Path, bench_out: &Path) -> Tier2 {
    // A skip is a test failure to cargo; without --no-fail-fast it stops
    // after the first binary that has one.
    let mut cargo = vec!["test", "--workspace", "--no-fail-fast", "--color", "never"];
    if !quick {
        cargo.push("--release");
    }
    eprintln!("==> listing Tier 2 tests");
    let mut args = cargo.clone();
    args.extend(["--", "--ignored", "--list"]);
    let list = capture_merged("cargo", &args, &[]);

    eprintln!("==> running Tier 2 tests");
    let mut args = cargo;
    args.extend(["--", "--ignored", "--test-threads=1"]);
    if quick {
        args.extend(["--skip", BENCH]);
    }
    // A stale file would mix in another run's numbers.
    if let Err(e) = std::fs::remove_file(bench_out)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("warning: could not clear {}: {e}", bench_out.display());
    }
    let samples_s = samples.to_string_lossy().into_owned();
    let bench_s = bench_out.to_string_lossy().into_owned();
    let run = capture_merged(
        "cargo",
        &args,
        &[
            ("SPHERECUT_TIER2", "1"),
            ("SPHERECUT_SAMPLES", &samples_s),
            ("SPHERECUT_BENCH_OUT", &bench_s),
        ],
    );
    let output: BTreeMap<String, String> = split_by_binary(&run.text).into_iter().collect();
    let ran = output
        .iter()
        .map(|(bin, text)| (bin.clone(), parse_libtest(text)))
        .collect();
    let results = merge(&parse_list(&list.text), &ran, quick);
    Tier2 {
        list,
        run,
        output,
        results,
    }
}

/// Cargo's target directory as `cargo metadata` gives it, which follows
/// `CARGO_TARGET_DIR` and `build.target-dir`.
fn target_dir(root: &Path) -> PathBuf {
    let m = capture("cargo", &["metadata", "--format-version", "1", "--no-deps"]);
    if m.ok
        && let Some(dir) = json_str_field(&m.text, "target_directory")
    {
        return PathBuf::from(dir);
    }
    eprintln!("warning: `cargo metadata` gave no target directory; guessing");
    std::env::var_os("CARGO_TARGET_DIR")
        .map(|d| root.join(d))
        .unwrap_or_else(|| root.join("target"))
}

/// The string value of `"key":` in compact JSON such as `cargo metadata`
/// prints; enough to read one field without a JSON parser.
fn json_str_field(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = json.find(&pat)? + pat.len();
    let mut chars = json[start..].chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
}

fn short_commit() -> String {
    let c = capture("git", &["rev-parse", "--short=10", "HEAD"]);
    // Reports from earlier runs stay untracked until they are pushed; they
    // are not changes to the code under test.
    let status = capture(
        "git",
        &["status", "--porcelain", "--", ".", ":(exclude)hw-reports"],
    );
    let dirty = status.ok && !status.text.trim().is_empty();
    let base = if c.ok {
        c.text.trim().to_string()
    } else {
        "nogit".into()
    };
    if dirty { format!("{base}-dirty") } else { base }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let c = capture("uname", &["-n"]);
            c.ok.then(|| c.text.trim().to_string())
        })
        .unwrap_or_else(|| "unknown-host".into())
}

fn details(out: &mut String, title: &str, c: &Captured) {
    let _ = writeln!(
        out,
        "<details><summary>{title} (<code>{}</code>{})</summary>\n\n```\n{}\n```\n</details>\n",
        c.cmdline,
        if c.ok { "" } else { ", failed" },
        c.text.trim_end()
    );
}

fn first_line_containing<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
    text.lines().find(|l| l.contains(needle)).map(str::trim)
}

pub fn run(a: Args) -> Result<()> {
    let root = workspace_root();
    let started = Instant::now();
    let host = hostname();
    let commit = short_commit();
    let os = os_release();
    let d = distro(&os);
    // Cargo runs each test binary in its package directory, so the tests
    // need an absolute path to see the same directory as this report.
    let samples = a.samples.unwrap_or_else(|| root.join("samples"));
    let samples = std::fs::canonicalize(&samples)
        .or_else(|_| std::path::absolute(&samples))
        .with_context(|| format!("resolving {}", samples.display()))?;
    let target = target_dir(&root);
    let profile = if a.quick { "debug" } else { "release" };
    let bench_out = target.join("hw-bench.tsv");

    eprintln!("==> recording environment");
    let uname = capture("uname", &["-srvm"]);
    let rustc = capture("rustc", &["-Vv"]);
    let lspci = capture("sh", &["-c", "lspci -nn | grep -Ei 'vga|display|3d'"]);
    let vulkan = capture("vulkaninfo", &["--summary"]);
    let vainfo = capture(
        "vainfo",
        &["--display", "drm", "--device", "/dev/dri/renderD128"],
    );
    let ffmpeg_ver = capture("ffmpeg", &["-hide_banner", "-version"]);
    let hwaccels = capture("ffmpeg", &["-hide_banner", "-hwaccels"]);
    let encoders = capture("ffmpeg", &["-hide_banner", "-encoders"]);
    let decoders = capture("ffmpeg", &["-hide_banner", "-decoders"]);
    let libs = capture(
        "pkg-config",
        &[
            "--modversion",
            "libavcodec",
            "libavformat",
            "libavutil",
            "libswscale",
            "libswresample",
        ],
    );
    let session = format!(
        "{} / {}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "?".into()),
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "?".into())
    );
    let mesa = first_line_containing(&vulkan.text, "driverInfo")
        .map(|l| {
            l.trim_start_matches("driverInfo")
                .trim_start_matches([' ', '='])
                .to_string()
        })
        .unwrap_or_else(|| "unknown (vulkaninfo unavailable)".into());
    let gpu = first_line_containing(&vulkan.text, "deviceName")
        .map(|l| {
            l.trim_start_matches("deviceName")
                .trim_start_matches([' ', '='])
                .to_string()
        })
        .or_else(|| lspci.text.lines().next().map(str::to_string))
        .unwrap_or_else(|| "unknown".into());

    eprintln!("==> checking prerequisites");
    let missing = check_prereqs(d, &decoders.text, if vainfo.ok { &vainfo.text } else { "" });
    let build_blocked = missing.iter().any(|m| m.blocks == "building Spherecut");

    let mut build: Option<Captured> = None;
    let mut tier2: Option<Tier2> = None;
    let mut doctor: Option<Captured> = None;
    if build_blocked {
        eprintln!("==> build prerequisites missing; skipping build and tests");
    } else {
        // `--quick` builds too: `cargo test` alone would not rebuild the
        // spherecut binary, and doctor output from an old one misleads.
        eprintln!("==> building ({profile})");
        let mut args = vec!["build", "--workspace", "--all-targets"];
        if !a.quick {
            args.push("--release");
        }
        let b = capture("cargo", &args);
        let built = b.ok;
        build = Some(b);
        if built {
            tier2 = Some(run_tier2(a.quick, &samples, &bench_out));
            let exe = target.join(profile).join("spherecut");
            doctor = Some(capture(&exe.to_string_lossy(), &["doctor"]));
        }
    }

    let mut problems = Vec::new();
    match &build {
        _ if build_blocked => problems.push("build prerequisites are missing".to_string()),
        Some(b) if !b.ok => problems.push(format!("the {profile} build failed")),
        _ => {}
    }
    if let Some(t) = &tier2 {
        problems.extend(t.problems());
    }

    // ---- report ----
    let mut r = String::new();
    let _ = writeln!(r, "# Hardware report: {host} @ {commit}\n");
    let _ = writeln!(
        r,
        "Generated by `cargo xtask hw-test{}` in {:.0} s.\n",
        if a.quick { " --quick" } else { "" },
        started.elapsed().as_secs_f64()
    );
    let _ = writeln!(r, "## Summary\n");
    let _ = writeln!(r, "| | |\n|---|---|");
    let _ = writeln!(r, "| Host | {host} |");
    let _ = writeln!(
        r,
        "| OS | {} |",
        os.get("PRETTY_NAME").map(String::as_str).unwrap_or("?")
    );
    let _ = writeln!(r, "| Kernel | {} |", uname.text.trim());
    let _ = writeln!(r, "| Session | {session} |");
    let _ = writeln!(r, "| GPU (Vulkan) | {gpu} |");
    let _ = writeln!(r, "| Mesa | {mesa} |");
    let _ = writeln!(
        r,
        "| FFmpeg | {} |",
        ffmpeg_ver.text.lines().next().unwrap_or("not found").trim()
    );
    let _ = writeln!(
        r,
        "| FFmpeg libs | {} |",
        if libs.ok {
            libs.text.split_whitespace().collect::<Vec<_>>().join(", ")
        } else {
            "headers not found".into()
        }
    );
    let _ = writeln!(
        r,
        "| Rust | {} |",
        rustc.text.lines().next().unwrap_or("not found")
    );
    let hevc = |entry: &str| {
        vainfo
            .text
            .lines()
            .any(|l| l.contains("VAProfileHEVCMain ") && l.contains(entry))
    };
    let _ = writeln!(
        r,
        "| VA-API HEVC | {} |",
        if !vainfo.ok {
            "no VA-API device (vainfo failed)".to_string()
        } else {
            format!(
                "decode: {}, encode: {}",
                yes_no(hevc("VAEntrypointVLD")),
                yes_no(hevc("VAEntrypointEncSlice"))
            )
        }
    );
    let _ = writeln!(
        r,
        "| FFmpeg hwaccels | {} |",
        hwaccels
            .text
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(
        r,
        "| Samples dir | {} ({}) |\n",
        samples.display(),
        if samples.is_dir() {
            "present"
        } else {
            "missing"
        }
    );

    match &tier2 {
        Some(t) => {
            let _ = writeln!(r, "**{}.**\n", t.counts(a.quick));
        }
        None => {
            let _ = writeln!(r, "**Tier 2: not run.**\n");
        }
    }
    if !problems.is_empty() {
        let _ = writeln!(r, "**This run FAILED:** {}.\n", problems.join("; "));
    }

    let _ = writeln!(r, "## Prerequisites\n");
    if missing.is_empty() {
        let _ = writeln!(r, "Nothing missing.\n");
    } else {
        for m in &missing {
            let _ = writeln!(
                r,
                "- **Missing: {}** (blocks {})\n  ```\n  {}\n  ```",
                m.what, m.blocks, m.fix
            );
        }
        let _ = writeln!(r);
    }

    let _ = writeln!(r, "## Build\n");
    match &build {
        None => {
            let _ = writeln!(r, "Not attempted: build prerequisites are missing.\n");
        }
        Some(b) => {
            let _ = writeln!(
                r,
                "{} ({profile}) in {:.0} s.\n",
                if b.ok { "Succeeded" } else { "**FAILED**" },
                b.elapsed.as_secs_f64()
            );
            if !b.ok {
                let _ = writeln!(r, "```\n{}\n```\n", tail(&b.text, 80));
            }
        }
    }

    let _ = writeln!(r, "## Tier 2 tests\n");
    match &tier2 {
        None => {
            let _ = writeln!(r, "Not run: there was no build.\n");
        }
        Some(t) => {
            let _ = writeln!(r, "| Test | Result |\n|---|---|");
            for (n, o) in &t.results {
                let result = match o {
                    Outcome::Passed => "pass".to_string(),
                    Outcome::Failed(_) => "**FAIL**".to_string(),
                    Outcome::Skipped(why) => format!("skipped: {why}"),
                    Outcome::LeftOut => "skipped: benchmark, left out by `--quick`".to_string(),
                    Outcome::DidNotRun => "**did not run**".to_string(),
                };
                let _ = writeln!(r, "| `{n}` | {result} |");
            }
            if t.results.is_empty() {
                let _ = writeln!(r, "| (no Tier 2 tests found) | |");
            }
            let _ = writeln!(r);
            for (n, o) in &t.results {
                if let Outcome::Failed(out) = o {
                    let out = if out.trim().is_empty() {
                        "(libtest printed no output for this failure)".to_string()
                    } else {
                        tail(out, 60)
                    };
                    let _ = writeln!(r, "### FAIL `{n}`\n\n```\n{out}\n```\n");
                }
            }
            if !t.list.ok {
                let _ = writeln!(
                    r,
                    "Listing the Tier 2 tests (`{}`) failed, so tests may be missing above:\n\n```\n{}\n```\n",
                    t.list.cmdline,
                    tail(&t.list.text, 60)
                );
            }
            let did_not_run = t.did_not_run();
            for (bin, n) in &did_not_run {
                let (why, out) = match t.output.get(*bin) {
                    Some(out) => ("it stopped early. The end of its output", out),
                    None => (
                        "cargo never ran it. The end of the test output",
                        &t.run.text,
                    ),
                };
                let _ = writeln!(
                    r,
                    "{n} test(s) in `{bin}` did not run: {why}:\n\n```\n{}\n```\n",
                    tail(out, 30)
                );
            }
            if did_not_run.is_empty() && t.unexplained_failure() {
                let _ = writeln!(
                    r,
                    "`cargo test` failed, but no test failed. The end of its output:\n\n```\n{}\n```\n",
                    tail(&t.run.text, 60)
                );
            }
        }
    }

    let _ = writeln!(r, "## Benchmarks\n");
    let benches = if tier2.is_some() {
        std::fs::read_to_string(&bench_out)
            .map(|t| parse_benches(&t))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if a.quick {
        let _ = writeln!(r, "Skipped (`--quick` builds in debug mode).\n");
    } else if benches.is_empty() {
        let _ = writeln!(
            r,
            "None recorded (benchmarks need the hardware the Tier 2 tests above need).\n"
        );
    } else {
        let _ = writeln!(r, "Release build, one run each.\n");
        let _ = writeln!(r, "| Benchmark | Result |\n|---|---:|");
        for (name, value, unit) in &benches {
            let _ = writeln!(r, "| {name} | {value} {unit} |");
        }
        let _ = writeln!(r);
    }
    let _ = writeln!(r, "## GUI smoke run\n");
    let _ = writeln!(r, "Not available yet (the GUI arrives in M1).\n");

    let _ = writeln!(r, "## Raw environment\n");
    if let Some(dc) = &doctor {
        details(&mut r, "spherecut doctor", dc);
    }
    details(&mut r, "rustc", &rustc);
    details(&mut r, "GPUs (lspci)", &lspci);
    details(&mut r, "vulkaninfo --summary", &vulkan);
    details(&mut r, "vainfo", &vainfo);
    details(&mut r, "ffmpeg -version", &ffmpeg_ver);
    details(&mut r, "ffmpeg -hwaccels", &hwaccels);
    let enc = Captured {
        text: filter_lines(
            &encoders.text,
            &["vaapi", "vulkan", "x264", "x265", "svt", "aom", "prores"],
        ),
        ..encoders
    };
    details(&mut r, "ffmpeg encoders (filtered)", &enc);
    let dec = Captured {
        text: filter_lines(&decoders.text, &["hevc", "h264", "av1", "prores"]),
        ..decoders
    };
    details(&mut r, "ffmpeg decoders (filtered)", &dec);
    if let Some(t) = &tier2 {
        details(&mut r, "Tier 2 test list", &t.list);
        let c = Captured {
            text: tail(&t.run.text, 400),
            cmdline: t.run.cmdline.clone(),
            ok: t.run.ok,
            elapsed: t.run.elapsed,
        };
        details(&mut r, "test output", &c);
    }

    let out = a
        .out
        .unwrap_or_else(|| root.join("hw-reports").join(format!("{host}-{commit}.md")));
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&out, &r).with_context(|| format!("writing {}", out.display()))?;
    eprintln!("==> report written to {}", display_rel(&out, &root));
    if let Some(t) = &tier2 {
        eprintln!("==> {}", t.counts(a.quick));
    }
    if !missing.is_empty() {
        eprintln!(
            "==> {} prerequisite(s) missing; see the report",
            missing.len()
        );
    }
    if !problems.is_empty() {
        bail!("{}; see {}", problems.join("; "), display_rel(&out, &root));
    }
    Ok(())
}

/// Lines of `name\tvalue\tunit` as written by `spherecut_testkit::bench`.
fn parse_benches(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((
                f.next()?.to_string(),
                f.next()?.to_string(),
                f.next()?.to_string(),
            ))
        })
        .collect()
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn filter_lines(text: &str, needles: &[&str]) -> String {
    text.lines()
        .filter(|l| needles.iter().any(|n| l.contains(n)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

fn display_rel(p: &Path, root: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_libtest_output() {
        let out = "\
running 3 tests
test a::passes ... ok
test b::needs_gpu ... FAILED
test c::real_failure ... FAILED
test d::not_tier2 ... ignored, tier1 only

failures:

---- b::needs_gpu stdout ----

thread 'b::needs_gpu' panicked at crates/x/src/lib.rs:1:1:
TIER2-SKIP: no VA-API device at /dev/dri/renderD128

---- c::real_failure stdout ----

thread 'c::real_failure' panicked at crates/x/src/lib.rs:2:1:
assertion failed: psnr > 40.0

failures:
    b::needs_gpu
    c::real_failure

test result: FAILED. 1 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out
";
        let r = parse_libtest(out);
        assert_eq!(r.len(), 3, "{r:?}");
        assert_eq!(r["a::passes"], Outcome::Passed);
        assert_eq!(
            r["b::needs_gpu"],
            Outcome::Skipped("no VA-API device at /dev/dri/renderD128".into())
        );
        assert!(matches!(&r["c::real_failure"], Outcome::Failed(o) if o.contains("psnr")));
    }

    #[test]
    fn skip_marker_must_start_the_panic_message() {
        let out = "\
test a ... FAILED

failures:

---- a stdout ----

thread 'a' panicked at crates/x/src/lib.rs:1:1:
open failed: TIER2-SKIP: is not a reason here

failures:
    a

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
";
        assert!(matches!(parse_libtest(out)["a"], Outcome::Failed(_)));
    }

    #[test]
    fn stray_stderr_does_not_hide_results() {
        // libva and Mesa write to fd 2, into the middle of libtest's lines.
        let out = "\
running 3 tests
test noisy_pass ... libva info: VA-API version 1.22.0
libva info: Trying to open /usr/lib64/dri/radeonsi_drv_video.so
ok
test noisy_fail ... libva info: hi
FAILED
test quiet ... ok

failures:

---- noisy_fail stdout ----

thread 'noisy_fail' (10115) panicked at tests/a.rs:24:5:
boom

failures:
    noisy_fail

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
";
        let r = parse_libtest(out);
        assert_eq!(r.len(), 3);
        assert_eq!(r["noisy_pass"], Outcome::Passed);
        assert_eq!(r["quiet"], Outcome::Passed);
        assert!(matches!(&r["noisy_fail"], Outcome::Failed(o) if o.contains("boom")));
    }

    #[test]
    fn a_crash_fails_the_running_test() {
        let out = "\
running 3 tests
test a_first ... ok
test b_aborts ... error: test failed, to rerun pass `--test crash`

Caused by:
  process didn't exit successfully: `/t/debug/deps/crash-368fcdd4679797b5 --ignored --test-threads=1` (signal: 6, SIGABRT: process abort signal)
";
        let r = parse_libtest(out);
        assert_eq!(r.len(), 2, "c_after never started: {r:?}");
        assert_eq!(r["a_first"], Outcome::Passed);
        assert!(matches!(&r["b_aborts"], Outcome::Failed(o) if o.contains("SIGABRT")));
    }

    /// `cargo test -- --ignored --list` with stdout and stderr merged.
    const LIST: &str = "\
   Compiling spherecut-media v0.1.0 (/w/crates/media)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 9.10s
     Running unittests src/lib.rs (/t/debug/deps/spherecut_media-390c456703c2e029)
0 tests, 0 benchmarks
     Running tests/tier2_samples.rs (/t/debug/deps/tier2_samples-2dfb113bee85abda)
bench_samples_realtime_decode: test
samples_probe_cleanly: test

2 tests, 0 benchmarks
     Running tests/tier2.rs (/t/debug/deps/tier2-1111111111111111)
same_name: test

1 tests, 0 benchmarks
     Running tests/tier2.rs (/t/debug/deps/tier2-2222222222222222)
same_name: test

1 tests, 0 benchmarks
   Doc-tests spherecut_core
0 tests, 0 benchmarks
";

    #[test]
    fn lists_tests_per_binary() {
        let l = parse_list(LIST);
        let names: Vec<&str> = l.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "spherecut_core (doc-tests)",
                "spherecut_media",
                "tier2",
                "tier2-2222222222222222",
                "tier2_samples"
            ]
        );
        assert!(l["spherecut_media"].is_empty());
        assert_eq!(
            l["tier2_samples"],
            ["bench_samples_realtime_decode", "samples_probe_cleanly"]
        );
        assert_eq!(l["tier2"], ["same_name"]);
        assert_eq!(l["tier2-2222222222222222"], ["same_name"]);
    }

    #[test]
    fn splits_run_output_per_binary() {
        // No `\` after the quote: it would eat the first line's indent.
        let run =
            "     Running tests/tier2_samples.rs (/t/debug/deps/tier2_samples-2dfb113bee85abda)

running 1 test
test samples_probe_cleanly ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out

     Running tests/tier2_hw_decode.rs (/t/debug/deps/tier2_hw_decode-e196a90bf238723a)

running 1 test
test samples_probe_cleanly ... FAILED

failures:

---- samples_probe_cleanly stdout ----

thread 'samples_probe_cleanly' panicked at tests/x.rs:1:1:
TIER2-SKIP: no DRM render node at /dev/dri/renderD128

failures:
    samples_probe_cleanly

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
";
        let parts = split_by_binary(run);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "tier2_samples");
        assert_eq!(parts[1].0, "tier2_hw_decode");
        assert_eq!(
            parse_libtest(&parts[0].1)["samples_probe_cleanly"],
            Outcome::Passed
        );
        assert!(matches!(
            parse_libtest(&parts[1].1)["samples_probe_cleanly"],
            Outcome::Skipped(_)
        ));
    }

    fn listed(bins: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        bins.iter()
            .map(|(b, ts)| (b.to_string(), ts.iter().map(|t| t.to_string()).collect()))
            .collect()
    }

    #[test]
    fn merge_reports_a_missing_binary_as_did_not_run() {
        let listed = listed(&[
            ("tier2_samples", &["samples_probe_cleanly", "bench_samples"]),
            ("tier2_hw_decode", &["vaapi_h264", "bench_decode"]),
        ]);
        // tier2_hw_decode never ran; an unlisted result is still kept.
        let ran = BTreeMap::from([(
            "tier2_samples".to_string(),
            BTreeMap::from([
                ("samples_probe_cleanly".to_string(), Outcome::Passed),
                ("surprise".to_string(), Outcome::Failed("x".into())),
            ]),
        )]);
        let full = merge(&listed, &ran, false);
        let expect = [
            ("tier2_hw_decode::bench_decode", Outcome::DidNotRun),
            ("tier2_hw_decode::vaapi_h264", Outcome::DidNotRun),
            ("tier2_samples::bench_samples", Outcome::DidNotRun),
            ("tier2_samples::samples_probe_cleanly", Outcome::Passed),
            ("tier2_samples::surprise", Outcome::Failed("x".into())),
        ];
        let expect: Vec<(String, Outcome)> = expect
            .into_iter()
            .map(|(n, o)| (n.to_string(), o))
            .collect();
        assert_eq!(full, expect);

        // `--quick` leaves the benchmarks out on purpose.
        let quick = merge(&listed, &ran, true);
        assert_eq!(
            quick[0],
            ("tier2_hw_decode::bench_decode".into(), Outcome::LeftOut)
        );
        assert_eq!(quick[1].1, Outcome::DidNotRun);
        assert_eq!(quick[2].1, Outcome::LeftOut);
    }

    fn captured(ok: bool) -> Captured {
        Captured {
            cmdline: "cargo test".into(),
            ok,
            text: String::new(),
            elapsed: Duration::ZERO,
        }
    }

    fn tier2(run_ok: bool, results: &[Outcome]) -> Tier2 {
        Tier2 {
            list: captured(true),
            run: captured(run_ok),
            output: BTreeMap::new(),
            results: results
                .iter()
                .enumerate()
                .map(|(i, o)| (format!("t::{i}"), o.clone()))
                .collect(),
        }
    }

    #[test]
    fn skips_alone_are_not_a_failure() {
        // cargo exits non-zero for a skip, which is not worth a note.
        let t = tier2(false, &[Outcome::Passed, Outcome::Skipped("no GPU".into())]);
        assert!(!t.unexplained_failure());
        assert!(t.problems().is_empty());
        let t = tier2(true, &[Outcome::Passed, Outcome::LeftOut]);
        assert!(t.problems().is_empty());
    }

    #[test]
    fn missing_or_failed_tests_fail_the_run() {
        let t = tier2(
            false,
            &[Outcome::Skipped("no GPU".into()), Outcome::DidNotRun],
        );
        assert_eq!(t.problems(), ["1 Tier 2 test(s) did not run"]);
        assert!(t.counts(false).contains("1 did not run"));
        assert_eq!(t.did_not_run(), BTreeMap::from([("t", 1)]));

        let t = tier2(false, &[Outcome::Failed(String::new())]);
        assert_eq!(t.problems(), ["1 Tier 2 test(s) failed"]);

        let t = tier2(false, &[Outcome::Passed]);
        assert!(t.unexplained_failure());
        assert_eq!(t.problems(), ["`cargo test` failed without a failing test"]);

        let mut t = tier2(true, &[Outcome::Passed]);
        t.list.ok = false;
        assert_eq!(t.problems().len(), 1);
    }

    #[test]
    fn reads_target_directory_from_cargo_metadata() {
        let json = r#"{"packages":[],"target_directory":"/a/b \"q\"\\c\u00e9","version":1}"#;
        assert_eq!(
            json_str_field(json, "target_directory").as_deref(),
            Some("/a/b \"q\"\\cé")
        );
        assert_eq!(json_str_field(json, "workspace_root"), None);
    }
}
