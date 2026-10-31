//! Render jobs (a2-plan 5.3 and 5.4, D58): the queue an agent's
//! `render` adds to, the one worker thread that runs them in turn, and
//! the last finished ones, for `job` to report on.
//!
//! A job holds what its render checked until it ends: the output's
//! folder and each media file, by descriptor, so that the export writes
//! and reads those and no others. Its output's name is reserved from the
//! moment it is queued, so a second render to the same file is refused
//! when it is asked for. Ids are `j<tag>-<n>`: the tag is drawn when the
//! server starts, so an id from an earlier server is known as one
//! (`job_lost`). The jobs end with the server: [`Jobs::cancel_all`] when
//! the calls are stopped, then [`Jobs::shutdown`] against the same
//! deadline.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::hash::{BuildHasher, RandomState};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use spherecut_export::{Export, ExportError, ExportJob, Progress, Report};

use crate::error::{ToolError, codes};
use crate::limits::{EXIT_GRACE, FINISHED_JOBS, QUEUED_RENDERS};
use crate::ops::check::Target;
use crate::roots::{Folder, Opened};

/// How often the worker reads a running export's progress and looks
/// for a cancel.
const POLL: Duration = Duration::from_millis(100);

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Queued,
    Running,
    /// The frames are encoded; the last pass writes the file and checks
    /// it.
    Finishing,
    Done,
    Failed,
    Cancelled,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Finishing => "finishing",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }

    pub fn ended(self) -> bool {
        matches!(self, State::Done | State::Failed | State::Cancelled)
    }
}

/// A render, checked and ready to run.
#[derive(Debug)]
pub struct Render {
    /// The export, writing through `folder` and reading through `media`.
    pub export: ExportJob,
    pub folder: Folder,
    pub media: Vec<Opened>,
    /// Paths of the export's own, each with the agent's name for it, for
    /// its messages.
    pub paths: Vec<(PathBuf, PathBuf)>,
    pub frames: u64,
    pub encoder: &'static str,
    /// What the finished file is checked for.
    pub target: Option<Target>,
}

/// A job as `job` reports it.
#[derive(Debug, Clone)]
pub struct Status {
    pub id: String,
    pub state: State,
    /// The output, as the agent names it.
    pub output: PathBuf,
    pub frames: u64,
    /// The encoder it starts with.
    pub encoder: &'static str,
    /// The export's own, once it runs.
    pub progress: Option<Progress>,
    /// Jobs to run before it, the running one included.
    pub ahead: usize,
    /// Its file's paths are the agent's.
    pub report: Option<Report>,
    pub error: Option<ToolError>,
    pub target: Option<Target>,
    /// `check`'s result for the file, once a `job` call has made it.
    pub check: Option<Value>,
}

#[derive(Debug)]
struct Job {
    state: State,
    /// Taken by the worker when the job starts, or dropped when it is
    /// cancelled before.
    render: Option<Render>,
    output: PathBuf,
    /// The output's folder and name, reserved until the job ends.
    key: (u64, u64, OsString),
    frames: u64,
    encoder: &'static str,
    progress: Option<Progress>,
    /// Asked to stop while it runs.
    cancel: bool,
    report: Option<Report>,
    error: Option<ToolError>,
    target: Option<Target>,
    check: Option<Value>,
}

#[derive(Debug, Default)]
struct Table {
    /// The last job's number.
    last: u64,
    queue: VecDeque<u64>,
    jobs: BTreeMap<u64, Job>,
    /// Ended jobs, oldest first, at most [`FINISHED_JOBS`].
    ended: VecDeque<u64>,
    /// The server is ending: nothing more runs.
    closing: bool,
    /// A `job` call is waiting.
    waiting: bool,
    worker: Option<JoinHandle<()>>,
}

impl Table {
    /// Job `n` has ended: keep it among the last few.
    fn ended(&mut self, n: u64) {
        self.ended.push_back(n);
        while self.ended.len() > FINISHED_JOBS {
            if let Some(old) = self.ended.pop_front() {
                self.jobs.remove(&old);
            }
        }
    }

    fn ahead(&self, n: u64) -> usize {
        let running = self
            .jobs
            .iter()
            .filter(|(m, j)| **m != n && matches!(j.state, State::Running | State::Finishing))
            .count();
        running + self.queue.iter().take_while(|m| **m != n).count()
    }
}

/// The one waiting call's mark, taken off when it is dropped.
struct Waiting<'a>(&'a Shared);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.lock().waiting = false;
    }
}

#[derive(Debug, Default)]
struct Shared {
    table: Mutex<Table>,
    /// Signalled whenever a job changes.
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, t: MutexGuard<'a, Table>, at_most: Duration) -> MutexGuard<'a, Table> {
        self.changed
            .wait_timeout(t, at_most)
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }
}

/// The server's render jobs.
#[derive(Debug)]
pub struct Jobs {
    /// This server's part of every id: 8 hex digits.
    tag: String,
    shared: Arc<Shared>,
}

impl Default for Jobs {
    fn default() -> Self {
        Self::new()
    }
}

impl Jobs {
    pub fn new() -> Jobs {
        let seed = (std::process::id(), SystemTime::now());
        let tag = RandomState::new().hash_one(seed) as u32;
        Jobs {
            tag: format!("{tag:08x}"),
            shared: Arc::default(),
        }
    }

    fn id(&self, n: u64) -> String {
        format!("j{}-{n}", self.tag)
    }

    /// Queue `render`, to `output` as the agent names it: its id and the
    /// jobs ahead of it. Refused `output_exists` when a job not yet ended
    /// writes the same file, `busy` when [`QUEUED_RENDERS`] wait already.
    pub fn submit(&self, output: PathBuf, render: Render) -> Result<(String, usize), ToolError> {
        let key = render.folder.key().map_err(|e| {
            ToolError::new(
                codes::IO_ERROR,
                format!("the output's folder can't be read: {e}"),
            )
            .at("output")
        })?;
        let mut t = self.shared.lock();
        if t.closing {
            return Err(ToolError::new(codes::CANCELLED, "the server is ending"));
        }
        if t.jobs.values().any(|j| !j.state.ended() && j.key == key) {
            return Err(ToolError::new(
                codes::OUTPUT_EXISTS,
                "a render not yet finished writes that file",
            )
            .at("output")
            .next("choose another name, or cancel that render with job"));
        }
        if t.queue.len() >= QUEUED_RENDERS {
            return Err(ToolError::new(
                codes::BUSY,
                format!("{QUEUED_RENDERS} renders are waiting already"),
            )
            .next("wait for one with job, or cancel one"));
        }
        if t.worker.is_none() {
            let shared = self.shared.clone();
            let worker = std::thread::Builder::new()
                .name("mcp-render".into())
                .spawn(move || work(&shared))
                .map_err(|e| ToolError::internal(format!("can't start the render thread: {e}")))?;
            t.worker = Some(worker);
        }
        t.last += 1;
        let n = t.last;
        t.jobs.insert(
            n,
            Job {
                state: State::Queued,
                output,
                key,
                frames: render.frames,
                encoder: render.encoder,
                target: render.target,
                render: Some(render),
                progress: None,
                cancel: false,
                report: None,
                error: None,
                check: None,
            },
        );
        t.queue.push_back(n);
        let ahead = t.ahead(n);
        drop(t);
        self.shared.changed.notify_all();
        Ok((self.id(n), ahead))
    }

    /// The job of `id`, the argument `arg`: `job_lost` for another
    /// server's, `not_found` for one this server doesn't have or no
    /// longer keeps.
    pub fn find(&self, arg: &str, id: &str) -> Result<u64, ToolError> {
        let parsed = id
            .strip_prefix('j')
            .and_then(|rest| rest.split_once('-'))
            .filter(|(tag, _)| tag.len() == 8 && tag.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|(tag, n)| Some((tag, n.parse::<u64>().ok()?)));
        let Some((tag, n)) = parsed else {
            return Err(ToolError::new(codes::BAD_ARGUMENT, "isn't a job's id")
                .at(arg)
                .valid("the id render gave, such as j1f2e3d4c-1"));
        };
        if tag != self.tag {
            return Err(ToolError::new(
                codes::JOB_LOST,
                "is a job of a server that has ended, and its render went with it",
            )
            .at(arg)
            .next("render again"));
        }
        if self.shared.lock().jobs.contains_key(&n) {
            return Ok(n);
        }
        let why = if n <= self.shared.lock().last {
            format!("is an older job than the last {FINISHED_JOBS} this server keeps")
        } else {
            "isn't a job of this server's".into()
        };
        Err(ToolError::new(codes::NOT_FOUND, why).at(arg))
    }

    /// Job `n` now.
    pub fn status(&self, n: u64) -> Option<Status> {
        let t = self.shared.lock();
        self.status_in(&t, n)
    }

    fn status_in(&self, t: &Table, n: u64) -> Option<Status> {
        let j = t.jobs.get(&n)?;
        Some(Status {
            id: self.id(n),
            state: j.state,
            output: j.output.clone(),
            frames: j.frames,
            encoder: j.encoder,
            progress: j.progress.clone(),
            ahead: if j.state == State::Queued {
                t.ahead(n)
            } else {
                0
            },
            report: j.report.clone(),
            error: j.error.clone(),
            target: j.target,
            check: j.check.clone(),
        })
    }

    /// Keep `check`'s result for job `n`'s file.
    pub fn checked(&self, n: u64, check: Value) {
        if let Some(j) = self.shared.lock().jobs.get_mut(&n) {
            j.check = Some(check);
        }
    }

    /// Wait for job `n` to end, until `until` or until `stop` says to,
    /// calling `progress` with each change: the job then, and whether
    /// this call waited. One call waits at a time; another gets the job
    /// at once, so that the server always has a worker free of waits.
    pub fn wait(
        &self,
        n: u64,
        until: Instant,
        stop: impl Fn() -> bool,
        mut progress: impl FnMut(&Status),
    ) -> Option<(Status, bool)> {
        {
            let mut t = self.shared.lock();
            if t.waiting {
                return self.status_in(&t, n).map(|s| (s, false));
            }
            t.waiting = true;
        }
        // Cleared however the wait ends, a panic in `progress` too (the
        // call's panic is caught and answered): dropped after `t`.
        let _waiting = Waiting(&self.shared);
        let mut t = self.shared.lock();
        let mut seen = None;
        let status = loop {
            let Some(s) = self.status_in(&t, n) else {
                break None;
            };
            let now = Instant::now();
            if s.state.ended() || now >= until || stop() {
                break Some(s);
            }
            let done = s.progress.as_ref().map(|p| p.frames_done);
            if done != seen {
                seen = done;
                drop(t);
                progress(&s);
                t = self.shared.lock();
                continue;
            }
            t = self
                .shared
                .wait(t, until.saturating_duration_since(now).min(POLL));
        };
        drop(t);
        status.map(|s| (s, true))
    }

    /// Cancel job `n`: a queued one at once, a running one within
    /// `until`, by when its export has removed its file. One that has
    /// ended stays as it ended.
    pub fn cancel(&self, n: u64, until: Instant) -> Option<Status> {
        let mut t = self.shared.lock();
        let j = t.jobs.get_mut(&n)?;
        match j.state {
            State::Queued => {
                j.state = State::Cancelled;
                let render = j.render.take();
                t.queue.retain(|m| *m != n);
                t.ended(n);
                let s = self.status_in(&t, n);
                drop(t);
                drop(render);
                self.shared.changed.notify_all();
                return s;
            }
            State::Running | State::Finishing => j.cancel = true,
            State::Done | State::Failed | State::Cancelled => {}
        }
        self.shared.changed.notify_all();
        loop {
            let now = Instant::now();
            match t.jobs.get(&n) {
                Some(j) if !j.state.ended() && now < until => {}
                _ => break,
            }
            t = self
                .shared
                .wait(t, until.saturating_duration_since(now).min(POLL));
        }
        self.status_in(&t, n)
    }

    /// Stop every job, as the server ends: the queue is emptied and the
    /// running export told to stop. Nothing more starts.
    pub fn cancel_all(&self) {
        let mut t = self.shared.lock();
        t.closing = true;
        let queued: Vec<u64> = t.queue.drain(..).collect();
        let mut renders = Vec::new();
        for n in queued {
            if let Some(j) = t.jobs.get_mut(&n) {
                j.state = State::Cancelled;
                renders.extend(j.render.take());
            }
        }
        for j in t.jobs.values_mut() {
            if matches!(j.state, State::Running | State::Finishing) {
                j.cancel = true;
            }
        }
        drop(t);
        drop(renders);
        self.shared.changed.notify_all();
    }

    /// [`cancel_all`](Self::cancel_all), then wait until `until` for the
    /// worker to end, so that a cancelled export has removed its file:
    /// how many threads were still running then (0 or 1).
    pub fn shutdown(&self, until: Instant) -> usize {
        self.cancel_all();
        let mut t = self.shared.lock();
        let Some(worker) = t.worker.take() else {
            return 0;
        };
        while !worker.is_finished() && Instant::now() < until {
            let left = until.saturating_duration_since(Instant::now());
            t = self.shared.wait(t, left.min(Duration::from_millis(10)));
        }
        drop(t);
        if worker.is_finished() {
            worker.join().ok();
            0
        } else {
            tracing::warn!("mcp: a render didn't stop in time; leaving it");
            1
        }
    }
}

impl Drop for Jobs {
    /// A server's jobs end with it, whoever drops them.
    fn drop(&mut self) {
        self.shutdown(Instant::now() + EXIT_GRACE);
    }
}

/// The worker: each job in turn, until the server ends.
fn work(shared: &Shared) {
    loop {
        let (n, render) = {
            let mut t = shared.lock();
            loop {
                if t.closing {
                    drop(t);
                    shared.changed.notify_all();
                    return;
                }
                if let Some(n) = t.queue.pop_front() {
                    let render = t.jobs.get_mut(&n).and_then(|j| {
                        j.state = State::Running;
                        j.render.take()
                    });
                    break (n, render);
                }
                t = shared.wait(t, Duration::from_secs(60));
            }
        };
        shared.changed.notify_all();
        let Some(render) = render else {
            continue;
        };
        let outcome = run(shared, n, render);
        let mut t = shared.lock();
        if let Some(j) = t.jobs.get_mut(&n) {
            match outcome {
                Ok(report) => {
                    j.state = State::Done;
                    j.report = Some(report);
                }
                Err(None) => j.state = State::Cancelled,
                Err(Some(e)) => {
                    j.state = State::Failed;
                    j.error = Some(e);
                }
            }
            t.ended(n);
        }
        drop(t);
        shared.changed.notify_all();
    }
}

/// Run one render: its report, or its error (None when it was
/// cancelled). The folder and media it checked are held until the
/// export has ended.
fn run(shared: &Shared, n: u64, render: Render) -> Result<Report, Option<ToolError>> {
    let Render {
        export,
        folder,
        media,
        paths,
        ..
    } = render;
    let mut paths = paths;
    // The longest first, so that no path is taken for the start of another.
    paths.sort_by_key(|(ours, _)| std::cmp::Reverse(ours.as_os_str().len()));
    let failed = |e: ExportError| match e {
        ExportError::Cancelled => None,
        e => Some(export_error(&e, &paths)),
    };
    let mut x = Export::start(export).map_err(failed)?;
    let ended = loop {
        if let Some(r) = x.wait(POLL) {
            break r;
        }
        let p = x.progress();
        let mut t = shared.lock();
        let closing = t.closing;
        let cancel = match t.jobs.get_mut(&n) {
            Some(j) => {
                j.state = if p.finishing.is_some() {
                    State::Finishing
                } else {
                    State::Running
                };
                j.progress = Some(p);
                j.cancel || closing
            }
            None => true,
        };
        drop(t);
        shared.changed.notify_all();
        if cancel {
            x.cancel();
        }
    };
    let p = x.progress();
    if let Some(j) = shared.lock().jobs.get_mut(&n) {
        j.progress = Some(p);
    }
    drop((x, folder, media));
    let mut report = ended.map_err(failed)?;
    report.output = theirs(&report.output.to_string_lossy(), &paths).into();
    for w in &mut report.warnings {
        w.message = theirs(&w.message, &paths);
    }
    Ok(report)
}

/// `text` with the export's paths turned back into the agent's.
fn theirs(text: &str, paths: &[(PathBuf, PathBuf)]) -> String {
    let mut text = text.to_owned();
    for (ours, theirs) in paths {
        text = text.replace(
            ours.to_string_lossy().as_ref(),
            theirs.to_string_lossy().as_ref(),
        );
    }
    text
}

/// An export's failure as the agent gets it: its code (D51), and its
/// message with the export's paths turned back into the agent's.
fn export_error(e: &ExportError, paths: &[(PathBuf, PathBuf)]) -> ToolError {
    let message = match e {
        ExportError::OutputExists(_) => {
            "something turned up at the output before the render finished, and nothing is \
             replaced"
                .to_owned()
        }
        e => theirs(&e.to_string(), paths),
    };
    let e2 = ToolError::new(e.code(), message);
    match e.code() {
        codes::OUTPUT_EXISTS | codes::NO_SPACE | codes::IO_ERROR => e2.at("output"),
        _ => e2,
    }
}
