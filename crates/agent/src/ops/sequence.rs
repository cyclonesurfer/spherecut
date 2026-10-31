//! A project's sequence for the tools that render or measure it (a2-plan
//! 5.1 and 7, D59): the project and its clip's media loaded beneath the
//! roots and checked against what the project recorded; the frames a
//! render makes from a time; and each of those frames rendered as the
//! export renders it, through `frame --project`'s views
//! ([`frame_views_at`]) with the export's filter, at any size.
//!
//! Times on a sequence are the render's: 0 is its first frame, which
//! shows the clip's media at the trim's start (or at `from`).

use std::path::Path;

use spherecut_core::project::{Media, Output, Project, Range, file};
use spherecut_core::{FrameRate, Rounding, Time, TimeSpec};
use spherecut_export::FramePlan;
use spherecut_media::backend::SeekMode;
use spherecut_render::output::{FrameViews, frame_views_at};
use spherecut_render::{Depth, Filter, Gpu, Image};

use super::Ctx;
use super::project::folder_error;
use crate::contract::Warning;
use crate::error::{ToolError, codes};
use crate::roots::Opened;
use crate::still::{self, Decoded, Failure, Still};
use crate::text;

/// A project as a render reads it.
#[derive(Debug)]
pub struct Loaded {
    pub project: Project,
    /// The clip's media, as the project records it.
    pub media: Media,
    /// That file, opened beneath the roots.
    pub opened: Opened,
}

/// The project at `path`, the argument `arg`, beneath any root, and its
/// clip's media beneath the read roots, the file it was made with.
pub fn load(cx: &Ctx, arg: &str, path: &Path) -> Result<Loaded, ToolError> {
    let (folder, name) = cx.roots.project_folder(arg, path, false)?;
    let project = folder
        .load(&name)
        .map_err(|e| folder_error(arg, e))?
        .project;
    let media = project
        .media_by_id(project.the_clip().media)
        .ok_or_else(|| ToolError::internal("the clip's media isn't in the project"))?
        .clone();
    let opened = media_file(cx, arg, &media)?;
    Ok(Loaded {
        project,
        media,
        opened,
    })
}

/// The project's media file, opened beneath the roots and checked
/// against what the project recorded of it (D45, a2-plan amendment 6).
fn media_file(cx: &Ctx, arg: &str, m: &Media) -> Result<Opened, ToolError> {
    let named = text::field(&m.path.to_string_lossy());
    let missing = |why: &str| {
        ToolError::new(
            codes::MEDIA_MISSING,
            format!("the project's media {named} {why}"),
        )
        .at(arg)
        .next("put the file back where the project says, or make a project of the file where it is")
    };
    let opened = match cx.roots.open_file(arg, &m.path) {
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
        .at(arg)
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

/// The frames a render of a project makes, from `from` to the trim's
/// end: frame `n` is at `n / rate` in the render and shows the clip's
/// media at `from + n / rate`.
#[derive(Debug, Clone)]
pub struct Frames {
    plan: FramePlan,
    output: Output,
    blur: bool,
}

impl Frames {
    /// The frames of `project`'s render from `from` (the argument
    /// `arg`, the clip's media time), the trim's start when `None`.
    pub fn new(project: &Project, from: Option<Time>, arg: &str) -> Result<Frames, ToolError> {
        let sequence = project.the_sequence();
        let trim = project.the_clip().source;
        let start = from.unwrap_or(trim.start);
        if start < trim.start || start >= trim.end {
            return Err(
                ToolError::new(codes::OUT_OF_RANGE, "is outside the clip's trim")
                    .at(arg)
                    .valid(format!(
                        "from {} s to before {} s, the trim",
                        still::seconds(trim.start),
                        still::seconds(trim.end)
                    )),
            );
        }
        let plan = FramePlan::new(
            Range {
                start,
                end: trim.end,
            },
            sequence.rate,
        )
        .map_err(|e| ToolError::new(codes::OUT_OF_RANGE, e.to_string()).at(arg))?;
        Ok(Frames {
            plan,
            output: sequence.output,
            blur: sequence.export.motion_blur,
        })
    }

    pub fn count(&self) -> u64 {
        self.plan.count()
    }

    pub fn rate(&self) -> FrameRate {
        self.plan.rate()
    }

    /// How long the render lasts.
    pub fn duration(&self) -> Time {
        self.plan.duration()
    }

    /// The output's width over its height.
    pub fn aspect(&self) -> f64 {
        let (w, h) = self.output.size();
        f64::from(w) / f64::from(h.max(1))
    }

    /// The frame on screen at `when` in the render (the last that starts
    /// at or before a time), or `None` past its end.
    pub fn at(&self, when: TimeSpec) -> Option<u64> {
        let n = match when {
            TimeSpec::Frame(n) => n,
            TimeSpec::Time(t) => {
                if t < Time::ZERO {
                    return None;
                }
                u64::try_from(self.rate().time_to_frame(t, Rounding::Floor).ok()?).ok()?
            }
        };
        (n < self.count()).then_some(n)
    }

    /// Frame `n`'s time in the render.
    pub fn shown(&self, n: u64) -> Time {
        self.plan.sequence_time(n).unwrap_or(Time::ZERO)
    }

    /// The media time frame `n` shows.
    pub fn source(&self, n: u64) -> Time {
        self.plan.source_time(n).unwrap_or(Time::ZERO)
    }

    /// The views frame `n` is rendered through at `size`.
    pub fn views(&self, project: &Project, n: u64, size: (u32, u32)) -> FrameViews {
        let clip = project.the_clip();
        frame_views_at(
            &clip.view,
            project.targets(),
            &self.output,
            size,
            self.source(n).to_secs_f64(),
            self.rate().frame_duration().to_secs_f64(),
            self.blur,
        )
    }
}

/// The export's filter on `gpu` (D49): anisotropic where it filters so.
pub fn filter(gpu: &Gpu) -> Filter {
    if gpu.caps.anisotropy {
        Filter::Anisotropic
    } else {
        Filter::Isotropic
    }
}

/// A project's media open for its frames.
#[derive(Debug)]
pub struct Renderer<'g> {
    still: Still<'g>,
    filter: Filter,
}

impl<'g> Renderer<'g> {
    /// `loaded`'s media, its stream the project's, opened for stills on
    /// `gpu`.
    pub fn open(
        loaded: &Loaded,
        gpu: &'g Gpu,
        stop: &still::Stop,
        warnings: &mut Vec<Warning>,
    ) -> Result<Renderer<'g>, Failure> {
        let opts = still::Options {
            stream: Some(loaded.media.video.stream as usize),
            equirect: true,
            ..still::Options::default()
        };
        let src = loaded.opened.source();
        let probed = still::probe(src, &opts, stop, warnings)?;
        let still = Still::open(src, &probed, Some(gpu), &opts, stop, warnings)?;
        Ok(Renderer {
            still,
            filter: filter(gpu),
        })
    }

    /// The media's frame at `t`, exactly.
    pub fn seek(&mut self, t: Time, warnings: &mut Vec<Warning>) -> Result<Decoded, Failure> {
        self.still
            .seek(TimeSpec::Time(t), SeekMode::Exact, warnings)
    }

    /// `decoded` through `views`, at their size, in `depth`.
    pub fn render(
        &mut self,
        decoded: &Decoded,
        views: &FrameViews,
        depth: Depth,
    ) -> Result<Image, Failure> {
        let look = still::Look {
            draw: still::Draw::View,
            filter: self.filter,
            depth,
        };
        self.still.render(decoded, &views.views, views.size, look)
    }

    pub fn still(&self) -> &Still<'g> {
        &self.still
    }
}
