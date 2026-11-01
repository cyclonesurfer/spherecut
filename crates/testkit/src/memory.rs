//! How much memory a test's code takes: how far the process's peak
//! resident set rises while it runs (M3 plan 4.3's K3).
//!
//! Linux keeps the peak as `VmHWM` in `/proc/self/status`, and sets it
//! back to the present resident set when "5" is written to
//! `/proc/self/clear_refs`. The peak is the whole process's, so tests
//! running beside the one measured count too, and memory an earlier run
//! freed but the allocator kept hides the growth of the run after it. A
//! run measured on its own is measured in a child process of its own.

use std::path::Path;

/// The file through which this process's peak resident set is reset.
pub const CLEAR_REFS: &str = "/proc/self/clear_refs";

/// Reset this process's peak resident memory through `clear_refs`
/// ([`CLEAR_REFS`]), run `f`, and return how far the peak rose above the
/// starting level, in KiB. `f` runs in any case; the result is None where
/// the peak cannot be reset or read.
pub fn peak_rss_growth_kib(clear_refs: &Path, f: impl FnOnce()) -> Option<u64> {
    let before = std::fs::write(clear_refs, "5")
        .ok()
        .and_then(|()| peak_rss_kib());
    f();
    Some(peak_rss_kib()?.saturating_sub(before?))
}

/// This process's peak resident set since it started or was last reset,
/// `VmHWM`, in KiB; None where `/proc/self/status` can't be read.
pub fn peak_rss_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{at_least, must_fail};

    #[test]
    fn memory_growth_runs_its_closure_when_the_peak_cannot_be_reset() {
        let mut ran = false;
        let grew = peak_rss_growth_kib(Path::new("/nonexistent/clear_refs"), || ran = true);
        assert!(ran);
        assert_eq!(grew, None);
    }

    /// 64 MiB, written to so that every page is resident.
    fn touch_64_mib() {
        let v = vec![1u8; 64 << 20];
        assert_eq!(std::hint::black_box(&v)[v.len() - 1], 1);
    }

    #[test]
    fn memory_a_closure_touches_raises_the_peak() {
        let grew = peak_rss_growth_kib(Path::new(CLEAR_REFS), touch_64_mib);
        let Some(kib) = grew else {
            // A kernel without clear_refs, or a /proc that hides it:
            // nothing here can be measured, as the closure's run says.
            eprintln!("the peak can't be reset here: {CLEAR_REFS} isn't writable");
            return;
        };
        let held = at_least(kib, 60 << 10);
        assert_eq!(held.passes(), Some(true), "{held} KiB");
        // The control: a closure that touches nothing doesn't look like
        // one that does. The tests beside this one allocate far less.
        let idle = peak_rss_growth_kib(Path::new(CLEAR_REFS), || {}).unwrap();
        must_fail("a closure that touches nothing", at_least(idle, 60 << 10));
    }
}
