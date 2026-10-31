//! The reframer's project (M2 plan sections 10.1 to 10.3; amendment 2
//! items 20, 21, 29 and 36): the project open in the window, made
//! untitled from a video or loaded from a file, saved and closed, with
//! the file work on jobs; the viewer's keyed view, its look-around, and
//! the output frame it draws; and the trim the player keeps to.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use spherecut_core::Time;
use spherecut_core::project::file::{self, MediaStatus};
use spherecut_core::project::{
    Edit, Fingerprint, InitialView, Media, MediaId, MediaVideo, Orientation, Output, Project,
};
use spherecut_media::backend::{SeekMode, Timing, VideoStreamInfo};
use spherecut_media::doctor::Severity;
use spherecut_playback::{PlaySpan, PlayState};
use spherecut_render::Framing;
use spherecut_sphere::perspective::MIN_HFOV_DEG;

use crate::app::AppState;
use crate::doc::{Doc, MAX_FOV_DEG, ViewValues};
use crate::jobs::{Job, Poll, Threads};
use crate::notices::{Notice, NoticeKind};

/// How long a change of the view that isn't a drag (a scroll, a key, a
/// projection switch) rests before it becomes key edits.
pub const SETTLE: Duration = Duration::from_millis(400);
/// The output frame's share of the viewer, on the side it fills.
pub const FRAME_FILL: f64 = 0.9;
/// What is past the output frame is multiplied by this, in linear light.
pub const FRAME_DIM: f64 = 0.25;
/// The badge a look-around shows.
pub const NOT_KEYED: &str = "Not keyed: Alt+K to key";

/// What the viewer shows of the view (section 10.2).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Look {
    /// The keys' view at the frame on screen: what plays is what exports.
    #[default]
    Keyed,
    /// Being changed, from the frame at `at`, last moved at `moved`: key
    /// edits, or a look-around, once the drag ends or it rests.
    Editing { at: Time, moved: Instant },
    /// A change no key takes: shown, not keyed, until the next play,
    /// seek or step.
    Around,
}

impl Look {
    /// The overlay's word for it.
    pub fn label(self) -> &'static str {
        match self {
            Look::Keyed => "keyed",
            Look::Editing { .. } => "editing a key",
            Look::Around => "look-around (not keyed)",
        }
    }
}

/// A step that drops the open project, held while its unsaved changes
/// are asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Open a video, or a project file.
    Open(PathBuf),
    Close,
    Quit,
}

/// The Save as window's folder and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveAs {
    pub dir: PathBuf,
    pub name: String,
}

#[derive(Debug)]
enum Saved {
    Done,
    /// The file is there, and the save wasn't to replace it.
    Exists,
    Failed(String),
}

#[derive(Debug)]
struct Saving {
    job: Job<Saved>,
    path: PathBuf,
    /// The revision saved, and the project it was of.
    revision: u64,
    generation: u64,
}

type Loaded = Result<(Project, Vec<MediaStatus>), String>;

/// An untitled project being made of the video just opened.
#[derive(Debug)]
enum Making {
    /// Waiting for its first stream to open.
    Opening(PathBuf),
    /// Its file's size and first MiB being read.
    Fingerprint(Job<Result<Media, String>>),
    /// Read, waiting for a drag or a projection switch to end: the
    /// project starts with the view on screen.
    Ready(Media),
}

/// The open project, and what the viewer and the file work make of it.
#[derive(Debug, Default)]
pub struct Reframe {
    pub doc: Option<Doc>,
    /// Bumped by each project dropped, so a job's result for an older
    /// one is known.
    generation: u64,
    pub look: Look,
    /// Play loops between in and out (`Ctrl+L`).
    pub looping: bool,
    /// The values the viewer last took from the keys, to tell a change
    /// from them, and those it last had, to time a change's rest.
    applied: Option<ViewValues>,
    seen: Option<ViewValues>,
    /// A drag of the picture ended this frame.
    pub released: bool,
    /// The span the player was given since its last open.
    span: Option<PlaySpan>,
    making: Option<Making>,
    loading: Option<(PathBuf, Job<Loaded>)>,
    saving: Option<Saving>,
    pub save_as: Option<SaveAs>,
    /// A step waiting on "Save changes?".
    pub confirm: Option<Pending>,
    /// The step to take once the save under way is done.
    after_save: Option<Pending>,
    /// A save that would replace a file, waiting on "Replace it?".
    pub replace: Option<PathBuf>,
    threads: Threads,
    pub strip: crate::timeline::Strip,
    /// The window may close: nothing is unsaved, or the user said so.
    pub may_close: bool,
    /// The window is to close, now that it may.
    pub quit: bool,
}

impl Reframe {
    /// A load, a save or an untitled project's making is under way.
    pub fn pending(&self) -> bool {
        self.making.is_some() || self.loading.is_some() || self.saving.is_some()
    }

    pub fn saving(&self) -> bool {
        self.saving.is_some()
    }

    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// The flat output's size: the viewer frames it and shows the keyed
    /// view. A 360 output's viewer looks around freely, as M1's did.
    pub fn flat_output(&self) -> Option<(u32, u32)> {
        let doc = self.doc.as_ref()?;
        let out = doc.project.the_sequence().output;
        matches!(out, Output::Flat { .. }).then(|| out.size())
    }

    /// The jobs' threads still running, for the join at exit.
    pub fn take_threads(&mut self) -> Vec<JoinHandle<()>> {
        self.keep_jobs();
        if let Some(s) = self.saving.take() {
            self.threads.keep(s.job);
        }
        self.threads.take()
    }

    /// The project let go of, unsaved changes and all, and any being
    /// made or loaded. A save under way goes on, for an older project.
    pub(crate) fn let_go(&mut self) {
        self.doc = None;
        self.generation += 1;
        self.look = Look::Keyed;
        self.applied = None;
        self.seen = None;
        self.keep_jobs();
        self.save_as = None;
        self.replace = None;
        self.after_save = None;
        self.strip = crate::timeline::Strip::default();
    }

    fn keep_jobs(&mut self) {
        if let Some(Making::Fingerprint(job)) = self.making.take() {
            self.threads.keep(job);
        }
        if let Some((_, job)) = self.loading.take() {
            self.threads.keep(job);
        }
    }

    /// The player was opened afresh, which drops its span.
    pub(crate) fn span_reset(&mut self) {
        self.span = None;
    }
}

/// Whether `path` is a project file, by its extension.
pub fn is_project_file(path: &Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| x.eq_ignore_ascii_case(file::EXTENSION))
}

/// The output frame of a `out`-sized output in a `size`-pixel viewer:
/// as large as [`FRAME_FILL`] of it allows, centred, on whole pixels.
pub fn frame_rect(size: (u32, u32), out: (u32, u32)) -> [f64; 4] {
    let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let (ow, oh) = (f64::from(out.0.max(1)), f64::from(out.1.max(1)));
    let scale = (FRAME_FILL * w / ow).min(FRAME_FILL * h / oh);
    let fw = (ow * scale).round().max(1.0);
    let fh = (oh * scale).round().max(1.0);
    [((w - fw) / 2.0).round(), ((h - fh) / 2.0).round(), fw, fh]
}

/// The media of the video at `path`, whose first stream opened as
/// `info`, for its untitled project. On a worker: it reads the file's
/// size and start.
fn untitled_media(path: &Path, info: &VideoStreamInfo) -> Result<Media, String> {
    let rate = match info.timing {
        Timing::Constant(r) | Timing::Variable { average: Some(r) } => r,
        Timing::Variable { average: None } => return Err("its frame rate isn't known".into()),
    };
    let duration = info.duration.ok_or("its length isn't known")?;
    let bytes = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    let head = file::head_hash(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Media {
        id: MediaId::NEW,
        path: std::path::absolute(path).map_err(|e| e.to_string())?,
        fingerprint: Fingerprint { bytes, head },
        video: MediaVideo {
            stream: u32::try_from(info.index).map_err(|e| e.to_string())?,
            width: info.size.0,
            height: info.size.1,
            rate,
            duration,
        },
    })
}

impl AppState {
    /// Open a video, which makes an untitled project of it, or a project
    /// file, asking first if the open project has unsaved changes.
    pub fn open(&mut self, path: PathBuf) {
        self.unless_unsaved(Pending::Open(path));
    }

    /// File ▸ Close: the project and its video.
    pub fn close_project(&mut self) {
        self.unless_unsaved(Pending::Close);
    }

    /// The window was asked to close: whether it may now. With unsaved
    /// changes it asks, and closes once they are saved or let go.
    pub fn close_requested(&mut self) -> bool {
        if self.reframe.may_close || !self.reframe.doc.as_ref().is_some_and(Doc::dirty) {
            return true;
        }
        self.reframe.confirm = Some(Pending::Quit);
        false
    }

    fn unless_unsaved(&mut self, step: Pending) {
        if self.reframe.doc.as_ref().is_some_and(Doc::dirty) {
            self.reframe.confirm = Some(step);
        } else {
            self.take_step(step);
        }
    }

    /// "Save changes?" answered: save first, or not.
    pub fn answer_confirm(&mut self, save: bool) {
        let Some(step) = self.reframe.confirm.take() else {
            return;
        };
        if !save {
            self.take_step(step);
            return;
        }
        self.reframe.after_save = Some(step);
        self.save();
    }

    fn take_step(&mut self, step: Pending) {
        match step {
            Pending::Open(path) if is_project_file(&path) => self.load_project(path),
            Pending::Open(path) => {
                self.drop_doc();
                self.open_media(path.clone(), vec![None]);
                self.reframe.making = Some(Making::Opening(path));
            }
            Pending::Close => {
                self.drop_doc();
                self.close_media();
            }
            Pending::Quit => {
                self.reframe.may_close = true;
                self.reframe.quit = true;
            }
        }
    }

    /// The project let go of, unsaved changes and all.
    fn drop_doc(&mut self) {
        self.reframe.let_go();
        self.notices.retain(|k| {
            !matches!(
                k,
                NoticeKind::Project | NoticeKind::MediaOffline | NoticeKind::MediaChanged
            )
        });
    }

    fn set_doc(&mut self, doc: Doc) {
        let r = &mut self.reframe;
        r.doc = Some(doc);
        r.look = Look::Keyed;
        r.applied = None;
        r.seen = None;
    }

    /// The video closed: the player empty, the panes forgetting it.
    fn close_media(&mut self) {
        self.forget_file();
        self.info.close();
        self.view.reset();
        if let Some(p) = &self.player {
            p.close();
        }
        self.reframe.span_reset();
    }

    fn load_project(&mut self, path: PathBuf) {
        self.drop_doc();
        self.close_media();
        let p = path.clone();
        let job = Job::spawn("load project", move || {
            let project = file::load(&p).map_err(|e| e.to_string())?;
            let status = file::media_status(&project);
            Ok((project, status))
        });
        self.reframe.loading = Some((path, job));
    }

    /// File ▸ Save: to the project's file, or by Save as when it has none.
    pub fn save(&mut self) {
        let Some(doc) = &self.reframe.doc else {
            return;
        };
        match doc.path.clone() {
            Some(path) => self.save_to(path, true),
            None => self.open_save_as(),
        }
    }

    /// File ▸ Save as: the window, in the project's folder (else the
    /// Files pane's, else the video's).
    pub fn open_save_as(&mut self) {
        let Some(doc) = &self.reframe.doc else {
            return;
        };
        let dir = doc
            .path
            .as_deref()
            .and_then(Path::parent)
            .or_else(|| self.files.dir())
            .or_else(|| doc.media().and_then(|m| m.path.parent()))
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let stem = doc
            .path
            .as_deref()
            .and_then(Path::file_stem)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| doc.project.the_sequence().name.clone());
        self.reframe.save_as = Some(SaveAs {
            dir,
            name: format!("{stem}.{}", file::EXTENSION),
        });
    }

    /// Save the project to `path`, on a worker; a file already there is
    /// replaced only when `replace` says so, else it is asked about.
    pub fn save_to(&mut self, path: PathBuf, replace: bool) {
        let Some(doc) = &self.reframe.doc else {
            return;
        };
        if self.reframe.saving.is_some() {
            return;
        }
        let project = doc.project.clone();
        let revision = project.revision();
        let p = path.clone();
        let job = Job::spawn("save project", move || {
            if !replace && p.symlink_metadata().is_ok() {
                return Saved::Exists;
            }
            match file::save(&project, &p) {
                Ok(()) => Saved::Done,
                Err(e) => Saved::Failed(e.to_string()),
            }
        });
        self.reframe.saving = Some(Saving {
            job,
            path,
            revision,
            generation: self.reframe.generation,
        });
    }

    /// The project's jobs polled and the player's span kept to its trim.
    /// Never waits.
    pub(crate) fn reframe_tick(&mut self) {
        self.poll_making();
        self.poll_loading();
        self.poll_saving();
        self.sync_span();
    }

    fn poll_making(&mut self) {
        match self.reframe.making.take() {
            None => {}
            Some(Making::Opening(path)) => {
                if self.snapshot.path.as_ref() != Some(&path) {
                    self.reframe.making = Some(Making::Opening(path));
                    return;
                }
                if self.snapshot.state == PlayState::Failed {
                    return;
                }
                let Some(info) = self.stream0().cloned() else {
                    self.reframe.making = Some(Making::Opening(path));
                    return;
                };
                // A raw view's pictures (lenses, a flat source) have no
                // view to key until M3 stitches them.
                if self.raw_view() {
                    return;
                }
                let job = Job::spawn("untitled project", move || untitled_media(&path, &info));
                self.reframe.making = Some(Making::Fingerprint(job));
            }
            Some(Making::Fingerprint(job)) => match job.poll() {
                Poll::Pending => self.reframe.making = Some(Making::Fingerprint(job)),
                Poll::Done(Ok(media)) => self.reframe.making = Some(Making::Ready(media)),
                Poll::Done(Err(why)) => self.no_project(why),
                Poll::Lost => tracing::error!("making the untitled project ended without one"),
            },
            Some(Making::Ready(media)) => {
                if self.drag.is_some() || self.view.animating() {
                    self.reframe.making = Some(Making::Ready(media));
                    return;
                }
                match Doc::untitled(media, self.initial_view()) {
                    Ok(doc) => self.set_doc(doc),
                    Err(p) => self.no_project(p.to_string()),
                }
            }
        }
    }

    fn no_project(&mut self, why: String) {
        self.notices.push(
            Notice::new(
                NoticeKind::Project,
                Severity::Warning,
                "This video can't be reframed",
            )
            .impact(format!("{why}; it plays, but makes no project")),
        );
    }

    /// The view on screen, as an untitled project's one key on each
    /// track: the projection nearest a morph's.
    fn initial_view(&self) -> InitialView {
        let lens = self.view.lens;
        InitialView {
            orientation: Orientation::new(self.view.q).unwrap_or(Orientation::IDENTITY),
            fov_deg: lens.hfov.to_degrees().clamp(MIN_HFOV_DEG, MAX_FOV_DEG),
            projection: if lens.morph > 0.5 { lens.to } else { lens.from },
        }
    }

    fn poll_loading(&mut self) {
        let Some((path, job)) = self.reframe.loading.take() else {
            return;
        };
        let (project, status) = match job.poll() {
            Poll::Pending => {
                self.reframe.loading = Some((path, job));
                return;
            }
            Poll::Done(Ok(loaded)) => loaded,
            Poll::Done(Err(why)) => {
                self.notices.push(
                    Notice::new(
                        NoticeKind::Project,
                        Severity::Error,
                        "This project can't be opened",
                    )
                    .impact(why),
                );
                return;
            }
            Poll::Lost => {
                tracing::error!("loading {} ended without a result", path.display());
                return;
            }
        };
        let doc = Doc::new(project, Some(path));
        let media = doc.media().cloned();
        let status = media
            .as_ref()
            .and_then(|m| doc.project.media().iter().position(|x| x.id == m.id))
            .and_then(|i| status.get(i).cloned());
        self.set_doc(doc);
        let Some(m) = media else {
            return;
        };
        if status == Some(MediaStatus::Missing) {
            self.notices.push(
                Notice::new(
                    NoticeKind::MediaOffline,
                    Severity::Error,
                    "The project's video is missing",
                )
                .impact(format!(
                    "{} isn't there; the keys and trims are kept",
                    m.path.display()
                ))
                .fix(format!(
                    "Put the video back at {} and open the project again",
                    m.path.display()
                )),
            );
            return;
        }
        self.open_media(m.path.clone(), vec![Some(m.video.stream as usize)]);
        if let Some(MediaStatus::Changed(why)) = status {
            self.notices.push(
                Notice::new(
                    NoticeKind::MediaChanged,
                    Severity::Warning,
                    "The project's video has changed since it was added",
                )
                .impact(format!("{why}; the keys and trims may not fit it")),
            );
        }
    }

    fn poll_saving(&mut self) {
        let Some(s) = self.reframe.saving.take() else {
            return;
        };
        let failed = |why: String| {
            Notice::new(
                NoticeKind::Project,
                Severity::Error,
                "The project can't be saved",
            )
            .impact(why)
        };
        match s.job.poll() {
            Poll::Pending => self.reframe.saving = Some(s),
            Poll::Done(Saved::Done) => {
                if s.generation == self.reframe.generation
                    && let Some(doc) = &mut self.reframe.doc
                {
                    doc.saved_to(s.path, s.revision);
                }
                self.notices.retain(|k| *k != NoticeKind::Project);
                if let Some(step) = self.reframe.after_save.take() {
                    self.take_step(step);
                }
            }
            Poll::Done(Saved::Exists) => self.reframe.replace = Some(s.path),
            Poll::Done(Saved::Failed(why)) => {
                self.reframe.after_save = None;
                self.notices.push(failed(why));
            }
            Poll::Lost => {
                self.reframe.after_save = None;
                self.notices
                    .push(failed("the save ended without a result".into()));
            }
        }
    }

    /// The player plays the trim, or the whole video without a project.
    fn sync_span(&mut self) {
        let want = self.reframe.doc.as_ref().map(|d| {
            let r = d.clip().source;
            PlaySpan {
                start: r.start,
                end: r.end,
                looping: self.reframe.looping,
            }
        });
        if want != self.reframe.span
            && let Some(p) = &self.player
        {
            p.set_span(want);
            self.reframe.span = want;
        }
    }

    /// The source time of the frame on screen: what the keyed view and
    /// the keys' edits are at.
    pub fn frame_time(&self) -> Time {
        self.presented
            .as_ref()
            .and_then(|p| p.frames.first())
            .map(|f| f.frame.time)
            .or_else(|| {
                self.snapshot
                    .shown
                    .first()
                    .copied()
                    .flatten()
                    .map(|f| f.time)
            })
            .unwrap_or(self.snapshot.clock)
    }

    /// The end of the frame on screen, where `O` puts out.
    pub fn frame_end(&self) -> Time {
        let t = self.frame_time();
        let d = self
            .snapshot
            .shown
            .first()
            .copied()
            .flatten()
            .and_then(|f| f.duration)
            .or_else(|| self.reframe.doc.as_ref().map(|d| d.rate().frame_duration()))
            .unwrap_or(Time::ZERO);
        t.checked_add(d).unwrap_or(t)
    }

    /// The viewer's output frame, in a `size`-pixel viewer.
    pub fn framing(&self, size: (u32, u32)) -> Option<Framing> {
        let out = self.reframe.flat_output()?;
        Some(Framing {
            rect: frame_rect(size, out),
            dim: FRAME_DIM,
        })
    }

    /// A change of the view since the keys set it, timed: a drag, a
    /// scroll, a projection switch, `R`. While paused it becomes key
    /// edits once it ends; while playing it is a look-around.
    pub(crate) fn watch_view(&mut self, now: Instant) {
        let released = std::mem::take(&mut self.reframe.released);
        if self.reframe.flat_output().is_none() {
            return;
        }
        let vals = ViewValues::of(self.view.q, &self.view.lens);
        let moved = self
            .reframe
            .seen
            .is_some_and(|s| !s.changed(&vals).is_empty());
        let busy = self.drag.is_some() || self.view.animating();
        self.reframe.seen = Some(vals);
        match self.reframe.look {
            Look::Keyed => {
                let off = self
                    .reframe
                    .applied
                    .is_some_and(|a| !a.changed(&vals).is_empty());
                if off || busy {
                    self.reframe.look = if self.snapshot.want_play {
                        Look::Around
                    } else {
                        Look::Editing {
                            at: self.frame_time(),
                            moved: now,
                        }
                    };
                }
            }
            Look::Editing { at, moved: last } => {
                if busy || (moved && !released) {
                    self.reframe.look = Look::Editing { at, moved: now };
                } else if released || now.duration_since(last) >= SETTLE {
                    self.commit_look();
                }
            }
            Look::Around => {}
        }
    }

    /// The view the keys give at the frame on screen, while it is keyed.
    pub(crate) fn apply_keys(&mut self) {
        if self.reframe.look != Look::Keyed || self.reframe.flat_output().is_none() {
            return;
        }
        let t = self.frame_time();
        let Some(doc) = &self.reframe.doc else {
            return;
        };
        let v = doc.view_at(t);
        self.view.set_keyed(v.orientation, v.lens);
        let vals = ViewValues::of(self.view.q, &self.view.lens);
        self.reframe.applied = Some(vals);
        self.reframe.seen = Some(vals);
    }

    /// The change under way into key edits, one undo step, when every
    /// lane it changed has a key to take it; else a look-around.
    fn commit_look(&mut self) {
        let Look::Editing { at, .. } = self.reframe.look else {
            return;
        };
        let vals = ViewValues::of(self.view.q, &self.view.lens);
        let Some(edits) = self
            .reframe
            .doc
            .as_ref()
            .and_then(|d| d.view_edits(at, &vals))
        else {
            self.reframe.look = Look::Around;
            return;
        };
        self.reframe.look = if self.edit("Reframe", &edits) {
            self.reframe.applied = Some(vals);
            self.reframe.seen = Some(vals);
            Look::Keyed
        } else {
            Look::Around
        };
    }

    /// Play, a seek or a step: a change under way is kept as key edits
    /// if it can be, and the viewer goes back to the keyed view.
    pub fn end_look(&mut self) {
        if matches!(self.reframe.look, Look::Editing { .. }) {
            self.commit_look();
        }
        // The keys set the view again from the next frame; what the look
        // left is not a change of it.
        self.reframe.look = Look::Keyed;
        self.reframe.applied = None;
        self.reframe.seen = None;
    }

    /// `edits` as one undo step labelled `label`; a refusal is a notice.
    /// Whether it was made.
    pub fn edit(&mut self, label: &str, edits: &[Edit]) -> bool {
        let Some(doc) = &mut self.reframe.doc else {
            return false;
        };
        match doc.record(label, edits) {
            Ok(()) => true,
            Err(e) => {
                self.notices.push(
                    Notice::new(
                        NoticeKind::Edit,
                        Severity::Warning,
                        format!("{label}: not done"),
                    )
                    .impact(e.problem.to_string()),
                );
                false
            }
        }
    }

    /// `Alt+K`: the view on screen keyed at the frame on screen, a key
    /// on each lane (amendment 2 items 20 and 36).
    pub fn key_view(&mut self) {
        if self.reframe.flat_output().is_none() {
            return;
        }
        let t = self.frame_time();
        let vals = ViewValues::of(self.view.q, &self.view.lens);
        let Some(doc) = &self.reframe.doc else {
            return;
        };
        match doc.key_view(t, &vals) {
            Ok(edits) => {
                if self.edit("Key the view", &edits) {
                    self.reframe.look = Look::Keyed;
                    self.reframe.applied = Some(vals);
                    self.reframe.seen = Some(vals);
                }
            }
            Err(p) => self.notices.push(
                Notice::new(
                    NoticeKind::Edit,
                    Severity::Warning,
                    "Key the view: not done",
                )
                .impact(p.to_string()),
            ),
        }
    }

    /// `I`: in at the frame on screen.
    pub fn set_in(&mut self) {
        let t = self.frame_time();
        if let Some(edits) = self.reframe.doc.as_ref().map(|d| d.set_in(d.snap(t))) {
            self.edit("Set in", &edits);
        }
    }

    /// `O`: out at the end of the frame on screen, so it is the last in.
    pub fn set_out(&mut self) {
        let end = self.frame_end();
        if let Some(edits) = self.reframe.doc.as_ref().map(|d| d.set_out(end)) {
            self.edit("Set out", &edits);
        }
    }

    /// `M`: a marker at the frame on screen.
    pub fn add_marker(&mut self) {
        let t = self.frame_time();
        if let Some(edits) = self.reframe.doc.as_ref().map(|d| d.add_marker(t)) {
            self.edit("Add marker", &edits);
        }
    }

    /// `Delete`: the selected keys removed, but for a lane's last.
    pub fn remove_selected(&mut self) {
        let Some(doc) = &self.reframe.doc else {
            return;
        };
        if doc.selection.is_empty() {
            return;
        }
        let (edits, kept) = doc.remove_keys(&doc.selection);
        self.edit("Delete keys", &edits);
        if kept > 0 {
            self.notices.push(
                Notice::new(
                    NoticeKind::Edit,
                    Severity::Info,
                    "A lane keeps its last key",
                )
                .impact(format!("{kept} key(s) stayed: a lane can't be empty")),
            );
        }
    }

    /// `Alt+[` and `Alt+]`: to the key before or after the frame on screen.
    pub fn seek_key(&mut self, after: bool) {
        let t = self.frame_time();
        let to = self.reframe.doc.as_ref().and_then(|d| {
            if after {
                d.key_after(t)
            } else {
                d.key_before(t)
            }
        });
        if let Some(to) = to {
            self.seek(to, SeekMode::Exact);
        }
    }

    pub fn undo(&mut self) {
        self.end_look();
        let Some(doc) = &mut self.reframe.doc else {
            return;
        };
        if let Err(e) = doc.undo() {
            tracing::error!(error = %e, "an undo was refused");
        }
    }

    pub fn redo(&mut self) {
        self.end_look();
        let Some(doc) = &mut self.reframe.doc else {
            return;
        };
        if let Err(e) = doc.redo() {
            tracing::error!(error = %e, "a redo was refused");
        }
    }

    /// `Ctrl+L`: loop between in and out, or stop at out.
    pub fn toggle_loop(&mut self) {
        self.reframe.looping = !self.reframe.looping;
    }
}

/// The project's windows: "Save changes?", Save as, and "Replace it?".
pub fn windows(s: &mut AppState, ctx: &egui::Context) {
    confirm_window(s, ctx);
    save_as_window(s, ctx);
    replace_window(s, ctx);
}

fn confirm_window(s: &mut AppState, ctx: &egui::Context) {
    let Some(step) = s.reframe.confirm.clone() else {
        return;
    };
    let name = s.reframe.doc.as_ref().map(Doc::name).unwrap_or_default();
    let mut answer = None;
    egui::Window::new("Save changes?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!("{name} has changes that aren't saved."));
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    answer = Some(Some(true));
                }
                let lose = if step == Pending::Quit {
                    "Quit without saving"
                } else {
                    "Don't save"
                };
                if ui.button(lose).clicked() {
                    answer = Some(Some(false));
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(None);
                }
            });
        });
    match answer {
        Some(Some(save)) => s.answer_confirm(save),
        Some(None) => s.reframe.confirm = None,
        None => {}
    }
}

fn save_as_window(s: &mut AppState, ctx: &egui::Context) {
    let Some(mut save_as) = s.reframe.save_as.clone() else {
        return;
    };
    let mut done = None;
    egui::Window::new("Save as")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!("In {}", save_as.dir.display()));
            let field = ui.add(
                egui::TextEdit::singleline(&mut save_as.name)
                    .desired_width(320.0)
                    .hint_text("name.spherecut"),
            );
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                done = Some(true);
            }
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
    match done {
        Some(true) => {
            let name = save_as.name.trim();
            if name.is_empty() || name.contains('/') {
                s.reframe.save_as = Some(save_as);
                return;
            }
            let mut path = save_as.dir.join(name);
            if !is_project_file(&path) {
                path.as_mut_os_string()
                    .push(format!(".{}", file::EXTENSION));
            }
            s.reframe.save_as = None;
            s.save_to(path, false);
        }
        Some(false) => {
            s.reframe.save_as = None;
            s.reframe.after_save = None;
        }
        None => s.reframe.save_as = Some(save_as),
    }
}

fn replace_window(s: &mut AppState, ctx: &egui::Context) {
    let Some(path) = s.reframe.replace.clone() else {
        return;
    };
    let mut answer = None;
    egui::Window::new("Replace it?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(format!("{} is there already.", path.display()));
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
    match answer {
        Some(true) => {
            s.reframe.replace = None;
            s.save_to(path, true);
        }
        Some(false) => {
            s.reframe.replace = None;
            s.reframe.after_save = None;
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_fills_nine_tenths_of_the_side_it_meets() {
        // 16:9 in a 16:9 viewer: 90 % each way, centred.
        assert_eq!(
            frame_rect((1600, 900), (1920, 1080)),
            [80.0, 45.0, 1440.0, 810.0]
        );
        // 9:16 in a 16:9 viewer: the height.
        let [x, y, w, h] = frame_rect((1600, 900), (1080, 1920));
        assert_eq!((y, h), (45.0, 810.0));
        assert_eq!(w, (810.0f64 * 9.0 / 16.0).round());
        assert_eq!(x, ((1600.0 - w) / 2.0).round());
    }

    #[test]
    fn project_files_go_by_their_extension() {
        assert!(is_project_file(Path::new("/a/b.spherecut")));
        assert!(is_project_file(Path::new("B.SPHERECUT")));
        assert!(!is_project_file(Path::new("/a/b.mp4")));
        assert!(!is_project_file(Path::new("/a/spherecut")));
    }
}
