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
        let dir = std::env::var_os("SPHERECUT_SAMPLES")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("samples"));
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
    pub fn measure(name: &str, unit: &str, mut f: impl FnMut() -> f64) {
        f();
        let values: Vec<f64> = (0..runs()).map(|_| f()).collect();
        record_runs(name, &values, unit);
    }

    /// Record one measurement, e.g. `record("decode 3840x1920 hevc8 vaapi", 212.0, "fps")`.
    pub fn record(name: &str, value: f64, unit: &str) {
        record_runs(name, &[value], unit);
    }

    /// Record several runs of one benchmark as a line of
    /// `name, median, unit, min, max, runs`, tab-separated.
    pub fn record_runs(name: &str, values: &[f64], unit: &str) {
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let (Some(min), Some(max)) = (v.first(), v.last()) else {
            return;
        };
        let n = v.len();
        let median = if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        };
        let line = format!("{name}\t{median:.2}\t{unit}\t{min:.2}\t{max:.2}\t{n}\n");
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
}

#[cfg(test)]
mod tests {
    use super::{bench, tier2};

    #[test]
    fn skip_panics_with_marker() {
        let r = std::panic::catch_unwind(|| tier2::skip("no GPU"));
        let msg = r
            .err()
            .and_then(|e| e.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        assert_eq!(msg, "TIER2-SKIP: no GPU");
    }

    #[test]
    fn runs_are_recorded_as_median_and_range() {
        let out = std::env::temp_dir().join(format!("spherecut-bench-{}.tsv", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // SAFETY: no other test in this crate reads or writes these
        // variables.
        unsafe {
            std::env::set_var("SPHERECUT_BENCH_OUT", &out);
            std::env::set_var("SPHERECUT_BENCH_RUNS", "4");
        }
        let mut calls = 0;
        bench::measure("m", "fps", || {
            calls += 1;
            [0.0, 4.0, 1.0, 3.0, 2.0][calls - 1]
        });
        bench::record("one", 5.0, "fps");
        // The warm-up's 0.0 is left out.
        assert_eq!(calls, 5);
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "m\t2.50\tfps\t1.00\t4.00\t4\none\t5.00\tfps\t5.00\t5.00\t1\n"
        );
        let _ = std::fs::remove_file(&out);
    }
}
