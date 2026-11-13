//! The project file (M2 plan section 5, DECISIONS D47): JSON in a
//! `.spherecut` file.
//!
//! ```json
//! {
//!   "format": "spherecut.project",
//!   "schema": 2,
//!   "saved_by": "spherecut 0.1.0",
//!   "project": { "revision": 41, "next_id": 97, "media": [...], ... }
//! }
//! ```
//!
//! Pretty-printed, in the structs' field order, so a `git diff` of two
//! saves shows what changed. `schema` is read first: a newer one is
//! refused, an older one migrated. Unknown fields are refused, so a typo
//! in a hand edit is caught instead of dropped. Media paths are written
//! relative to the file's folder, which [`save`] and [`load`] take as the
//! kernel finds it, and [`save`] names each media path's folder so too
//! first (R8); they are read back lexically from that folder (D47). A
//! time's `seconds` and an orientation's
//! `ypr_deg` are written for readers; a hand edit that changes one without
//! the exact value beside it is refused, saying which to edit.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::OFlags;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::Project;
use super::validate::{Code, Problem};
use crate::blocking::may_block;
use crate::brand::{APP_SLUG, VERSION};
use crate::fs::TempFile;
use crate::time::Rational;

/// The `format` every project file names. A rename of the product after
/// files are out would have to read the old name too.
pub const FORMAT: &str = concat!(crate::brand::slug!(), ".project");
/// The schema this build writes, and the newest it reads: 2 since M3,
/// whose media have a source (M3 plan 11, D68).
pub const SCHEMA: u64 = 2;
/// A project file's extension.
pub const EXTENSION: &str = APP_SLUG;
/// The largest project file read: 10,000 keys on each track are a few MB.
pub const MAX_FILE_BYTES: u64 = 64 << 20;
/// How much of a media file its fingerprint hashes.
pub const HEAD_BYTES: u64 = 1 << 20;
/// How far a time's `seconds` may be from its `exact` value, in seconds,
/// before it is taken for a hand edit: 1 µs (D47).
const SECONDS_TOLERANCE: f64 = 1e-6;

#[derive(Debug, Error)]
pub enum FileError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: larger than {MAX_FILE_BYTES} bytes, so not a project file")]
    TooLarge { path: PathBuf },
    #[error("not JSON: {0}")]
    NotJson(serde_json::Error),
    #[error("not a Spherecut project: {0}")]
    NotProject(String),
    #[error("saved by a newer Spherecut (schema {found}); this one reads schemas up to {SCHEMA}")]
    NewerSchema { found: u64 },
    #[error("{0}")]
    Malformed(serde_json::Error),
    #[error("at {pointer}: {message}")]
    Time { pointer: String, message: String },
    #[error("the project breaks a rule: {0}")]
    Invalid(Problem),
    #[error("media {id}'s path {} isn't UTF-8, which a project file can't hold", .path.display())]
    PathNotUtf8 { id: u64, path: PathBuf },
}

impl FileError {
    /// A stable code for each kind of failure (A2's `project` tool).
    pub fn code(&self) -> &'static str {
        match self {
            FileError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => "not_found",
            FileError::Io { .. } => "io",
            FileError::TooLarge { .. } => "too_large",
            FileError::NotJson(_) | FileError::NotProject(_) => "not_project",
            FileError::NewerSchema { .. } => "newer_schema",
            FileError::Malformed(_) | FileError::Time { .. } | FileError::PathNotUtf8 { .. } => {
                "malformed"
            }
            FileError::Invalid(_) => "invalid",
        }
    }
}

#[derive(Serialize)]
struct EnvelopeOut<'a> {
    format: &'static str,
    schema: u64,
    saved_by: String,
    project: &'a Project,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeIn {
    #[allow(dead_code)]
    format: String,
    #[allow(dead_code)]
    schema: u64,
    #[allow(dead_code)]
    #[serde(default)]
    saved_by: String,
    project: Project,
}

/// `project` as a project file's text, its media paths relative to
/// `folder` (the folder the file will be in, absolute), each taken as it
/// is named.
pub fn to_json(project: &Project, folder: &Path) -> Result<String, FileError> {
    write_json(project, folder, lexical)
}

/// [`to_json`] for a file in `folder` as the kernel finds it, named
/// physically ([`crate::fs::real_file`], F031): each media path's folder
/// is named physically too before it is made relative
/// ([`crate::fs::in_real_folder`]), so that a media named through a
/// linked ancestor is stored from the folder it is really in rather than
/// climbing to the root and back (R8). The project's own paths are left
/// as they are. `file::save` and a folder from [`ProjectFolder::of_file`]
/// save so; an agent's folder, named lexically beneath its roots, doesn't.
///
/// [`ProjectFolder::of_file`]: super::ProjectFolder::of_file
pub(crate) fn to_json_from_real_folder(
    project: &Project,
    folder: &Path,
) -> Result<String, FileError> {
    write_json(project, folder, |p| {
        let real = crate::fs::in_real_folder(&lexical(p));
        // A link's target that isn't UTF-8 is left unread.
        if real.to_str().is_some() {
            real
        } else {
            lexical(p)
        }
    })
}

/// `project`'s text with each media path, as `name` names it, relative
/// to `folder`.
fn write_json(
    project: &Project,
    folder: &Path,
    name: impl Fn(&Path) -> PathBuf,
) -> Result<String, FileError> {
    let mut p = project.clone();
    let folder = lexical(folder);
    for m in &mut p.media {
        if m.path.to_str().is_none() {
            return Err(FileError::PathNotUtf8 {
                id: m.id.0,
                path: m.path.clone(),
            });
        }
        m.path = relative(&name(&m.path), &folder);
    }
    let envelope = EnvelopeOut {
        format: FORMAT,
        schema: SCHEMA,
        saved_by: format!("{APP_SLUG} {VERSION}"),
        project: &p,
    };
    let mut text = serde_json::to_string_pretty(&envelope).map_err(FileError::Malformed)?;
    text.push('\n');
    Ok(text)
}

/// Read a project file's text; relative media paths are taken against
/// `folder` (the file's folder, absolute).
pub fn from_json(text: &str, folder: &Path) -> Result<Project, FileError> {
    let value: Value = serde_json::from_str(text).map_err(FileError::NotJson)?;
    let Value::Object(top) = &value else {
        return Err(FileError::NotProject(
            "the file is not a JSON object".into(),
        ));
    };
    match top.get("format") {
        Some(Value::String(f)) if f == FORMAT => {}
        Some(other) => {
            return Err(FileError::NotProject(format!(
                "its format is {other}, not \"{FORMAT}\""
            )));
        }
        None => return Err(FileError::NotProject("it has no \"format\"".into())),
    }
    let schema = match top.get("schema") {
        Some(Value::Number(n)) => n.as_u64(),
        _ => None,
    };
    match schema {
        Some(s) if s > SCHEMA => return Err(FileError::NewerSchema { found: s }),
        // Schema 1's media have no source or caps, which read as equirect
        // and no caps (M3 plan 11.2), so it needs no migration of its own;
        // a later schema's migrations go here.
        Some(1 | SCHEMA) => {}
        _ => {
            return Err(FileError::NotProject(
                "its \"schema\" is not a schema number".into(),
            ));
        }
    }
    if let Some(project) = top.get("project") {
        check_sources(project, schema == Some(SCHEMA))?;
        check_times(project, "/project")?;
    }
    // Parsed again from the text, so an error says its line and column.
    let envelope: EnvelopeIn = serde_json::from_str(text).map_err(FileError::Malformed)?;
    let mut project = envelope.project;
    let folder = lexical(folder);
    for m in &mut project.media {
        m.path = lexical(&folder.join(&m.path));
    }
    project.validate().map_err(FileError::Invalid)?;
    Ok(project)
}

/// Save `project` to `path`: written beside it and renamed over it
/// (D16), so a crash leaves the old file or the new one.
pub fn save(project: &Project, path: &Path) -> Result<(), FileError> {
    may_block("saving a project");
    let io = |source| FileError::Io {
        path: path.to_path_buf(),
        source,
    };
    // Through a link the file is written where it points, so its paths
    // are relative to that folder, which load reads them from too: the
    // folder the kernel finds it in (F031), from which each media is
    // named as the kernel finds its folder too (R8).
    let dest = crate::fs::real_file(path).map_err(io)?;
    let folder = dest.parent().unwrap_or(Path::new("/"));
    let text = to_json_from_real_folder(project, folder)?;
    let mut tmp = TempFile::create(&dest).map_err(io)?;
    tmp.file_mut().write_all(text.as_bytes()).map_err(io)?;
    tmp.persist_replace().map_err(io)
}

/// Load the project file at `path`.
pub fn load(path: &Path) -> Result<Project, FileError> {
    may_block("loading a project");
    let io = |source| FileError::Io {
        path: path.to_path_buf(),
        source,
    };
    let path = std::path::absolute(path).map_err(io)?;
    // Without waiting: a pipe at the path would hold the open until a
    // writer came, and a folder or a device is no project (F034).
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NONBLOCK.bits() as i32)
        .open(&path)
        .map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(FileError::NotProject(
            "it is not a regular file (a folder, a pipe or a device, say)".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(FileError::TooLarge { path });
    }
    // Text that isn't UTF-8 is no project, as `ProjectFolder::load` says,
    // not an I/O error (F034).
    let text = String::from_utf8(bytes)
        .map_err(|_| FileError::NotProject("it isn't UTF-8 text".into()))?;
    // The real folder of the file itself, a link's target, as save
    // takes it (F031).
    let real = crate::fs::real_file(&path).map_err(io)?;
    let folder = real.parent().unwrap_or(Path::new("/"));
    from_json(&text, folder)
}

/// Each media's `source` and `caps` as its schema writes them (M3 plan
/// 11.2): every media of a schema 2 file has both, and not null, so a
/// damaged file never reads as equirect or as no caps by default (the
/// M3.9 review's N1), as a schema 1 media does; and no media of a schema
/// 1 file has either, so a file marked schema 1 that holds schema 2's
/// keys isn't read by schema 2's rules (N3). Read on the value, so that
/// the typed read after it keeps its line and column; what isn't a list
/// of objects is left to that read to refuse.
fn check_sources(project: &Value, schema_2: bool) -> Result<(), FileError> {
    let Some(Value::Array(media)) = project.get("media") else {
        return Ok(());
    };
    for m in media {
        let Value::Object(m) = m else { continue };
        let path = m.get("path").and_then(Value::as_str).unwrap_or("");
        let which = match m.get("id").and_then(Value::as_u64) {
            Some(id) => format!("media {id} ({path:?})"),
            None => format!("the media at {path:?}"),
        };
        let refuse =
            |message: String| Err(FileError::Invalid(Problem::new(Code::Invalid, message)));
        for (key, none) in [
            (
                "source",
                "{\"kind\": \"equirect\"} for a picture that is equirect already",
            ),
            ("caps", "{\"nadir\": null, \"zenith\": null} for none"),
        ] {
            let absent = matches!(m.get(key), None | Some(Value::Null));
            if schema_2 && absent {
                return refuse(format!(
                    "{which} has no \"{key}\": a schema 2 file writes it for every media, so \
                     this one was cut short or edited; give it its {key} ({none})"
                ));
            }
            if !schema_2 && m.contains_key(key) {
                return refuse(format!(
                    "{which} has a \"{key}\", which a schema 1 file never wrote: the file was \
                     edited or its \"schema\" mislabelled; mark it schema 2 if it is one"
                ));
            }
        }
    }
    Ok(())
}

/// Check every time in `v` (an object with `exact` and `seconds`): the
/// two must agree, since only `exact` is read, and nothing else may be
/// beside them, since the typed read would drop it unseen (D47 refuses
/// unknown fields).
fn check_times(v: &Value, pointer: &str) -> Result<(), FileError> {
    match v {
        Value::Object(map) => {
            if map.contains_key("exact") || map.contains_key("seconds") {
                let fail = |message: String| FileError::Time {
                    pointer: pointer.to_owned(),
                    message,
                };
                if let Some(k) = map
                    .keys()
                    .find(|k| !matches!(k.as_str(), "exact" | "seconds"))
                {
                    return Err(fail(format!(
                        "a time has \"exact\" and \"seconds\" only, not \"{k}\""
                    )));
                }
                let Some(exact) = map.get("exact") else {
                    return Err(fail(
                        "a time needs \"exact\" (\"seconds\" is only for reading)".into(),
                    ));
                };
                let (Value::String(e), Some(Value::Number(s))) = (exact, map.get("seconds")) else {
                    // Left for the typed read to refuse with its message.
                    return Ok(());
                };
                let (Ok(r), Some(s)) = (e.parse::<Rational>(), s.as_f64()) else {
                    return Ok(());
                };
                let x = r.to_f64();
                if (s - x).abs() > SECONDS_TOLERANCE {
                    return Err(fail(format!(
                        "\"seconds\" is {s} but \"exact\" is \"{e}\" ({x}); \"exact\" is the time, so edit it (and \"seconds\" with it, or leave \"seconds\" out)"
                    )));
                }
                return Ok(());
            }
            for (k, v) in map {
                check_times(
                    v,
                    &format!("{pointer}/{}", k.replace('~', "~0").replace('/', "~1")),
                )?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                check_times(v, &format!("{pointer}/{i}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// FNV-1a 64 of the first [`HEAD_BYTES`] of `path` (or all of it when
/// shorter), as 16 hex digits: a media file's [`Fingerprint::head`].
///
/// [`Fingerprint::head`]: super::Fingerprint::head
pub fn head_hash(path: &Path) -> io::Result<String> {
    may_block("reading a media file's start");
    let mut buf = Vec::new();
    File::open(path)?.take(HEAD_BYTES).read_to_end(&mut buf)?;
    Ok(format!("{:016x}", fnv1a64(&buf)))
}

/// FNV-1a 64 of `bytes`: the project's hashes, a media file's
/// [`Fingerprint::head`] and a saved project file's identity, and from M3
/// a camera calibration's (M3 plan 11.1). Public so that camera, media
/// and the app hash as the project does, and their 16 hex digits agree.
///
/// [`Fingerprint::head`]: super::Fingerprint::head
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What [`media_status`] found of a media file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaStatus {
    /// There, the same size and with the same first MiB.
    Present,
    /// Not there: the clip is offline.
    Missing,
    /// There, but not the file that was added: what differs.
    Changed(String),
}

/// Whether each media file is where the project says, and the same file
/// (its size and first MiB; the video's own numbers need a probe, which
/// the media crate adds). In the project's media order.
pub fn media_status(project: &Project) -> Vec<MediaStatus> {
    may_block("checking media files");
    project
        .media()
        .iter()
        .map(|m| match std::fs::metadata(&m.path) {
            Err(_) => MediaStatus::Missing,
            Ok(meta) if meta.len() != m.fingerprint.bytes => MediaStatus::Changed(format!(
                "its size is {} bytes, not {}",
                meta.len(),
                m.fingerprint.bytes
            )),
            Ok(_) => match head_hash(&m.path) {
                Err(e) => MediaStatus::Changed(format!("it can't be read: {e}")),
                Ok(h) if h != m.fingerprint.head => {
                    MediaStatus::Changed("its first MiB is different".into())
                }
                Ok(_) => MediaStatus::Present,
            },
        })
        .collect()
}

/// `p` without `.` and with each `..` taken against the component before
/// it, without looking at the file system (so a symlink is not
/// resolved): `/a/b/../c` is `/a/c`. `..` at the root stays at the root.
pub fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// `path` relative to `base`, both absolute and lexical, made from their
/// components: `/v/clips/a.mp4` from `/v/projects` is `../clips/a.mp4`.
pub fn relative(path: &Path, base: &Path) -> PathBuf {
    let p: Vec<Component> = path.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_paths() {
        assert_eq!(lexical(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
        assert_eq!(lexical(Path::new("/../a")), Path::new("/a"));
        assert_eq!(lexical(Path::new("a/../../b")), Path::new("../b"));
    }

    #[test]
    fn relative_paths() {
        let r = |p: &str, b: &str| relative(Path::new(p), Path::new(b));
        assert_eq!(
            r("/v/clips/a.mp4", "/v/projects"),
            Path::new("../clips/a.mp4")
        );
        assert_eq!(r("/v/projects/a.mp4", "/v/projects"), Path::new("a.mp4"));
        assert_eq!(
            r("/v/projects/sub/a.mp4", "/v/projects"),
            Path::new("sub/a.mp4")
        );
        // Nothing in common but the root: still relative.
        assert_eq!(
            r("/media/card/a.mp4", "/home/pat/p"),
            Path::new("../../../media/card/a.mp4")
        );
        // A component that merely starts the same is not in common.
        assert_eq!(
            r("/v/projectsX/a.mp4", "/v/projects"),
            Path::new("../projectsX/a.mp4")
        );
        for (p, b) in [
            ("/v/clips/a.mp4", "/v/projects"),
            ("/a.mp4", "/x/y/z"),
            ("/x/y/z/a", "/x"),
        ] {
            let back = lexical(&Path::new(b).join(r(p, b)));
            assert_eq!(back, Path::new(p));
        }
    }

    #[test]
    fn fnv_matches_its_reference_values() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
