//! `edit` and `undo` (a2-plan 4.3 and 4.4), and the wire forms of edits
//! (4.1): A1's times in the fields named `at`, `start` and `end`, frames
//! at the rate of the time's own axis, and `add_media` by path.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use spherecut_core::FrameRate;
use spherecut_core::project::{
    AnyKey, AnyKeys, ClipId, Edit, EditError, Kept, Key, MarkerId, MediaId, Origin, Project,
    TargetId,
};

use super::Ctx;
use super::look::TimeArg;
use super::project::{
    self as proj, Writing, folder_error, history_of, next_step, probe_media, refusal,
};
use crate::contract::Warning;
use crate::error::{ToolError, codes};
use crate::limits::{EDITS_PER_CALL, LABEL_CHARS};
use crate::text;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub file: PathBuf,
    pub edits: Vec<Value>,
    pub label: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    pub if_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UndoRequest {
    pub file: PathBuf,
    #[serde(default)]
    pub redo: bool,
    pub if_revision: Option<u64>,
}

/// The most ids a result lists for one edit; past it, the count.
const IDS_LISTED: usize = 50;

pub fn run(req: Request, cx: &Ctx) -> Result<Value, ToolError> {
    cx.call.check()?;
    if req.edits.is_empty() {
        return Err(ToolError::new(codes::BAD_ARGUMENT, "is empty").at("edits"));
    }
    if req.edits.len() > EDITS_PER_CALL {
        return Err(ToolError::new(
            codes::OUT_OF_RANGE,
            format!(
                "{} edits, past the {EDITS_PER_CALL} a call may make",
                req.edits.len()
            ),
        )
        .at("edits")
        .next("a view path of many points is one set_view_path edit"));
    }
    let label = req.label.as_deref().and_then(label);
    let Writing {
        folder,
        name,
        loaded,
        lock,
    } = proj::open(&req.file, !req.dry_run, cx)?;
    let mut project = loaded.project;
    stale(project.revision(), req.if_revision)?;
    let edits = req
        .edits
        .into_iter()
        .enumerate()
        .map(|(i, v)| edit(i, v, &project, cx))
        .collect::<Result<Vec<_>, _>>()?;
    cx.call.check()?;
    let (mut history, reset) = history_of(loaded.history);
    let before = project.clone();
    let label = label.unwrap_or_else(|| default_label(&edits));
    let origin = Origin::Agent {
        name: cx.call.client().to_owned(),
    };
    let step = history
        .record(&mut project, label, origin, &edits)
        .map_err(bad_edit)?;
    let mut out = json!({
        "revision": project.revision(),
        "label": text::field(&step.label),
        "ids": made(&step.edits),
    });
    let mut warnings = outside_trim(&before, &project);
    if reset {
        out["history_reset"] = json!(true);
    }
    if req.dry_run {
        out["dry_run"] = json!(true);
    } else {
        let saved = folder
            .save(&name, &project, &history, Some(&loaded.identity))
            .map_err(|e| folder_error("file", e))?;
        if saved.dropped > 0 {
            out["history_dropped"] = json!(saved.dropped);
        }
        if let Some(e) = saved.history_error {
            warnings.push(Warning::new(
                "history_not_saved",
                format!("the project was saved, its history wasn't ({e}): undo can't take it back"),
            ));
        }
        drop(lock);
        out["undo"] = next_step(&Kept::Kept(history), false, project.revision());
    }
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(out)
}

pub fn undo(req: UndoRequest, cx: &Ctx) -> Result<Value, ToolError> {
    cx.call.check()?;
    let Writing {
        folder,
        name,
        loaded,
        lock,
    } = proj::open(&req.file, true, cx)?;
    let mut project = loaded.project;
    let revision = project.revision();
    stale(revision, req.if_revision)?;
    let (what, done) = if req.redo {
        ("redo", "redone")
    } else {
        ("undo", "undone")
    };
    if let Some((code, why)) = refusal(&loaded.history, req.redo, revision) {
        let e = ToolError::new(code, why).at("file");
        return Err(match code {
            codes::NOT_TOP => e.next(format!(
                "ask the user to {what} it in Spherecut's window, or make an edit that changes it"
            )),
            codes::STALE_REVISION => {
                e.next("read the project again with project and edit it: undo can't go past this")
            }
            _ => e,
        });
    }
    let Kept::Kept(mut history) = loaded.history else {
        return Err(ToolError::internal("a history refusal didn't refuse"));
    };
    let stepped = if req.redo {
        history.redo(&mut project)
    } else {
        history.undo(&mut project)
    };
    let label = match stepped {
        Ok(Some(step)) => step.label.clone(),
        Ok(None) => return Err(ToolError::internal(format!("nothing to {what} after all"))),
        Err(e) => {
            return Err(ToolError::new(
                codes::STALE_REVISION,
                format!("the step no longer applies to the project: {}", e.problem),
            )
            .at("file"));
        }
    };
    let saved = folder
        .save(&name, &project, &history, Some(&loaded.identity))
        .map_err(|e| folder_error("file", e))?;
    drop(lock);
    let revision = project.revision();
    let kept = Kept::Kept(history);
    let mut out = json!({
        "revision": revision,
        done: text::field(&label),
        "undo": next_step(&kept, false, revision),
        "redo": next_step(&kept, true, revision),
    });
    if let Some(e) = saved.history_error {
        out["warnings"] = json!([Warning::new(
            "history_not_saved",
            format!("the project was saved, its history wasn't: {e}"),
        )]);
    }
    Ok(out)
}

/// Refused `stale_revision` unless the project is at `want`.
fn stale(revision: u64, want: Option<u64>) -> Result<(), ToolError> {
    match want {
        Some(w) if w != revision => Err(ToolError::new(
            codes::STALE_REVISION,
            format!("the project is at revision {revision}, not {w}"),
        )
        .at("if_revision")
        .next("read it again with project, then decide the edit again")),
        _ => Ok(()),
    }
}

/// A label as the user's Edit menu shows it: control characters as
/// spaces, at most [`LABEL_CHARS`]; none when nothing is left.
fn label(s: &str) -> Option<String> {
    let clean: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(LABEL_CHARS)
        .collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_owned())
}

/// The first op's name as words ("Set trim"), or "N edits".
fn default_label(edits: &[Edit]) -> String {
    match edits {
        [one] => {
            let words = one.op().replace('_', " ");
            let mut c = words.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
        many => format!("{} edits", many.len()),
    }
}

fn bad_edit(e: EditError) -> ToolError {
    ToolError::new(
        codes::BAD_EDIT,
        format!(
            "{} refused, {}: {}",
            e.op, e.problem.code, e.problem.message
        ),
    )
    .at(format!("edits[{}]", e.index))
    .next("nothing was saved; fix this edit and send them all again")
}

/// Edit `i` as the agent wrote it, as an `Edit`: `add_media` probed by
/// its path, the rest through [`wire`].
fn edit(i: usize, v: Value, project: &Project, cx: &Ctx) -> Result<Edit, ToolError> {
    let arg = format!("edits[{i}]");
    if v.get("op").and_then(Value::as_str) == Some("add_media") {
        let path = match v.as_object() {
            Some(m) if m.len() == 2 => m.get("path").and_then(Value::as_str),
            _ => None,
        };
        let Some(path) = path else {
            return Err(ToolError::new(
                codes::BAD_ARGUMENT,
                "add_media takes a path and nothing else: the video is probed for the rest",
            )
            .at(arg)
            .valid(r#"{"op": "add_media", "path": "clips/b.mp4"}"#));
        };
        let media = probe_media(&format!("{arg}.path"), Path::new(path), cx)?;
        return Ok(Edit::AddMedia { media });
    }
    wire(&arg, v, project)
}

/// The fields whose values are times.
pub(crate) const TIME_FIELDS: [&str; 3] = ["at", "start", "end"];

/// An edit as the agent wrote it, as an `Edit`: every value of a field
/// named `at`, `start` or `end` turned into the file's time, frames
/// counted at the rate of that time's axis.
pub(crate) fn wire(arg: &str, mut v: Value, project: &Project) -> Result<Edit, ToolError> {
    let Some(obj) = v.as_object() else {
        return Err(ToolError::new(codes::BAD_ARGUMENT, "an edit is an object with an op").at(arg));
    };
    let op = obj
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let (media, sequence) = rates(&op, obj, project);
    let clip_at = format!("{arg}.clip.at");
    let rate = |at: &str| {
        if op == "add_clip" && at == clip_at {
            sequence
        } else {
            media
        }
    };
    times(&mut v, arg, &rate)?;
    serde_json::from_value(v).map_err(|e| {
        ToolError::new(codes::BAD_ARGUMENT, format!("doesn't read as an edit: {e}"))
            .at(arg)
            .next("project with part \"edits\" gives every op with an example")
    })
}

/// The rates an edit's times are counted at: its media's, and for
/// `add_clip`'s `at` its sequence's. `None` where the edit names nothing
/// there is, which the edit's own check refuses.
fn rates(
    op: &str,
    obj: &Map<String, Value>,
    p: &Project,
) -> (Option<FrameRate>, Option<FrameRate>) {
    let id = |k: &str| obj.get(k).and_then(Value::as_u64);
    let of_media = |m: u64| p.media_by_id(MediaId(m)).map(|m| m.video.rate);
    let of_clip = |c: u64| p.clip(ClipId(c)).and_then(|c| of_media(c.media.0));
    let media = match op {
        "set_trim" | "add_key" | "set_keys" | "set_view_path" | "add_marker" => {
            id("clip").and_then(of_clip)
        }
        "set_key" => id("key").and_then(|k| {
            p.clips()
                .find(|c| {
                    let k = spherecut_core::project::KeyId(k);
                    c.view.orientation.key(k).is_some()
                        || c.view.fov.key(k).is_some()
                        || c.view.projection.key(k).is_some()
                })
                .and_then(|c| of_media(c.media.0))
        }),
        "set_marker" => id("marker").and_then(|m| {
            p.clips()
                .find(|c| c.marker(MarkerId(m)).is_some())
                .and_then(|c| of_media(c.media.0))
        }),
        "add_target" => obj
            .get("target")
            .and_then(|t| t.get("media"))
            .and_then(Value::as_u64)
            .and_then(of_media),
        "set_target" => id("target")
            .and_then(|t| p.target(TargetId(t)))
            .and_then(|t| of_media(t.media.0)),
        "add_clip" => obj
            .get("clip")
            .and_then(|c| c.get("media"))
            .and_then(Value::as_u64)
            .and_then(of_media),
        _ => None,
    };
    let sequence = (op == "add_clip")
        .then(|| id("track"))
        .flatten()
        .and_then(|t| {
            p.sequences()
                .iter()
                .find(|s| s.tracks.iter().any(|tr| tr.id.0 == t))
                .map(|s| s.rate)
        });
    (media, sequence)
}

/// Every time field in `v`, at `at`, as the file writes it.
fn times(
    v: &mut Value,
    at: &str,
    rate: &dyn Fn(&str) -> Option<FrameRate>,
) -> Result<(), ToolError> {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                let here = format!("{at}.{k}");
                if TIME_FIELDS.contains(&k.as_str()) {
                    wire_time(x, &here, rate(&here))?;
                } else {
                    times(x, &here, rate)?;
                }
            }
        }
        Value::Array(items) => {
            for (i, x) in items.iter_mut().enumerate() {
                times(x, &format!("{at}[{i}]"), rate)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// One time, the argument `arg`: A1's forms turned into the file's, which
/// is left as it is, as is a null (an optional time left out).
fn wire_time(v: &mut Value, arg: &str, rate: Option<FrameRate>) -> Result<(), ToolError> {
    let t = match v {
        Value::Null | Value::Object(_) => return Ok(()),
        Value::String(s) => TimeArg::Text(s.clone()),
        Value::Array(a) => TimeArg::Pair(a.clone()),
        Value::Number(_) | Value::Bool(_) => {
            return Err(ToolError::new(
                codes::BAD_ARGUMENT,
                "a time is a string or [num, den], not a bare number",
            )
            .at(arg)
            .valid(r#""12.5", "1:02.5", "120f", [25, 2] or {"exact": "25/2"}"#));
        }
    };
    *v = proj::time(proj::at(arg, &t, rate)?);
    Ok(())
}

/// The ids `apply` gave the things the edits added, by the edit's index:
/// a list of at most [`IDS_LISTED`], else their count.
fn made(edits: &[Edit]) -> Vec<Value> {
    fn ids<V>(keys: &[Key<V>]) -> Vec<u64> {
        keys.iter().map(|k| k.id.0).collect()
    }
    edits
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let ids: Vec<u64> = match e {
                Edit::AddMedia { media } => vec![media.id.0],
                Edit::AddClip { clip, .. } => vec![clip.id.0],
                Edit::AddKey { key, .. } => vec![match key {
                    AnyKey::Orientation(k) => k.id.0,
                    AnyKey::Fov(k) => k.id.0,
                    AnyKey::Projection(k) => k.id.0,
                }],
                Edit::AddMarker { marker, .. } => vec![marker.id.0],
                Edit::AddTarget { target } => vec![target.id.0],
                Edit::SetKeys { keys, .. } => match keys {
                    AnyKeys::Orientation(k) => ids(k),
                    AnyKeys::Fov(k) => ids(k),
                    AnyKeys::Projection(k) => ids(k),
                },
                Edit::SetViewPath { points, .. } => points
                    .iter()
                    .flat_map(|p| [p.orientation_key.0, p.fov_key.0])
                    .filter(|id| *id != 0)
                    .collect(),
                _ => return None,
            };
            Some(if ids.len() <= IDS_LISTED {
                json!({ "edit": i, "ids": ids })
            } else {
                json!({ "edit": i, "count": ids.len() })
            })
        })
        .collect()
}

/// Warnings for keys and markers the edits left outside their clip's
/// trim, which no render shows: only those that weren't outside before.
fn outside_trim(before: &Project, after: &Project) -> Vec<Warning> {
    fn outside(p: &Project) -> (BTreeSet<u64>, BTreeSet<u64>) {
        let mut keys = BTreeSet::new();
        let mut markers = BTreeSet::new();
        for c in p.clips() {
            let out = |at| at < c.source.start || at > c.source.end;
            let v = &c.view;
            keys.extend(
                v.orientation
                    .keys
                    .iter()
                    .filter(|k| out(k.at))
                    .map(|k| k.id.0),
            );
            keys.extend(v.fov.keys.iter().filter(|k| out(k.at)).map(|k| k.id.0));
            keys.extend(
                v.projection
                    .keys
                    .iter()
                    .filter(|k| out(k.at))
                    .map(|k| k.id.0),
            );
            markers.extend(c.markers.iter().filter(|m| out(m.at)).map(|m| m.id.0));
        }
        (keys, markers)
    }
    let (k0, m0) = outside(before);
    let (k1, m1) = outside(after);
    let listed = |ids: Vec<&u64>| {
        let shown: Vec<String> = ids.iter().take(10).map(|id| id.to_string()).collect();
        let more = ids.len().saturating_sub(10);
        if more > 0 {
            format!("{} and {more} more", shown.join(", "))
        } else {
            shown.join(", ")
        }
    };
    let mut w = Vec::new();
    let keys: Vec<&u64> = k1.difference(&k0).collect();
    if !keys.is_empty() {
        w.push(Warning::new(
            "outside_trim",
            format!(
                "keys outside the clip's trim, which no render shows: {}",
                listed(keys)
            ),
        ));
    }
    let markers: Vec<&u64> = m1.difference(&m0).collect();
    if !markers.is_empty() {
        w.push(Warning::new(
            "outside_trim",
            format!("markers outside the clip's trim: {}", listed(markers)),
        ));
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference;

    fn project() -> Project {
        reference::project(reference::media(Path::new("/m/a.mp4")))
            .unwrap()
            .0
    }

    /// Every example in the reference, in the file's form.
    fn wired() -> Vec<(&'static str, Value)> {
        let p = project();
        reference::OPS
            .iter()
            .filter(|o| o.op != "add_media")
            .map(|o| {
                let e = wire("edits[0]", reference::example(o), &p)
                    .unwrap_or_else(|e| panic!("{}: {e}", o.op));
                (o.op, serde_json::to_value(e).unwrap())
            })
            .collect()
    }

    #[test]
    fn the_reference_has_every_op_once_in_order() {
        let ops: Vec<&str> = reference::OPS.iter().map(|o| o.op).collect();
        assert_eq!(ops, Edit::OPS);
        for o in reference::OPS {
            assert_eq!(reference::example(o)["op"], o.op, "{}", o.op);
        }
        // And each variant serializes to the op it is listed as.
        for (op, v) in wired() {
            assert_eq!(v["op"], op);
        }
    }

    /// The pass turns exactly the times: every field named at, start or
    /// end holds a time, and every time is in such a field (4.1).
    #[test]
    fn every_time_is_in_a_time_field_and_every_time_field_is_a_time() {
        fn walk(v: &Value, key: Option<&str>, op: &str) {
            if let Value::Object(m) = v {
                let is_time = m.contains_key("exact");
                assert_eq!(
                    is_time,
                    key.is_some_and(|k| TIME_FIELDS.contains(&k)),
                    "{op}: {key:?} holds {v}"
                );
                if is_time {
                    return;
                }
                for (k, x) in m {
                    if TIME_FIELDS.contains(&k.as_str()) {
                        assert!(x.is_null() || x.get("exact").is_some(), "{op}: {k} is {x}");
                    }
                    walk(x, Some(k), op);
                }
            } else if let Value::Array(a) = v {
                a.iter().for_each(|x| walk(x, key, op));
            }
        }
        for (op, v) in wired() {
            walk(&v, None, op);
        }
    }

    #[test]
    fn times_take_a1s_forms_frames_at_the_axis_rate_and_no_bare_number() {
        let p = project();
        let trim = |start: Value| {
            wire(
                "edits[0]",
                json!({"op": "set_trim", "clip": 4, "start": start}),
                &p,
            )
        };
        let start = |e: Edit| match e {
            Edit::SetTrim { start, .. } => start.unwrap().to_secs_f64(),
            _ => unreachable!(),
        };
        assert_eq!(start(trim(json!("1.5")).unwrap()), 1.5);
        assert_eq!(start(trim(json!("0:01.5")).unwrap()), 1.5);
        assert_eq!(start(trim(json!([3, 2])).unwrap()), 1.5);
        assert_eq!(start(trim(json!({"exact": "3/2"})).unwrap()), 1.5);
        // Frames at the clip's media's rate: 30 fps.
        assert_eq!(start(trim(json!("45f")).unwrap()), 1.5);
        let e = trim(json!(1.5)).unwrap_err();
        assert_eq!(e.code, codes::BAD_ARGUMENT);
        assert_eq!(e.argument.as_deref(), Some("edits[0].start"));
        // A clip that isn't there has no rate to count frames by.
        let e = wire(
            "edits[2]",
            json!({"op": "set_trim", "clip": 99, "end": "10f"}),
            &p,
        )
        .unwrap_err();
        assert_eq!(e.argument.as_deref(), Some("edits[2].end"));
        // A key's own clip, and a target's own media.
        let e = wire(
            "edits[0]",
            json!({"op": "set_key", "key": 8, "at": "60f"}),
            &p,
        )
        .unwrap();
        assert!(matches!(e, Edit::SetKey { at: Some(t), .. } if t.to_secs_f64() == 2.0));
        let e = wire(
            "edits[0]",
            json!({"op": "set_target", "target": 10,
                   "path": [{"at": "15f", "value": {"yaw_deg": 1, "pitch_deg": 2}}]}),
            &p,
        )
        .unwrap();
        let Edit::SetTarget {
            path: Some(path), ..
        } = e
        else {
            panic!("{e:?}")
        };
        assert_eq!(path.keys[0].at.to_secs_f64(), 0.5);
    }

    #[test]
    fn add_clips_at_counts_the_sequences_frames() {
        let p = project();
        let mut v = reference::example(&reference::OPS[2]);
        v["clip"]["at"] = json!("60f");
        v["clip"]["source"]["end"] = json!("60f");
        let Edit::AddClip { clip, .. } = wire("edits[0]", v, &p).unwrap() else {
            panic!()
        };
        // The sequence and the media are both 30 fps here; the pass
        // still asks each axis for its own.
        assert_eq!(clip.at.to_secs_f64(), 2.0);
        assert_eq!(clip.source.end.to_secs_f64(), 2.0);
    }

    #[test]
    fn labels_and_their_defaults() {
        assert_eq!(
            label("  Follow\nthe rider "),
            Some("Follow the rider".into())
        );
        assert_eq!(label(" \t "), None);
        assert_eq!(
            label(&"x".repeat(500)).unwrap().chars().count(),
            LABEL_CHARS
        );
        let p = project();
        let one = wire("e", json!({"op": "set_trim", "clip": 4, "end": "9"}), &p).unwrap();
        assert_eq!(default_label(std::slice::from_ref(&one)), "Set trim");
        assert_eq!(default_label(&[one.clone(), one]), "2 edits");
    }

    #[test]
    fn a_refused_edit_names_its_index_op_and_rule() {
        let mut p = project();
        let edits = vec![
            wire("e", json!({"op": "set_trim", "clip": 4, "end": "9"}), &p).unwrap(),
            wire("e", json!({"op": "remove_media", "media": 1}), &p).unwrap(),
        ];
        let e = bad_edit(p.apply(&edits).unwrap_err());
        assert_eq!(e.code, codes::BAD_EDIT);
        assert_eq!(e.argument.as_deref(), Some("edits[1]"));
        assert!(
            e.message.starts_with("remove_media refused, in_use:"),
            "{}",
            e.message
        );
    }

    #[test]
    fn warnings_name_only_what_the_edits_left_outside_the_trim() {
        let before = project();
        let mut after = before.clone();
        let e = wire(
            "e",
            json!({"op": "set_trim", "clip": 4, "end": "4"}),
            &after,
        )
        .unwrap();
        after.apply(&[e]).unwrap();
        let w = outside_trim(&before, &after);
        // The key at 5 s (8) and nothing else: the marker is at 3 s.
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].message.ends_with(": 8"), "{}", w[0].message);
        assert!(outside_trim(&after, &after).is_empty());
    }
}
