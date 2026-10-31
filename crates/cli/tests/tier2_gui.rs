//! Tier 2: the GUI on a real desktop (M1 plan section 12, item 5).
//! `gui_smoke` runs the smoke run, `spherecut --smoke`, for 20 s on each
//! clip there is: hw-test's noisy 8- and 10-bit 5.7K perf clips, a seam
//! clip, the sync clip (`sync-flash-beep`), each stitched sample and the
//! longest X5 raw file (its raw view), in the graphical session
//! `require_graphical_session` finds. It skips without one, so here,
//! with no desktop, it reports TIER2-SKIP and never a pass.
//!
//! A run fails when, after its first second (the document's `settled`,
//! as the playback benches leave out the cache's filling):
//!
//! - the process fails, or its document isn't `ok`;
//! - fewer than 90% of the UI frames expected were presented. With as
//!   many passes without the UI as half the frames missing, the
//!   compositor didn't draw the window (a locked session, or one on
//!   another VT), so nothing else the run measured counts; with fewer,
//!   the window was drawn and the UI was slow;
//! - more than 0.1% of the frames were dropped;
//! - the UI's p99 is over 1.5 × the refresh period;
//! - on the sync clip, |A/V|'s p95 is 20 ms or more;
//! - an HEVC stream decodes in software where doctor's check says VA-API
//!   decodes its profile.
//!
//! The first run also exports the clip's first 2 s while it plays
//! (`--smoke-export`, M2 plan section 11); the second exports them as an
//! 8K 360 (`--smoke-export-preset 360-7680`), and the third as a 4K
//! flat with motion blur over a pan (`--smoke-export-preset
//! flat-16x9-2160 --smoke-export-blur`): amendment 2 item 22's
//! measurement of the heaviest export frames while the viewer plays.
//! Each fails when the export does, or when the presented share, the
//! drops or the UI's p99 over the time it ran miss the same gates.
//!
//! A locked screen draws no window, so nothing runs while logind says
//! the session is locked (`tier2::session_locked`): with the screen
//! locked at the start, the test fails at once, saying so; a run it
//! locks before is failed unrun, and a failed run says whether the
//! screen was locked by its end. A window that isn't drawn has no UI
//! passes, and the smoke run's watchdog ends it after 15 s.
//!
//! Each run's document and screenshot go to `SPHERECUT_GUI_OUT`
//! (hw-test's), and `gui-smoke.json` there lists the runs and their
//! verdicts, which the report's GUI section shows. A clip that isn't
//! there (no samples; perf clips `--quick` didn't make) is listed as not
//! run, with why. `the_gates_fail_what_they_say` is the gates' Tier 1
//! check.
//!
//! `gui_proto` runs the toolkit check's timeline prototype (M2 plan
//! section 12 and amendment 3), `spherecut --timeline-proto`, for 20 s
//! at the desktop's scale, and when that is 1.0 again at egui zooms of
//! 1.25 and 1.5. A run fails when, after its first second:
//!
//! - the process fails, or its document isn't `ok`;
//! - the monitor gave no refresh rate, so nothing can be judged;
//! - fewer than 99% of the UI frames expected were presented (said as
//!   above when the compositor didn't draw the window);
//! - a pass's own work, as eframe counts it, has a p99 of a refresh
//!   period or more;
//! - the interval between passes has a p99 over 1.5 periods;
//! - any of the script's moves, trims, zooms or pans didn't land, the
//!   zoom didn't reach 2 s on screen, or a thumbnail is missing.
//!
//! Its runs go to the same directory, listed in `gui-proto.json`.
//! `the_prototype_gates_fail_what_they_say` is their Tier 1 check.
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists them as
//! ignored.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_media::detect::Layout;
use spherecut_media::doctor::{self, CheckResult};
use spherecut_media::probe::{self, StreamKind};
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Codec, PERF_5K7_8BIT, PERF_5K7_10BIT, Spec};
use spherecut_testkit::tier2;

/// How long each run plays.
const SECONDS: f64 = 20.0;

/// Past this, a run that hasn't ended is killed: the window's first
/// frame waits up to 60 s, and the writer a little.
const HANG: Duration = Duration::from_secs(120);

/// The gates (module docs).
const MIN_PRESENTED: f64 = 0.90;
const MAX_DROPPED: f64 = 0.001;
const MAX_UI_P99_PERIODS: f64 = 1.5;
const MAX_AV_P95_MS: f64 = 20.0;

/// The summary hw-test reads.
const SUMMARY: &str = "gui-smoke.json";

/// Why nothing ran.
const LOCKED: &str = "the screen is locked: unlock it, keep it on, and run the GUI part again \
     with `cargo xtask hw-test --only gui_smoke --no-tier1` (and `--samples` as before)";

/// The prototype's runs: how long, its gates (module docs), and its
/// summary.
const PROTO_SECONDS: f64 = 20.0;
const MIN_PROTO_PRESENTED: f64 = 0.99;
const MAX_INTERVAL_P99_PERIODS: f64 = 1.5;
const PROTO_SUMMARY: &str = "gui-proto.json";
const PROTO_LOCKED: &str = "the screen is locked: unlock it, keep it on, and run the GUI part \
     again with `cargo xtask hw-test --only gui_proto --no-tier1`";

/// One clip to run.
struct Clip {
    label: String,
    path: PathBuf,
    /// The sync clip, whose A/V offset gates.
    sync: bool,
}

/// What each run's document is judged by, besides the clip.
struct Gates {
    /// The HEVC profiles doctor's check says VA-API decodes ("HEVC Main",
    /// "HEVC Main 10").
    vaapi_hevc: Vec<String>,
}

/// Why the run in `doc` fails, by the gates; empty when it passes.
fn judge(doc: &Value, clip_is_sync: bool, gates: &Gates) -> Vec<String> {
    let mut why = Vec::new();
    if doc["ok"] != true {
        why.push(format!(
            "the run failed: {} ({})",
            doc["error"]["message"].as_str().unwrap_or("no message"),
            doc["error"]["code"].as_str().unwrap_or("no code")
        ));
        return why;
    }
    let r = &doc["result"];
    let f = |v: &Value| v.as_f64();
    // After the first second, while the cache fills, as the playback
    // benches judge; the whole run where it ended sooner.
    let judged = if r["settled"].is_object() {
        &r["settled"]
    } else {
        r
    };
    let ui = &judged["ui"];
    let presented = f(&ui["presented"]).unwrap_or(0.0);
    if presented < MIN_PRESENTED {
        // eframe runs passes without the UI while the compositor draws
        // nothing of the window; a window drawn but slow has none.
        let expected = f(&ui["expected"]).unwrap_or(0.0);
        let frames = f(&ui["frames"]).unwrap_or(0.0);
        let hidden = f(&ui["passes"]).unwrap_or(0.0) - frames;
        if hidden >= (expected - frames) / 2.0 {
            why.push(format!(
                "{:.0}% of the UI frames expected were presented: the compositor didn't draw \
                 the window (a locked session, or one on another VT), so the rest doesn't count",
                presented * 100.0
            ));
            return why;
        }
        why.push(format!(
            "{:.0}% of the UI frames expected were presented, the window drawn ({hidden:.0} \
             passes without the UI): the UI was slow, each pass's own work {} ms at p95",
            presented * 100.0,
            f(&r["ui"]["work"]["p95"]).map_or("unmeasured".into(), |w| format!("{w:.1}"))
        ));
    }
    let shown = f(&judged["frames"]["shown"]).unwrap_or(0.0);
    let dropped = f(&judged["frames"]["dropped"]).unwrap_or(0.0);
    if shown + dropped > 0.0 && dropped / (shown + dropped) > MAX_DROPPED {
        why.push(format!(
            "{dropped} of {} frames dropped ({:.2}%, over {:.1}%)",
            shown + dropped,
            100.0 * dropped / (shown + dropped),
            MAX_DROPPED * 100.0
        ));
    }
    if let (Some(hz), Some(p99)) = (f(&r["refresh_hz"]), f(&ui["p99"])) {
        let limit = MAX_UI_P99_PERIODS * 1000.0 / hz;
        if p99 > limit {
            why.push(format!(
                "UI p99 {p99:.1} ms, over {MAX_UI_P99_PERIODS} × the {hz:.0} Hz period ({limit:.1} ms)"
            ));
        }
    }
    if clip_is_sync {
        match f(&r["av"]["p95_abs_ms"]) {
            Some(p95) if p95 >= MAX_AV_P95_MS => {
                why.push(format!("|A/V| p95 {p95:.1} ms, {MAX_AV_P95_MS} ms or more"));
            }
            Some(_) => {}
            None => why.push("no A/V offset measured on the sync clip".into()),
        }
    }
    for (i, st) in r["streams"].as_array().into_iter().flatten().enumerate() {
        let format = st["format"].as_str().unwrap_or("");
        let Some(rest) = format.strip_prefix("hevc ") else {
            continue;
        };
        let pix = rest.split(' ').nth(1).unwrap_or("");
        let profile = if pix.contains("10") {
            "HEVC Main 10"
        } else {
            "HEVC Main"
        };
        // Vulkan Video is hardware too, and Auto's pick on a discrete GPU.
        if gates.vaapi_hevc.iter().any(|p| p == profile) && st["decoder"] == "software" {
            why.push(format!(
                "stream {i} ({profile}) decodes on {} where doctor says VA-API decodes it",
                st["decode"].as_str().unwrap_or("?")
            ));
        }
    }
    why
}

/// The camera files under the samples directory, by layout: each
/// stitched equirect, and the longest X5 raw file with two lenses.
fn sample_clips(dir: &Path) -> (Vec<Clip>, Option<Clip>) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| {
                ["insv", "mp4", "mov"].contains(&x.to_string_lossy().to_ascii_lowercase().as_str())
            }) {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, &mut files);
    files.sort();
    let mut stitched = Vec::new();
    let mut raw: Option<(u64, PathBuf)> = None;
    for p in files {
        let Ok(r) = probe::probe(&p) else { continue };
        let layout = r.camera.as_ref().map(|c| c.layout);
        let videos = r
            .streams
            .iter()
            .filter(|s| s.kind == StreamKind::Video)
            .count();
        let name = p.strip_prefix(dir).unwrap_or(&p).display().to_string();
        match layout {
            Some(Layout::EquirectTagged) => stitched.push(Clip {
                label: format!("stitched sample {name}"),
                path: p,
                sync: false,
            }),
            Some(Layout::Insta360DualFisheye)
                if videos == 2 && raw.as_ref().is_none_or(|(size, _)| r.file_size > *size) =>
            {
                raw = Some((r.file_size, p));
            }
            _ => {}
        }
    }
    let raw = raw.map(|(_, p)| Clip {
        label: format!(
            "X5 raw view {}",
            p.strip_prefix(dir).unwrap_or(&p).display()
        ),
        path: p,
        sync: false,
    });
    (stitched, raw)
}

/// The run's process: the window in `session`, killed if it hangs.
fn smoke(
    session: &tier2::GraphicalSession,
    clip: &Path,
    json: &Path,
    png: &Path,
    export: Option<&Export>,
) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    cmd.arg("--smoke")
        .arg(clip)
        .args(["--smoke-seconds", &SECONDS.to_string()])
        .arg("--smoke-out")
        .arg(json)
        .arg("--smoke-screenshot")
        .arg(png);
    if let Some(e) = export {
        cmd.arg("--smoke-export").arg(&e.path);
        if let Some(p) = e.preset {
            cmd.args(["--smoke-export-preset", p]);
        }
        if e.blur {
            cmd.arg("--smoke-export-blur");
        }
    }
    window(session, cmd)
}

/// A run's export while it plays.
struct Export {
    path: PathBuf,
    preset: Option<&'static str>,
    blur: bool,
}

/// `cmd`'s window in `session`, killed if it hangs: its exit code and
/// what it wrote to stderr.
fn window(session: &tier2::GraphicalSession, mut cmd: Command) -> (Option<i32>, String) {
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    for (k, v) in &session.env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                use std::io::Read;
                let _ = e.read_to_string(&mut err);
            }
            return (status.code(), err);
        }
        if start.elapsed() > HANG {
            let _ = child.kill();
            let _ = child.wait();
            return (None, format!("killed after {} s", HANG.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "tier2: needs a desktop session (GNOME or Hyprland) on a GPU"]
fn gui_smoke() {
    let session = tier2::require_graphical_session();
    let out = std::env::var_os("SPHERECUT_GUI_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("gui-smoke-{}", std::process::id()))
        });
    std::fs::create_dir_all(&out).unwrap();
    let locked = tier2::session_locked();
    let summary = |vaapi_hevc: &[String], runs: &[Value], not_run: &[(String, String)]| {
        let s = json!({
            "session": session.found.describe(),
            "env": session.env.iter().map(|(k, v)| format!("{k}={}", v.to_string_lossy())).collect::<Vec<_>>(),
            "locked": locked,
            "vaapi_hevc": vaapi_hevc,
            "runs": runs,
            "not_run": not_run.iter().map(|(l, w)| json!({"label": l, "why": w})).collect::<Vec<_>>(),
        });
        std::fs::write(
            out.join(SUMMARY),
            serde_json::to_string_pretty(&s).unwrap() + "\n",
        )
        .unwrap();
    };
    if locked == Some(true) {
        summary(&[], &[], &[("every run".into(), LOCKED.into())]);
        panic!("{LOCKED}");
    }

    // What doctor says VA-API decodes.
    let node = tier2::render_node().unwrap_or_else(|| PathBuf::from("/dev/dri/renderD128"));
    let report = doctor::run(&node);
    let gates = Gates {
        vaapi_hevc: report
            .decode_checks
            .iter()
            .filter(|c| {
                c.path == "vaapi" && c.result == CheckResult::Works && c.format.starts_with("HEVC")
            })
            .map(|c| c.format.clone())
            .collect(),
    };

    let mut clips = Vec::new();
    let mut not_run: Vec<(String, String)> = Vec::new();
    for (label, name) in [
        ("noisy 5.7K 8-bit", PERF_5K7_8BIT),
        ("noisy 5.7K 10-bit", PERF_5K7_10BIT),
    ] {
        match tier2::perf_clip(&format!("{name}.mp4")) {
            Ok(path) => clips.push(Clip {
                label: label.into(),
                path,
                sync: false,
            }),
            Err(why) => not_run.push((label.into(), why)),
        }
    }
    let made = out.join("media");
    std::fs::create_dir_all(&made).unwrap();
    let seam = media::write(
        &Spec {
            name: "gui-seam".into(),
            pattern: Pattern::Seam,
            width: 3840,
            height: 1920,
            frames: 300,
            codec: Codec::Hevc,
            spherical: true,
            ..Spec::default()
        },
        &made,
    )
    .unwrap();
    clips.push(Clip {
        label: "seam 3840×1920 HEVC".into(),
        path: seam,
        sync: false,
    });
    clips.push(Clip {
        label: "sync flash and beep".into(),
        path: media::write(&media::sync_flash_beep(), &made).unwrap(),
        sync: true,
    });
    match tier2::samples_dir() {
        Some(dir) => {
            let (stitched, raw) = sample_clips(&dir);
            if stitched.is_empty() {
                not_run.push((
                    "stitched samples".into(),
                    format!("none under {} (clip 12 is still wanted)", dir.display()),
                ));
            }
            clips.extend(stitched);
            match raw {
                Some(c) => clips.push(c),
                None => not_run.push((
                    "X5 raw view".into(),
                    format!("no X5 file with two lenses under {}", dir.display()),
                )),
            }
        }
        None => not_run.push(("camera samples".into(), "no samples directory".into())),
    }

    let mut runs = Vec::new();
    let mut failed = Vec::new();
    for (n, clip) in clips.iter().enumerate() {
        let json = out.join(format!("gui-{n}.json"));
        let png = out.join(format!("gui-{n}.png"));
        // The first run exports 2 s while it plays (M2 plan section 11),
        // the second an 8K 360 and the third a 4K flat with blur
        // (amendment 2 item 22).
        let path = out.join(format!("gui-{n}-export.mp4"));
        let export = match n {
            0 => Some((None, false)),
            1 => Some((Some("360-7680"), false)),
            2 => Some((Some("flat-16x9-2160"), true)),
            _ => None,
        }
        .map(|(preset, blur)| Export { path, preset, blur });
        let _ = std::fs::remove_file(&json);
        let _ = std::fs::remove_file(&png);
        if let Some(e) = &export {
            let _ = std::fs::remove_file(&e.path);
        }
        eprintln!("==> {}: {}", clip.label, clip.path.display());
        if tier2::session_locked() == Some(true) {
            let why = "not run: the screen locked during the runs".to_string();
            failed.push(format!("{}: {why}", clip.label));
            runs.push(json!({
                "n": n,
                "label": clip.label,
                "clip": clip.path.display().to_string(),
                "document": json.display().to_string(),
                "screenshot": null,
                "failed": [why],
            }));
            continue;
        }
        let (status, stderr) = smoke(&session, &clip.path, &json, &png, export.as_ref());
        let doc = std::fs::read(&json)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let mut why = match &doc {
            Some(d) if export.is_some() => {
                let mut why = judge(d, clip.sync, &gates);
                why.extend(judge_export(d));
                why
            }
            Some(d) => judge(d, clip.sync, &gates),
            None => vec![format!(
                "no document (exit {status:?}): {}",
                stderr.lines().last().unwrap_or("")
            )],
        };
        if why.is_empty() && status != Some(0) {
            why.push(format!(
                "exit {status:?}: {}",
                stderr.lines().last().unwrap_or("")
            ));
        }
        if !why.is_empty() && tier2::session_locked() == Some(true) {
            why.push("the screen was locked by the run's end".into());
        }
        for w in &why {
            failed.push(format!("{}: {w}", clip.label));
        }
        runs.push(json!({
            "n": n,
            "label": clip.label,
            "clip": clip.path.display().to_string(),
            "document": json.display().to_string(),
            "screenshot": png.is_file().then(|| png.display().to_string()),
            "failed": why,
        }));
    }
    summary(&gates.vaapi_hevc, &runs, &not_run);
    for (label, why) in &not_run {
        eprintln!("not run: {label}: {why}");
    }
    assert!(
        failed.is_empty(),
        "{} of {} runs failed:\n{}",
        runs.iter().filter(|r| r["failed"] != json!([])).count(),
        runs.len(),
        failed.join("\n")
    );
}

/// Why the export a run made while it played fails, by the run's own
/// gates over the time it ran; empty when it passes.
fn judge_export(doc: &Value) -> Vec<String> {
    let mut why = Vec::new();
    let e = &doc["result"]["export"];
    if e["ok"] != true {
        why.push(format!(
            "the export while playing failed: {}",
            e["error"].as_str().unwrap_or("no document part")
        ));
        return why;
    }
    let f = |v: &Value| v.as_f64();
    let w = &e["while_running"];
    let presented = f(&w["ui"]["presented"]).unwrap_or(0.0);
    if presented < MIN_PRESENTED {
        why.push(format!(
            "{:.0}% of the UI frames expected were presented while the export ran, under {:.0}%",
            presented * 100.0,
            MIN_PRESENTED * 100.0
        ));
    }
    let shown = f(&w["frames"]["shown"]).unwrap_or(0.0);
    let dropped = f(&w["frames"]["dropped"]).unwrap_or(0.0);
    if shown + dropped > 0.0 && dropped / (shown + dropped) > MAX_DROPPED {
        why.push(format!(
            "{dropped} of {} frames dropped while the export ran",
            shown + dropped
        ));
    }
    if let (Some(hz), Some(p99)) = (f(&doc["result"]["refresh_hz"]), f(&w["ui"]["p99"])) {
        let limit = MAX_UI_P99_PERIODS * 1000.0 / hz;
        if p99 > limit {
            why.push(format!(
                "UI p99 {p99:.1} ms while the export ran, over {MAX_UI_P99_PERIODS} × the \
                 {hz:.0} Hz period ({limit:.1} ms)"
            ));
        }
    }
    why
}

/// Why the prototype's run in `doc` fails, by its gates; empty when it
/// passes.
fn judge_proto(doc: &Value) -> Vec<String> {
    let mut why = Vec::new();
    if doc["ok"] != true {
        why.push(format!(
            "the run failed: {} ({})",
            doc["error"]["message"].as_str().unwrap_or("no message"),
            doc["error"]["code"].as_str().unwrap_or("no code")
        ));
        return why;
    }
    let r = &doc["result"];
    let ui = &r["ui"];
    let f = |v: &Value| v.as_f64();
    let Some(hz) = f(&r["refresh_hz"]) else {
        why.push("the monitor gave no refresh rate, so nothing can be judged".into());
        return why;
    };
    let period = 1000.0 / hz;
    let presented = f(&ui["presented"]).unwrap_or(0.0);
    if presented < MIN_PROTO_PRESENTED {
        let expected = f(&ui["expected"]).unwrap_or(0.0);
        let frames = f(&ui["frames"]).unwrap_or(0.0);
        let hidden = f(&ui["passes"]).unwrap_or(0.0) - frames;
        if hidden >= (expected - frames) / 2.0 {
            why.push(format!(
                "{:.0}% of the UI frames expected were presented: the compositor didn't draw \
                 the window (a locked session, or one on another VT), so the rest doesn't count",
                presented * 100.0
            ));
            return why;
        }
        why.push(format!(
            "{:.1}% of the UI frames expected were presented, under {:.0}% ({} late)",
            presented * 100.0,
            MIN_PROTO_PRESENTED * 100.0,
            ui["late"]
        ));
    }
    match f(&ui["work_ms"]["p99"]) {
        Some(w) if w >= period => why.push(format!(
            "each pass's own work has a p99 of {w:.2} ms, not under the {hz:.0} Hz period \
             ({period:.2} ms); building the UI's p99 {} ms",
            f(&ui["build_ms"]["p99"]).map_or("unmeasured".into(), |b| format!("{b:.2}"))
        )),
        Some(_) => {}
        None => why.push("eframe counted no pass's work".into()),
    }
    if let Some(p99) = f(&ui["interval_ms"]["p99"]) {
        let limit = MAX_INTERVAL_P99_PERIODS * period;
        if p99 > limit {
            why.push(format!(
                "UI interval p99 {p99:.1} ms, over {MAX_INTERVAL_P99_PERIODS} × the {hz:.0} Hz \
                 period ({limit:.1} ms)"
            ));
        }
    }
    let a = &r["actions"];
    for k in ["moves", "trims", "zoom_passes", "pan_passes"] {
        if a[k].as_u64().unwrap_or(0) == 0 {
            why.push(format!("the script's {} didn't land", k.replace('_', " ")));
        }
    }
    if f(&r["span_s"][0]).is_none_or(|lo| lo > 2.05) {
        why.push(format!(
            "the zoom didn't reach 2 s on screen: {} s at the closest",
            r["span_s"][0]
        ));
    }
    if r["textures"] != r["clips"] || r["clips"].as_u64().unwrap_or(0) == 0 {
        why.push(format!(
            "{} thumbnails for {} clips",
            r["textures"], r["clips"]
        ));
    }
    if r["blocked_on_ui_thread"] != 0 {
        why.push(format!(
            "{} blocking calls on the UI thread",
            r["blocked_on_ui_thread"]
        ));
    }
    why
}

#[test]
#[ignore = "tier2: needs a desktop session (GNOME or Hyprland) on a GPU"]
fn gui_proto() {
    let session = tier2::require_graphical_session();
    let out = std::env::var_os("SPHERECUT_GUI_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("gui-proto-{}", std::process::id()))
        });
    std::fs::create_dir_all(&out).unwrap();
    let locked = tier2::session_locked();
    let summary = |runs: &[Value], not_run: &[(String, String)]| {
        let s = json!({
            "session": session.found.describe(),
            "env": session.env.iter().map(|(k, v)| format!("{k}={}", v.to_string_lossy())).collect::<Vec<_>>(),
            "locked": locked,
            "seconds": PROTO_SECONDS,
            "runs": runs,
            "not_run": not_run.iter().map(|(l, w)| json!({"label": l, "why": w})).collect::<Vec<_>>(),
        });
        std::fs::write(
            out.join(PROTO_SUMMARY),
            serde_json::to_string_pretty(&s).unwrap() + "\n",
        )
        .unwrap();
    };
    if locked == Some(true) {
        summary(&[], &[("every run".into(), PROTO_LOCKED.into())]);
        panic!("{PROTO_LOCKED}");
    }

    let mut runs = Vec::new();
    let mut failed = Vec::new();
    // The desktop's scale first; at 1.0, egui's zoom gives the others.
    let mut zooms = vec![1.0f32];
    let mut n = 0;
    while n < zooms.len() {
        let zoom = zooms[n];
        let json = out.join(format!("gui-proto-{n}.json"));
        let png = out.join(format!("gui-proto-{n}.png"));
        let _ = std::fs::remove_file(&json);
        let _ = std::fs::remove_file(&png);
        let label = format!("egui zoom {zoom}");
        eprintln!("==> the timeline prototype, {label}");
        if tier2::session_locked() == Some(true) {
            let why = "not run: the screen locked during the runs".to_string();
            failed.push(format!("{label}: {why}"));
            runs.push(json!({"n": n, "label": label, "zoom": zoom, "document": json.display().to_string(), "screenshot": null, "failed": [why]}));
            n += 1;
            continue;
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
        cmd.arg("--timeline-proto")
            .args(["--proto-seconds", &PROTO_SECONDS.to_string()])
            .args(["--proto-zoom", &zoom.to_string()])
            .arg("--proto-out")
            .arg(&json)
            .arg("--proto-screenshot")
            .arg(&png);
        let (status, stderr) = window(&session, cmd);
        let doc = std::fs::read(&json)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let mut why = match &doc {
            Some(d) => judge_proto(d),
            None => vec![format!(
                "no document (exit {status:?}): {}",
                stderr.lines().last().unwrap_or("")
            )],
        };
        if why.is_empty() && status != Some(0) {
            why.push(format!(
                "exit {status:?}: {}",
                stderr.lines().last().unwrap_or("")
            ));
        }
        if !why.is_empty() && tier2::session_locked() == Some(true) {
            why.push("the screen was locked by the run's end".into());
        }
        if n == 0
            && doc
                .as_ref()
                .and_then(|d| d["result"]["native_scale"].as_f64())
                .is_some_and(|s| (s - 1.0).abs() < 1e-3)
        {
            zooms.extend([1.25, 1.5]);
        }
        for w in &why {
            failed.push(format!("{label}: {w}"));
        }
        runs.push(json!({
            "n": n,
            "label": label,
            "zoom": zoom,
            "document": json.display().to_string(),
            "screenshot": png.is_file().then(|| png.display().to_string()),
            "failed": why,
        }));
        n += 1;
    }
    summary(&runs, &[]);
    assert!(
        failed.is_empty(),
        "{} of {} runs failed:\n{}",
        runs.iter().filter(|r| r["failed"] != json!([])).count(),
        runs.len(),
        failed.join("\n")
    );
}

/// The prototype's gates on documents made up to fail each one (Tier 1).
#[test]
fn the_prototype_gates_fail_what_they_say() {
    let good = || {
        json!({
            "ok": true,
            "result": {
                "refresh_hz": 60.0,
                "clips": 500,
                "textures": 500,
                "span_s": [2.0, 777.0],
                "actions": {"moves": 2, "trims": 2, "scrubs": 0, "zoom_passes": 300, "pan_passes": 200, "snaps": 1},
                "blocked_on_ui_thread": 0,
                "ui": {
                    "presented": 0.995, "expected": 1140.0, "frames": 1134, "passes": 1134, "late": 3,
                    "interval_ms": {"p99": 17.4},
                    "work_ms": {"p99": 6.0},
                    "build_ms": {"p99": 2.5},
                },
            },
        })
    };
    assert!(
        judge_proto(&good()).is_empty(),
        "{:?}",
        judge_proto(&good())
    );
    let fails = |d: &Value, has: &str| {
        let why = judge_proto(d);
        assert!(
            why.iter().any(|w| w.contains(has)),
            "{has:?} not in {why:?}"
        );
    };
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut d = good();
        edit(&mut d["result"]);
        d
    };
    fails(
        &json!({"ok": false, "error": {"code": "io_error", "message": "full"}}),
        "the run failed: full (io_error)",
    );
    let no_hz = with(&|r| r["refresh_hz"] = Value::Null);
    assert_eq!(judge_proto(&no_hz).len(), 1);
    fails(&no_hz, "no refresh rate");
    // Hidden: only that is said.
    let hidden = with(&|r| {
        r["ui"]["presented"] = json!(0.4);
        r["ui"]["frames"] = json!(456);
        r["ui"]["passes"] = json!(1130);
        r["ui"]["work_ms"]["p99"] = json!(30.0);
    });
    assert_eq!(judge_proto(&hidden).len(), 1, "{:?}", judge_proto(&hidden));
    fails(&hidden, "the compositor didn't draw");
    // 98.5% drawn: slow, and the other gates still judge.
    let slow = with(&|r| {
        r["ui"]["presented"] = json!(0.985);
        r["ui"]["frames"] = json!(1123);
        r["ui"]["passes"] = json!(1123);
        r["ui"]["late"] = json!(17);
        r["ui"]["work_ms"]["p99"] = json!(16.7);
    });
    let why = judge_proto(&slow);
    assert_eq!(why.len(), 2, "{why:?}");
    fails(
        &slow,
        "98.5% of the UI frames expected were presented, under 99% (17 late)",
    );
    fails(
        &slow,
        "a p99 of 16.70 ms, not under the 60 Hz period (16.67 ms); building the UI's p99 2.50 ms",
    );
    fails(
        &with(&|r| r["ui"]["work_ms"]["p99"] = Value::Null),
        "eframe counted no pass's work",
    );
    // 1.5 × 16.7 ms is 25 ms.
    fails(
        &with(&|r| r["ui"]["interval_ms"]["p99"] = json!(25.5)),
        "UI interval p99 25.5 ms",
    );
    assert!(judge_proto(&with(&|r| r["ui"]["interval_ms"]["p99"] = json!(24.9))).is_empty());
    fails(
        &with(&|r| r["actions"]["trims"] = json!(0)),
        "the script's trims didn't land",
    );
    fails(
        &with(&|r| r["actions"]["zoom_passes"] = json!(0)),
        "the script's zoom passes didn't land",
    );
    fails(
        &with(&|r| r["span_s"][0] = json!(3.0)),
        "the zoom didn't reach 2 s",
    );
    fails(
        &with(&|r| r["textures"] = json!(0)),
        "0 thumbnails for 500 clips",
    );
    fails(
        &with(&|r| r["blocked_on_ui_thread"] = json!(2)),
        "2 blocking calls",
    );
}

/// The gates on documents made up to fail each one (Tier 1).
#[test]
fn the_gates_fail_what_they_say() {
    let good = || {
        json!({
            "ok": true,
            "result": {
                "refresh_hz": 60.0,
                "ui": {"presented": 0.99, "p99": 20.0, "expected": 1200.0, "frames": 1188, "passes": 1188},
                "frames": {"shown": 600, "dropped": 0},
                "av": {"p95_abs_ms": 8.0},
                "streams": [{"format": "hevc 5760×2880 yuv420p10le limited/709", "decoder": "vaapi", "decode": "VA-API"}],
            },
        })
    };
    let gates = Gates {
        vaapi_hevc: vec!["HEVC Main".into(), "HEVC Main 10".into()],
    };
    assert!(judge(&good(), true, &gates).is_empty());
    let with = |path: &[&str], v: Value| {
        let mut d = good();
        let mut at = &mut d;
        for k in path {
            at = &mut at[*k];
        }
        *at = v;
        d
    };
    let fails = |d: &Value, sync: bool, has: &str| {
        let why = judge(d, sync, &gates);
        assert!(
            why.iter().any(|w| w.contains(has)),
            "{has:?} not in {why:?}"
        );
    };
    fails(
        &json!({"ok": false, "error": {"code": "not_found", "message": "gone"}}),
        false,
        "the run failed: gone (not_found)",
    );
    // A hidden window, eframe's logic running without the UI in the
    // frames missing: only that is said.
    let mut hidden = good();
    hidden["result"]["ui"] =
        json!({"presented": 0.4, "p99": 90.0, "expected": 1200.0, "frames": 480, "passes": 1180});
    assert_eq!(judge(&hidden, true, &gates).len(), 1);
    fails(&hidden, false, "40% of the UI frames");
    fails(&hidden, false, "the compositor didn't draw");
    // A window drawn but slow: said, and the other gates still judge.
    let mut slow = good();
    slow["result"]["ui"] = json!({"presented": 0.69, "p99": 54.8, "expected": 1200.0, "frames": 828, "passes": 830, "work": {"p95": 3.25}});
    let why = judge(&slow, true, &gates);
    assert_eq!(why.len(), 2, "{why:?}");
    fails(
        &slow,
        false,
        "69% of the UI frames expected were presented, the window drawn (2 passes",
    );
    fails(
        &slow,
        false,
        "the UI was slow, each pass's own work 3.2 ms at p95",
    );
    fails(&slow, false, "UI p99 54.8 ms");
    fails(
        &with(&["result", "frames", "dropped"], json!(1)),
        false,
        "1 of 601 frames dropped",
    );
    assert!(
        judge(
            &with(&["result", "frames", "shown"], json!(1000)),
            false,
            &gates
        )
        .is_empty()
    );
    // 1.5 × 16.7 ms is 25 ms.
    fails(
        &with(&["result", "ui", "p99"], json!(25.5)),
        false,
        "UI p99 25.5 ms",
    );
    assert!(judge(&with(&["result", "refresh_hz"], Value::Null), false, &gates).is_empty());
    // After the first second when the document has it: the startup's
    // drops and slow frames aren't judged.
    let mut startup = with(&["result", "frames", "dropped"], json!(9));
    startup["result"]["ui"]["p99"] = json!(60.0);
    startup["result"]["settled"] = json!({
        "from_s": 1.0,
        "ui": {"presented": 0.99, "p99": 17.5, "expected": 1140.0, "frames": 1129, "passes": 1129},
        "frames": {"shown": 570, "dropped": 0},
    });
    assert!(judge(&startup, false, &gates).is_empty());
    startup["result"]["settled"]["frames"]["dropped"] = json!(2);
    fails(&startup, false, "2 of 572 frames dropped");
    // A/V only on the sync clip.
    let late = with(&["result", "av", "p95_abs_ms"], json!(20.0));
    fails(&late, true, "|A/V| p95 20.0 ms");
    assert!(judge(&late, false, &gates).is_empty());
    fails(&with(&["result", "av"], Value::Null), true, "no A/V offset");
    // Software where VA-API decodes the profile; not where it doesn't.
    let mut sw = good();
    sw["result"]["streams"][0]["decoder"] = json!("software");
    sw["result"]["streams"][0]["decode"] = json!("software");
    fails(&sw, false, "stream 0 (HEVC Main 10) decodes on software");
    // Vulkan Video, as Auto picks on a discrete GPU, is hardware.
    let mut vk = good();
    vk["result"]["streams"][0]["decoder"] = json!("vulkan");
    vk["result"]["streams"][0]["decode"] = json!("Vulkan Video");
    assert!(judge(&vk, false, &gates).is_empty());
    let only8 = Gates {
        vaapi_hevc: vec!["HEVC Main".into()],
    };
    assert!(judge(&sw, false, &only8).is_empty());
}

#[test]
fn the_export_gates_fail_what_they_say() {
    let good = || {
        json!({
            "ok": true,
            "result": {
                "refresh_hz": 60.0,
                "export": {
                    "ok": true,
                    "while_running": {
                        "ui": {"presented": 0.97, "p99": 21.0},
                        "frames": {"shown": 180, "dropped": 0},
                    },
                },
            },
        })
    };
    assert!(judge_export(&good()).is_empty());
    let fails = |d: &Value, has: &str| {
        let why = judge_export(d);
        assert!(
            why.iter().any(|w| w.contains(has)),
            "{has:?} not in {why:?}"
        );
    };
    let mut failed = good();
    failed["result"]["export"] = json!({"ok": false, "error": "no encoder for H.264"});
    fails(
        &failed,
        "the export while playing failed: no encoder for H.264",
    );
    fails(&json!({"ok": true, "result": {}}), "no document part");
    let mut slow = good();
    slow["result"]["export"]["while_running"]["ui"] = json!({"presented": 0.8, "p99": 40.0});
    fails(
        &slow,
        "80% of the UI frames expected were presented while the export ran",
    );
    fails(&slow, "UI p99 40.0 ms while the export ran");
    let mut dropped = good();
    dropped["result"]["export"]["while_running"]["frames"]["dropped"] = json!(3);
    fails(&dropped, "3 of 183 frames dropped while the export ran");
}
