//! `docs/agents.md` against the code (a1-plan sections 9 and 10.3): every
//! limit in `limits.rs` and the others agents meet, every error code, and
//! every code of media and render with what it becomes, are listed; each
//! client has its setup; and nothing tells anyone to run `npx`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use spherecut_agent::error::codes;
use spherecut_media::MediaError;
use spherecut_media::probe::codes as probe_codes;
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
