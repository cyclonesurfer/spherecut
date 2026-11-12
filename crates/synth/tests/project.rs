//! A camera file's project media (M3 plan 11.4 and 11.5): media's
//! `camera_source` of synth's two-lens files, the superseded-geometry
//! check against the built-in table, and the export and viewer fixture,
//! a `set_source` and a `set_stitch` on the rung-3 file.
//!
//! M3.9's types stage names the tests; unit (b) writes them and the
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

/// `camera_source` of a synthetic X5 file gives its tracks, its camera as
/// the metadata names it, the table's geometry and the trailer's hash, and
/// a snapshot whose calibration is the trailer's, field for field, from
/// which `rig()` builds. Control: a file whose V6 string differs in one
/// digit hashes otherwise.
#[test]
#[ignore = "M3.9 unit (b)"]
fn a_camera_file_makes_a_dual_fisheye_media() {
    let _time = timed("a_camera_file_makes_a_dual_fisheye_media");
    todo!()
}

/// A two-lens file without a calibration (`InsvFault::NoCalibration`) and
/// a plain MP4 give no source. Control: the whole file gives one.
#[test]
#[ignore = "M3.9 unit (b)"]
fn a_file_without_calibration_stays_equirect() {
    let _time = timed("a_file_without_calibration_stays_equirect");
    todo!()
}

/// `superseded`: a provisional geometry is superseded by a settled entry,
/// or by a provisional one with other evidence; a settled, fitted or user
/// geometry never is (11.4). Control: equal evidence gives none.
#[test]
#[ignore = "M3.9 unit (b)"]
fn a_newer_table_entry_supersedes_a_guess() {
    let _time = timed("a_newer_table_entry_supersedes_a_guess");
    todo!()
}

/// 11.5's export and viewer fixture is `project_media` of the rung-3 file
/// with `camera_source`'s source, then a `set_source` and one
/// `set_stitch`; its rig equals the one built from the numbers it hands
/// out apart from the project. Controls: at `Tune::default()` lens B
/// doesn't move; without the `set_source` the rigs differ.
#[test]
#[ignore = "M3.9 unit (b)"]
fn the_stitch_fixture_is_a_set_source_and_a_set_stitch() {
    let _time = timed("the_stitch_fixture_is_a_set_source_and_a_set_stitch");
    todo!()
}
