//! `docs/agents.md` against the code (a1-plan sections 9 and 10.3): every
//! limit in `limits.rs` and the others agents meet is listed with its
//! value, every error code and every warning code the tools give, and
//! every code of media and render with what it becomes; each client has
//! its setup; nothing tells anyone to run `npx`; and the crate's own doc
//! names every tool.
//!
//! Each check beside the first ones has a control that must fail (D32):
//! the same check on a doc made wrong on purpose. The agent's tests take
//! no testkit, so [`must_fail`] here is testkit's `control::must_fail` in
//! small.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use spherecut_agent::contract::codes::warnings as still_warnings;
use spherecut_agent::error::codes;
use spherecut_agent::limits::*;
use spherecut_agent::tools::{A1_LIST_BYTES, MAX_LIST_BYTES, MAX_TOOLS, TOOLS};
use spherecut_agent::{CALL_REFUSES, INSTRUCTIONS_CHARS};
use spherecut_media::probe::codes as probe_codes;
use spherecut_media::{MAX_PIXELS, MediaError};
use spherecut_render::RenderError;

fn read(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(path)).unwrap()
}

/// The table under `heading`: its rows' first cells, as they are written.
fn first_cells<'a>(doc: &'a str, heading: &str) -> Vec<&'a str> {
    let (_, after) = doc
        .split_once(heading)
        .unwrap_or_else(|| panic!("no {heading}"));
    after
        .lines()
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        .skip(2)
        .map(|l| l.split('|').nth(1).unwrap().trim())
        .collect()
}

/// The `code`s in a table's first cells.
fn listed<'a>(cells: &[&'a str]) -> Vec<&'a str> {
    cells
        .iter()
        .flat_map(|c| c.split(", "))
        .map(|c| c.trim_matches('`'))
        .collect()
}

#[test]
fn every_limit_is_listed() {
    let doc = read("docs/agents.md");
    let source = read("crates/agent/src/limits.rs");
    let names: Vec<&str> = source
        .lines()
        .filter_map(|l| l.strip_prefix("pub const "))
        .map(|l| l.split(':').next().unwrap())
        .collect();
    assert!(names.len() > 40, "{names:?}");
    let mut rows = listed(&first_cells(&doc, "\n## Limits"));
    rows.extend(listed(&first_cells(&doc, "`analyze`'s thresholds")));
    for name in names
        .iter()
        .chain(&["MAX_PIXELS", "MAX_TOOLS", "MAX_LIST_BYTES", "A1_LIST_BYTES"])
        .chain(&["INSTRUCTIONS_CHARS"])
    {
        assert!(
            rows.contains(name),
            "{name} isn't in docs/agents.md's limits"
        );
    }
}

#[test]
fn every_code_is_listed() {
    let doc = read("docs/agents.md");
    let rows = listed(&first_cells(&doc, "\n## Errors"));
    assert_eq!(
        rows,
        codes::ALL,
        "the codes, in the order error.rs has them"
    );
    let from = listed(&first_cells(&doc, "How the codes of media and render"));
    let mut theirs: Vec<&str> = MediaError::CODES.to_vec();
    theirs.extend(probe_codes::ALL);
    theirs.extend(RenderError::CODES);
    for code in theirs {
        assert!(from.contains(&code), "{code} has no row in docs/agents.md");
    }
}

#[test]
fn every_client_has_its_setup() {
    let doc = read("docs/agents.md");
    for client in [
        "Claude Code",
        "Codex",
        "Gemini CLI",
        "Copilot CLI",
        "Cursor",
        "Zed",
        "goose",
        "opencode",
        "a Flatpak",
    ] {
        let heading = doc
            .lines()
            .find(|l| l.starts_with("### ") && l.contains(client))
            .unwrap_or_else(|| panic!("no heading for {client}"));
        assert!(heading.len() < 60, "{heading}");
    }
    // What Codex doesn't pass, by name, in its config.
    for var in [
        "LIBVA_DRIVER_NAME",
        "LIBVA_DRIVERS_PATH",
        "MESA_VK_DEVICE_SELECT",
        "VK_ICD_FILENAMES",
        "XDG_RUNTIME_DIR",
    ] {
        assert!(doc.contains(&format!("\"{var}\"")), "{var}");
    }
    assert!(doc.contains("flatpak-spawn"));
    assert!(!doc.contains("npx"), "nothing tells anyone to run npx");
}

/// A control (D32): `outcome`, a check's on input wrong on purpose, must
/// be a failure, and one that names each of `naming`. A control that
/// stops failing fails the test.
#[track_caller]
fn must_fail(name: &str, outcome: Result<(), String>, naming: &[&str]) {
    match outcome {
        Ok(()) => panic!(
            "control `{name}` did not fail (it passed): a control that stops failing fails the \
             test (DECISIONS D32)"
        ),
        Err(why) => {
            for n in naming {
                assert!(
                    why.contains(n),
                    "control `{name}` failed, but not naming {n}: {why}"
                );
            }
        }
    }
}

/// The text of every `.rs` file under `dir`, but its tests (from the
/// first `#[cfg(test)]` on), with its path.
fn sources(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).unwrap();
                let code = text.split("#[cfg(test)]").next().unwrap().to_owned();
                out.push((p.display().to_string(), code));
            }
        }
    }
    out.sort();
    out
}

/// The string literals in `code`'s first argument from `at`: up to the
/// first `,` or `)` outside brackets.
fn first_argument_literals(code: &str, at: usize) -> Vec<String> {
    let bytes = code.as_bytes();
    let (mut depth, mut i, mut out) = (0usize, at, Vec::new());
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => break,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => break,
            b'"' => {
                let end = i + 1 + code[i + 1..].find('"').unwrap();
                out.push(code[i + 1..end].to_owned());
                i = end;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The warning codes the tools' own code gives: the literals in the
/// first argument of each `Warning::new(` and of probe's `warning(`, and
/// each `"code": "..."` of a warning made as JSON.
fn warning_codes(sources: &[(String, String)]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (_, code) in sources {
        for call in ["Warning::new(", "warning("] {
            for (at, _) in code.match_indices(call) {
                let before = code[..at].chars().next_back();
                if call == "warning("
                    && before.is_some_and(|c| c.is_alphanumeric() || "_:".contains(c))
                {
                    continue;
                }
                found.extend(first_argument_literals(code, at + call.len()));
            }
        }
        let key = "\"code\": \"";
        for (at, _) in code.match_indices(key) {
            let rest = &code[at + key.len()..];
            found.insert(rest[..rest.find('"').unwrap()].to_owned());
        }
    }
    found
}

/// The codes in the first cells of every table in `doc` whose heading
/// line (the paragraph or heading before it) holds `marker`.
fn codes_under(doc: &str, marker: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = doc;
    while let Some(at) = rest.find(marker) {
        rest = &rest[at + marker.len()..];
        out.extend(
            listed(&first_cells(rest, ""))
                .iter()
                .map(|c| (*c).to_owned()),
        );
    }
    out
}

/// Whether every code in `codes` is in agents.md's Warnings table, or in
/// one of cli-json.md's tables of warnings and damage.
fn warnings_listed(agents: &str, cli_json: &str, codes: &BTreeSet<String>) -> Result<(), String> {
    let mut rows: BTreeSet<String> = listed(&first_cells(agents, "\n## Warnings"))
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    rows.extend(codes_under(cli_json, "`warnings[].code`"));
    rows.extend(codes_under(cli_json, "`damage[].code`"));
    let missing: Vec<&String> = codes.iter().filter(|c| !rows.contains(*c)).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("warning codes listed nowhere: {missing:?}"))
    }
}

#[test]
fn every_warning_code_is_listed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut codes = warning_codes(&sources(&root.join("src")));
    // Still's warnings, which look, compare and a project's frames give
    // as frame does, and project's doubt that a file is an equirect.
    codes.extend(still_warnings::ALL.iter().map(|c| (*c).to_owned()));
    codes.insert(codes::NOT_EQUIRECT.to_owned());
    for code in [
        "history_not_saved",
        "not_flushed",
        "outside_trim",
        "result_cut",
        "untimed_packets",
    ] {
        assert!(codes.contains(code), "the scan missed {code}: {codes:?}");
    }
    let agents = read("docs/agents.md");
    let cli_json = read("docs/cli-json.md");
    warnings_listed(&agents, &cli_json, &codes).unwrap();

    let without: String = agents
        .lines()
        .filter(|l| !l.starts_with("| `result_cut` |"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(
        without, agents,
        "agents.md has no result_cut row to take out"
    );
    must_fail(
        "agents.md without result_cut's row",
        warnings_listed(&without, &cli_json, &codes),
        &["result_cut"],
    );
}

/// A count as the Limits table gives it: `1,048,576`.
fn count(n: impl Into<u64>) -> String {
    let digits = n.into().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Bytes in KiB or MiB, as the table gives them.
fn kib(n: usize) -> String {
    if n >= 1 << 20 && n.is_multiple_of(1 << 20) {
        format!("{} MiB", n >> 20)
    } else {
        assert_eq!(n % 1024, 0, "{n} bytes");
        format!("{} KiB", n >> 10)
    }
}

/// A time: `250 ms`, `1.5 s`, `50 s`.
fn time(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{} s", ms as f64 / 1000.0)
    }
}

fn px(n: u32) -> String {
    format!("{n} px")
}

/// A share as a percentage: `1 %`.
fn percent(x: f64) -> String {
    format!("{} %", (x * 100.0).round())
}

/// dBFS, with a minus sign: `−60 dBFS`.
fn dbfs(x: f64) -> String {
    format!("{x} dBFS").replace('-', "\u{2212}")
}

/// Every limit and threshold, by name, with its value as the tables in
/// agents.md give it.
fn limit_values() -> Vec<(&'static str, String)> {
    let n = |x: usize| count(x as u64);
    vec![
        ("MAX_REQUEST_BYTES", kib(MAX_REQUEST_BYTES)),
        ("CALLS_AT_ONCE", n(CALLS_AT_ONCE)),
        ("MAX_QUEUED", n(MAX_QUEUED)),
        ("CALL_TIME", time(CALL_TIME)),
        ("STUCK_GRACE", time(STUCK_GRACE)),
        ("MAX_ABANDONED", n(MAX_ABANDONED)),
        ("EXIT_GRACE", time(EXIT_GRACE)),
        ("PROGRESS_EVERY", time(PROGRESS_EVERY)),
        ("FILES_PER_CALL", n(FILES_PER_CALL)),
        ("FULL_PER_CALL", n(FULL_PER_CALL)),
        ("PAGE_SIZE", n(PAGE_SIZE)),
        ("MAX_PAGE_SIZE", n(MAX_PAGE_SIZE)),
        ("FOLDERS_CHARS", n(FOLDERS_CHARS)),
        ("FOLDER_ENTRIES", n(FOLDER_ENTRIES)),
        ("RESULT_CHARS", n(RESULT_CHARS)),
        ("FIELD_BYTES", kib(FIELD_BYTES)),
        ("ERROR_CHARS", n(ERROR_CHARS)),
        ("CHECK_BYTES", kib(CHECK_BYTES)),
        ("SPEED_TIME", time(SPEED_TIME)),
        ("MAX_TILES", n(MAX_TILES)),
        ("IMAGE_DEFAULT_EDGE", px(IMAGE_DEFAULT_EDGE)),
        ("IMAGE_MAX_EDGE", px(IMAGE_MAX_EDGE)),
        ("IMAGE_LIMIT_EDGE", px(IMAGE_LIMIT_EDGE)),
        ("MIN_TILE_WIDTH", px(MIN_TILE_WIDTH)),
        ("INLINE_IMAGE_BYTES", n(INLINE_IMAGE_BYTES)),
        ("MAX_IMAGE_BUDGET", n(MAX_IMAGE_BUDGET)),
        ("JPEG_QSCALE", count(JPEG_QSCALE)),
        ("JPEG_QSCALE_MAX", count(JPEG_QSCALE_MAX)),
        ("RAW_STREAMS", n(RAW_STREAMS)),
        ("BUCKETS", count(BUCKETS)),
        ("MAX_BUCKETS", count(MAX_BUCKETS)),
        ("ANALYZE_RESERVE", time(ANALYZE_RESERVE)),
        ("TIMING_PACKETS", n(TIMING_PACKETS)),
        ("LISTED", n(LISTED)),
        ("MAX_PIXELS", format!("2^{}", MAX_PIXELS.trailing_zeros())),
        ("EDITS_PER_CALL", n(EDITS_PER_CALL)),
        ("LABEL_CHARS", n(LABEL_CHARS)),
        ("VIEW_KEYS", n(VIEW_KEYS)),
        ("VIEW_CHARS", n(VIEW_CHARS)),
        ("HISTORY_STEPS", n(HISTORY_STEPS)),
        ("SUMMARY_MARKERS", n(SUMMARY_MARKERS)),
        ("LOCK_WAIT", time(LOCK_WAIT)),
        ("QUEUED_RENDERS", n(QUEUED_RENDERS)),
        ("FINISHED_JOBS", n(FINISHED_JOBS)),
        ("JOB_WAIT", time(JOB_WAIT)),
        ("JOB_WAIT_MARGIN", time(JOB_WAIT_MARGIN)),
        ("CANCEL_WAIT", time(CANCEL_WAIT)),
        ("COMPARE_TIMES", n(COMPARE_TIMES)),
        ("COMPARE_COUNT", count(COMPARE_COUNT)),
        ("SHEET_ROWS", n(SHEET_ROWS)),
        ("DIFF_GAIN", format!("{DIFF_GAIN}")),
        ("COMPARE_RESERVE", time(COMPARE_RESERVE)),
        ("MAX_TOOLS", n(MAX_TOOLS)),
        ("MAX_LIST_BYTES", kib(MAX_LIST_BYTES)),
        ("A1_LIST_BYTES", kib(A1_LIST_BYTES)),
        ("INSTRUCTIONS_CHARS", n(INSTRUCTIONS_CHARS)),
        ("GAP_OVER", format!("{GAP_OVER}")),
        ("VFR_SPREAD", percent(VFR_SPREAD)),
        ("REPEAT_UNDER", percent(REPEAT_UNDER)),
        (
            "MOTION_GRID",
            format!("{} \u{d7} {}", MOTION_GRID.0, MOTION_GRID.1),
        ),
        ("CUT_OVER_MEDIAN", format!("{CUT_OVER_MEDIAN}")),
        ("CUT_FLOOR", format!("{CUT_FLOOR}")),
        ("CUT_WINDOW", n(CUT_WINDOW)),
        ("CLIP_LEVEL", format!("{CLIP_LEVEL}")),
        ("CLIP_RUN", n(CLIP_RUN)),
        ("SILENCE_DBFS", dbfs(SILENCE_DBFS)),
        ("SILENCE_MIN", time(SILENCE_MIN)),
        ("DBFS_FLOOR", dbfs(DBFS_FLOOR)),
    ]
}

/// The tables' rows, `(name, value)`, from the Limits table and
/// analyze's thresholds.
fn limit_rows(doc: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for heading in ["\n## Limits", "`analyze`'s thresholds"] {
        let (_, after) = doc.split_once(heading).unwrap();
        for line in after
            .lines()
            .skip_while(|l| !l.starts_with('|'))
            .take_while(|l| l.starts_with('|'))
            .skip(2)
        {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            rows.push((cells[1].trim_matches('`').to_owned(), cells[2].to_owned()));
        }
    }
    rows
}

/// Whether each limit's row in `doc` gives its value.
fn limits_agree(doc: &str, values: &[(&str, String)]) -> Result<(), String> {
    let rows = limit_rows(doc);
    let mut wrong = Vec::new();
    for (name, value) in values {
        match rows.iter().find(|(n, _)| n == name) {
            Some((_, v)) if v == value => {}
            Some((_, v)) => wrong.push(format!("{name} is {value}, the doc says {v}")),
            None => wrong.push(format!("{name} has no row")),
        }
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(wrong.join("; "))
    }
}

#[test]
fn every_limit_has_its_value() {
    let values = limit_values();
    let source = read("crates/agent/src/limits.rs");
    for name in source
        .lines()
        .filter_map(|l| l.strip_prefix("pub const "))
        .map(|l| l.split(':').next().unwrap())
    {
        assert!(
            values.iter().any(|(n, _)| *n == name),
            "{name} in limits.rs has no value in this test's list"
        );
    }
    let doc = read("docs/agents.md");
    limits_agree(&doc, &values).unwrap();

    let row = "| `QUEUED_RENDERS` | 4 |";
    assert_eq!(doc.matches(row).count(), 1, "no {row} row to change");
    must_fail(
        "QUEUED_RENDERS given as 5",
        limits_agree(&doc.replace(row, "| `QUEUED_RENDERS` | 5 |"), &values),
        &["QUEUED_RENDERS"],
    );
}

/// Whether `doc`, a crate doc's text, names every tool in backticks.
fn names_every_tool(doc: &str) -> Result<(), String> {
    let missing: Vec<&str> = TOOLS
        .iter()
        .map(|t| t.name)
        .filter(|n| !doc.contains(&format!("`{n}`")))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("tools the crate doc doesn't name: {missing:?}"))
    }
}

#[test]
fn the_agent_crate_doc_names_every_tool() {
    let lib = read("crates/agent/src/lib.rs");
    let doc: String = lib
        .lines()
        .filter_map(|l| l.strip_prefix("//!"))
        .map(|l| format!("{l}\n"))
        .collect();
    names_every_tool(&doc).unwrap();
    for refused in CALL_REFUSES {
        assert!(doc.contains(&format!("`{refused}`")), "{refused}");
    }
    assert!(
        doc.contains("CALL_REFUSES"),
        "the doc says what call refuses"
    );

    // The doc as it was at 073715b, lines 4 to 7.
    let before = "An agent reads footage through five tools (probe, look, analyze,
check, doctor), read-only but for look's `output`, and reads, makes
and edits projects through three more (project, edit, undo; A2,
[`reference`] for the ops); docs/agents.md has them all.";
    must_fail(
        "the crate doc at 073715b",
        names_every_tool(before),
        &["render", "job", "compare"],
    );
}
