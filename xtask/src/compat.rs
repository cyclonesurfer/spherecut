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
//!
//! compat deletes only what it made: a target directory is
//! `spherecut-compat-<label>`, and one is deleted only while it holds
//! compat's stamp. A directory there without one is refused, and so is a
//! target that is one of the prefixes listed, holds one or lies inside
//! one, or lies inside a directory holding `include/libavcodec/avcodec.h`
//! (an FFmpeg installed there with its headers straight in `include/`,
//! listed or not; Debian's `include/<triplet>/` and Fedora's
//! `include/ffmpeg/` aren't recognised unless their prefix is listed),
//! symlinks and `..` resolved: `SPHERECUT_FFMPEG_COMPAT_TARGET_DIR` puts
//! the target directories elsewhere. Cargo's build directory is the target directory
//! too, whatever `build.build-dir` says, so that all a build makes is
//! built against its FFmpeg alone and goes with it; cargo reads that path
//! as a template, so a target whose path holds a brace is refused.
//!
//! A build counts for a prefix only if it bound that prefix's FFmpeg:
//! pkg-config must find each library ffmpeg-sys-next binds there, its
//! library and header inside the prefix, before the build and again
//! after it, and ffmpeg-sys-next's build script, in clippy's build and in
//! the link's, as cargo's JSON messages report it (wherever cargo put it:
//! a build directory, a target triple), must have linked libavcodec and
//! searched for libraries only inside the prefix, and the FFmpeg
//! libraries the builds linked must be those pkg-config was asked about.
//!
//! The linker takes a library from the first directory on its search
//! path that has it, and a crate's build scripts put theirs on it in an
//! order compat doesn't choose: alsa-sys's pkg-config puts the system's
//! library directory there (`/usr/lib/x86_64-linux-gnu`), ahead of
//! ffmpeg-sys-next's in a crate that depends on both, which would link
//! the system's FFmpeg against the prefix's headers. So every build runs
//! with the prefix's library directories first, as `-L` flags of rustc's
//! own (`CARGO_ENCODED_RUSTFLAGS`, ahead of the caller's
//! `CARGO_ENCODED_RUSTFLAGS` or `RUSTFLAGS`; rustflags in cargo's config
//! files don't apply), and once linked, each test executable and binary
//! must load every FFmpeg library it needs from the prefix, as `ldd`
//! resolves them with the prefix's libraries on the loader's path.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};

use crate::{hw_test, run_step};

/// The FFmpeg prefixes `cargo xtask check` lints against, separated by
/// colons.
pub const PREFIXES_ENV: &str = "SPHERECUT_FFMPEG_COMPAT";
/// Where the per-FFmpeg target directories go, instead of `compat/` in
/// cargo's target directory.
pub const TARGET_ENV: &str = "SPHERECUT_FFMPEG_COMPAT_TARGET_DIR";
/// Cargo's JSON messages on its standard output, its diagnostics as text
/// on its standard error, for [`check_built`].
const JSON: &str = "--message-format=json-render-diagnostics";
/// What each target directory's name starts with, before its label.
const TARGET_PREFIX: &str = "spherecut-compat-";
/// The file in each target directory that says what it was built against.
const STAMP_FILE: &str = "spherecut-ffmpeg-stamp";
/// The stamp's first line: compat made the directory, so compat may
/// delete it.
const STAMP_HEADER: &str = "Made by `cargo xtask compat`, against:\n";
/// The libraries ffmpeg-sys-next binds with the workspace's features
/// (`avcodec`, `avformat`, `swscale` and `swresample`, and libavutil, which
/// it always binds), each found by pkg-config on its own, and the header
/// of each that its bindings read. [`check_built`] fails a build that
/// links any other FFmpeg library, or none of one of these, and a unit
/// test fails once the features cargo resolves for ffmpeg-sys-next say
/// otherwise, without a compat run.
const BOUND: [(&str, &str); 5] = [
    ("libavutil", "libavutil/avutil.h"),
    ("libavformat", "libavformat/avformat.h"),
    ("libswscale", "libswscale/swscale.h"),
    ("libswresample", "libswresample/swresample.h"),
    ("libavcodec", "libavcodec/avcodec.h"),
];
/// The libraries ffmpeg-sys-next can bind, whose pkg-config files a stamp
/// covers. It can also bind those of [`DROPPED`].
const STAMPED: [&str; 7] = [
    "libavcodec",
    "libavdevice",
    "libavfilter",
    "libavformat",
    "libavutil",
    "libswresample",
    "libswscale",
];
/// The libraries ffmpeg-sys-next can also bind that FFmpeg no longer
/// has: libpostproc, which FFmpeg 8 dropped, and libavresample, which 5
/// did. A build that links one links an older FFmpeg's.
const DROPPED: [&str; 2] = ["libpostproc", "libavresample"];

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
    /// default they are kept, and later runs build only what changed);
    /// only one with compat's stamp is deleted.
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
                "{} and {} would share the target directory {TARGET_PREFIX}{}: \
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
    /// The bound libraries' directories, for `LD_LIBRARY_PATH`: one, in
    /// any FFmpeg install.
    libdirs: Vec<PathBuf>,
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
/// have, where it finds each library ffmpeg-sys-next binds ([`BOUND`]),
/// and fails unless each is `prefix`'s: its `.pc` file in `pc_dir`, and
/// the library and header it names inside the prefix, symlinks resolved.
/// ffmpeg-sys-next would otherwise bind another FFmpeg (the system's,
/// say) without a word: a library whose `.pc` file is missing from the
/// prefix (an install that stopped partway) or names a `libdir` or
/// `includedir` elsewhere (a prefix moved, its files unchanged) comes from
/// there, and libavutil, which it finds first, puts its directory first
/// on the linker's search path, for libavcodec too.
fn bound(program: &OsStr, prefix: &Path, pc_dir: &Path, search: &OsStr) -> Result<Bound> {
    let query = |args: &[&str]| pkg_config(program, search, args);
    let home = prefix
        .canonicalize()
        .with_context(|| format!("could not resolve {}", prefix.display()))?;
    let mut libdirs: Vec<PathBuf> = Vec::new();
    for (lib, header) in BOUND {
        let found = PathBuf::from(query(&["--variable=pcfiledir", lib])?);
        if !same_dir(&found, pc_dir) {
            bail!(
                "pkg-config finds {lib} in {}, not in {}",
                found.display(),
                pc_dir.display()
            );
        }
        let libdir = PathBuf::from(query(&["--variable=libdir", lib])?);
        let includedir = PathBuf::from(query(&["--variable=includedir", lib])?);
        for (what, file) in [
            ("library", libdir.join(format!("{lib}.so"))),
            ("header", includedir.join(header)),
        ] {
            let real = file.canonicalize().with_context(|| {
                format!(
                    "{lib}'s {what} {} isn't there, where its pkg-config file says",
                    file.display()
                )
            })?;
            if !real.starts_with(&home) {
                bail!(
                    "{lib}'s {what} {} is {}, outside the prefix {}",
                    file.display(),
                    real.display(),
                    home.display()
                );
            }
        }
        if !libdirs.contains(&libdir) {
            libdirs.push(libdir);
        }
    }
    let libavcodec = query(&["--modversion", "libavcodec"])?
        .to_string_lossy()
        .into_owned();
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
        libdirs,
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
    let mut out = format!(
        "{STAMP_HEADER}pkg-config directory {}\n{}\n",
        dir.display(),
        b.describe()
    );
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

/// The target directory for the prefix labelled `label`, in `root`.
fn target_dir(root: &Path, label: &str) -> PathBuf {
    root.join(format!("{TARGET_PREFIX}{label}"))
}

/// Whether compat made `target`: a directory, not a symlink, holding a
/// stamp of compat's.
fn made_by_compat(target: &Path) -> bool {
    let file = target.join(STAMP_FILE);
    std::fs::symlink_metadata(target).is_ok_and(|m| m.is_dir())
        && std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file())
        && std::fs::read_to_string(&file).is_ok_and(|s| s.starts_with(STAMP_HEADER))
}

/// `p` with symlinks resolved, for a path that need not exist: its nearest
/// existing ancestor resolved, and the rest after it, where `..` takes
/// the component before it off, as it will once `create_dir_all` has made
/// those directories. As it is when nothing of it exists.
fn resolved(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut rest = Vec::new();
    let mut at = p;
    loop {
        if let Ok(real) = at.canonicalize() {
            let up = rest.contains(&Component::ParentDir);
            let path = rest.iter().rev().fold(real, |mut path, c| {
                match c {
                    Component::ParentDir => {
                        path.pop();
                    }
                    Component::Normal(name) => path.push(name),
                    _ => {}
                }
                path
            });
            // Where `..` led back to may exist, through symlinks of its own.
            return if up { resolved(&path) } else { path };
        }
        match (at.parent(), at.components().next_back()) {
            (Some(parent), Some(last)) => {
                rest.push(last);
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Refuses a target directory that is one of the `prefixes`, holds one or
/// lies inside one, or that lies inside any directory holding
/// `include/libavcodec/avcodec.h`, as an FFmpeg installed there with its
/// headers straight in `include/` does, listed or not, symlinks and `..`
/// resolved: building there, or deleting it, would change an FFmpeg.
/// Nothing else of an FFmpeg is looked for: not its source tree, which has
/// no `include/`, nor an install whose headers are in
/// `include/<triplet>/` (Debian's and Ubuntu's) or `include/ffmpeg/`
/// (Fedora's), unless its prefix is listed.
fn guard(target: &Path, prefixes: &[PathBuf]) -> Result<()> {
    let t = resolved(target);
    for p in prefixes {
        let real = resolved(p);
        let how = if t == real {
            "is"
        } else if real.starts_with(&t) {
            "holds"
        } else if t.starts_with(&real) {
            "lies inside"
        } else {
            continue;
        };
        bail!(
            "the target directory {} {how} the FFmpeg prefix {}: set {TARGET_ENV} \
             to a directory outside every prefix",
            target.display(),
            p.display()
        );
    }
    if let Some(ff) = t
        .ancestors()
        .find(|a| a.join("include/libavcodec/avcodec.h").is_file())
    {
        bail!(
            "the target directory {} lies inside {}, which holds FFmpeg's headers \
             (include/libavcodec/avcodec.h), so compat won't build or delete anything \
             there: set {TARGET_ENV} to a directory outside it",
            target.display(),
            ff.display()
        );
    }
    Ok(())
}

/// Makes `target` ready for a build against what `stamp` describes. One
/// built against anything else is deleted first: cargo would keep its
/// bindings, since ffmpeg-sys-next binds again only when its own files or
/// the variables it reads change, not when the FFmpeg behind a prefix does
/// (a distro upgrade of `/usr`, a prefix built again in place, a symlink
/// pointed elsewhere). A directory there that compat didn't make, stamp
/// and all, is refused rather than deleted. `true` when one was deleted.
fn prepare_target(target: &Path, stamp: &str) -> Result<bool> {
    let exists = std::fs::symlink_metadata(target).is_ok();
    if exists && !made_by_compat(target) {
        bail!(
            "{} is there and `cargo xtask compat` didn't make it (it has no stamp \
             of compat's, {STAMP_FILE}), so compat won't build in it or delete it: \
             move it, or set {TARGET_ENV} to another directory",
            target.display()
        );
    }
    let file = target.join(STAMP_FILE);
    let stale = exists && std::fs::read_to_string(&file).ok().as_deref() != Some(stamp);
    if stale {
        std::fs::remove_dir_all(target)
            .with_context(|| format!("could not remove {}", target.display()))?;
    }
    std::fs::create_dir_all(target)
        .with_context(|| format!("could not create {}", target.display()))?;
    std::fs::write(&file, stamp).with_context(|| format!("could not write {}", file.display()))?;
    Ok(stale)
}

/// [`prepare_target`] once [`guard`] allows `target`, then the stamp again
/// from `restamp`, which must not have changed: deleting a stale target
/// must leave the FFmpeg it is for as it was, which a prefix whose files
/// are links into the target would not be. A target whose path holds a
/// brace is refused first: it is cargo's build directory too
/// (`CARGO_BUILD_BUILD_DIR`), which cargo reads as a template
/// (`{workspace-root}`), failing every build on a name in braces it
/// doesn't know.
fn ready_target(
    target: &Path,
    prefixes: &[PathBuf],
    stamp: &str,
    restamp: impl FnOnce() -> Result<String>,
) -> Result<bool> {
    if target.to_string_lossy().contains(['{', '}']) {
        bail!(
            "the target directory {} has a brace in its path, which cargo would read as a \
             template in its build directory, compat's target directory too: set \
             {TARGET_ENV} to a directory whose path has none",
            target.display()
        );
    }
    guard(target, prefixes)?;
    let stale = prepare_target(target, stamp)?;
    let now = restamp()?;
    if now != stamp {
        bail!(
            "the FFmpeg changed while its target {} was made ready; it was\n{stamp}and is now\n{now}",
            target.display()
        );
    }
    Ok(stale)
}

/// `--clean`'s deletion of `target`, only once compat made it and
/// [`guard`] allows it. `true` when it was there and is gone.
fn clean_target(target: &Path, prefixes: &[PathBuf]) -> Result<bool> {
    if std::fs::symlink_metadata(target).is_err() {
        return Ok(false);
    }
    guard(target, prefixes)?;
    if !made_by_compat(target) {
        bail!(
            "{} has no stamp of compat's ({STAMP_FILE}): it is left as it is",
            target.display()
        );
    }
    std::fs::remove_dir_all(target)
        .with_context(|| format!("could not remove {}", target.display()))?;
    Ok(true)
}

/// After the build against `prefix`, whose stamp was `before`: fails
/// unless `restamp`, pkg-config asked again, gives the same, and
/// ffmpeg-sys-next's build script, in each build cargo's JSON `messages`
/// report, linked libavcodec and searched for libraries only in
/// directories inside the prefix, symlinks resolved. Checked before the
/// build alone, a prefix that went away or changed while it ran had
/// pkg-config find another FFmpeg for the build (the system's), which
/// then passed under the prefix's name. The FFmpeg libraries the builds
/// link must also be those of [`BOUND`], which `bound` asks pkg-config
/// about: a feature of ffmpeg-sys-next turned on anywhere in the
/// workspace (ffmpeg-next's default features turn on `avdevice` and
/// `avfilter`) binds one more, whose `.pc` file, library and header no
/// one checked.
fn check_built(
    prefix: &Path,
    before: &str,
    restamp: impl FnOnce() -> Result<String>,
    messages: &str,
) -> Result<()> {
    let now =
        restamp().context("after the build, pkg-config doesn't find the FFmpeg it was for")?;
    if now != before {
        bail!(
            "the FFmpeg at {} changed during the build; it was\n{before}and is now\n{now}",
            prefix.display()
        );
    }
    let home = prefix
        .canonicalize()
        .with_context(|| format!("could not resolve {}", prefix.display()))?;
    let runs = build_script_runs(messages, "ffmpeg-sys-next");
    if runs.is_empty() {
        bail!("cargo reported no run of ffmpeg-sys-next's build script, to check what it bound");
    }
    let mut linked: Vec<String> = Vec::new();
    for run in &runs {
        let names: Vec<String> = run
            .libs
            .iter()
            .map(|l| format!("lib{}", link_name(l)))
            .collect();
        if !names.iter().any(|n| n == "libavcodec") {
            bail!(
                "ffmpeg-sys-next's build script linked no libavcodec ({}), to check what it bound",
                run.libs.join(", ")
            );
        }
        for name in names {
            let ffmpeg = STAMPED.contains(&name.as_str()) || DROPPED.contains(&name.as_str());
            if ffmpeg && !BOUND.iter().any(|(b, _)| *b == name) {
                bail!(
                    "ffmpeg-sys-next's build script linked {name}, which compat doesn't ask \
                     pkg-config about: add it to BOUND"
                );
            }
            linked.push(name);
        }
        for search in &run.paths {
            // KIND=PATH, the kind optional.
            let dir = match search.split_once('=') {
                Some((kind, dir))
                    if ["native", "dependency", "crate", "framework", "all"].contains(&kind) =>
                {
                    dir
                }
                _ => search.as_str(),
            };
            let real = Path::new(dir).canonicalize().with_context(|| {
                format!("ffmpeg-sys-next's build searched {dir} for libraries, which isn't there")
            })?;
            if !real.starts_with(&home) {
                bail!(
                    "ffmpeg-sys-next's build searched {} for libraries, outside the prefix {}",
                    real.display(),
                    home.display()
                );
            }
        }
    }
    if let Some((lib, _)) = BOUND.iter().find(|(b, _)| !linked.iter().any(|l| l == b)) {
        bail!("no build linked {lib}, which compat asks pkg-config about: take it out of BOUND");
    }
    Ok(())
}

/// The library a build script's `rustc-link-lib` value names:
/// `KIND[:MODIFIERS]=NAME[:RENAME]`, the kind optional.
fn link_name(value: &str) -> &str {
    let name = value.rsplit('=').next().unwrap_or(value);
    name.split(':').next().unwrap_or(name)
}

/// What a build script had cargo link with, as one of cargo's
/// `build-script-executed` messages reports it: each `rustc-link-lib`
/// value and each `rustc-link-search` value it gave.
#[derive(Debug, Clone, PartialEq)]
struct BuildScriptRun {
    libs: Vec<String>,
    paths: Vec<String>,
}

/// The runs of package `name`'s build script that cargo's JSON messages
/// report, one message a line (`--message-format=json`), fresh or not.
/// Lines that aren't such a message are passed over.
fn build_script_runs(messages: &str, name: &str) -> Vec<BuildScriptRun> {
    messages
        .lines()
        .filter(|l| l.starts_with('{') && l.contains("\"build-script-executed\""))
        .filter_map(Json::parse)
        .filter(|m| m.get("reason").and_then(Json::str) == Some("build-script-executed"))
        .filter(|m| {
            m.get("package_id")
                .and_then(Json::str)
                .is_some_and(|id| package_name(id) == name)
        })
        .filter_map(|m| {
            Some(BuildScriptRun {
                libs: m.get("linked_libs")?.strings()?,
                paths: m.get("linked_paths")?.strings()?,
            })
        })
        .collect()
}

/// The package a cargo package ID names: `ffmpeg-sys-next` in
/// `registry+https://...#ffmpeg-sys-next@8.0.1`, in a path package's
/// `path+file:///src/ffmpeg-sys-next#8.0.1` (its directory's name, when
/// that is its name), and in the older `ffmpeg-sys-next 8.0.1 (...)`.
fn package_name(id: &str) -> &str {
    match id.rsplit_once('#') {
        Some((_, frag)) if frag.contains('@') => frag.split('@').next().unwrap_or(frag),
        Some((url, _)) => {
            let path = url.split(['?', '#']).next().unwrap_or(url);
            path.trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(path)
        }
        None => id.split(' ').next().unwrap_or(id),
    }
}

/// A JSON value, as much of one as cargo's messages need: a number is
/// kept as its text.
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// `text` as one JSON value, or `None` when it isn't one.
    fn parse(text: &str) -> Option<Json> {
        let mut p = JsonParser {
            text: text.as_bytes(),
            at: 0,
        };
        let v = p.value()?;
        p.space();
        (p.at == p.text.len()).then_some(v)
    }

    /// An object's member.
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    /// An array of strings.
    fn strings(&self) -> Option<Vec<String>> {
        match self {
            Json::Array(items) => items.iter().map(|i| i.str().map(str::to_owned)).collect(),
            _ => None,
        }
    }
}

struct JsonParser<'a> {
    text: &'a [u8],
    at: usize,
}

impl JsonParser<'_> {
    fn space(&mut self) {
        while self.text.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    /// Past `c`, after any white space, when it comes next.
    fn eat(&mut self, c: u8) -> bool {
        self.space();
        let here = self.text.get(self.at) == Some(&c);
        if here {
            self.at += 1;
        }
        here
    }

    fn value(&mut self) -> Option<Json> {
        self.space();
        match *self.text.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut members = Vec::new();
                if self.eat(b'}') {
                    return Some(Json::Object(members));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return None;
                    }
                    members.push((key, self.value()?));
                    if !self.eat(b',') {
                        return self.eat(b'}').then_some(Json::Object(members));
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Some(Json::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    if !self.eat(b',') {
                        return self.eat(b']').then_some(Json::Array(items));
                    }
                }
            }
            b'"' => self.string().map(Json::String),
            _ => {
                let start = self.at;
                while self
                    .text
                    .get(self.at)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || b"+-.".contains(c))
                {
                    self.at += 1;
                }
                match &self.text[start..self.at] {
                    b"null" => Some(Json::Null),
                    b"true" => Some(Json::Bool(true)),
                    b"false" => Some(Json::Bool(false)),
                    n if n.first().is_some_and(|c| *c == b'-' || c.is_ascii_digit()) => {
                        Some(Json::Number(String::from_utf8_lossy(n).into_owned()))
                    }
                    _ => None,
                }
            }
        }
    }

    /// A string, its escapes undone.
    fn string(&mut self) -> Option<String> {
        if self.text.get(self.at) != Some(&b'"') {
            return None;
        }
        self.at += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.text.get(self.at)?;
            self.at += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.text.get(self.at)?;
                    self.at += 1;
                    let ch = match e {
                        b'"' | b'\\' | b'/' => char::from(e),
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xd800..0xdc00).contains(&hi) {
                                // A surrogate pair.
                                if self.text.get(self.at..self.at + 2) != Some(b"\\u") {
                                    return None;
                                }
                                self.at += 2;
                                let lo = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&lo) {
                                    return None;
                                }
                                char::from_u32(0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00))?
                            } else {
                                char::from_u32(hi)?
                            }
                        }
                        _ => return None,
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = std::str::from_utf8(self.text.get(self.at..self.at + 4)?).ok()?;
        let v = u32::from_str_radix(digits, 16).ok()?;
        self.at += 4;
        Some(v)
    }
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
/// `vars`: each with its value, or `None` for one it runs without. The
/// bound libraries' directories, `libdirs`, go first on rustc's search
/// path ([`rustflags`]), and on the loader's too when `load` (for the
/// tests, run).
fn cargo_env(
    pc_dir: &Path,
    target: &Path,
    libdirs: &[PathBuf],
    load: bool,
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
        // Cargo's build directory (`build.build-dir`), where it keeps
        // what it builds on the way: else every FFmpeg's build would
        // share one, rebinding ffmpeg-sys-next each time, and `--clean`
        // would leave it. Cargo expands `{...}` in it, so `ready_target`
        // refuses a target whose path holds a brace.
        (
            "CARGO_BUILD_BUILD_DIR".to_owned(),
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
    if let Some(flags) = rustflags(libdirs, vars) {
        env.push(("CARGO_ENCODED_RUSTFLAGS".to_owned(), Some(flags)));
    }
    if load && !libdirs.is_empty() {
        env.push(("LD_LIBRARY_PATH".to_owned(), loader_path(libdirs, vars)));
    }
    env.extend(
        overriding(vars.iter().map(|(k, _)| k.as_os_str()))
            .into_iter()
            .map(|n| (n, None)),
    );
    env
}

/// `libdirs` ahead of this process's `LD_LIBRARY_PATH` in `vars`.
fn loader_path(libdirs: &[PathBuf], vars: &[(OsString, OsString)]) -> Option<OsString> {
    libdirs
        .iter()
        .rev()
        .fold(get(vars, "LD_LIBRARY_PATH"), |rest, dir| {
            Some(prepend(dir, rest))
        })
}

/// `CARGO_ENCODED_RUSTFLAGS` for one FFmpeg's builds: `-L native=DIR` for
/// each of `libdirs`, ahead of the flags `vars` give rustc already
/// (`CARGO_ENCODED_RUSTFLAGS`, else `RUSTFLAGS`, split on spaces as cargo
/// splits it). Rustc searches its `-L` directories in order, and those a
/// build script gives come after these (see the module docs). `None`
/// with no directories.
fn rustflags(libdirs: &[PathBuf], vars: &[(OsString, OsString)]) -> Option<OsString> {
    if libdirs.is_empty() {
        return None;
    }
    let mut flags: Vec<OsString> = Vec::new();
    for dir in libdirs {
        flags.push("-L".into());
        let mut d = OsString::from("native=");
        d.push(dir);
        flags.push(d);
    }
    match (get(vars, "CARGO_ENCODED_RUSTFLAGS"), get(vars, "RUSTFLAGS")) {
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
    let mut out = OsString::new();
    for (i, f) in flags.iter().enumerate() {
        if i > 0 {
            out.push("\x1f");
        }
        out.push(f);
    }
    Some(out)
}

/// The test executables and binaries cargo's JSON messages say it
/// linked, fresh or not, each once.
fn executables(messages: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = messages
        .lines()
        .filter(|l| l.starts_with('{') && l.contains("\"compiler-artifact\""))
        .filter_map(Json::parse)
        .filter(|m| m.get("reason").and_then(Json::str) == Some("compiler-artifact"))
        .filter_map(|m| m.get("executable").and_then(Json::str).map(PathBuf::from))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The FFmpeg libraries in `ldd`'s output, each with where it resolves
/// (`None` for "not found").
fn ffmpeg_loads(ldd: &str) -> Vec<(String, Option<PathBuf>)> {
    ldd.lines()
        .filter_map(|l| {
            let (name, to) = l.trim().split_once(" => ")?;
            let lib = name.split(".so").next()?;
            let ffmpeg = STAMPED.contains(&lib) || DROPPED.contains(&lib);
            ffmpeg.then(|| {
                let path = to.split(" (").next().unwrap_or(to).trim();
                (
                    name.to_owned(),
                    (path != "not found" && !path.is_empty()).then(|| PathBuf::from(path)),
                )
            })
        })
        .collect()
}

/// Fails unless each of `exes` loads every FFmpeg library it needs from
/// inside `prefix`, as `ldd` resolves them with `libdirs` first on the
/// loader's path, as the Tier 1 tests run.
fn check_loads(
    exes: &[PathBuf],
    prefix: &Path,
    libdirs: &[PathBuf],
    vars: &[(OsString, OsString)],
) -> Result<()> {
    if exes.is_empty() {
        bail!("cargo reported no test executable or binary, to check what it loads");
    }
    let home = prefix
        .canonicalize()
        .with_context(|| format!("could not resolve {}", prefix.display()))?;
    let mut cmd_env = Vec::new();
    if let Some(path) = loader_path(libdirs, vars) {
        cmd_env.push(("LD_LIBRARY_PATH", path));
    }
    let mut wrong = Vec::new();
    let mut checked = 0;
    for exe in exes {
        let out = Command::new("ldd")
            .arg(exe)
            .envs(cmd_env.iter().map(|(k, v)| (*k, v)))
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("could not run ldd on {}", exe.display()))?;
        if !out.status.success() {
            bail!("ldd failed on {} ({})", exe.display(), out.status);
        }
        for (lib, at) in ffmpeg_loads(&String::from_utf8_lossy(&out.stdout)) {
            checked += 1;
            let inside = at
                .as_deref()
                .and_then(|p| p.canonicalize().ok())
                .is_some_and(|p| p.starts_with(&home));
            if !inside {
                let name = exe.file_name().unwrap_or(exe.as_os_str()).to_string_lossy();
                wrong.push(match at {
                    Some(p) => format!("{name} loads {lib} from {}", p.display()),
                    None => format!("{name} needs {lib}, which isn't found"),
                });
            }
        }
    }
    if checked == 0 {
        bail!(
            "ldd found no FFmpeg library in any of the {} executables",
            exes.len()
        );
    }
    if !wrong.is_empty() {
        wrong.sort();
        wrong.dedup();
        bail!(
            "linked against FFmpeg libraries outside the prefix {}: {}",
            home.display(),
            wrong.join("; ")
        );
    }
    Ok(())
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

/// [`run_step`], but returning what the command printed to its standard
/// output: cargo's JSON messages, with `--message-format=json-render-diagnostics`,
/// which still prints the diagnostics as text to its standard error.
fn run_json(name: &str, cmd: &mut Command) -> Result<String> {
    eprintln!("==> {name}");
    let out = cmd
        .current_dir(crate::workspace_root())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(std::process::Child::wait_with_output)
        .with_context(|| format!("could not run {name}"))?;
    if !out.status.success() {
        bail!("{name} failed ({})", out.status);
    }
    String::from_utf8(out.stdout).with_context(|| format!("{name} printed something not UTF-8"))
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
    /// What the build bound, checked once it is done ([`check_built`]).
    binding: Option<Stage>,
}

impl Row {
    fn failed(&self) -> bool {
        self.bound.is_err()
            || self.clippy != Stage::Passed
            || self.link != Some(Stage::Passed)
            || self.tier1.as_ref().is_some_and(|t| *t != Stage::Passed)
            || self.binding != Some(Stage::Passed)
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
        if let Some(b) = &r.binding {
            let _ = write!(result, ", binding {}", b.word());
        }
        let _ = writeln!(out, "  {:lw$}  {w:ww$}  {result}", r.label);
        let _ = writeln!(out, "  {:lw$}  {}", "", r.prefix.display());
        let stages = std::iter::once(&r.clippy)
            .chain(&r.link)
            .chain(&r.tier1)
            .chain(&r.binding);
        for s in stages {
            if let Stage::Failed(why) | Stage::NotRun(why) = s {
                let _ = writeln!(out, "  {:lw$}  {why}", "");
            }
        }
    }
    out
}

/// Where the target directories go: `SPHERECUT_FFMPEG_COMPAT_TARGET_DIR`,
/// else `compat/` in cargo's target directory.
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
        let target = target_dir(&root, &b.label);
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
            binding: None,
        };
        if let (Ok(pc), Ok(found)) = (&b.pc_dir, &row.bound) {
            let what = format!("against {} at {}", found.describe(), row.prefix.display());
            // Cargo's messages from clippy and the link, which say what
            // ffmpeg-sys-next's build script bound.
            let mut messages = String::new();
            let env = cargo_env(pc, &target, &found.libdirs, false, &vars);
            let before = stamp(pc, found);
            let restamp =
                || bound(&program, &row.prefix, pc, &search(pc)).map(|again| stamp(pc, &again));
            row.clippy = match ready_target(&target, prefixes, &before, restamp) {
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
                    Stage::of(
                        run_json(
                            &format!("clippy {what} (target {})", target.display()),
                            &mut cargo(
                                &[
                                    "clippy",
                                    "--workspace",
                                    "--all-targets",
                                    "--all-features",
                                    JSON,
                                    "--",
                                    "-D",
                                    "warnings",
                                ],
                                &env,
                            ),
                        )
                        .map(|m| messages.push_str(&m)),
                    )
                }
            };
            if row.clippy == Stage::Passed {
                let link = Stage::of(
                    run_json(
                        &format!("link: the tests and binaries built {what}"),
                        &mut cargo(
                            &["test", "--workspace", "--all-targets", "--no-run", JSON],
                            &env,
                        ),
                    )
                    .and_then(|m| {
                        messages.push_str(&m);
                        check_loads(&executables(&m), &row.prefix, &found.libdirs, &vars)
                    }),
                );
                if link == Stage::Passed && row.tier1.is_some() {
                    let env = cargo_env(pc, &target, &found.libdirs, true, &vars);
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
                row.binding = Some(Stage::of(check_built(
                    &row.prefix,
                    &before,
                    restamp,
                    &messages,
                )));
            }
        }
        if clean {
            match clean_target(&target, prefixes) {
                Ok(true) => eprintln!("==> removed {}", target.display()),
                Ok(false) => {}
                Err(e) => eprintln!("warning: {e:#}"),
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
        assert!(e.contains("spherecut-compat-n8.1"), "{e}");
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
            &[],
            false,
            &[],
        );
        let set = |env: &[(String, Option<OsString>)], k: &str| value(env, k).flatten();
        assert_eq!(
            set(&env, "PKG_CONFIG_PATH"),
            Some("/ff/lib/pkgconfig".into())
        );
        assert_eq!(set(&env, "CARGO_TARGET_DIR"), Some("/t/compat/ff".into()));
        assert_eq!(
            set(&env, "CARGO_BUILD_BUILD_DIR"),
            Some("/t/compat/ff".into())
        );
        assert_eq!(
            set(&env, "CARGO_PROFILE_DEV_DEBUG"),
            Some("line-tables-only".into())
        );
        assert_eq!(set(&env, "CARGO_INCREMENTAL"), Some("0".into()));
        assert_eq!(value(&env, "LD_LIBRARY_PATH"), None);
        assert_eq!(value(&env, "CARGO_ENCODED_RUSTFLAGS"), None);
        assert!(env.iter().all(|(_, v)| v.is_some()), "{env:?}");

        let existing = vars(&[
            ("PKG_CONFIG_PATH", "/vk/lib/pkgconfig"),
            ("LD_LIBRARY_PATH", "/x/lib:"),
            ("CARGO_PROFILE_DEV_DEBUG", "0"),
            ("CARGO_BUILD_BUILD_DIR", "/build"),
        ]);
        // The libraries go first on rustc's search path for every build,
        // and on the loader's only for the tests.
        let env = cargo_env(
            Path::new("/ff/lib/pkgconfig"),
            Path::new("/t/compat/ff"),
            &[p("/ff/lib")],
            false,
            &existing,
        );
        assert_eq!(
            set(&env, "CARGO_ENCODED_RUSTFLAGS"),
            Some("-L\x1fnative=/ff/lib".into())
        );
        assert_eq!(value(&env, "LD_LIBRARY_PATH"), None);
        let env = cargo_env(
            Path::new("/ff/lib/pkgconfig"),
            Path::new("/t/compat/ff"),
            &[p("/ff/lib")],
            true,
            &existing,
        );
        assert_eq!(
            set(&env, "CARGO_ENCODED_RUSTFLAGS"),
            Some("-L\x1fnative=/ff/lib".into())
        );
        assert_eq!(
            set(&env, "PKG_CONFIG_PATH"),
            Some("/ff/lib/pkgconfig:/vk/lib/pkgconfig".into())
        );
        assert_eq!(set(&env, "LD_LIBRARY_PATH"), Some("/ff/lib:/x/lib".into()));
        // The caller's own setting stays, but not a build directory
        // shared by every FFmpeg.
        assert_eq!(value(&env, "CARGO_PROFILE_DEV_DEBUG"), None);
        assert_eq!(
            set(&env, "CARGO_BUILD_BUILD_DIR"),
            Some("/t/compat/ff".into())
        );
        // Every bound library's directory, in order, should they differ.
        let env = cargo_env(
            Path::new("/ff/lib/pkgconfig"),
            Path::new("/t/compat/ff"),
            &[p("/ff/lib"), p("/ff/lib64")],
            true,
            &existing,
        );
        assert_eq!(
            set(&env, "LD_LIBRARY_PATH"),
            Some("/ff/lib:/ff/lib64:/x/lib".into())
        );
        assert_eq!(
            set(&env, "CARGO_ENCODED_RUSTFLAGS"),
            Some("-L\x1fnative=/ff/lib\x1f-L\x1fnative=/ff/lib64".into())
        );
    }

    #[test]
    fn the_prefixs_libraries_go_ahead_of_the_callers_rustflags() {
        let dirs = [p("/ff/lib")];
        assert_eq!(rustflags(&[], &vars(&[("RUSTFLAGS", "-Cx")])), None);
        assert_eq!(
            rustflags(&dirs, &vars(&[("RUSTFLAGS", " -C  opt-level=1 --cfg x ")])),
            Some("-L\x1fnative=/ff/lib\x1f-C\x1fopt-level=1\x1f--cfg\x1fx".into())
        );
        // Cargo takes the encoded form over the plain one, as given.
        assert_eq!(
            rustflags(
                &dirs,
                &vars(&[
                    ("RUSTFLAGS", "-Cignored"),
                    ("CARGO_ENCODED_RUSTFLAGS", "--cfg\x1fa b")
                ])
            ),
            Some("-L\x1fnative=/ff/lib\x1f--cfg\x1fa b".into())
        );
        assert_eq!(
            rustflags(&dirs, &vars(&[("CARGO_ENCODED_RUSTFLAGS", "")])),
            Some("-L\x1fnative=/ff/lib".into())
        );
    }

    #[test]
    fn executables_are_read_from_cargos_messages() {
        let messages = "\
{\"reason\":\"compiler-artifact\",\"executable\":\"/t/debug/deps/engine-1\",\"fresh\":true}
{\"reason\":\"compiler-artifact\",\"executable\":null}
{\"reason\":\"build-script-executed\",\"executable\":\"/not/this\"}
warning: something
{\"reason\":\"compiler-artifact\",\"executable\":\"/t/debug/spherecut\"}
{\"reason\":\"compiler-artifact\",\"executable\":\"/t/debug/deps/engine-1\"}
";
        assert_eq!(
            executables(messages),
            [p("/t/debug/deps/engine-1"), p("/t/debug/spherecut")]
        );
    }

    #[test]
    fn the_load_check_fails_an_executable_loading_ffmpeg_from_elsewhere() {
        // This test binary links the FFmpeg it was built against (xtask
        // has synth, for hw-test's perf clips), which is inside `/` and
        // not inside an empty directory.
        let me = std::env::current_exe().unwrap();
        check_loads(std::slice::from_ref(&me), Path::new("/"), &[], &[]).unwrap();
        let elsewhere =
            std::env::temp_dir().join(format!("spherecut-compat-loads-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        let e = check_loads(&[me], &elsewhere, &[], &[]).unwrap_err();
        let _ = std::fs::remove_dir(&elsewhere);
        let e = format!("{e:#}");
        assert!(
            e.contains("outside the prefix") && e.contains("loads libavcodec"),
            "{e}"
        );
        let e = check_loads(&[], Path::new("/"), &[], &[]).unwrap_err();
        assert!(format!("{e:#}").contains("no test executable"), "{e:#}");
    }

    #[test]
    fn ldd_output_gives_each_ffmpeg_library_and_where_it_loads() {
        let ldd = "\
\tlinux-vdso.so.1 (0x00007ffd5a7f2000)
\tlibasound.so.2 => /lib/x86_64-linux-gnu/libasound.so.2 (0x00007f0a1c000000)
\tlibavcodec.so.62 => /ff/n8.1/lib/libavcodec.so.62 (0x00007f0a1a000000)
\tlibavutil.so.60 => /lib/x86_64-linux-gnu/libavutil.so.60 (0x00007f0a19000000)
\tlibswresample.so.6 => not found
\tlibavcodec-extra.so.1 => /x/libavcodec-extra.so.1 (0x1)
\t/lib64/ld-linux-x86-64.so.2 (0x00007f0a1d000000)
";
        assert_eq!(
            ffmpeg_loads(ldd),
            [
                (
                    "libavcodec.so.62".to_owned(),
                    Some(p("/ff/n8.1/lib/libavcodec.so.62"))
                ),
                (
                    "libavutil.so.60".to_owned(),
                    Some(p("/lib/x86_64-linux-gnu/libavutil.so.60"))
                ),
                ("libswresample.so.6".to_owned(), None),
            ]
        );
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
            &[],
            false,
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
            libdirs: vec![dir.join("lib")],
        };
        (pc, b)
    }

    #[test]
    fn a_target_built_against_another_ffmpeg_is_deleted_first() {
        let d = scratch("stamp");
        let target = target_dir(&d.join("compat"), "ff");
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

        // A target with no stamp can't be trusted either, and compat can't
        // tell that it made it: it is refused, and left as it is.
        build(&target);
        std::fs::remove_file(target.join(STAMP_FILE)).unwrap();
        let e = prepare_target(&target, &again).unwrap_err().to_string();
        assert!(e.contains("didn't make it"), "{e}");
        assert!(built(&target).is_file());

        // A file the stamp covers that is gone says so.
        std::fs::remove_file(pc71.join("libavdevice.pc")).unwrap();
        let s = stamp(&pc71, &b71);
        assert!(s.contains("libavdevice.pc missing"), "{s}");
        std::fs::remove_dir_all(&d).ok();
    }

    fn symlink(to: &Path, at: &Path) {
        std::os::unix::fs::symlink(to, at).unwrap();
    }

    /// A directory with a stamp of compat's, as `prepare_target` leaves
    /// it.
    fn made(target: &Path) {
        std::fs::create_dir_all(target).unwrap();
        std::fs::write(target.join(STAMP_FILE), format!("{STAMP_HEADER}x\n")).unwrap();
    }

    #[test]
    fn a_directory_compat_did_not_make_is_refused_and_kept() {
        let d = scratch("not-made");
        let (pc, b) = fake_prefix(&d.join("n8.1"), "62.28.100");
        let s = stamp(&pc, &b);
        assert!(s.starts_with(STAMP_HEADER), "{s}");
        let root = d.join("t");
        let target = target_dir(&root, "n8.1");
        assert_eq!(target, root.join("spherecut-compat-n8.1"));
        // Someone's directory where the target would go, without a stamp,
        // or with one compat didn't write (compat's stamps before this
        // one had no header either).
        std::fs::create_dir_all(&target).unwrap();
        let keep = target.join("keep");
        std::fs::write(&keep, "mine").unwrap();
        for stamp_file in [
            None,
            Some("pkg-config directory /x\nlibavcodec 62.28.100\n"),
        ] {
            if let Some(text) = stamp_file {
                std::fs::write(target.join(STAMP_FILE), text).unwrap();
            }
            let e = prepare_target(&target, &s).unwrap_err().to_string();
            assert!(e.contains("didn't make it"), "{e}");
            assert!(e.contains(TARGET_ENV), "{e}");
            let e = clean_target(&target, &[]).unwrap_err().to_string();
            assert!(e.contains("it is left as it is"), "{e}");
            assert!(keep.is_file());
        }
        // A stamp that is a directory, or a symlink, isn't one.
        let other = target_dir(&root, "other");
        std::fs::create_dir_all(other.join(STAMP_FILE)).unwrap();
        assert!(prepare_target(&other, &s).is_err());
        assert!(other.join(STAMP_FILE).is_dir());
        // A symlink to a directory compat made isn't one compat made, and
        // what it leads to is kept.
        let ours = target_dir(&root, "ours");
        assert!(!prepare_target(&ours, &s).unwrap());
        let link = target_dir(&root, "link");
        symlink(&ours, &link);
        assert!(prepare_target(&link, &s).is_err());
        assert!(clean_target(&link, &[]).is_err());
        assert!(ours.join(STAMP_FILE).is_file());
        // What compat made, --clean deletes, and a target that isn't there
        // is nothing to do.
        assert!(clean_target(&ours, &[]).unwrap());
        assert!(!ours.exists());
        assert!(!clean_target(&ours, &[]).unwrap());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_target_that_is_holds_or_lies_inside_a_prefix_is_refused() {
        let d = scratch("guard");
        let prefix = d.join("ff/n8.1");
        std::fs::create_dir_all(prefix.join("lib")).unwrap();
        let link = d.join("link");
        symlink(&prefix, &link);
        let prefixes = [prefix.clone(), d.join("elsewhere/gone")];
        let never = || -> Result<String> { panic!("the target was made ready") };
        for (target, how) in [
            (prefix.clone(), "is"),
            (d.join("ff"), "holds"),
            (d.clone(), "holds"),
            (prefix.join("lib"), "lies inside"),
            // One that isn't there yet, and one through a symlink.
            (prefix.join("lib/spherecut-compat-n8.1"), "lies inside"),
            (link.clone(), "is"),
            (link.join("spherecut-compat-n8.1"), "lies inside"),
        ] {
            let e = guard(&target, &prefixes).unwrap_err().to_string();
            let want = format!(
                "{} {how} the FFmpeg prefix {}",
                target.display(),
                prefix.display()
            );
            assert!(
                e.starts_with(&format!("the target directory {want}")),
                "{e}"
            );
            assert!(ready_target(&target, &prefixes, "", never).is_err());
        }
        // A prefix named through a symlink is resolved too.
        assert!(guard(&prefix.join("lib"), std::slice::from_ref(&link)).is_err());
        // Beside the prefixes is where targets go.
        assert!(guard(&target_dir(&d.join("ff"), "n8.1"), &prefixes).is_ok());
        assert!(guard(&d.join("other/t"), &prefixes).is_ok());
        // --clean is held to it, even for a directory with compat's stamp.
        let inside = prefix.join("lib/spherecut-compat-n8.1");
        made(&inside);
        assert!(clean_target(&inside, &prefixes).is_err());
        assert!(inside.join(STAMP_FILE).is_file());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_target_root_with_dotdot_is_resolved_before_it_is_guarded() {
        let d = scratch("dotdot");
        let prefix = d.join("ff/n8.1");
        std::fs::create_dir_all(prefix.join("lib")).unwrap();
        let real = d.canonicalize().unwrap();
        // `..` after a directory that isn't there yet takes it off again,
        // as `create_dir_all` will make it, and may lead back into what is.
        assert_eq!(
            resolved(&d.join("missing/../ff/n8.1/spherecut-compat-n8.1")),
            real.join("ff/n8.1/spherecut-compat-n8.1")
        );
        assert_eq!(
            resolved(&d.join("ff/new/more/../../n8.1")),
            real.join("ff/n8.1")
        );
        assert_eq!(resolved(&d.join("ff/new/./x/..")), real.join("ff/new"));
        // And what it leads back into is resolved in turn.
        symlink(&d.join("ff"), &d.join("ln"));
        assert_eq!(
            resolved(&d.join("missing/../ln/n8.1/spherecut-compat-n8.1")),
            real.join("ff/n8.1/spherecut-compat-n8.1")
        );
        // The review's root: compat built inside the prefix.
        let prefixes = [prefix.clone()];
        let target = target_dir(&d.join("missing/../ff/n8.1"), "n8.1");
        let e = guard(&target, &prefixes).unwrap_err().to_string();
        assert!(e.contains("lies inside the FFmpeg prefix"), "{e}");
        let never = || -> Result<String> { panic!("the target was made ready") };
        assert!(ready_target(&target, &prefixes, "", never).is_err());
        assert!(!d.join("missing").exists());
        assert!(!prefix.join("spherecut-compat-n8.1").exists());
        let e = guard(
            &target_dir(&d.join("missing/../ln/n8.1"), "n8.1"),
            &prefixes,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("lies inside the FFmpeg prefix"), "{e}");
        // Elsewhere, `..` and all, is where targets go.
        assert!(guard(&target_dir(&d.join("missing/../t"), "n8.1"), &prefixes).is_ok());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_target_inside_an_ffmpeg_this_run_does_not_list_is_refused() {
        let d = scratch("unlisted");
        let ff = d.join("ff/n8.1");
        std::fs::create_dir_all(ff.join("include/libavcodec")).unwrap();
        std::fs::write(ff.join("include/libavcodec/avcodec.h"), "").unwrap();
        let listed = [d.join("ff2/n9.0")];
        let real = ff.canonicalize().unwrap();
        for target in [
            target_dir(&ff, "n9.0"),
            target_dir(&ff.join("lib/deeper"), "n9.0"),
        ] {
            let e = guard(&target, &listed).unwrap_err().to_string();
            let want = format!(
                "lies inside {}, which holds FFmpeg's headers \
                 (include/libavcodec/avcodec.h)",
                real.display()
            );
            assert!(e.contains(&want), "{e}");
            // And says how to build elsewhere.
            assert!(
                e.ends_with(&format!("set {TARGET_ENV} to a directory outside it")),
                "{e}"
            );
        }
        // Through a symlink too.
        let link = d.join("link");
        symlink(&ff, &link);
        assert!(guard(&target_dir(&link, "n9.0"), &listed).is_err());
        // Beside it is fine.
        assert!(guard(&target_dir(&d.join("ff"), "n9.0"), &listed).is_ok());
        std::fs::remove_dir_all(&d).ok();
    }

    /// One of cargo's `build-script-executed` messages, for `package`'s
    /// build script, which linked `libs` and searched `paths`.
    fn ran(package: &str, libs: &[&str], paths: &[String]) -> String {
        let list = |items: &mut dyn Iterator<Item = &str>| {
            items
                .map(|i| format!("\"{}\"", i.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "{{\"reason\":\"build-script-executed\",\"package_id\":\
             \"registry+https://github.com/rust-lang/crates.io-index#{package}@8.0.1\",\
             \"linked_libs\":[{}],\"linked_paths\":[{}],\"cfgs\":[\"feature=\\\"x\\\"\"],\
             \"env\":[],\"out_dir\":\"/t/debug/build/{package}-0/out\"}}\n",
            list(&mut libs.iter().copied()),
            list(&mut paths.iter().map(String::as_str))
        )
    }

    #[test]
    fn what_the_build_bound_is_checked_after_it() {
        let d = scratch("built");
        let prefix = d.join("n8.1");
        let (pc, b) = fake_prefix(&prefix, "62.28.100");
        let before = stamp(&pc, &b);
        let same = || Ok(stamp(&pc, &b));
        let lib = prefix.join("lib");
        let native = |dir: &Path| format!("native={}", dir.display());
        let libs: Vec<&str> = BOUND.iter().map(|(l, _)| &l[3..]).collect();
        let check = |restamp: &dyn Fn() -> Result<String>, messages: &str| {
            check_built(&prefix, &before, restamp, messages).map_err(|e| format!("{e:#}"))
        };
        // Nothing built: nothing says what was bound. Nor does another
        // package's build script.
        for messages in ["", &ran("ffmpeg-next", &libs, &[native(&lib)])] {
            let e = check(&same, messages).unwrap_err();
            assert!(
                e.starts_with("cargo reported no run of ffmpeg-sys-next"),
                "{e}"
            );
        }
        // Nor does one that didn't link libavcodec.
        let e = check(&same, &ran("ffmpeg-sys-next", &["avutil"], &[native(&lib)])).unwrap_err();
        assert!(e.contains("linked no libavcodec (avutil)"), "{e}");
        let good = ran("ffmpeg-sys-next", &libs, &[native(&lib)]);
        assert_eq!(check(&same, &good), Ok(()));
        // Among cargo's other messages, and a line that isn't one.
        let mixed = format!(
            "{{\"reason\":\"compiler-artifact\",\"fresh\":true}}\nwarning: x\n{good}\
             {{\"reason\":\"build-finished\",\"success\":true}}\n"
        );
        assert_eq!(check(&same, &mixed), Ok(()));
        // Another FFmpeg's directory searched, in either build (clippy's or
        // the link's), as when the prefix went away during the build and
        // pkg-config found the system's.
        let other = d.join("other/lib");
        std::fs::create_dir_all(&other).unwrap();
        let elsewhere = ran("ffmpeg-sys-next", &libs, &[native(&other)]);
        for messages in [format!("{good}{elsewhere}"), format!("{elsewhere}{good}")] {
            let e = check(&same, &messages).unwrap_err();
            assert!(e.contains("outside the prefix"), "{e}");
        }
        // No kind, and a directory that isn't there.
        let e = check(
            &same,
            &ran("ffmpeg-sys-next", &libs, &["/nowhere/lib".into()]),
        )
        .unwrap_err();
        assert!(
            e.contains("searched /nowhere/lib for libraries, which isn't there"),
            "{e}"
        );
        let dylib: Vec<String> = libs.iter().map(|l| format!("dylib={l}")).collect();
        let dylib: Vec<&str> = dylib.iter().map(String::as_str).collect();
        let plain = ran("ffmpeg-sys-next", &dylib, &[lib.display().to_string()]);
        assert_eq!(check(&same, &plain), Ok(()));
        // The FFmpeg changed, or pkg-config finds it no more.
        let e = check(&|| Ok("another stamp\n".into()), &good).unwrap_err();
        assert!(e.contains("changed during the build"), "{e}");
        let e = check(&|| bail!("pkg-config finds libavutil in /usr/lib"), &good).unwrap_err();
        assert!(e.contains("pkg-config finds libavutil in /usr/lib"), "{e}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn what_the_build_bound_is_found_wherever_cargo_builds() {
        // A package named ffmpeg-sys-next whose build script links the
        // bound libraries from the prefix, built with cargo's build directory
        // elsewhere (`build.build-dir`) and a target triple named
        // (`build.target`): the script's output is in the build
        // directory's `x86_64-unknown-linux-gnu/debug/build`, not in the
        // target directory's `debug/build`, where compat looked for it.
        let d = scratch("build-dir");
        let prefix = d.join("n8.1");
        let (pc, b) = fake_prefix(&prefix, "62.28.100");
        let pkg = d.join("ffmpeg-sys-next");
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(
            pkg.join("Cargo.toml"),
            "[package]\nname = \"ffmpeg-sys-next\"\nversion = \"8.0.1\"\nedition = \"2021\"\n\
             \n[workspace]\n",
        )
        .unwrap();
        std::fs::write(pkg.join("src/lib.rs"), "").unwrap();
        let search = format!(
            "cargo:rustc-link-search=native={}",
            prefix.join("lib").display()
        );
        let links: String = BOUND
            .iter()
            .map(|(lib, _)| format!("    println!(\"cargo:rustc-link-lib={}\");\n", &lib[3..]))
            .collect();
        std::fs::write(
            pkg.join("build.rs"),
            format!("fn main() {{\n    println!({search:?});\n{links}}}\n"),
        )
        .unwrap();
        let rustc = Command::new("rustc").arg("-vV").output().unwrap();
        let rustc = String::from_utf8(rustc.stdout).unwrap();
        let host = rustc
            .lines()
            .find_map(|l| l.strip_prefix("host: "))
            .unwrap();
        let target = d.join("t");
        let out = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["build", "--offline", JSON])
            .current_dir(&pkg)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_BUILD_DIR", d.join("build"))
            .env("CARGO_BUILD_TARGET", host)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!target.join("debug/build").exists());
        let messages = String::from_utf8(out.stdout).unwrap();
        let restamp = || Ok(stamp(&pc, &b));
        let checked = check_built(&prefix, &stamp(&pc, &b), restamp, &messages);
        assert_eq!(checked.map_err(|e| format!("{e:#}")), Ok(()));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn cargo_messages_are_read_as_json() {
        let v = Json::parse(
            " {\"a\": [1, -2.5e3, true, false, null, {}], \"s\": \"q\\\"b\\\\s\\/\\n\\t\\u00e9\\ud83d\\ude00\"} ",
        )
        .unwrap();
        assert_eq!(
            v.get("a"),
            Some(&Json::Array(vec![
                Json::Number("1".into()),
                Json::Number("-2.5e3".into()),
                Json::Bool(true),
                Json::Bool(false),
                Json::Null,
                Json::Object(vec![]),
            ]))
        );
        assert_eq!(
            v.get("s").and_then(Json::str),
            Some("q\"b\\s/\n\té\u{1f600}")
        );
        assert_eq!(v.get("b"), None);
        for bad in [
            "",
            "{",
            "{\"a\" 1}",
            "[1,]",
            "\"\\x\"",
            "\"\\ud83d\"",
            "nul",
            "{} {}",
        ] {
            assert_eq!(Json::parse(bad), None, "{bad}");
        }
        assert_eq!(
            Json::parse("[\"a\", \"b\"]").and_then(|v| v.strings()),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(Json::parse("[\"a\", 1]").and_then(|v| v.strings()), None);
    }

    #[test]
    fn a_package_id_names_its_package() {
        for id in [
            "registry+https://github.com/rust-lang/crates.io-index#ffmpeg-sys-next@8.0.1",
            "git+https://github.com/zmwangx/rust-ffmpeg-sys?branch=master#ffmpeg-sys-next@8.0.1",
            "path+file:///src/ffmpeg-sys-next#8.0.1",
            "path+file:///src/sys#ffmpeg-sys-next@8.0.1",
            "ffmpeg-sys-next 8.0.1 (registry+https://github.com/rust-lang/crates.io-index)",
        ] {
            assert_eq!(package_name(id), "ffmpeg-sys-next", "{id}");
        }
        assert_eq!(
            package_name("registry+https://github.com/rust-lang/crates.io-index#ffmpeg-next@8.0.0"),
            "ffmpeg-next"
        );
    }

    #[test]
    fn an_ffmpeg_that_making_its_target_ready_changes_is_refused() {
        // A prefix whose libavcodec.pc is a link into its stale target:
        // deleting the target takes the file with it.
        let d = scratch("restamp");
        let (pc, b) = fake_prefix(&d.join("n8.1"), "62.28.100");
        let target = target_dir(&d.join("t"), "n8.1");
        let prefixes = [d.join("n8.1")];
        let restamp = || Ok(stamp(&pc, &b));
        assert!(!ready_target(&target, &prefixes, &stamp(&pc, &b), restamp).unwrap());
        std::fs::rename(pc.join("libavcodec.pc"), target.join("libavcodec.pc")).unwrap();
        symlink(&target.join("libavcodec.pc"), &pc.join("libavcodec.pc"));
        let s = stamp(&pc, &b);
        let e = ready_target(&target, &prefixes, &s, restamp)
            .unwrap_err()
            .to_string();
        assert!(e.contains("the FFmpeg changed"), "{e}");
        assert!(e.contains("libavcodec.pc missing"), "{e}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_target_whose_path_holds_a_brace_is_refused_before_a_build() {
        // Cargo would fail every build there: "unexpected variable `x` in
        // build.build-dir path".
        let d = scratch("brace");
        let (pc, b) = fake_prefix(&d.join("n8.1"), "62.28.100");
        let prefixes = [d.join("n8.1")];
        let restamp = || Ok(stamp(&pc, &b));
        for root in ["ct{x}", "ct}", "{workspace-root}"] {
            let target = target_dir(&d.join(root), "n8.1");
            let e = ready_target(&target, &prefixes, &stamp(&pc, &b), restamp)
                .unwrap_err()
                .to_string();
            assert!(e.contains("has a brace in its path"), "{e}");
            assert!(e.contains(TARGET_ENV), "{e}");
            assert!(!d.join(root).exists(), "{root}");
        }
        let target = target_dir(&d.join("ct"), "n8.1");
        assert!(!ready_target(&target, &prefixes, &stamp(&pc, &b), restamp).unwrap());
        std::fs::remove_dir_all(&d).ok();
    }

    /// `lib`'s pkg-config file in `pc`, naming `libdir` and `includedir`.
    fn write_pc(pc: &Path, lib: &str, libdir: &Path, includedir: &Path) {
        std::fs::create_dir_all(pc).unwrap();
        let text = format!(
            "libdir={}\nincludedir={}\n\nName: {lib}\nDescription: FFmpeg\n\
             Version: 61.19.100\nLibs: -L${{libdir}} -l{}\nCflags: -I${{includedir}}\n",
            libdir.display(),
            includedir.display(),
            &lib[3..]
        );
        std::fs::write(pc.join(format!("{lib}.pc")), text).unwrap();
    }

    /// A prefix at `dir` whose pkg-config files, one for each bound
    /// library, name `libdir` and `includedir`; and the directory holding
    /// them.
    fn pc_prefix(dir: &Path, libdir: &Path, includedir: &Path) -> PathBuf {
        let pc = dir.join("lib/pkgconfig");
        for (lib, _) in BOUND {
            write_pc(&pc, lib, libdir, includedir);
        }
        pc
    }

    /// Each bound library in `lib` and its header in `include`.
    fn install(lib: &Path, include: &Path) {
        std::fs::create_dir_all(lib).unwrap();
        for (name, header) in BOUND {
            std::fs::write(lib.join(format!("{name}.so")), "").unwrap();
            let h = include.join(header);
            std::fs::create_dir_all(h.parent().unwrap()).unwrap();
            std::fs::write(h, "").unwrap();
        }
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
        assert_eq!(b.libdirs, [ff.join("lib")]);
        // Through a symlink to the prefix, everything is still inside it.
        let link = d.join("link");
        symlink(&ff, &link);
        assert!(bind(&link, &link.join("lib"), &link.join("include")).is_ok());
        // Another FFmpeg's library or header, named outright.
        let e = bind(&ff, &other.join("lib"), &ff.join("include")).unwrap_err();
        assert!(e.contains("libavutil's library"), "{e}");
        assert!(e.contains("outside the prefix"), "{e}");
        let e = bind(&ff, &ff.join("lib"), &other.join("include")).unwrap_err();
        assert!(e.contains("libavutil's header"), "{e}");
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
        assert!(e.contains("libavutil's header"), "{e}");
        assert!(e.contains("isn't there"), "{e}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn every_library_the_build_binds_must_be_the_prefixs() {
        let d = scratch("every");
        let ff = d.join("ff");
        install(&ff.join("lib"), &ff.join("include"));
        let other = d.join("other");
        install(&other.join("lib"), &other.join("include"));
        let bind = |pc: &Path| {
            bound(OsStr::new("pkg-config"), &ff, pc, pc.as_os_str()).map_err(|e| format!("{e:#}"))
        };
        for (lib, _) in BOUND {
            // Only this library's file leads to another FFmpeg, as when a
            // moved prefix's libavcodec.pc alone was put right.
            let pc = pc_prefix(&ff, &ff.join("lib"), &ff.join("include"));
            assert!(bind(&pc).is_ok());
            write_pc(&pc, lib, &other.join("lib"), &ff.join("include"));
            let e = bind(&pc).unwrap_err();
            assert!(e.starts_with(&format!("{lib}'s library")), "{e}");
            assert!(e.contains("outside the prefix"), "{e}");
            write_pc(&pc, lib, &ff.join("lib"), &other.join("include"));
            let e = bind(&pc).unwrap_err();
            assert!(e.starts_with(&format!("{lib}'s header")), "{e}");
            // Or the prefix has none, as an install that stopped partway
            // left it: pkg-config finds the system's, or nothing.
            std::fs::remove_file(pc.join(format!("{lib}.pc"))).unwrap();
            let e = bind(&pc).unwrap_err();
            assert!(
                e.starts_with(&format!("pkg-config finds {lib} in "))
                    || e.starts_with(&format!("pkg-config --variable=pcfiledir {lib} failed")),
                "{e}"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_libraries_checked_are_those_the_build_links() {
        let d = scratch("linked");
        let prefix = d.join("n8.1");
        let (pc, b) = fake_prefix(&prefix, "62.28.100");
        let before = stamp(&pc, &b);
        let lib = format!("native={}", prefix.join("lib").display());
        let check = |messages: &str| {
            check_built(&prefix, &before, || Ok(stamp(&pc, &b)), messages)
                .map_err(|e| format!("{e:#}"))
        };
        let linking =
            |libs: &[&str]| check(&ran("ffmpeg-sys-next", libs, std::slice::from_ref(&lib)));
        let bound: Vec<&str> = BOUND.iter().map(|(l, _)| &l[3..]).collect();
        assert_eq!(linking(&bound), Ok(()));
        // What isn't FFmpeg's isn't asked about.
        assert_eq!(linking(&[&bound[..], &["z", "static=m"]].concat()), Ok(()));
        // One more of FFmpeg's, as a feature turned on anywhere binds, in
        // any form a build script can give it.
        for (extra, name) in [
            ("avdevice", "libavdevice"),
            ("dylib=avfilter", "libavfilter"),
            ("static:+whole-archive=postproc:pp", "libpostproc"),
            ("avresample", "libavresample"),
        ] {
            let e = linking(&[&bound[..], &[extra]].concat()).unwrap_err();
            let want = format!("linked {name}, which compat doesn't ask pkg-config about");
            assert!(e.contains(&want), "{e}");
        }
        // One fewer: pkg-config is asked about a library no build binds.
        let e = linking(&["avutil", "avformat", "avcodec", "swscale"]).unwrap_err();
        assert!(e.starts_with("no build linked libswresample"), "{e}");
        // One build may link what another doesn't (clippy's has every
        // feature of the workspace's crates turned on).
        let split = format!(
            "{}{}",
            ran(
                "ffmpeg-sys-next",
                &["avutil", "avformat", "avcodec"],
                std::slice::from_ref(&lib)
            ),
            ran(
                "ffmpeg-sys-next",
                &["avcodec", "swscale", "swresample"],
                std::slice::from_ref(&lib)
            ),
        );
        assert_eq!(check(&split), Ok(()));
        for (lib, header) in BOUND {
            assert!(STAMPED.contains(&lib), "{lib}");
            assert!(header.starts_with(&format!("{lib}/")), "{header}");
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_libraries_checked_are_those_the_workspace_binds() {
        // The features cargo resolves for ffmpeg-sys-next with every
        // feature of the workspace's crates on, as clippy builds it: each
        // library's feature binds it, and libavutil is always bound. So a
        // feature turned on anywhere that binds another library fails
        // here, before a compat run would.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        // This machine's packages only: a cargo cache that has built the
        // workspace here (odin's) has no Android or Windows crates, and
        // offline, metadata for every platform would need them.
        let rustc = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("-vV")
            .output()
            .unwrap();
        let rustc = String::from_utf8(rustc.stdout).unwrap();
        let host = rustc
            .lines()
            .find_map(|l| l.strip_prefix("host: "))
            .unwrap();
        let out = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["metadata", "--format-version", "1", "--all-features"])
            .args(["--offline", "--locked", "--filter-platform", host])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let meta = Json::parse(&String::from_utf8(out.stdout).unwrap()).unwrap();
        let Some(Json::Array(packages)) = meta.get("packages") else {
            panic!("no packages");
        };
        let ids: Vec<&str> = packages
            .iter()
            .filter(|p| p.get("name").and_then(Json::str) == Some("ffmpeg-sys-next"))
            .filter_map(|p| p.get("id").and_then(Json::str))
            .collect();
        assert_eq!(ids.len(), 1, "{ids:?}");
        let Some(Json::Array(nodes)) = meta.get("resolve").and_then(|r| r.get("nodes")) else {
            panic!("no resolve nodes");
        };
        let features = nodes
            .iter()
            .find(|n| n.get("id").and_then(Json::str) == Some(ids[0]))
            .and_then(|n| n.get("features"))
            .and_then(Json::strings)
            .unwrap();
        let mut bound: Vec<String> = features
            .iter()
            .map(|f| format!("lib{f}"))
            .filter(|l| STAMPED.contains(&l.as_str()) || DROPPED.contains(&l.as_str()))
            .chain(["libavutil".to_owned()])
            .collect();
        bound.sort();
        bound.dedup();
        let mut checked: Vec<&str> = BOUND.iter().map(|(l, _)| *l).collect();
        checked.sort_unstable();
        assert_eq!(bound, checked, "ffmpeg-sys-next's features: {features:?}");
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
            e.starts_with("false --variable=pcfiledir libavutil failed"),
            "{e}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    fn found(v: &str, ff: Option<&str>) -> Result<Bound, String> {
        Ok(Bound {
            libavcodec: v.into(),
            ffmpeg: ff.map(Into::into),
            version_header: None,
            libdirs: vec![p("/lib")],
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
                binding: Some(Stage::Passed),
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
                binding: Some(Stage::Passed),
            },
            Row {
                label: "n9.0.2".into(),
                prefix: p("/ff/n9.0.2"),
                bound: found("63.3.100", None),
                clippy: Stage::Failed("clippy failed (exit status: 101)".into()),
                link: None,
                tier1: None,
                binding: None,
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
                binding: Some(Stage::Passed),
            },
            Row {
                label: "moved".into(),
                prefix: p("/ff/moved"),
                bound: found("62.28.100", Some("n8.1")),
                clippy: Stage::Passed,
                link: Some(Stage::Passed),
                tier1: None,
                binding: Some(Stage::Failed(
                    "ffmpeg-sys-next's build in /t searched /usr/lib for libraries, \
                     outside the prefix /ff/moved"
                        .into(),
                )),
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
                binding: None,
            },
        ];
        let r = report(&rows);
        let want = "\
==> FFmpeg compatibility: 1 of 6 builds passed
  n7.1         FFmpeg n7.1, libavcodec 61.19.100    clippy passed, link passed, binding passed
               /ff/n7.1
  n8.1.3-full  FFmpeg n8.1.3, libavcodec 62.28.103  clippy passed, link passed, Tier 1 FAILED, binding passed
               /ff/n8.1.3-full
               tests (Tier 1) failed (exit status: 101)
  n9.0.2       libavcodec 63.3.100                  clippy FAILED
               /ff/n9.0.2
               clippy failed (exit status: 101)
  n8.1         FFmpeg n8.1, libavcodec 62.28.100    clippy passed, link FAILED, binding passed
               /ff/n8.1
               link: the tests and binaries built against FFmpeg n8.1 failed (exit status: 101)
  moved        FFmpeg n8.1, libavcodec 62.28.100    clippy passed, link passed, binding FAILED
               /ff/moved
               ffmpeg-sys-next's build in /t searched /usr/lib for libraries, outside the prefix /ff/moved
  gone         -                                    not an FFmpeg prefix: no libavcodec.pc in /ff/gone/lib/pkgconfig
               /ff/gone
               its FFmpeg was not found
";
        assert_eq!(r, want, "\n{r}");
        let failed: Vec<bool> = rows.iter().map(Row::failed).collect();
        assert_eq!(failed, [false, true, true, true, true, true]);
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
            binding: Some(Stage::Passed),
        };
        assert!(row.failed());
        assert!(report(&[row]).contains("clippy passed, link FAILED, Tier 1 not run"));
        // Nor is a link that did not happen a pass, nor a build whose
        // binding was never checked.
        let row = |link: Option<Stage>, binding: Option<Stage>| Row {
            label: "n8.1".into(),
            prefix: p("/ff/n8.1"),
            bound: found("62.28.100", None),
            clippy: Stage::Passed,
            link,
            tier1: None,
            binding,
        };
        assert!(row(None, Some(Stage::Passed)).failed());
        assert!(row(Some(Stage::Passed), None).failed());
        assert!(!row(Some(Stage::Passed), Some(Stage::Passed)).failed());
    }
}
