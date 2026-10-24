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
        usable_samples_dir(
            std::env::var_os("SPHERECUT_SAMPLES")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("samples")),
        )
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
    use super::tier2;

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
}
