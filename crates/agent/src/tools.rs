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

use crate::error::{ToolError, codes};
use crate::limits::{INLINE_IMAGE_BYTES, RESULT_CHARS};
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
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "probe",
        title: "Read what media files are",
        schema: include_str!("../../../docs/agents/schemas/probe.json"),
    },
    Tool {
        name: "look",
        title: "See frames of a video",
        schema: include_str!("../../../docs/agents/schemas/look.json"),
    },
    Tool {
        name: "analyze",
        title: "Measure a video over time",
        schema: include_str!("../../../docs/agents/schemas/analyze.json"),
    },
    Tool {
        name: "check",
        title: "Check a file's metadata and fit",
        schema: include_str!("../../../docs/agents/schemas/check.json"),
    },
    Tool {
        name: "doctor",
        title: "Check this machine's decoders",
        schema: include_str!("../../../docs/agents/schemas/doctor.json"),
    },
];

/// The most tools `tools/list` may hold: past a dozen, agents choose
/// worse (a1-plan section 4.4).
pub const MAX_TOOLS: usize = 12;

/// The most bytes `tools/list`'s JSON may take through M9: it is in every
/// session's context.
pub const MAX_LIST_BYTES: usize = 12 << 10;

/// A1's share of [`MAX_LIST_BYTES`], for its five tools, so that A2's
/// seven have the rest (a1-plan amendments 8, 32 and 34: look's schema
/// alone takes 2.4 KB, and the five 6.7 KB, so the share is 7 KiB rather
/// than 5).
pub const A1_LIST_BYTES: usize = 7 << 10;

/// A tool with its schema parsed.
#[derive(Debug)]
struct Parsed {
    tool: &'static Tool,
    schema: Value,
}

/// The tools Spherecut serves, the roots they open files under, and how
/// much base64 an image inline may take.
#[derive(Debug)]
pub struct Spherecut {
    tools: Vec<Parsed>,
    list: Value,
    roots: Roots,
    image_budget: usize,
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
        let writes = roots.writes();
        let list = Value::Array(tools.iter().map(|p| listed(p, writes)).collect());
        Self {
            tools,
            list,
            roots,
            image_budget: INLINE_IMAGE_BYTES,
        }
    }

    /// The same, with images inline held to `bytes` of base64.
    pub fn image_budget(mut self, bytes: usize) -> Self {
        self.image_budget = bytes;
        self
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

/// `name`'s schema, parsed.
pub(crate) fn schema(name: &str) -> Option<&'static Value> {
    parsed()
        .iter()
        .find(|(t, _)| t.name == name)
        .map(|(_, s)| s)
}

/// The tools that write a file when asked to and writing is on.
const WRITERS: &[&str] = &["look"];

/// A tool as `tools/list` has it. The schema's own title and description
/// move up to the tool, where clients read them, rather than appear
/// twice, and its `$schema` goes: 2020-12 is MCP's default dialect
/// (2025-11-25), and the schemas use nothing that differs from draft 7.
/// A tool that writes with writing on (`--allow-write`) isn't read-only,
/// but never destroys: it only creates new files, so a second call the
/// same has no more effect (it is refused, `output_exists`). The other
/// hints mean something only for a tool that isn't read-only, so a
/// read-only one leaves them out.
fn listed(p: &Parsed, writes: bool) -> Value {
    let mut input: Map<String, Value> = p.schema.as_object().cloned().unwrap_or_default();
    let description = input.remove("description").unwrap_or(Value::Null);
    input.remove("title");
    input.remove("$schema");
    let mut annotations = json!({
        "title": p.tool.title,
        "readOnlyHint": true,
        "openWorldHint": false,
    });
    if writes && WRITERS.contains(&p.tool.name) {
        annotations["readOnlyHint"] = json!(false);
        annotations["destructiveHint"] = json!(false);
        annotations["idempotentHint"] = json!(true);
    }
    json!({
        "name": p.tool.name,
        "title": p.tool.title,
        "description": description,
        "inputSchema": input,
        "annotations": annotations,
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
        let cx = ops::Ctx {
            roots: &self.roots,
            call: cx,
            image_budget: self.image_budget,
        };
        let (mut result, images) = match name {
            "probe" => (ops::probe::run(ops::request(args)?, &cx)?, Vec::new()),
            "look" => ops::look::run(ops::request(args)?, &cx)?,
            "analyze" => (ops::analyze::run(ops::request(args)?, &cx)?, Vec::new()),
            "check" => (ops::check::run(ops::request(args)?, &cx)?, Vec::new()),
            "doctor" => (ops::doctor::run(ops::request(args)?, &cx)?, Vec::new()),
            _ => return Err(ToolError::internal(format!("{name} has no op"))),
        };
        text::sanitize(&mut result);
        fits(&result)?;
        Ok(Output { result, images })
    }
}

/// Err when a result's text would pass [`RESULT_CHARS`]: the ops keep
/// under it, so this is the backstop.
fn fits(result: &Value) -> Result<(), ToolError> {
    let n = result.to_string().chars().count();
    if n <= RESULT_CHARS {
        return Ok(());
    }
    tracing::warn!("mcp: a result of {n} characters, past {RESULT_CHARS}");
    Err(ToolError::new(
        codes::OUT_OF_RANGE,
        format!("the result would take {n} characters, past the {RESULT_CHARS} a client shows"),
    )
    .next("ask for less: fewer files, a smaller page_size, or detail \"summary\""))
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
        eprintln!("tools/list: {tools} tools, {bytes} bytes");
        assert!(bytes <= MAX_LIST_BYTES, "{bytes} bytes");
        assert!(bytes <= A1_LIST_BYTES, "{bytes} bytes, past A1's share");
        for t in list.as_array().into_iter().flatten() {
            assert!(
                t["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{t}"
            );
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["inputSchema"].get("description").is_none());
            assert!(t["inputSchema"].get("$schema").is_none());
            assert_eq!(t["annotations"]["readOnlyHint"], true);
        }
    }

    #[test]
    fn with_writing_on_look_says_it_writes_but_never_destroys() {
        let dir = std::env::temp_dir();
        let s = Spherecut::new(Roots::new(&[], Some(&dir)).unwrap());
        let list = s.list();
        let bytes = json!({ "tools": list }).to_string().len();
        assert!(bytes <= A1_LIST_BYTES, "{bytes} bytes, past A1's share");
        for t in list.as_array().into_iter().flatten() {
            let writer = t["name"] == "look";
            assert_eq!(t["annotations"]["readOnlyHint"], !writer, "{t}");
            if writer {
                assert_eq!(t["annotations"]["destructiveHint"], false);
                assert_eq!(t["annotations"]["idempotentHint"], true);
            } else {
                assert!(t["annotations"].get("idempotentHint").is_none(), "{t}");
            }
        }
    }

    /// One argument object per mode of each tool, holding every property
    /// that mode allows (a1-plan amendment 30).
    fn examples(name: &str) -> Vec<Value> {
        match name {
            "probe" => vec![
                json!({"files": ["a.mp4", "/media/b.insv"], "detail": "full"}),
                json!({"folder": "DCIM", "detail": "summary", "page_size": 10, "cursor": "10:00000000000000ff"}),
            ],
            "look" => vec![
                json!({"file": "a.mp4", "mode": "view", "times": ["1.5", [30000, 1001]],
                       "view": {"yaw": 90, "pitch": -20, "roll": 5, "fov": 100,
                                "projection": "fisheye"},
                       "size": 1024, "format": "png", "seek": "keyframe",
                       "output": "out/a.png", "assume_equirect": true}),
                json!({"file": "a.mp4", "times": ["0", "10"], "count": 5,
                       "views": [{"yaw": 0, "vfov": 60}, {"yaw": 180}]}),
            ],
            "analyze" => vec![json!({"file": "a.mp4", "kind": "timing", "from": "1.5",
                                    "to": [30000, 1001], "buckets": 20, "stream": 0})],
            "check" => vec![json!({"file": "a.mp4", "target": "youtube_360"})],
            "doctor" => {
                vec![json!({"file": "a.mp4", "speed": true, "render_node": "/dev/dri/renderD128"})]
            }
            _ => vec![],
        }
    }

    /// Whether the tool's request type takes `args`.
    fn accepts(name: &str, args: Value) -> Result<(), ToolError> {
        match name {
            "probe" => ops::request::<ops::probe::Request>(args).map(drop),
            "look" => ops::request::<ops::look::Request>(args).map(drop),
            "analyze" => ops::request::<ops::analyze::Request>(args).map(drop),
            "check" => ops::request::<ops::check::Request>(args).map(drop),
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

    /// The schemas' numbers are the limits'.
    #[test]
    fn schemas_hold_the_limits() {
        use crate::limits::{
            FILES_PER_CALL, IMAGE_LIMIT_EDGE, MAX_BUCKETS, MAX_PAGE_SIZE, MAX_TILES,
        };
        let schema = |name: &str| {
            parsed()
                .iter()
                .find(|(t, _)| t.name == name)
                .map(|(_, s)| s["properties"].clone())
                .unwrap_or_default()
        };
        let probe = schema("probe");
        assert_eq!(probe["files"]["maxItems"], json!(FILES_PER_CALL));
        assert_eq!(probe["page_size"]["maximum"], json!(MAX_PAGE_SIZE));
        let look = schema("look");
        assert_eq!(look["times"]["maxItems"], json!(MAX_TILES));
        assert_eq!(look["count"]["maximum"], json!(MAX_TILES));
        assert_eq!(look["views"]["maxItems"], json!(MAX_TILES));
        assert_eq!(look["size"]["maximum"], json!(IMAGE_LIMIT_EDGE));
        assert_eq!(schema("analyze")["buckets"]["maximum"], json!(MAX_BUCKETS));
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
