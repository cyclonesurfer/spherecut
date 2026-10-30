//! `spherecut doctor` on a machine without a GPU, made so even where there
//! is one: VA-API looks at a render node that doesn't exist, and the Vulkan
//! loader sees only lavapipe (llvmpipe), which must be flagged as a CPU
//! device with no GPU acceleration.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

mod common;
mod document;
mod lavapipe;

use lavapipe::lavapipe_icd;

fn missing_node() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("no-such-dri")
        .join("renderD128")
}

/// `spherecut doctor` with `args`, on lavapipe and no render node.
fn doctor(args: &[&str]) -> Output {
    run_doctor(Command::new(env!("CARGO_BIN_EXE_spherecut")), args)
}

/// [`doctor`], run by `cmd`, which runs the spherecut binary with what
/// is appended to it.
fn run_doctor(mut cmd: Command, args: &[&str]) -> Output {
    let icd = lavapipe_icd();
    let out = cmd
        .arg("doctor")
        .arg("--render-node")
        .arg(missing_node())
        .args(args)
        // VK_DRIVER_FILES for current loaders, VK_ICD_FILENAMES for older.
        .env("VK_DRIVER_FILES", &icd)
        .env("VK_ICD_FILENAMES", &icd)
        .env_remove("SPHERECUT_LOG")
        .env_remove("SPHERECUT_ZERO_COPY")
        .output()
        .unwrap();
    // Warnings only: a missing GPU slows things down but opens everything.
    assert!(
        out.status.success(),
        "doctor failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn json_flags_lavapipe_as_cpu_and_the_render_node_as_missing() {
    let out = doctor(&["--json"]);
    let v = document::document(&out.stdout);
    // Warnings only, so the run is ok and the report comes without an
    // error.
    assert_eq!(v["schema"], "spherecut.doctor/1");
    assert_eq!(v["ok"], true);
    assert!(v.get("error").is_none(), "{v}");
    let r: &Value = &v["result"];

    // The keys doctor has always had are still there.
    for k in [
        "distro",
        "ffmpeg_version",
        "libraries",
        "hw_device_types",
        "decoders",
        "encoders",
        "devices",
        "issues",
    ] {
        assert!(r.get(k).is_some(), "no {k} in {r:#}");
    }

    let vk = r["vulkan_devices"].as_array().unwrap();
    assert_eq!(vk.len(), 1, "{vk:#?}");
    assert_eq!(vk[0]["device_type"], "cpu");
    assert_eq!(vk[0]["driver"], "llvmpipe");
    assert_eq!(vk[0]["used_by_ffmpeg"], true);
    assert!(vk[0]["name"].as_str().unwrap().starts_with("llvmpipe"));
    // lavapipe has no video queue, so path B can't run on it, and doctor
    // says why as the renderer does when it opens the device.
    assert_eq!(vk[0]["shared_decode"], false);
    let why = vk[0]["shared_decode_reason"].as_str().unwrap();
    assert_eq!(why, "the device lacks VK_KHR_video_queue");
    let gpu =
        spherecut_render::Gpu::headless_shared(&spherecut_render::AdapterChoice::Cpu).unwrap();
    assert_eq!(gpu.shared_video().unwrap_err(), why);
    assert_eq!(r["zero_copy"], false);

    let issues = r["issues"].as_array().unwrap();
    let what = |prefix: &str| {
        issues
            .iter()
            .find(|i| i["what"].as_str().unwrap().starts_with(prefix))
            .unwrap_or_else(|| panic!("no issue \"{prefix}…\" in {issues:#?}"))
    };
    let cpu = what("the only Vulkan device is llvmpipe");
    assert!(
        cpu["what"]
            .as_str()
            .unwrap()
            .ends_with("no GPU acceleration")
    );
    assert_eq!(cpu["severity"], "warning");
    assert!(!cpu["fix"].as_str().unwrap().is_empty());
    let node = what("no DRM render node at");
    assert_eq!(node["severity"], "warning");

    // Every format decodes in software; nothing is tested on VA-API.
    for c in r["decode_checks"].as_array().unwrap() {
        match c["path"].as_str().unwrap() {
            "software" => assert_eq!(c["result"], "works", "{c:#}"),
            "vaapi" => assert_eq!(c["result"], "not_tested", "{c:#}"),
            _ => assert_eq!(c["result"], "fails", "{c:#}"),
        }
    }
}

#[test]
fn text_lists_the_vulkan_device_and_the_check_tables() {
    let out = doctor(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    for want in [
        "\nVulkan devices\n  llvmpipe",
        "CPU: no GPU acceleration; driver llvmpipe",
        "    zero copy: DMA-BUF import ",
        "; shared with FFmpeg's Vulkan Video no: the device lacks VK_KHR_video_queue\n",
        "\nZero copy: off, so every frame is copied through RAM (SPHERECUT_ZERO_COPY=1 turns it on)\n",
        "\nDecode check: one 256x256 frame each",
        "  HEVC Main 10   works",
        "\nEncode check: one 256x256 frame each on VA-API",
        "[warning] the only Vulkan device is llvmpipe",
    ] {
        assert!(text.contains(want), "no {want:?} in:\n{text}");
    }
}

#[test]
fn zero_copy_is_on_as_the_viewer_reads_the_variable() {
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_spherecut"))
            .arg("doctor")
            .arg("--render-node")
            .arg(missing_node())
            .args(args)
            .env("VK_DRIVER_FILES", lavapipe_icd())
            .env("VK_ICD_FILENAMES", lavapipe_icd())
            .env("SPHERECUT_ZERO_COPY", " 1\n")
            .output()
            .unwrap()
    };
    let r = &document::document(&run(&["--json"]).stdout)["result"];
    assert_eq!(r["zero_copy"], true);
    let text = String::from_utf8_lossy(&run(&[]).stdout).into_owned();
    assert!(
        text.contains("\nZero copy: on (SPHERECUT_ZERO_COPY=1): VA-API frames are imported"),
        "{text}"
    );
}

#[test]
fn doctor_opens_no_network_socket() {
    let log = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("doctor-strace-{}.log", std::process::id()));
    let mut cmd = common::strace(&log, "socket,openat,?open");
    cmd.arg(env!("CARGO_BIN_EXE_spherecut"));
    run_doctor(cmd, &[]);
    let log_text = std::fs::read_to_string(&log).unwrap();
    std::fs::remove_file(&log).ok();
    // The trace saw doctor load the Vulkan driver it tests ...
    assert!(
        log_text
            .lines()
            .any(|l| l.contains("open") && l.contains("lvp_icd")),
        "strace saw no open of lavapipe's ICD:\n{log_text}"
    );
    // ... and no network socket.
    let sockets = common::inet_sockets(&log_text);
    assert!(sockets.is_empty(), "{sockets:#?}");
}
