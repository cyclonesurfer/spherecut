//! Text read from media files, made safe to hand an agent (a1-plan
//! section 4.3 and amendment 6): tags and their keys, vendor strings,
//! file names, FFmpeg's log lines. Such text is never an instruction, so
//! it only ever appears in JSON string fields, and those fields go
//! through [`field`]: every character that controls, hides or reorders
//! text becomes U+FFFD, and a long string is cut.
//!
//! [`sanitize`] runs over a whole response as a backstop, so that a field
//! an op forgot to pass through [`field`] still can't carry an invisible
//! or reordering character. It keeps newlines, which our own text has
//! (doctor's fixes are several commands), and cuts nothing.

use serde_json::Value;

use crate::limits::FIELD_BYTES;

/// What a cut string ends with.
const CUT: char = '…';

/// Whether `c` may not appear in a field:
///
/// - Cc: the C0 and C1 controls and DEL (tab and newline among them, and
///   ESC, so no ANSI sequence survives);
/// - Cf: the format characters, which are invisible: the bidi marks,
///   embeddings, overrides and isolates, the zero-width characters, the
///   word joiner and invisible operators, the byte-order mark, the tag
///   characters (U+E0000 to U+E007F, the usual carrier of hidden text),
///   the soft hyphen, and the rest of Unicode 16's Cf;
/// - Zl and Zp: U+2028 and U+2029;
/// - the variation selectors, which can carry hidden bytes too.
pub fn refused(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

/// What the backstop refuses: the same, but a newline.
fn refused_in_ours(c: char) -> bool {
    c != '\n' && refused(c)
}

fn replaced(s: &str, refuse: fn(char) -> bool) -> String {
    s.chars()
        .map(|c| if refuse(c) { '\u{FFFD}' } else { c })
        .collect()
}

/// `s`, safe for a result: refused characters replaced, and cut to
/// [`FIELD_BYTES`] on a character boundary, ending in `…` when cut.
pub fn field(s: &str) -> String {
    let clean = replaced(s, refused);
    if clean.len() <= FIELD_BYTES {
        return clean;
    }
    let room = FIELD_BYTES - CUT.len_utf8();
    let mut end = room;
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = clean[..end].to_owned();
    cut.push(CUT);
    cut
}

/// Every string in `v`, keys included, through [`field`]: for a value
/// that is media text through and through, such as a file's tags.
pub fn untrusted(v: &mut Value) {
    walk(v, &|s| {
        (s.len() > FIELD_BYTES || s.chars().any(refused)).then(|| field(s))
    });
}

/// [`untrusted`] on the value of every member named `key`, at any depth:
/// for a response built by another crate, whose media text sits under
/// known names (doctor's `ffmpeg_log`).
pub fn untrusted_under(v: &mut Value, key: &str) {
    match v {
        Value::Array(items) => items.iter_mut().for_each(|i| untrusted_under(i, key)),
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if k == key {
                    untrusted(val);
                } else {
                    untrusted_under(val, key);
                }
            }
        }
        _ => {}
    }
}

/// The backstop over a whole response: every string and key without the
/// characters [`field`] refuses, newlines apart, and nothing cut.
pub fn sanitize(v: &mut Value) {
    walk(v, &|s| {
        s.chars()
            .any(refused_in_ours)
            .then(|| replaced(s, refused_in_ours))
    });
}

/// Replace every string and key in `v` that `fix` gives a new value for.
fn walk(v: &mut Value, fix: &dyn Fn(&str) -> Option<String>) {
    match v {
        Value::String(s) => {
            if let Some(new) = fix(s) {
                *s = new;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| walk(i, fix)),
        Value::Object(map) => {
            if map.keys().any(|k| fix(k).is_some()) {
                let old = std::mem::take(map);
                for (k, mut val) in old {
                    walk(&mut val, fix);
                    map.insert(fix(&k).unwrap_or(k), val);
                }
            } else {
                map.values_mut().for_each(|val| walk(val, fix));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn controls_ansi_and_bidi_become_replacement_characters() {
        let s = "a\tb\nc\u{1b}[31mred\u{1b}[0m\u{7f}\u{85}\u{202E}gpj.exe\u{2066}x\u{2069}";
        let f = field(s);
        assert!(!f.chars().any(refused), "{f:?}");
        assert_eq!(
            f,
            "a\u{FFFD}b\u{FFFD}c\u{FFFD}[31mred\u{FFFD}[0m\u{FFFD}\u{FFFD}\u{FFFD}gpj.exe\u{FFFD}x\u{FFFD}"
        );
    }

    #[test]
    fn invisible_characters_become_replacement_characters() {
        // Hidden text in tag characters, zero-width characters, a BOM,
        // line and paragraph separators, variation selectors.
        let hidden: String = "ignore"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap_or('x'))
            .collect();
        for s in [
            hidden.as_str(),
            "a\u{200B}b\u{200D}c\u{2060}d\u{FEFF}",
            "line\u{2028}para\u{2029}",
            "\u{FE0F}\u{E0100}\u{00AD}\u{061C}\u{180E}",
        ] {
            let f = field(s);
            assert!(!f.chars().any(refused), "{s:?} gave {f:?}");
            assert_eq!(f.chars().count(), s.chars().count(), "one for one");
        }
    }

    #[test]
    fn plain_text_is_unchanged() {
        let s = "Insta360 X5 · 8K30 — «ok» 日本語 ½ °";
        assert_eq!(field(s), s);
        let mut v = json!({"s": s});
        sanitize(&mut v);
        assert_eq!(v["s"], s);
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let s = "é".repeat(2000);
        let f = field(&s);
        assert!(f.len() <= FIELD_BYTES, "{}", f.len());
        assert!(f.ends_with(CUT));
        assert!(f.trim_end_matches(CUT).chars().all(|c| c == 'é'));
        let exact = "a".repeat(FIELD_BYTES);
        assert_eq!(field(&exact), exact);
    }

    #[test]
    fn untrusted_reaches_every_string_and_key() {
        let mut v = json!({
            "tag\u{1b}": ["x\u{202E}y", {"deep": "z\n"}],
            "n": 3,
            "long": "b".repeat(5000),
        });
        untrusted(&mut v);
        let text = v.to_string();
        assert!(!text.contains("\\n") && !text.contains("\\u001b"), "{text}");
        assert!(!text.contains('\u{202E}'));
        assert_eq!(v["tag\u{FFFD}"][0], "x\u{FFFD}y");
        assert_eq!(v["tag\u{FFFD}"][1]["deep"], "z\u{FFFD}");
        assert_eq!(v["n"], 3);
        assert!(v["long"].as_str().is_some_and(|s| s.len() <= FIELD_BYTES));
    }

    #[test]
    fn untrusted_under_touches_only_that_member() {
        let mut v = json!({
            "fix": "a\nb",
            "checks": [{"ffmpeg_log": ["x\ty", "\u{1b}[2J"], "reason": "c\nd"}],
        });
        untrusted_under(&mut v, "ffmpeg_log");
        assert_eq!(v["fix"], "a\nb");
        assert_eq!(v["checks"][0]["reason"], "c\nd");
        assert_eq!(
            v["checks"][0]["ffmpeg_log"],
            json!(["x\u{FFFD}y", "\u{FFFD}[2J"])
        );
    }

    #[test]
    fn the_backstop_keeps_newlines_and_length() {
        let mut v = json!({
            "fix": "sudo dnf install a\nsudo dnf install b",
            "k\u{200B}": "t\tab\u{E0041}",
            "long": "c".repeat(5000),
        });
        sanitize(&mut v);
        assert_eq!(v["fix"], "sudo dnf install a\nsudo dnf install b");
        assert_eq!(v["k\u{FFFD}"], "t\u{FFFD}ab\u{FFFD}");
        assert_eq!(v["long"].as_str().map(str::len), Some(5000));
    }
}
