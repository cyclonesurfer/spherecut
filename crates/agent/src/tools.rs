//! The tool table (a1-plan section 4.4): each tool's name, title, schema
//! and annotations, and the way from a `tools/call` to its op.
//!
//! A schema is a file in `docs/agents/schemas/`, compiled in. Its root
//! description is the tool's description, so the file is the one place
//! an agent's view of a tool is written. A call's arguments pass the
//! schema before the op sees them, and the op's response passes
//! [`text::sanitize`] before the agent does.

use std::sync::OnceLock;

use serde_json::{Map, Value, json};

use crate::error::ToolError;
#[cfg(test)]
use crate::error::codes;
use crate::roots::Roots;
use crate::rpc::{CallCx, Output, Tools};
use crate::{ops, schema, text};

/// One tool, as it is written down.
#[derive(Debug)]
pub struct Tool {
    pub name: &'static str,
    /// What a client shows for it.
    pub title: &'static str,
    /// Its input schema, `docs/agents/schemas/<name>.json`.
    pub schema: &'static str,
}

/// Every tool, in `tools/list`'s order.
pub const TOOLS: &[Tool] = &[Tool {
    name: "doctor",
    title: "Check this machine's decoders",
    schema: include_str!("../../../docs/agents/schemas/doctor.json"),
}];

/// The most tools `tools/list` may hold: past a dozen, agents choose
/// worse (a1-plan section 4.4).
pub const MAX_TOOLS: usize = 12;

/// The most bytes `tools/list`'s JSON may take through M9: it is in every
/// session's context.
pub const MAX_LIST_BYTES: usize = 12 << 10;

/// A1's share of [`MAX_LIST_BYTES`], for its five tools, so that A2's
/// seven have the rest (a1-plan amendment 8).
pub const A1_LIST_BYTES: usize = 5 << 10;

/// A tool with its schema parsed.
#[derive(Debug)]
struct Parsed {
    tool: &'static Tool,
    schema: Value,
}

/// The tools Spherecut serves, and the roots they open files under.
#[derive(Debug)]
pub struct Spherecut {
    tools: Vec<Parsed>,
    list: Value,
    roots: Roots,
}

impl Spherecut {
    pub fn new(roots: Roots) -> Self {
        let tools: Vec<Parsed> = parsed()
            .iter()
            .map(|(tool, schema)| Parsed {
                tool,
                schema: schema.clone(),
            })
            .collect();
        let list = Value::Array(tools.iter().map(listed).collect());
        Self { tools, list, roots }
    }

    /// The roots the tools open files under.
    pub fn roots(&self) -> &Roots {
        &self.roots
    }

    fn find(&self, name: &str) -> Option<&Parsed> {
        self.tools.iter().find(|p| p.tool.name == name)
    }
}

/// The schemas, parsed once. A schema that doesn't parse is left out
/// with an error in the log; the schema tests fail on it first.
fn parsed() -> &'static [(&'static Tool, Value)] {
    static PARSED: OnceLock<Vec<(&'static Tool, Value)>> = OnceLock::new();
    PARSED.get_or_init(|| {
        TOOLS
            .iter()
            .filter_map(|tool| match serde_json::from_str(tool.schema) {
                Ok(v) => Some((tool, v)),
                Err(e) => {
                    tracing::error!("mcp: {}'s schema doesn't parse: {e}", tool.name);
                    None
                }
            })
            .collect()
    })
}

/// A tool as `tools/list` has it. The schema's own title and description
/// move up to the tool, where clients read them, rather than appear
/// twice.
fn listed(p: &Parsed) -> Value {
    let mut input: Map<String, Value> = p.schema.as_object().cloned().unwrap_or_default();
    let description = input.remove("description").unwrap_or(Value::Null);
    input.remove("title");
    json!({
        "name": p.tool.name,
        "title": p.tool.title,
        "description": description,
        "inputSchema": input,
        "annotations": {
            "title": p.tool.title,
            "readOnlyHint": true,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

impl Tools for Spherecut {
    fn list(&self) -> Value {
        self.list.clone()
    }

    fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    fn call(&self, name: &str, args: Value, cx: &CallCx) -> Result<Output, ToolError> {
        let Some(p) = self.find(name) else {
            return Err(ToolError::internal(format!(
                "no tool {}",
                text::field(name)
            )));
        };
        schema::validate(&p.schema, &args)?;
        let mut result = match name {
            "doctor" => ops::doctor::run(ops::request(args)?, cx)?,
            _ => return Err(ToolError::internal(format!("{name} has no op"))),
        };
        text::sanitize(&mut result);
        Ok(Output::json(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_parses_and_passes_the_lint() {
        assert_eq!(parsed().len(), TOOLS.len());
        for (tool, schema) in parsed() {
            let problems = schema::lint(tool.name, schema);
            assert!(problems.is_empty(), "{}: {problems:#?}", tool.name);
            assert_eq!(
                schema["title"], tool.name,
                "a schema's title is its tool's name"
            );
        }
    }

    #[test]
    fn the_list_fits() {
        let s = Spherecut::new(Roots::new(&[], None).unwrap());
        let list = s.list();
        let tools = list.as_array().map_or(0, Vec::len);
        assert!(tools <= MAX_TOOLS, "{tools} tools");
        let bytes = json!({ "tools": list }).to_string().len();
        assert!(bytes <= MAX_LIST_BYTES, "{bytes} bytes");
        assert!(bytes <= A1_LIST_BYTES, "{bytes} bytes, past A1's share");
        for t in list.as_array().into_iter().flatten() {
            assert!(
                t["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{t}"
            );
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["inputSchema"].get("description").is_none());
            assert_eq!(t["annotations"]["readOnlyHint"], true);
        }
    }

    /// One argument object per mode of each tool, holding every property
    /// that mode allows (a1-plan amendment 30).
    fn examples(name: &str) -> Vec<Value> {
        match name {
            "doctor" => vec![json!({"render_node": "/dev/dri/renderD128"})],
            _ => vec![],
        }
    }

    /// Whether the tool's request type takes `args`.
    fn accepts(name: &str, args: Value) -> Result<(), ToolError> {
        match name {
            "doctor" => ops::request::<ops::doctor::Request>(args).map(drop),
            _ => Err(ToolError::internal(format!("no request type for {name}"))),
        }
    }

    #[test]
    fn schemas_and_request_types_agree() {
        use std::collections::BTreeSet;
        for (tool, schema) in parsed() {
            let name = tool.name;
            let examples = examples(name);
            assert!(!examples.is_empty(), "{name} has no examples");
            let keys = |v: &Value| -> BTreeSet<String> {
                v.as_object()
                    .into_iter()
                    .flat_map(|m| m.keys().cloned())
                    .collect()
            };
            let covered: BTreeSet<String> = examples.iter().flat_map(keys).collect();
            assert_eq!(
                covered,
                keys(&schema["properties"]),
                "{name}: the examples miss a property"
            );
            let required: Vec<&str> = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            for example in examples {
                assert_eq!(
                    schema::validate(schema, &example),
                    Ok(()),
                    "{name}: {example}"
                );
                assert_eq!(accepts(name, example.clone()), Ok(()), "{name}: {example}");
                for r in &required {
                    let mut without = example.clone();
                    without.as_object_mut().map(|m| m.remove(*r));
                    let e = schema::validate(schema, &without).err().map(|e| e.code);
                    assert_eq!(e, Some(codes::BAD_ARGUMENT), "{name} without {r}");
                    assert!(
                        accepts(name, without).is_err(),
                        "{name}'s type takes it without {r}"
                    );
                }
                let mut extra = example.clone();
                extra
                    .as_object_mut()
                    .map(|m| m.insert("zz_unknown".into(), json!(1)));
                let e = schema::validate(schema, &extra).err().map(|e| e.code);
                assert_eq!(
                    e,
                    Some(codes::BAD_ARGUMENT),
                    "{name} with an unknown property"
                );
                assert!(
                    accepts(name, extra).is_err(),
                    "{name}'s type takes an unknown field"
                );
            }
        }
    }

    #[test]
    fn names_are_unique_and_snake_case() {
        let mut seen = std::collections::BTreeSet::new();
        for t in TOOLS {
            assert!(seen.insert(t.name), "{} twice", t.name);
            assert!(t.name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        }
    }
}
