//! The tools' input schemas, and a validator for the part of JSON Schema
//! they use (D43, a1-plan section 4.4).
//!
//! The schemas are written by hand in `docs/agents/schemas/` and compiled
//! in, so what the docs show is what clients get. Arguments are checked
//! against them before they are deserialized, which gives every error
//! the path of the argument at fault (`times[3]`, `view.fov`), and tests
//! hold each schema to its request type. The validator knows exactly the
//! keywords below; a test fails a schema that uses any other.

use serde_json::{Map, Value};

use crate::error::{ToolError, codes};

/// The keywords a schema may use: the ones [`validate`] checks, and
/// annotations it ignores.
pub const KEYWORDS: &[&str] = &[
    "$schema",
    "title",
    "description",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "enum",
    "default",
];

/// Check `value` against `schema`.
pub fn validate(schema: &Value, value: &Value) -> Result<(), ToolError> {
    check(schema, value, &mut Vec::new())
}

/// A step into the arguments: a property or an index.
enum Step<'a> {
    Key(&'a str),
    Index(usize),
}

fn path(steps: &[Step]) -> Option<String> {
    let mut out = String::new();
    for s in steps {
        match s {
            Step::Key(k) if out.is_empty() => out.push_str(k),
            Step::Key(k) => {
                out.push('.');
                out.push_str(k);
            }
            Step::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    (!out.is_empty()).then_some(out)
}

fn error(code: &'static str, steps: &[Step], message: impl Into<String>) -> ToolError {
    let mut e = ToolError::new(code, message);
    e.argument = path(steps);
    e
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn has_type(v: &Value, t: &str) -> bool {
    match t {
        "number" => v.is_number(),
        t => type_name(v) == t,
    }
}

fn with_article(t: &str) -> String {
    match t {
        "integer" | "array" | "object" => format!("an {t}"),
        "null" => "null".into(),
        t => format!("a {t}"),
    }
}

fn types(schema: &Map<String, Value>) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn number(schema: &Map<String, Value>, key: &str) -> Option<f64> {
    schema.get(key).and_then(Value::as_f64)
}

fn count(schema: &Map<String, Value>, key: &str) -> Option<usize> {
    schema
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

/// A number as JSON shows it, for a message.
fn shown(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

fn check<'a>(
    schema: &'a Value,
    value: &'a Value,
    steps: &mut Vec<Step<'a>>,
) -> Result<(), ToolError> {
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };
    let ts = types(schema);
    if !ts.is_empty() && !ts.iter().any(|t| has_type(value, t)) {
        let wanted = ts
            .iter()
            .map(|t| with_article(t))
            .collect::<Vec<_>>()
            .join(" or ");
        let mut e = error(
            codes::BAD_ARGUMENT,
            steps,
            format!("expected {wanted}, not {}", with_article(type_name(value))),
        );
        // A time is a string or a [num, den] pair (a1-plan section 4.1):
        // say what to send for a bare number.
        if ts.contains(&"string") && ts.contains(&"array") && value.is_number() {
            e.message = "a time is a string or a [num, den] pair, never a bare number".into();
            e = e.next(format!("send \"{value}\""));
        }
        return Err(e);
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array)
        && !choices.contains(value)
    {
        let list = choices
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(error(codes::BAD_ARGUMENT, steps, "isn't one of the choices").valid(list));
    }
    match value {
        Value::Object(map) => {
            let props = schema.get("properties").and_then(Value::as_object);
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for r in required.iter().filter_map(Value::as_str) {
                    if !map.contains_key(r) {
                        steps.push(Step::Key(r));
                        let e = error(codes::BAD_ARGUMENT, steps, "is required");
                        steps.pop();
                        return Err(e);
                    }
                }
            }
            let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
            for (k, v) in map {
                match props.and_then(|p| p.get(k)) {
                    Some(sub) => {
                        steps.push(Step::Key(k));
                        check(sub, v, steps)?;
                        steps.pop();
                    }
                    None if closed => {
                        let known = props
                            .map(|p| p.keys().map(String::as_str).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default();
                        // The name is the agent's own text: shown as the
                        // argument, cut short.
                        let name: String = k.chars().take(64).collect();
                        let at = match path(steps) {
                            Some(p) => format!("{p}.{name}"),
                            None => name,
                        };
                        return Err(ToolError::new(
                            codes::BAD_ARGUMENT,
                            "isn't an argument of this tool",
                        )
                        .at(at)
                        .valid(known));
                    }
                    None => {}
                }
            }
        }
        Value::Array(items) => {
            let n = items.len();
            let (lo, hi) = (count(schema, "minItems"), count(schema, "maxItems"));
            if lo.is_some_and(|lo| n < lo) || hi.is_some_and(|hi| n > hi) {
                let valid = match (lo, hi) {
                    (Some(lo), Some(hi)) if lo == hi => format!("exactly {lo} items"),
                    (Some(lo), Some(hi)) => format!("{lo} to {hi} items"),
                    (Some(lo), None) => format!("at least {lo} items"),
                    (None, Some(hi)) => format!("at most {hi} items"),
                    (None, None) => unreachable!(),
                };
                return Err(
                    error(codes::OUT_OF_RANGE, steps, format!("has {n} items")).valid(valid)
                );
            }
            if let Some(sub) = schema.get("items") {
                for (i, v) in items.iter().enumerate() {
                    steps.push(Step::Index(i));
                    check(sub, v, steps)?;
                    steps.pop();
                }
            }
        }
        Value::String(s) => {
            let n = s.chars().count();
            let (lo, hi) = (count(schema, "minLength"), count(schema, "maxLength"));
            if lo.is_some_and(|lo| n < lo) || hi.is_some_and(|hi| n > hi) {
                let valid = format!(
                    "{} to {} characters",
                    lo.unwrap_or(0),
                    hi.map_or("any number of".into(), |h| h.to_string())
                );
                return Err(
                    error(codes::OUT_OF_RANGE, steps, format!("has {n} characters")).valid(valid),
                );
            }
        }
        Value::Number(x) => {
            let x = x.as_f64().unwrap_or(f64::NAN);
            let (lo, hi) = (number(schema, "minimum"), number(schema, "maximum"));
            if lo.is_some_and(|lo| x < lo) || hi.is_some_and(|hi| x > hi) {
                let valid = match (lo, hi) {
                    (Some(lo), Some(hi)) => format!("{} to {}", shown(lo), shown(hi)),
                    (Some(lo), None) => format!("at least {}", shown(lo)),
                    (None, Some(hi)) => format!("at most {}", shown(hi)),
                    (None, None) => unreachable!(),
                };
                return Err(
                    error(codes::OUT_OF_RANGE, steps, format!("is {}", shown(x))).valid(valid),
                );
            }
        }
        Value::Null | Value::Bool(_) => {}
    }
    Ok(())
}

/// What makes a schema fit to publish (PLAN A1): draft 2020-12, an
/// object at the root that takes no other properties, no combinator, no
/// `$ref`, snake_case names, a description on the tool and every
/// property under 4,096 characters, and only the keywords in
/// [`KEYWORDS`]. Each problem as a sentence.
pub fn lint(name: &str, schema: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(root) = schema.as_object() else {
        return vec![format!("{name}: the schema isn't an object")];
    };
    if root.get("$schema").and_then(Value::as_str)
        != Some("https://json-schema.org/draft/2020-12/schema")
    {
        problems.push(format!("{name}: $schema isn't draft 2020-12"));
    }
    if root.get("type").and_then(Value::as_str) != Some("object") {
        problems.push(format!("{name}: the root isn't of type object"));
    }
    if root.get("additionalProperties") != Some(&Value::Bool(false)) {
        problems.push(format!("{name}: the root takes other properties"));
    }
    lint_node(name, schema, &mut problems);
    problems
}

fn lint_node(at: &str, node: &Value, problems: &mut Vec<String>) {
    let Some(map) = node.as_object() else {
        problems.push(format!("{at}: a schema that isn't an object"));
        return;
    };
    for k in map.keys() {
        if !KEYWORDS.contains(&k.as_str()) {
            problems.push(format!("{at}: keyword {k} isn't one the validator knows"));
        }
    }
    match map.get("description").and_then(Value::as_str) {
        None => problems.push(format!("{at}: no description")),
        Some(d) if d.chars().count() >= 4096 => {
            problems.push(format!("{at}: its description is 4,096 characters or more"))
        }
        Some(_) => {}
    }
    if let Some(props) = map.get("properties").and_then(Value::as_object) {
        for (k, sub) in props {
            let snake = k.len() <= 64
                && k.starts_with(|c: char| c.is_ascii_lowercase())
                && k.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !snake {
                problems.push(format!("{at}.{k}: not a snake_case ASCII name"));
            }
            lint_node(&format!("{at}.{k}"), sub, problems);
        }
        if let Some(required) = map.get("required").and_then(Value::as_array) {
            for r in required {
                if !r.as_str().is_some_and(|r| props.contains_key(r)) {
                    problems.push(format!("{at}: required {r} isn't a property"));
                }
            }
        }
    }
    if let Some(items) = map.get("items") {
        lint_node(&format!("{at}[]"), items, problems);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "description": "a tool",
            "type": "object",
            "additionalProperties": false,
            "required": ["file"],
            "properties": {
                "file": {"type": "string", "description": "a path", "minLength": 1},
                "times": {
                    "type": "array", "description": "times", "minItems": 1, "maxItems": 3,
                    "items": {"type": ["string", "array"], "description": "a time",
                              "items": {"type": "integer", "description": "num or den"},
                              "minItems": 2, "maxItems": 2}
                },
                "view": {
                    "type": "object", "description": "a view", "additionalProperties": false,
                    "properties": {"fov": {"type": "number", "description": "deg", "minimum": 1, "maximum": 360}}
                },
                "mode": {"type": "string", "description": "mode", "enum": ["view", "sheet"]}
            }
        })
    }

    fn err(v: Value) -> ToolError {
        validate(&schema(), &v).expect_err("should fail")
    }

    #[test]
    fn a_valid_value_passes() {
        let v = json!({"file": "a.mp4", "times": ["1.5", [1, 2]], "view": {"fov": 90}, "mode": "sheet"});
        validate(&schema(), &v).expect("valid");
        assert!(
            lint("t", &schema()).is_empty(),
            "{:?}",
            lint("t", &schema())
        );
    }

    #[test]
    fn errors_name_the_argument_at_fault() {
        let e = err(json!({"times": ["1"]}));
        assert_eq!(
            (e.code, e.argument.as_deref()),
            (codes::BAD_ARGUMENT, Some("file"))
        );
        let e = err(json!({"file": "a", "times": ["1", "2", [1, 2, 3]]}));
        assert_eq!(
            (e.code, e.argument.as_deref()),
            (codes::OUT_OF_RANGE, Some("times[2]"))
        );
        assert_eq!(e.valid.as_deref(), Some("exactly 2 items"));
        let e = err(json!({"file": "a", "view": {"fov": 400}}));
        assert_eq!(
            (e.code, e.argument.as_deref()),
            (codes::OUT_OF_RANGE, Some("view.fov"))
        );
        assert_eq!(e.valid.as_deref(), Some("1 to 360"));
        let e = err(json!({"file": "a", "mode": "cube"}));
        assert_eq!(e.argument.as_deref(), Some("mode"));
        assert_eq!(e.valid.as_deref(), Some("\"view\", \"sheet\""));
        let e = err(json!({"file": "a", "zoom": 2}));
        assert_eq!(
            (e.code, e.argument.as_deref()),
            (codes::BAD_ARGUMENT, Some("zoom"))
        );
        let e = err(json!({"file": "a", "view": {"fov": 90, "yaw": 1}}));
        assert_eq!(e.argument.as_deref(), Some("view.yaw"));
        let e = err(json!({"file": ""}));
        assert_eq!(e.code, codes::OUT_OF_RANGE);
        let e = err(json!([1]));
        assert_eq!(e.argument, None);
    }

    #[test]
    fn a_bare_number_time_says_what_to_send() {
        let e = err(json!({"file": "a", "times": [12.5]}));
        assert_eq!(e.argument.as_deref(), Some("times[0]"));
        assert!(e.message.contains("never a bare number"), "{}", e.message);
        assert_eq!(e.next.as_deref(), Some("send \"12.5\""));
    }

    #[test]
    fn integers_and_numbers() {
        let s = json!({"type": "integer", "description": "n"});
        assert!(validate(&s, &json!(3)).is_ok());
        assert!(validate(&s, &json!(3.5)).is_err());
        let s = json!({"type": "number", "description": "x"});
        assert!(validate(&s, &json!(3)).is_ok() && validate(&s, &json!(3.5)).is_ok());
    }

    #[test]
    fn lint_finds_what_clients_refuse() {
        let bad = json!({
            "type": "object",
            "oneOf": [],
            "properties": {"BadName": {"type": "string"}, "x": {"$ref": "#/a", "description": "d"}},
            "required": ["y"]
        });
        let p = lint("bad", &bad).join("\n");
        for needle in [
            "draft 2020-12",
            "takes other properties",
            "keyword oneOf",
            "keyword $ref",
            "BadName: not a snake_case",
            "BadName: no description",
            "required \"y\"",
            "bad: no description",
        ] {
            assert!(p.contains(needle), "{needle} in:\n{p}");
        }
    }
}
