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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::workspace_root;

#[derive(clap::Args)]
pub struct Args {
    /// Directory with camera samples (default: ./samples).
    #[arg(long)]
    samples: Option<PathBuf>,
    /// Skip the release build and benchmarks (environment and tests only).
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

fn capture_env(program: &str, args: &[&str], env: &[(&str, &str)]) -> Captured {
    let cmdline = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
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
        let freeworld = capture("rpm", &["-q", "mesa-va-drivers-freeworld"]);
        if !freeworld.ok && capture("rpm", &["-q", "mesa-va-drivers"]).ok {
            m.push(Missing {
                what:
                    "Mesa VA-API drivers with H.264/HEVC (stock mesa-va-drivers has them disabled)"
                        .into(),
                fix: "sudo dnf swap mesa-va-drivers mesa-va-drivers-freeworld   (RPM Fusion free)"
                    .into(),
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
    if !vainfo.is_empty() && !vainfo.contains("VAProfileHEVCMain") {
        m.push(Missing {
            what: "VA-API HEVC profiles (vainfo lists none)".into(),
            fix: match d {
                Distro::Fedora => "sudo dnf swap mesa-va-drivers mesa-va-drivers-freeworld   (AMD; Intel: sudo dnf install intel-media-driver from RPM Fusion nonfree)".into(),
                Distro::Arch => "sudo pacman -S --needed mesa   (AMD) or intel-media-driver (Intel)".into(),
                Distro::Other => "install a VA-API driver with HEVC enabled".into(),
            },
            blocks: "hardware HEVC decode and encode",
        });
    }
    m
}

/// Results of one libtest run, parsed from its text output.
#[derive(Default)]
struct TestResults {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
    skipped: Vec<(String, String)>,
    ignored: Vec<String>,
}

fn parse_libtest(output: &str) -> TestResults {
    let mut r = TestResults::default();
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut buf = String::new();
    for line in output.lines() {
        if let Some((name, status)) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.rsplit_once(" ... "))
        {
            let name = name.trim().to_string();
            match status.trim() {
                "ok" => r.passed.push(name),
                "FAILED" => {
                    failures.entry(name).or_default();
                }
                s if s.starts_with("ignored") => r.ignored.push(name),
                _ => {}
            }
            continue;
        }
        if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|l| l.strip_suffix(" stdout ----"))
        {
            if let Some(prev) = current.take() {
                failures.insert(prev, std::mem::take(&mut buf));
            }
            current = Some(name.to_string());
            continue;
        }
        if current.is_some() {
            if line == "failures:" || line.starts_with("test result:") {
                if let Some(prev) = current.take() {
                    failures.insert(prev, std::mem::take(&mut buf));
                }
            } else {
                buf.push_str(line);
                buf.push('\n');
            }
        }
    }
    if let Some(prev) = current.take() {
        failures.insert(prev, buf);
    }
    for (name, out) in failures {
        if let Some(reason) = out.lines().find_map(|l| {
            l.split_once("TIER2-SKIP:")
                .map(|(_, r)| r.trim().to_string())
        }) {
            r.skipped.push((name, reason));
        } else {
            r.failed.push((name, out));
        }
    }
    r
}

fn short_commit() -> String {
    let c = capture("git", &["rev-parse", "--short=10", "HEAD"]);
    let dirty = !capture("git", &["status", "--porcelain"])
        .text
        .trim()
        .is_empty();
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
    let samples = a.samples.unwrap_or_else(|| root.join("samples"));

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
    let mut tests: Option<(Captured, TestResults)> = None;
    let mut doctor: Option<Captured> = None;
    if build_blocked {
        eprintln!("==> build prerequisites missing; skipping build and tests");
    } else {
        if !a.quick {
            eprintln!("==> building (release)");
            build = Some(capture(
                "cargo",
                &["build", "--release", "--workspace", "--all-targets"],
            ));
        }
        let built = build.as_ref().is_none_or(|b| b.ok);
        if built {
            eprintln!("==> Tier 2 tests");
            let samples_s = samples.to_string_lossy().into_owned();
            let mut args = vec!["test", "--workspace"];
            if !a.quick {
                args.push("--release");
            }
            args.extend(["--", "--ignored", "--test-threads=1"]);
            let t = capture_env(
                "cargo",
                &args,
                &[("SPHERECUT_TIER2", "1"), ("SPHERECUT_SAMPLES", &samples_s)],
            );
            let parsed = parse_libtest(&t.text);
            tests = Some((t, parsed));
            let bin = if a.quick { "debug" } else { "release" };
            let exe = root.join("target").join(bin).join("spherecut");
            if exe.exists() {
                doctor = Some(capture(&exe.to_string_lossy(), &["doctor"]));
            }
        }
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
    let _ = writeln!(
        r,
        "| VA-API HEVC | decode: {}, encode: {} |",
        yes_no(
            vainfo.text.contains("VAProfileHEVCMain") && vainfo.text.contains("VAEntrypointVLD")
        ),
        yes_no(
            vainfo.text.contains("VAProfileHEVCMain")
                && vainfo.text.contains("VAEntrypointEncSlice")
        )
    );
    let _ = writeln!(
        r,
        "| FFmpeg hwaccels | {} |",
        hwaccels
            .text
            .lines()
            .skip(1)
            .map(str::trim)
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

    if let Some((_, t)) = &tests {
        let _ = writeln!(
            r,
            "**Tier 2: {} passed, {} failed, {} skipped.**\n",
            t.passed.len(),
            t.failed.len(),
            t.skipped.len()
        );
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
        _ if build_blocked => {
            let _ = writeln!(r, "Not attempted: build prerequisites are missing.\n");
        }
        None => {
            let _ = writeln!(r, "Skipped (`--quick`).\n");
        }
        Some(b) => {
            let _ = writeln!(
                r,
                "{} in {:.0} s.\n",
                if b.ok { "Succeeded" } else { "**FAILED**" },
                b.elapsed.as_secs_f64()
            );
            if !b.ok {
                let _ = writeln!(r, "```\n{}\n```\n", tail(&b.text, 80));
            }
        }
    }

    let _ = writeln!(r, "## Tier 2 tests\n");
    match &tests {
        None => {
            let _ = writeln!(r, "Not run.\n");
        }
        Some((cap, t)) => {
            let _ = writeln!(r, "| Test | Result |\n|---|---|");
            for n in &t.passed {
                let _ = writeln!(r, "| `{n}` | pass |");
            }
            for (n, _) in &t.failed {
                let _ = writeln!(r, "| `{n}` | **FAIL** |");
            }
            for (n, why) in &t.skipped {
                let _ = writeln!(r, "| `{n}` | skipped: {why} |");
            }
            if t.passed.is_empty() && t.failed.is_empty() && t.skipped.is_empty() {
                let _ = writeln!(r, "| (no Tier 2 tests yet) | |");
            }
            let _ = writeln!(r);
            for (n, out) in &t.failed {
                let _ = writeln!(r, "### FAIL `{n}`\n\n```\n{}\n```\n", tail(out, 60));
            }
            if !cap.ok && t.failed.is_empty() {
                let _ = writeln!(
                    r,
                    "The test run itself failed:\n\n```\n{}\n```\n",
                    tail(&cap.text, 60)
                );
            }
        }
    }

    let _ = writeln!(r, "## Benchmarks\n");
    let _ = writeln!(
        r,
        "No benchmarks yet (they arrive with M1 decode and render).\n"
    );
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
    if let Some((cap, _)) = &tests {
        let c = Captured {
            text: tail(&cap.text, 400),
            cmdline: cap.cmdline.clone(),
            ok: cap.ok,
            elapsed: cap.elapsed,
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
    if !missing.is_empty() {
        eprintln!(
            "==> {} prerequisite(s) missing; see the report",
            missing.len()
        );
    }
    Ok(())
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
        assert_eq!(r.passed, vec!["a::passes"]);
        assert_eq!(r.ignored, vec!["d::not_tier2"]);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].1, "no VA-API device at /dev/dri/renderD128");
        assert_eq!(r.failed.len(), 1);
        assert!(r.failed[0].1.contains("psnr"));
    }
}
