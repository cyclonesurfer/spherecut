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
//! - more than 0.1% of the frames were dropped: of pairs for a locked
//!   pair, whose drops count once a pair and frames shown once a lens
//!   (the frames a display slower than the clip passes over by design
//!   are `skipped`, apart);
//! - the UI's p99 is over 1.5 × the refresh period;
//! - on the sync clip, |A/V|'s p95 is 20 ms or more;
//! - an HEVC stream decodes in software where doctor's check says VA-API
//!   decodes its profile;
//! - on the X5 raw view, the lenses didn't lock, or over the whole run a
//!   present showed them at different frames (`unpaired`) or the pair
//!   resynced (`pair_resyncs`), M3 plan section 9.
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
//! `gui_decode_behind` checks that the UI keeps to the display while
//! decode falls behind (perf.md's target, D40), which `gui_smoke` can't:
//! it fails any run that drops frames, and odin's default path keeps
//! up. Its three runs take the smoke run's hidden flags: run 0 holds
//! decode to a frame in 50 ms (20 fps, `--smoke-decode-floor-ms 50`) on
//! the default path, playing a 10 s, 30 fps seam clip to its end; run 1
//! plays the noisy 5.7K 10-bit clip for 20 s on VA-API with its frames
//! copied (`--smoke-decoder vaapi`, `SPHERECUT_ZERO_COPY=0`), where
//! VA-API decodes HEVC Main 10; run 2, the control, is run 0 with the UI
//! waiting for each decoded frame (`--smoke-control-ui-waits`). After
//! the first second each run must have run as asked with decode behind
//! (at most 85% of the frames expected shown), and runs 0 and 1 must
//! hold the UI's gates above (90% presented, a p99 of at most 1.5
//! refresh periods) with no blocking call on the UI thread; run 1 must
//! also decode on VA-API and download its frames. Decode keeping up on
//! run 1 is inconclusive, not a failure. Drops, decode's drops and A/V
//! aren't judged. The control must block, and must fail the UI's gates
//! as a slow UI in a window drawn; a control that passes them, or that
//! didn't run as asked, fails the test. Its runs are listed in
//! `gui-decode-behind.json`, its documents and screenshots
//! `gui-behind-<n>.*`; `the_behind_gates_fail_what_they_say` is the
//! gates' Tier 1 check.
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
use spherecut_testkit::control::must_fail;
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

/// `gui_decode_behind`'s runs (D40): decode is behind when at most this
/// share of the frames expected is shown; the floor that holds it there
/// on the default path; the summary hw-test reads; and why nothing ran.
const MAX_SHOWN_BEHIND: f64 = 0.85;
const DECODE_FLOOR_MS: u32 = 50;
const BEHIND_SUMMARY: &str = "gui-decode-behind.json";
const BEHIND_LOCKED: &str = "the screen is locked: unlock it, keep it on, and run the GUI part \
     again with `cargo xtask hw-test --only gui_decode_behind --no-tier1`";

/// One clip to run.
struct Clip {
    label: String,
    path: PathBuf,
    /// The sync clip, whose A/V offset gates.
    sync: bool,
    /// A camera's raw file, whose lenses must lock ([`judge_pair`]).
    pair: bool,
}

/// What each run's document is judged by, besides the clip.
struct Gates {
    /// The HEVC profiles doctor's check says VA-API decodes ("HEVC Main",
    /// "HEVC Main 10").
    vaapi_hevc: Vec<String>,
}

/// What the gates judge of the result `r`: after the first second,
/// while the cache fills, as the playback benches judge; the whole run
/// where it ended sooner.
fn judged(r: &Value) -> &Value {
    if r["settled"].is_object() {
        &r["settled"]
    } else {
        r
    }
}

/// How many lenses a frame shown counts once a lens for, and what a
/// drop counts: a locked pair's drops count once a pair and its frames
/// shown once a lens.
fn per_lens(r: &Value) -> (f64, &'static str) {
    match r["streams"].as_array().map_or(1, Vec::len) {
        n @ 2.. if r["lock"]["status"] == "locked" => (n as f64, "pairs"),
        _ => (1.0, "frames"),
    }
}

/// Whether the UI in `judged` presented too few frames because the
/// compositor didn't draw the window: eframe runs passes without the UI
/// while the compositor draws nothing of the window, and a window drawn
/// but slow has none.
fn undrawn(judged: &Value) -> bool {
    let ui = &judged["ui"];
    let f = |v: &Value| v.as_f64().unwrap_or(0.0);
    let frames = f(&ui["frames"]);
    f(&ui["presented"]) < MIN_PRESENTED
        && f(&ui["passes"]) - frames >= (f(&ui["expected"]) - frames) / 2.0
}

/// Why the UI in `judged` (the result `r`'s part the gates judge)
/// misses the UI's gates: fewer than 90% of the UI frames expected
/// presented, said alone when the compositor didn't draw the window,
/// since nothing else counts then; and a p99 over 1.5 refresh periods.
/// Empty when it holds them.
fn ui_gates(r: &Value, judged: &Value) -> Vec<String> {
    let mut why = Vec::new();
    let f = |v: &Value| v.as_f64();
    let ui = &judged["ui"];
    let presented = f(&ui["presented"]).unwrap_or(0.0);
    if presented < MIN_PRESENTED {
        if undrawn(judged) {
            why.push(format!(
                "{:.0}% of the UI frames expected were presented: the compositor didn't draw \
                 the window (a locked session, or one on another VT), so the rest doesn't count",
                presented * 100.0
            ));
            return why;
        }
        let hidden = f(&ui["passes"]).unwrap_or(0.0) - f(&ui["frames"]).unwrap_or(0.0);
        why.push(format!(
            "{:.0}% of the UI frames expected were presented, the window drawn ({hidden:.0} \
             passes without the UI): the UI was slow, each pass's own work {} ms at p95",
            presented * 100.0,
            f(&r["ui"]["work"]["p95"]).map_or("unmeasured".into(), |w| format!("{w:.1}"))
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
    why
}

/// Why the run in `doc` fails, by the gates; empty when it passes.
fn judge(doc: &Value, clip_is_sync: bool, gates: &Gates) -> Vec<String> {
    if doc["ok"] != true {
        return vec![format!(
            "the run failed: {} ({})",
            doc["error"]["message"].as_str().unwrap_or("no message"),
            doc["error"]["code"].as_str().unwrap_or("no code")
        )];
    }
    let r = &doc["result"];
    let f = |v: &Value| v.as_f64();
    let judged = judged(r);
    let mut why = ui_gates(r, judged);
    if undrawn(judged) {
        return why;
    }
    // A locked pair's drops count once a pair and its frames shown once
    // a lens: the share is of pairs, as the overlay's.
    let (lenses, unit) = per_lens(r);
    let shown = f(&judged["frames"]["shown"]).unwrap_or(0.0) / lenses;
    let dropped = f(&judged["frames"]["dropped"]).unwrap_or(0.0);
    if shown + dropped > 0.0 && dropped / (shown + dropped) > MAX_DROPPED {
        why.push(format!(
            "{dropped} of {} {unit} dropped ({:.2}%, over {:.1}%)",
            shown + dropped,
            100.0 * dropped / (shown + dropped),
            MAX_DROPPED * 100.0
        ));
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

/// Why a camera's raw file's run fails as a pair (M3 plan section 9),
/// over the whole run: its lenses didn't lock, a present showed them at
/// different frames, or the backstop resynced them; empty when it
/// passes.
fn judge_pair(doc: &Value) -> Vec<String> {
    let r = &doc["result"];
    let mut why = Vec::new();
    if r["lock"]["status"] != "locked" {
        why.push(format!(
            "the lenses didn't lock: {}",
            match (r["lock"]["status"].as_str(), r["lock"]["field"].as_str()) {
                (Some(status), Some(field)) => format!("{status} on {field}"),
                (Some(status), None) => status.into(),
                _ => "no lock in the document".into(),
            }
        ));
    }
    let count = |k: &str| r["frames"][k].as_u64();
    match count("unpaired") {
        Some(0) => {}
        Some(n) => why.push(format!(
            "{n} presents showed the lenses at different frames"
        )),
        None => why.push("no unpaired count in the document".into()),
    }
    match count("pair_resyncs") {
        Some(0) => {}
        Some(n) => why.push(format!("the pair resynced {n} times")),
        None => why.push("no pair_resyncs count in the document".into()),
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
                pair: false,
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
        pair: true,
    });
    (stitched, raw)
}

/// What doctor's check says VA-API decodes of HEVC ("HEVC Main", "HEVC
/// Main 10"), on the render node hw-test decodes on.
fn vaapi_hevc() -> Vec<String> {
    let node = tier2::render_node().unwrap_or_else(|| PathBuf::from("/dev/dri/renderD128"));
    doctor::run(&node)
        .decode_checks
        .iter()
        .filter(|c| {
            c.path == "vaapi" && c.result == CheckResult::Works && c.format.starts_with("HEVC")
        })
        .map(|c| c.format.clone())
        .collect()
}

/// The run's process, for [`window`] to run in the session.
fn smoke_cmd(clip: &Path, json: &Path, png: &Path, export: Option<&Export>) -> Command {
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
    cmd
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

    let gates = Gates {
        vaapi_hevc: vaapi_hevc(),
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
                pair: false,
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
        pair: false,
    });
    clips.push(Clip {
        label: "sync flash and beep".into(),
        path: media::write(&media::sync_flash_beep(), &made).unwrap(),
        sync: true,
        pair: false,
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
        let (status, stderr) = window(
            &session,
            smoke_cmd(&clip.path, &json, &png, export.as_ref()),
        );
        let doc = std::fs::read(&json)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let mut why = match &doc {
            Some(d) => {
                let mut why = judge(d, clip.sync, &gates);
                if export.is_some() {
                    why.extend(judge_export(d));
                }
                if clip.pair && d["ok"] == true {
                    why.extend(judge_pair(d));
                }
                why
            }
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
    // Made at the start whatever the run does: the pictures drawn say
    // the zoomed-in timeline showed them.
    if r["drawn"]["thumbnails"].as_u64().unwrap_or(0) == 0 {
        why.push("no thumbnail was drawn".into());
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

/// Which of `gui_decode_behind`'s runs a document is, for
/// [`judge_behind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Behind {
    /// Run 0: the default path, decode held to a frame in
    /// [`DECODE_FLOOR_MS`].
    Floor,
    /// Run 1: VA-API, its frames copied, on the noisy 5.7K 10-bit clip:
    /// behind where VA-API is slow, as on odin.
    VaapiCopied,
    /// Run 2, the control: run 0 with the UI waiting for each decoded
    /// frame.
    Control,
}

impl Behind {
    /// The run's flags beside the smoke run's own.
    fn flags(self) -> Vec<String> {
        let floor = || {
            vec![
                "--smoke-decode-floor-ms".to_string(),
                DECODE_FLOOR_MS.to_string(),
            ]
        };
        match self {
            Behind::Floor => floor(),
            Behind::VaapiCopied => vec!["--smoke-decoder".into(), "vaapi".into()],
            Behind::Control => {
                let mut f = floor();
                f.push("--smoke-control-ui-waits".into());
                f
            }
        }
    }

    /// What the document's `asked` must say.
    fn asked(self) -> Value {
        let floor = f64::from(DECODE_FLOOR_MS);
        match self {
            Behind::Floor => {
                json!({"decoder": null, "decode_floor_ms": floor, "control_ui_waits": false})
            }
            Behind::VaapiCopied => {
                json!({"decoder": "vaapi", "decode_floor_ms": null, "control_ui_waits": false})
            }
            Behind::Control => {
                json!({"decoder": null, "decode_floor_ms": floor, "control_ui_waits": true})
            }
        }
    }
}

/// What [`judge_behind`] makes of a run.
#[derive(Debug, Default)]
struct Verdict {
    /// Why the run fails beside the UI's gates; for the control, why it
    /// didn't run as asked.
    failed: Vec<String>,
    /// Why the run says nothing of a UI with decode behind, which isn't
    /// a failure: VA-API kept up.
    inconclusive: Option<String>,
    /// Why the UI misses its gates ([`ui_gates`]).
    ui: Vec<String>,
}

/// The frames shown of those expected after the first second, per lens
/// for a locked pair as [`judge`] counts them, and their unit.
fn shown_of(r: &Value) -> Option<(f64, f64, &'static str)> {
    let frames = &judged(r)["frames"];
    let (lenses, unit) = per_lens(r);
    let shown = frames["shown"].as_f64()?;
    let expected = frames["expected"].as_f64().filter(|e| *e > 0.0)?;
    Some((shown / lenses, expected / lenses, unit))
}

/// "380 of 570 frames shown (67%)".
fn shown_words(r: &Value) -> Option<String> {
    shown_of(r).map(|(s, e, unit)| format!("{s:.0} of {e:.0} {unit} shown ({:.0}%)", 100.0 * s / e))
}

/// The run in `doc`, `kind` of `gui_decode_behind`'s, by its gates (D40),
/// after the first second: the run must have run as asked, with decode
/// behind (at most [`MAX_SHOWN_BEHIND`] of the frames expected shown),
/// and the UI must hold [`ui_gates`]. Runs 0 and 1 must make no
/// blocking call on the UI thread; the control must make one, since it
/// waits. On run 1 decode must be VA-API's, its frames downloaded (the
/// copy path); and decode keeping up there is inconclusive, not a
/// failure. Drops, decode's own drops and A/V aren't judged: with decode
/// behind, frames are dropped by design.
fn judge_behind(doc: &Value, kind: Behind) -> Verdict {
    let mut v = Verdict::default();
    if doc["ok"] != true {
        v.failed.push(format!(
            "the run failed: {} ({})",
            doc["error"]["message"].as_str().unwrap_or("no message"),
            doc["error"]["code"].as_str().unwrap_or("no code")
        ));
        return v;
    }
    let r = &doc["result"];
    if r["refresh_hz"].as_f64().is_none() {
        v.failed
            .push("the monitor gave no refresh rate, so nothing can be judged".into());
        return v;
    }
    // Numbers as numbers: the document's 50.0 is the flag's 50.
    let a = &r["asked"];
    let want = kind.asked();
    let same = |k: &str| match (a[k].as_f64(), want[k].as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a[k] == want[k],
    };
    if !["decoder", "decode_floor_ms", "control_ui_waits"]
        .into_iter()
        .all(same)
    {
        v.failed
            .push(format!("the document says it was asked {a}, not {want}"));
    }
    match shown_of(r) {
        Some((shown, expected, _)) if shown > MAX_SHOWN_BEHIND * expected => {
            let why = format!("decode kept up: {}", shown_words(r).unwrap_or_default());
            if kind == Behind::VaapiCopied {
                v.inconclusive = Some(why);
            } else {
                v.failed.push(why);
            }
        }
        Some(_) => {}
        None => v
            .failed
            .push("no frames expected in the document, so it can't say decode was behind".into()),
    }
    v.ui = ui_gates(r, judged(r));
    match (kind, r["blocked_on_ui_thread"].as_u64()) {
        (Behind::Control, Some(1..)) => {}
        (Behind::Control, _) => v
            .failed
            .push("the UI never waited: no blocking call on the UI thread".into()),
        (_, Some(0)) => {}
        (_, Some(n)) => v
            .failed
            .push(format!("{n} blocking calls on the UI thread")),
        (_, None) => v.failed.push("no blocking count in the document".into()),
    }
    if kind == Behind::VaapiCopied {
        let decoder = r["streams"][0]["decoder"].as_str();
        if decoder != Some("vaapi") {
            v.failed.push(format!(
                "asked for VA-API, decoded on {}",
                decoder.unwrap_or("nothing")
            ));
        }
        if r["stages"]["download"]["samples"].as_u64().unwrap_or(0) == 0 {
            v.failed
                .push("no frame was downloaded: not the copy path".into());
        }
    }
    v
}

/// Run 0's or 1's verdict as a check: why it fails, the UI's gates with
/// the rest unless decode kept up (inconclusive).
fn run_verdict(v: &Verdict) -> Result<(), Vec<String>> {
    let mut why = v.failed.clone();
    if v.inconclusive.is_none() {
        why.extend(v.ui.iter().cloned());
    }
    if why.is_empty() { Ok(()) } else { Err(why) }
}

/// The UI's gates alone, as a check: the control must fail them.
fn ui_verdict(v: &Verdict) -> Result<(), Vec<String>> {
    if v.ui.is_empty() {
        Ok(())
    } else {
        Err(v.ui.clone())
    }
}

/// Why the control fails the test: it didn't run as asked (so the gates
/// are untested here), it passed the UI's gates, or it failed them but
/// not as a slow UI in a window drawn. Empty when it failed as it must.
fn control_failed(v: &Verdict) -> Vec<String> {
    if !v.failed.is_empty() {
        return vec![format!(
            "the control didn't run as asked, so the gates are untested here: {}",
            v.failed.join("; ")
        )];
    }
    if v.ui.is_empty() {
        return vec!["the control passed the UI gates".into()];
    }
    if !v.ui.iter().any(|w| w.contains("the window drawn")) {
        return vec![format!(
            "the control failed the UI gates, but not as a slow UI in a window drawn: {}",
            v.ui.join("; ")
        )];
    }
    Vec::new()
}

#[test]
#[ignore = "tier2: needs a desktop session (GNOME or Hyprland) on a GPU"]
fn gui_decode_behind() {
    let session = tier2::require_graphical_session();
    let out = std::env::var_os("SPHERECUT_GUI_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("gui-decode-behind-{}", std::process::id()))
        });
    std::fs::create_dir_all(&out).unwrap();
    let locked = tier2::session_locked();
    let summary = |vaapi_hevc: &[String],
                   runs: &[Value],
                   not_run: &[(String, String)],
                   inconclusive: &[(String, String)]| {
        let pairs = |l: &[(String, String)]| {
            l.iter()
                .map(|(l, w)| json!({"label": l, "why": w}))
                .collect::<Vec<_>>()
        };
        let s = json!({
            "session": session.found.describe(),
            "env": session.env.iter().map(|(k, v)| format!("{k}={}", v.to_string_lossy())).collect::<Vec<_>>(),
            "locked": locked,
            "vaapi_hevc": vaapi_hevc,
            "max_shown_behind": MAX_SHOWN_BEHIND,
            "runs": runs,
            "not_run": pairs(not_run),
            "inconclusive": pairs(inconclusive),
        });
        std::fs::write(
            out.join(BEHIND_SUMMARY),
            serde_json::to_string_pretty(&s).unwrap() + "\n",
        )
        .unwrap();
    };
    if locked == Some(true) {
        summary(&[], &[], &[("every run".into(), BEHIND_LOCKED.into())], &[]);
        panic!("{BEHIND_LOCKED}");
    }

    let vaapi_hevc = vaapi_hevc();
    let made = out.join("media");
    std::fs::create_dir_all(&made).unwrap();
    let seam = media::write(
        &Spec {
            name: "gui-behind".into(),
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
    let mut planned: Vec<(u64, &str, PathBuf, Behind)> = vec![(
        0,
        "decode held to 20 fps (default path)",
        seam.clone(),
        Behind::Floor,
    )];
    let mut not_run: Vec<(String, String)> = Vec::new();
    let copied = "VA-API copied, noisy 5.7K 10-bit";
    match tier2::perf_clip(&format!("{PERF_5K7_10BIT}.mp4")) {
        Ok(_) if !vaapi_hevc.iter().any(|p| p == "HEVC Main 10") => not_run.push((
            copied.into(),
            "doctor's check says VA-API doesn't decode HEVC Main 10 here".into(),
        )),
        Ok(path) => planned.push((1, copied, path, Behind::VaapiCopied)),
        Err(why) => not_run.push((copied.into(), why)),
    }
    planned.push((
        2,
        "the control: decode held, the UI waiting for each decoded frame",
        seam,
        Behind::Control,
    ));

    let mut runs = Vec::new();
    let mut failed = Vec::new();
    let mut inconclusive = Vec::new();
    let mut control = None;
    for (n, label, clip, kind) in planned {
        let json = out.join(format!("gui-behind-{n}.json"));
        let png = out.join(format!("gui-behind-{n}.png"));
        let _ = std::fs::remove_file(&json);
        let _ = std::fs::remove_file(&png);
        let role = if kind == Behind::Control {
            "control"
        } else {
            "run"
        };
        let mut asked = vec![kind.flags().join(" ")];
        if kind == Behind::VaapiCopied {
            asked.push("SPHERECUT_ZERO_COPY=0".into());
        }
        let row = |failed: &[String], gates: &[String], behind: Option<String>, shot: bool| {
            json!({
                "n": n,
                "label": label,
                "clip": clip.display().to_string(),
                "asked": asked,
                "role": role,
                "behind": behind,
                "document": json.display().to_string(),
                "screenshot": shot.then(|| png.display().to_string()),
                "failed": failed,
                "gates": gates,
            })
        };
        eprintln!("==> {label}: {}", clip.display());
        if tier2::session_locked() == Some(true) {
            let why = "not run: the screen locked during the runs".to_string();
            failed.push(format!("{label}: {why}"));
            runs.push(row(&[why], &[], None, false));
            continue;
        }
        let mut cmd = smoke_cmd(&clip, &json, &png, None);
        cmd.args(kind.flags());
        match kind {
            Behind::VaapiCopied => {
                cmd.env("SPHERECUT_ZERO_COPY", "0");
            }
            Behind::Floor | Behind::Control => {
                cmd.env_remove("SPHERECUT_ZERO_COPY");
            }
        }
        let (status, stderr) = window(&session, cmd);
        let doc = std::fs::read(&json)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let v = match &doc {
            Some(d) => judge_behind(d, kind),
            None => Verdict {
                failed: vec![format!(
                    "no document (exit {status:?}): {}",
                    stderr.lines().last().unwrap_or("")
                )],
                ..Verdict::default()
            },
        };
        let mut why = match kind {
            Behind::Control => control_failed(&v),
            Behind::Floor | Behind::VaapiCopied => run_verdict(&v).err().unwrap_or_default(),
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
        if let Some(i) = &v.inconclusive {
            inconclusive.push((label.to_string(), i.clone()));
        }
        for w in &why {
            failed.push(format!("{label}: {w}"));
        }
        let behind = doc.as_ref().and_then(|d| shown_words(&d["result"]));
        runs.push(row(&why, &v.ui, behind, png.is_file()));
        if kind == Behind::Control {
            control = Some(v);
        }
    }
    summary(&vaapi_hevc, &runs, &not_run, &inconclusive);
    for (label, why) in &not_run {
        eprintln!("not run: {label}: {why}");
    }
    for (label, why) in &inconclusive {
        eprintln!("inconclusive: {label}: {why}");
    }
    assert!(
        failed.is_empty(),
        "{} of {} runs failed:\n{}",
        runs.iter().filter(|r| r["failed"] != json!([])).count(),
        runs.len(),
        failed.join("\n")
    );
    // The control fails the UI's gates, as a slow UI in a window drawn.
    let v = control.expect("the control ran");
    let why = must_fail("a UI that waits for each decoded frame", ui_verdict(&v));
    assert!(
        why.iter().any(|w| w.contains("the window drawn")),
        "{why:?}"
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
                "drawn": {"clips": 120, "thumbnails": 400, "names": 60, "snap_lines": 0},
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
        &with(&|r| r["drawn"]["thumbnails"] = json!(0)),
        "no thumbnail was drawn",
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

    // A locked pair: 600 pairs shown, 1200 frames of its lenses, and a
    // drop counted once a pair is 1 in 601 pairs, over 0.1%; the same
    // counts unlocked, each stream's, are 1 in 1201 frames, under it.
    let mut pair = good();
    pair["result"]["streams"] = json!([
        {"format": "hevc 3840×3840 yuv420p limited/709", "decoder": "vaapi", "decode": "VA-API"},
        {"format": "hevc 3840×3840 yuv420p limited/709", "decoder": "vaapi", "decode": "VA-API"},
    ]);
    pair["result"]["lock"] = json!({"status": "locked", "field": null});
    pair["result"]["frames"] =
        json!({"shown": 1200, "dropped": 0, "skipped": 600, "unpaired": 0, "pair_resyncs": 0});
    assert!(judge(&pair, false, &gates).is_empty());
    assert!(judge_pair(&pair).is_empty());
    pair["result"]["frames"]["dropped"] = json!(1);
    must_fail(
        "a locked pair dropping 1 in 601 pairs",
        judge(&pair, false, &gates).is_empty(),
    );
    fails(&pair, false, "1 of 601 pairs dropped");
    let mut apart = pair.clone();
    apart["result"]["lock"] = json!({"status": "refused", "field": "slots"});
    let why = judge(&apart, false, &gates);
    assert!(why.is_empty(), "1 in 1201 frames: {why:?}");
    // Skipped frames, by design, aren't drops.
    pair["result"]["frames"]["dropped"] = json!(0);
    pair["result"]["frames"]["skipped"] = json!(1200);
    assert!(judge(&pair, false, &gates).is_empty());
    // The X5 raw view's pair gates, each on a document made to fail it.
    let pair_fails = |d: &Value, has: &str| {
        let why = judge_pair(d);
        assert!(
            why.iter().any(|w| w.contains(has)),
            "{has:?} not in {why:?}"
        );
    };
    let mut unpaired = pair.clone();
    unpaired["result"]["frames"]["unpaired"] = json!(3);
    pair_fails(
        &unpaired,
        "3 presents showed the lenses at different frames",
    );
    let mut resynced = pair.clone();
    resynced["result"]["frames"]["pair_resyncs"] = json!(2);
    pair_fails(&resynced, "the pair resynced 2 times");
    pair_fails(&apart, "the lenses didn't lock: refused on slots");
    let mut unlocked = pair.clone();
    unlocked["result"]["lock"] = Value::Null;
    pair_fails(&unlocked, "no lock in the document");
    unlocked["result"]["frames"] = json!({"shown": 1200, "dropped": 0});
    pair_fails(&unlocked, "no unpaired count");
    pair_fails(&unlocked, "no pair_resyncs count");
}

/// `gui_decode_behind`'s gates on documents made up to pass and to fail
/// each one (Tier 1).
#[test]
fn the_behind_gates_fail_what_they_say() {
    // Decode held to 20 fps of a 30 fps clip on a 60 Hz display: 380 of
    // 570 frames after the first second, the UI on time.
    let floor = || {
        json!({
            "ok": true,
            "result": {
                "refresh_hz": 60.0,
                "asked": {"decoder": null, "decode_floor_ms": 50.0, "control_ui_waits": false},
                "ui": {"presented": 0.99, "p99": 17.2, "expected": 1200.0, "frames": 1188, "passes": 1188},
                "settled": {
                    "ui": {"presented": 0.99, "p99": 17.2, "expected": 1140.0, "frames": 1129, "passes": 1129},
                    "frames": {"shown": 380, "dropped": 190, "decode_dropped": 0, "expected": 570.0},
                },
                "streams": [{"decoder": "vulkan", "decode": "Vulkan Video"}],
                "stages": {"decode": {"samples": 400}, "download": {"samples": 0}},
                "blocked_on_ui_thread": 0,
            },
        })
    };
    let vaapi = || {
        let mut d = floor();
        d["result"]["asked"] =
            json!({"decoder": "vaapi", "decode_floor_ms": null, "control_ui_waits": false});
        d["result"]["streams"][0] = json!({"decoder": "vaapi", "decode": "VA-API"});
        d["result"]["stages"]["download"]["samples"] = json!(300);
        d
    };
    for (d, kind) in [(floor(), Behind::Floor), (vaapi(), Behind::VaapiCopied)] {
        let v = judge_behind(&d, kind);
        assert_eq!(run_verdict(&v), Ok(()), "{kind:?}: {v:?}");
        assert_eq!(v.inconclusive, None);
    }
    // The floor run's flag, as a number: the document's 50.0 is 50.
    let mut whole = floor();
    whole["result"]["asked"]["decode_floor_ms"] = json!(50);
    assert_eq!(run_verdict(&judge_behind(&whole, Behind::Floor)), Ok(()));
    // VA-API keeping up says nothing of the UI with decode behind:
    // inconclusive, not failed, its UI gates aside.
    let mut kept = vaapi();
    kept["result"]["settled"]["frames"]["shown"] = json!(568);
    kept["result"]["settled"]["ui"]["p99"] = json!(50.0);
    let v = judge_behind(&kept, Behind::VaapiCopied);
    assert_eq!(
        v.inconclusive.as_deref(),
        Some("decode kept up: 568 of 570 frames shown (100%)")
    );
    assert_eq!(run_verdict(&v), Ok(()), "{v:?}");

    // Each gate on a document made to fail it.
    let fails = |name: &str, d: &Value, kind: Behind, has: &str| -> Verdict {
        let v = judge_behind(d, kind);
        let why = must_fail(name, run_verdict(&v));
        assert!(
            why.iter().any(|w| w.contains(has)),
            "{name}: {has:?} not in {why:?}"
        );
        v
    };
    let mut slow = floor();
    slow["result"]["settled"]["ui"] =
        json!({"presented": 0.33, "p99": 17.2, "expected": 1140.0, "frames": 376, "passes": 376});
    let v = fails(
        "a UI presenting a third of its frames",
        &slow,
        Behind::Floor,
        "33% of the UI frames expected were presented, the window drawn",
    );
    must_fail("a UI presenting a third of its frames", ui_verdict(&v));
    let mut late = floor();
    late["result"]["settled"]["ui"]["p99"] = json!(50.0);
    let v = fails(
        "a UI p99 of 50 ms",
        &late,
        Behind::Floor,
        "UI p99 50.0 ms, over 1.5 × the 60 Hz period",
    );
    must_fail("a UI p99 of 50 ms", ui_verdict(&v));
    let mut kept_up = floor();
    kept_up["result"]["settled"]["frames"]["shown"] = json!(568);
    let v = fails(
        "a floor run whose decode kept up",
        &kept_up,
        Behind::Floor,
        "decode kept up: 568 of 570 frames shown",
    );
    assert_eq!(v.inconclusive, None);
    let mut no_rate = floor();
    no_rate["result"]["refresh_hz"] = Value::Null;
    fails(
        "a run without a refresh rate",
        &no_rate,
        Behind::Floor,
        "the monitor gave no refresh rate, so nothing can be judged",
    );
    let mut blocked = floor();
    blocked["result"]["blocked_on_ui_thread"] = json!(2);
    fails(
        "a run that blocked the UI thread",
        &blocked,
        Behind::Floor,
        "2 blocking calls on the UI thread",
    );
    let mut on_vulkan = vaapi();
    on_vulkan["result"]["streams"][0]["decoder"] = json!("vulkan");
    fails(
        "a VA-API run decoded on Vulkan",
        &on_vulkan,
        Behind::VaapiCopied,
        "asked for VA-API, decoded on vulkan",
    );
    let mut not_copied = vaapi();
    not_copied["result"]["stages"]["download"]["samples"] = json!(0);
    fails(
        "a VA-API run that downloaded nothing",
        &not_copied,
        Behind::VaapiCopied,
        "no frame was downloaded: not the copy path",
    );
    let mut unasked = floor();
    unasked["result"]["asked"]["decode_floor_ms"] = Value::Null;
    fails(
        "a floor run without its floor",
        &unasked,
        Behind::Floor,
        "the document says it was asked",
    );
    // A document from before the flags has no `asked`.
    let mut old = floor();
    old["result"]["asked"] = Value::Null;
    fails(
        "a document from before the flags",
        &old,
        Behind::Floor,
        "the document says it was asked null",
    );
    fails(
        "a run that failed",
        &json!({"ok": false, "error": {"code": "timed_out", "message": "no frame"}}),
        Behind::Floor,
        "the run failed: no frame (timed_out)",
    );

    // The control: decode held, the UI waiting for each decoded frame,
    // so it presents a frame a decoded frame and blocks each time.
    let waits = || {
        let mut d = floor();
        d["result"]["asked"]["control_ui_waits"] = json!(true);
        d["result"]["settled"]["ui"] = json!({"presented": 0.33, "p99": 50.0, "expected": 1140.0, "frames": 376, "passes": 376});
        d["result"]["blocked_on_ui_thread"] = json!(399);
        d
    };
    let v = judge_behind(&waits(), Behind::Control);
    assert_eq!(control_failed(&v), Vec::<String>::new(), "{v:?}");
    let why = must_fail("a UI that waits for each decoded frame", ui_verdict(&v));
    assert!(
        why.iter().any(|w| w.contains("the window drawn")),
        "{why:?}"
    );
    // A control that holds the gates fails the test; so does one that
    // didn't run as asked, or that failed them as a window not drawn.
    let mut held = waits();
    held["result"]["settled"]["ui"] = floor()["result"]["settled"]["ui"].clone();
    assert_eq!(
        control_failed(&judge_behind(&held, Behind::Control)),
        ["the control passed the UI gates"]
    );
    let mut never_waited = waits();
    never_waited["result"]["blocked_on_ui_thread"] = json!(0);
    let why = control_failed(&judge_behind(&never_waited, Behind::Control));
    assert!(
        why[0].starts_with(
            "the control didn't run as asked, so the gates are untested here: the UI never waited"
        ),
        "{why:?}"
    );
    let mut caught_up = waits();
    caught_up["result"]["settled"]["frames"]["shown"] = json!(568);
    let why = control_failed(&judge_behind(&caught_up, Behind::Control));
    assert!(why[0].contains("decode kept up: 568 of 570"), "{why:?}");
    let mut hidden = waits();
    hidden["result"]["settled"]["ui"] =
        json!({"presented": 0.33, "p99": 50.0, "expected": 1140.0, "frames": 376, "passes": 1130});
    let why = control_failed(&judge_behind(&hidden, Behind::Control));
    assert!(
        why[0].starts_with("the control failed the UI gates, but not as a slow UI"),
        "{why:?}"
    );
    // The control's own flags are asked of it: run 0's document isn't
    // the control's.
    let why = control_failed(&judge_behind(&floor(), Behind::Control));
    assert!(why[0].contains("the document says it was asked"), "{why:?}");
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
