//! A project's renders over time (M3 plan 11.4): only an edit changes a
//! project's geometry, so an old project renders the same bytes when a
//! newer build's table has measured its camera.
//!
//! M3.9's types stage names the test; unit (d) writes it and the suite's
//! budget.

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

/// A project of a camera file whose stored geometry is an older guess,
/// superseded by a fake newer table entry that differs where a render
/// shows it, loads unchanged and renders through its `rig()` the same
/// bytes as through `Rig::build` from the old numbers written out by hand.
/// Control: the render under the newer entry's geometry differs.
#[test]
#[ignore = "M3.9 unit (d)"]
fn an_old_project_renders_the_same_bytes() {
    let _time = timed("an_old_project_renders_the_same_bytes");
    todo!()
}
