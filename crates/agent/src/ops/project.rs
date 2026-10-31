//! `project` (a2-plan 4.2): a project's summary, one part of it, or a
//! new project of a clip; and what `edit` and `undo` share with it: the
//! project opened through its folder (3.1), the lock (3.2), the history
//! beside it (3.3), and a folder's errors as the agent gets them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use spherecut_core::project::file::FileError;
use spherecut_core::project::{
    By, Clip, FolderError, History, INITIAL_VIEW, Kept, Key, Loaded, Media, Origin, Project,
    ProjectFolder, ProjectLock, Step, file,
};
use spherecut_core::{FrameRate, Time, TimeSpec};

use super::Ctx;
use super::look::{self, TimeArg};
use crate::contract::Warning;
use crate::error::{ToolError, codes};
use crate::limits::{HISTORY_STEPS, LOCK_WAIT, SUMMARY_MARKERS, VIEW_CHARS, VIEW_KEYS};
use crate::still::{self, Decoder, Still};
use crate::{reference, text};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub file: PathBuf,
    pub create: Option<Create>,
    pub part: Option<Part>,
    pub clip: Option<u64>,
    pub from: Option<TimeArg>,
    pub to: Option<TimeArg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub media: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Sequence,
    View,
    Markers,
    Targets,
    History,
    Edits,
}

pub fn run(req: Request, cx: &Ctx) -> Result<Value, ToolError> {
    cx.call.check()?;
    if req.create.is_some() && (req.part.is_some() || req.clip.is_some()) {
        return Err(ToolError::new(
            codes::BAD_ARGUMENT,
            "create gives the new project's summary; ask for a part in another call",
        )
        .at("create"));
    }
    let ranged = matches!(req.part, Some(Part::View | Part::Markers | Part::Targets));
    if !ranged && (req.from.is_some() || req.to.is_some()) {
        return Err(ToolError::new(
            codes::BAD_ARGUMENT,
            "from and to go with part view, markers or targets",
        )
        .at(if req.from.is_some() { "from" } else { "to" }));
    }
    if req.clip.is_some() && !matches!(req.part, Some(Part::View | Part::Markers)) {
        return Err(
            ToolError::new(codes::BAD_ARGUMENT, "clip goes with part view or markers").at("clip"),
        );
    }
    if let Some(c) = &req.create {
        return create(&req.file, &c.media, cx);
    }
    if req.part == Some(Part::Edits) {
        return Ok(reference::part());
    }
    let (folder, name) = cx.roots.project_folder("file", &req.file, false)?;
    let loaded = folder.load(&name).map_err(|e| folder_error("file", e))?;
    cx.call.check()?;
    let p = &loaded.project;
    match req.part {
        None => Ok(summary(&folder, &name, &loaded, cx)),
        Some(Part::Sequence) => Ok(sequence(p)),
        Some(Part::View) => {
            let clip = the_clip(p, req.clip)?;
            let (from, to) = range(p, clip, req.from.as_ref(), req.to.as_ref())?;
            Ok(view(clip, from, to))
        }
        Some(Part::Markers) => {
            let clip = the_clip(p, req.clip)?;
            let (from, to) = range(p, clip, req.from.as_ref(), req.to.as_ref())?;
            Ok(markers(clip, from, to))
        }
        Some(Part::Targets) => targets(p, req.from.as_ref(), req.to.as_ref()),
        Some(Part::History) => Ok(history(&loaded)),
        Some(Part::Edits) => Ok(reference::part()),
    }
}

/// `create`: the untitled project of `media` at `file`, as the GUI makes
/// it of a clip it opens, and its summary.
fn create(file: &Path, media: &Path, cx: &Ctx) -> Result<Value, ToolError> {
    let (folder, name) = cx.roots.project_folder("file", file, true)?;
    if Path::new(&name).extension() != Some(std::ffi::OsStr::new(file::EXTENSION)) {
        return Err(ToolError::new(
            codes::BAD_ARGUMENT,
            format!("a project's name ends .{}", file::EXTENSION),
        )
        .at("file"));
    }
    let taken = || {
        ToolError::new(
            codes::OUTPUT_EXISTS,
            "is there already; nothing was replaced",
        )
        .at("file")
        .next("give another name, or read this project with project")
    };
    // Whatever is there, a link or a folder too, is something a new
    // project would replace.
    match folder.identity(&name) {
        Ok(None) => {}
        Ok(Some(_)) | Err(FolderError::Link { .. } | FolderError::NotAFile { .. }) => {
            return Err(taken());
        }
        Err(e) => return Err(folder_error("file", e)),
    }
    let mut doubts = Vec::new();
    let entry = probe_media("create.media", media, cx, &mut doubts)?;
    let project = Project::untitled(entry, INITIAL_VIEW).map_err(|p| {
        ToolError::new(
            codes::BAD_ARGUMENT,
            format!("can't be a project's clip: {p}"),
        )
        .at("create.media")
    })?;
    // As the GUI's Save As does: the target's lock before anything is
    // written.
    let _lock = lock(&folder, &name)?;
    folder
        .create(&name, &project, &History::new())
        .map_err(|e| match e {
            FolderError::Exists { .. } => taken(),
            e => folder_error("file", e),
        })?;
    let loaded = folder.load(&name).map_err(|e| folder_error("file", e))?;
    let mut s = summary(&folder, &name, &loaded, cx);
    s["created"] = json!(true);
    if !doubts.is_empty() {
        s["warnings"] = json!(doubts);
    }
    Ok(s)
}

/// The media entry of the video at `path`, the argument `arg`: opened
/// beneath the roots, probed through what was opened, its stream opened
/// for its rate and length, and recorded as the agent names it. Like
/// the GUI's, it takes footage whatever its metadata says, with a
/// `not_equirect` warning in `doubts` when it isn't tagged a stitched
/// equirect: looking at the project shows what a render makes of it.
pub(crate) fn probe_media(
    arg: &str,
    path: &Path,
    cx: &Ctx,
    doubts: &mut Vec<Warning>,
) -> Result<Media, ToolError> {
    let opened = cx.roots.open_file(arg, path)?;
    let named = cx.roots.named(arg, path)?;
    let stop = still::Stop::new(cx.call.token(), cx.call.interrupt());
    let opts = still::Options {
        decoder: Decoder::Software,
        equirect: true,
        ..still::Options::default()
    };
    let mut warnings = Vec::new();
    let failed = |f: &still::Failure| {
        if f.code == codes::CANCELLED {
            cx.call
                .check()
                .err()
                .unwrap_or_else(|| ToolError::new(codes::CANCELLED, "the call was stopped"))
        } else {
            let mut e = look::file_failed(f);
            e.argument = Some(arg.to_owned());
            e
        }
    };
    let probed =
        still::probe(opened.source(), &opts, &stop, &mut warnings).map_err(|f| failed(&f))?;
    if let Some(why) = still::equirect_doubt(&probed) {
        doubts.push(Warning::new(
            codes::NOT_EQUIRECT,
            format!("{why}; the project renders it as a stitched equirect all the same"),
        ));
    }
    let open = Still::open(opened.source(), &probed, None, &opts, &stop, &mut warnings)
        .map_err(|f| failed(&f))?;
    let info = open.info().clone();
    drop(open);
    spherecut_media::project::project_media(opened.source(), &named, &info)
        .map_err(|why| ToolError::new(codes::NO_VIDEO, why).at(arg))
}

/// The lock on `name` for one call: another call's is waited for
/// ([`LOCK_WAIT`]), the GUI's refused at once.
pub(crate) fn lock(folder: &ProjectFolder, name: &OsString) -> Result<ProjectLock, ToolError> {
    folder
        .lock(name, By::Mcp, LOCK_WAIT)
        .map_err(|e| folder_error("file", e))
}

/// A project open to change: its folder, its file's name, what was
/// loaded, and the lock it was loaded under (none for a dry run).
pub(crate) struct Writing {
    pub folder: ProjectFolder,
    pub name: OsString,
    pub loaded: Loaded,
    pub lock: Option<ProjectLock>,
}

/// The project at `file`, beneath the write root, loaded under its lock
/// when `lock` (else as a dry run sees it). It is read once before the
/// lock is taken, so that a file that isn't a project gets no lock file
/// beside it.
pub(crate) fn open(file: &Path, lock: bool, cx: &Ctx) -> Result<Writing, ToolError> {
    let (folder, name) = cx.roots.project_folder("file", file, true)?;
    let first = folder.load(&name).map_err(|e| folder_error("file", e))?;
    if !lock {
        // A dry run takes no lock, but meets the window's hold as the
        // edit would; another call's it would wait out.
        let holder = folder
            .holder(&name)
            .map_err(|e| folder_error("file", e))?
            .flatten();
        if let Some(h) = holder.filter(|h| h.by == By::Gui) {
            return Err(folder_error(
                "file",
                FolderError::Locked {
                    path: folder.path_of(&name),
                    holder: Some(h),
                },
            ));
        }
        return Ok(Writing {
            folder,
            name,
            loaded: first,
            lock: None,
        });
    }
    drop(first);
    let held = self::lock(&folder, &name)?;
    let loaded = folder.load(&name).map_err(|e| folder_error("file", e))?;
    Ok(Writing {
        folder,
        name,
        loaded,
        lock: Some(held),
    })
}

/// A folder's refusal as the agent gets it (a2-plan 10).
pub(crate) fn folder_error(arg: &str, e: FolderError) -> ToolError {
    let new = |code, message: String| ToolError::new(code, message).at(arg);
    match e {
        FolderError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            new(codes::NOT_FOUND, "doesn't exist".into())
        }
        FolderError::Io { source, .. } => new(
            codes::IO_ERROR,
            format!("can't be read or written: {}", source.kind()),
        ),
        FolderError::Link { .. } => new(
            codes::BAD_ARGUMENT,
            "is a symbolic link; give the path of the file it points to".into(),
        ),
        FolderError::NotAFile { .. } => new(codes::NOT_A_FILE, "isn't a regular file".into()),
        FolderError::Exists { .. } => new(
            codes::OUTPUT_EXISTS,
            "is there already; nothing was replaced".into(),
        ),
        FolderError::Locked { holder, .. } => match holder.map(|h| (h.by, h.pid)) {
            Some((By::Gui, pid)) => new(
                codes::PROJECT_LOCKED,
                format!("is open in Spherecut's window (pid {pid})"),
            )
            .next(
                "until agents can attach to the window, ask the user to close it there, then \
                 try again",
            ),
            Some((By::Mcp, pid)) => new(
                codes::PROJECT_LOCKED,
                format!("another agent's call is writing it (pid {pid})"),
            )
            .next("try again in a moment"),
            None => new(
                codes::PROJECT_LOCKED,
                "another program is writing it".into(),
            )
            .next("try again in a moment"),
        },
        FolderError::Changed { .. } => new(
            codes::CHANGED_ON_DISK,
            "changed on disk since it was read; nothing was saved".into(),
        )
        .next("read it again with project, then make the edit again"),
        FolderError::File(f) => {
            // Its own words can quote the file (a path, a field, a
            // format's name), which a message never carries: the log
            // has them.
            tracing::info!("mcp: a project that doesn't read: {f}");
            let (code, message) = match &f {
                FileError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                    (codes::NOT_FOUND, "doesn't exist".to_owned())
                }
                FileError::Io { source, .. } => {
                    (codes::IO_ERROR, format!("can't be read: {}", source.kind()))
                }
                FileError::TooLarge { .. } => (
                    codes::NOT_PROJECT,
                    format!(
                        "is over {} MiB, so not a project",
                        file::MAX_FILE_BYTES >> 20
                    ),
                ),
                FileError::NotJson(_) | FileError::NotProject(_) => {
                    (codes::NOT_PROJECT, "isn't a Spherecut project".to_owned())
                }
                FileError::NewerSchema { .. } => (codes::NEWER_SCHEMA, f.to_string()),
                FileError::Malformed(e) => (
                    codes::BAD_PROJECT,
                    format!(
                        "doesn't read as a project, at line {} column {}",
                        e.line(),
                        e.column()
                    ),
                ),
                FileError::Time { .. } => (
                    codes::BAD_PROJECT,
                    "holds a time that doesn't read".to_owned(),
                ),
                FileError::Invalid(p) => (
                    codes::BAD_PROJECT,
                    format!("breaks a project's rules ({})", p.code),
                ),
                FileError::PathNotUtf8 { id, .. } => {
                    (codes::BAD_PROJECT, format!("media {id}'s path isn't UTF-8"))
                }
            };
            let e = new(code, message);
            match code {
                codes::NOT_PROJECT => e.next("give a .spherecut file, or make one with create"),
                codes::NEWER_SCHEMA => e.next("a newer Spherecut reads it"),
                _ => e,
            }
        }
    }
}

/// The history to go on with, and whether the one beside the project was
/// dropped (stale or unreadable: 3.3).
pub(crate) fn history_of(kept: Kept) -> (History, bool) {
    let reset = matches!(kept, Kept::Stale { .. } | Kept::Unreadable(_));
    (kept.into_history(), reset)
}

/// Why an agent may not undo (or with `redo`, redo) now: the code and
/// the message. `None` when it may.
pub(crate) fn refusal(kept: &Kept, redo: bool, revision: u64) -> Option<(&'static str, String)> {
    let h = match kept {
        Kept::Missing => {
            return Some(nothing(redo));
        }
        Kept::Stale { revision: at } => {
            return Some((
                codes::STALE_REVISION,
                format!(
                    "the history was written with the project at revision {at}, and the file is \
                     at {revision}: something else wrote it since"
                ),
            ));
        }
        Kept::Unreadable(why) => {
            // The reason quotes the file, which isn't the message's to
            // carry (untrusted text is only in fields).
            tracing::info!("mcp: a history that doesn't read: {why}");
            return Some((
                codes::STALE_REVISION,
                "the history beside it can't be read".into(),
            ));
        }
        Kept::Kept(h) => h,
    };
    let step = if redo { h.redo_step() } else { h.undo_step() };
    match step {
        None => Some(nothing(redo)),
        // The step's label is the file's text, so it is in the result's
        // fields (the label, the history part), not in this message.
        Some(s) if s.origin == Origin::User => Some((
            codes::NOT_TOP,
            format!(
                "the step to {} is the user's (project's history part names it)",
                if redo { "redo" } else { "undo" },
            ),
        )),
        Some(_) => None,
    }
}

fn nothing(redo: bool) -> (&'static str, String) {
    if redo {
        (codes::NOTHING_TO_REDO, "nothing to redo".into())
    } else {
        (codes::NOTHING_TO_UNDO, "nothing to undo".into())
    }
}

/// The step undo (or redo) would take next, for a result: null when
/// there is none, and only `agent_may: false` with why when the history
/// beside the project can't be trusted.
pub(crate) fn next_step(kept: &Kept, redo: bool, revision: u64) -> Value {
    let step = match kept {
        Kept::Kept(h) if redo => h.redo_step(),
        Kept::Kept(h) => h.undo_step(),
        _ => None,
    };
    let mut v = match (kept, step) {
        (Kept::Kept(_), Some(s)) => {
            json!({ "label": text::field(&s.label), "by": origin(&s.origin) })
        }
        // Steps there may be, but none undo can trust: say why.
        (Kept::Stale { .. } | Kept::Unreadable(_), _) => json!({}),
        _ => return Value::Null,
    };
    match refusal(kept, redo, revision) {
        None => v["agent_may"] = json!(true),
        Some((code, why)) => {
            v["agent_may"] = json!(false);
            v["why_not"] = json!(code);
            v["message"] = json!(why);
        }
    }
    v
}

fn origin(o: &Origin) -> Value {
    match o {
        Origin::User => json!("user"),
        Origin::Agent { name } => json!({ "agent": text::field(name) }),
    }
}

/// A time as results give it: `{exact, seconds}`.
pub(crate) fn time(t: Time) -> Value {
    serde_json::to_value(t).unwrap_or(Value::Null)
}

/// Where a part goes on from, as `from` takes it: the exact time's text,
/// such as "1001/30000".
fn next_from(t: Time) -> Value {
    time(t)["exact"].clone()
}

fn value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The summary: a few KB whatever the project's size.
fn summary(folder: &ProjectFolder, name: &OsString, loaded: &Loaded, cx: &Ctx) -> Value {
    let p = &loaded.project;
    let seq = p.the_sequence();
    let mut out = json!({
        // The path the other calls name it by: a relative path is read
        // under the first read root but made under the write root.
        "file": text::field(&folder.path_of(name).to_string_lossy()),
        "name": text::field(&seq.name),
        "revision": p.revision(),
        "sequence": seq.id.0,
        "rate": value(&seq.rate),
        "output": value(&seq.output),
        "export": value(&seq.export),
    });
    out["media"] = p
        .media()
        .iter()
        .map(|m| {
            let (status, why) = media_status(cx, m);
            let mut v = json!({
                "id": m.id.0,
                "path": text::field(&m.path.to_string_lossy()),
                "bytes": m.fingerprint.bytes,
                "video": {
                    "size": [m.video.width, m.video.height],
                    "rate": value(&m.video.rate),
                    "duration": time(m.video.duration),
                },
                "status": status,
            });
            if let Some(why) = why {
                v["why"] = json!(why);
            }
            v
        })
        .collect();
    let mut markers_left = SUMMARY_MARKERS;
    out["clips"] = p
        .clips()
        .map(|c| {
            let listed: Vec<Value> = c.markers.iter().take(markers_left).map(marker).collect();
            markers_left -= listed.len();
            json!({
                "id": c.id.0,
                "media": c.media.0,
                "trim": {"start": time(c.source.start), "end": time(c.source.end)},
                "at": time(c.at),
                "view": {
                    "mode": value(&c.view.mode),
                    "orientation": track(&c.view.orientation.keys),
                    "fov": track(&c.view.fov.keys),
                    "projection": track(&c.view.projection.keys),
                },
                "markers": c.markers.len(),
                "first_markers": listed,
            })
        })
        .collect();
    out["targets"] = p
        .targets()
        .iter()
        .map(|t| {
            json!({
                "id": t.id.0,
                "name": text::field(&t.name),
                "media": t.media.0,
                "path": track(&t.path.keys),
            })
        })
        .collect();
    let revision = p.revision();
    let steps = match &loaded.history {
        Kept::Kept(h) => h.steps().len(),
        _ => 0,
    };
    let mut h = json!({
        "steps": steps,
        "undo": next_step(&loaded.history, false, revision),
        "redo": next_step(&loaded.history, true, revision),
    });
    match &loaded.history {
        Kept::Stale { revision: at } => {
            h["stale"] = json!(format!(
                "written with the project at revision {at}: the next edit starts a new history"
            ));
        }
        Kept::Unreadable(why) => h["unreadable"] = json!(text::field(why)),
        _ => {}
    }
    out["history"] = h;
    match folder.holder(name) {
        Ok(Some(Some(holder))) => {
            out["locked"] = json!({"by": value(&holder.by), "pid": holder.pid});
        }
        Ok(Some(None)) => out["locked"] = json!({"by": null}),
        Ok(None) => {}
        Err(e) => tracing::warn!("mcp: the project's lock couldn't be read: {e}"),
    }
    out
}

/// Whether media `m` is there and the same, through the roots: a file
/// outside them isn't read (a2-plan amendment 6).
fn media_status(cx: &Ctx, m: &Media) -> (&'static str, Option<String>) {
    match cx.roots.open_file("media", &m.path) {
        Err(e) if e.code == codes::OUTSIDE_ROOTS => ("outside_roots", None),
        Err(e) if e.code == codes::NOT_FOUND => ("missing", None),
        Err(e) => ("missing", Some(e.message)),
        Ok(f) if f.size() != m.fingerprint.bytes => (
            "changed",
            Some(format!(
                "its size is {} bytes, not {}",
                f.size(),
                m.fingerprint.bytes
            )),
        ),
        Ok(f) => match file::head_hash(f.source().open) {
            Err(e) => ("changed", Some(format!("it can't be read: {e}"))),
            Ok(h) if h != m.fingerprint.head => {
                ("changed", Some("its first MiB is different".into()))
            }
            Ok(_) => ("present", None),
        },
    }
}

/// A track's key count and its first and last times.
fn track<V>(keys: &[Key<V>]) -> Value {
    json!({
        "keys": keys.len(),
        "first": keys.first().map(|k| time(k.at)),
        "last": keys.last().map(|k| time(k.at)),
    })
}

fn marker(m: &spherecut_core::project::Marker) -> Value {
    json!({"id": m.id.0, "at": time(m.at), "name": text::field(&m.name)})
}

/// `clip`, or the only clip.
fn the_clip(p: &Project, clip: Option<u64>) -> Result<&Clip, ToolError> {
    match clip {
        None => Ok(p.the_clip()),
        Some(id) => p
            .clip(spherecut_core::project::ClipId(id))
            .ok_or_else(|| ToolError::new(codes::NOT_FOUND, format!("no clip {id}")).at("clip")),
    }
}

/// `from` and `to` in `clip`'s file time, frames at its media's rate.
fn range(
    p: &Project,
    clip: &Clip,
    from: Option<&TimeArg>,
    to: Option<&TimeArg>,
) -> Result<(Time, Option<Time>), ToolError> {
    let rate = p.media_by_id(clip.media).map(|m| m.video.rate);
    let from = from.map(|t| at("from", t, rate)).transpose()?;
    let to = to.map(|t| at("to", t, rate)).transpose()?;
    Ok((from.unwrap_or(Time::ZERO), to))
}

/// An agent's time as a time on an axis of `rate`'s frames.
pub(crate) fn at(arg: &str, t: &TimeArg, rate: Option<FrameRate>) -> Result<Time, ToolError> {
    match look::time(arg, t)? {
        TimeSpec::Time(t) => Ok(t),
        TimeSpec::Frame(n) => {
            let rate = rate.ok_or_else(|| {
                ToolError::new(
                    codes::BAD_ARGUMENT,
                    "a frame number needs a rate, and this time has none to count by",
                )
                .at(arg)
                .next("give it in seconds")
            })?;
            i64::try_from(n)
                .ok()
                .and_then(|n| rate.frame_to_time(n).ok())
                .ok_or_else(|| {
                    ToolError::new(codes::OUT_OF_RANGE, "is past any frame a clip has").at(arg)
                })
        }
    }
}

fn within(at: Time, from: Time, to: Option<Time>) -> bool {
    at >= from && to.is_none_or(|to| at <= to)
}

/// The sequence, its tracks and clips, without their keys and markers.
fn sequence(p: &Project) -> Value {
    let s = p.the_sequence();
    json!({
        "id": s.id.0,
        "name": text::field(&s.name),
        "rate": value(&s.rate),
        "output": value(&s.output),
        "export": value(&s.export),
        "tracks": s.tracks.iter().map(|t| json!({
            "id": t.id.0,
            "kind": value(&t.kind),
            "clips": t.clips.iter().map(|c| json!({
                "id": c.id.0,
                "media": c.media.0,
                "source": {"start": time(c.source.start), "end": time(c.source.end)},
                "at": time(c.at),
                "mode": value(&c.view.mode),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// A clip's keys from `from` to `to`, in time order across its three
/// tracks, until a track has [`VIEW_KEYS`] or the part [`VIEW_CHARS`]:
/// then `next_from` is where to go on, and no track is listed from it
/// on, so the next call from there misses nothing and repeats nothing.
fn view(clip: &Clip, from: Time, to: Option<Time>) -> Value {
    let v = &clip.view;
    let mut all: Vec<(Time, usize, Value)> = Vec::new();
    let mut add = |track: usize, at: Time, key: Value| {
        if within(at, from, to) {
            all.push((at, track, key));
        }
    };
    v.orientation
        .keys
        .iter()
        .for_each(|k| add(0, k.at, value(k)));
    v.fov.keys.iter().for_each(|k| add(1, k.at, value(k)));
    v.projection
        .keys
        .iter()
        .for_each(|k| add(2, k.at, value(k)));
    all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut counts = [0usize; 3];
    let mut chars = 0;
    let mut next_from = None;
    for (at, track, key) in &all {
        let n = key.to_string().len();
        if counts[*track] >= VIEW_KEYS || chars + n > VIEW_CHARS {
            next_from = Some(*at);
            break;
        }
        chars += n;
        counts[*track] += 1;
    }
    let mut tracks: [Vec<Value>; 3] = Default::default();
    for (at, track, key) in all {
        if next_from.is_some_and(|cut| at >= cut) {
            break;
        }
        tracks[track].push(key);
    }
    let [orientation, fov, projection] = tracks;
    let mut out = json!({
        "clip": clip.id.0,
        "mode": value(&v.mode),
        "orientation": orientation,
        "fov": fov,
        "projection": projection,
        "more": next_from.is_some(),
    });
    if let Some(t) = next_from {
        out["next_from"] = self::next_from(t);
    }
    out
}

/// A clip's markers from `from` to `to`, [`VIEW_KEYS`] at most. As in
/// [`view`], none is listed from `next_from` on, unless more than a page
/// share the first time: then a page of those, the rest `skipped`, and
/// `next_from` the time after.
fn markers(clip: &Clip, from: Time, to: Option<Time>) -> Value {
    let mut inside: Vec<_> = clip
        .markers
        .iter()
        .filter(|m| within(m.at, from, to))
        .collect();
    inside.sort_by_key(|m| m.at);
    let mut cut = inside.get(VIEW_KEYS).map(|m| m.at);
    let before = inside
        .iter()
        .take_while(|m| cut.is_none_or(|c| m.at < c))
        .count();
    let mut skipped = 0;
    let listed = if before == 0 && cut.is_some() {
        // More than a page at one time: the page lists some of them, and
        // goes on after that time, or `from` would give the same page.
        let at_once = inside.iter().take_while(|m| Some(m.at) == cut).count();
        skipped = at_once - VIEW_KEYS;
        cut = inside.get(at_once).map(|m| m.at);
        VIEW_KEYS
    } else {
        before.min(VIEW_KEYS)
    };
    let mut out = json!({
        "clip": clip.id.0,
        "markers": inside.iter().take(listed).map(|m| marker(m)).collect::<Vec<_>>(),
        "more": cut.is_some(),
    });
    if skipped > 0 {
        out["skipped"] = json!(skipped);
    }
    if let Some(t) = cut {
        out["next_from"] = next_from(t);
    }
    out
}

/// Every target, with its path's keys from `from` to `to` (in its own
/// media's time), [`VIEW_KEYS`] a target at most.
fn targets(p: &Project, from: Option<&TimeArg>, to: Option<&TimeArg>) -> Result<Value, ToolError> {
    let mut list = Vec::new();
    for t in p.targets() {
        let rate = p.media_by_id(t.media).map(|m| m.video.rate);
        let from = from
            .map(|a| at("from", a, rate))
            .transpose()?
            .unwrap_or(Time::ZERO);
        let to = to.map(|a| at("to", a, rate)).transpose()?;
        let inside: Vec<_> = t
            .path
            .keys
            .iter()
            .filter(|k| within(k.at, from, to))
            .collect();
        let mut v = json!({
            "id": t.id.0,
            "name": text::field(&t.name),
            "media": t.media.0,
            "path": inside.iter().copied().take(VIEW_KEYS).map(value).collect::<Vec<_>>(),
            "more": inside.len() > VIEW_KEYS,
        });
        if let Some(k) = inside.get(VIEW_KEYS) {
            v["next_from"] = next_from(k.at);
        }
        list.push(v);
    }
    Ok(json!({ "targets": list }))
}

/// The last [`HISTORY_STEPS`] steps, newest last, and where the cursor
/// is: the steps after it are undone.
fn history(loaded: &Loaded) -> Value {
    let revision = loaded.project.revision();
    let mut out = Map::new();
    match &loaded.history {
        Kept::Kept(h) => {
            let skip = h.steps().len().saturating_sub(HISTORY_STEPS);
            let steps: Vec<Value> = h
                .steps()
                .iter()
                .enumerate()
                .skip(skip)
                .map(|(i, s)| step(s, i < h.cursor()))
                .collect();
            out.insert("steps".into(), json!(steps));
            out.insert("left_out".into(), json!(skip));
            out.insert("done".into(), json!(h.cursor()));
        }
        Kept::Missing => {
            out.insert("steps".into(), json!([]));
        }
        Kept::Stale { revision: at } => {
            out.insert("steps".into(), json!([]));
            out.insert(
                "stale".into(),
                json!(format!(
                    "written with the project at revision {at}: the next edit starts a new history"
                )),
            );
        }
        Kept::Unreadable(why) => {
            out.insert("steps".into(), json!([]));
            out.insert("unreadable".into(), json!(text::field(why)));
        }
    }
    out.insert("undo".into(), next_step(&loaded.history, false, revision));
    out.insert("redo".into(), next_step(&loaded.history, true, revision));
    Value::Object(out)
}

fn step(s: &Step, done: bool) -> Value {
    json!({
        "label": text::field(&s.label),
        "by": origin(&s.origin),
        "edits": s.edits.len(),
        "before": s.before,
        "after": s.after,
        "done": done,
    })
}
