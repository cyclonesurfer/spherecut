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
//! first, shows up as "did not run" and fails the run. So does a binary
//! that dies after its tests reported: each binary's exit is judged on its
//! own, since a skip anywhere already makes `cargo test` fail. The list and
//! the run are read from cargo's and libtest's text output, so cargo runs
//! with fixed terminal settings rather than the user's. `--only` runs the
//! tests whose report names contain its names, and lists the rest as left
//! out, as `--quick` lists the benchmarks.
//!
//! After Tier 2, the Tier 1 tests run as `cargo xtask check` runs them
//! (`--no-tier1` leaves them out), which catches what this machine's
//! FFmpeg and Mesa do differently. Before Tier 2, hw-test makes the perf
//! clips the playback benchmarks play, once (`perf_clips`), unless
//! `--quick`. The report also records the power settings and every render
//! node (`machine`). `--list` lists the Tier 2 tests and stops.
//!
//! The command exits non-zero when the build fails, a Tier 2 or Tier 1
//! test fails or does not run, or a test binary dies; skips alone are not
//! a failure. The report is written either way.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::machine::{self, Access, MIN_RUST};
use crate::perf_clips::{self, Clip, Made};
use crate::workspace_root;

/// What a skipped test's panic message starts with
/// (`spherecut_testkit::tier2::SKIP_MARKER`).
const SKIP_MARKER: &str = "TIER2-SKIP:";

/// `--quick` leaves out tests whose names contain this: debug-build timings
/// would only mislead.
const BENCH: &str = "bench_";

/// Cargo and libtest settings that reading their output depends on, over
/// the user's own (`term.verbose` or `term.quiet` in a cargo config, or the
/// `CARGO_TERM_*` variables). Verbose cargo prints the test command instead
/// of `Running <src> (<exe>)`; quiet cargo prints no `Running` lines and
/// has libtest print one character per test. `RUST_TEST_NOCAPTURE` would
/// move a failing test's output out of its `---- <name> stdout ----`
/// section.
const PLAIN_OUTPUT: [(&str, &str); 5] = [
    ("CARGO_TERM_VERBOSE", "false"),
    ("CARGO_TERM_QUIET", "false"),
    ("CARGO_TERM_COLOR", "never"),
    ("CARGO_TERM_PROGRESS_WHEN", "never"),
    ("RUST_TEST_NOCAPTURE", "0"),
];

#[derive(clap::Args)]
pub struct Args {
    /// Directory with camera samples (default: `samples/` in the workspace root).
    #[arg(long)]
    samples: Option<PathBuf>,
    /// Build in debug mode and skip the benchmarks (environment and tests only).
    #[arg(long)]
    quick: bool,
    /// Where to write the report (default: hw-reports/<host>-<commit>.md).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Leave out the Tier 1 tests on this machine.
    #[arg(long)]
    no_tier1: bool,
    /// Run only the Tier 2 tests whose names, as `--list` prints them,
    /// contain this (repeatable); the rest are listed as left out.
    #[arg(long, value_name = "NAME")]
    only: Vec<String>,
    /// List the Tier 2 tests a run would run, by test binary, and stop
    /// (a debug build, as `--quick` makes).
    #[arg(long)]
    list: bool,
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
/// stream says which binary a result came from. Runs in `dir`. The
/// variables in `env` reach the child as they are, paths that are not
/// UTF-8 included.
fn capture_merged<V: AsRef<OsStr>>(
    dir: &Path,
    program: &str,
    args: &[&str],
    env: &[(&str, V)],
) -> Captured {
    let cmdline = cmdline(program, args);
    let start = Instant::now();
    let run = || -> std::io::Result<(bool, String)> {
        let (mut reader, writer) = std::io::pipe()?;
        // The Command is a temporary, so its copies of the write end close
        // once the child has started and the read below ends at EOF.
        let mut child = Command::new(program)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v.as_ref())))
            .current_dir(dir)
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

fn check_prereqs(d: Distro, ffmpeg_info: &str, vainfo: &str, rustc: &str) -> Vec<Missing> {
    let mut m = Vec::new();
    if let Some(v) = machine::rustc_version(rustc)
        && v < MIN_RUST
    {
        m.push(Missing {
            what: format!(
                "rustc {}.{} or newer (this is {}.{})",
                MIN_RUST.0, MIN_RUST.1, v.0, v.1
            ),
            fix: if have("rustup") {
                "rustup update stable".into()
            } else {
                match d {
                    Distro::Fedora => "sudo dnf upgrade --refresh rust cargo".into(),
                    Distro::Arch => "sudo pacman -Syu rust".into(),
                    Distro::Other => {
                        "install rustup (https://rustup.rs) and its stable toolchain".into()
                    }
                }
            },
            blocks: "building Spherecut",
        });
    }
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
    // The Tier 1 strace tests (D24) fail without it, and `cargo xtask
    // check` stops before anything else.
    if !have("strace") {
        m.push(Missing {
            what: "strace".into(),
            fix: pkg_cmd(d, "strace", "strace"),
            blocks: "`cargo xtask check` (Tier 1 strace tests)",
        });
    }
    if machine::lavapipe_icd(Path::new("/")).is_none() {
        m.push(Missing {
            what: "lavapipe, Mesa's Vulkan CPU driver (no lvp_icd*.json)".into(),
            fix: pkg_cmd(d, "mesa-vulkan-drivers", "vulkan-swrast"),
            blocks: "the Tier 1 render and playback tests",
        });
    }
    // cpal's ALSA host, always built on Linux, links libasound (D35).
    if !capture("pkg-config", &["--exists", "alsa"]).ok {
        m.push(Missing {
            what: "the ALSA development headers".into(),
            fix: pkg_cmd(d, "alsa-lib-devel", "alsa-lib"),
            blocks: "building Spherecut",
        });
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
    /// Left out on purpose, by the flag named: a benchmark by `--quick`,
    /// or a test `--only` didn't pick.
    LeftOut(&'static str),
    /// Listed, but no result came back.
    DidNotRun,
}

/// A test binary in `cargo test` output.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Binary {
    /// A test executable, as its `Running` line gives it (relative to the
    /// directory cargo runs in, if it is inside). The list and the run give
    /// an executable the same path, whichever other binaries ran.
    Exe(String),
    /// The doc tests of a library crate.
    Doc(String),
}

/// Splits `cargo test` output into one part per test binary. Text before
/// the first binary (compiler output) is dropped.
fn split_by_binary(output: &str) -> Vec<(Binary, String)> {
    let mut parts: Vec<(Binary, String)> = Vec::new();
    for line in output.lines() {
        // Cargo indents its status lines; test output that happens to say
        // "Running" usually isn't.
        let l = line.trim_start();
        let bin = if l.len() == line.len() {
            None
        } else if let Some(r) = l.strip_prefix("Running ") {
            r.strip_suffix(')')
                .and_then(|r| r.rsplit_once(" ("))
                .map(|(_, exe)| Binary::Exe(exe.to_string()))
        } else {
            l.strip_prefix("Doc-tests ")
                .map(|c| Binary::Doc(c.trim().to_string()))
        };
        match bin {
            Some(bin) => parts.push((bin, String::new())),
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

/// Report names for test binaries, given once for the list and the run: the
/// executable's name without Cargo's hash (`tier2_samples`), or
/// `<package>/<name>` when binaries in two packages have that name, so that
/// reports from different builds line up. Doc tests are `<crate>
/// (doc-tests)`. `packages` gives each executable's package, by its
/// absolute path; `dir` is where cargo ran.
fn binary_names<'a>(
    bins: impl IntoIterator<Item = &'a Binary>,
    packages: &BTreeMap<PathBuf, String>,
    dir: &Path,
) -> BTreeMap<Binary, String> {
    let file = |exe: &str| exe.rsplit('/').next().unwrap_or(exe).to_string();
    let mut names: BTreeMap<Binary, String> = bins
        .into_iter()
        .map(|b| {
            let name = match b {
                Binary::Exe(exe) => strip_hash(&file(exe)).to_string(),
                Binary::Doc(krate) => format!("{krate} (doc-tests)"),
            };
            (b.clone(), name)
        })
        .collect();
    // Two packages can each have a `tests/foo.rs`. Name those after their
    // package, and keep Cargo's hash only if that is not enough.
    for qualify in [true, false] {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for n in names.values() {
            *seen.entry(n.clone()).or_default() += 1;
        }
        for (b, n) in &mut names {
            if let Binary::Exe(exe) = b
                && seen.get(n.as_str()).is_some_and(|&c| c > 1)
            {
                *n = match packages.get(&dir.join(exe)) {
                    Some(package) if qualify => format!("{package}/{n}"),
                    _ => file(exe),
                };
            }
        }
    }
    names
}

/// Rekeys a per-binary map by report name.
fn by_name<T>(m: BTreeMap<Binary, T>, names: &BTreeMap<Binary, String>) -> BTreeMap<String, T> {
    m.into_iter()
        .filter_map(|(b, v)| Some((names.get(&b)?.clone(), v)))
        .collect()
}

/// The package of each test executable, by its absolute path, from the
/// `compiler-artifact` messages of `cargo test --no-run
/// --message-format=json`. Cargo prints each message as one line.
fn test_packages(output: &str) -> BTreeMap<PathBuf, String> {
    output
        .lines()
        .filter(|l| {
            l.starts_with('{')
                && json_str_field(l, "reason").as_deref() == Some("compiler-artifact")
        })
        .filter_map(|l| {
            let exe = json_str_field(l, "executable")?;
            let id = json_str_field(l, "package_id")?;
            Some((PathBuf::from(exe), package_name(&id)?.to_string()))
        })
        .collect()
}

/// The name in a package ID spec as cargo's JSON gives it:
/// `path+file:///w/crates/media#spherecut-media@0.1.0`, or
/// `path+file:///w/xtask#0.1.0` when the name is the directory's.
fn package_name(id: &str) -> Option<&str> {
    let (url, fragment) = id.rsplit_once('#')?;
    match fragment.split_once('@') {
        Some((name, _version)) => Some(name),
        None => url.split('?').next()?.rsplit('/').next(),
    }
}

/// The tests in each binary, from `cargo test -- --ignored --list`.
fn parse_list(output: &str) -> BTreeMap<Binary, Vec<String>> {
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

/// How a test binary exited, from what cargo printed after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Exit {
    /// Cargo printed nothing: exit status 0.
    #[default]
    Clean,
    /// `error: test failed, to rerun pass …` alone: libtest's exit status
    /// when a test failed (101), which a skip is too.
    TestsFailed,
    /// A signal, another exit status, or no start at all, as cargo puts it
    /// (`signal: 6, SIGABRT: process abort signal`).
    Abnormal(String),
}

/// One test binary's part of the run.
#[derive(Debug, Default)]
struct BinaryRun {
    /// What the binary printed, then what cargo printed about it.
    output: String,
    results: BTreeMap<String, Outcome>,
    /// libtest printed its `test result:` line.
    finished: bool,
    exit: Exit,
}

impl BinaryRun {
    /// What went wrong with the binary itself, beyond its tests' results.
    /// libtest prints `test result:` and then exits 0, or 101 if a test
    /// failed or skipped; anything else means the binary died, which its
    /// test list alone can hide (all passed, or a skip that explains a 101).
    fn problem(&self) -> Option<String> {
        let failed = self
            .results
            .values()
            .any(|o| matches!(o, Outcome::Failed(_) | Outcome::Skipped(_)));
        match &self.exit {
            Exit::Abnormal(how) if !self.finished => Some(format!("did not finish ({how})")),
            _ if !self.finished => Some("did not finish".to_string()),
            Exit::Abnormal(how) => Some(format!(
                "exited abnormally after its tests finished ({how})"
            )),
            Exit::TestsFailed if !failed => Some("failed, but none of its tests did".to_string()),
            _ => None,
        }
    }
}

/// One binary's part of the run: its libtest output (run with
/// `--test-threads=1`), then what cargo printed when it exited.
///
/// Libraries such as libva write straight to stderr, which can split a
/// `test name ... ok` line, so a result is also taken from a bare `ok` or
/// `FAILED` line, from the failure sections and from the `failures:` list
/// at the end. A test that started but has no confirmed result failed: its
/// binary stopped (crashed, aborted, was killed) while it ran, and what
/// follows in the output says how. Until libtest's summary, only the next
/// test starting confirms an `ok`, since a library can print one too.
fn parse_run(output: &str) -> BinaryRun {
    let lines: Vec<&str> = output.lines().collect();
    // (name, status, line index), in the order they started.
    let mut started: Vec<(String, String, usize)> = Vec::new();
    let mut sections: BTreeMap<String, String> = BTreeMap::new();
    let mut listed: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut in_list = false;
    let mut finished = false;
    let mut exit = Exit::Clean;
    for (i, &line) in lines.iter().enumerate() {
        // Cargo's error once the binary has exited. It can end a line that
        // libtest started (`test b ... error: test failed, …`).
        let line = match line.find("error: ") {
            Some(at) if line[at..].contains(" failed, to rerun pass `") => {
                exit = Exit::TestsFailed;
                current = None;
                &line[..at]
            }
            _ => line,
        };
        // Then, unless libtest's exit status was all it had to say, why.
        let cause = line
            .trim_start()
            .strip_prefix("process didn't exit successfully: ")
            .or_else(|| line.trim_start().strip_prefix("could not execute process "))
            .filter(|_| exit != Exit::Clean);
        if let Some(cause) = cause {
            // "`<command>` (<status>)", and a status can have parentheses.
            let how = cause.rsplit_once('`').map_or(cause, |(_, how)| how).trim();
            let how = how
                .strip_prefix('(')
                .and_then(|h| h.strip_suffix(')'))
                .unwrap_or(how);
            exit = Exit::Abnormal(how.to_string());
        } else if let Some(name) = line
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
            // Of two results, one is a library's; FAILED is the safe one.
            if let Some((_, status, _)) = started.last_mut()
                && (line == "FAILED" || !matches!(status.as_str(), "ok" | "FAILED"))
            {
                *status = line.to_string();
            }
        } else if in_list && let Some(name) = line.strip_prefix("    ") {
            listed.push(name.trim().to_string());
        }
    }
    let mut results = BTreeMap::new();
    for (k, (name, status, at)) in started.iter().enumerate() {
        let outcome = if let Some(text) = sections.get(name) {
            failure(text)
        } else if listed.contains(name) || (finished && status == "FAILED") {
            Outcome::Failed(String::new())
        } else if status.starts_with("ignored") {
            // Not run, so no result (`--ignored` runs only these, though).
            continue;
        } else if finished || (status == "ok" && k + 1 < started.len()) {
            // A failed test would be in the `failures:` list.
            Outcome::Passed
        } else {
            Outcome::Failed(format!(
                "The test binary stopped before it confirmed this test's result:\n\n{}",
                lines[*at..].join("\n")
            ))
        };
        results.insert(name.clone(), outcome);
    }
    for (name, text) in &sections {
        results.entry(name.clone()).or_insert_with(|| failure(text));
    }
    for name in listed {
        results
            .entry(name)
            .or_insert(Outcome::Failed(String::new()));
    }
    BinaryRun {
        output: output.to_string(),
        results,
        finished,
        exit,
    }
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

/// Which Tier 2 tests a run means to run: `--quick` leaves out the
/// benchmarks, and `--only` keeps the tests it names.
#[derive(Default)]
struct Selection {
    quick: bool,
    only: Vec<String>,
}

impl Selection {
    /// The flag that leaves out test `test` of binary `bin`, if one does.
    fn left_out(&self, bin: &str, test: &str) -> Option<&'static str> {
        let name = format!("{bin}::{test}");
        if self.quick && test.contains(BENCH) {
            Some("`--quick`")
        } else if !self.only.is_empty() && !self.only.iter().any(|f| name.contains(f.as_str())) {
            Some("`--only`")
        } else {
            None
        }
    }

    /// The `--only` names that pick no listed test.
    fn unmatched(&self, listed: &BTreeMap<String, Vec<String>>) -> Vec<String> {
        self.only
            .iter()
            .filter(|f| {
                !listed.iter().any(|(bin, tests)| {
                    tests
                        .iter()
                        .any(|t| format!("{bin}::{t}").contains(f.as_str()))
                })
            })
            .cloned()
            .collect()
    }
}

/// Every listed test exactly once, as `binary::test` in name order, with how
/// it ended. A listed test without a result did not run. One `sel` leaves
/// out is left out even if it ran: libtest runs a picked test's namesakes
/// in other binaries too. Results for tests that were not listed are kept.
fn merge(
    listed: &BTreeMap<String, Vec<String>>,
    ran: &BTreeMap<String, BinaryRun>,
    sel: &Selection,
) -> Vec<(String, Outcome)> {
    let mut all: BTreeMap<(&str, &str), Outcome> = BTreeMap::new();
    for (bin, tests) in listed {
        for t in tests {
            let o = match sel.left_out(bin, t) {
                Some(flag) => Outcome::LeftOut(flag),
                None => ran
                    .get(bin)
                    .and_then(|r| r.results.get(t))
                    .cloned()
                    .unwrap_or(Outcome::DidNotRun),
            };
            all.insert((bin.as_str(), t.as_str()), o);
        }
    }
    for (bin, run) in ran {
        for (t, o) in &run.results {
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
    /// The tests in each binary, from the list, by report name.
    listed: BTreeMap<String, Vec<String>>,
    run: Captured,
    /// Each test binary's part of the run, by report name.
    binaries: BTreeMap<String, BinaryRun>,
    /// Every Tier 2 test once.
    results: Vec<(String, Outcome)>,
    /// The `--only` names that picked no test.
    unmatched: Vec<String>,
}

impl Tier2 {
    /// Pairs every test in the list with its outcome in the run. `packages`
    /// and `dir` are for [`binary_names`].
    fn new(
        list: Captured,
        run: Captured,
        packages: &BTreeMap<PathBuf, String>,
        dir: &Path,
        sel: &Selection,
    ) -> Tier2 {
        let listed = parse_list(&list.text);
        let output: BTreeMap<Binary, String> = split_by_binary(&run.text).into_iter().collect();
        let names = binary_names(listed.keys().chain(output.keys()), packages, dir);
        let listed = by_name(listed, &names);
        let binaries = by_name(output, &names)
            .into_iter()
            .map(|(bin, text)| (bin, parse_run(&text)))
            .collect();
        let results = merge(&listed, &binaries, sel);
        let unmatched = sel.unmatched(&listed);
        Tier2 {
            list,
            listed,
            run,
            binaries,
            results,
            unmatched,
        }
    }

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

    /// A note for each binary with tests that did not run or a problem of
    /// its own: what happened, and the output that shows why.
    fn binary_notes(&self) -> Vec<(String, &str)> {
        let did_not_run = self.did_not_run();
        let mut notes = Vec::new();
        for (bin, n) in &did_not_run {
            if !self.binaries.contains_key(*bin) {
                notes.push((
                    format!(
                        "{n} test(s) in `{bin}` did not run: cargo never ran it. The end of the test output"
                    ),
                    self.run.text.as_str(),
                ));
            }
        }
        for (bin, b) in &self.binaries {
            let what = match (did_not_run.get(bin.as_str()), b.problem()) {
                (Some(n), Some(why)) => format!("{n} test(s) in `{bin}` did not run, and it {why}"),
                (Some(n), None) => {
                    format!("{n} test(s) in `{bin}` did not run, although it finished")
                }
                (None, Some(why)) => format!("`{bin}` {why}"),
                (None, None) => continue,
            };
            notes.push((format!("{what}. The end of its output"), b.output.as_str()));
        }
        notes
    }

    /// `cargo test` failed, but every binary that ran exited cleanly and no
    /// test is missing, so the failure is somewhere else.
    fn unexplained_failure(&self) -> bool {
        !self.run.ok
            && self.binaries.values().all(|b| b.exit == Exit::Clean)
            && self.count(|o| *o == Outcome::DidNotRun) == 0
    }

    /// Why the Tier 2 part failed; empty if it did not. Skips alone are fine.
    fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        if !self.list.ok {
            p.push("listing the Tier 2 tests failed, so some may be missing".to_string());
        } else if self.listed.is_empty() {
            // Then cargo's output was not what this module reads.
            p.push("the listing found no test binaries".to_string());
        } else if self.listed.values().all(Vec::is_empty) {
            p.push("the listing found no Tier 2 tests".to_string());
        }
        let failed = self.count(|o| matches!(o, Outcome::Failed(_)));
        if failed > 0 {
            p.push(format!("{failed} Tier 2 test(s) failed"));
        }
        let missing = self.count(|o| *o == Outcome::DidNotRun);
        if missing > 0 {
            p.push(format!("{missing} Tier 2 test(s) did not run"));
        }
        for f in &self.unmatched {
            p.push(format!("`--only {f}` picks no Tier 2 test"));
        }
        // Judged per binary: a skip in one binary explains only its own
        // failure to cargo, not another binary's crash.
        for (bin, b) in &self.binaries {
            if let Some(why) = b.problem() {
                p.push(format!("`{bin}` {why}"));
            }
        }
        if self.unexplained_failure() {
            p.push("`cargo test` failed, but no test binary did".to_string());
        }
        p
    }

    /// One line with every count, so nothing missing can look clean.
    fn counts(&self) -> String {
        let mut by: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, o) in &self.results {
            if let Outcome::LeftOut(flag) = o {
                *by.entry(flag).or_default() += 1;
            }
        }
        let left_out: String = by
            .iter()
            .map(|(flag, n)| format!("{n} left out by {flag}, "))
            .collect();
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

/// Lists the workspace's Tier 2 tests, runs them all with what they need
/// from hw-test, and pairs every listed test with its outcome.
fn run_tier2(
    sel: &Selection,
    samples: &Path,
    bench_out: &Path,
    gui_out: &Path,
    perf_media: Option<&Path>,
) -> Tier2 {
    // A stale file would mix in another run's numbers.
    if let Err(e) = std::fs::remove_file(bench_out)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("warning: could not clear {}: {e}", bench_out.display());
    }
    // And another run's GUI smoke runs into this report.
    if let Err(e) = std::fs::remove_dir_all(gui_out)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("warning: could not clear {}: {e}", gui_out.display());
    }
    list_and_run(
        &workspace_root(),
        sel,
        &tier2_env(samples, bench_out, gui_out, perf_media),
    )
}

/// What the Tier 2 tests get from hw-test, and the perf clips' directory
/// when it made them. The paths go as they are, so that the tests look at
/// the same directory as the report.
fn tier2_env<'a>(
    samples: &'a Path,
    bench_out: &'a Path,
    gui_out: &'a Path,
    perf_media: Option<&'a Path>,
) -> Vec<(&'static str, &'a OsStr)> {
    let mut env = vec![
        ("SPHERECUT_TIER2", OsStr::new("1")),
        ("SPHERECUT_SAMPLES", samples.as_os_str()),
        ("SPHERECUT_BENCH_OUT", bench_out.as_os_str()),
        ("SPHERECUT_GUI_OUT", gui_out.as_os_str()),
    ];
    if let Some(dir) = perf_media {
        env.push(("SPHERECUT_PERF_MEDIA", dir.as_os_str()));
    }
    env
}

/// `env` and [`PLAIN_OUTPUT`], for every cargo command hw-test reads.
fn plain_env<'a, V: AsRef<OsStr>>(env: &'a [(&'a str, V)]) -> Vec<(&'a str, &'a OsStr)> {
    env.iter()
        .map(|(k, v)| (*k, v.as_ref()))
        .chain(PLAIN_OUTPUT.iter().map(|&(k, v)| (k, OsStr::new(v))))
        .collect()
}

/// `cargo test` for the workspace in the given profile: `--no-fail-fast`,
/// since a skip is a test failure to cargo, and without it cargo stops
/// after the first binary that has one.
fn cargo_test(quick: bool) -> Vec<&'static str> {
    let mut cargo = vec!["test", "--workspace", "--no-fail-fast", "--color", "never"];
    if !quick {
        cargo.push("--release");
    }
    cargo
}

/// The package of each test executable `cargo` (a `cargo test` command
/// line) builds: the `Running` lines name only the executable.
fn packages_of(dir: &Path, cargo: &[&str], env: &[(&str, &OsStr)]) -> BTreeMap<PathBuf, String> {
    let mut args = cargo.to_vec();
    args.extend(["--no-run", "--message-format=json-render-diagnostics"]);
    test_packages(&capture_merged(dir, "cargo", &args, env).text)
}

/// `cargo test -- --ignored --list` in `dir`, and the packages of its
/// test executables.
fn list_tier2(
    dir: &Path,
    quick: bool,
    env: &[(&str, &OsStr)],
) -> (Captured, BTreeMap<PathBuf, String>) {
    let cargo = cargo_test(quick);
    let packages = packages_of(dir, &cargo, env);
    let mut args = cargo;
    args.extend(["--", "--ignored", "--list", "--format", "pretty"]);
    (capture_merged(dir, "cargo", &args, env), packages)
}

/// `--list`: every Tier 2 test as `binary::test`, as the report names
/// them, from a debug build.
pub(crate) fn listed_tests(dir: &Path) -> Result<Vec<String>> {
    let env = plain_env::<&str>(&[]);
    let (list, packages) = list_tier2(dir, true, &env);
    if !list.ok {
        bail!("`{}` failed:\n{}", list.cmdline, tail(&list.text, 60));
    }
    let listed = parse_list(&list.text);
    let names = binary_names(listed.keys(), &packages, dir);
    Ok(by_name(listed, &names)
        .into_iter()
        .flat_map(|(bin, tests)| tests.into_iter().map(move |t| format!("{bin}::{t}")))
        .collect())
}

/// Lists the ignored tests of the cargo workspace in `dir`, runs them, and
/// pairs every listed test with its outcome. Every cargo command gets
/// `env`, so that they all build the same way, and [`PLAIN_OUTPUT`].
fn list_and_run<V: AsRef<OsStr>>(dir: &Path, sel: &Selection, env: &[(&str, V)]) -> Tier2 {
    let env = plain_env(env);
    eprintln!("==> listing Tier 2 tests");
    let (list, packages) = list_tier2(dir, sel.quick, &env);

    // `--only` matches the names the report gives, which libtest doesn't
    // know, so libtest gets the picked tests' own names, exactly.
    let mut picked = Vec::new();
    if !sel.only.is_empty() {
        let listed = parse_list(&list.text);
        let names = binary_names(listed.keys(), &packages, dir);
        for (bin, tests) in by_name(listed, &names) {
            for t in tests {
                if sel.left_out(&bin, &t).is_none() && !picked.contains(&t) {
                    picked.push(t);
                }
            }
        }
    }
    let run = if !sel.only.is_empty() && picked.is_empty() {
        eprintln!("==> no Tier 2 test to run");
        Captured {
            cmdline: "(none: `--only` picked no test)".into(),
            ok: true,
            text: String::new(),
            elapsed: Duration::ZERO,
        }
    } else {
        eprintln!("==> running Tier 2 tests");
        let mut args = cargo_test(sel.quick);
        args.extend(["--", "--ignored", "--test-threads=1", "--format", "pretty"]);
        if sel.quick {
            args.extend(["--skip", BENCH]);
        }
        if !picked.is_empty() {
            args.push("--exact");
            args.extend(picked.iter().map(String::as_str));
        }
        capture_merged(dir, "cargo", &args, &env)
    };
    Tier2::new(list, run, &packages, dir, sel)
}

/// Tier 1 on this machine: `cargo test --workspace --all-targets`, in the
/// debug build, as `cargo xtask check` runs it (M1 plan section 12.4).
struct Tier1 {
    run: Captured,
    /// Each test binary's part of the run, by report name.
    binaries: BTreeMap<String, BinaryRun>,
}

impl Tier1 {
    fn new(run: Captured, packages: &BTreeMap<PathBuf, String>, dir: &Path) -> Tier1 {
        let output: BTreeMap<Binary, String> = split_by_binary(&run.text).into_iter().collect();
        let names = binary_names(output.keys(), packages, dir);
        let binaries = by_name(output, &names)
            .into_iter()
            .map(|(bin, text)| (bin, parse_run(&text)))
            .collect();
        Tier1 { run, binaries }
    }

    /// Every test with a result, as `binary::test`.
    fn results(&self) -> impl Iterator<Item = (String, &Outcome)> {
        self.binaries.iter().flat_map(|(bin, b)| {
            b.results
                .iter()
                .map(move |(t, o)| (format!("{bin}::{t}"), o))
        })
    }

    fn passed(&self) -> usize {
        self.results()
            .filter(|(_, o)| **o == Outcome::Passed)
            .count()
    }

    /// The tests that failed, with their output. A Tier 1 test that skips
    /// has failed: only Tier 2 tests may skip.
    fn failed(&self) -> Vec<(String, String)> {
        self.results()
            .filter_map(|(n, o)| match o {
                Outcome::Failed(out) => Some((n, out.clone())),
                Outcome::Skipped(why) => Some((
                    n,
                    format!("{SKIP_MARKER} {why}\n(a Tier 1 test may not skip)"),
                )),
                _ => None,
            })
            .collect()
    }

    /// Why Tier 1 failed here; empty if it did not.
    fn problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        let failed = self.failed().len();
        if failed > 0 {
            p.push(format!("{failed} Tier 1 test(s) failed on this machine"));
        }
        for (bin, b) in &self.binaries {
            if let Some(why) = b.problem() {
                p.push(format!("Tier 1 `{bin}` {why}"));
            }
        }
        if self.binaries.is_empty() {
            p.push("the Tier 1 run ran no test binaries".to_string());
        } else if !self.run.ok && p.is_empty() {
            p.push("the Tier 1 `cargo test` failed, but no test binary did".to_string());
        }
        p
    }

    fn counts(&self) -> String {
        format!(
            "Tier 1 on this machine: {} passed, {} failed, in {} test binaries ({:.0} s)",
            self.passed(),
            self.failed().len(),
            self.binaries.len(),
            self.run.elapsed.as_secs_f64()
        )
    }
}

/// Runs the Tier 1 tests of the cargo workspace in `dir`, with `env` and
/// [`PLAIN_OUTPUT`].
fn run_tier1<V: AsRef<OsStr>>(dir: &Path, env: &[(&str, V)]) -> Tier1 {
    let env = plain_env(env);
    let cargo = vec![
        "test",
        "--workspace",
        "--all-targets",
        "--no-fail-fast",
        "--color",
        "never",
    ];
    eprintln!("==> running Tier 1 tests");
    let packages = packages_of(dir, &cargo, &env);
    let mut args = cargo;
    args.extend(["--", "--format", "pretty"]);
    Tier1::new(capture_merged(dir, "cargo", &args, &env), &packages, dir)
}

/// Software decode faster than this marks a perf clip "too easy" (M1 plan
/// section 12.4): the clean grid decoded at 330 fps, the X5's own 5.7K60
/// at about 58.
const TOO_EASY_FPS: f64 = 150.0;

/// The perf clips section's table rows: each clip's size, bitrate, how it
/// came to be there, and its software decode rate from the benchmarks,
/// flagged above [`TOO_EASY_FPS`]. A pair has a row a lens, with the
/// lens's own samples, bitrate and decode rate, so that each track is
/// flagged on its own.
fn perf_clip_rows(clips: &[Clip], benches: &[Bench]) -> Vec<String> {
    let decode = |row: String| match benches.iter().find(|b| b.name == row) {
        Some(b) if b.median > TOO_EASY_FPS => format!(
            "{:.1} fps, **too easy** (over {TOO_EASY_FPS:.0}: it proves nothing about footage)",
            b.median
        ),
        Some(b) => format!("{:.1} fps", b.median),
        None => "not measured".to_string(),
    };
    let size = |bytes: u64| {
        if bytes > 0 {
            format!("{:.0} MB", bytes as f64 / 1e6)
        } else {
            String::new()
        }
    };
    let mbps = |m: Option<f64>| m.map_or(String::new(), |m| format!("{m:.1} Mbit/s"));
    let mut rows = Vec::new();
    for c in clips {
        let file = c
            .path
            .file_name()
            .map_or_else(|| c.name.clone(), |f| f.to_string_lossy().into_owned());
        let made = match &c.made {
            Made::Cached => "cached".to_string(),
            Made::Now(d) => format!("now, in {:.0} s", d.as_secs_f64()),
            Made::Failed(why) => format!("**not made**: {why}"),
        };
        if c.track_bytes.is_empty() {
            let row = format!("perf clip {}: software decode", c.name);
            rows.push(format!(
                "| `{file}` | {} | {} | {made} | {} |",
                size(c.bytes),
                mbps(c.mbps()),
                decode(row)
            ));
            continue;
        }
        for (k, &bytes) in c.track_bytes.iter().enumerate() {
            let row = format!("perf clip {} lens {k}: software decode", c.name);
            rows.push(format!(
                "| `{file}` lens {k} | {} | {} | {made} | {} |",
                size(bytes),
                mbps(c.track_mbps(k)),
                decode(row)
            ));
        }
    }
    rows
}

/// Cargo's target directory as `cargo metadata` gives it, which follows
/// `CARGO_TARGET_DIR` and `build.target-dir`.
pub(crate) fn target_dir(root: &Path) -> PathBuf {
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

/// The string value of the first `"key":` in compact JSON such as cargo
/// prints; enough to read a field whose key appears once (or first at the
/// top level) without a JSON parser.
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
    if a.list {
        let tests = listed_tests(&root)?;
        for t in &tests {
            println!("{t}");
        }
        eprintln!("==> {} Tier 2 tests", tests.len());
        return Ok(());
    }
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
    let gui_out = target.join("hw-gui");
    let report_path = a
        .out
        .clone()
        .unwrap_or_else(|| root.join("hw-reports").join(format!("{host}-{commit}.md")));

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
    // Where the GUI smoke run opens its window: as gui_smoke finds it.
    let display = match spherecut_testkit::tier2::graphical_session(
        |k| std::env::var_os(k),
        spherecut_testkit::tier2::own_uid(),
    ) {
        Ok(s) => format!(
            "{}: {}",
            s.found.describe(),
            s.env
                .iter()
                .map(|(k, v)| format!("{k}={}", v.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Err(why) => why,
    };
    let pactl = have("pactl").then(|| capture("pactl", &["info"]));
    let audio_server = pactl
        .as_ref()
        .filter(|c| c.ok)
        .and_then(|c| first_line_containing(&c.text, "Server Name:"))
        .map_or_else(
            || "unknown (pactl unavailable or failed)".to_string(),
            |l| l.trim_start_matches("Server Name:").trim().to_string(),
        );
    let mutter = have("gsettings").then(|| {
        capture(
            "gsettings",
            &["get", "org.gnome.mutter", "experimental-features"],
        )
    });
    let hyprland = have("hyprctl").then(|| {
        (
            capture("hyprctl", &["version"]),
            capture("hyprctl", &["monitors"]),
        )
    });
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

    let profiles_daemon = have("powerprofilesctl")
        .then(|| capture("powerprofilesctl", &["get"]))
        .filter(|c| c.ok)
        .map(|c| c.text);
    let power = machine::power_profile(Path::new("/"), profiles_daemon.as_deref());
    let nodes = machine::render_nodes(Path::new("/"), Path::new("/dev/dri"));

    eprintln!("==> checking prerequisites");
    let missing = check_prereqs(
        d,
        &decoders.text,
        if vainfo.ok { &vainfo.text } else { "" },
        &rustc.text,
    );
    let build_blocked = missing.iter().any(|m| m.blocks == "building Spherecut");

    let mut build: Option<Captured> = None;
    let mut tier2: Option<Tier2> = None;
    let mut tier1: Option<Tier1> = None;
    let mut doctor: Option<Captured> = None;
    let mut clips: Vec<Clip> = Vec::new();
    let media_dir = target.join("hw-media");
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
            // The benchmarks that play them are left out by --quick.
            if !a.quick {
                clips = perf_clips::ensure(&media_dir);
            }
            let perf_media = (!a.quick).then_some(media_dir.as_path());
            let sel = Selection {
                quick: a.quick,
                only: a.only.clone(),
            };
            tier2 = Some(run_tier2(&sel, &samples, &bench_out, &gui_out, perf_media));
            let exe = target.join(profile).join("spherecut");
            doctor = Some(capture(&exe.to_string_lossy(), &["doctor"]));
            if !a.no_tier1 {
                tier1 = Some(run_tier1::<&str>(&root, &[]));
            }
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
    for c in &clips {
        if let Made::Failed(_) = c.made {
            problems.push(format!("the perf clip {} could not be made", c.name));
        }
    }
    if let Some(t) = &tier1 {
        problems.extend(t.problems());
    }

    // ---- report ----
    let mut r = String::new();
    let _ = writeln!(r, "# Hardware report: {host} @ {commit}\n");
    let flags: String = a
        .quick
        .then_some(" --quick".to_string())
        .into_iter()
        .chain(a.only.iter().map(|f| format!(" --only {f}")))
        .collect();
    let _ = writeln!(
        r,
        "Generated by `cargo xtask hw-test{flags}` in {:.0} s.\n",
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
    let _ = writeln!(r, "| GUI display | {display} |");
    let _ = writeln!(
        r,
        "| Screen locked at the end | {} |",
        match spherecut_testkit::tier2::session_locked() {
            Some(true) => "yes",
            Some(false) => "no",
            None => "not known (no LockedHint from logind)",
        }
    );
    let _ = writeln!(r, "| Audio server | {audio_server} |");
    if let Some(m) = &mutter {
        let _ = writeln!(
            r,
            "| GNOME mutter experimental features | {} |",
            if m.ok { m.text.trim() } else { "not read" }
        );
    }
    if let Some((v, _)) = &hyprland {
        let _ = writeln!(
            r,
            "| Hyprland | {} |",
            v.text.lines().next().unwrap_or("?").trim()
        );
    }
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
        samples_status(&samples)
    );

    match &tier2 {
        Some(t) => {
            let _ = writeln!(r, "**{}.**\n", t.counts());
        }
        None => {
            let _ = writeln!(r, "**Tier 2: not run.**\n");
        }
    }
    match &tier1 {
        Some(t) => {
            let _ = writeln!(r, "**{}.**\n", t.counts());
        }
        None if a.no_tier1 => {
            let _ = writeln!(r, "**Tier 1 on this machine: left out by `--no-tier1`.**\n");
        }
        None => {
            let _ = writeln!(r, "**Tier 1 on this machine: not run.**\n");
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

    let _ = writeln!(r, "## Power profile\n");
    let _ = writeln!(
        r,
        "Read once, before the build, and never changed: decode speed moves with these.\n"
    );
    let _ = writeln!(r, "| Setting | Value |\n|---|---|");
    for (what, value) in &power {
        let _ = writeln!(r, "| {what} | {value} |");
    }
    let _ = writeln!(r);

    let _ = writeln!(r, "## Render nodes\n");
    if nodes.is_empty() {
        let _ = writeln!(
            r,
            "None: /sys/class/drm lists no render node (is a GPU driver loaded?).\n"
        );
    } else {
        let _ = writeln!(
            r,
            "| Node | PCI slot | Vendor | Device | Driver | Opens |\n|---|---|---|---|---|---|"
        );
        for n in &nodes {
            let opens = match &n.access {
                Access::Yes => "yes".to_string(),
                Access::No(why) => format!("**no**: {why}"),
            };
            let _ = writeln!(
                r,
                "| {} | {} | {} | {} | {} | {opens} |",
                n.name,
                n.pci_slot.as_deref().unwrap_or("?"),
                n.vendor_name(),
                n.device.as_deref().unwrap_or("?"),
                n.driver.as_deref().unwrap_or("?"),
            );
        }
        let _ = writeln!(r);
    }

    let _ = writeln!(r, "## Perf clips\n");
    if a.quick {
        let _ = writeln!(
            r,
            "Not made: `--quick` leaves out the benchmarks that play them.\n"
        );
    } else if clips.is_empty() {
        let _ = writeln!(r, "Not made: there was no build.\n");
    } else {
        let _ = writeln!(
            r,
            "synth's noisy grid in HEVC at about a camera's bitrate, made once and kept in \
             `{}` for the playback benchmarks. Software decode over {TOO_EASY_FPS:.0} fps \
             would make a clip too easy to say anything about footage. A two-track clip has \
             a row a lens: its bitrate from that lens's samples, its decode that lens's \
             alone.\n",
            media_dir.display()
        );
        let _ = writeln!(
            r,
            "| Clip | Size | Bitrate | Made | Software decode |\n|---|---:|---:|---|---|"
        );
        for row in perf_clip_rows(&clips, &parse_benches(&bench_text_of(&tier2, &bench_out))) {
            let _ = writeln!(r, "{row}");
        }
        let _ = writeln!(r);
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
                    Outcome::LeftOut(flag) => format!("left out by {flag}"),
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
            for (what, out) in t.binary_notes() {
                let _ = writeln!(r, "{what}:\n\n```\n{}\n```\n", tail(out, 30));
            }
            if t.unexplained_failure() {
                let _ = writeln!(
                    r,
                    "`cargo test` failed, but no test binary did. The end of its output:\n\n```\n{}\n```\n",
                    tail(&t.run.text, 60)
                );
            }
        }
    }

    let _ = writeln!(r, "## Benchmarks\n");
    let bench_text = bench_text_of(&tier2, &bench_out);
    let benches = parse_benches(&bench_text);
    if a.quick {
        let _ = writeln!(r, "Skipped (`--quick` builds in debug mode).\n");
    } else if benches.is_empty() {
        let _ = writeln!(
            r,
            "None recorded (benchmarks need the hardware the Tier 2 tests above need).\n"
        );
    } else {
        let _ = writeln!(
            r,
            "Release build. Each benchmark runs once to warm up, then {} times \
             (`SPHERECUT_BENCH_RUNS`); the result is the median. Spread is the \
             range over the median: over {UNSTABLE}% means the machine was busy \
             or changing clocks, so take that result with care. A row marked \
             \"per second\" is one run's seconds instead: Runs counts the seconds, \
             and the range is the slowest and the fastest.\n",
            bench_runs()
        );
        let _ = writeln!(
            r,
            "| Benchmark | Median | Range | Runs | Spread |\n|---|---:|---:|---:|---:|"
        );
        for b in &benches {
            let _ = writeln!(r, "{}", b.row());
        }
        let _ = writeln!(r);
    }
    // Such as which device a row ran on, or why a row has no result.
    let notes = parse_bench_notes(&bench_text);
    if !notes.is_empty() {
        let _ = writeln!(r, "Notes from the benchmarks:\n");
        for n in notes {
            let _ = writeln!(r, "- {n}");
        }
        let _ = writeln!(r);
    }
    let _ = writeln!(r, "## Tier 1 on this machine\n");
    match &tier1 {
        None if a.no_tier1 => {
            let _ = writeln!(r, "Left out by `--no-tier1`.\n");
        }
        None => {
            let _ = writeln!(r, "Not run: there was no build.\n");
        }
        Some(t) => {
            let _ = writeln!(
                r,
                "`{}`, the debug build, as `cargo xtask check` runs it: {}.\n",
                t.run.cmdline,
                t.counts()
            );
            for (n, out) in t.failed() {
                let out = if out.trim().is_empty() {
                    "(libtest printed no output for this failure)".to_string()
                } else {
                    tail(&out, 60)
                };
                let _ = writeln!(r, "### FAIL `{n}`\n\n```\n{out}\n```\n");
            }
            for p in t.problems() {
                if !p.contains("test(s) failed") {
                    let _ = writeln!(r, "- {p}");
                }
            }
            if !t.run.ok && t.failed().is_empty() {
                let _ = writeln!(
                    r,
                    "\nThe end of its output:\n\n```\n{}\n```\n",
                    tail(&t.run.text, 60)
                );
            }
            let _ = writeln!(r);
        }
    }

    let _ = writeln!(r, "## GUI smoke run\n");
    let gui_test = |name: &str| match &tier2 {
        None => "didn't run: there was no build".to_string(),
        Some(t) => match t.results.iter().find(|(n, _)| n.ends_with(name)) {
            None => "wasn't listed".to_string(),
            Some((_, Outcome::Passed)) => "passed".to_string(),
            Some((_, Outcome::Failed(_))) => {
                "**FAILED** (its output is under Tier 2 tests)".to_string()
            }
            Some((_, Outcome::Skipped(why))) => format!("skipped: {why}"),
            Some((_, Outcome::LeftOut(flag))) => format!("was left out by {flag}"),
            Some((_, Outcome::DidNotRun)) => "**did not run**".to_string(),
        },
    };
    r.push_str(&crate::gui_report::section(
        &gui_out,
        &report_path,
        &gui_test("::gui_smoke"),
    ));
    let _ = writeln!(r, "## Timeline prototype\n");
    r.push_str(&crate::gui_report::proto_section(
        &gui_out,
        &report_path,
        &gui_test("::gui_proto"),
    ));

    let _ = writeln!(r, "## Raw environment\n");
    if let Some(dc) = &doctor {
        details(&mut r, "spherecut doctor", dc);
    }
    details(&mut r, "rustc", &rustc);
    if let Some(p) = &pactl {
        details(&mut r, "pactl info", p);
    }
    if let Some((v, m)) = &hyprland {
        details(&mut r, "hyprctl version", v);
        details(&mut r, "hyprctl monitors", m);
    }
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

    let out = report_path;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&out, &r).with_context(|| format!("writing {}", out.display()))?;
    eprintln!("==> report written to {}", display_rel(&out, &root));
    if let Some(t) = &tier2 {
        eprintln!("==> {}", t.counts());
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

/// What the benchmarks wrote, once Tier 2 ran.
fn bench_text_of(tier2: &Option<Tier2>, bench_out: &Path) -> String {
    if tier2.is_some() {
        std::fs::read_to_string(bench_out).unwrap_or_default()
    } else {
        String::new()
    }
}

/// Whether the samples directory is there. Its path need not be UTF-8:
/// it reaches the tests byte for byte, and they open samples from the
/// path's bytes.
fn samples_status(samples: &Path) -> &'static str {
    if samples.is_dir() {
        "present"
    } else {
        "missing"
    }
}

/// Spread, in percent of the median, above which a benchmark is flagged.
const UNSTABLE: f64 = 10.0;

/// The runs `spherecut_testkit::bench::measure` makes, as the test
/// processes will see it.
fn bench_runs() -> usize {
    std::env::var("SPHERECUT_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(3)
}

/// One line of `hw-bench.tsv`.
#[derive(Debug, PartialEq)]
struct Bench {
    name: String,
    median: f64,
    unit: String,
    /// Min and max, when there was more than one run.
    range: Option<(f64, f64)>,
    runs: usize,
}

impl Bench {
    fn row(&self) -> String {
        let Bench {
            name,
            median,
            unit,
            range,
            runs,
        } = self;
        match range {
            Some((lo, hi)) if *runs > 1 => {
                // From the unrounded numbers; there is none without a
                // median to divide by.
                let spread = if *median > 0.0 {
                    let spread = (hi - lo) / median * 100.0;
                    let flag = if spread > UNSTABLE {
                        " **unstable**"
                    } else {
                        ""
                    };
                    format!("{spread:.0}%{flag}")
                } else {
                    "n/a".into()
                };
                format!("| {name} | {median:.2} {unit} | {lo:.2} to {hi:.2} | {runs} | {spread} |")
            }
            _ => format!("| {name} | {median:.2} {unit} | | {runs} | |"),
        }
    }
}

/// What a note's line in `hw-bench.tsv` starts with
/// (`spherecut_testkit::bench::NOTE_PREFIX`).
const NOTE_PREFIX: &str = "# ";

/// Lines of `name, median, unit[, min, max, runs]`, tab-separated, as
/// written by `spherecut_testkit::bench`. Notes are left out.
fn parse_benches(text: &str) -> Vec<Bench> {
    text.lines()
        .filter(|l| !l.starts_with(NOTE_PREFIX))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
            Some(Bench {
                name: f.first()?.to_string(),
                median: num(1)?,
                unit: f.get(2)?.to_string(),
                range: num(3).zip(num(4)),
                runs: f.get(5).and_then(|v| v.parse().ok()).unwrap_or(1),
            })
        })
        .collect()
}

/// The notes in `hw-bench.tsv`, one line each, such as why a benchmark
/// has no result (`spherecut_testkit::bench::note`). Each note is listed
/// once, where it first appears: two benchmarks on the same device both
/// say which device it is.
fn parse_bench_notes(text: &str) -> Vec<&str> {
    let mut notes = Vec::new();
    for n in text.lines().filter_map(|l| l.strip_prefix(NOTE_PREFIX)) {
        if !notes.contains(&n) {
            notes.push(n);
        }
    }
    notes
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
    use std::collections::BTreeSet;

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
        let r = parse_run(out).results;
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
        assert!(matches!(parse_run(out).results["a"], Outcome::Failed(_)));
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
        let r = parse_run(out).results;
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
        let b = parse_run(out);
        let r = &b.results;
        assert_eq!(r.len(), 2, "c_after never started: {r:?}");
        assert_eq!(r["a_first"], Outcome::Passed);
        assert!(matches!(&r["b_aborts"], Outcome::Failed(o) if o.contains("SIGABRT")));
        assert!(!b.finished);
        assert_eq!(
            b.problem().as_deref(),
            Some("did not finish (signal: 6, SIGABRT: process abort signal)")
        );
    }

    #[test]
    fn a_crash_after_the_summary_fails_the_binary() {
        // Every test passed, then the binary died on its way out (a
        // driver's exit handler, say).
        let out = "\
running 1 test
test passes_then_binary_crashes_at_exit ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `-p spherecut-testkit --test tier2_exit_crash`

Caused by:
  process didn't exit successfully: `/t/debug/deps/tier2_exit_crash-f5ee94ef98ecad17 --ignored --test-threads=1` (signal: 6, SIGABRT: process abort signal)
";
        let b = parse_run(out);
        assert_eq!(
            b.results["passes_then_binary_crashes_at_exit"],
            Outcome::Passed
        );
        assert!(b.finished);
        assert_eq!(
            b.exit,
            Exit::Abnormal("signal: 6, SIGABRT: process abort signal".into())
        );
        assert_eq!(
            b.problem().as_deref(),
            Some(
                "exited abnormally after its tests finished (signal: 6, SIGABRT: process abort signal)"
            )
        );
    }

    #[test]
    fn a_stray_ok_before_a_crash_is_not_a_pass() {
        // The next test starting confirms `a`'s ok; nothing confirms `b`'s.
        let out = "running 3 tests
test a ... libva info: VA-API version 1.22.0
ok
test b ... \nok
error: test failed, to rerun pass `--test tier2_ok_then_abort`

Caused by:
  process didn't exit successfully: `/t/debug/deps/tier2_ok_then_abort-f12029a49ed85019 --ignored` (signal: 6, SIGABRT: process abort signal (core dumped))
";
        let b = parse_run(out);
        assert_eq!(
            b.exit,
            Exit::Abnormal("signal: 6, SIGABRT: process abort signal (core dumped)".into())
        );
        assert_eq!(b.results.len(), 2, "{:?}", b.results);
        assert_eq!(b.results["a"], Outcome::Passed);
        assert!(matches!(&b.results["b"], Outcome::Failed(o) if o.contains("SIGABRT")));
        assert!(b.problem().is_some_and(|p| p.starts_with("did not finish")));

        // A FAILED after a stray ok is the real result.
        let out = "test a ... \nok\nFAILED\ntest b ... ok\nerror: test failed, to rerun pass `--test x`\n";
        assert!(matches!(parse_run(out).results["a"], Outcome::Failed(_)));
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

    fn packages(exes: &[(&str, &str)]) -> BTreeMap<PathBuf, String> {
        exes.iter()
            .map(|(exe, p)| (PathBuf::from(exe), p.to_string()))
            .collect()
    }

    #[test]
    fn lists_tests_per_binary() {
        let l = parse_list(LIST);
        let packages = packages(&[
            ("/t/debug/deps/tier2-1111111111111111", "probe-a"),
            ("/t/debug/deps/tier2-2222222222222222", "probe-b"),
        ]);
        let names = binary_names(l.keys(), &packages, Path::new("/w"));
        let l = by_name(l, &names);
        let names: Vec<&str> = l.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "probe-a/tier2",
                "probe-b/tier2",
                "spherecut_core (doc-tests)",
                "spherecut_media",
                "tier2_samples"
            ]
        );
        assert!(l["spherecut_media"].is_empty());
        assert_eq!(
            l["tier2_samples"],
            ["bench_samples_realtime_decode", "samples_probe_cleanly"]
        );
        assert_eq!(l["probe-a/tier2"], ["same_name"]);
        assert_eq!(l["probe-b/tier2"], ["same_name"]);

        // Without the packages, Cargo's hash is all that tells them apart.
        let names = binary_names(parse_list(LIST).keys(), &BTreeMap::new(), Path::new("/w"));
        let names: BTreeSet<&str> = names.values().map(String::as_str).collect();
        assert!(names.contains("tier2-1111111111111111"), "{names:?}");
        assert!(names.contains("tier2-2222222222222222"), "{names:?}");
        assert!(names.contains("tier2_samples"), "{names:?}");
    }

    #[test]
    fn same_named_binaries_keep_their_own_results() {
        // probe-a's tier2 comes first in the list, but cargo never ran it.
        let list = "     Running tests/tier2.rs (target/debug/deps/tier2-1111111111111111)
same_name: test

1 test, 0 benchmarks
     Running tests/tier2.rs (target/debug/deps/tier2-2222222222222222)
only_in_b: test
same_name: test

2 tests, 0 benchmarks
";
        let run = "     Running tests/tier2.rs (target/debug/deps/tier2-2222222222222222)

running 2 tests
test only_in_b ... ok
test same_name ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
";
        let packages = packages(&[
            ("/w/target/debug/deps/tier2-1111111111111111", "probe-a"),
            ("/w/target/debug/deps/tier2-2222222222222222", "probe-b"),
        ]);
        let t = Tier2::new(
            Captured {
                text: list.into(),
                ..captured(true)
            },
            Captured {
                text: run.into(),
                ..captured(false)
            },
            &packages,
            Path::new("/w"),
            &Selection::default(),
        );
        let expect = [
            ("probe-a/tier2::same_name", Outcome::DidNotRun),
            ("probe-b/tier2::only_in_b", Outcome::Passed),
            ("probe-b/tier2::same_name", Outcome::Passed),
        ];
        let expect: Vec<(String, Outcome)> = expect
            .into_iter()
            .map(|(n, o)| (n.to_string(), o))
            .collect();
        assert_eq!(t.results, expect);
        assert_eq!(t.did_not_run(), BTreeMap::from([("probe-a/tier2", 1)]));
    }

    #[test]
    fn reads_packages_from_cargo_json() {
        assert_eq!(
            package_name("path+file:///w/crates/media#spherecut-media@0.1.0"),
            Some("spherecut-media")
        );
        assert_eq!(package_name("path+file:///w/xtask#0.1.0"), Some("xtask"));
        assert_eq!(package_name("xtask 0.1.0 (path+file:///w/xtask)"), None);

        let json = r#"{"reason":"compiler-artifact","package_id":"path+file:///w/a#probe-a@0.0.0","manifest_path":"/w/a/Cargo.toml","target":{"kind":["lib"],"name":"probe_a"},"executable":null,"fresh":true}
{"reason":"compiler-artifact","package_id":"path+file:///w/a#probe-a@0.0.0","manifest_path":"/w/a/Cargo.toml","target":{"kind":["test"],"name":"tier2"},"executable":"/w/target/debug/deps/tier2-721e609f1adde35e","fresh":true}
{"reason":"compiler-artifact","package_id":"path+file:///w/xtask#0.1.0","manifest_path":"/w/xtask/Cargo.toml","target":{"kind":["bin"],"name":"xtask"},"executable":"/w/target/debug/deps/xtask-253d62ad6a641493","fresh":true}
  Executable tests/tier2.rs (target/debug/deps/tier2-721e609f1adde35e)
{"reason":"build-finished","success":true}
"#;
        assert_eq!(
            test_packages(json),
            packages(&[
                ("/w/target/debug/deps/tier2-721e609f1adde35e", "probe-a"),
                ("/w/target/debug/deps/xtask-253d62ad6a641493", "xtask"),
            ])
        );
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

error: test failed, to rerun pass `-p spherecut-synth --test tier2_hw_decode`
";
        let parts = split_by_binary(run);
        assert_eq!(parts.len(), 2);
        assert_eq!(parse_run(&parts[0].1).exit, Exit::Clean);
        // A skip makes libtest exit 101, which is not a problem by itself.
        let skips = parse_run(&parts[1].1);
        assert_eq!(skips.exit, Exit::TestsFailed);
        assert!(skips.finished);
        assert_eq!(skips.problem(), None);
        let names = binary_names(
            parts.iter().map(|(b, _)| b),
            &BTreeMap::new(),
            Path::new("/w"),
        );
        assert_eq!(names[&parts[0].0], "tier2_samples");
        assert_eq!(names[&parts[1].0], "tier2_hw_decode");
        assert_eq!(
            parse_run(&parts[0].1).results["samples_probe_cleanly"],
            Outcome::Passed
        );
        assert!(matches!(
            parse_run(&parts[1].1).results["samples_probe_cleanly"],
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
            ran(&[
                ("samples_probe_cleanly", Outcome::Passed),
                ("surprise", Outcome::Failed("x".into())),
            ]),
        )]);
        let full = merge(&listed, &ran, &Selection::default());
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
        let quick = merge(&listed, &ran, &QUICK);
        assert_eq!(
            quick[0],
            (
                "tier2_hw_decode::bench_decode".into(),
                Outcome::LeftOut("`--quick`")
            )
        );
        assert_eq!(quick[1].1, Outcome::DidNotRun);
        assert_eq!(quick[2].1, Outcome::LeftOut("`--quick`"));
    }

    const QUICK: Selection = Selection {
        quick: true,
        only: Vec::new(),
    };

    fn only(names: &[&str]) -> Selection {
        Selection {
            quick: false,
            only: names.iter().map(|n| n.to_string()).collect(),
        }
    }

    #[test]
    fn only_picks_by_the_reports_names_and_leaves_the_rest_out() {
        let listed = listed(&[
            ("tier2_gui", &["gui_smoke"]),
            ("tier2_samples", &["samples_probe_cleanly", "bench_samples"]),
        ]);
        let ran = BTreeMap::from([(
            "tier2_gui".to_string(),
            ran(&[("gui_smoke", Outcome::Passed)]),
        )]);
        // A test's own name, or the binary's, as `--list` prints them.
        for sel in [only(&["gui_smoke"]), only(&["tier2_gui::"]), only(&["gui"])] {
            let m = merge(&listed, &ran, &sel);
            assert_eq!(m[0], ("tier2_gui::gui_smoke".into(), Outcome::Passed));
            assert_eq!(m[1].1, Outcome::LeftOut("`--only`"));
            assert_eq!(m[2].1, Outcome::LeftOut("`--only`"));
            assert!(sel.unmatched(&listed).is_empty());
        }
        // A picked test that didn't come back still did not run.
        let m = merge(&listed, &ran, &only(&["samples_probe"]));
        assert_eq!(m[2].1, Outcome::DidNotRun);
        // A name that picks nothing is a problem, not a clean run.
        let sel = only(&["gui_smoke", "nonesuch"]);
        assert_eq!(sel.unmatched(&listed), ["nonesuch"]);
        let mut t = tier2(true, &[("tier2_gui", &["gui_smoke"])], vec![]);
        t.unmatched = sel.unmatched(&listed);
        t.results = merge(&t.listed, &ran, &sel);
        assert_eq!(t.problems(), ["`--only nonesuch` picks no Tier 2 test"]);
        assert_eq!(
            t.counts(),
            "Tier 2: 1 passed, 0 failed, 0 skipped, 0 did not run (1 tests)"
        );
        // With `--quick`, a benchmark `--only` picks is still left out.
        let sel = Selection {
            quick: true,
            only: vec!["samples".into()],
        };
        assert_eq!(
            sel.left_out("tier2_samples", "bench_samples"),
            Some("`--quick`")
        );
        assert_eq!(sel.left_out("tier2_samples", "samples_probe_cleanly"), None);
    }

    fn captured(ok: bool) -> Captured {
        Captured {
            cmdline: "cargo test".into(),
            ok,
            text: String::new(),
            elapsed: Duration::ZERO,
        }
    }

    /// A binary that finished with these results and exited as libtest
    /// does.
    fn ran(results: &[(&str, Outcome)]) -> BinaryRun {
        let failed = results
            .iter()
            .any(|(_, o)| matches!(o, Outcome::Failed(_) | Outcome::Skipped(_)));
        BinaryRun {
            output: String::new(),
            results: results
                .iter()
                .map(|(t, o)| (t.to_string(), o.clone()))
                .collect(),
            finished: true,
            exit: if failed {
                Exit::TestsFailed
            } else {
                Exit::Clean
            },
        }
    }

    /// A `--quick` Tier 2 part with these listed tests and binary runs.
    fn tier2(run_ok: bool, list: &[(&str, &[&str])], runs: Vec<(&str, BinaryRun)>) -> Tier2 {
        let binaries = runs
            .into_iter()
            .map(|(bin, b)| (bin.to_string(), b))
            .collect();
        let listed = listed(list);
        let results = merge(&listed, &binaries, &QUICK);
        Tier2 {
            list: captured(true),
            listed,
            run: captured(run_ok),
            binaries,
            results,
            unmatched: Vec::new(),
        }
    }

    fn skip() -> Outcome {
        Outcome::Skipped("no GPU".into())
    }

    #[test]
    fn skips_alone_are_not_a_failure() {
        // cargo exits non-zero for a skip, which is not worth a note.
        let t = tier2(
            false,
            &[("a", &["passes", "skips"])],
            vec![("a", ran(&[("passes", Outcome::Passed), ("skips", skip())]))],
        );
        assert!(!t.unexplained_failure());
        assert!(t.problems().is_empty());
        let t = tier2(
            true,
            &[("a", &["passes", "bench_x"])],
            vec![("a", ran(&[("passes", Outcome::Passed)]))],
        );
        assert_eq!(t.results[0].1, Outcome::LeftOut("`--quick`"));
        assert!(t.problems().is_empty());
    }

    #[test]
    fn missing_or_failed_tests_fail_the_run() {
        let t = tier2(
            false,
            &[("a", &["skips"]), ("b", &["never_ran"])],
            vec![("a", ran(&[("skips", skip())]))],
        );
        assert_eq!(t.problems(), ["1 Tier 2 test(s) did not run"]);
        assert!(t.counts().contains("1 did not run"));
        assert_eq!(t.did_not_run(), BTreeMap::from([("b", 1)]));
        let notes = t.binary_notes();
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0]
                .0
                .starts_with("1 test(s) in `b` did not run: cargo never ran it")
        );

        let t = tier2(
            false,
            &[("a", &["fails"])],
            vec![("a", ran(&[("fails", Outcome::Failed(String::new()))]))],
        );
        assert_eq!(t.problems(), ["1 Tier 2 test(s) failed"]);

        let t = tier2(
            false,
            &[("a", &["passes"])],
            vec![("a", ran(&[("passes", Outcome::Passed)]))],
        );
        assert!(t.unexplained_failure());
        assert_eq!(
            t.problems(),
            ["`cargo test` failed, but no test binary did"]
        );

        let mut t = tier2(
            true,
            &[("a", &["passes"])],
            vec![("a", ran(&[("passes", Outcome::Passed)]))],
        );
        t.list.ok = false;
        assert_eq!(t.problems().len(), 1);
    }

    #[test]
    fn an_empty_listing_fails_the_run() {
        // Cargo output this module could not read must not look clean.
        let t = tier2(true, &[], vec![]);
        assert_eq!(t.problems(), ["the listing found no test binaries"]);
        assert!(t.counts().contains("(0 tests)"));
        let t = tier2(true, &[("a", &[]), ("b", &[])], vec![]);
        assert_eq!(t.problems(), ["the listing found no Tier 2 tests"]);
    }

    #[test]
    fn a_skip_does_not_hide_another_binarys_failure() {
        // A skip makes cargo fail, but only explains its own binary.
        let mut crashed = ran(&[("passes", Outcome::Passed)]);
        crashed.exit = Exit::Abnormal("signal: 6, SIGABRT: process abort signal".into());
        let t = tier2(
            false,
            &[("a", &["skips"]), ("b", &["passes"])],
            vec![("a", ran(&[("skips", skip())])), ("b", crashed)],
        );
        assert_eq!(
            t.problems(),
            [
                "`b` exited abnormally after its tests finished (signal: 6, SIGABRT: process abort signal)"
            ]
        );
        assert!(!t.unexplained_failure());
        let notes = t.binary_notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].0.starts_with("`b` exited abnormally"));

        let mut odd = ran(&[("passes", Outcome::Passed)]);
        odd.exit = Exit::TestsFailed;
        let t = tier2(
            false,
            &[("a", &["skips"]), ("b", &["passes"])],
            vec![("a", ran(&[("skips", skip())])), ("b", odd)],
        );
        assert_eq!(t.problems(), ["`b` failed, but none of its tests did"]);

        let mut unfinished = ran(&[("passes", Outcome::Passed)]);
        unfinished.finished = false;
        let t = tier2(true, &[("b", &["passes"])], vec![("b", unfinished)]);
        assert_eq!(t.problems(), ["`b` did not finish"]);
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

    /// A throwaway cargo workspace in the temp directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str, files: &[(&str, &str)]) -> Scratch {
            let dir =
                std::env::temp_dir().join(format!("spherecut-xtask-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for (rel, text) in files {
                let p = dir.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Two packages with a `tests/tier2.rs` each, and two binaries that
    /// print what libtest would and then die. They exit with status 3
    /// rather than abort, so that no core file is left behind.
    const PROBE: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"3\"\n",
        ),
        (
            "a/Cargo.toml",
            r#"[package]
name = "probe-a"
version = "0.0.0"
edition = "2024"

[[test]]
name = "dies_after_summary"
harness = false

[[test]]
name = "says_ok_then_dies"
harness = false
"#,
        ),
        ("a/src/lib.rs", ""),
        (
            "a/tests/tier2.rs",
            "#[test]\n#[ignore]\nfn same_name() {}\n\n#[test]\nfn tier1() {}\n",
        ),
        (
            "a/tests/dies_after_summary.rs",
            r#"fn main() {
    if std::env::args().any(|a| a == "--list") {
        println!("passes: test\n\n1 test, 0 benchmarks");
        return;
    }
    println!("\nrunning 1 test\ntest passes ... ok\n");
    println!("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n");
    std::process::exit(3);
}
"#,
        ),
        (
            "a/tests/says_ok_then_dies.rs",
            r#"use std::io::Write;

fn main() {
    if std::env::args().any(|a| a == "--list") {
        println!("dies: test\n\n1 test, 0 benchmarks");
        return;
    }
    print!("\nrunning 1 test\ntest dies ... ");
    std::io::stdout().flush().unwrap();
    eprintln!("\nok");
    std::process::exit(3);
}
"#,
        ),
        (
            "b/Cargo.toml",
            "[package]\nname = \"probe-b\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        ),
        ("b/src/lib.rs", ""),
        (
            "b/tests/tier2.rs",
            "#[test]\n#[ignore]\nfn same_name() {\n    panic!(\"TIER2-SKIP: nothing here\");\n}\n",
        ),
    ];

    /// `text`, another cargo run's output, quoted for an assertion's
    /// message: as it is, its status and result lines would read as this
    /// run's to hw-test's own Tier 1 run, binaries and tests of its own.
    fn quoted(text: &str) -> String {
        text.lines().map(|l| format!("\n| {l}")).collect()
    }

    #[test]
    fn reads_real_cargo_output_whatever_the_users_terminal_settings() {
        let s = Scratch::new("hw-test", PROBE);
        let target = s.0.join("target").to_string_lossy().into_owned();
        let base = [
            ("CARGO_TARGET_DIR", target.as_str()),
            ("CARGO_NET_OFFLINE", "true"),
            ("CARGO_INCREMENTAL", "0"),
        ];
        // The user's cargo config, or variables hw-test inherits.
        let users: [(&str, &[(&str, &str)]); 3] = [
            ("[term]\nverbose = true\n", &[]),
            ("[term]\nquiet = true\n", &[]),
            (
                "",
                &[("CARGO_TERM_VERBOSE", "true"), ("RUST_TEST_NOCAPTURE", "1")],
            ),
        ];
        for (config, user_env) in users {
            let why = format!("config {config:?}, env {user_env:?}");
            std::fs::create_dir_all(s.0.join(".cargo")).unwrap();
            std::fs::write(s.0.join(".cargo/config.toml"), config).unwrap();
            let env: Vec<(&str, &str)> = base.iter().chain(user_env).copied().collect();

            // The setting does change cargo's output when left alone: with
            // only the user's settings, not what this test inherits, such
            // as hw-test's own Tier 1 run's PLAIN_OUTPUT.
            let mut alone: Vec<String> = PLAIN_OUTPUT
                .iter()
                .flat_map(|(k, _)| ["-u".to_string(), k.to_string()])
                .collect();
            alone.extend(user_env.iter().map(|(k, v)| format!("{k}={v}")));
            alone.extend(
                ["cargo", "test", "--workspace", "--", "--ignored", "--list"].map(String::from),
            );
            let alone: Vec<&str> = alone.iter().map(String::as_str).collect();
            let plain = capture_merged(&s.0, "env", &alone, &env);
            assert!(plain.ok, "{why}: {}", quoted(&plain.text));
            assert!(
                !plain.text.contains("     Running tests/tier2.rs ("),
                "{why}: {}",
                quoted(&plain.text)
            );

            let t = list_and_run(&s.0, &QUICK, &env);
            assert!(t.list.ok, "{why}: {}", quoted(&t.list.text));
            let names: Vec<&str> = t.results.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(
                names,
                [
                    "dies_after_summary::passes",
                    "probe-a/tier2::same_name",
                    "probe-b/tier2::same_name",
                    "says_ok_then_dies::dies"
                ],
                "{why}: {}",
                quoted(&t.run.text)
            );
            assert_eq!(t.results[0].1, Outcome::Passed, "{why}");
            assert_eq!(t.results[1].1, Outcome::Passed, "{why}");
            assert_eq!(
                t.results[2].1,
                Outcome::Skipped("nothing here".into()),
                "{why}"
            );
            assert!(matches!(t.results[3].1, Outcome::Failed(_)), "{why}");
            assert_eq!(
                t.problems(),
                [
                    "1 Tier 2 test(s) failed",
                    "`dies_after_summary` exited abnormally after its tests finished (exit status: 3)",
                    "`says_ok_then_dies` did not finish (exit status: 3)"
                ],
                "{why}: {}",
                quoted(&t.run.text)
            );

            // Tier 1 on the same workspace: its one Tier 1 test, and the
            // binaries that print libtest's output whatever they are asked.
            let t1 = run_tier1(&s.0, &env);
            let mut results: Vec<(String, Outcome)> =
                t1.results().map(|(n, o)| (n, o.clone())).collect();
            results.sort_by(|a, b| a.0.cmp(&b.0));
            let names: Vec<&str> = results.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(
                names,
                [
                    "dies_after_summary::passes",
                    "probe-a/tier2::tier1",
                    "says_ok_then_dies::dies"
                ],
                "{why}: {}",
                quoted(&t1.run.text)
            );
            assert_eq!(t1.passed(), 2, "{why}");
            assert_eq!(t1.failed().len(), 1, "{why}");
            assert_eq!(t1.failed()[0].0, "says_ok_then_dies::dies", "{why}");
            assert_eq!(
                t1.problems(),
                [
                    "1 Tier 1 test(s) failed on this machine",
                    "Tier 1 `dies_after_summary` exited abnormally after its tests finished (exit status: 3)",
                    "Tier 1 `says_ok_then_dies` did not finish (exit status: 3)"
                ],
                "{why}: {}",
                quoted(&t1.run.text)
            );
            assert!(
                t1.counts()
                    .starts_with("Tier 1 on this machine: 2 passed, 1 failed, in "),
                "{}",
                t1.counts()
            );
        }

        // `--only`: libtest gets the picked test's own name, exactly, and
        // the report leaves out the rest, its namesake in probe-a and the
        // binaries that print results whatever they're asked included.
        std::fs::write(s.0.join(".cargo/config.toml"), "").unwrap();
        let t = list_and_run(&s.0, &only(&["probe-b/"]), &base);
        assert!(
            t.run.cmdline.ends_with("--exact same_name"),
            "{}",
            t.run.cmdline
        );
        let expect = [
            ("dies_after_summary::passes", Outcome::LeftOut("`--only`")),
            ("probe-a/tier2::same_name", Outcome::LeftOut("`--only`")),
            (
                "probe-b/tier2::same_name",
                Outcome::Skipped("nothing here".into()),
            ),
            ("says_ok_then_dies::dies", Outcome::LeftOut("`--only`")),
        ];
        let expect: Vec<(String, Outcome)> = expect
            .into_iter()
            .map(|(n, o)| (n.to_string(), o))
            .collect();
        assert_eq!(t.results, expect, "{}", quoted(&t.run.text));
        // A name that picks nothing runs nothing and fails the run.
        let t = list_and_run(&s.0, &only(&["nonesuch"]), &base);
        assert!(t.run.text.is_empty());
        assert!(
            t.results
                .iter()
                .all(|(_, o)| *o == Outcome::LeftOut("`--only`"))
        );
        assert_eq!(t.problems(), ["`--only nonesuch` picks no Tier 2 test"]);
    }

    #[test]
    fn a_tier1_test_that_skips_has_failed() {
        // Cargo indents its `Running` line, which a string continuation
        // would take off.
        let out = "     Running unittests src/lib.rs (target/debug/deps/probe_a-0123456789abcdef)

running 2 tests
test a ... ok
test b ... FAILED

failures:

---- b stdout ----

thread 'b' panicked at src/lib.rs:1:1:
TIER2-SKIP: no GPU

failures:
    b

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out

error: test failed, to rerun pass `-p probe-a --lib`
";
        let t1 = Tier1::new(captured_text(false, out), &BTreeMap::new(), Path::new("/w"));
        assert_eq!(t1.passed(), 1);
        let failed = t1.failed();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].1.contains("may not skip"), "{failed:?}");
        assert_eq!(t1.problems(), ["1 Tier 1 test(s) failed on this machine"]);
    }

    #[test]
    fn another_cargo_runs_output_in_a_failure_stays_the_failures_when_quoted() {
        // What the test above prints when it fails, under hw-test's own
        // Tier 1 run: another cargo run's status and result lines.
        let nested = "     Running tests/tier2.rs (target/debug/deps/tier2-0123456789abcdef)

running 1 test
test same_name ... ok

failures:
    same_name

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

   Doc-tests probe_b
";
        let run = |inner: &str| {
            format!(
                "     Running unittests src/main.rs (target/debug/deps/xtask-0123456789abcdef)

running 2 tests
test a ... ok
test b ... FAILED

failures:

---- b stdout ----

thread 'b' panicked at src/main.rs:1:1:
why: {inner}

failures:
    b

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out

error: test failed, to rerun pass `-p xtask --bin xtask`
"
            )
        };
        let t1 = |text: &str| {
            Tier1::new(
                captured_text(false, text),
                &BTreeMap::new(),
                Path::new("/w"),
            )
        };
        let names = |t: &Tier1| t.results().map(|(n, _)| n).collect::<Vec<_>>();
        let quoted_run = t1(&run(&quoted(nested)));
        assert_eq!(names(&quoted_run), ["xtask::a", "xtask::b"]);
        assert_eq!(
            quoted_run.problems(),
            ["1 Tier 1 test(s) failed on this machine"]
        );
        assert!(quoted_run.failed()[0].1.contains("| test same_name ... ok"));
        // As it is, it reads as binaries and tests of this run's.
        let raw = t1(&run(nested));
        assert_ne!(names(&raw), ["xtask::a", "xtask::b"]);
        assert_ne!(raw.problems(), ["1 Tier 1 test(s) failed on this machine"]);
    }

    #[test]
    fn a_tier1_build_failure_is_a_problem() {
        let t1 = Tier1::new(
            captured_text(false, "error[E0425]: cannot find value `x` in this scope\n"),
            &BTreeMap::new(),
            Path::new("/w"),
        );
        assert_eq!(t1.problems(), ["the Tier 1 run ran no test binaries"]);
        let ok = "     Running unittests src/lib.rs (target/debug/deps/probe_a-0123456789abcdef)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

error: 1 target failed:
    `-p probe-b --test x`
";
        let t1 = Tier1::new(captured_text(false, ok), &BTreeMap::new(), Path::new("/w"));
        assert_eq!(
            t1.problems(),
            ["the Tier 1 `cargo test` failed, but no test binary did"]
        );
    }

    fn captured_text(ok: bool, text: &str) -> Captured {
        Captured {
            cmdline: "cargo test".into(),
            ok,
            text: text.into(),
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn perf_clips_show_their_bitrate_and_whether_they_are_too_easy() {
        let clip = |name: &str, made| Clip {
            name: name.into(),
            path: PathBuf::from(format!("/t/hw-media/{name}.mp4")),
            made,
            bytes: 250_000_000,
            seconds: 20.0,
            track_bytes: Vec::new(),
        };
        let clips = [
            clip("hevc8", Made::Cached),
            clip("hevc10", Made::Now(Duration::from_secs(312))),
            Clip {
                bytes: 0,
                ..clip("hevc8-8k", Made::Failed("no libx265".into()))
            },
        ];
        let benches = parse_benches(
            "perf clip hevc8: software decode\t61.24\tfps\t60\t62\t3\n\
             perf clip hevc10: software decode\t212\tfps\t200\t220\t3\n",
        );
        assert_eq!(
            perf_clip_rows(&clips, &benches),
            [
                "| `hevc8.mp4` | 250 MB | 100.0 Mbit/s | cached | 61.2 fps |",
                "| `hevc10.mp4` | 250 MB | 100.0 Mbit/s | now, in 312 s | 212.0 fps, **too easy** \
                 (over 150: it proves nothing about footage) |",
                "| `hevc8-8k.mp4` |  |  | **not made**: no libx265 | not measured |",
            ]
        );
    }

    #[test]
    fn a_pairs_lenses_are_each_flagged_too_easy_on_their_own() {
        let pair = Clip {
            name: "pair".into(),
            path: PathBuf::from("/t/hw-media/pair.mp4"),
            made: Made::Cached,
            bytes: 312_600_000,
            seconds: 20.0,
            track_bytes: vec![162_600_000, 150_000_000],
        };
        // A row for the file as a whole is no lens's.
        let benches = parse_benches(
            "perf clip pair lens 0: software decode\t61\tfps\t60\t62\t3\n\
             perf clip pair lens 1: software decode\t180\tfps\t170\t190\t3\n\
             perf clip pair: software decode\t300\tfps\t290\t310\t3\n",
        );
        let rows = perf_clip_rows(std::slice::from_ref(&pair), &benches);
        assert_eq!(
            rows,
            [
                "| `pair.mp4` lens 0 | 163 MB | 65.0 Mbit/s | cached | 61.0 fps |",
                "| `pair.mp4` lens 1 | 150 MB | 60.0 Mbit/s | cached | 180.0 fps, **too easy** \
                 (over 150: it proves nothing about footage) |",
            ]
        );
        // Its control: lens 0's row isn't flagged, and the same check fails
        // the pair rated as one file, whose row a bench of the whole file
        // flags.
        let unflagged = |row: &str| !row.contains("too easy");
        assert!(unflagged(&rows[0]));
        let whole = Clip {
            track_bytes: Vec::new(),
            ..pair
        };
        let rows = perf_clip_rows(&[whole], &benches);
        spherecut_testkit::control::must_fail("the pair rated as one file", unflagged(&rows[0]));
    }

    #[test]
    fn paths_that_are_not_utf8_reach_the_tests_unchanged_and_are_reported() {
        use std::os::unix::ffi::OsStrExt;
        // A lossy copy is another path: the report said "present" while
        // the tests looked for a directory with U+FFFD in its name.
        let raw = OsStr::from_bytes(b"samples-\xff");
        let s = Scratch::new("non-utf8", &[]);
        let samples = s.0.join(raw);
        std::fs::create_dir_all(&samples).unwrap();
        let bench = s.0.join(raw).join("hw-bench.tsv");
        let gui = s.0.join(raw).join("hw-gui");
        let media = s.0.join(raw).join("hw-media");
        assert_eq!(tier2_env(&samples, &bench, &gui, None).len(), 4);
        let env = tier2_env(&samples, &bench, &gui, Some(&media));
        assert_eq!(env[1], ("SPHERECUT_SAMPLES", samples.as_os_str()));
        assert_eq!(env[2], ("SPHERECUT_BENCH_OUT", bench.as_os_str()));
        assert_eq!(env[3], ("SPHERECUT_GUI_OUT", gui.as_os_str()));
        assert_eq!(env[4], ("SPHERECUT_PERF_MEDIA", media.as_os_str()));
        let child = capture_merged(
            &s.0,
            "sh",
            &["-c", "printf %s \"$SPHERECUT_SAMPLES\" | od -An -tx1"],
            &env,
        );
        let want: String = samples
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let got: String = child.text.split_whitespace().collect();
        assert_eq!(got, want, "{}", child.text);

        assert_eq!(samples_status(&samples), "present");
        assert_eq!(samples_status(&s.0), "present");
        assert_eq!(samples_status(&s.0.join("none")), "missing");
    }

    #[test]
    fn benchmarks_show_median_range_and_spread() {
        let b = parse_benches(
            "8K30: VA-API decode\t1.14\t× real time\t1.1\t1.18\t3\n\
             5.7K: VA-API\t88\tfps\t67\t110\t3\n\
             old format\t2.50\tfps\n\
             not a number\tfast\tfps\n\
             8K30: software\t0.056\t× real time\t0.054\t0.058\t3\n\
             stalled\t0\tfps\t0\t1.5\t3\n",
        );
        assert_eq!(b.len(), 5, "{b:?}");
        assert_eq!(
            b[0].row(),
            "| 8K30: VA-API decode | 1.14 × real time | 1.10 to 1.18 | 3 | 7% |"
        );
        assert_eq!(
            b[1].row(),
            "| 5.7K: VA-API | 88.00 fps | 67.00 to 110.00 | 3 | 49% **unstable** |"
        );
        assert_eq!(b[2].runs, 1);
        assert_eq!(b[2].row(), "| old format | 2.50 fps | | 1 | |");
        // The spread comes from the numbers as measured: rounded to 0.06,
        // 0.05 and 0.06 first, these read as 17% and unstable.
        assert_eq!(
            b[3].row(),
            "| 8K30: software | 0.06 × real time | 0.05 to 0.06 | 3 | 7% |"
        );
        // No spread without a median to divide by.
        assert_eq!(
            b[4].row(),
            "| stalled | 0.00 fps | 0.00 to 1.50 | 3 | n/a |"
        );
    }

    #[test]
    fn benchmark_notes_are_kept_apart_from_results() {
        let text = "a: VA-API\t2\tfps\t1\t3\t3\n\
                    # Vulkan Video rows ran on GPU 0\n\
                    # a: Vulkan Video: not measured: no decode 1\t2\n\
                    #not a note\t5\tfps\n\
                    # Vulkan Video rows ran on GPU 0\n";
        let b = parse_benches(text);
        assert_eq!(
            b.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            ["a: VA-API", "#not a note"]
        );
        assert_eq!(
            parse_bench_notes(text),
            [
                "Vulkan Video rows ran on GPU 0",
                "a: Vulkan Video: not measured: no decode 1\t2"
            ]
        );
    }
}
