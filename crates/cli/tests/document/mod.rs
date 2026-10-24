//! Reading the JSON document a `spherecut --json` run prints (D27).

// Test helpers; panicking is the point here.
#![allow(clippy::unwrap_used)]

/// The one JSON document a `--json` run printed on stdout: exactly one
/// line, which parses.
pub fn document(stdout: &[u8]) -> serde_json::Value {
    let text = std::str::from_utf8(stdout).unwrap();
    assert!(
        text.ends_with('\n') && text.matches('\n').count() == 1,
        "not one line: {text:?}"
    );
    serde_json::from_str(text).unwrap()
}
