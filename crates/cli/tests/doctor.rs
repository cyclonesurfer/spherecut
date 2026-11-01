//! `spherecut doctor` on a machine without a GPU, made so even where there
//! is one: VA-API looks at a render node that doesn't exist, and the Vulkan
//! loader sees only lavapipe (llvmpipe), which must be flagged as a CPU
//! device with no GPU acceleration.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

mod common;
mod document;
#[allow(dead_code)]
mod insv;
mod lavapipe;

use lavapipe::lavapipe_icd;
use spherecut_synth::insv::InsvFault;
use spherecut_testkit::control::must_fail;

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

    // Without VA-API, export uses the software encoders this FFmpeg has.
    let has = |name: &str| {
        r["encoders"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == name && e["available"] == true)
    };
    let software = |name: &str| if has(name) { json!(name) } else { Value::Null };
    assert_eq!(
        r["export_encoders"],
        json!([
            {"format": "H.264", "encoder": software("libx264")},
            {"format": "HEVC Main", "encoder": software("libx265")},
            {"format": "HEVC Main 10", "encoder": software("libx265")},
            {"format": "AV1", "encoder": software("libsvtav1")},
        ])
    );

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
        "\nZero copy: Vulkan Video's frames, decoded on the renderer's own device where it can be \
         shared; VA-API's and the rest are copied through RAM (SPHERECUT_ZERO_COPY=1 imports \
         VA-API's too, 0 copies every frame)\n",
        "\nDecode check: one 256x256 frame each",
        "  HEVC Main 10   works",
        "\nEncode check: one 256x256 frame each on VA-API",
        "[warning] the only Vulkan device is llvmpipe",
    ] {
        assert!(text.contains(want), "no {want:?} in:\n{text}");
    }
}

#[test]
fn zero_copy_is_as_the_viewer_reads_the_variable() {
    let run = |value: Option<&str>, args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
        cmd.arg("doctor")
            .arg("--render-node")
            .arg(missing_node())
            .args(args)
            .env("VK_DRIVER_FILES", lavapipe_icd())
            .env("VK_ICD_FILENAMES", lavapipe_icd())
            .env_remove("SPHERECUT_ZERO_COPY");
        if let Some(v) = value {
            cmd.env("SPHERECUT_ZERO_COPY", v);
        }
        cmd.output().unwrap()
    };
    let vulkan = "Vulkan Video's frames";
    for (value, on, mode, says) in [
        (None, true, "vulkan", vulkan),
        (Some("yes"), true, "vulkan", vulkan),
        (Some(" 1\n"), true, "all", "on (SPHERECUT_ZERO_COPY=1)"),
        (Some("0"), false, "off", "off (SPHERECUT_ZERO_COPY=0)"),
    ] {
        let r = &document::document(&run(value, &["--json"]).stdout)["result"];
        assert_eq!(r["zero_copy"], on, "{value:?}");
        assert_eq!(r["zero_copy_mode"], mode, "{value:?}");
        let text = String::from_utf8_lossy(&run(value, &[]).stdout).into_owned();
        let says = format!("\nZero copy: {says}");
        assert!(text.contains(&says), "{value:?}: {text}");
    }
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

/// `doctor --file` on a synthetic `.insv`: whether it can be stitched
/// (its calibration is there), whether both lenses decode on the chosen
/// path, and what is missing when not ("Insta360 X5 v1.1.22, geometry
/// provisional"); on one with `NoCalibration`, that it can't (M3 plan
/// 5.4).
#[test]
fn doctor_file_says_whether_an_insv_can_be_stitched() {
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("doctor-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let master = insv::master(&dir, insv::MASTER, None);
    let lrv = insv::lrv(&dir, insv::LRV, insv::FRAMES);
    let none = insv::master(
        &dir,
        "VID_20261022_101500_00_002.insv",
        Some(InsvFault::NoCalibration),
    );
    let text = |path: &Path| {
        let out = doctor(&["--file", path.to_str().unwrap()]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // The master: its camera and firmware, both lenses decoded on the
    // path doctor chose (software, with no render node, saying why not
    // VA-API), and its V6.
    let t = text(&master.path);
    let software = format!(
        "decodes on software, not on vaapi: no VA-API device: no render node at {}",
        missing_node().display()
    );
    let want = format!(
        "\nFile {}\n  Insta360 X5 v1.1.22, geometry provisional\n  can be stitched: yes, \
         with its v6 calibration\n  stream #0  h264 128x128 yuv420p, {software}\n  \
         stream #1  h264 128x128 yuv420p, {software}\n",
        master.path.display()
    );
    assert!(t.ends_with(&want), "no {want:?} at the end of:\n{t}");
    // The machine's report comes first, as it does without --file.
    assert!(t.contains("\nDecode check: one 256x256 frame each"), "{t}");

    // The controls: a master without calibration strings, and an LRV
    // with one picture of both lenses, can't be stitched, and doctor
    // says what is missing.
    let t = text(&none.path);
    must_fail(
        "a master with no calibration, stitchable",
        t.contains("can be stitched: yes"),
    );
    for want in [
        "  can be stitched: no\n".to_string(),
        "  missing: a calibration: the trailer holds none that parses\n".into(),
        format!("  stream #1  h264 128x128 yuv420p, {software}\n"),
    ] {
        assert!(t.contains(&want), "no {want:?} in:\n{t}");
    }
    let t = text(&lrv);
    must_fail("an LRV, stitchable", t.contains("can be stitched: yes"));
    for want in [
        "  Insta360 X5 v1.1.22, geometry provisional\n  can be stitched: no\n".to_string(),
        format!("  stream #0  h264 256x128 yuv420p, {software}\n"),
        "  missing: two lens streams: an LRV holds both lenses in one picture, so stitch its \
         master\n"
            .into(),
    ] {
        assert!(t.contains(&want), "no {want:?} in:\n{t}");
    }
    assert!(!t.contains("stream #1"), "{t}");

    // JSON: the machine's report as it was, and `file`.
    let v =
        document::document(&doctor(&["--json", "--file", master.path.to_str().unwrap()]).stdout);
    assert_eq!(v["ok"], true, "{v}");
    let r = &v["result"];
    for k in ["distro", "decoders", "vulkan_devices", "issues"] {
        assert!(r.get(k).is_some(), "no {k} in {r:#}");
    }
    let f = &r["file"];
    assert_eq!(f["path"], master.path.to_str().unwrap());
    assert_eq!(f["camera"], "Insta360 X5");
    assert_eq!(f["firmware"], "v1.1.22_build1");
    assert_eq!(f["stitchable"], true, "{f:#}");
    assert_eq!(f["calibration"], "v6");
    assert_eq!(f["geometry"], "provisional");
    assert_eq!(f["missing"], json!([]));
    assert_eq!(f["summary"], "Insta360 X5 v1.1.22, geometry provisional");
    // What decoding warned of is the file's; doctor's envelope has none.
    assert_eq!(f["warnings"], json!([]), "{f:#}");
    assert_eq!(v["warnings"], json!([]));
    let lenses = f["lenses"].as_array().unwrap();
    assert_eq!(lenses.len(), 2, "{f:#}");
    for (n, l) in lenses.iter().enumerate() {
        assert_eq!(l["stream"], n);
        assert_eq!(l["decodes"], true, "{l:#}");
        assert_eq!(l["decode"]["path"], "software", "{l:#}");
        assert_eq!(l["source"]["size"], json!({"width": 128, "height": 128}));
        assert_eq!(l["error"], Value::Null);
    }
    let v = document::document(&doctor(&["--json", "--file", none.path.to_str().unwrap()]).stdout);
    must_fail(
        "the master with no calibration, stitchable in JSON",
        v["result"]["file"]["stitchable"] == true,
    );

    // A file that can't be probed fails the run with probe's error, and
    // the machine's report comes with it.
    let gone = dir.join("gone.insv");
    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .arg("doctor")
        .arg("--render-node")
        .arg(missing_node())
        .args(["--json", "--file"])
        .arg(&gone)
        .env("VK_DRIVER_FILES", lavapipe_icd())
        .env("VK_ICD_FILENAMES", lavapipe_icd())
        .env_remove("SPHERECUT_LOG")
        .output()
        .unwrap();
    let v = document::document(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "not_found", "{v}");
    assert_eq!(v["result"]["file"], Value::Null);
    assert!(v["result"].get("decoders").is_some(), "{v}");
    std::fs::remove_dir_all(&dir).ok();
}
