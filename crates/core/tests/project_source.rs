//! A media's source in the project (M3 plan 11.1 and 11.2): `validate`'s
//! rules for the tune, the gains, the caps and the snapshot, the feather
//! held short of each lens's frame, the coverage rule, and the Auto
//! gains read along their track.
//!
//! M3.9's types stage names the tests; unit (a) writes them and the
//! suite's budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::OnceLock;
use std::time::Instant;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run. The tests run side by side, so none of
/// them can time the rest: the last line printed is the suite's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the suite {suite:.3} s so far",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// Every number `validate` bounds (11.2), at each end (valid) and a step
/// past it (refused, naming the media and the field), and the structural
/// rules (distinct tracks, front track 0 or 1, the geometry's generation
/// in the snapshot, the hash of the raw strings); the row count pinned so
/// no rule drops unseen.
#[test]
#[ignore = "M3.9 unit (a)"]
fn validate_at_each_bound_and_just_past() {
    let _time = timed("validate_at_each_bound_and_just_past");
    todo!()
}

/// The largest feather that validates is max(0, min(θ_max, θ_frame) −
/// 90°), each lens in turn, θ_frame from `rig::theta_frame` (11.2).
/// Control: a rule from θ_max alone passes a feather the frame cuts.
#[test]
#[ignore = "M3.9 unit (a)"]
fn the_feather_stops_short_of_the_frame() {
    let _time = timed("the_feather_stops_short_of_the_frame");
    todo!()
}

/// `Gains::at`: ones for Off, `rgb` for Fixed, and Auto's track exact at
/// its samples, linear between them, held past its ends, with g0 · g1 = 1
/// (plan 16). Control: a nearest-sample read fails the midpoint.
#[test]
#[ignore = "M3.9 unit (a)"]
fn gains_at_follows_its_track() {
    let _time = timed("gains_at_follows_its_track");
    todo!()
}

/// F038: `validate` refuses ξ + `xi_delta` ≤ −1 for each lens, the rule
/// `Rig::build` applies, and holds `xi_delta` to the range this slice
/// proposes beside the tune's others (11.2).
#[test]
#[ignore = "M3.9 unit (a)"]
fn xi_delta_keeps_xi_above_minus_one() {
    let _time = timed("xi_delta_keeps_xi_above_minus_one");
    todo!()
}

/// A tune under which some direction is covered by neither lens at the
/// project's q is refused (the coverage rule): `Rig::weights` about the
/// seam is never (0, 0) at tunes the rule passes, and is somewhere at
/// tunes it refuses. Control: the rule with the angle between the lenses'
/// axes left out passes a yawed tune whose hole the grid finds.
#[test]
#[ignore = "M3.9 unit (a)"]
fn the_coverage_rule_finds_the_holes() {
    let _time = timed("the_coverage_rule_finds_the_holes");
    todo!()
}
