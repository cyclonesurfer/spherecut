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
use crate::jobs::Jobs;
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
    pub writes: Writes,
}

/// What a tool does to files with writing on (`--allow-write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writes {
    /// Nothing.
    Never,
    /// Only new files, when asked to: it never destroys, and the same
    /// call again is refused (`output_exists`), so it is idempotent.
    New,
    /// It replaces a project: listed only with a write root (a2-plan
    /// 1.1), since without one every call would be refused.
    Replaces,
    /// It starts or stops renders, which make only new files: listed
    /// only with a write root, as for [`Writes::Replaces`], and hinted as
    /// for [`Writes::New`].
    Renders,
}

impl Writes {
    /// Listed only when the server may write.
    fn needs_root(self) -> bool {
        matches!(self, Writes::Replaces | Writes::Renders)
    }
}

/// Every tool, in `tools/list`'s order.
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "probe",
        title: "Read what media files are",
        schema: include_str!("../../../docs/agents/schemas/probe.json"),
        writes: Writes::Never,
    },
    Tool {
        name: "look",
        title: "See frames of a video",
        schema: include_str!("../../../docs/agents/schemas/look.json"),
        writes: Writes::New,
    },
    Tool {
        name: "analyze",
        title: "Measure a video over time",
        schema: include_str!("../../../docs/agents/schemas/analyze.json"),
        writes: Writes::Never,
    },
    Tool {
        name: "check",
        title: "Check a file's metadata and fit",
        schema: include_str!("../../../docs/agents/schemas/check.json"),
        writes: Writes::Never,
    },
    Tool {
        name: "doctor",
        title: "Check this machine's decoders",
        schema: include_str!("../../../docs/agents/schemas/doctor.json"),
        writes: Writes::Never,
    },
    Tool {
        name: "project",
        title: "Read or make a project",
        schema: include_str!("../../../docs/agents/schemas/project.json"),
        writes: Writes::New,
    },
    Tool {
        name: "edit",
        title: "Edit a project",
        schema: include_str!("../../../docs/agents/schemas/edit.json"),
        writes: Writes::Replaces,
    },
    Tool {
        name: "undo",
        title: "Undo or redo an edit",
        schema: include_str!("../../../docs/agents/schemas/undo.json"),
        writes: Writes::Replaces,
    },
    Tool {
        name: "render",
        title: "Render a project",
        schema: include_str!("../../../docs/agents/schemas/render.json"),
        writes: Writes::Renders,
    },
    Tool {
        name: "job",
        title: "Follow or cancel a render",
        schema: include_str!("../../../docs/agents/schemas/job.json"),
        writes: Writes::Renders,
    },
    Tool {
        name: "compare",
        title: "Compare two videos or a render",
        schema: include_str!("../../../docs/agents/schemas/compare.json"),
        writes: Writes::Never,
    },
];

/// How many of [`TOOLS`] are A1's: the rest are A2's.
pub const A1_TOOLS: usize = 5;

/// Each of A2's tools' share of [`MAX_LIST_BYTES`], as listed with
/// writing on (a2-plan 4.5).
pub const BUDGETS: &[(&str, usize)] = &[
    ("project", 1000),
    ("edit", 950),
    ("undo", 650),
    ("render", 950),
    ("job", 650),
    ("compare", 1000),
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
    /// `tools/list`, with the title at the top and in the annotations:
    /// see [`top_title`].
    lists: [Value; 2],
    roots: Roots,
    image_budget: usize,
    /// The renders this server runs (a2-plan 5.3).
    jobs: Jobs,
}

impl Spherecut {
    pub fn new(roots: Roots) -> Self {
        let writes = roots.writes();
        let tools: Vec<Parsed> = parsed()
            .iter()
            .filter(|(tool, _)| writes || !tool.writes.needs_root())
            .map(|(tool, schema)| Parsed {
                tool,
                schema: schema.clone(),
            })
            .collect();
        let lists = [true, false]
            .map(|top| Value::Array(tools.iter().map(|p| listed(p, writes, top)).collect()));
        Self {
            tools,
            lists,
            roots,
            image_budget: INLINE_IMAGE_BYTES,
            jobs: Jobs::new(),
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

/// Whether a session on protocol revision `version` has a tool's title
/// at the top: 2025-06-18 added it there, and older clients read
/// `annotations.title`. Each form lists the title once (a2-plan 4.5).
/// Revisions are dates, so they compare as text.
pub fn top_title(version: &str) -> bool {
    version >= "2025-06-18"
}

/// A tool as `tools/list` has it. The schema's own title and description
/// move up to the tool, where clients read them, rather than appear
/// twice, and its `$schema` goes: 2020-12 is MCP's default dialect
/// (2025-11-25), and the schemas use nothing that differs from draft 7.
/// The title is at the top when `top` and in the annotations otherwise.
/// A tool that writes with writing on isn't read-only; see [`Writes`]
/// for the rest. The other hints mean something only for a tool that
/// isn't read-only, so a read-only one leaves them out.
fn listed(p: &Parsed, writes: bool, top: bool) -> Value {
    let mut input: Map<String, Value> = p.schema.as_object().cloned().unwrap_or_default();
    let description = input.remove("description").unwrap_or(Value::Null);
    input.remove("title");
    input.remove("$schema");
    let mut annotations = json!({
        "readOnlyHint": true,
        "openWorldHint": false,
    });
    match (writes, p.tool.writes) {
        (false, _) | (_, Writes::Never) => {}
        (true, Writes::New | Writes::Renders) => {
            annotations["readOnlyHint"] = json!(false);
            annotations["destructiveHint"] = json!(false);
            annotations["idempotentHint"] = json!(true);
        }
        (true, Writes::Replaces) => {
            annotations["readOnlyHint"] = json!(false);
            annotations["destructiveHint"] = json!(true);
            annotations["idempotentHint"] = json!(false);
        }
    }
    let mut tool = json!({ "name": p.tool.name });
    if top {
        tool["title"] = json!(p.tool.title);
    } else {
        annotations["title"] = json!(p.tool.title);
    }
    tool["description"] = description;
    tool["inputSchema"] = Value::Object(input);
    tool["annotations"] = annotations;
    tool
}

impl Tools for Spherecut {
    fn list(&self, version: &str) -> Value {
        self.lists[usize::from(!top_title(version))].clone()
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
            "project" => (ops::project::run(ops::request(args)?, &cx)?, Vec::new()),
            "edit" => (ops::edit::run(ops::request(args)?, &cx)?, Vec::new()),
            "undo" => (ops::edit::undo(ops::request(args)?, &cx)?, Vec::new()),
            "render" => (
                ops::render::run(ops::request(args)?, &cx, &self.jobs)?,
                Vec::new(),
            ),
            "job" => (
                ops::job::run(ops::request(args)?, &cx, &self.jobs)?,
                Vec::new(),
            ),
            "compare" => ops::compare::run(ops::request(args)?, &cx)?,
            _ => return Err(ToolError::internal(format!("{name} has no op"))),
        };
        text::sanitize(&mut result);
        fits(&result)?;
        Ok(Output { result, images })
    }

    fn cancel_all(&self) {
        self.jobs.cancel_all();
    }

    fn shutdown(&self, until: std::time::Instant) -> usize {
        self.jobs.shutdown(until)
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

    /// Spherecut with writing on, so that every tool is listed.
    fn writing() -> Spherecut {
        Spherecut::new(Roots::new(&[], Some(&std::env::temp_dir())).unwrap())
    }

    fn bytes(tools: &[Value]) -> usize {
        json!({ "tools": tools }).to_string().len()
    }

    #[test]
    fn the_list_fits() {
        let reading = Spherecut::new(Roots::new(&[], None).unwrap());
        for version in crate::rpc::SUPPORTED {
            for (s, count) in [(&reading, 7), (&writing(), TOOLS.len())] {
                let list = s.list(version);
                let list = list.as_array().cloned().unwrap_or_default();
                assert_eq!(list.len(), count, "{version}");
                assert!(list.len() <= MAX_TOOLS, "{} tools", list.len());
                let all = bytes(&list);
                let a1 = bytes(&list[..A1_TOOLS]);
                eprintln!("tools/list on {version}: {count} tools, {all} bytes, A1's {a1}");
                assert!(all <= MAX_LIST_BYTES, "{all} bytes");
                assert!(a1 <= A1_LIST_BYTES, "{a1} bytes, past A1's share");
                for t in &list {
                    assert!(
                        t["description"].as_str().is_some_and(|d| !d.is_empty()),
                        "{t}"
                    );
                    assert_eq!(t["inputSchema"]["type"], "object");
                    assert!(t["inputSchema"].get("description").is_none());
                    assert!(t["inputSchema"].get("$schema").is_none());
                }
            }
            for t in reading.list(version).as_array().into_iter().flatten() {
                assert_eq!(t["annotations"]["readOnlyHint"], true, "{t}");
            }
        }
    }

    /// Each of A2's tools within its share, in both title forms.
    #[test]
    fn each_new_tool_keeps_to_its_budget() {
        let s = writing();
        assert_eq!(BUDGETS.len(), TOOLS.len() - A1_TOOLS);
        for version in crate::rpc::SUPPORTED {
            for (name, budget) in BUDGETS {
                let list = s.list(version);
                let t = list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|t| t["name"] == *name)
                    .cloned()
                    .unwrap_or_else(|| panic!("{name} isn't listed"));
                let n = t.to_string().len();
                eprintln!("{name} on {version}: {n} bytes of {budget}");
                assert!(
                    n <= *budget,
                    "{name} on {version}: {n} bytes, past {budget}"
                );
            }
        }
    }

    #[test]
    fn each_tool_has_one_title_where_its_clients_read_it() {
        let s = writing();
        for version in crate::rpc::SUPPORTED {
            let top = top_title(version);
            assert_eq!(top, *version != "2025-03-26" && *version != "2024-11-05");
            for t in s.list(version).as_array().into_iter().flatten() {
                let title = TOOLS.iter().find(|x| t["name"] == x.name).unwrap().title;
                let (at, not) = if top {
                    (&t["title"], &t["annotations"]["title"])
                } else {
                    (&t["annotations"]["title"], &t["title"])
                };
                assert_eq!(at, title, "{version}: {t}");
                assert!(not.is_null(), "{version}: two titles in {t}");
            }
        }
    }

    #[test]
    fn with_writing_on_look_and_project_make_files_and_edit_and_undo_replace_them() {
        let list = writing().list(crate::rpc::SUPPORTED[0]);
        let a = |t: &Value, hint: &str| t["annotations"][hint].clone();
        for t in list.as_array().into_iter().flatten() {
            match t["name"].as_str().unwrap_or_default() {
                "look" | "project" => {
                    assert_eq!(a(t, "readOnlyHint"), false, "{t}");
                    assert_eq!(a(t, "destructiveHint"), false, "{t}");
                    assert_eq!(a(t, "idempotentHint"), true, "{t}");
                }
                "render" | "job" => {
                    assert_eq!(a(t, "readOnlyHint"), false, "{t}");
                    assert_eq!(a(t, "destructiveHint"), false, "{t}");
                    assert_eq!(a(t, "idempotentHint"), true, "{t}");
                }
                "edit" | "undo" => {
                    assert_eq!(a(t, "readOnlyHint"), false, "{t}");
                    assert_eq!(a(t, "destructiveHint"), true, "{t}");
                    assert_eq!(a(t, "idempotentHint"), false, "{t}");
                }
                _ => {
                    assert_eq!(a(t, "readOnlyHint"), true, "{t}");
                    assert!(t["annotations"].get("idempotentHint").is_none(), "{t}");
                }
            }
        }
    }

    #[test]
    fn without_writing_edit_undo_render_and_job_are_not_listed() {
        let s = Spherecut::new(Roots::new(&[], None).unwrap());
        for name in ["edit", "undo", "render", "job"] {
            assert!(!s.has(name), "{name}");
        }
        assert!(s.has("project"));
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
                json!({"project": "a.spherecut", "mode": "sheet", "count": 6}),
            ],
            "analyze" => vec![json!({"file": "a.mp4", "kind": "timing", "from": "1.5",
                                    "to": [30000, 1001], "buckets": 20, "stream": 0})],
            "check" => vec![
                json!({"file": "a.mp4", "target": "youtube_360"}),
                json!({"project": "a.spherecut", "target": "archive"}),
            ],
            "doctor" => {
                vec![json!({"file": "a.mp4", "speed": true, "render_node": "/dev/dri/renderD128"})]
            }
            "project" => vec![
                json!({"file": "a.spherecut", "part": "view", "clip": 4, "from": "1",
                       "to": [3, 1]}),
                json!({"file": "a.spherecut", "create": {"media": "a.mp4"}}),
            ],
            "edit" => vec![json!({"file": "a.spherecut",
                                  "edits": [{"op": "rename", "id": 2, "name": "x"}],
                                  "label": "x", "dry_run": true, "if_revision": 3})],
            "undo" => vec![json!({"file": "a.spherecut", "redo": true, "if_revision": 3})],
            "render" => vec![json!({"project": "a.spherecut", "output": "a.mp4",
                                    "range": {"from": "1", "to": [3, 1]}, "draft": true})],
            "job" => vec![
                json!({"job": "j0123abcd-1", "wait": 30}),
                json!({"job": "j0123abcd-1", "cancel": true}),
            ],
            "compare" => vec![
                json!({"file": "a.mp4", "against_file": "b.mp4", "times": ["1", [3, 1]],
                       "sheet": true}),
                json!({"file": "a.mp4", "against_project": "a.spherecut", "from": "2",
                       "count": 4}),
            ],
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
            "project" => ops::request::<ops::project::Request>(args).map(drop),
            "edit" => ops::request::<ops::edit::Request>(args).map(drop),
            "undo" => ops::request::<ops::edit::UndoRequest>(args).map(drop),
            "render" => ops::request::<ops::render::Request>(args).map(drop),
            "job" => ops::request::<ops::job::Request>(args).map(drop),
            "compare" => ops::request::<ops::compare::Request>(args).map(drop),
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
            EDITS_PER_CALL, FILES_PER_CALL, IMAGE_LIMIT_EDGE, LABEL_CHARS, MAX_BUCKETS,
            MAX_PAGE_SIZE, MAX_TILES,
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
        let edit = schema("edit");
        assert_eq!(edit["edits"]["maxItems"], json!(EDITS_PER_CALL));
        assert_eq!(edit["label"]["maxLength"], json!(LABEL_CHARS));
        assert_eq!(
            schema("job")["wait"]["maximum"],
            json!(crate::limits::JOB_WAIT.as_secs())
        );
        let compare = schema("compare");
        assert_eq!(
            compare["times"]["maxItems"],
            json!(crate::limits::COMPARE_TIMES)
        );
        assert_eq!(
            compare["count"]["maximum"],
            json!(crate::limits::COMPARE_TIMES)
        );
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
