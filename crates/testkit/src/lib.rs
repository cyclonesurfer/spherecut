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

    /// A DRM render node for VA-API, or skip.
    #[track_caller]
    pub fn require_render_node() -> PathBuf {
        require_hw_run();
        let p = std::env::var_os("SPHERECUT_RENDER_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/dev/dri/renderD128"));
        if !p.exists() {
            skip(format!("no DRM render node at {}", p.display()));
        }
        p
    }
}

/// Benchmark results from Tier 2 tests. `cargo xtask hw-test` sets
/// `SPHERECUT_BENCH_OUT` to a file and puts every recorded line into the
/// report's benchmark table; elsewhere results go to stderr.
pub mod bench {
    use std::io::Write;

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
    pub fn measure(name: &str, unit: &str, f: impl FnMut() -> f64) {
        record_runs(name, &warm_then_run(runs(), f), unit);
    }

    /// `f`'s results after one warm-up call, `runs` of them.
    fn warm_then_run(runs: usize, mut f: impl FnMut() -> f64) -> Vec<f64> {
        f();
        (0..runs).map(|_| f()).collect()
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
        let Some(line) = line(name, values, unit) else {
            return;
        };
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
                [0.0, 4.0, 1.0, 3.0, 2.0][calls - 1]
            });
            // The warm-up's 0.0 is left out.
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
