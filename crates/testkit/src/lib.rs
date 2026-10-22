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
}
