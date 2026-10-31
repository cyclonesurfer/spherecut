//! `render` (a2-plan 5.1): a project's sequence checked as its export
//! would be, then queued as a job, answering within a second with
//! nothing rendered yet; `job` ([`super::job`]) follows it.
//!
//! The checks, in order: the project (from any root) and its clip's
//! media, there beneath the roots and the same file; the output's folder
//! beneath the write root, with nothing at the name and no render
//! waiting to write it; the range within the trim; the preset, frames
//! and encoder as [`spherecut_export::prepare`] finds them; and room on
//! the output's disk. The job then holds the media and the folder by
//! descriptor, so the export reads and writes what was checked (5.3).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use spherecut_core::Time;
use spherecut_core::fs::free_space;
use spherecut_core::project::{Aspect, Edit, Media, Output, file};
use spherecut_export::{ExportError, ExportJob};

use super::Ctx;
use super::check::Target;
use super::look::{self, TimeArg};
use super::project::{at, folder_error, time};
use crate::error::{ToolError, codes};
use crate::jobs::{Jobs, Render};
use crate::roots::Opened;
use crate::text;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub project: PathBuf,
    pub output: PathBuf,
    pub range: Option<Range>,
    #[serde(default)]
    pub draft: bool,
}

/// Part of the trim, in the clip's file time.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub from: Option<TimeArg>,
    pub to: Option<TimeArg>,
}

pub fn run(req: Request, cx: &Ctx, jobs: &Jobs) -> Result<Value, ToolError> {
    cx.call.check()?;
    let (folder, name) = cx.roots.project_folder("project", &req.project, false)?;
    let project = folder
        .load(&name)
        .map_err(|e| folder_error("project", e))?
        .project;
    let clip = project.the_clip().clone();
    let media = project
        .media_by_id(clip.media)
        .ok_or_else(|| ToolError::internal("the clip's media isn't in the project"))?
        .clone();
    let opened = media_file(cx, &media)?;
    cx.call.check()?;

    if !is_mp4(&req.output) {
        return Err(ToolError::new(codes::BAD_ARGUMENT, "isn't an .mp4 file's name")
            .at("output")
            .valid("a new file ending in .mp4"));
    }
    let out = cx.roots.output("output", &req.output)?;

    let mut project = project;
    if let Some(r) = &req.range {
        let (from, to) = range(r, clip.source.start, clip.source.end, media.video.rate)?;
        project
            .apply(&[Edit::SetTrim {
                clip: clip.id,
                start: Some(from),
                end: Some(to),
            }])
            .map_err(|e| ToolError::internal(format!("narrowing the trim: {e}")))?;
    }
    let target = target_of(&project.the_sequence().output);
    let proc = opened.proc_path().to_path_buf();
    let project = project.reading_media_from(|m| {
        if m.id == media.id {
            proc.clone()
        } else {
            m.path.clone()
        }
    });

    let gpu = look::gpu()?;
    let mut job = ExportJob::new(project, out.exact(), (*gpu).clone());
    job.exact = true;
    job.draft = req.draft;
    let prepared = spherecut_export::prepare(&job).map_err(|e| prepare_error(&e))?;
    let needed = spherecut_export::needed_bytes(&job, &prepared).map_err(|e| prepare_error(&e))?;
    let mut warnings = Vec::new();
    if let Some(w) = room(out.path(), needed, free_space(out.dir()))? {
        warnings.push(w);
    }
    cx.call.check()?;

    let frames = prepared.plan.count();
    let duration = prepared.plan.duration();
    let size = if req.draft {
        prepared.preset.draft_size()
    } else {
        prepared.preset.size()
    };
    let encoder = prepared.encoder();
    let output = out.path().to_path_buf();
    let folder_named = output.parent().map(Path::to_path_buf).unwrap_or_default();
    let paths = vec![
        (out.exact(), output.clone()),
        (out.dir().to_path_buf(), folder_named),
        (proc, media.path.clone()),
    ];
    let (id, ahead) = jobs.submit(
        output.clone(),
        Render {
            export: job,
            folder: out,
            media: vec![opened],
            paths,
            frames,
            encoder,
            target,
        },
    )?;
    let mut v = json!({
        "job": id,
        "output": text::field(&output.to_string_lossy()),
        "frames": frames,
        "duration": time(duration),
        "size": [size.0, size.1],
        "draft": req.draft,
        "encoder": encoder,
        "estimate_bytes": needed / 2,
        "ahead": ahead,
    });
    if !warnings.is_empty() {
        v["warnings"] = json!(warnings);
    }
    Ok(v)
}

/// Whether `path` names an MP4: the export writes no other kind.
fn is_mp4(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
}

/// The project's media file, opened beneath the roots and checked
/// against what the project recorded of it (D45, a2-plan amendment 6).
fn media_file(cx: &Ctx, m: &Media) -> Result<Opened, ToolError> {
    let named = text::field(&m.path.to_string_lossy());
    let missing = |why: &str| {
        ToolError::new(
            codes::MEDIA_MISSING,
            format!("the project's media {named} {why}"),
        )
        .at("project")
        .next("put the file back where the project says, or make a project of the file where it is")
    };
    let opened = match cx.roots.open_file("project", &m.path) {
        Ok(f) => f,
        Err(e) if e.code == codes::OUTSIDE_ROOTS => {
            return Err(missing("isn't beneath the roots the server reads"));
        }
        Err(e) if e.code == codes::NOT_FOUND => return Err(missing("isn't there")),
        Err(e) => return Err(missing(&format!("can't be read: {}", e.message))),
    };
    let changed = |why: String| {
        ToolError::new(
            codes::MEDIA_CHANGED,
            format!("the project's media {named} isn't the file it was made with: {why}"),
        )
        .at("project")
        .next("make a new project of the file as it is now")
    };
    if opened.size() != m.fingerprint.bytes {
        return Err(changed(format!(
            "its size is {} bytes, not {}",
            opened.size(),
            m.fingerprint.bytes
        )));
    }
    match file::head_hash(opened.source().open) {
        Err(e) => Err(changed(format!("it can't be read: {e}"))),
        Ok(h) if h != m.fingerprint.head => Err(changed("its first MiB is different".into())),
        Ok(_) => Ok(opened),
    }
}

/// `range` within the trim `start`..`end`, at the clip's rate: from and
/// to default to the trim's ends.
fn range(
    r: &Range,
    start: Time,
    end: Time,
    rate: spherecut_core::FrameRate,
) -> Result<(Time, Time), ToolError> {
    let from = r
        .from
        .as_ref()
        .map(|t| at("range.from", t, Some(rate)))
        .transpose()?
        .unwrap_or(start);
    let to = r
        .to
        .as_ref()
        .map(|t| at("range.to", t, Some(rate)))
        .transpose()?
        .unwrap_or(end);
    let valid = || {
        format!(
            "from {} to {} s, the trim",
            seconds(start),
            seconds(end)
        )
    };
    for (arg, t) in [("range.from", from), ("range.to", to)] {
        if t < start || t > end {
            return Err(ToolError::new(codes::OUT_OF_RANGE, "is outside the clip's trim")
                .at(arg)
                .valid(valid())
                .next("set the trim with edit first to render past it"));
        }
    }
    if from >= to {
        return Err(
            ToolError::new(codes::OUT_OF_RANGE, "ends where it starts, or before")
                .at("range.to")
                .valid(valid()),
        );
    }
    Ok((from, to))
}

fn seconds(t: Time) -> String {
    let s = format!("{:.3}", t.to_secs_f64());
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The target a render's output fits, for `job`'s check: a 360 upload
/// for the whole sphere, flat 16:9 and vertical by their shapes, and the
/// rules alone for 1:1 and 4:5 (D46).
fn target_of(o: &Output) -> Option<Target> {
    match o {
        Output::Equirect { .. } => Some(Target::Youtube360),
        Output::Flat {
            aspect: Aspect::Wide,
            ..
        } => Some(Target::Flat16x9),
        Output::Flat {
            aspect: Aspect::Portrait,
            ..
        } => Some(Target::Vertical),
        Output::Flat { .. } => None,
    }
}

/// [`spherecut_export::prepare`]'s refusal: D51's codes.
fn prepare_error(e: &ExportError) -> ToolError {
    let err = ToolError::new(e.code(), e.to_string()).at("project");
    match e {
        ExportError::Invalid(_) => {
            err.next("read the project's sequence part: its output and trim decide the export")
        }
        ExportError::NoEncoder(_) => {
            err.next("choose another codec with edit's set_export, or ask the user to install it")
        }
        _ => err,
    }
}

/// Err `no_space` when the folder has less than `needed` free, with both
/// numbers; a warning when it couldn't say, as the export's own check
/// gives.
fn room(
    output: &Path,
    needed: u64,
    free: std::io::Result<u64>,
) -> Result<Option<Value>, ToolError> {
    let folder = output
        .parent()
        .map(|p| text::field(&p.to_string_lossy()))
        .unwrap_or_default();
    match free {
        Ok(free) if free < needed => Err(ToolError::new(
            codes::NO_SPACE,
            format!(
                "{folder} has {free} bytes free, and the render may need {needed}: two copies \
                 of the file at its preset's highest bitrate are on the disk while it finishes"
            ),
        )
        .at("output")
        .next("free some space there, render a shorter range or a draft, or render elsewhere")),
        Ok(_) => Ok(None),
        Err(e) => Ok(Some(json!({
            "code": "free_space_unknown",
            "message": format!("{folder} couldn't say how much room it has ({e}), so it wasn't checked"),
        }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_without_room_is_refused_with_both_numbers() {
        let out = Path::new("/w/out/a.mp4");
        let e = room(out, 2_000, Ok(1_999)).unwrap_err();
        assert_eq!(e.code, codes::NO_SPACE);
        assert_eq!(e.argument.as_deref(), Some("output"));
        assert!(e.message.contains("1999 bytes free"), "{}", e.message);
        assert!(e.message.contains("need 2000"), "{}", e.message);
        assert!(e.message.starts_with("/w/out "), "{}", e.message);
        assert_eq!(room(out, 2_000, Ok(2_000)), Ok(None));
        let unknown = room(out, 2_000, Err(std::io::Error::from_raw_os_error(38)))
            .unwrap()
            .unwrap();
        assert_eq!(unknown["code"], "free_space_unknown");
    }

    #[test]
    fn a_range_is_inside_the_trim() {
        let rate = spherecut_core::FrameRate::FPS_30;
        let s = |x: i64| Time::from_secs(x);
        let t = |x: &str| Some(TimeArg::Text(x.into()));
        let r = |from, to| Range { from, to };
        assert_eq!(range(&r(None, None), s(1), s(3), rate), Ok((s(1), s(3))));
        assert_eq!(
            range(&r(t("1.5"), t("60f")), s(1), s(3), rate),
            Ok((Time::seconds(spherecut_core::Rational::new(3, 2).unwrap()), s(2)))
        );
        for (from, to, arg) in [
            (t("0.5"), None, "range.from"),
            (None, t("3.1"), "range.to"),
            (t("2"), t("2"), "range.to"),
            (t("2.5"), t("2"), "range.to"),
        ] {
            let e = range(&r(from, to), s(1), s(3), rate).unwrap_err();
            assert_eq!(e.code, codes::OUT_OF_RANGE);
            assert_eq!(e.argument.as_deref(), Some(arg));
            assert_eq!(e.valid.as_deref(), Some("from 1 to 3 s, the trim"));
        }
    }

    #[test]
    fn each_output_fits_its_target() {
        use spherecut_core::project::Orientation;
        let flat = |aspect| Output::Flat {
            aspect,
            short_side: 1080,
        };
        assert_eq!(
            target_of(&Output::Equirect {
                width: 3840,
                orientation: Orientation::IDENTITY
            }),
            Some(Target::Youtube360)
        );
        assert_eq!(target_of(&flat(Aspect::Wide)), Some(Target::Flat16x9));
        assert_eq!(target_of(&flat(Aspect::Portrait)), Some(Target::Vertical));
        assert_eq!(target_of(&flat(Aspect::Square)), None);
        assert_eq!(target_of(&flat(Aspect::Tall)), None);
    }

    #[test]
    fn only_mp4_is_rendered() {
        assert!(is_mp4(Path::new("a/b.mp4")));
        assert!(is_mp4(Path::new("B.MP4")));
        assert!(!is_mp4(Path::new("b.mov")));
        assert!(!is_mp4(Path::new("mp4")));
    }
}
