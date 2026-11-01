//! Tier 2c: samples-roles.toml against Pat's samples folder.
//!
//! Run in the cloud, with `SPHERECUT_TIER2` unset, by
//! `SPHERECUT_SAMPLES=/home/claude/spherecut-samples/insta360-x5 cargo test
//! -p spherecut-testkit --test samples_roles -- --ignored`; hw-test runs it
//! with the rest.

use std::ffi::OsString;
use std::path::Path;

use spherecut_testkit::control::must_fail;
use spherecut_testkit::samples::{self, Role};
use spherecut_testkit::tier2;

/// Whether `lrv` is `master`'s LRV by its name: `VID_<date>_<time>_00_NNN.insv`
/// beside `LRV_<date>_<time>_01_NNN.lrv`.
fn companions(master: &Path, lrv: &Path) -> Result<(), String> {
    let name = |p: &Path| p.file_name().map(OsString::from).unwrap_or_default();
    let (m, l) = (name(master), name(lrv));
    let (m, l) = (m.to_string_lossy(), l.to_string_lossy());
    let expected = m
        .strip_prefix("VID_")
        .and_then(|rest| rest.strip_suffix(".insv"))
        .and_then(|stem| {
            let (when, number) = stem.rsplit_once("_00_")?;
            Some(format!("LRV_{when}_01_{number}.lrv"))
        });
    match expected {
        Some(e) if e == l => Ok(()),
        Some(e) => Err(format!("{m}'s LRV would be {e}, not {l}")),
        None => Err(format!("{m} is not named as a master")),
    }
}

#[test]
#[ignore = "tier2: Pat's X5 samples (Tier 2c: SPHERECUT_SAMPLES and the CPU only)"]
fn samples_every_present_role_resolves() {
    let dir = tier2::require_samples_cpu();
    let shot: Vec<&Role> = samples::roles()
        .iter()
        .filter(|r| r.file.is_some())
        .collect();
    assert!(!shot.is_empty());
    let mut first = None;
    for r in &shot {
        let master = samples::role(r.name);
        let lrv = samples::lrv(r.name);
        assert!(master.is_file() && lrv.is_file(), "{}", r.name);
        companions(&master, &lrv).unwrap();
        eprintln!(
            "take {} ({}, {}): {}",
            r.take,
            r.name,
            r.mode.name(),
            master.display()
        );
        first.get_or_insert(master);
    }
    // Controls: a file number the folder hasn't, and a take paired with
    // another take's LRV.
    let clip8 = samples::entry("tripod_seam");
    let why = must_fail(
        "a file number Pat's folder hasn't",
        samples::master_in(
            &dir,
            &Role {
                file: Some("999"),
                ..*clip8
            },
        ),
    );
    assert!(why.contains("no file ending in _00_999.insv"), "{why}");
    let master = first.unwrap();
    let other = samples::lrv(shot[shot.len() - 1].name);
    let why = must_fail(
        "a master and another take's LRV",
        companions(&master, &other),
    );
    assert!(why.contains("'s LRV would be "), "{why}");
}
