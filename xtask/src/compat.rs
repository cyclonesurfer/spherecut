//! `cargo xtask compat`: the workspace against other FFmpeg builds (D2).
//!
//! ffmpeg-sys-next binds whichever FFmpeg pkg-config finds, and its
//! bindings differ between releases (FFmpeg 8 made the `SWS_` defines an
//! enum, so code that built against 6.1 failed on Pat's 8.1). For each
//! FFmpeg install prefix this runs clippy on the workspace, all targets
//! and features with `-D warnings` as `cargo xtask check` does, then
//! builds and links the Tier 1 tests and the binaries without running
//! them (clippy doesn't link, and a function `ffi.rs` declares that an
//! FFmpeg doesn't export fails only there), with pkg-config pointed at
//! that build and in a target directory of its own: sharing one would
//! rebind ffmpeg-sys-next, and rebuild everything above it, each time the
//! FFmpeg changes. A target directory records what it was built against,
//! and one built against anything else is deleted first (ffmpeg-sys-next
//! binds again only when its own variables change, not when the FFmpeg
//! behind a prefix does). `--test` also runs the Tier 1 tests against one
//! of them, loading its libraries through `LD_LIBRARY_PATH`, and `--clean`
//! deletes each target directory once its build is checked (about 1.4 GB
//! after clippy and the link, and 1.8 GB after the Tier 1 tests).

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};

use crate::{hw_test, run_step};

/// The FFmpeg prefixes `cargo xtask check` lints against, separated by
/// colons.
pub const PREFIXES_ENV: &str = "SPHERECUT_FFMPEG_COMPAT";
/// Where the per-FFmpeg target directories go, instead of `compat/` in
/// cargo's target directory.
pub const TARGET_ENV: &str = "SPHERECUT_FFMPEG_COMPAT_TARGET_DIR";
/// The file in each target directory that says what it was built against.
const STAMP_FILE: &str = "spherecut-ffmpeg-stamp";
/// The libraries ffmpeg-sys-next binds, whose pkg-config files a stamp
/// covers.
const STAMPED: [&str; 7] = [
    "libavcodec",
    "libavdevice",
    "libavfilter",
    "libavformat",
    "libavutil",
    "libswresample",
    "libswscale",
];

#[derive(clap::Args)]
pub struct Args {
    /// FFmpeg install prefixes, such as /opt/ffmpeg-8.1 (default: the
    /// colon-separated list in SPHERECUT_FFMPEG_COMPAT).
    #[arg(value_name = "PREFIX")]
    prefixes: Vec<PathBuf>,
    /// Also run the Tier 1 tests against this prefix, which is linted too
    /// if it isn't listed.
    #[arg(long, value_name = "PREFIX")]
    test: Option<PathBuf>,
    /// Delete each build's target directory once it is checked (by
    /// default they are kept, and later runs build only what changed).
    #[arg(long)]
    clean: bool,
}

pub fn run(a: Args) -> Result<()> {
    let env = std::env::var_os(PREFIXES_ENV);
    let prefixes = prefix_list(&a.prefixes, env.as_deref(), a.test.as_deref());
    if prefixes.is_empty() {
        bail!(
            "no FFmpeg prefixes: name them (`cargo xtask compat /opt/ffmpeg-8.1 ...`) \
             or list them in {PREFIXES_ENV}, separated by colons"
        );
    }
    let test = a.test.as_deref().map(absolute);
    check(&prefixes, test.as_deref(), a.clean)
}

/// `cargo xtask check`'s stage: clippy and the link against each FFmpeg
/// in `SPHERECUT_FFMPEG_COMPAT`, or a line saying it was skipped.
pub fn from_check() -> Result<()> {
    let env = std::env::var_os(PREFIXES_ENV);
    let prefixes = prefix_list(&[], env.as_deref(), None);
    if prefixes.is_empty() {
        let why = if env.is_none() {
            "is not set"
        } else {
            "lists no prefix"
        };
        eprintln!(
            "==> skipping clippy and the link against other FFmpeg builds: \
             {PREFIXES_ENV} {why} (FFmpeg prefixes separated by colons; \
             see `cargo xtask compat --help`)"
        );
        return Ok(());
    }
    check(&prefixes, None, false)
}

/// The prefixes to check, made absolute: those named, else those in the
/// variable, with the `--test` prefix added when it isn't among them.
/// Empty entries are left out and each prefix comes once.
fn prefix_list(args: &[PathBuf], env: Option<&OsStr>, test: Option<&Path>) -> Vec<PathBuf> {
    let listed: Vec<PathBuf> = if args.is_empty() {
        env.map(|v| std::env::split_paths(v).collect())
            .unwrap_or_default()
    } else {
        args.to_vec()
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for p in listed.iter().map(PathBuf::as_path).chain(test) {
        if p.as_os_str().is_empty() {
            continue;
        }
        let p = absolute(p);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// The path made absolute without touching the file system (cargo runs
/// build scripts in their package's directory, so a relative
/// `PKG_CONFIG_PATH` would point elsewhere there).
fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The name of a prefix's target directory: its last component, with
/// anything but ASCII letters, digits, `.`, `_` and `-` replaced.
fn label(prefix: &Path) -> String {
    let name = prefix
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "root".into()
    } else {
        clean
    }
}

/// One FFmpeg to check.
#[derive(Debug)]
struct Build {
    prefix: PathBuf,
    label: String,
    /// The directory holding its `libavcodec.pc`, or why there is none.
    pc_dir: Result<PathBuf, String>,
}

fn builds(prefixes: &[PathBuf]) -> Result<Vec<Build>> {
    let builds: Vec<Build> = prefixes
        .iter()
        .map(|p| Build {
            prefix: p.clone(),
            label: label(p),
            pc_dir: pkgconfig_dir(p).ok_or_else(|| {
                format!(
                    "not an FFmpeg prefix: no libavcodec.pc in {} or {}",
                    ["lib/pkgconfig", "lib64/pkgconfig"]
                        .map(|d| p.join(d).display().to_string())
                        .join(", "),
                    p.join("lib/*/pkgconfig").display()
                )
            }),
        })
        .collect();
    for (i, a) in builds.iter().enumerate() {
        if let Some(b) = builds[i + 1..].iter().find(|b| b.label == a.label) {
            bail!(
                "{} and {} would share the target directory compat/{}: \
                 reach one of them through a symlink of another name",
                a.prefix.display(),
                b.prefix.display(),
                a.label
            );
        }
    }
    Ok(builds)
}

/// Where pkg-config finds the prefix's FFmpeg: `lib/pkgconfig`,
/// `lib64/pkgconfig`, or a multiarch `lib/<triplet>/pkgconfig` (so the
/// system's `/usr` works too).
fn pkgconfig_dir(prefix: &Path) -> Option<PathBuf> {
    let mut dirs = vec![
        prefix.join("lib").join("pkgconfig"),
        prefix.join("lib64").join("pkgconfig"),
    ];
    if let Ok(entries) = std::fs::read_dir(prefix.join("lib")) {
        let mut more: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("pkgconfig"))
            .collect();
        more.sort();
        dirs.extend(more);
    }
    dirs.into_iter().find(|d| d.join("libavcodec.pc").is_file())
}

/// What a prefix's pkg-config files say the bindings are built against.
#[derive(Debug, Clone, PartialEq)]
struct Bound {
    /// libavcodec's version, as pkg-config gives it.
    libavcodec: String,
    /// `FFMPEG_VERSION` from the installed `libavutil/ffversion.h`.
    ffmpeg: Option<String>,
    /// That header, where pkg-config says it is.
    version_header: Option<PathBuf>,
    /// libavcodec's library directory, for `LD_LIBRARY_PATH`.
    libdir: PathBuf,
}

impl Bound {
    fn describe(&self) -> String {
        match &self.ffmpeg {
            Some(v) => format!("FFmpeg {v}, libavcodec {}", self.libavcodec),
            None => format!("libavcodec {}", self.libavcodec),
        }
    }
}

/// Asks pkg-config (`program`), with the search path the build will
/// have, which libavcodec it finds, and fails unless it is `prefix`'s:
/// its `.pc` file in `pc_dir`, and the library and header it names inside
/// the prefix, symlinks resolved. ffmpeg-sys-next would otherwise bind
/// another FFmpeg (the system's, say) without a word, as it would through
/// a prefix's `.pc` file whose `libdir` or `includedir` lead elsewhere.
fn bound(program: &OsStr, prefix: &Path, pc_dir: &Path, search: &OsStr) -> Result<Bound> {
    let query = |args: &[&str]| pkg_config(program, search, args);
    let found = PathBuf::from(query(&["--variable=pcfiledir", "libavcodec"])?);
    if !same_dir(&found, pc_dir) {
        bail!(
            "pkg-config finds libavcodec in {}, not in {}",
            found.display(),
            pc_dir.display()
        );
    }
    let libavcodec = query(&["--modversion", "libavcodec"])?
        .to_string_lossy()
        .into_owned();
    let libdir = PathBuf::from(query(&["--variable=libdir", "libavcodec"])?);
    let includedir = PathBuf::from(query(&["--variable=includedir", "libavcodec"])?);
    let home = prefix
        .canonicalize()
        .with_context(|| format!("could not resolve {}", prefix.display()))?;
    for (what, file) in [
        ("library", libdir.join("libavcodec.so")),
        ("header", includedir.join("libavcodec/avcodec.h")),
    ] {
        let real = file.canonicalize().with_context(|| {
            format!(
                "libavcodec's {what} {} isn't there, where its pkg-config file says",
                file.display()
            )
        })?;
        if !real.starts_with(&home) {
            bail!(
                "libavcodec's {what} {} is {}, outside the prefix {}",
                file.display(),
                real.display(),
                home.display()
            );
        }
    }
    let version_header = query(&["--variable=includedir", "libavutil"])
        .ok()
        .map(|inc| Path::new(&inc).join("libavutil/ffversion.h"));
    let ffmpeg = version_header
        .as_deref()
        .and_then(|h| std::fs::read_to_string(h).ok())
        .and_then(|h| ffmpeg_version(&h));
    Ok(Bound {
        libavcodec,
        ffmpeg,
        version_header,
        libdir,
    })
}

/// What a prefix holds now, as a target directory's stamp records it: the
/// directory pkg-config reads, symlinks resolved, the versions found, and
/// the size and modification time of each bound library's pkg-config file
/// and of `ffversion.h`, which change when the FFmpeg there is installed
/// again.
fn stamp(pc_dir: &Path, b: &Bound) -> String {
    let dir = pc_dir
        .canonicalize()
        .unwrap_or_else(|_| pc_dir.to_path_buf());
    let mut out = format!("pkg-config directory {}\n{}\n", dir.display(), b.describe());
    let files = STAMPED
        .iter()
        .map(|lib| dir.join(format!("{lib}.pc")))
        .chain(b.version_header.clone())
        .map(|f| f.canonicalize().unwrap_or(f));
    for f in files {
        let _ = match std::fs::metadata(&f) {
            Ok(m) => {
                let modified = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or_else(
                        || "at an unknown time".into(),
                        |d| format!("at {}.{:09}", d.as_secs(), d.subsec_nanos()),
                    );
                writeln!(
                    out,
                    "{} {} bytes, modified {modified}",
                    f.display(),
                    m.len()
                )
            }
            Err(_) => writeln!(out, "{} missing", f.display()),
        };
    }
    out
}

/// Makes `target` ready for a build against what `stamp` describes. One
/// built against anything else, or with no stamp, is deleted first: cargo
/// would keep its bindings, since ffmpeg-sys-next binds again only when
/// its own files or the variables it reads change, not when the FFmpeg
/// behind a prefix does (a distro upgrade of `/usr`, a prefix built again
/// in place, a symlink pointed elsewhere). `true` when one was deleted.
fn prepare_target(target: &Path, stamp: &str) -> Result<bool> {
    let file = target.join(STAMP_FILE);
    let stale = target.exists() && std::fs::read_to_string(&file).ok().as_deref() != Some(stamp);
    if stale {
        std::fs::remove_dir_all(target)
            .with_context(|| format!("could not remove {}", target.display()))?;
    }
    std::fs::create_dir_all(target)
        .with_context(|| format!("could not create {}", target.display()))?;
    std::fs::write(&file, stamp).with_context(|| format!("could not write {}", file.display()))?;
    Ok(stale)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The pkg-config that ffmpeg-sys-next's build runs, through the
/// pkg-config crate, given this process's `vars`: `PKG_CONFIG` when it is
/// set, else `pkg-config` (the crate also tries `pkgconf` when there is no
/// `pkg-config` to run, as [`pkg_config`] does).
fn pkg_config_program(vars: &[(OsString, OsString)]) -> OsString {
    get(vars, "PKG_CONFIG").unwrap_or_else(|| "pkg-config".into())
}

/// One query's output from pkg-config (`program`), trimmed, as bytes (a
/// path need not be UTF-8).
fn pkg_config(program: &OsStr, search: &OsStr, args: &[&str]) -> Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let run = |program: &OsStr| {
        Command::new(program)
            .args(args)
            .env("PKG_CONFIG_PATH", search)
            .output()
    };
    let out = match run(program) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && program == "pkg-config" => {
            run(OsStr::new("pkgconf"))
        }
        r => r,
    }
    .with_context(|| format!("could not run {}", program.display()))?;
    if !out.status.success() {
        bail!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut text = out.stdout;
    while text.last().is_some_and(u8::is_ascii_whitespace) {
        text.pop();
    }
    Ok(OsString::from_vec(text))
}

/// The version in `ffversion.h`: `#define FFMPEG_VERSION "n8.1.3"`.
fn ffmpeg_version(header: &str) -> Option<String> {
    header
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FFMPEG_VERSION"))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

/// `dir` ahead of the list a variable already holds.
fn prepend(dir: &Path, existing: Option<OsString>) -> OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Some(e) = existing {
        paths.extend(std::env::split_paths(&e).filter(|p| !p.as_os_str().is_empty()));
    }
    // A directory with a colon in its name can't be in such a list at
    // all; give it alone then, and pkg-config or the loader says what it
    // can't find.
    std::env::join_paths(&paths).unwrap_or_else(|_| dir.as_os_str().to_owned())
}

/// A variable's value in `vars`, this process's environment (a
/// parameter, for the tests).
fn get(vars: &[(OsString, OsString)], name: &str) -> Option<OsString> {
    vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

/// The variables among `names` that would have ffmpeg-sys-next bind
/// another FFmpeg than the one `PKG_CONFIG_PATH` leads to, which `bound`
/// checks: `FFMPEG_DIR`, which it uses before pkg-config, and the pkg-config
/// crate's forms of pkg-config's variables for one target
/// (`PKG_CONFIG_PATH_x86_64-unknown-linux-gnu`, its form with
/// underscores, `HOST_PKG_CONFIG_PATH`, `TARGET_PKG_CONFIG_PATH`), which
/// it takes over the plain ones that pkg-config itself reads. Sorted.
fn overriding<'a>(names: impl Iterator<Item = &'a OsStr>) -> Vec<String> {
    const BASES: [&str; 4] = [
        "PKG_CONFIG_PATH",
        "PKG_CONFIG_LIBDIR",
        "PKG_CONFIG_SYSROOT_DIR",
        "PKG_CONFIG",
    ];
    let mut out: Vec<String> = names
        .filter_map(OsStr::to_str)
        .filter(|n| {
            *n == "FFMPEG_DIR"
                || BASES.iter().any(|b| {
                    let host = n
                        .strip_prefix("HOST_")
                        .or_else(|| n.strip_prefix("TARGET_"));
                    // A target triple is lower case, and pkg-config's own
                    // variables (`PKG_CONFIG_ALLOW_CROSS`) are upper.
                    let triple = n.strip_prefix(b).and_then(|r| r.strip_prefix('_'));
                    host == Some(b)
                        || triple.is_some_and(|t| {
                            !t.is_empty() && !t.bytes().any(|c| c.is_ascii_uppercase())
                        })
                })
        })
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The variables cargo runs with for one FFmpeg, given this process's
/// `vars`: each with its value, or `None` for one it runs without.
fn cargo_env(
    pc_dir: &Path,
    target: &Path,
    libdir: Option<&Path>,
    vars: &[(OsString, OsString)],
) -> Vec<(String, Option<OsString>)> {
    let mut env = vec![
        (
            "PKG_CONFIG_PATH".to_owned(),
            Some(prepend(pc_dir, get(vars, "PKG_CONFIG_PATH"))),
        ),
        (
            "CARGO_TARGET_DIR".to_owned(),
            Some(target.as_os_str().to_owned()),
        ),
    ];
    // Low debuginfo and no incremental state: one target directory per
    // FFmpeg adds up. Settings already made win.
    for (var, value) in [
        ("CARGO_PROFILE_DEV_DEBUG", "line-tables-only"),
        ("CARGO_INCREMENTAL", "0"),
    ] {
        if get(vars, var).is_none() {
            env.push((var.to_owned(), Some(value.into())));
        }
    }
    if let Some(dir) = libdir {
        env.push((
            "LD_LIBRARY_PATH".to_owned(),
            Some(prepend(dir, get(vars, "LD_LIBRARY_PATH"))),
        ));
    }
    env.extend(
        overriding(vars.iter().map(|(k, _)| k.as_os_str()))
            .into_iter()
            .map(|n| (n, None)),
    );
    env
}

/// `cargo` with `args`, run with `env`.
fn cargo(args: &[&str], env: &[(String, Option<OsString>)]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args(args);
    for (k, v) in env {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    cmd
}

/// What became of one stage for one FFmpeg.
#[derive(Debug, Clone, PartialEq)]
enum Stage {
    Passed,
    Failed(String),
    NotRun(String),
}

impl Stage {
    fn of(r: Result<()>) -> Stage {
        match r {
            Ok(()) => Stage::Passed,
            Err(e) => Stage::Failed(format!("{e:#}")),
        }
    }

    fn word(&self) -> &'static str {
        match self {
            Stage::Passed => "passed",
            Stage::Failed(_) => "FAILED",
            Stage::NotRun(_) => "not run",
        }
    }
}

/// One FFmpeg's line of the report.
#[derive(Debug)]
struct Row {
    label: String,
    prefix: PathBuf,
    bound: Result<Bound, String>,
    clippy: Stage,
    /// The tests and binaries built and linked, once clippy has passed.
    link: Option<Stage>,
    /// The Tier 1 tests, for the `--test` prefix.
    tier1: Option<Stage>,
}

impl Row {
    fn failed(&self) -> bool {
        self.bound.is_err()
            || self.clippy != Stage::Passed
            || self.link != Some(Stage::Passed)
            || self.tier1.as_ref().is_some_and(|t| *t != Stage::Passed)
    }
}

fn report(rows: &[Row]) -> String {
    let what: Vec<String> = rows
        .iter()
        .map(|r| match &r.bound {
            Ok(b) => b.describe(),
            Err(_) => "-".into(),
        })
        .collect();
    let lw = rows.iter().map(|r| r.label.len()).max().unwrap_or(0);
    let ww = what.iter().map(String::len).max().unwrap_or(0);
    let mut out = format!(
        "==> FFmpeg compatibility: {} of {} builds passed\n",
        rows.iter().filter(|r| !r.failed()).count(),
        rows.len()
    );
    for (r, w) in rows.iter().zip(&what) {
        let mut result = match &r.bound {
            Err(e) => e.clone(),
            Ok(_) => format!("clippy {}", r.clippy.word()),
        };
        if let Some(l) = &r.link {
            let _ = write!(result, ", link {}", l.word());
        }
        if let Some(t) = &r.tier1 {
            let _ = write!(result, ", Tier 1 {}", t.word());
        }
        let _ = writeln!(out, "  {:lw$}  {w:ww$}  {result}", r.label);
        let _ = writeln!(out, "  {:lw$}  {}", "", r.prefix.display());
        for s in std::iter::once(&r.clippy).chain(&r.link).chain(&r.tier1) {
            if let Stage::Failed(why) | Stage::NotRun(why) = s {
                let _ = writeln!(out, "  {:lw$}  {why}", "");
            }
        }
    }
    out
}

/// cargo's target directory for one FFmpeg.
fn target_root() -> PathBuf {
    std::env::var_os(TARGET_ENV)
        .filter(|v| !v.is_empty())
        .map(|v| absolute(Path::new(&v)))
        .unwrap_or_else(|| hw_test::target_dir(&crate::workspace_root()).join("compat"))
}

fn check(prefixes: &[PathBuf], test: Option<&Path>, clean: bool) -> Result<()> {
    let root = target_root();
    let vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let unset = overriding(vars.iter().map(|(k, _)| k.as_os_str()));
    if !unset.is_empty() {
        eprintln!(
            "==> running cargo without {}: ffmpeg-sys-next would bind the FFmpeg \
             they lead to rather than each prefix's",
            unset.join(", ")
        );
    }
    let program = pkg_config_program(&vars);
    let search = |pc: &Path| prepend(pc, get(&vars, "PKG_CONFIG_PATH"));
    let mut rows = Vec::new();
    for b in builds(prefixes)? {
        let target = root.join(&b.label);
        let found = b.pc_dir.clone().and_then(|pc| {
            bound(&program, &b.prefix, &pc, &search(&pc)).map_err(|e| format!("{e:#}"))
        });
        let tested = test == Some(b.prefix.as_path());
        let mut row = Row {
            label: b.label,
            prefix: b.prefix,
            bound: found,
            clippy: Stage::NotRun("its FFmpeg was not found".into()),
            link: None,
            tier1: tested
                .then(|| Stage::NotRun("Tier 1 needs clippy and the link to pass first".into())),
        };
        if let (Ok(pc), Ok(found)) = (&b.pc_dir, &row.bound) {
            let what = format!("against {} at {}", found.describe(), row.prefix.display());
            let env = cargo_env(pc, &target, None, &vars);
            row.clippy = match prepare_target(&target, &stamp(pc, found)) {
                Err(e) => Stage::Failed(format!("{e:#}")),
                Ok(stale) => {
                    if stale {
                        eprintln!(
                            "==> removed {}: it wasn't built against the FFmpeg {} \
                             holds now",
                            target.display(),
                            row.prefix.display()
                        );
                    }
                    Stage::of(run_step(
                        &format!("clippy {what} (target {})", target.display()),
                        &mut cargo(
                            &[
                                "clippy",
                                "--workspace",
                                "--all-targets",
                                "--all-features",
                                "--",
                                "-D",
                                "warnings",
                            ],
                            &env,
                        ),
                    ))
                }
            };
            if row.clippy == Stage::Passed {
                let link = Stage::of(run_step(
                    &format!("link: the tests and binaries built {what}"),
                    &mut cargo(&["test", "--workspace", "--all-targets", "--no-run"], &env),
                ));
                if link == Stage::Passed && row.tier1.is_some() {
                    let env = cargo_env(pc, &target, Some(&found.libdir), &vars);
                    row.tier1 = Some(Stage::of(
                        run_step(
                            &format!("tests (Tier 1) {what}"),
                            &mut cargo(&["test", "--workspace", "--all-targets"], &env),
                        )
                        .and_then(|()| {
                            run_step(
                                &format!("doc tests {what}"),
                                &mut cargo(&["test", "--workspace", "--doc"], &env),
                            )
                        }),
                    ));
                }
                row.link = Some(link);
            }
        }
        if clean && target.exists() {
            match std::fs::remove_dir_all(&target) {
                Ok(()) => eprintln!("==> removed {}", target.display()),
                Err(e) => eprintln!("warning: could not remove {}: {e}", target.display()),
            }
        }
        rows.push(row);
    }
    eprint!("{}", report(&rows));
    let failed: Vec<&str> = rows
        .iter()
        .filter(|r| r.failed())
        .map(|r| r.label.as_str())
        .collect();
    if !failed.is_empty() {
        bail!(
            "the workspace fails against {} of {} FFmpeg builds: {}",
            failed.len(),
            rows.len(),
            failed.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn parse(argv: &[&str]) -> Result<Args, clap::Error> {
        use clap::Parser as _;
        Cli::try_parse_from(std::iter::once("compat").chain(argv.iter().copied())).map(|c| c.args)
    }

    #[test]
    fn the_arguments_are_prefixes_a_test_prefix_and_clean() {
        let a = parse(&["/ff/n8.1", "/ff/n9.0", "--test", "/ff/n8.1.3"]).unwrap();
        assert_eq!(a.prefixes, [p("/ff/n8.1"), p("/ff/n9.0")]);
        assert_eq!(a.test, Some(p("/ff/n8.1.3")));
        assert!(!a.clean);
        let a = parse(&["--clean", "--test=/ff/n8.1"]).unwrap();
        assert!(a.prefixes.is_empty());
        assert_eq!(a.test, Some(p("/ff/n8.1")));
        assert!(a.clean);
        assert!(parse(&["--test"]).is_err());
        assert!(parse(&["--tests", "/ff/n8.1"]).is_err());
    }

    #[test]
    fn prefixes_come_from_the_arguments_else_the_variable() {
        let env = OsStr::new("/opt/ff/n8.1::/opt/ff/n9.0:");
        assert_eq!(
            prefix_list(&[], Some(env), None),
            [p("/opt/ff/n8.1"), p("/opt/ff/n9.0")]
        );
        assert_eq!(
            prefix_list(&[p("/opt/ff/n7.1")], Some(env), None),
            [p("/opt/ff/n7.1")]
        );
        assert!(prefix_list(&[], None, None).is_empty());
        assert!(prefix_list(&[], Some(OsStr::new("")), None).is_empty());
        assert!(prefix_list(&[], Some(OsStr::new(":")), None).is_empty());
    }

    #[test]
    fn the_test_prefix_is_linted_too_and_each_prefix_once() {
        let env = OsStr::new("/opt/ff/n8.1:/opt/ff/n9.0:/opt/ff/n8.1");
        assert_eq!(
            prefix_list(&[], Some(env), Some(Path::new("/opt/ff/n9.0"))),
            [p("/opt/ff/n8.1"), p("/opt/ff/n9.0")]
        );
        assert_eq!(
            prefix_list(&[], Some(env), Some(Path::new("/opt/ff/n7.1"))),
            [p("/opt/ff/n8.1"), p("/opt/ff/n9.0"), p("/opt/ff/n7.1")]
        );
        assert_eq!(
            prefix_list(&[], None, Some(Path::new("/opt/ff/n7.1"))),
            [p("/opt/ff/n7.1")]
        );
    }

    #[test]
    fn relative_prefixes_are_made_absolute() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            prefix_list(&[p("ff/n8.1")], None, Some(Path::new("ff/n8.1"))),
            [here.join("ff/n8.1")]
        );
    }

    #[test]
    fn labels_are_the_last_component_made_safe() {
        assert_eq!(label(Path::new("/opt/ff/n8.1.3-full")), "n8.1.3-full");
        assert_eq!(label(Path::new("/opt/ff/n8.1/")), "n8.1");
        assert_eq!(label(Path::new("/opt/ffmpeg 8 (rpm)")), "ffmpeg_8__rpm_");
        assert_eq!(label(Path::new("/usr")), "usr");
        assert_eq!(label(Path::new("/")), "root");
    }

    #[test]
    fn two_prefixes_with_one_label_are_refused() {
        let e = builds(&[p("/a/n8.1"), p("/b/n8.1")])
            .unwrap_err()
            .to_string();
        assert!(e.contains("/a/n8.1 and /b/n8.1"), "{e}");
        assert!(e.contains("compat/n8.1"), "{e}");
        assert!(builds(&[p("/a/n8.1"), p("/a/n9.0")]).is_ok());
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "spherecut-xtask-compat-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_pkgconfig_dir_is_found_under_lib_lib64_or_a_multiarch_dir() {
        let d = scratch("pcdir");
        for (prefix, sub) in [
            ("plain", "lib/pkgconfig"),
            ("fedora", "lib64/pkgconfig"),
            ("debian", "lib/x86_64-linux-gnu/pkgconfig"),
        ] {
            let pc = d.join(prefix).join(sub);
            std::fs::create_dir_all(&pc).unwrap();
            std::fs::write(pc.join("libavcodec.pc"), "").unwrap();
            assert_eq!(pkgconfig_dir(&d.join(prefix)), Some(pc), "{prefix}");
        }
        // A pkgconfig directory without libavcodec doesn't count.
        std::fs::create_dir_all(d.join("other/lib/pkgconfig")).unwrap();
        std::fs::write(d.join("other/lib/pkgconfig/libavutil.pc"), "").unwrap();
        assert_eq!(pkgconfig_dir(&d.join("other")), None);
        assert_eq!(pkgconfig_dir(&d.join("missing")), None);
        let b = builds(&[d.join("other")]).unwrap();
        let e = b[0].pc_dir.as_ref().unwrap_err();
        assert!(
            e.starts_with("not an FFmpeg prefix: no libavcodec.pc in "),
            "{e}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_prefix_pkg_config_does_not_find_is_refused() {
        // A libavcodec.pc that pkg-config can't read: the system's must not
        // be bound in its place.
        let d = scratch("unreadable");
        let pc = d.join("lib/pkgconfig");
        std::fs::create_dir_all(&pc).unwrap();
        std::fs::write(pc.join("libavcodec.pc"), "not a pc file\n").unwrap();
        assert!(bound(OsStr::new("pkg-config"), &d, &pc, pc.as_os_str()).is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_ffmpeg_version_is_read_from_ffversion_h() {
        let h = "/* Automatically generated by version.sh, do not manually edit! */\n\
                 #ifndef AVUTIL_FFVERSION_H\n\
                 #define AVUTIL_FFVERSION_H\n\
                 #define FFMPEG_VERSION \"n8.1.3\"\n\
                 #endif /* AVUTIL_FFVERSION_H */\n";
        assert_eq!(ffmpeg_version(h).as_deref(), Some("n8.1.3"));
        assert_eq!(
            ffmpeg_version("#define FFMPEG_VERSION \"6.1.1-3ubuntu5\"").as_deref(),
            Some("6.1.1-3ubuntu5")
        );
        assert_eq!(ffmpeg_version("#define FFMPEG_VERSION \"\""), None);
        assert_eq!(ffmpeg_version(""), None);
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(k, v)| (k.into(), v.into())).collect()
    }

    fn value(env: &[(String, Option<OsString>)], k: &str) -> Option<Option<OsString>> {
        env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn cargo_gets_the_prefix_first_its_own_target_and_low_debuginfo() {
        let env = cargo_env(
            Path::new("/ff/lib/pkgconfig"),
            Path::new("/t/compat/ff"),
            None,
            &[],
        );
        let set = |env: &[(String, Option<OsString>)], k: &str| value(env, k).flatten();
        assert_eq!(
            set(&env, "PKG_CONFIG_PATH"),
            Some("/ff/lib/pkgconfig".into())
        );
        assert_eq!(set(&env, "CARGO_TARGET_DIR"), Some("/t/compat/ff".into()));
        assert_eq!(
            set(&env, "CARGO_PROFILE_DEV_DEBUG"),
            Some("line-tables-only".into())
        );
        assert_eq!(set(&env, "CARGO_INCREMENTAL"), Some("0".into()));
        // The libraries are on the loader's path only for the tests.
        assert_eq!(value(&env, "LD_LIBRARY_PATH"), None);
        assert!(env.iter().all(|(_, v)| v.is_some()), "{env:?}");

        let existing = vars(&[
            ("PKG_CONFIG_PATH", "/vk/lib/pkgconfig"),
            ("LD_LIBRARY_PATH", "/x/lib:"),
            ("CARGO_PROFILE_DEV_DEBUG", "0"),
        ]);
        let env = cargo_env(
            Path::new("/ff/lib/pkgconfig"),
            Path::new("/t/compat/ff"),
            Some(Path::new("/ff/lib")),
            &existing,
        );
        assert_eq!(
            set(&env, "PKG_CONFIG_PATH"),
            Some("/ff/lib/pkgconfig:/vk/lib/pkgconfig".into())
        );
        assert_eq!(set(&env, "LD_LIBRARY_PATH"), Some("/ff/lib:/x/lib".into()));
        // The caller's own setting stays.
        assert_eq!(value(&env, "CARGO_PROFILE_DEV_DEBUG"), None);
    }

    #[test]
    fn cargo_runs_without_what_would_bind_another_ffmpeg() {
        let existing = vars(&[
            ("FFMPEG_DIR", "/ff/n7.1"),
            ("HOST_PKG_CONFIG_PATH", "/ff/n7.1/lib/pkgconfig"),
            (
                "PKG_CONFIG_PATH_x86_64-unknown-linux-gnu",
                "/ff/n7.1/lib/pkgconfig",
            ),
            (
                "PKG_CONFIG_PATH_x86_64_unknown_linux_gnu",
                "/ff/n7.1/lib/pkgconfig",
            ),
            ("TARGET_PKG_CONFIG_LIBDIR", "/x"),
            ("PKG_CONFIG_SYSROOT_DIR_aarch64-unknown-linux-gnu", "/x"),
            ("HOST_PKG_CONFIG", "/usr/bin/pkgconf"),
            // pkg-config reads these as they are, for `bound` as for the
            // build, and `bound` runs the `PKG_CONFIG` the build runs.
            ("PKG_CONFIG", "/usr/bin/pkgconf"),
            ("PKG_CONFIG_LIBDIR", "/x"),
            ("PKG_CONFIG_SYSROOT_DIR", "/x"),
            ("PKG_CONFIG_ALLOW_CROSS", "1"),
            ("PKG_CONFIG_ALLOW_SYSTEM_LIBS", "1"),
            ("HOST_PKG_CONFIG_ALLOW_CROSS", "1"),
            ("FFMPEG_DIRS", "/x"),
            ("PATH", "/usr/bin"),
        ]);
        let unset: Vec<&str> = [
            "FFMPEG_DIR",
            "HOST_PKG_CONFIG",
            "HOST_PKG_CONFIG_PATH",
            "PKG_CONFIG_PATH_x86_64-unknown-linux-gnu",
            "PKG_CONFIG_PATH_x86_64_unknown_linux_gnu",
            "PKG_CONFIG_SYSROOT_DIR_aarch64-unknown-linux-gnu",
            "TARGET_PKG_CONFIG_LIBDIR",
        ]
        .into();
        assert_eq!(
            overriding(existing.iter().map(|(k, _)| k.as_os_str())),
            unset
        );
        let env = cargo_env(
            Path::new("/ff/n8.1/lib/pkgconfig"),
            Path::new("/t/compat/n8.1"),
            None,
            &existing,
        );
        for name in &unset {
            assert_eq!(value(&env, name), Some(None), "{name}");
        }
        for kept in [
            "PKG_CONFIG",
            "PKG_CONFIG_LIBDIR",
            "PKG_CONFIG_ALLOW_CROSS",
            "PATH",
        ] {
            assert_eq!(value(&env, kept), None, "{kept}");
        }
        assert_eq!(
            value(&env, "PKG_CONFIG_PATH"),
            Some(Some("/ff/n8.1/lib/pkgconfig".into()))
        );
        // And a command made with it runs without them.
        let cmd = cargo(&["clippy"], &env);
        let removed: Vec<&OsStr> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k)
            .collect();
        assert_eq!(removed.len(), unset.len(), "{removed:?}");
        assert!(unset.iter().all(|n| removed.contains(&OsStr::new(n))));
    }

    /// A prefix with the files a stamp covers, and what `bound` would say
    /// of it.
    fn fake_prefix(dir: &Path, version: &str) -> (PathBuf, Bound) {
        let pc = dir.join("lib/pkgconfig");
        std::fs::create_dir_all(&pc).unwrap();
        for lib in STAMPED {
            std::fs::write(
                pc.join(format!("{lib}.pc")),
                format!("Version: {version}\n"),
            )
            .unwrap();
        }
        let header = dir.join("include/libavutil/ffversion.h");
        std::fs::create_dir_all(header.parent().unwrap()).unwrap();
        std::fs::write(&header, format!("#define FFMPEG_VERSION \"n{version}\"\n")).unwrap();
        let b = Bound {
            libavcodec: version.into(),
            ffmpeg: Some(format!("n{version}")),
            version_header: Some(header),
            libdir: dir.join("lib"),
        };
        (pc, b)
    }

    #[test]
    fn a_target_built_against_another_ffmpeg_is_deleted_first() {
        let d = scratch("stamp");
        let target = d.join("compat/ff");
        let (pc71, b71) = fake_prefix(&d.join("n7.1"), "61.19.100");
        let (pc81, b81) = fake_prefix(&d.join("n8.1"), "62.28.100");
        // The prefix is a symlink, as in the review that found this:
        // pointed at another build, its path and label stay the same.
        let ff = d.join("ff");
        std::os::unix::fs::symlink(d.join("n7.1"), &ff).unwrap();
        let pc = ff.join("lib/pkgconfig");
        let built = |t: &Path| t.join("debug/build/ffmpeg-sys-next/out/bindings.rs");
        let build = |t: &Path| {
            std::fs::create_dir_all(built(t).parent().unwrap()).unwrap();
            std::fs::write(built(t), "61.19.100").unwrap();
        };

        let s71 = stamp(&pc, &b71);
        assert!(!prepare_target(&target, &s71).unwrap());
        build(&target);
        // The same FFmpeg again: the build is kept.
        assert_eq!(stamp(&pc, &b71), s71);
        assert!(!prepare_target(&target, &stamp(&pc, &b71)).unwrap());
        assert!(built(&target).is_file());
        assert_eq!(
            std::fs::read_to_string(target.join(STAMP_FILE)).unwrap(),
            s71
        );

        // The symlink now leads to 8.1: the target is deleted and stamped
        // for it.
        std::fs::remove_file(&ff).unwrap();
        std::os::unix::fs::symlink(d.join("n8.1"), &ff).unwrap();
        let s81 = stamp(&pc, &b81);
        assert_ne!(s81, s71);
        assert_eq!(s81, stamp(&pc81, &b81));
        assert!(
            s81.contains("FFmpeg n62.28.100, libavcodec 62.28.100"),
            "{s81}"
        );
        assert!(prepare_target(&target, &s81).unwrap());
        assert!(!built(&target).exists());
        assert_eq!(
            std::fs::read_to_string(target.join(STAMP_FILE)).unwrap(),
            s81
        );

        // The same version installed again in place, as a distro upgrade
        // or a rebuild of the prefix does it: its files change.
        build(&target);
        std::fs::write(
            pc81.join("libavformat.pc"),
            "Version: 62.12.100\nLibs: -lavformat\n",
        )
        .unwrap();
        let again = stamp(&pc, &b81);
        assert_ne!(again, s81);
        assert!(prepare_target(&target, &again).unwrap());
        assert!(!built(&target).exists());

        // A target with no stamp (from a compat before stamps, or made by
        // hand) can't be trusted either.
        build(&target);
        std::fs::remove_file(target.join(STAMP_FILE)).unwrap();
        assert!(prepare_target(&target, &again).unwrap());
        assert!(!built(&target).exists());

        // A file the stamp covers that is gone says so.
        std::fs::remove_file(pc71.join("libavdevice.pc")).unwrap();
        let s = stamp(&pc71, &b71);
        assert!(s.contains("libavdevice.pc missing"), "{s}");
        std::fs::remove_dir_all(&d).ok();
    }

    fn symlink(to: &Path, at: &Path) {
        std::os::unix::fs::symlink(to, at).unwrap();
    }

    /// A prefix at `dir` whose libavcodec.pc names `libdir` and
    /// `includedir`, and the directory holding that file.
    fn pc_prefix(dir: &Path, libdir: &Path, includedir: &Path) -> PathBuf {
        let pc = dir.join("lib/pkgconfig");
        std::fs::create_dir_all(&pc).unwrap();
        let text = format!(
            "libdir={}\nincludedir={}\n\nName: libavcodec\nDescription: FFmpeg codec library\n\
             Version: 61.19.100\nLibs: -L${{libdir}} -lavcodec\nCflags: -I${{includedir}}\n",
            libdir.display(),
            includedir.display()
        );
        std::fs::write(pc.join("libavcodec.pc"), text).unwrap();
        pc
    }

    /// libavcodec's library in `lib` and its header in `include`.
    fn install(lib: &Path, include: &Path) {
        std::fs::create_dir_all(lib).unwrap();
        std::fs::write(lib.join("libavcodec.so"), "").unwrap();
        std::fs::create_dir_all(include.join("libavcodec")).unwrap();
        std::fs::write(include.join("libavcodec/avcodec.h"), "").unwrap();
    }

    #[test]
    fn a_prefix_whose_library_or_header_lies_elsewhere_is_refused() {
        let d = scratch("elsewhere");
        let bind = |prefix: &Path, lib: &Path, include: &Path| {
            let pc = pc_prefix(prefix, lib, include);
            bound(OsStr::new("pkg-config"), prefix, &pc, pc.as_os_str())
                .map_err(|e| format!("{e:#}"))
        };
        let ff = d.join("ff");
        install(&ff.join("lib"), &ff.join("include"));
        let other = d.join("other");
        install(&other.join("lib"), &other.join("include"));
        let b = bind(&ff, &ff.join("lib"), &ff.join("include")).unwrap();
        assert_eq!(b.libavcodec, "61.19.100");
        assert_eq!(b.libdir, ff.join("lib"));
        // Through a symlink to the prefix, everything is still inside it.
        let link = d.join("link");
        symlink(&ff, &link);
        assert!(bind(&link, &link.join("lib"), &link.join("include")).is_ok());
        // Another FFmpeg's library or header, named outright.
        let e = bind(&ff, &other.join("lib"), &ff.join("include")).unwrap_err();
        assert!(e.contains("libavcodec's library"), "{e}");
        assert!(e.contains("outside the prefix"), "{e}");
        let e = bind(&ff, &ff.join("lib"), &other.join("include")).unwrap_err();
        assert!(e.contains("libavcodec's header"), "{e}");
        assert!(e.contains("outside the prefix"), "{e}");
        // Or through a symlink in the prefix.
        let linked = d.join("linked");
        install(&linked.join("lib"), &linked.join("include"));
        std::fs::remove_file(linked.join("lib/libavcodec.so")).unwrap();
        symlink(
            &other.join("lib/libavcodec.so"),
            &linked.join("lib/libavcodec.so"),
        );
        let e = bind(&linked, &linked.join("lib"), &linked.join("include")).unwrap_err();
        assert!(e.contains("outside the prefix"), "{e}");
        // Or not there at all.
        let e = bind(&ff, &ff.join("lib"), &ff.join("nowhere")).unwrap_err();
        assert!(e.contains("libavcodec's header"), "{e}");
        assert!(e.contains("isn't there"), "{e}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn pkg_config_is_the_one_the_build_runs() {
        assert_eq!(pkg_config_program(&[]), "pkg-config");
        assert_eq!(
            pkg_config_program(&vars(&[("PKG_CONFIG", "/opt/bin/pkgconf")])),
            "/opt/bin/pkgconf"
        );
        // And `bound` runs it: `false` finds nothing.
        let d = scratch("program");
        let ff = d.join("ff");
        install(&ff.join("lib"), &ff.join("include"));
        let pc = pc_prefix(&ff, &ff.join("lib"), &ff.join("include"));
        assert!(bound(OsStr::new("pkg-config"), &ff, &pc, pc.as_os_str()).is_ok());
        let e = bound(OsStr::new("false"), &ff, &pc, pc.as_os_str())
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("false --variable=pcfiledir libavcodec failed"),
            "{e}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    fn found(v: &str, ff: Option<&str>) -> Result<Bound, String> {
        Ok(Bound {
            libavcodec: v.into(),
            ffmpeg: ff.map(Into::into),
            version_header: None,
            libdir: p("/lib"),
        })
    }

    #[test]
    fn the_report_names_each_build_its_version_and_every_failure() {
        let rows = [
            Row {
                label: "n7.1".into(),
                prefix: p("/ff/n7.1"),
                bound: found("61.19.100", Some("n7.1")),
                clippy: Stage::Passed,
                link: Some(Stage::Passed),
                tier1: None,
            },
            Row {
                label: "n8.1.3-full".into(),
                prefix: p("/ff/n8.1.3-full"),
                bound: found("62.28.103", Some("n8.1.3")),
                clippy: Stage::Passed,
                link: Some(Stage::Passed),
                tier1: Some(Stage::Failed(
                    "tests (Tier 1) failed (exit status: 101)".into(),
                )),
            },
            Row {
                label: "n9.0.2".into(),
                prefix: p("/ff/n9.0.2"),
                bound: found("63.3.100", None),
                clippy: Stage::Failed("clippy failed (exit status: 101)".into()),
                link: None,
                tier1: None,
            },
            Row {
                label: "n8.1".into(),
                prefix: p("/ff/n8.1"),
                bound: found("62.28.100", Some("n8.1")),
                clippy: Stage::Passed,
                link: Some(Stage::Failed(
                    "link: the tests and binaries built against FFmpeg n8.1 failed \
                     (exit status: 101)"
                        .into(),
                )),
                tier1: None,
            },
            Row {
                label: "gone".into(),
                prefix: p("/ff/gone"),
                bound: Err(
                    "not an FFmpeg prefix: no libavcodec.pc in /ff/gone/lib/pkgconfig".into(),
                ),
                clippy: Stage::NotRun("its FFmpeg was not found".into()),
                link: None,
                tier1: None,
            },
        ];
        let r = report(&rows);
        let want = "\
==> FFmpeg compatibility: 1 of 5 builds passed
  n7.1         FFmpeg n7.1, libavcodec 61.19.100    clippy passed, link passed
               /ff/n7.1
  n8.1.3-full  FFmpeg n8.1.3, libavcodec 62.28.103  clippy passed, link passed, Tier 1 FAILED
               /ff/n8.1.3-full
               tests (Tier 1) failed (exit status: 101)
  n9.0.2       libavcodec 63.3.100                  clippy FAILED
               /ff/n9.0.2
               clippy failed (exit status: 101)
  n8.1         FFmpeg n8.1, libavcodec 62.28.100    clippy passed, link FAILED
               /ff/n8.1
               link: the tests and binaries built against FFmpeg n8.1 failed (exit status: 101)
  gone         -                                    not an FFmpeg prefix: no libavcodec.pc in /ff/gone/lib/pkgconfig
               /ff/gone
               its FFmpeg was not found
";
        assert_eq!(r, want, "\n{r}");
        let failed: Vec<bool> = rows.iter().map(Row::failed).collect();
        assert_eq!(failed, [false, true, true, true, true]);
    }

    #[test]
    fn a_tier1_run_that_did_not_happen_is_a_failure() {
        let row = Row {
            label: "n8.1".into(),
            prefix: p("/ff/n8.1"),
            bound: found("62.28.100", None),
            clippy: Stage::Passed,
            link: Some(Stage::Failed("link failed".into())),
            tier1: Some(Stage::NotRun(
                "Tier 1 needs clippy and the link to pass first".into(),
            )),
        };
        assert!(row.failed());
        assert!(report(&[row]).contains("clippy passed, link FAILED, Tier 1 not run"));
        // Nor is a link that did not happen a pass.
        let row = Row {
            label: "n8.1".into(),
            prefix: p("/ff/n8.1"),
            bound: found("62.28.100", None),
            clippy: Stage::Passed,
            link: None,
            tier1: None,
        };
        assert!(row.failed());
    }
}
