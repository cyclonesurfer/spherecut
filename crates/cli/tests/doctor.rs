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
use spherecut_synth::insv::{self as synth_insv, InsvFault};
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
/// provisional"); on one with `NoCalibration`, an LRV, and a master
/// whose lens 1 doesn't decode, that it can't (M3 plan 5.4).
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

    // The lens control: a master whose lens 1 has lost its pictures, each
    // sample zeroed in place, probes as before, but stream #1's first
    // frame doesn't decode, so it can't be stitched.
    let blank = insv::master(&dir, "VID_20261022_101500_00_003.insv", None);
    blank_track(&blank.path, 1);
    let v = document::document(&doctor(&["--json", "--file", blank.path.to_str().unwrap()]).stdout);
    assert_eq!(v["ok"], true, "{v}");
    let f = &v["result"]["file"];
    must_fail(
        "a master whose lens 1 doesn't decode, stitchable",
        f["stitchable"] == true,
    );
    assert_eq!(f["calibration"], "v6", "{f:#}");
    let lenses = f["lenses"].as_array().unwrap();
    assert_eq!(lenses.len(), 2, "{f:#}");
    assert_eq!(lenses[0]["decodes"], true, "{f:#}");
    let l = &lenses[1];
    assert_eq!(l["stream"], 1, "{f:#}");
    assert_eq!(l["decodes"], false, "{f:#}");
    // Its decoder decodes the first frame as it opens, and that failed.
    assert_eq!(l["source"], Value::Null, "{f:#}");
    assert_eq!(l["decode"], Value::Null, "{f:#}");
    assert_eq!(l["error"]["code"], "decode_failed", "{f:#}");
    let error = l["error"]["message"].as_str().unwrap();
    assert_eq!(
        f["missing"],
        json!([format!("stream #1's pictures: {error}")]),
        "{f:#}"
    );
    let t = text(&blank.path);
    must_fail(
        "a master whose lens 1 doesn't decode, stitchable in text",
        t.contains("can be stitched: yes"),
    );
    for want in [
        "  can be stitched: no\n".to_string(),
        format!("  stream #0  h264 128x128 yuv420p, {software}\n"),
        format!("  stream #1  not opened, doesn't decode: {error}\n"),
        format!("  missing: stream #1's pictures: {error}\n"),
    ] {
        assert!(t.contains(&want), "no {want:?} in:\n{t}");
    }

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

/// `doctor --file` says why a file can't be stitched when its trailer is
/// there but can't be read (its tail's size past the file, or its magic
/// changed in an `inst` box), carrying the probe's own words and its
/// `bad_trailer` warning; when the camera names a calibration whose
/// string doesn't parse; and when the file has no video stream at all,
/// which is a check like any other, not a failed run.
#[test]
fn doctor_file_says_why_a_trailer_or_a_video_stream_is_missing() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("doctor-file-why-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let text = |path: &Path| {
        let out = doctor(&["--file", path.to_str().unwrap()]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let json = |path: &Path| {
        let v = document::document(&doctor(&["--json", "--file", path.to_str().unwrap()]).stdout);
        assert_eq!(v["ok"], true, "{v}");
        v["result"]["file"].clone()
    };

    // A trailer whose tail gives more than the file holds: read_trailer's
    // error, the probe's `bad_trailer` warning, both in doctor's words.
    let cut = insv::master(
        &dir,
        "VID_20261022_101500_00_001.insv",
        Some(InsvFault::TruncatedTrailer),
    );
    let t = text(&cut.path);
    let unreadable =
        "  missing: a camera trailer that can be read: the Insta360 trailer can't be read: ";
    assert!(t.contains(unreadable), "no {unreadable:?} in:\n{t}");
    assert!(t.contains("  can be stitched: no\n"), "{t}");
    must_fail(
        "an unreadable trailer, said to be missing",
        t.contains("missing: a camera trailer: Spherecut stitches"),
    );
    let f = json(&cut.path);
    let warned: Vec<&Value> = f["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "bad_trailer")
        .collect();
    assert_eq!(warned.len(), 1, "{f:#}");
    let said = warned[0]["message"].as_str().unwrap();
    assert!(
        said.starts_with(&format!(
            "{}: the Insta360 trailer can't be read: ",
            cut.path.display()
        )),
        "{said}"
    );
    // Its text gives the warning too.
    assert!(t.contains(&format!("  warning: {said}\n")), "{t}");

    // The magic's last byte changed: the file ends in an `inst` box with
    // no trailer after it.
    let magic = insv::master(
        &dir,
        "VID_20261022_101500_00_002.insv",
        Some(InsvFault::BadMagic),
    );
    let t = text(&magic.path);
    let want = "  missing: a camera trailer that can be read: the file ends in an `inst` box, where \
                Insta360 cameras put their trailer, but not in the trailer's magic: byte ";
    assert!(t.contains(want), "no {want:?} in:\n{t}");
    must_fail(
        "a damaged magic, said to be missing",
        t.contains("missing: a camera trailer: Spherecut stitches"),
    );

    // Field 136 names V6, whose string has lost its last token: the
    // camera says which calibration it uses, and that one doesn't parse.
    let mut records = insv::records();
    for f in records.metadata.iter_mut().filter(|f| f.number == 111) {
        if let synth_insv::Value::Bytes(s) = &mut f.value {
            let last = s.iter().rposition(|&b| b == b'_').unwrap();
            s.truncate(last);
        }
    }
    let v6 = insv::master_with(&dir, "VID_20261022_101500_00_003.insv", &records, None);
    let t = text(&v6.path);
    let want = "  missing: the calibration in use: the camera says it uses its v6 calibration \
                (field 136), which the trailer doesn't hold or which doesn't parse; it holds v3\n";
    assert!(t.contains(want), "no {want:?} in:\n{t}");
    must_fail(
        "a calibration the camera names, worded as one it doesn't",
        t.contains("doesn't say it uses one of them"),
    );

    // A file with only an audio stream probes, so it is a check, with no
    // lenses and "a video stream" missing; the run is ok.
    let audio = audio_only(&dir);
    let f = json(&audio);
    assert_eq!(f["lenses"], json!([]), "{f:#}");
    assert_eq!(f["stitchable"], false, "{f:#}");
    let no_video = "a video stream: the file has none, so it has no lenses to stitch";
    assert_eq!(f["missing"][0], no_video, "{f:#}");
    let t = text(&audio);
    assert!(t.contains(&format!("  missing: {no_video}\n")), "{t}");
    assert!(!t.contains("  stream #"), "{t}");
    // The control: a file with video streams isn't missing one.
    let f = json(&cut.path);
    must_fail(
        "a master with two video streams, missing a video stream",
        f["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.as_str().is_some_and(|m| m.starts_with("a video stream"))),
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A file in `dir` with one AAC stream and nothing else: a synthetic
/// clip's audio, remuxed alone into an MP4.
// Reads a file this test wrote and writes another with ffmpeg-next alone,
// apart from Spherecut's confined open (D24).
#[allow(clippy::disallowed_methods)]
fn audio_only(dir: &Path) -> PathBuf {
    use spherecut_media::ff;
    use spherecut_synth::media::{self, Audio, Spec};

    let clip = media::write(
        &Spec {
            name: "with-audio".into(),
            frames: 6,
            audio: Audio::Stereo,
            ..Spec::default()
        },
        &dir.join("with-audio"),
    )
    .unwrap();
    let dst = dir.join("audio-only.m4a");
    let mut ictx = ff::format::input(&clip).unwrap();
    let mut octx = ff::format::output_as(&dst, "mp4").unwrap();
    let st = ictx.streams().best(ff::media::Type::Audio).unwrap();
    let (index, tb) = (st.index(), st.time_base());
    let mut out = octx
        .add_stream(ff::encoder::find(ff::codec::Id::None))
        .unwrap();
    out.set_parameters(st.parameters());
    out.set_time_base(tb);
    octx.write_header().unwrap();
    for (st, mut pkt) in ictx.packets() {
        if st.index() != index {
            continue;
        }
        let out_tb = octx.stream(0).unwrap().time_base();
        pkt.rescale_ts(tb, out_tb);
        pkt.set_stream(0);
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
    dst
}

/// The file at `path` with every sample of its track `track` (in file
/// order) zeroed where it lies in `mdat`, its boxes and sample tables as
/// they were: a lens whose pictures are lost, which probe reads as it
/// did but from which no decoder makes a frame.
fn blank_track(path: &Path, track: usize) {
    let mut f = std::fs::read(path).unwrap();
    let tree = spherecut_media::mp4::read(&mut std::io::Cursor::new(&f)).unwrap();
    let stbl = tree
        .tracks()
        .nth(track)
        .and_then(|t| t.path(&[b"mdia", b"minf", b"stbl"]))
        .unwrap();
    // A table's 32-bit words after its first `skip` bytes: the version
    // and flags, the count, and in `stsz` the size all samples share.
    let words = |t: &[u8; 4], skip: usize| -> Vec<usize> {
        stbl.child(t).unwrap().payload[skip..]
            .chunks_exact(4)
            .map(|w| u32::from_be_bytes(w.try_into().unwrap()) as usize)
            .collect()
    };
    assert_eq!(words(b"stsz", 4)[0], 0, "the samples' sizes differ");
    let sizes = words(b"stsz", 12);
    let chunks = words(b"stco", 8);
    // `stsc`'s runs: first chunk (from 1), samples a chunk, description.
    let stsc = words(b"stsc", 8);
    let runs: Vec<&[usize]> = stsc.chunks_exact(3).collect();
    let mut sample = 0;
    for (k, &start) in chunks.iter().enumerate() {
        let run = runs.iter().rfind(|r| r[0] <= k + 1).unwrap();
        let mut at = start;
        for &size in &sizes[sample..sample + run[1]] {
            f[at..at + size].fill(0);
            at += size;
        }
        sample += run[1];
    }
    assert_eq!(sample, sizes.len());
    std::fs::write(path, &f).unwrap();
}
