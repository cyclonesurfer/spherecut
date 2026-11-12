//! The boundary crate's own checks (the project review's F137): Pat's
//! truths, parsed from `testkit::pats`, print as the strings they were
//! parsed from, byte for byte, and a truth a digit off doesn't.
//!
//! Before the literal calibrations in synth's `tests/lens_against_sphere.rs`
//! and render's `tests/common/rig.rs` were removed, a test held [`pats`]
//! to them, number for number, and passed: the truths every stitching test
//! reads didn't change when they moved here.
//!
//! Budget (M3 plan 4.1 rule 5): 1 s for the suite, which each test prints
//! its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use spherecut_boundary::pats;
use spherecut_synth::insv::print;
use spherecut_synth::lens::{self as truth, CalibTruth, StoredDist, x5_like_planted};
use spherecut_testkit::control::must_fail;
use spherecut_testkit::pats as testkit_pats;

/// The suite's budget in seconds.
const BUDGET_S: f64 = 1.0;

/// A test's clock, which prints its time against the suite's budget as
/// the test ends.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        eprintln!(
            "{}: {:.3} s, of the suite's {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// The string `pats(g)` is parsed from.
fn string(generation: truth::Generation) -> &'static str {
    match generation {
        truth::Generation::V3 => testkit_pats::V3,
        truth::Generation::V6 => testkit_pats::V6,
    }
}

/// Whether `cal` printed through synth's `print` is generation g's
/// string, or the tokens at which it isn't.
fn prints_as(cal: &CalibTruth, g: truth::Generation) -> Result<(), Vec<usize>> {
    let printed = print(cal, &x5_like_planted());
    if printed == string(g) {
        return Ok(());
    }
    let (a, b): (Vec<&str>, Vec<&str>) =
        (printed.split('_').collect(), string(g).split('_').collect());
    Err((0..a.len().max(b.len()))
        .filter(|&k| a.get(k) != b.get(k))
        .collect())
}

/// `pats(g)` printed through synth's `print` is `testkit::pats`'s string
/// of g, byte for byte: the parse lost and moved nothing, and synth's
/// printer writes Pat's strings unchanged. The control prints `pats(g)`
/// with block 0's first term (V3's k1, V6's d0) one unit off in its last
/// printed digit, its 8th decimal: the same check fails, at that token
/// alone, as the printer reads the term.
#[test]
fn pats_print_as_the_strings_they_parse() {
    let _t = timed("pats_print_as_the_strings_they_parse");
    for g in truth::Generation::all() {
        assert_eq!(prints_as(&pats(g), g), Ok(()), "{g:?}");
        let mut off = pats(g);
        match &mut off.dist {
            StoredDist::V3 { k, .. } => k[0][0] += 1e-8,
            StoredDist::V6 { d } => d[0][0] += 1e-8,
        }
        let moved = must_fail(
            &format!("{g:?} with block 0's first term a digit off"),
            prints_as(&off, g),
        );
        // Token 12 is block 0's k1 in V3 and d0 in V6.
        assert_eq!(moved, [12], "{g:?}");
    }
}
