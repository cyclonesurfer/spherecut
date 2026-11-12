//! hw-test's "GUI smoke run" section (M1 plan section 12, item 5), from
//! what the Tier 2 test `gui_smoke` (`crates/cli/tests/tier2_gui.rs`)
//! leaves in its directory: `gui-smoke.json`, which lists the runs and
//! their verdicts, and each run's `spherecut.smoke/1` document and
//! screenshot. The screenshots are copied next to the report as
//! `<report>-gui-<n>.png` and shown in it. Its "UI while decode falls
//! behind" section (D40) is the same from `gui_decode_behind`'s
//! `gui-decode-behind.json`, the screenshots as
//! `<report>-behind-<n>.png`, with the control's row read as a control.
//! Its "Timeline prototype" section (M2 plan section 12) is the same
//! from `gui_proto`'s `gui-proto.json`, the screenshots as
//! `<report>-proto-<n>.png`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The summaries' file names, as the tests write them.
pub const SUMMARY: &str = "gui-smoke.json";
pub const BEHIND_SUMMARY: &str = "gui-decode-behind.json";
pub const PROTO_SUMMARY: &str = "gui-proto.json";

/// Which smoke runs a section shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `gui_smoke`'s: each clip played on the app's own path.
    Smoke,
    /// `gui_decode_behind`'s (D40): the UI with decode behind, and its
    /// control.
    Behind,
}

impl Kind {
    /// The test, its summary's file name, and its screenshots' kind
    /// beside the report.
    fn test(self) -> &'static str {
        match self {
            Kind::Smoke => "gui_smoke",
            Kind::Behind => "gui_decode_behind",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Kind::Smoke => SUMMARY,
            Kind::Behind => BEHIND_SUMMARY,
        }
    }

    fn shots(self) -> &'static str {
        match self {
            Kind::Smoke => "gui",
            Kind::Behind => "behind",
        }
    }
}

/// The "GUI smoke run" section's Markdown for the runs in `dir`, with
/// the screenshots copied beside `report`. `test` says how the
/// `gui_smoke` test ended, for when there is no summary (it skipped, or
/// didn't run).
pub fn section(dir: &Path, report: &Path, test: &str) -> String {
    section_of(dir, report, test, Kind::Smoke)
}

/// The "UI while decode falls behind" section's, from
/// `gui_decode_behind`'s runs in `dir`, as [`section`].
pub fn behind_section(dir: &Path, report: &Path, test: &str) -> String {
    section_of(dir, report, test, Kind::Behind)
}

/// A section of `kind`'s smoke runs in `dir`, the screenshots copied
/// beside `report` as `<report>-<kind>-<n>.png`.
pub fn section_of(dir: &Path, report: &Path, test: &str, kind: Kind) -> String {
    let mut r = String::new();
    let summary = std::fs::read(dir.join(kind.summary()))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let Some(s) = summary else {
        let _ = writeln!(r, "No run: `{}` {test}.\n", kind.test());
        return r;
    };
    // As logind had it; not known without loginctl.
    let lock = match s["locked"].as_bool() {
        Some(true) => " The screen was locked when the runs began, so none ran.",
        Some(false) => " The screen was unlocked when the runs began.",
        None => "",
    };
    let vaapi = match list(&s["vaapi_hevc"]) {
        l if l.is_empty() => "no HEVC profile".to_string(),
        l => l,
    };
    let _ = match kind {
        Kind::Smoke => writeln!(
            r,
            "`gui_smoke` {test}. The session: {} ({}). VA-API decodes {vaapi} by doctor's \
             check, so those clips must decode in hardware.{lock} Presented, the UI's \
             intervals and the frames are counted after each run's first second, while the \
             cache fills, as the playback benches count them.\n",
            s["session"].as_str().unwrap_or("?"),
            list(&s["env"]),
        ),
        Kind::Behind => writeln!(
            r,
            "`gui_decode_behind` {test}. The session: {} ({}). VA-API decodes {vaapi} by \
             doctor's check.{lock} Each run plays with decode behind (at most {:.0}% of the \
             frames expected shown) and must present 90% of the UI frames expected, with a UI \
             p99 of at most 1.5 refresh periods and no blocking call on the UI thread (D40); \
             the control, a UI that waits for each decoded frame, must fail those gates. \
             Dropped frames aren't judged: with decode behind they are dropped by design. \
             Counted after each run's first second.\n",
            s["session"].as_str().unwrap_or("?"),
            list(&s["env"]),
            s["max_shown_behind"].as_f64().unwrap_or(0.85) * 100.0,
        ),
    };
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
        let label = run["label"].as_str().unwrap_or("?");
        let verdict = verdict(run, &s["inconclusive"]);
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
        if kind == Kind::Behind {
            let asked = list(&run["asked"]);
            let _ = writeln!(
                r,
                "Run with {}; {}.\n",
                if asked.is_empty() {
                    "no flag".to_string()
                } else {
                    format!("`{}`", asked.replace(", ", "`, `"))
                },
                match run["behind"].as_str() {
                    Some(b) => format!("after the first second {b}"),
                    None => "nothing shown to judge decode by".into(),
                }
            );
        }
        match doc.as_ref().map(|d| &d["result"]).filter(|v| !v.is_null()) {
            Some(res) => {
                let _ = writeln!(
                    r,
                    "{} ({}, {}); {} at {} pixels a point (the window's own {}), the window \
                     {} and the viewer {} pixels; {}; {}{}{}; played {:.1} s{}.\n",
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
                    gate(&res["submit_gate"]),
                    res["played_s"].as_f64().unwrap_or(0.0),
                    if res["ended"] == true {
                        ", to the clip's end"
                    } else {
                        ""
                    }
                );
                for line in [drain(res), probe(&res["probe"])].into_iter().flatten() {
                    let _ = writeln!(r, "{line}\n");
                }
                if let Some(e) = export(&res["export"]) {
                    let _ = writeln!(r, "{e}\n");
                }
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
            match copy_named(Path::new(png), report, &named(report, kind.shots(), n)) {
                Ok(name) => {
                    let _ = writeln!(r, "![{label}]({name})\n");
                }
                Err(e) => {
                    let _ = writeln!(r, "The screenshot couldn't be copied: {e}.\n");
                }
            }
        }
    }
    // Runs that say nothing of what they were for, as those not run.
    for (key, title) in [("not_run", "Not run"), ("inconclusive", "Inconclusive")] {
        let listed = s[key].as_array().cloned().unwrap_or_default();
        if listed.is_empty() {
            continue;
        }
        let _ = writeln!(r, "{title}:\n");
        for nr in listed {
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

/// A run's verdict cell: a control (`gui_decode_behind`'s) with nothing
/// failed failed the gates as it must, and says which; a run listed in
/// `inconclusive` with nothing failed is neither a pass nor a failure.
fn verdict(run: &Value, inconclusive: &Value) -> String {
    let failed = list_of(&run["failed"]);
    if !failed.is_empty() {
        return format!("**FAILED**: {}", failed.join("; "));
    }
    if run["role"] == "control" {
        return format!(
            "control, failed as it must: {}",
            list_of(&run["gates"]).join("; ")
        );
    }
    let listed = inconclusive
        .as_array()
        .into_iter()
        .flatten()
        .any(|i| i["label"] == run["label"]);
    if listed {
        "inconclusive".to_string()
    } else {
        "pass".to_string()
    }
}

/// The section's Markdown for the prototype's runs in `dir`, with the
/// screenshots copied beside `report`. `test` says how `gui_proto`
/// ended.
pub fn proto_section(dir: &Path, report: &Path, test: &str) -> String {
    let mut r = String::new();
    let summary = std::fs::read(dir.join(PROTO_SUMMARY))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let Some(s) = summary else {
        let _ = writeln!(r, "No run: `gui_proto` {test}.\n");
        return r;
    };
    let lock = match s["locked"].as_bool() {
        Some(true) => " The screen was locked when the runs began, so none ran.",
        Some(false) => " The screen was unlocked when the runs began.",
        None => "",
    };
    let _ = writeln!(
        r,
        "`gui_proto` {test}. The session: {} ({}).{lock} Each run plays the prototype's \
         script (zoom, drag, ripple trim, pan; 500 clips on 6 tracks) for {} s; after its first \
         second it must present 99% of the frames, each pass's own work must have a p99 under \
         the refresh period and the interval between passes a p99 of at most 1.5 periods, and \
         every action must land.\n",
        s["session"].as_str().unwrap_or("?"),
        list(&s["env"]),
        s["seconds"]
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
            "| # | Run | Verdict | Pixels a point | Presented | Late | Interval p50 / p99 ms | Work p50 / p99 ms | Building p99 ms | Refresh |\n\
             |---:|---|---|---:|---:|---:|---:|---:|---:|---:|"
        );
    }
    for (run, doc) in runs.iter().zip(&docs) {
        let failed = list_of(&run["failed"]);
        let verdict = if failed.is_empty() {
            "pass".to_string()
        } else {
            format!("**FAILED**: {}", failed.join("; "))
        };
        let res = doc.as_ref().map(|d| &d["result"]);
        let cell =
            |f: &dyn Fn(&Value) -> Option<String>| res.and_then(f).unwrap_or_else(|| "-".into());
        let _ = writeln!(
            r,
            "| {} | {} | {verdict} | {} | {} | {} | {} | {} | {} | {} |",
            run["n"],
            run["label"].as_str().unwrap_or("?"),
            cell(&|r| r["scale"].as_f64().map(|v| format!("{v}"))),
            cell(&|r| Some(format!("{:.1}%", r["ui"]["presented"].as_f64()? * 100.0))),
            cell(&|r| Some(r["ui"]["late"].to_string())),
            cell(&|r| Some(format!(
                "{} / {}",
                ms(&r["ui"]["interval_ms"]["p50"]),
                ms(&r["ui"]["interval_ms"]["p99"])
            ))),
            cell(&|r| Some(format!(
                "{} / {}",
                ms(&r["ui"]["work_ms"]["p50"]),
                ms(&r["ui"]["work_ms"]["p99"])
            ))),
            cell(&|r| Some(ms(&r["ui"]["build_ms"]["p99"]))),
            cell(&|r| r["refresh_hz"].as_f64().map(|hz| format!("{hz:.2} Hz"))),
        );
    }
    if !runs.is_empty() {
        let _ = writeln!(r);
    }
    for (run, doc) in runs.iter().zip(&docs) {
        let n = run["n"].as_u64().unwrap_or(0);
        let label = run["label"].as_str().unwrap_or("?");
        let _ = writeln!(r, "### Prototype {n}. {label}\n");
        match doc.as_ref().map(|d| &d["result"]).filter(|v| !v.is_null()) {
            Some(res) => {
                let (d, a) = (&res["drawn"], &res["actions"]);
                let _ = writeln!(
                    r,
                    "{} ({}, {}); {} at {} pixels a point (the window's own {}, egui's zoom {}), \
                     the window {} pixels; {} UI passes of {}; at most {} clips, {} thumbnails, \
                     {} names and {} snap lines in a pass; {} moves ({} snapped), {} trims, {} \
                     passes zooming and {} panning, from {} to {} s on screen; played {:.1} s.\n",
                    res["adapter"]["name"].as_str().unwrap_or("?"),
                    res["adapter"]["driver"].as_str().unwrap_or("?"),
                    res["adapter"]["driver_info"].as_str().unwrap_or("?"),
                    res["surface_format"].as_str().unwrap_or("?"),
                    res["scale"],
                    res["native_scale"],
                    res["zoom"],
                    size(&res["window"]),
                    res["ui"]["frames"],
                    res["ui"]["passes"],
                    d["clips"],
                    d["thumbnails"],
                    d["names"],
                    d["snap_lines"],
                    a["moves"],
                    a["snaps"],
                    a["trims"],
                    a["zoom_passes"],
                    a["pan_passes"],
                    res["span_s"][0]
                        .as_f64()
                        .map_or("?".into(), |v| format!("{v:.1}")),
                    res["span_s"][1]
                        .as_f64()
                        .map_or("?".into(), |v| format!("{v:.0}")),
                    res["played_s"].as_f64().unwrap_or(0.0),
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
            match copy_named(Path::new(png), report, &named(report, "proto", n)) {
                Ok(name) => {
                    let _ = writeln!(r, "![{label}]({name})\n");
                }
                Err(e) => {
                    let _ = writeln!(r, "The screenshot couldn't be copied: {e}.\n");
                }
            }
        }
    }
    for nr in s["not_run"].as_array().into_iter().flatten() {
        let _ = writeln!(
            r,
            "Not run: {}: {}\n",
            nr["label"].as_str().unwrap_or("?"),
            nr["why"].as_str().unwrap_or("?")
        );
    }
    r
}

/// `<report>-<kind>-<n>.png`: `gui`, `behind` and `proto` screenshots
/// never share a name.
fn named(report: &Path, kind: &str, n: u64) -> String {
    let stem = report
        .file_stem()
        .map_or_else(|| "report".into(), |s| s.to_string_lossy().into_owned());
    format!("{stem}-{kind}-{n}.png")
}

fn copy_named(png: &Path, report: &Path, name: &str) -> std::io::Result<String> {
    let name = name.to_string();
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

/// "; the submit gate held 412 submissions back, the longest 2.1 ms":
/// what waited for the UI's presents (D41). Empty for a document from
/// before the gate.
fn gate(g: &Value) -> String {
    let Some(waits) = g["waits"].as_u64() else {
        return String::new();
    };
    format!(
        "; the submit gate held {waits} submission{} back, the longest {} ms",
        if waits == 1 { "" } else { "s" },
        ms(&g["longest_ms"])
    )
}

/// "The VA-API drain (`download`): 380 frames drained, p50 / p95 / max
/// 0.4 / 2.1 / 9.1 ms each, waiting for p50 2 and at most 12 surfaces
/// not yet ready; 0 failed syncs, 1 surface forgotten past the 64 a
/// decoder keeps.": what the copy path's drain did over the run. None
/// with the drain off, or for a document from before it.
fn drain(res: &Value) -> Option<String> {
    let mode = res["vaapi_drain"].as_str().filter(|m| *m != "off")?;
    let st = &res["stages"]["drain"];
    let d = &res["drain"];
    let w = &d["waited_per_download"];
    let count = |v: &Value| v.as_u64().map_or_else(|| "-".into(), |n| n.to_string());
    let frames = st["samples"].as_u64().unwrap_or(0);
    let drained = if frames > 0 {
        format!(
            "{frames} frame{} drained, p50 / p95 / max {} / {} / {} ms each, waiting for p50 {} \
             and at most {} surfaces not yet ready",
            plural(frames),
            ms(&st["p50"]),
            ms(&st["p95"]),
            ms(&st["max"]),
            count(&w["p50"]),
            count(&w["max"])
        )
    } else {
        "no frame drained".into()
    };
    let (errors, overflow) = (
        d["errors"].as_u64().unwrap_or(0),
        d["overflow"].as_u64().unwrap_or(0),
    );
    let unavailable = match d["unavailable"].as_u64() {
        Some(n) if n > 0 => format!(
            "; {n} decoder{} found no vaSyncSurface to call, so drained nothing",
            plural(n)
        ),
        _ => String::new(),
    };
    Some(format!(
        "The VA-API drain (`{mode}`): {drained}; {errors} failed sync{}, {overflow} surface{} \
         forgotten past the 64 a decoder keeps{unavailable}.",
        plural(errors),
        plural(overflow)
    ))
}

/// How many of the gate probe's slow jobs [`probe`] lists; the document
/// has them all.
const PROBE_LISTED: usize = 12;

/// "The gate probe, a job on the viewer's queue every 4 ms: 4980 timed
/// while it played, p50 / p99 / max 0.6 / 3.1 / 53.0 ms; 2 slower than
/// 8 ms, at 1.00 s (40.0 ms), 2.00 s (41.0 ms).": the probe's jobs and
/// when the slow ones were submitted, in seconds into the run as the
/// late passes' are, the first [`PROBE_LISTED`]. None for a run without
/// the probe.
fn probe(p: &Value) -> Option<String> {
    if !p.is_object() {
        return None;
    }
    let over = p["over_8ms"].as_array().cloned().unwrap_or_default();
    let slow = over.len() as u64 + p["over_8ms_dropped"].as_u64().unwrap_or(0);
    let listed: Vec<String> = over
        .iter()
        .take(PROBE_LISTED)
        .filter_map(|j| {
            Some(format!(
                "{:.2} s ({:.1} ms)",
                j[0].as_f64()?,
                j[1].as_f64()?
            ))
        })
        .collect();
    let slow = match slow {
        0 => "none slower than 8 ms".to_string(),
        n => format!(
            "{n} slower than 8 ms, at {}{}",
            listed.join(", "),
            match n.saturating_sub(listed.len() as u64) {
                0 => String::new(),
                more => format!(" and {more} more"),
            }
        ),
    };
    let unfinished = match p["unfinished"].as_u64() {
        Some(n) if n > 0 => format!("; {n} not done a second after it stopped"),
        _ => String::new(),
    };
    Some(format!(
        "The gate probe, a job on the viewer's queue every {} ms: {} timed while it played, p50 \
         / p99 / max {} / {} / {} ms; {slow}{unfinished}.",
        p["period_ms"].as_f64().unwrap_or(0.0),
        p["n"].as_u64().unwrap_or(0),
        ms(&p["p50"]),
        ms(&p["p99"]),
        ms(&p["max"])
    ))
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The export a run made while it played (`--smoke-export`, M2.7): what
/// it made, how long each stage was busy, how often it waited for the
/// UI, and the UI over the time it ran. None for a run without one.
fn export(e: &Value) -> Option<String> {
    if !e.is_object() {
        return None;
    }
    let what = format!(
        "{}{}",
        e["preset"].as_str().unwrap_or("the project's output"),
        if e["motion_blur"] == true {
            ", with motion blur"
        } else {
            ""
        }
    );
    if e["ok"] != true {
        return Some(format!(
            "The export while it played ({what}) failed: {}.",
            e["error"].as_str().unwrap_or("no reason given")
        ));
    }
    let stages = e["stages_s"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some(format!("{k} {:.1} s", v.as_f64()?)))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let w = &e["while_running"];
    let ui = if w.is_object() {
        format!(
            "; while it ran and the viewer played {:.0}% of the UI frames were presented, UI \
             p50 / p99 {} / {} ms, {} frames shown and {} dropped",
            w["ui"]["presented"].as_f64().unwrap_or(0.0) * 100.0,
            ms(&w["ui"]["p50"]),
            ms(&w["ui"]["p99"]),
            w["frames"]["shown"],
            w["frames"]["dropped"]
        )
    } else {
        String::new()
    };
    let device = match e["device"].as_str() {
        Some("own") => " on a device of its own",
        Some("window") => " on the window's device",
        _ => "",
    };
    // A document from before D56's lead has no yields.
    let yielded = match e["yielded"].as_u64() {
        Some(n) => format!(
            "; its stages waited {n} time{} for the viewer's frames, the longest {} ms",
            if n == 1 { "" } else { "s" },
            ms(&e["longest_yield_ms"])
        ),
        None => String::new(),
    };
    // Nor from before it stood aside while the viewer played.
    let aside = match e["aside"].as_u64() {
        Some(n) => format!(
            "; its stages stood aside {n} time{} while the viewer played, {} ms in all{}",
            if n == 1 { "" } else { "s" },
            ms(&e["aside_ms"]),
            e["after_playing_s"]
                .as_f64()
                .map(|s| format!(", and it ran {s:.1} s after the viewer stopped"))
                .unwrap_or_default()
        ),
        None => String::new(),
    };
    Some(format!(
        "The export while it played ({what}){device}: {} frames at {} in {:.1} s with {}{}; \
         busy {}; {} frames waited for a UI pass, the longest {} ms{yielded}{aside}{ui}.",
        e["frames"],
        size(&e["size"]),
        e["elapsed_s"].as_f64().unwrap_or(0.0),
        e["encoder"].as_str().unwrap_or("?"),
        e["fallback"]
            .as_str()
            .map(|f| format!(" (not {f})"))
            .unwrap_or_default(),
        if stages.is_empty() {
            "-".into()
        } else {
            stages
        },
        e["held"],
        ms(&e["longest_hold_ms"])
    ))
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
    use spherecut_testkit::control::must_fail;

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
        let mut doc = serde_json::json!({
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
                "submit_gate": {"waits": 412, "longest_ms": 2.06},
            }
        });
        doc["result"]["export"] = serde_json::json!({
            "output": "/o/gui-0-export.mp4", "preset": "360-7680", "motion_blur": true, "device": "own",
            "ok": true, "error": null, "frames": 60, "size": [7680, 3840],
            "encoder": "hevc_vaapi", "fallback": null, "elapsed_s": 12.34,
            "stages_s": {"decode": 1.0, "encode": 3.31, "finish": 0.5, "readback": 2.0, "render": 9.0},
            "blur_capped": 0, "held": 58, "longest_hold_ms": 17.31,
            "yielded": 7, "longest_yield_ms": 41.31, "aside": 30, "aside_ms": 912.04, "after_playing_s": 3.2,
            "while_running": {"from_s": 1.2, "to_s": 13.5,
                "ui": {"presented": 0.95, "p50": 16.7, "p99": 24.5},
                "frames": {"shown": 360, "dropped": 2, "decode_dropped": 0}},
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
             gaps decode 29.5 ms, upload_gpu 2.1 ms; the submit gate held 412 submissions \
             back, the longest 2.1 ms; played 20.0 s."
        ));
        assert!(!md.contains("convert"));
        assert!(md.contains(
            "The export while it played (360-7680, with motion blur) on a device of its own: 60 \
             frames at 7680×3840 in 12.3 s with hevc_vaapi; busy decode 1.0 s, encode 3.3 s, finish 0.5 s, readback 2.0 \
             s, render 9.0 s; 58 frames waited for a UI pass, the longest 17.3 ms; its stages \
             waited 7 times for the viewer's frames, the longest 41.3 ms; its stages stood \
             aside 30 times while the viewer played, 912.0 ms in all, and it ran 3.2 s after \
             the viewer stopped; while it ran and the viewer played 95% of the UI frames were \
             presented, UI p50 / p99 16.7 / 24.5 ms, 360 frames shown and 2 dropped."
        ));
        let failed = serde_json::json!({"preset": null, "ok": false, "error": "no encoder"});
        assert_eq!(
            export(&failed).unwrap(),
            "The export while it played (the project's output) failed: no encoder."
        );
        assert_eq!(export(&Value::Null), None);
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

    #[test]
    fn the_behind_section_shows_each_run_and_the_control() {
        let s = Scratch(std::env::temp_dir().join(format!("xtask-behind-{}", std::process::id())));
        let dir = s.0.join("hw-gui");
        std::fs::create_dir_all(&dir).unwrap();
        let doc = |asked: Value, presented: f64, p99: f64, shown: u64| {
            serde_json::json!({
                "schema": "spherecut.smoke/1", "ok": true, "warnings": [],
                "result": {
                    "adapter": {"name": "AMD Radeon RX 7600 (RADV NAVI33)", "driver": "radv", "driver_info": "Mesa 26.2.5"},
                    "surface_format": "Bgra8UnormSrgb", "scale": 1.0, "native_scale": 1.0,
                    "window": [1280, 800], "viewer": [900, 600], "refresh_hz": 60.0,
                    "played_s": 20.0, "ended": false, "asked": asked,
                    "ui": {"presented": presented, "p50": 16.7, "p99": p99, "frames": 1188, "passes": 1188},
                    "settled": {"from_s": 1.0,
                                "ui": {"presented": presented, "p50": 16.7, "p99": p99},
                                "frames": {"shown": shown, "dropped": 190, "decode_dropped": 0, "expected": 570.0}},
                    "streams": [{"decode": "Vulkan Video", "decoder": "vulkan"}],
                    "stages": {"decode": {"samples": 400, "p50": 1.2, "p95": 2.0}},
                    "blocked_on_ui_thread": 0,
                }
            })
        };
        let floor = serde_json::json!({"decoder": null, "decode_floor_ms": 50.0, "control_ui_waits": false});
        std::fs::write(
            dir.join("gui-behind-0.json"),
            doc(floor, 0.99, 17.2, 380).to_string(),
        )
        .unwrap();
        // A document from before the flags: no `asked`.
        let mut old = doc(Value::Null, 0.99, 17.4, 568);
        old["result"].as_object_mut().unwrap().remove("asked");
        std::fs::write(dir.join("gui-behind-1.json"), old.to_string()).unwrap();
        let waits =
            serde_json::json!({"decoder": null, "decode_floor_ms": 50.0, "control_ui_waits": true});
        std::fs::write(
            dir.join("gui-behind-2.json"),
            doc(waits, 0.33, 50.0, 376).to_string(),
        )
        .unwrap();
        for n in 0..3 {
            std::fs::write(dir.join(format!("gui-behind-{n}.png")), b"behind").unwrap();
        }
        let slow = "33% of the UI frames expected were presented, the window drawn (0 passes \
                    without the UI): the UI was slow, each pass's own work unmeasured ms at p95";
        let run = |n: u64,
                   label: &str,
                   asked: &[&str],
                   role: &str,
                   behind: &str,
                   failed: Value,
                   gates: Value| {
            serde_json::json!({
                "n": n, "label": label, "clip": "/o/media/gui-behind.mp4", "asked": asked, "role": role,
                "behind": behind, "document": dir.join(format!("gui-behind-{n}.json")),
                "screenshot": dir.join(format!("gui-behind-{n}.png")), "failed": failed, "gates": gates,
            })
        };
        let summary = |control_failed: Value| {
            serde_json::json!({
                "session": "Wayland socket discovered",
                "env": ["WAYLAND_DISPLAY=wayland-0"],
                "locked": false,
                "vaapi_hevc": ["HEVC Main", "HEVC Main 10"],
                "max_shown_behind": 0.85,
                "runs": [
                    run(0, "decode held to 20 fps (default path)", &["--smoke-decode-floor-ms 50"], "run",
                        "380 of 570 frames shown (67%)", serde_json::json!([]), serde_json::json!([])),
                    run(1, "VA-API copied, noisy 5.7K 10-bit", &["--smoke-decoder vaapi", "SPHERECUT_ZERO_COPY=0"], "run",
                        "568 of 570 frames shown (100%)", serde_json::json!([]), serde_json::json!([])),
                    run(2, "the control", &["--smoke-decode-floor-ms 50 --smoke-control-ui-waits"], "control",
                        "376 of 570 frames shown (66%)", control_failed, serde_json::json!([slow, "UI p99 50.0 ms, over 1.5 × the 60 Hz period (25.0 ms)"])),
                ],
                "not_run": [],
                "inconclusive": [{"label": "VA-API copied, noisy 5.7K 10-bit", "why": "decode kept up: 568 of 570 frames shown (100%)"}],
            })
        };
        std::fs::write(
            dir.join(BEHIND_SUMMARY),
            summary(serde_json::json!([])).to_string(),
        )
        .unwrap();
        // gui_smoke's screenshot 0, already beside the report.
        let reports = s.0.join("hw-reports");
        std::fs::create_dir_all(&reports).unwrap();
        std::fs::write(reports.join("odin-abc-gui-0.png"), b"smoke").unwrap();
        let report = reports.join("odin-abc.md");
        let md = behind_section(&dir, &report, "passed");
        eprintln!("{md}");
        assert!(md.contains(
            "`gui_decode_behind` passed. The session: Wayland socket discovered \
             (WAYLAND_DISPLAY=wayland-0). VA-API decodes HEVC Main, HEVC Main 10 by doctor's \
             check. The screen was unlocked when the runs began. Each run plays with decode \
             behind (at most 85% of the frames expected shown)"
        ));
        assert!(md.contains(
            "| 0 | decode held to 20 fps (default path) | pass | 99% | 16.7 / 17.2 | 60.00 Hz | 380 / 190 / 0 | Vulkan Video |"
        ));
        // The old document renders as any other.
        assert!(md.contains(
            "| 1 | VA-API copied, noisy 5.7K 10-bit | inconclusive | 99% | 16.7 / 17.4 | 60.00 Hz | 568 / 190 / 0 |"
        ));
        let reads_as_control =
            |md: &str| md.contains("| 2 | the control | control, failed as it must: ");
        assert!(reads_as_control(&md));
        assert!(md.contains(&format!(
            "| 2 | the control | control, failed as it must: {slow}; UI p99 50.0 ms, over 1.5 × \
             the 60 Hz period (25.0 ms) | 33% |"
        )));
        assert!(md.contains(
            "Run with `--smoke-decoder vaapi`, `SPHERECUT_ZERO_COPY=0`; after the first second \
             568 of 570 frames shown (100%)."
        ));
        assert!(md.contains(
            "Inconclusive:\n\n- VA-API copied, noisy 5.7K 10-bit: decode kept up: 568 of 570 \
             frames shown (100%)"
        ));
        assert!(!md.contains("Not run:"));
        // Its screenshots beside gui_smoke's, which stays as it was.
        for n in 0..3 {
            assert!(md.contains(&format!("](odin-abc-behind-{n}.png)")), "{n}");
            assert_eq!(
                std::fs::read(reports.join(format!("odin-abc-behind-{n}.png"))).unwrap(),
                b"behind"
            );
        }
        assert_eq!(
            std::fs::read(reports.join("odin-abc-gui-0.png")).unwrap(),
            b"smoke"
        );
        // The control: a control that passed the gates fails the run.
        std::fs::write(
            dir.join(BEHIND_SUMMARY),
            summary(serde_json::json!(["the control passed the UI gates"])).to_string(),
        )
        .unwrap();
        let md = behind_section(
            &dir,
            &report,
            "**FAILED** (its output is under Tier 2 tests)",
        );
        must_fail("a control that passed the UI gates", reads_as_control(&md));
        assert!(md.contains("| 2 | the control | **FAILED**: the control passed the UI gates |"));
        // No summary: why, from the test's outcome.
        assert_eq!(
            behind_section(&s.0.join("nothing"), &report, "wasn't listed"),
            "No run: `gui_decode_behind` wasn't listed.\n\n"
        );
    }

    #[test]
    fn the_prototype_section_shows_each_run_and_its_screenshot() {
        let s = Scratch(std::env::temp_dir().join(format!("xtask-proto-{}", std::process::id())));
        let dir = s.0.join("hw-gui");
        std::fs::create_dir_all(&dir).unwrap();
        let doc = serde_json::json!({
            "schema": "spherecut.timeline-proto/1", "ok": true, "warnings": [],
            "result": {
                "adapter": {"name": "AMD Radeon RX 7600 (RADV NAVI33)", "driver": "radv", "driver_info": "Mesa 26.2.5"},
                "surface_format": "Bgra8UnormSrgb", "scale": 1.25, "native_scale": 1.0, "zoom": 1.25,
                "window": [1280, 800], "refresh_hz": 59.95, "played_s": 20.02,
                "ui": {"presented": 0.9966, "late": 2, "frames": 1136, "passes": 1136,
                       "interval_ms": {"p50": 16.68, "p99": 17.9},
                       "work_ms": {"p50": 3.1, "p99": 5.04},
                       "build_ms": {"p99": 2.26}},
                "drawn": {"clips": 500, "thumbnails": 812, "names": 260, "snap_lines": 4},
                "actions": {"moves": 2, "snaps": 2, "trims": 2, "zoom_passes": 310, "pan_passes": 168},
                "span_s": [2.0, 777.31],
            }
        });
        std::fs::write(dir.join("gui-proto-0.json"), doc.to_string()).unwrap();
        std::fs::write(dir.join("gui-proto-0.png"), b"png").unwrap();
        let summary = serde_json::json!({
            "session": "Wayland socket discovered",
            "env": ["WAYLAND_DISPLAY=wayland-0"],
            "locked": false,
            "seconds": 20.0,
            "runs": [
                {"n": 0, "label": "egui zoom 1.25", "zoom": 1.25,
                 "document": dir.join("gui-proto-0.json"), "screenshot": dir.join("gui-proto-0.png"), "failed": []},
                {"n": 1, "label": "egui zoom 1.5", "zoom": 1.5,
                 "document": dir.join("gui-proto-1.json"), "screenshot": null,
                 "failed": ["no document (exit None): killed after 120 s"]},
            ],
            "not_run": [],
        });
        std::fs::write(dir.join(PROTO_SUMMARY), summary.to_string()).unwrap();
        let report = s.0.join("hw-reports").join("odin-abc.md");
        let md = proto_section(&dir, &report, "passed");
        eprintln!("{md}");
        assert!(md.contains("`gui_proto` passed. The session: Wayland socket discovered (WAYLAND_DISPLAY=wayland-0). The screen was unlocked"));
        assert!(md.contains("for 20.0 s;"));
        assert!(md.contains(
            "| 0 | egui zoom 1.25 | pass | 1.25 | 99.7% | 2 | 16.7 / 17.9 | 3.1 / 5.0 | 2.3 | 59.95 Hz |"
        ));
        assert!(md.contains(
            "| 1 | egui zoom 1.5 | **FAILED**: no document (exit None): killed after 120 s | - |"
        ));
        assert!(md.contains(
            "(radv, Mesa 26.2.5); Bgra8UnormSrgb at 1.25 pixels a point (the window's own 1.0, \
             egui's zoom 1.25), the window 1280×800 pixels; 1136 UI passes of 1136; at most 500 \
             clips, 812 thumbnails, 260 names and 4 snap lines in a pass; 2 moves (2 snapped), 2 \
             trims, 310 passes zooming and 168 panning, from 2.0 to 777 s on screen; played 20.0 s."
        ));
        assert!(md.contains("![egui zoom 1.25](odin-abc-proto-0.png)"));
        assert_eq!(
            std::fs::read(s.0.join("hw-reports").join("odin-abc-proto-0.png")).unwrap(),
            b"png"
        );
        assert!(md.contains("No measurements: no document."));
        assert_eq!(
            proto_section(&s.0.join("nothing"), &report, "wasn't listed"),
            "No run: `gui_proto` wasn't listed.\n\n"
        );
    }

    /// With the VA-API drain and the gate probe on, a run's paragraphs
    /// say what the drain waited for and list the probe's slow jobs, the
    /// first 12 of them; with both off, or a document from before them,
    /// nothing is added.
    ///
    /// Control: the run with both off fails the check that the section
    /// shows them.
    #[test]
    fn the_section_shows_the_drain_and_the_probe_when_on() {
        let s = Scratch(std::env::temp_dir().join(format!("xtask-drain-{}", std::process::id())));
        let dir = s.0.join("hw-gui");
        std::fs::create_dir_all(&dir).unwrap();
        let doc = |extra: Value| {
            let mut d = serde_json::json!({
                "schema": "spherecut.smoke/1", "ok": true, "warnings": [],
                "result": {
                    "adapter": {"name": "AMD Radeon RX 7600 (RADV NAVI33)", "driver": "radv", "driver_info": "Mesa 26.2.5"},
                    "surface_format": "Bgra8UnormSrgb", "scale": 1.0, "native_scale": 1.0,
                    "window": [1280, 800], "viewer": [900, 600], "refresh_hz": 60.0,
                    "played_s": 20.0, "ended": false,
                    "ui": {"presented": 0.97, "p50": 16.7, "p99": 30.1, "frames": 1160, "passes": 1160},
                    "streams": [{"decode": "VA-API, copied", "decoder": "vaapi"}],
                    "stages": {"decode": {"samples": 400, "p50": 8.2, "p95": 11.0},
                               "drain": {"samples": 0}},
                    "submit_gate": {"waits": 30, "longest_ms": 4.5},
                }
            });
            for (k, v) in extra.as_object().unwrap() {
                d["result"][k] = v.clone();
            }
            d
        };
        // Fourteen slow jobs listed and two dropped.
        let over: Vec<Value> = (0..14)
            .map(|k| serde_json::json!([1.0 + f64::from(k), 40.0 + f64::from(k)]))
            .collect();
        let on = doc(serde_json::json!({
            "vaapi_drain": "download",
            "stages": {"decode": {"samples": 400, "p50": 8.2, "p95": 11.0},
                       "drain": {"samples": 380, "p50": 0.4, "p95": 2.06, "max": 9.12}},
            "drain": {"waited_per_download": {"p50": 2, "max": 12},
                      "errors": 0, "overflow": 1, "unavailable": 0},
            "probe": {"period_ms": 4.0, "n": 4980, "p50": 0.61, "p99": 3.12, "max": 53.0,
                      "over_8ms": over, "over_8ms_dropped": 2, "unfinished": 0},
        }));
        let off = doc(serde_json::json!({"vaapi_drain": "off"}));
        let before = doc(serde_json::json!({}));
        for (n, d) in [&on, &off, &before].into_iter().enumerate() {
            std::fs::write(dir.join(format!("gui-behind-{n}.json")), d.to_string()).unwrap();
        }
        let run = |n: u64| {
            serde_json::json!({
                "n": n, "label": format!("run {n}"), "clip": "/m/a.mp4", "asked": [], "role": "run",
                "behind": null, "document": dir.join(format!("gui-behind-{n}.json")),
                "screenshot": null, "failed": [], "gates": [],
            })
        };
        let summary = |runs: Vec<Value>| {
            serde_json::json!({
                "session": "Wayland socket discovered", "env": [], "locked": false,
                "vaapi_hevc": ["HEVC Main 10"], "max_shown_behind": 0.85,
                "runs": runs, "not_run": [],
            })
        };
        let report = s.0.join("hw-reports").join("odin-abc.md");
        let section_with = |runs: Vec<Value>| {
            std::fs::write(dir.join(BEHIND_SUMMARY), summary(runs).to_string()).unwrap();
            behind_section(&dir, &report, "passed")
        };
        let shows = |md: &str| md.contains("The VA-API drain") && md.contains("The gate probe");
        let md = section_with(vec![run(0)]);
        eprintln!("{md}");
        assert!(shows(&md));
        assert!(md.contains("p50 / p95: decode 8.2 / 11.0 ms, drain 0.4 / 2.1 ms;"));
        assert!(md.contains(
            "The VA-API drain (`download`): 380 frames drained, p50 / p95 / max 0.4 / 2.1 / 9.1 \
             ms each, waiting for p50 2 and at most 12 surfaces not yet ready; 0 failed syncs, \
             1 surface forgotten past the 64 a decoder keeps.\n"
        ));
        assert!(md.contains(
            "The gate probe, a job on the viewer's queue every 4 ms: 4980 timed while it \
             played, p50 / p99 / max 0.6 / 3.1 / 53.0 ms; 16 slower than 8 ms, at 1.00 s (40.0 \
             ms), 2.00 s (41.0 ms), 3.00 s (42.0 ms), 4.00 s (43.0 ms), 5.00 s (44.0 ms), 6.00 \
             s (45.0 ms), 7.00 s (46.0 ms), 8.00 s (47.0 ms), 9.00 s (48.0 ms), 10.00 s (49.0 \
             ms), 11.00 s (50.0 ms), 12.00 s (51.0 ms) and 4 more.\n"
        ));
        // A drain that found nothing to call, and a probe with no slow job
        // and some not done.
        let mut lost = on.clone();
        lost["result"]["stages"]["drain"] = serde_json::json!({"samples": 0});
        lost["result"]["drain"] = serde_json::json!({
            "waited_per_download": {"p50": null, "max": null},
            "errors": 0, "overflow": 0, "unavailable": 2,
        });
        lost["result"]["probe"]["over_8ms"] = serde_json::json!([]);
        lost["result"]["probe"]["over_8ms_dropped"] = serde_json::json!(0);
        lost["result"]["probe"]["unfinished"] = serde_json::json!(3);
        assert_eq!(
            drain(&lost["result"]).unwrap(),
            "The VA-API drain (`download`): no frame drained; 0 failed syncs, 0 surfaces \
             forgotten past the 64 a decoder keeps; 2 decoders found no vaSyncSurface to call, \
             so drained nothing."
        );
        assert_eq!(
            probe(&lost["result"]["probe"]).unwrap(),
            "The gate probe, a job on the viewer's queue every 4 ms: 4980 timed while it \
             played, p50 / p99 / max 0.6 / 3.1 / 53.0 ms; none slower than 8 ms; 3 not done a \
             second after it stopped."
        );
        // Off, and from before: neither.
        let md = section_with(vec![run(1), run(2)]);
        assert!(
            !md.contains("The VA-API drain") && !md.contains("The gate probe"),
            "{md}"
        );
        assert!(!md.contains("drain 0") && !md.contains(", drain"), "{md}");
        must_fail("the runs with both off", shows(&md));
    }
}
