//! Tier 2c: the trailer, metadata and strings of Pat's 14 X5 files (M3
//! plan 5.5, 19.4). They need `SPHERECUT_SAMPLES` and the CPU only:
//!
//! ```text
//! SPHERECUT_SAMPLES=/home/claude/spherecut-samples/insta360-x5 \
//!   cargo test -p spherecut-camera --test tier2_samples -- --ignored
//! ```

/// On all 14 files: the index (31 slots, the records' sizes and 128 KiB
/// boundaries) and records 1, 3 and 4 (unit a); the metadata, field 27,
/// the frame counts, the four generations' strings and their equality
/// across masters and LRVs (unit b).
#[test]
#[ignore = "M3.2 units (a) and (b)"]
fn samples_insv_trailer_parses() {
    todo!()
}

/// Every calibration string in the 14 files prints back byte for byte.
#[test]
#[ignore = "M3.2 unit (b)"]
fn samples_calib_print_is_identity() {
    todo!()
}
