//! `cargo xtask compat`: the workspace against other FFmpeg builds (D2).
//!
//! ffmpeg-sys-next binds whichever FFmpeg pkg-config finds, and its
//! bindings differ between releases (FFmpeg 8 made the `SWS_` defines an
//! enum, so code that built against 6.1 failed on Pat's 8.1). For each
//! FFmpeg install prefix this runs clippy on the workspace, all targets
//! and features with `-D warnings` as `cargo xtask check` does, with
//! pkg-config pointed at that build and in a target directory of its own:
//! sharing one would rebind ffmpeg-sys-next, and rebuild everything above
//! it, each time the FFmpeg changes. `--test` also runs the Tier 1 tests
//! against one of them, loading its libraries through `LD_LIBRARY_PATH`.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::{hw_test, run_step};

/// The FFmpeg prefixes `cargo xtask check` lints against, separated by
/// colons.
pub const PREFIXES_ENV: &str = "SPHERECUT_FFMPEG_COMPAT";
/// Where the per-FFmpeg target directories go, instead of `compat/` in
/// cargo's target directory.
pub const TARGET_ENV: &str = "SPHERECUT_FFMPEG_COMPAT_TARGET_DIR";

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
    check(&prefixes, test.as_deref())
}

/// `cargo xtask check`'s stage: clippy against each FFmpeg in
/// `SPHERECUT_FFMPEG_COMPAT`, or a line saying it was skipped.
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
            "==> skipping clippy against other FFmpeg builds: {PREFIXES_ENV} {why} \
             (FFmpeg prefixes separated by colons; see `cargo xtask compat --help`)"
        );
        return Ok(());
    }
    check(&prefixes, None)
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

/// Asks pkg-config, with the search path the build will have, which
/// libavcodec it finds, and fails unless it is the prefix's: ffmpeg-sys-next
/// would otherwise bind another FFmpeg (the system's, say) without a word.
fn bound(pc_dir: &Path, search: &OsStr) -> Result<Bound> {
    let found = PathBuf::from(pkg_config(search, &["--variable=pcfiledir", "libavcodec"])?);
    if !same_dir(&found, pc_dir) {
        bail!(
            "pkg-config finds libavcodec in {}, not in {}",
            found.display(),
            pc_dir.display()
        );
    }
    let libavcodec = pkg_config(search, &["--modversion", "libavcodec"])?
        .to_string_lossy()
        .into_owned();
    let libdir = PathBuf::from(pkg_config(search, &["--variable=libdir", "libavcodec"])?);
    let ffmpeg = pkg_config(search, &["--variable=includedir", "libavutil"])
        .ok()
        .and_then(|inc| std::fs::read_to_string(Path::new(&inc).join("libavutil/ffversion.h")).ok())
        .and_then(|h| ffmpeg_version(&h));
    Ok(Bound {
        libavcodec,
        ffmpeg,
        libdir,
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// One pkg-config query's output, trimmed, as bytes (a path need not be
/// UTF-8).
fn pkg_config(search: &OsStr, args: &[&str]) -> Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let out = Command::new("pkg-config")
        .args(args)
        .env("PKG_CONFIG_PATH", search)
        .output()
        .context("could not run pkg-config")?;
    if !out.status.success() {
        bail!(
            "pkg-config {} failed: {}",
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

/// The variables cargo runs with for one FFmpeg. `existing` reads this
/// process's environment (a parameter, for the tests).
fn cargo_env(
    pc_dir: &Path,
    target: &Path,
    libdir: Option<&Path>,
    existing: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, OsString)> {
    let mut env = vec![
        (
            "PKG_CONFIG_PATH",
            prepend(pc_dir, existing("PKG_CONFIG_PATH")),
        ),
        ("CARGO_TARGET_DIR", target.as_os_str().to_owned()),
    ];
    // Low debuginfo and no incremental state: one target directory per
    // FFmpeg adds up. Settings already made win.
    for (var, value) in [
        ("CARGO_PROFILE_DEV_DEBUG", "line-tables-only"),
        ("CARGO_INCREMENTAL", "0"),
    ] {
        if existing(var).is_none() {
            env.push((var, value.into()));
        }
    }
    if let Some(dir) = libdir {
        env.push(("LD_LIBRARY_PATH", prepend(dir, existing("LD_LIBRARY_PATH"))));
    }
    env
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
    tier1: Option<Stage>,
}

impl Row {
    fn failed(&self) -> bool {
        self.bound.is_err()
            || self.clippy != Stage::Passed
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
        if let Some(t) = &r.tier1 {
            let _ = write!(result, ", Tier 1 {}", t.word());
        }
        let _ = writeln!(out, "  {:lw$}  {w:ww$}  {result}", r.label);
        let _ = writeln!(out, "  {:lw$}  {}", "", r.prefix.display());
        for s in std::iter::once(&r.clippy).chain(&r.tier1) {
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

fn check(prefixes: &[PathBuf], test: Option<&Path>) -> Result<()> {
    let root = target_root();
    let existing = |var: &str| std::env::var_os(var);
    let mut rows = Vec::new();
    for b in builds(prefixes)? {
        let target = root.join(&b.label);
        let bound = b.pc_dir.clone().and_then(|pc| {
            bound(&pc, &prepend(&pc, existing("PKG_CONFIG_PATH"))).map_err(|e| format!("{e:#}"))
        });
        let clippy = match (&b.pc_dir, &bound) {
            (Ok(pc), Ok(found)) => Stage::of(run_step(
                &format!(
                    "clippy against {} at {} (target {})",
                    found.describe(),
                    b.prefix.display(),
                    target.display()
                ),
                Command::new("cargo")
                    .args([
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--all-features",
                        "--",
                        "-D",
                        "warnings",
                    ])
                    .envs(cargo_env(pc, &target, None, &existing)),
            )),
            _ => Stage::NotRun("its FFmpeg was not found".into()),
        };
        let tier1 = (test == Some(b.prefix.as_path())).then(|| match (&b.pc_dir, &bound) {
            (Ok(pc), Ok(found)) if clippy == Stage::Passed => {
                let env = cargo_env(pc, &target, Some(&found.libdir), &existing);
                let what = format!("against {} at {}", found.describe(), b.prefix.display());
                Stage::of(
                    run_step(
                        &format!("tests (Tier 1) {what}"),
                        Command::new("cargo")
                            .args(["test", "--workspace", "--all-targets"])
                            .envs(env.clone()),
                    )
                    .and_then(|()| {
                        run_step(
                            &format!("doc tests {what}"),
                            Command::new("cargo")
                                .args(["test", "--workspace", "--doc"])
                                .envs(env),
                        )
                    }),
                )
            }
            _ => Stage::NotRun("Tier 1 needs clippy to pass first".into()),
        });
        rows.push(Row {
            label: b.label,
            prefix: b.prefix,
            bound,
            clippy,
            tier1,
        });
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
        assert!(bound(&pc, pc.as_os_str()).is_err());
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

    #[test]
    fn cargo_gets_the_prefix_first_its_own_target_and_low_debuginfo() {
        let none = |_: &str| None;
        let env = cargo_env(
            Path::new("/ff/lib/pkgconfig"),
            Path::new("/t/compat/ff"),
            None,
            &none,
        );
        let get = |env: &[(&str, OsString)], k: &str| {
            env.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
        };
        assert_eq!(
            get(&env, "PKG_CONFIG_PATH"),
            Some("/ff/lib/pkgconfig".into())
        );
        assert_eq!(get(&env, "CARGO_TARGET_DIR"), Some("/t/compat/ff".into()));
        assert_eq!(
            get(&env, "CARGO_PROFILE_DEV_DEBUG"),
            Some("line-tables-only".into())
        );
        assert_eq!(get(&env, "CARGO_INCREMENTAL"), Some("0".into()));
        // The libraries are on the loader's path only for the tests.
        assert_eq!(get(&env, "LD_LIBRARY_PATH"), None);

        let set = |k: &str| match k {
            "PKG_CONFIG_PATH" => Some("/vk/lib/pkgconfig".into()),
            "LD_LIBRARY_PATH" => Some("/x/lib:".into()),
            "CARGO_PROFILE_DEV_DEBUG" => Some("0".into()),
            _ => None,
        };
        let env = cargo_env(
            Path::new("/ff/lib/pkgconfig"),
            Path::new("/t/compat/ff"),
            Some(Path::new("/ff/lib")),
            &set,
        );
        assert_eq!(
            get(&env, "PKG_CONFIG_PATH"),
            Some("/ff/lib/pkgconfig:/vk/lib/pkgconfig".into())
        );
        assert_eq!(get(&env, "LD_LIBRARY_PATH"), Some("/ff/lib:/x/lib".into()));
        // The caller's own setting stays.
        assert_eq!(get(&env, "CARGO_PROFILE_DEV_DEBUG"), None);
    }

    fn found(v: &str, ff: Option<&str>) -> Result<Bound, String> {
        Ok(Bound {
            libavcodec: v.into(),
            ffmpeg: ff.map(Into::into),
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
                tier1: None,
            },
            Row {
                label: "n8.1.3-full".into(),
                prefix: p("/ff/n8.1.3-full"),
                bound: found("62.28.103", Some("n8.1.3")),
                clippy: Stage::Passed,
                tier1: Some(Stage::Failed(
                    "tests (Tier 1) failed (exit status: 101)".into(),
                )),
            },
            Row {
                label: "n9.0.2".into(),
                prefix: p("/ff/n9.0.2"),
                bound: found("63.3.100", None),
                clippy: Stage::Failed("clippy failed (exit status: 101)".into()),
                tier1: None,
            },
            Row {
                label: "gone".into(),
                prefix: p("/ff/gone"),
                bound: Err(
                    "not an FFmpeg prefix: no libavcodec.pc in /ff/gone/lib/pkgconfig".into(),
                ),
                clippy: Stage::NotRun("its FFmpeg was not found".into()),
                tier1: None,
            },
        ];
        let r = report(&rows);
        let want = "\
==> FFmpeg compatibility: 1 of 4 builds passed
  n7.1         FFmpeg n7.1, libavcodec 61.19.100    clippy passed
               /ff/n7.1
  n8.1.3-full  FFmpeg n8.1.3, libavcodec 62.28.103  clippy passed, Tier 1 FAILED
               /ff/n8.1.3-full
               tests (Tier 1) failed (exit status: 101)
  n9.0.2       libavcodec 63.3.100                  clippy FAILED
               /ff/n9.0.2
               clippy failed (exit status: 101)
  gone         -                                    not an FFmpeg prefix: no libavcodec.pc in /ff/gone/lib/pkgconfig
               /ff/gone
               its FFmpeg was not found
";
        assert_eq!(r, want, "\n{r}");
        let failed: Vec<bool> = rows.iter().map(Row::failed).collect();
        assert_eq!(failed, [false, true, true, true]);
    }

    #[test]
    fn a_tier1_run_that_did_not_happen_is_a_failure() {
        let row = Row {
            label: "n8.1".into(),
            prefix: p("/ff/n8.1"),
            bound: found("62.28.100", None),
            clippy: Stage::Passed,
            tier1: Some(Stage::NotRun("Tier 1 needs clippy to pass first".into())),
        };
        assert!(row.failed());
        assert!(report(&[row]).contains("clippy passed, Tier 1 not run"));
    }
}
