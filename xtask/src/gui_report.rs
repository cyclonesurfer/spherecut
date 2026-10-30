//! hw-test's "GUI smoke run" section (M1 plan section 12, item 5), from
//! what the Tier 2 test `gui_smoke` (`crates/cli/tests/tier2_gui.rs`)
//! leaves in its directory: `gui-smoke.json`, which lists the runs and
//! their verdicts, and each run's `spherecut.smoke/1` document and
//! screenshot. The screenshots are copied next to the report as
//! `<report>-gui-<n>.png` and shown in it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The summary's file name, as the test writes it.
pub const SUMMARY: &str = "gui-smoke.json";

/// The section's Markdown for the runs in `dir`, with the screenshots
/// copied beside `report`. `test` says how the `gui_smoke` test ended,
/// for when there is no summary (it skipped, or didn't run).
pub fn section(dir: &Path, report: &Path, test: &str) -> String {
    let mut r = String::new();
    let summary = std::fs::read(dir.join(SUMMARY))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let Some(s) = summary else {
        let _ = writeln!(r, "No run: `gui_smoke` {test}.\n");
        return r;
    };
    // As logind had it; not known without loginctl.
    let lock = match s["locked"].as_bool() {
        Some(true) => " The screen was locked when the runs began, so none ran.",
        Some(false) => " The screen was unlocked when the runs began.",
        None => "",
    };
    let _ = writeln!(
        r,
        "`gui_smoke` {test}. The session: {} ({}). VA-API decodes {} by doctor's check, so \
         those clips must decode in hardware.{lock} Presented, the UI's intervals and the \
         frames are counted after each run's first second, while the cache fills, as the \
         playback benches count them.\n",
        s["session"].as_str().unwrap_or("?"),
        list(&s["env"]),
        match list(&s["vaapi_hevc"]) {
            l if l.is_empty() => "no HEVC profile".to_string(),
            l => l,
        }
    );
    let runs = s["runs"].as_array().cloned().unwrap_or_default();
    let docs: Vec<Option<Value>> = runs
        .iter()
        .map(|run| {
            let p = run["document"].as_str()?;
            serde_json::from_slice(&std::fs::read(p).ok()?).ok()
        })
        .collect();
    if !runs.is_empty() {
        let _ = writeln!(
            r,
            "| # | Clip | Verdict | Presented | UI p50 / p99 ms | Refresh | Shown / dropped / decode-dropped | Decoder | A/V median / p95 ms | Audio |\n\
             |---:|---|---|---:|---:|---:|---:|---|---:|---|"
        );
    }
    for (run, doc) in runs.iter().zip(&docs) {
        let n = run["n"].as_u64().unwrap_or(0);
        let failed = list_of(&run["failed"]);
        let verdict = if failed.is_empty() {
            "pass".to_string()
        } else {
            format!("**FAILED**: {}", failed.join("; "))
        };
        let label = run["label"].as_str().unwrap_or("?");
        let res = doc.as_ref().map(|d| &d["result"]);
        let cell =
            |f: &dyn Fn(&Value) -> Option<String>| res.and_then(f).unwrap_or_else(|| "-".into());
        // What the gates judge: after the first second, where the run
        // lasted that long.
        let judged = |r: &Value| {
            if r["settled"].is_object() {
                r["settled"].clone()
            } else {
                r.clone()
            }
        };
        let _ = writeln!(
            r,
            "| {n} | {label} | {verdict} | {} | {} | {} | {} | {} | {} | {} |",
            cell(&|r| Some(format!(
                "{:.0}%",
                judged(r)["ui"]["presented"].as_f64()? * 100.0
            ))),
            cell(&|r| {
                let j = judged(r);
                Some(format!("{} / {}", ms(&j["ui"]["p50"]), ms(&j["ui"]["p99"])))
            }),
            cell(&|r| r["refresh_hz"].as_f64().map(|hz| format!("{hz:.2} Hz"))),
            cell(&|r| {
                let j = judged(r);
                Some(format!(
                    "{} / {} / {}",
                    j["frames"]["shown"], j["frames"]["dropped"], j["frames"]["decode_dropped"]
                ))
            }),
            cell(&|r| {
                let ds: Vec<String> = r["streams"]
                    .as_array()?
                    .iter()
                    .map(|st| st["decode"].as_str().unwrap_or("?").to_string())
                    .collect();
                Some(ds.join(", "))
            }),
            cell(&|r| Some(format!(
                "{} / {}",
                ms(&r["av"]["median_ms"]),
                ms(&r["av"]["p95_abs_ms"])
            ))),
            cell(&|r| {
                let a = &r["audio"];
                Some(match a["host"].as_str() {
                    Some(host) => format!(
                        "{host}, {} ms",
                        a["latency_ms"]
                            .as_f64()
                            .map_or("?".into(), |v| format!("{v:.0}"))
                    ),
                    None => "none".into(),
                })
            }),
        );
    }
    if !runs.is_empty() {
        let _ = writeln!(r);
    }
    for (run, doc) in runs.iter().zip(&docs) {
        let n = run["n"].as_u64().unwrap_or(0);
        let label = run["label"].as_str().unwrap_or("?");
        let _ = writeln!(r, "### {n}. {label}\n");
        let _ = writeln!(r, "`{}`\n", run["clip"].as_str().unwrap_or("?"));
        match doc.as_ref().map(|d| &d["result"]).filter(|v| !v.is_null()) {
            Some(res) => {
                let _ = writeln!(
                    r,
                    "{} ({}, {}); {} at {} pixels a point (the window's own {}), the window \
                     {} and the viewer {} pixels; {}; {}{}; played {:.1} s{}.\n",
                    res["adapter"]["name"].as_str().unwrap_or("?"),
                    res["adapter"]["driver"].as_str().unwrap_or("?"),
                    res["adapter"]["driver_info"].as_str().unwrap_or("?"),
                    res["surface_format"].as_str().unwrap_or("?"),
                    res["scale"],
                    res["native_scale"],
                    size(&res["window"]),
                    size(&res["viewer"]),
                    stages(&res["stages"]),
                    passes(&res["ui"]),
                    late(res),
                    res["played_s"].as_f64().unwrap_or(0.0),
                    if res["ended"] == true {
                        ", to the clip's end"
                    } else {
                        ""
                    }
                );
            }
            None => {
                let why = doc
                    .as_ref()
                    .and_then(|d| d["error"]["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "no document".into());
                let _ = writeln!(r, "No measurements: {why}.\n");
            }
        }
        if let Some(png) = run["screenshot"].as_str() {
            match copy_beside(Path::new(png), report, n) {
                Ok(name) => {
                    let _ = writeln!(r, "![{label}]({name})\n");
                }
                Err(e) => {
                    let _ = writeln!(r, "The screenshot couldn't be copied: {e}.\n");
                }
            }
        }
    }
    let not_run = s["not_run"].as_array().cloned().unwrap_or_default();
    if !not_run.is_empty() {
        let _ = writeln!(r, "Not run:\n");
        for nr in not_run {
            let _ = writeln!(
                r,
                "- {}: {}",
                nr["label"].as_str().unwrap_or("?"),
                nr["why"].as_str().unwrap_or("?")
            );
        }
        let _ = writeln!(r);
    }
    r
}

/// `report`'s screenshot `n`: its name beside the report.
pub fn screenshot_name(report: &Path, n: u64) -> String {
    let stem = report
        .file_stem()
        .map_or_else(|| "report".into(), |s| s.to_string_lossy().into_owned());
    format!("{stem}-gui-{n}.png")
}

fn copy_beside(png: &Path, report: &Path, n: u64) -> std::io::Result<String> {
    let name = screenshot_name(report, n);
    let dest: PathBuf = report.with_file_name(&name);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::copy(png, &dest)?;
    Ok(name)
}

fn list(v: &Value) -> String {
    list_of(v).join(", ")
}

fn list_of(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_owned))
        .collect()
}

fn ms(v: &Value) -> String {
    v.as_f64().map_or_else(|| "-".into(), |v| format!("{v:.1}"))
}

fn size(v: &Value) -> String {
    match (v[0].as_u64(), v[1].as_u64()) {
        (Some(w), Some(h)) => format!("{w}×{h}"),
        _ => "?".into(),
    }
}

/// "1188 UI passes of 1190, their own work 2.1 / 3.3 ms": the passes
/// with the UI of all passes, and eframe's count of their work, p50 and
/// p95.
fn passes(ui: &Value) -> String {
    let work = &ui["work"];
    format!(
        "{} UI passes of {}, their own work {}",
        ui["frames"].as_u64().unwrap_or(0),
        ui["passes"].as_u64().unwrap_or(0),
        if work["samples"].as_u64().unwrap_or(0) > 0 {
            format!("{} / {} ms", ms(&work["p50"]), ms(&work["p95"]))
        } else {
            "unmeasured".into()
        }
    )
}

/// "; 3 late UI passes after the first second, the longest 99.9 ms; the
/// slowest in their gaps decode 29.5 ms, upload_gpu 2.1 ms": what ran
/// while the UI waited. Empty without a refresh rate to be late for, or
/// a document from before the list.
fn late(res: &Value) -> String {
    let Some(late) = res["late"].as_array() else {
        return String::new();
    };
    if res["refresh_hz"].is_null() {
        return String::new();
    }
    if late.is_empty() {
        return "; no late UI pass after the first second".into();
    }
    let longest = late
        .iter()
        .filter_map(|l| l["interval_ms"].as_f64())
        .fold(0.0, f64::max);
    let mut slowest = std::collections::BTreeMap::<String, f64>::new();
    for l in late {
        for (k, v) in l["slowest"].as_object().into_iter().flatten() {
            if let Some(v) = v.as_f64() {
                let m = slowest.entry(k.clone()).or_insert(v);
                *m = m.max(v);
            }
        }
    }
    let stages: Vec<String> = slowest
        .iter()
        .map(|(k, v)| format!("{k} {v:.1} ms"))
        .collect();
    format!(
        "; {} late UI pass{} after the first second, the longest {longest:.1} ms{}",
        late.len(),
        if late.len() == 1 { "" } else { "es" },
        if stages.is_empty() {
            String::new()
        } else {
            format!("; the slowest in their gaps {}", stages.join(", "))
        }
    )
}

/// "decode 4.1 / 6.0 ms, upload_cpu ..." for the stages with samples:
/// p50 and p95.
fn stages(v: &Value) -> String {
    let Some(m) = v.as_object() else {
        return "no stages".into();
    };
    let parts: Vec<String> = m
        .iter()
        .filter(|(_, s)| s["samples"].as_u64().unwrap_or(0) > 0)
        .map(|(k, s)| format!("{k} {} / {} ms", ms(&s["p50"]), ms(&s["p95"])))
        .collect();
    if parts.is_empty() {
        "no stages".into()
    } else {
        format!("p50 / p95: {}", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own, removed when dropped.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_section_shows_each_run_and_its_screenshot() {
        let s = Scratch(std::env::temp_dir().join(format!("xtask-gui-{}", std::process::id())));
        let dir = s.0.join("hw-gui");
        std::fs::create_dir_all(&dir).unwrap();
        let doc = serde_json::json!({
            "schema": "spherecut.smoke/1", "ok": true, "warnings": [],
            "result": {
                "adapter": {"name": "AMD Radeon RX 7600 (RADV NAVI33)", "driver": "radv", "driver_info": "Mesa 26.2.5"},
                "surface_format": "Bgra8UnormSrgb", "scale": 1.25, "native_scale": 1.25,
                "window": [1600, 1000], "viewer": [1167, 776], "refresh_hz": 59.95,
                "played_s": 20.0, "ended": false,
                "ui": {"presented": 0.998, "p50": 16.7, "p99": 18.2, "frames": 1188, "passes": 1190,
                       "work": {"samples": 1187, "p50": 2.1, "p95": 3.3}},
                "frames": {"shown": 600, "dropped": 3, "decode_dropped": 1},
                "settled": {"from_s": 1.0,
                            "ui": {"presented": 0.999, "p50": 16.7, "p99": 17.9},
                            "frames": {"shown": 570, "dropped": 0, "decode_dropped": 0}},
                "streams": [{"decode": "VA-API", "decoder": "vaapi"}],
                "av": {"median_ms": 2.0, "p95_abs_ms": 6.5},
                "audio": {"host": "PulseAudio", "latency_ms": 41.2},
                "stages": {"decode": {"samples": 600, "p50": 4.1, "p95": 6.0}, "convert": {"samples": 0}},
                "late": [
                    {"at_s": 3.2, "interval_ms": 33.4, "work_ms": 1.1, "slowest": {"decode": 12.0}},
                    {"at_s": 9.8, "interval_ms": 99.9, "work_ms": null,
                     "slowest": {"decode": 29.5, "upload_gpu": 2.1}},
                ],
            }
        });
        std::fs::write(dir.join("gui-0.json"), doc.to_string()).unwrap();
        std::fs::write(dir.join("gui-0.png"), b"png").unwrap();
        std::fs::write(
            dir.join("gui-1.json"),
            r#"{"schema":"spherecut.smoke/1","ok":false,"error":{"code":"timed_out","message":"no frame"},"warnings":[]}"#,
        )
        .unwrap();
        let summary = serde_json::json!({
            "session": "Wayland socket discovered",
            "env": ["WAYLAND_DISPLAY=wayland-0"],
            "locked": false,
            "vaapi_hevc": ["HEVC Main", "HEVC Main 10"],
            "runs": [
                {"n": 0, "label": "noisy 5.7K 8-bit", "clip": "/m/a.mp4",
                 "document": dir.join("gui-0.json"), "screenshot": dir.join("gui-0.png"), "failed": []},
                {"n": 1, "label": "sync flash and beep", "clip": "/m/s.mp4",
                 "document": dir.join("gui-1.json"), "screenshot": null,
                 "failed": ["the run failed: no frame (timed_out)"]},
            ],
            "not_run": [{"label": "X5 raw view", "why": "no samples directory"}],
        });
        std::fs::write(dir.join(SUMMARY), summary.to_string()).unwrap();
        let report = s.0.join("hw-reports").join("odin-abc.md");
        let md = section(&dir, &report, "passed");
        eprintln!("{md}");
        assert!(md.contains("The session: Wayland socket discovered (WAYLAND_DISPLAY=wayland-0)"));
        assert!(md.contains(
            "VA-API decodes HEVC Main, HEVC Main 10 by doctor's check, so those clips must \
             decode in hardware. The screen was unlocked when the runs began. Presented, the \
             UI's intervals and the frames are counted after each run's first second"
        ));
        assert!(md.contains(
            "| 0 | noisy 5.7K 8-bit | pass | 100% | 16.7 / 17.9 | 59.95 Hz | 570 / 0 / 0 | VA-API | 2.0 / 6.5 | PulseAudio, 41 ms |"
        ));
        assert!(md.contains(
            "| 1 | sync flash and beep | **FAILED**: the run failed: no frame (timed_out) | - |"
        ));
        assert!(md.contains(
            "AMD Radeon RX 7600 (RADV NAVI33) (radv, Mesa 26.2.5); Bgra8UnormSrgb at 1.25"
        ));
        assert!(md.contains(
            "p50 / p95: decode 4.1 / 6.0 ms; 1188 UI passes of 1190, their own work 2.1 / 3.3 ms; \
             2 late UI passes after the first second, the longest 99.9 ms; the slowest in their \
             gaps decode 29.5 ms, upload_gpu 2.1 ms; played 20.0 s."
        ));
        assert!(!md.contains("convert"));
        assert!(md.contains("![noisy 5.7K 8-bit](odin-abc-gui-0.png)"));
        assert_eq!(
            std::fs::read(s.0.join("hw-reports").join("odin-abc-gui-0.png")).unwrap(),
            b"png"
        );
        assert!(md.contains("No measurements: no frame."));
        assert!(md.contains("- X5 raw view: no samples directory"));
        // Locked from the start: nothing ran, and the report says why.
        std::fs::write(
            dir.join(SUMMARY),
            serde_json::json!({
                "session": "WAYLAND_DISPLAY set", "env": ["WAYLAND_DISPLAY=wayland-0"],
                "locked": true, "vaapi_hevc": [], "runs": [],
                "not_run": [{"label": "every run", "why": "the screen is locked"}],
            })
            .to_string(),
        )
        .unwrap();
        let locked = section(&dir, &report, "failed");
        assert!(locked.contains("no HEVC profile by doctor's check"));
        assert!(locked.contains("The screen was locked when the runs began, so none ran."));
        assert!(locked.contains("- every run: the screen is locked"));
        assert!(!locked.contains("| # |"));
        // No summary: why, from the test's outcome.
        let none = section(
            &s.0.join("nothing"),
            &report,
            "skipped: no graphical session",
        );
        assert_eq!(
            none,
            "No run: `gui_smoke` skipped: no graphical session.\n\n"
        );
    }
}
