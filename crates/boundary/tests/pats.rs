//! The boundary crate's own checks (the project review's F137): Pat's
//! truths, parsed from `testkit::pats`, print as the strings they were
//! parsed from, byte for byte.
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
use spherecut_synth::lens::{self as truth, x5_like_planted};
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

/// `s` with token `at`'s last digit changed, 1 to 2, 9 to 0.
fn one_digit_off(s: &str, at: usize) -> String {
    let mut tokens: Vec<String> = s.split('_').map(str::to_owned).collect();
    let token = &mut tokens[at];
    let last = token.pop().unwrap();
    let digit = last.to_digit(10).unwrap();
    token.push(char::from_digit((digit + 1) % 10, 10).unwrap());
    tokens.join("_")
}

/// `pats(g)` printed through synth's `print` is `testkit::pats`'s string
/// of g, byte for byte: the parse lost and moved nothing, and synth's
/// printer writes Pat's strings unchanged. The control changes one token,
/// block 0's first term, in its last digit, and the printed string no
/// longer equals it.
#[test]
fn pats_print_as_the_strings_they_parse() {
    let _t = timed("pats_print_as_the_strings_they_parse");
    let planted = x5_like_planted();
    for g in truth::Generation::all() {
        let printed = print(&pats(g), &planted);
        assert_eq!(printed, string(g), "{g:?}");
        // Token 12 is block 0's k1 in V3 and d0 in V6.
        let off = one_digit_off(string(g), 12);
        assert_ne!(off, string(g));
        must_fail(&format!("{g:?} with one digit off"), printed == off);
    }
}
