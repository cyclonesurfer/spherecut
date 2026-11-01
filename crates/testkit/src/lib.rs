//! Shared test helpers.
//!
//! # Tier 2 tests
//!
//! Tests that need real hardware or Pat's camera samples are marked
//! `#[ignore = "tier2: <what it needs>"]`, so a plain `cargo test` lists them
//! as ignored with the reason. `cargo xtask hw-test` runs them with
//! `--ignored` and `SPHERECUT_TIER2=1`.
//!
//! Inside such a test, call the `tier2::require_*` helpers first. When a
//! prerequisite is missing they **fail** the test with a `TIER2-SKIP:`
//! marker, which the hw-test report shows as "skipped" with the reason. A
//! Tier 2 test therefore never passes on a machine that lacks what it needs.
//!
//! There are two kinds (M3 plan 4.4). **Tier 2h** tests need the hardware,
//! real time or the GUI, and run only under hw-test: their gates, and the
//! samples gates `require_samples_dir` and `require_sample` beside them,
//! call `require_hw_run` first, which skips unless `SPHERECUT_TIER2` is
//! set. **Tier 2c** tests need Pat's samples and the CPU only, so agents
//! run them in the cloud while they implement, with `SPHERECUT_TIER2`
//! unset:
//! `SPHERECUT_SAMPLES=<folder> cargo test -p <crate> --test <file> -- --ignored <name>`.
//! They gate on `tier2::require_samples_cpu` alone, which needs only
//! `SPHERECUT_SAMPLES`; hw-test runs them with the rest.
//!
//! # Controls
//!
//! Every check has a control that must fail, run on every run
//! ([`control::must_fail`]): a control that stops failing fails the test.
//!
//! # Hashing
//!
//! [`fnv64`] is the hash the golden manifest identifies a source's planes
//! by. The image metrics are render's (`spherecut_render::compare`).

#![forbid(unsafe_code)]

pub mod control;

/// FNV-1a, 64 bits, fed in pieces: the planes of a frame row by row, say,
/// so that row padding doesn't change the hash.
#[derive(Debug, Clone, Copy)]
pub struct Fnv64(u64);

impl Fnv64 {
    pub const fn new() -> Self {
        Fnv64(0xcbf2_9ce4_8422_2325)
    }

    pub fn write(&mut self, bytes: &[u8]) -> &mut Self {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv64 {
    fn default() -> Self {
        Fnv64::new()
    }
}

/// FNV-1a, 64 bits, of `bytes`.
pub fn fnv64(bytes: &[u8]) -> u64 {
    Fnv64::new().write(bytes).finish()
}

pub mod tier2 {
    use std::path::{Path, PathBuf};

    /// Marker the hw-test report looks for in a failed test's output.
    pub const SKIP_MARKER: &str = "TIER2-SKIP:";

    /// Fail the current test as skipped, with a reason.
    #[track_caller]
    pub fn skip(reason: impl std::fmt::Display) -> ! {
        panic!("{SKIP_MARKER} {reason}");
    }

    /// Require that we are running under `cargo xtask hw-test`.
    #[track_caller]
    pub fn require_hw_run() {
        if std::env::var_os("SPHERECUT_TIER2").is_none() {
            skip("not running under `cargo xtask hw-test` (SPHERECUT_TIER2 unset)");
        }
    }

    /// The samples directory, or skip if it is missing.
    #[track_caller]
    pub fn require_samples_dir() -> PathBuf {
        require_hw_run();
        usable_samples_dir(samples_path())
    }

    /// The samples directory for a Tier 2c test, one that needs Pat's
    /// samples and the CPU only: `SPHERECUT_SAMPLES`, or skip when it is
    /// unset or names no folder. Unlike [`require_samples_dir`] it doesn't
    /// need hw-test (`SPHERECUT_TIER2`), so the test runs in the cloud too.
    #[track_caller]
    pub fn require_samples_cpu() -> PathBuf {
        samples_cpu_in(std::env::var_os("SPHERECUT_SAMPLES").map(PathBuf::from))
    }

    /// [`require_samples_cpu`]'s rule, on `SPHERECUT_SAMPLES` as `var`
    /// holds it.
    #[track_caller]
    pub(crate) fn samples_cpu_in(var: Option<PathBuf>) -> PathBuf {
        let Some(dir) = var else {
            skip("SPHERECUT_SAMPLES unset: a Tier 2c test needs it to name the samples folder");
        };
        usable_samples_dir(dir)
    }

    /// The samples directory when there is one, for a test that goes on
    /// without it.
    pub fn samples_dir() -> Option<PathBuf> {
        Some(samples_path()).filter(|d| d.is_dir())
    }

    /// `SPHERECUT_SAMPLES`, or `samples`.
    fn samples_path() -> PathBuf {
        std::env::var_os("SPHERECUT_SAMPLES")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("samples"))
    }

    /// `dir`, or skip if it is missing. Its path need not be UTF-8: the
    /// samples tests open files through `spherecut-media`, which gives
    /// FFmpeg the path's bytes as they are.
    #[track_caller]
    pub(crate) fn usable_samples_dir(dir: PathBuf) -> PathBuf {
        if !dir.is_dir() {
            skip(format!("samples directory {} not found", dir.display()));
        }
        dir
    }

    /// A specific sample file under the samples directory, or skip.
    #[track_caller]
    pub fn require_sample(rel: impl AsRef<Path>) -> PathBuf {
        let p = require_samples_dir().join(rel.as_ref());
        if !p.is_file() {
            skip(format!("sample {} not found", p.display()));
        }
        p
    }

    /// The variable naming the directory of hw-test's perf clips.
    pub const PERF_MEDIA_ENV: &str = "SPHERECUT_PERF_MEDIA";

    /// One of the perf clips `cargo xtask hw-test` generates for the
    /// benchmarks (`spherecut_synth::media::perf_clips`), by its file
    /// name, or skip: hw-test leaves them out with `--quick`, and says
    /// in its report why one it couldn't make is missing.
    #[track_caller]
    pub fn require_perf_clip(file_name: &str) -> PathBuf {
        require_hw_run();
        perf_clip(file_name).unwrap_or_else(|why| skip(why))
    }

    /// [`require_perf_clip`]'s clip, or why it isn't there, for a test
    /// that goes on without it.
    pub fn perf_clip(file_name: &str) -> Result<PathBuf, String> {
        perf_clip_in(
            std::env::var_os(PERF_MEDIA_ENV).map(PathBuf::from),
            file_name,
        )
    }

    pub(crate) fn perf_clip_in(dir: Option<PathBuf>, file_name: &str) -> Result<PathBuf, String> {
        let Some(dir) = dir else {
            return Err(format!(
                "no perf clips ({PERF_MEDIA_ENV} unset): `cargo xtask hw-test` makes them, \
                 except with --quick"
            ));
        };
        let p = dir.join(file_name);
        if !p.is_file() {
            return Err(format!(
                "perf clip {} not found: hw-test's report says why it wasn't made",
                p.display()
            ));
        }
        Ok(p)
    }

    /// The DRM render node VA-API is tested on: `SPHERECUT_RENDER_NODE`,
    /// default `/dev/dri/renderD128`.
    fn configured_render_node() -> PathBuf {
        std::env::var_os("SPHERECUT_RENDER_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/dev/dri/renderD128"))
    }

    /// A DRM render node for VA-API, or skip.
    #[track_caller]
    pub fn require_render_node() -> PathBuf {
        require_hw_run();
        let p = configured_render_node();
        if !p.exists() {
            skip(format!("no DRM render node at {}", p.display()));
        }
        p
    }

    /// VA-API's render node, if it is there, without skipping. The Vulkan
    /// tests use it to pick the same GPU, and need none.
    pub fn render_node() -> Option<PathBuf> {
        Some(configured_render_node()).filter(|p| p.exists())
    }

    /// The display a GUI test's child process opens its window on: the
    /// variables to set on it, and how they were found.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GraphicalSession {
        pub env: Vec<(&'static str, std::ffi::OsString)>,
        pub found: SessionFrom,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SessionFrom {
        /// `WAYLAND_DISPLAY` was set.
        WaylandDisplay,
        /// The one Wayland socket of the user's in `XDG_RUNTIME_DIR`
        /// (`/run/user/<uid>` without it): hw-test was started from a
        /// TTY while the user's desktop runs.
        Discovered,
        /// `DISPLAY` was set (X11, or Xwayland).
        X11,
    }

    impl SessionFrom {
        /// As the hw-test report says it.
        pub fn describe(self) -> &'static str {
            match self {
                SessionFrom::WaylandDisplay => "WAYLAND_DISPLAY set",
                SessionFrom::Discovered => "Wayland socket discovered",
                SessionFrom::X11 => "DISPLAY set",
            }
        }
    }

    /// Why there is no session to run in.
    pub const NO_SESSION: &str = "no graphical session: run from a terminal inside GNOME or \
         Hyprland, or leave the desktop unlocked and on screen";

    /// The graphical session to open a window in, or skip: `WAYLAND_DISPLAY`
    /// when set; otherwise, from a TTY, the single `wayland-*` socket the
    /// user owns in `XDG_RUNTIME_DIR`, for the child process only;
    /// otherwise `DISPLAY` (M1 plan section 12, item 5).
    #[track_caller]
    pub fn require_graphical_session() -> GraphicalSession {
        require_hw_run();
        graphical_session(|k| std::env::var_os(k), own_uid()).unwrap_or_else(|why| skip(why))
    }

    /// [`require_graphical_session`]'s rule, on these variables, for the
    /// user `uid` (its sockets, and its `/run/user/<uid>`).
    pub fn graphical_session(
        var: impl Fn(&str) -> Option<std::ffi::OsString>,
        uid: Option<u32>,
    ) -> Result<GraphicalSession, String> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let set = |k: &str| var(k).filter(|v| !v.is_empty());
        if let Some(v) = set("WAYLAND_DISPLAY") {
            return Ok(GraphicalSession {
                env: vec![("WAYLAND_DISPLAY", v)],
                found: SessionFrom::WaylandDisplay,
            });
        }
        let runtime = set("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| uid.map(|u| PathBuf::from(format!("/run/user/{u}"))));
        let mut sockets: Vec<String> = runtime
            .as_deref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let meta = e.metadata().ok()?;
                let ours = uid.is_none_or(|u| meta.uid() == u);
                (name.starts_with("wayland-") && meta.file_type().is_socket() && ours)
                    .then_some(name)
            })
            .collect();
        sockets.sort();
        if let ([one], Some(dir)) = (&sockets[..], &runtime) {
            let mut env = vec![("WAYLAND_DISPLAY", one.into())];
            if set("XDG_RUNTIME_DIR").is_none() {
                env.push(("XDG_RUNTIME_DIR", dir.clone().into_os_string()));
            }
            return Ok(GraphicalSession {
                env,
                found: SessionFrom::Discovered,
            });
        }
        if let Some(v) = set("DISPLAY") {
            return Ok(GraphicalSession {
                env: vec![("DISPLAY", v)],
                found: SessionFrom::X11,
            });
        }
        Err(if sockets.len() > 1 {
            format!(
                "{NO_SESSION} (or set WAYLAND_DISPLAY to one of {})",
                sockets.join(", ")
            )
        } else {
            NO_SESSION.to_owned()
        })
    }

    /// Whether the user's graphical session is locked, by logind's
    /// `LockedHint`, which GNOME sets; a locker that doesn't tell logind
    /// leaves it "no". None when it can't be read (no `loginctl`, no
    /// graphical session of the user's).
    pub fn session_locked() -> Option<bool> {
        locked_hint(own_uid()?, |args| {
            let out = std::process::Command::new("loginctl")
                .args(args)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        })
    }

    /// [`session_locked`]'s rule for the user `uid`, with `loginctl` as
    /// `run` answers it: the user's graphical session (`Display`), then
    /// its `LockedHint`.
    pub fn locked_hint(uid: u32, run: impl Fn(&[&str]) -> Option<String>) -> Option<bool> {
        let uid = uid.to_string();
        let display = run(&["show-user", &uid, "--property=Display", "--value"])?;
        let display = display.trim();
        if display.is_empty() {
            return None;
        }
        match run(&["show-session", display, "--property=LockedHint", "--value"])?.trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        }
    }

    /// The process's real user id, from `/proc/self/status`.
    pub fn own_uid() -> Option<u32> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find_map(|l| l.strip_prefix("Uid:"))?;
        line.split_whitespace().next()?.parse().ok()
    }

    /// The default audio output device, as `open` opens it, or skip with
    /// the reason `open` gives when there is none. `open` panics, failing
    /// the test, when there is a device that won't play: that isn't a
    /// skip. (testkit has no dependencies, so the caller opens it.)
    #[track_caller]
    pub fn require_audio_device<T>(open: impl FnOnce() -> Result<T, String>) -> T {
        require_hw_run();
        match open() {
            Ok(device) => device,
            Err(why) => skip(format!("no audio output device: {why}")),
        }
    }
}

/// Benchmark results from Tier 2 tests. `cargo xtask hw-test` sets
/// `SPHERECUT_BENCH_OUT` to a file and puts every recorded line into the
/// report's benchmark table, and every note under it; elsewhere results
/// and notes go to stderr.
pub mod bench {
    use std::convert::Infallible;
    use std::fmt::Display;
    use std::io::Write;

    /// What a note's line in the results file starts with. Results start
    /// with their name, so `#` keeps notes apart from them.
    pub const NOTE_PREFIX: &str = "# ";

    /// How many measured runs [`measure`] makes: `SPHERECUT_BENCH_RUNS`,
    /// default 3.
    pub fn runs() -> usize {
        std::env::var("SPHERECUT_BENCH_RUNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(3)
    }

    /// Run `f` once to warm up (file cache, GPU clocks), then [`runs`]
    /// times, and record the median and the range. `f` returns one
    /// measurement in `unit`.
    pub fn measure(name: &str, unit: &str, mut f: impl FnMut() -> f64) {
        let Ok(()) = try_measure(name, unit, || Ok::<_, Infallible>(f()));
    }

    /// Like [`measure`], for an `f` that can fail, such as a decode on a
    /// device that may turn the file down: on the first failure, of the
    /// warm-up or of a run, nothing is recorded and the error is returned.
    pub fn try_measure<E>(
        name: &str,
        unit: &str,
        f: impl FnMut() -> Result<f64, E>,
    ) -> Result<(), E> {
        record_runs(name, &warm_then_run(runs(), f)?, unit);
        Ok(())
    }

    /// For latencies, such as a seek's: `batch` measures a batch of them
    /// in `unit`, and runs once to warm up, then [`runs`] times. Two lines
    /// are recorded, `{name}: median` and `{name}: p95`, each with the
    /// median over the batches of that batch statistic and their range, so
    /// that a spread says how much the batches differ, not how much one
    /// seek differs from another. On the first failure nothing is recorded
    /// and the error is returned.
    pub fn try_measure_latency<E>(
        name: &str,
        unit: &str,
        mut batch: impl FnMut() -> Result<Vec<f64>, E>,
    ) -> Result<(), E> {
        let mut medians = Vec::new();
        let mut p95s = Vec::new();
        warm_then_run(runs(), || {
            let v = batch()?;
            medians.push(percentile(&v, 50.0).unwrap_or(f64::NAN));
            p95s.push(percentile(&v, 95.0).unwrap_or(f64::NAN));
            Ok(0.0)
        })?;
        // The warm-up's are left out.
        record_runs(&format!("{name}: median"), &medians[1..], unit);
        record_runs(&format!("{name}: p95"), &p95s[1..], unit);
        Ok(())
    }

    /// The `p`th percentile of `values` by the nearest rank: the smallest
    /// value at least `p`% of them are at or below. `None` for none.
    pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let rank = (p / 100.0 * v.len() as f64).ceil() as usize;
        v.get(rank.clamp(1, v.len().max(1)) - 1).copied()
    }

    /// `f`'s results after one warm-up call, `runs` of them, or its first
    /// error.
    fn warm_then_run<E>(runs: usize, mut f: impl FnMut() -> Result<f64, E>) -> Result<Vec<f64>, E> {
        f()?;
        (0..runs).map(|_| f()).collect()
    }

    /// Say why benchmark `name` has no result, e.g. that this machine's
    /// driver can't decode the format. It becomes a note (see [`note`]).
    pub fn not_measured(name: &str, reason: impl Display) {
        note(format!("{name}: not measured: {reason}"));
    }

    /// A note for the report's Benchmarks section, such as which device a
    /// benchmark ran on or why one has no result. Its whitespace is
    /// collapsed, so it stays one line.
    pub fn note(text: impl Display) {
        write_line(&note_line(&text.to_string()));
    }

    fn note_line(text: &str) -> String {
        let words: Vec<&str> = text.split_whitespace().collect();
        format!("{NOTE_PREFIX}{}\n", words.join(" "))
    }

    /// Record one measurement, e.g. `record("decode 3840x1920 hevc8 vaapi", 212.0, "fps")`.
    pub fn record(name: &str, value: f64, unit: &str) {
        record_runs(name, &[value], unit);
    }

    /// Record several runs of one benchmark as a line of
    /// `name, median, unit, min, max, runs`, tab-separated. The numbers
    /// are written at full precision; hw-test rounds them for its table,
    /// after working out the spread.
    pub fn record_runs(name: &str, values: &[f64], unit: &str) {
        if let Some(line) = line(name, values, unit) {
            write_line(&line);
        }
    }

    /// Append `line` to the results file, or print it.
    fn write_line(line: &str) {
        match std::env::var_os("SPHERECUT_BENCH_OUT") {
            Some(p) => {
                let ok = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .and_then(|mut f| f.write_all(line.as_bytes()));
                if let Err(e) = ok {
                    eprintln!("could not record benchmark to {}: {e}", p.to_string_lossy());
                }
            }
            None => eprint!("BENCH {line}"),
        }
    }

    /// The `hw-bench.tsv` line for `values`, `None` for no values.
    fn line(name: &str, values: &[f64], unit: &str) -> Option<String> {
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let (min, max) = (v.first()?, v.last()?);
        let n = v.len();
        let median = if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        };
        Some(format!("{name}\t{median}\t{unit}\t{min}\t{max}\t{n}\n"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn runs_are_recorded_as_median_and_range() {
            let mut calls = 0;
            let v = warm_then_run(4, || {
                calls += 1;
                Ok::<_, Infallible>([0.0, 4.0, 1.0, 3.0, 2.0][calls - 1])
            });
            // The warm-up's 0.0 is left out.
            let Ok(v) = v;
            assert_eq!(v, [4.0, 1.0, 3.0, 2.0]);
            assert_eq!(
                line("m", &v, "fps").as_deref(),
                Some("m\t2.5\tfps\t1\t4\t4\n")
            );
            assert_eq!(
                line("one", &[5.0], "fps").as_deref(),
                Some("one\t5\tfps\t5\t5\t1\n")
            );
            // Unrounded: as 0.06, 0.05 and 0.06, a 7% spread read as 17%.
            assert_eq!(
                line("8K", &[0.058, 0.054, 0.056], "x").as_deref(),
                Some("8K\t0.056\tx\t0.054\t0.058\t3\n")
            );
            assert_eq!(line("none", &[], "fps"), None);
        }

        #[test]
        fn a_failure_stops_the_runs() {
            let calls = std::cell::Cell::new(0);
            let run = |fail_at| {
                calls.set(0);
                warm_then_run(3, || {
                    calls.set(calls.get() + 1);
                    if calls.get() == fail_at {
                        Err(fail_at)
                    } else {
                        Ok(1.0)
                    }
                })
            };
            // The warm-up fails, then the second run: nothing more runs.
            assert_eq!((run(1), calls.get()), (Err(1), 1));
            assert_eq!((run(3), calls.get()), (Err(3), 3));
            assert_eq!((run(9), calls.get()), (Ok(vec![1.0; 3]), 4));
        }

        #[test]
        fn percentiles_are_by_nearest_rank() {
            let v: Vec<f64> = (1..=20).map(f64::from).rev().collect();
            assert_eq!(percentile(&v, 50.0), Some(10.0));
            assert_eq!(percentile(&v, 95.0), Some(19.0));
            assert_eq!(percentile(&v, 100.0), Some(20.0));
            assert_eq!(percentile(&v, 0.0), Some(1.0));
            assert_eq!(percentile(&[3.0], 95.0), Some(3.0));
            assert_eq!(percentile(&[], 50.0), None);
        }

        #[test]
        fn notes_are_one_line_apart_from_results() {
            assert_eq!(
                note_line("vulkan row: not measured:\n  [hevc]\tno decode"),
                "# vulkan row: not measured: [hevc] no decode\n"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fnv64, fnv64, tier2};

    #[test]
    fn fnv64_has_the_reference_values() {
        // From the FNV reference test suite (FNV-1a, 64 bits).
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv64(b"foobar"), 0x8594_4171_f739_67e8);
        // In pieces, the same.
        assert_eq!(
            Fnv64::new().write(b"foo").write(b"bar").finish(),
            fnv64(b"foobar")
        );
    }

    #[test]
    fn skip_panics_with_marker() {
        let r = std::panic::catch_unwind(|| tier2::skip("no GPU"));
        let msg = r
            .err()
            .and_then(|e| e.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        assert_eq!(msg, "TIER2-SKIP: no GPU");
    }

    /// A test's own directory, removed with what it holds when dropped,
    /// so also when the test fails.
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn samples_with_a_path_that_is_not_utf8_are_used() {
        use std::os::unix::ffi::OsStrExt;
        let base = std::env::temp_dir().join(format!("spherecut-testkit-{}", std::process::id()));
        let guard = TempDir(base.clone());
        let dir = base.join(std::ffi::OsStr::from_bytes(b"samples-\xff"));
        std::fs::create_dir_all(&dir).unwrap();
        let usable = |dir: std::path::PathBuf| {
            std::panic::catch_unwind(|| tier2::usable_samples_dir(dir))
                .map_err(|e| e.downcast_ref::<String>().cloned().unwrap_or_default())
        };
        assert_eq!(usable(dir.clone()), Ok(dir));
        let msg = usable(base.join("none")).unwrap_err();
        assert!(
            msg.starts_with("TIER2-SKIP: samples directory ") && msg.ends_with(" not found"),
            "{msg}"
        );
        drop(guard);
        assert!(!base.exists());
    }

    /// What `f` skipped with, or the folder it returned.
    fn gate(
        f: impl FnOnce() -> std::path::PathBuf + std::panic::UnwindSafe,
    ) -> Result<std::path::PathBuf, String> {
        std::panic::catch_unwind(f)
            .map_err(|e| e.downcast_ref::<String>().cloned().unwrap_or_default())
    }

    #[test]
    fn the_cpu_gate_returns_the_samples_folder_alone() {
        // No SPHERECUT_TIER2 asked for: `cargo test` leaves it unset.
        let base = std::env::temp_dir().join(format!("spherecut-cpu-{}", std::process::id()));
        let guard = TempDir(base.clone());
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.clone();
        assert_eq!(gate(|| tier2::samples_cpu_in(Some(dir))), Ok(base.clone()));
        drop(guard);
    }

    #[test]
    fn the_cpu_gate_skips_without_its_folder() {
        use crate::control::must_fail;
        let missing = std::env::temp_dir().join(format!("spherecut-none-{}", std::process::id()));
        let named = missing.clone();
        let why = must_fail(
            "a missing samples folder",
            gate(|| tier2::samples_cpu_in(Some(named))),
        );
        assert_eq!(
            why,
            format!(
                "TIER2-SKIP: samples directory {} not found",
                missing.display()
            )
        );
        let why = must_fail(
            "SPHERECUT_SAMPLES unset",
            gate(|| tier2::samples_cpu_in(None)),
        );
        assert!(
            why.starts_with("TIER2-SKIP: SPHERECUT_SAMPLES unset"),
            "{why}"
        );
    }

    #[test]
    fn a_graphical_session_is_set_or_found_or_none() {
        use std::ffi::OsString;
        use std::os::unix::fs::MetadataExt;
        use tier2::{SessionFrom, graphical_session};
        let base = std::env::temp_dir().join(format!("spherecut-session-{}", std::process::id()));
        let guard = TempDir(base.clone());
        std::fs::create_dir_all(&base).unwrap();
        let uid = std::fs::metadata(&base).unwrap().uid();
        let env = |pairs: Vec<(&'static str, OsString)>| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
        };
        let runtime = || ("XDG_RUNTIME_DIR", base.clone().into_os_string());
        // Set, first; empty counts as unset.
        let s = graphical_session(
            env(vec![
                ("WAYLAND_DISPLAY", "wayland-1".into()),
                ("DISPLAY", ":0".into()),
            ]),
            Some(uid),
        )
        .unwrap();
        assert_eq!(s.found, SessionFrom::WaylandDisplay);
        assert_eq!(s.env, [("WAYLAND_DISPLAY", OsString::from("wayland-1"))]);
        // From a TTY: no socket, so DISPLAY, else none.
        let x = graphical_session(
            env(vec![
                ("WAYLAND_DISPLAY", "".into()),
                runtime(),
                ("DISPLAY", ":1".into()),
            ]),
            Some(uid),
        )
        .unwrap();
        assert_eq!(
            (x.found, &x.env[..]),
            (SessionFrom::X11, &[("DISPLAY", OsString::from(":1"))][..])
        );
        let none = graphical_session(env(vec![runtime()]), Some(uid)).unwrap_err();
        assert_eq!(none, tier2::NO_SESSION);
        // A lock file and a plain file named like a socket aren't one.
        std::fs::write(base.join("wayland-0.lock"), b"").unwrap();
        std::fs::write(base.join("wayland-9"), b"").unwrap();
        assert!(graphical_session(env(vec![runtime()]), Some(uid)).is_err());
        // The one socket: found, over DISPLAY.
        let _a = std::os::unix::net::UnixListener::bind(base.join("wayland-0")).unwrap();
        let found =
            graphical_session(env(vec![runtime(), ("DISPLAY", ":0".into())]), Some(uid)).unwrap();
        assert_eq!(found.found, SessionFrom::Discovered);
        assert_eq!(
            found.env,
            [("WAYLAND_DISPLAY", OsString::from("wayland-0"))]
        );
        // Another user's isn't ours.
        assert!(graphical_session(env(vec![runtime()]), Some(uid + 1)).is_err());
        // Two: which is the desktop's isn't known, so DISPLAY or none,
        // and the reason names them.
        let _b = std::os::unix::net::UnixListener::bind(base.join("wayland-1")).unwrap();
        let two = graphical_session(env(vec![runtime()]), Some(uid)).unwrap_err();
        assert!(
            two.ends_with("(or set WAYLAND_DISPLAY to one of wayland-0, wayland-1)"),
            "{two}"
        );
        drop(guard);
    }

    #[test]
    fn the_lock_is_read_from_the_users_graphical_session() {
        use tier2::locked_hint;
        let logind = |display: &'static str, hint: &'static str| {
            move |args: &[&str]| -> Option<String> {
                match args {
                    ["show-user", "1000", "--property=Display", "--value"] => {
                        Some(format!("{display}\n"))
                    }
                    ["show-session", "3", "--property=LockedHint", "--value"] => {
                        Some(format!("{hint}\n"))
                    }
                    _ => None,
                }
            }
        };
        assert_eq!(locked_hint(1000, logind("3", "yes")), Some(true));
        assert_eq!(locked_hint(1000, logind("3", "no")), Some(false));
        // Another user, no graphical session, or an answer not known.
        assert_eq!(locked_hint(1001, logind("3", "yes")), None);
        assert_eq!(locked_hint(1000, logind("", "yes")), None);
        assert_eq!(locked_hint(1000, logind("3", "maybe")), None);
        assert_eq!(locked_hint(1000, |_| None), None);
    }

    #[test]
    fn a_session_found_without_the_runtime_dir_brings_it() {
        // /run/user/<uid> is where a login puts it; a temp dir stands in
        // through XDG_RUNTIME_DIR, so only the variable's own case is
        // checked here: it isn't added when it was set.
        use std::ffi::OsString;
        let base = std::env::temp_dir().join(format!("spherecut-rt-{}", std::process::id()));
        let guard = TempDir(base.clone());
        std::fs::create_dir_all(&base).unwrap();
        let _s = std::os::unix::net::UnixListener::bind(base.join("wayland-0")).unwrap();
        let dir = base.clone().into_os_string();
        let found =
            tier2::graphical_session(move |k| (k == "XDG_RUNTIME_DIR").then(|| dir.clone()), None)
                .unwrap();
        assert_eq!(
            found.env,
            [("WAYLAND_DISPLAY", OsString::from("wayland-0"))]
        );
        drop(guard);
    }

    #[test]
    fn a_perf_clip_skips_until_hw_test_has_made_it() {
        let base = std::env::temp_dir().join(format!("spherecut-perf-{}", std::process::id()));
        let guard = TempDir(base.clone());
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("made.mp4"), b"").unwrap();
        let find = tier2::perf_clip_in;
        assert_eq!(
            find(Some(base.clone()), "made.mp4"),
            Ok(base.join("made.mp4"))
        );
        let unset = find(None, "made.mp4").unwrap_err();
        assert!(
            unset.starts_with("no perf clips (SPHERECUT_PERF_MEDIA unset)"),
            "{unset}"
        );
        let missing = find(Some(base.clone()), "not-made.mp4").unwrap_err();
        assert!(
            missing.starts_with("perf clip ") && missing.contains("not-made.mp4"),
            "{missing}"
        );
        // require_perf_clip skips with that reason.
        let skipped = std::panic::catch_unwind(|| tier2::skip(missing.clone()))
            .map_err(|e| e.downcast_ref::<String>().cloned().unwrap_or_default())
            .unwrap_err();
        assert_eq!(skipped, format!("TIER2-SKIP: {missing}"));
        drop(guard);
    }
}
