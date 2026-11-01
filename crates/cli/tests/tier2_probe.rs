//! Tier 2c: `spherecut probe` on Pat's X5 samples, with `SPHERECUT_SAMPLES`
//! and the CPU only (M3 plan 4.4).
//!
//! - `samples_probe_names_the_camera` (M3 plan 5.4): every master and its
//!   LRV, by the CLI and by the agent's `probe`, name the X5, its firmware
//!   and serial, the master's two lens tracks, the four calibrations with
//!   V6 in use (one snapshot, one hash, in all of them), and each other as
//!   companions verified by record 4; the exposure entries are record 4's
//!   16-byte items. One master's report is printed with its serial hidden.
//!   Its control pairs a master with another take's LRV.
//!
//! Run by 4.4's command, `SPHERECUT_SAMPLES=<folder> cargo test -p
//! spherecut-cli --test tier2_probe -- --ignored`; plain `cargo test`
//! lists it as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod document;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{Value, json};
use spherecut_testkit::control::must_fail;
use spherecut_testkit::samples::{self, Role};
use spherecut_testkit::tier2;

/// `spherecut probe --json` of `files`: its exit code and document.
fn probe(files: &[&Path]) -> (Option<i32>, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["probe", "--json"])
        .args(files)
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap();
    (out.status.code(), document::document(&out.stdout))
}

/// A probe report's string at `pointer`.
fn text<'a>(r: &'a Value, pointer: &str) -> &'a str {
    r.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no string at {pointer} in {r}"))
}

/// The size record `id` has in report `r`'s `records`.
fn record_size(r: &Value, id: u64) -> u64 {
    r["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|rec| rec["id"] == id)
        .and_then(|rec| rec["size"].as_u64())
        .unwrap_or_else(|| panic!("no record {id} in {}", r["records"]))
}

#[test]
#[ignore = "tier2: Pat's X5 samples (Tier 2c: SPHERECUT_SAMPLES and the CPU only)"]
fn samples_probe_names_the_camera() {
    let started = Instant::now();
    tier2::require_samples_cpu();
    let shot: Vec<&'static Role> = samples::roles()
        .iter()
        .filter(|r| r.file.is_some())
        .collect();
    assert!(!shot.is_empty());
    // What every file must agree on: one camera, one firmware, one
    // calibration snapshot.
    let mut serial: Option<String> = None;
    let mut firmware: Option<String> = None;
    let mut hash: Option<String> = None;
    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
    for role in &shot {
        let (master, lrv) = (samples::role(role.name), samples::lrv(role.name));
        let (status, doc) = probe(&[&master, &lrv]);
        assert_eq!(status, Some(0), "{doc}");
        let files = &doc["result"]["files"];
        let (m, l) = (&files[0]["report"], &files[1]["report"]);
        for (r, other) in [(m, &lrv), (l, &master)] {
            let path = text(r, "/path");
            assert_eq!(text(r, "/camera/vendor"), "Insta360", "{path}");
            assert_eq!(text(r, "/camera/model"), "Insta360 X5", "{path}");
            let s = text(r, "/camera/serial");
            assert!(!s.is_empty(), "{path}");
            assert_eq!(serial.get_or_insert_with(|| s.to_owned()), s, "{path}");
            let f = text(r, "/camera/firmware");
            assert_eq!(firmware.get_or_insert_with(|| f.to_owned()), f, "{path}");
            assert_eq!(
                r["companion"],
                json!({"path": other.to_str().unwrap(), "verified": true, "why": null}),
                "{path}"
            );
            let c = &r["calibration"];
            assert_eq!(c["generations"], json!(["v1", "v2", "v3", "v6"]), "{path}");
            assert_eq!(c["in_use"], "v6", "{path}");
            let h = text(c, "/hash");
            assert_eq!(h.len(), 16, "{path}");
            assert_eq!(hash.get_or_insert_with(|| h.to_owned()), h, "{path}");
            assert_eq!(
                r["exposure_entries"].as_u64().unwrap() * 16,
                record_size(r, 4),
                "{path}"
            );
            // The metadata and the IMU; the thumbnail (2) is in the
            // trailer of some takes only, beside a `.thumbnail.bin` in
            // others.
            for id in [1, 3] {
                record_size(r, id);
            }
            assert_eq!(r["warnings"], json!([]), "{path}");
        }
        assert_eq!(m["camera"]["layout"], "insta360_dual_fisheye");
        assert_eq!(l["camera"]["layout"], "low_res_companion");
        assert_eq!(m["lenses"], json!({"tracks": [0, 1]}));
        assert_eq!(l["lenses"], Value::Null, "the LRV's one picture");
        // The master's strings are its LRV's, so the snapshot is one.
        assert_eq!(m["calibration"], l["calibration"]);
        eprintln!(
            "{} ({}): {} {}, {}; calibration {} ({} in use, canvas {}, window {}), hash {}; \
             {} exposure entries; records {}",
            master.file_name().unwrap().to_string_lossy(),
            role.name,
            text(m, "/camera/model"),
            text(m, "/camera/firmware"),
            m["lenses"],
            m["calibration"]["generations"],
            text(m, "/calibration/in_use"),
            m["calibration"]["canvas"],
            m["calibration"]["window"],
            text(m, "/calibration/hash"),
            m["exposure_entries"],
            m["records"],
        );
        if pairs.is_empty() {
            let shown = serde_json::to_string_pretty(m)
                .unwrap()
                .replace(serial.as_deref().unwrap(), "<serial>");
            eprintln!("{shown}");
        }
        pairs.push((master, lrv));
    }

    // The agent's summary of the first pair, from a read root: `camera`
    // a name, `camera_detail` the CLI's camera, the companion verified
    // as the agent opens it beneath the root (D45).
    let (master, lrv) = &pairs[0];
    let root = master.parent().unwrap();
    let names = [master, lrv].map(|p| p.file_name().unwrap().to_str().unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args([
            "call",
            "probe",
            &json!({ "files": names }).to_string(),
            "--read-root",
        ])
        .arg(root)
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap();
    let doc = document::document(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{doc}");
    let summary = &doc["result"]["files"];
    for (s, other) in [(&summary[0], names[1]), (&summary[1], names[0])] {
        assert_eq!(s["camera"], "Insta360 X5", "{s}");
        assert_eq!(
            s["camera_detail"],
            json!({
                "vendor": "Insta360",
                "model": "Insta360 X5",
                "firmware": firmware.as_deref().unwrap(),
                "serial": serial.as_deref().unwrap(),
            })
        );
        assert_eq!(
            s["companion"],
            json!({"path": other, "verified": true, "why": null})
        );
        assert_eq!(s["calibration"]["in_use"], "v6");
    }

    // The control: a master beside another take's LRV, under its own
    // LRV's name, is paired by name only, since record 4 differs.
    if let [(master, _), (_, wrong), ..] = &pairs[..] {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("tier2-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = master.file_name().unwrap().to_str().unwrap();
        let m = dir.join(name);
        let l = dir.join(
            name.replace("VID_", "LRV_")
                .replace("_00_", "_01_")
                .replace(".insv", ".lrv"),
        );
        std::os::unix::fs::symlink(master, &m).unwrap();
        std::os::unix::fs::symlink(wrong, &l).unwrap();
        let (_, doc) = probe(&[&m]);
        let c = &doc["result"]["files"][0]["report"]["companion"];
        must_fail("another take's LRV, verified", c["verified"] == true);
        assert!(
            c["why"].as_str().is_some_and(|w| w.contains("record")),
            "{c}"
        );
        eprintln!("the control's companion: {c}");
        std::fs::remove_dir_all(&dir).ok();
    }
    eprintln!(
        "samples_probe_names_the_camera: {} takes in {:.1} s",
        pairs.len(),
        started.elapsed().as_secs_f64()
    );
}
