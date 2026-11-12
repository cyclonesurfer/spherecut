//! `synth::insv::write_insv`'s files (M3 plan 7.2; M3.5's unit (a)): a
//! master and its LRV as the X5 writes them, the planes a lossless file
//! decodes to, each `InsvFault` planted where it belongs, and Pat's
//! strings printed unchanged (the project review's F141, amendment 7).
//!
//! M3.5's types stage names the tests; unit (a) writes them and the
//! suite's budget.

/// The master is the X5's container (3.1, 7.2): two square `hvc1`
/// tracks coded as the X5 codes them (full range, BT.709, chroma left,
/// the mode's GOP or the spec's `keyint`, no B-frames), one silent AAC
/// track, `ftyp wide mdat moov`, and the `inst` box holding synth's
/// trailer where `InsvFiles::layout` says, which camera's reader reads.
#[test]
#[ignore = "M3.5 unit (a)"]
fn write_insv_writes_the_x5s_container() {
    todo!()
}

/// A `Quality::Lossless` file's decoded planes hash equal to
/// `InsvSpec::planes`, track by track and frame by frame, corner codes
/// and all (4.2's rung 3).
#[test]
#[ignore = "M3.5 unit (a)"]
fn write_insv_lossless_planes_hash_equal() {
    todo!()
}

/// Each of `InsvFault`'s MP4 faults (`LensOneFrameShort`,
/// `StartPtsDiffer`, `StssDiffer`, `RateDiffer`) is in the tracks as
/// its name says, as `track_timing` reads them, and nothing else
/// differs from the unfaulted file's.
#[test]
#[ignore = "M3.5 unit (a)"]
fn write_insv_plants_each_mp4_fault() {
    todo!()
}

/// Each trailer fault (`TruncatedTrailer` to `VarintOverflow`, and
/// `NoCalibration`) is planted in the master's trailer, the tracks left
/// as the unfaulted file's, and camera's reader gives a clear error
/// naming the record (5.5).
#[test]
#[ignore = "M3.5 unit (a)"]
fn write_insv_plants_each_trailer_fault() {
    todo!()
}

/// The LRV follows the X5's rules (3.1): one H.264 track of 2 · side ×
/// side with track 1 on the left, 25 frames a second for a 25 fps master
/// and 29.97 otherwise, the master's frame count at 25 and 29.97, half
/// at 59.94 and a quarter, rounded up, at 119.88, named
/// `LRV_…_01_NNN.lrv` beside the master, with `Records::x5_lrv`'s
/// records, so that `verify_companion` pairs the two.
#[test]
#[ignore = "M3.5 unit (a)"]
fn write_insv_lrv_follows_the_x5s_rules() {
    todo!()
}

/// The strings in a written file's metadata are `print`'s of the stored
/// calibration, byte for byte whatever is planted and whatever the
/// tracks' size: Pat's truths (`spherecut_boundary::pats`) give
/// `testkit::pats`'s strings (F141).
#[test]
#[ignore = "M3.5 unit (a)"]
fn write_insv_prints_pats_strings_unchanged() {
    todo!()
}
