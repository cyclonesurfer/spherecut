//! The CI workflow held to what it stands for (D78): its check job
//! installs at least README's Ubuntu 24.04 packages and runs `cargo
//! xtask check` as it is, its MSRV is the workspace's `rust-version`, and
//! its compat job installs what `scripts/build-ffmpeg.sh` asks for and
//! builds against FFmpeg 8.1.3 and 9.0.2. Each rule also runs on a copy
//! doctored to break it, which must fail (D32). The test reads the files
//! as text, so it needs no YAML parser.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use spherecut_testkit::control::must_fail;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap()
}

/// The words of the shell command that starts at the first line holding
/// `start` among `lines`, its `\` continuations joined, from just after
/// `start`. `strip` comes off each line first (a comment's `#`).
fn command_words(lines: &[&str], start: &str, strip: &str) -> Vec<String> {
    let Some(first) = lines.iter().position(|l| l.contains(start)) else {
        return Vec::new();
    };
    let mut text = String::new();
    for line in &lines[first..] {
        let line = line.trim_start();
        let line = line.strip_prefix(strip).unwrap_or(line).trim();
        match line.strip_suffix('\\') {
            Some(more) => {
                text.push_str(more);
                text.push(' ');
            }
            None => {
                text.push_str(line);
                break;
            }
        }
    }
    let after = text.split_once(start).map_or("", |(_, rest)| rest);
    after.split_whitespace().map(str::to_owned).collect()
}

/// The packages README's Ubuntu 24.04 section installs.
fn readme_packages(readme: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = readme
        .lines()
        .skip_while(|l| !l.starts_with("### Ubuntu 24.04"))
        .take_while(|l| !l.starts_with("## "))
        .collect();
    command_words(&lines, "sudo apt install", "")
        .into_iter()
        .collect()
}

/// The packages `scripts/build-ffmpeg.sh` says it needs on Ubuntu 24.04,
/// in its header comment.
fn build_ffmpeg_packages(script: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = script.lines().collect();
    command_words(&lines, "sudo apt install", "#")
        .into_iter()
        .collect()
}

/// One job of the workflow: its lines from `  <name>:` to the next job
/// or the end.
fn job<'a>(workflow: &'a str, name: &str) -> Vec<&'a str> {
    workflow
        .lines()
        .skip_while(|l| *l != format!("  {name}:"))
        .skip(1)
        .take_while(|l| !(l.len() > 2 && l.starts_with("  ") && !l.starts_with("   ")))
        .collect()
}

/// The packages a job's `apt-get install -y` installs.
fn job_packages(job: &[&str]) -> BTreeSet<String> {
    command_words(job, "apt-get install -y", "")
        .into_iter()
        .collect()
}

/// The commands a job runs through `cargo xtask`, each as written.
fn xtask_runs(job: &[&str]) -> Vec<String> {
    job.iter()
        .map(|l| l.trim())
        .filter_map(|l| l.strip_prefix("run: "))
        .filter(|cmd| cmd.contains("cargo xtask"))
        .map(str::to_owned)
        .collect()
}

/// An `env:` value of the workflow, its quotes taken off.
fn env_value<'a>(workflow: &'a str, name: &str) -> Option<&'a str> {
    workflow
        .lines()
        .find_map(|l| l.trim().strip_prefix(&format!("{name}: ")))
        .map(|v| v.trim_matches('\''))
}

/// The workspace's `rust-version`.
fn rust_version(manifest: &str) -> &str {
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("rust-version = "))
        .unwrap()
        .trim_matches('"')
}

/// The files the workflow is held to.
struct Files {
    workflow: String,
    readme: String,
    build_ffmpeg: String,
    manifest: String,
}

impl Files {
    fn real() -> Files {
        Files {
            workflow: read(".github/workflows/check.yml"),
            readme: read("README.md"),
            build_ffmpeg: read("scripts/build-ffmpeg.sh"),
            manifest: read("Cargo.toml"),
        }
    }
}

/// Where the workflow parts from the files it stands for.
fn workflow_disagrees(f: &Files) -> Result<(), Vec<String>> {
    let mut why = Vec::new();
    let readme = readme_packages(&f.readme);
    let build = build_ffmpeg_packages(&f.build_ffmpeg);
    if readme.len() < 10 || build.len() < 10 {
        why.push(format!("too few packages read: {readme:?}, {build:?}"));
    }
    let check = job(&f.workflow, "check");
    let compat = job(&f.workflow, "compat");
    for p in readme.difference(&job_packages(&check)) {
        why.push(format!("check doesn't install README's {p}"));
    }
    let compat_packages = job_packages(&compat);
    for p in readme
        .union(&build)
        .filter(|p| !compat_packages.contains(*p))
    {
        why.push(format!("compat doesn't install {p}"));
    }
    let runs = xtask_runs(&check);
    if runs != ["cargo xtask check"] {
        why.push(format!(
            "check runs {runs:?}, not `cargo xtask check` alone"
        ));
    }
    let msrv = env_value(&f.workflow, "RUST_MSRV");
    let wanted = rust_version(&f.manifest);
    if msrv != Some(wanted) {
        why.push(format!("RUST_MSRV is {msrv:?}, not rust-version {wanted}"));
    }
    let compat_runs = xtask_runs(&compat);
    for tag in ["n8.1.3", "n9.0.2"] {
        let prefix = format!("~/ffmpeg/{tag}-full");
        if !compat_runs
            .iter()
            .any(|r| r.starts_with("cargo xtask compat ") && r.contains(&prefix))
        {
            why.push(format!("compat doesn't build against {tag}"));
        }
    }
    if why.is_empty() { Ok(()) } else { Err(why) }
}

#[test]
fn the_workflow_matches_the_readme_and_the_check() {
    let real = Files::real();
    assert_eq!(workflow_disagrees(&real), Ok(()));
    // The controls, each on a copy doctored to break one rule.
    let doctored = |change: &dyn Fn(&mut Files)| {
        let mut f = Files::real();
        change(&mut f);
        workflow_disagrees(&f)
    };
    let why = must_fail(
        "README asking for libfoo-dev",
        doctored(&|f| {
            f.readme = f
                .readme
                .replace(" vainfo strace", " vainfo strace libfoo-dev")
        }),
    );
    assert_eq!(
        why,
        [
            "check doesn't install README's libfoo-dev",
            "compat doesn't install libfoo-dev"
        ]
    );
    let why = must_fail(
        "build-ffmpeg.sh asking for libbar-dev",
        doctored(&|f| {
            f.build_ffmpeg = f
                .build_ffmpeg
                .replace("libshaderc-dev", "libshaderc-dev libbar-dev")
        }),
    );
    assert_eq!(why, ["compat doesn't install libbar-dev"]);
    let why = must_fail(
        "the check run without the supply chain",
        doctored(&|f| {
            f.workflow = f.workflow.replace(
                "run: cargo xtask check\n",
                "run: cargo xtask check --no-supply-chain\n",
            )
        }),
    );
    assert_eq!(
        why,
        ["check runs [\"cargo xtask check --no-supply-chain\"], not `cargo xtask check` alone"]
    );
    let why = must_fail(
        "RUST_MSRV 1.94",
        doctored(&|f| f.workflow = f.workflow.replace("RUST_MSRV: '1.95'", "RUST_MSRV: '1.94'")),
    );
    assert_eq!(why, ["RUST_MSRV is Some(\"1.94\"), not rust-version 1.95"]);
    let why = must_fail(
        "compat without 9.0.2",
        doctored(&|f| f.workflow = f.workflow.replace(" ~/ffmpeg/n9.0.2-full", "")),
    );
    assert_eq!(why, ["compat doesn't build against n9.0.2"]);
}
