//! `spherecut doctor` on this machine.
//!
//! Tier 1 runs anywhere: it needs a full FFmpeg (as all of Tier 1 does)
//! and points VA-API at a render node that doesn't exist. The Tier 2 test
//! needs a GPU and requires VA-API to decode HEVC Main, so a broken or
//! codec-stripped driver fails `cargo xtask hw-test` with doctor's fix.
//!
//! Doctor's checks are run once and shared: they change FFmpeg's global
//! log level while they run, and are slow enough not to repeat.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use spherecut_media::doctor::{self, Check, CheckResult, DoctorReport, SOFTWARE, Severity};
use spherecut_testkit::tier2;

/// A render node path that exists nowhere.
fn missing_node() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("no-such-dri")
        .join("renderD128")
}

fn report() -> &'static DoctorReport {
    static R: OnceLock<DoctorReport> = OnceLock::new();
    R.get_or_init(|| doctor::run(&missing_node()))
}

fn checks<'a>(r: &'a DoctorReport, path: &'a str) -> impl Iterator<Item = &'a Check> {
    r.decode_checks.iter().filter(move |c| c.path == path)
}

#[test]
fn every_embedded_format_decodes_in_software() {
    let r = report();
    let sw: Vec<_> = checks(r, SOFTWARE).collect();
    let formats: Vec<_> = sw.iter().map(|c| c.format.as_str()).collect();
    assert_eq!(
        formats,
        ["H.264 High", "HEVC Main", "HEVC Main 10", "AV1 Main"]
    );
    // Size and pixel format included: the HEVC Main 10 frame is 10-bit.
    for c in sw {
        assert_eq!(c.result, CheckResult::Works, "{c:#?}");
    }
    assert!(
        r.issues.iter().all(|i| i.severity != Severity::Error),
        "{:#?}",
        r.issues
    );
}

#[test]
fn a_missing_render_node_is_reported_and_vaapi_not_tested() {
    let r = report();
    let node = missing_node().to_string_lossy().into_owned();
    let dev = r.devices.iter().find(|d| d.kind == "vaapi").unwrap();
    assert!(!dev.ok);
    assert_eq!(dev.device.as_deref(), Some(node.as_str()));
    if r.hw_device_types.iter().any(|t| t == "vaapi") {
        // FFmpeg's own explanation, captured although nothing subscribes
        // to its log here.
        assert!(
            dev.ffmpeg_log
                .iter()
                .any(|l| l.contains("No VA display found")),
            "{dev:#?}"
        );
    }
    let vaapi: Vec<_> = checks(r, "vaapi").chain(&r.encode_checks).collect();
    assert_eq!(vaapi.len(), 8);
    for c in vaapi {
        assert_eq!(c.result, CheckResult::NotTested, "{c:#?}");
        assert!(c.reason.as_deref().unwrap().starts_with("no VA-API device"));
    }
    let i = r
        .issues
        .iter()
        .find(|i| i.what == format!("no DRM render node at {node}"))
        .unwrap_or_else(|| panic!("{:#?}", r.issues));
    assert_eq!(i.severity, Severity::Warning);
}

/// The checks on `path` and the issues, for a failure message.
fn summary(r: &DoctorReport, path: &str) -> String {
    let mut s = String::new();
    for c in r.decode_checks.iter().chain(&r.encode_checks) {
        if c.path == path {
            s += &format!(
                "\n  {} ({}): {:?} {}",
                c.format,
                c.codec,
                c.result,
                c.reason.as_deref().unwrap_or("")
            );
            for l in &c.ffmpeg_log {
                s += &format!("\n      FFmpeg: {l}");
            }
        }
    }
    for i in &r.issues {
        s += &format!("\n  [{:?}] {}\n      fix: {}", i.severity, i.what, i.fix);
    }
    s
}

#[test]
#[ignore = "tier2: needs a VA-API GPU with HEVC Main"]
fn doctor_decodes_hevc_main_on_vaapi() {
    let node = tier2::require_render_node();
    let r = doctor::run(&node);
    let dev = r.devices.iter().find(|d| d.kind == "vaapi").unwrap();
    assert!(
        dev.ok,
        "no VA-API device on {}: {:?} {:?}{}",
        node.display(),
        dev.error,
        dev.ffmpeg_log,
        summary(&r, "vaapi")
    );
    let c = checks(&r, "vaapi")
        .find(|c| c.format == "HEVC Main")
        .unwrap();
    assert_eq!(
        c.result,
        CheckResult::Works,
        "VA-API cannot decode HEVC Main on {}:{}",
        node.display(),
        summary(&r, "vaapi")
    );
    // Reported, not required: what else this GPU does.
    eprintln!("doctor on {}:{}", node.display(), summary(&r, "vaapi"));
}
