//! The MCP layer (D43, a1-plan section 3): JSON-RPC 2.0, one message a
//! line, on whatever the server is given to read and write (stdin and a
//! copy of stdout for `spherecut mcp`, pipes in tests).
//!
//! One reader thread parses each line and answers `initialize`, `ping`,
//! `tools/list` and the notifications at once. Tool calls wait in a queue
//! (at most [`MAX_QUEUED`]) for one of [`CALLS_AT_ONCE`] workers, each
//! call with its own stop source and a deadline [`CALL_TIME`] after its
//! request arrived. A reaper stops calls past their deadline; one still
//! going [`STUCK_GRACE`] later is answered `timed_out` by the reaper, and
//! its worker is given up on and replaced, up to [`MAX_ABANDONED`]
//! (a1-plan amendment 20). A cancelled call gets no response. When the
//! input ends, every call is stopped, and [`serve`] returns within
//! [`EXIT_GRACE`] whatever the workers are doing, saying how many are
//! still inside a call.
//!
//! The revisions in [`SUPPORTED`] are served through `initialize`. A
//! request in 2026-07-28's form (its version in `_meta`, or
//! `server/discover`) gets -32022 with that list, which is how a client
//! that asks for 2026-07-28 first learns to fall back (D43). The two
//! oldest allowed JSON-RPC batches; this server refuses them, as no
//! client is known to send one.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use spherecut_core::{StopSource, StopToken};

use crate::error::{ToolError, codes};
use crate::limits::{
    CALL_TIME, CALLS_AT_ONCE, EXIT_GRACE, MAX_ABANDONED, MAX_QUEUED, MAX_REQUEST_BYTES,
    PROGRESS_EVERY, STUCK_GRACE,
};
use crate::text;

/// The revisions served through `initialize`, newest first.
pub const SUPPORTED: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Where a 2026-07-28 request carries its revision, in `params._meta`.
const MODERN_META: &str = "io.modelcontextprotocol/protocolVersion";

/// JSON-RPC's error codes, and MCP's for an unsupported revision.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
}

/// A request id: a string or an integer, as MCP has it, kept as its JSON
/// text so that `1` and `"1"` stay apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Id(String);

impl Id {
    fn from_value(v: &Value) -> Option<Id> {
        match v {
            Value::String(_) => Some(Id(v.to_string())),
            Value::Number(n) if n.is_i64() || n.is_u64() => Some(Id(v.to_string())),
            _ => None,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::from_str(&self.0).unwrap_or(Value::Null)
    }
}

/// One line from the client, sorted.
#[derive(Debug)]
pub enum Message {
    Request {
        id: Id,
        method: String,
        params: Map<String, Value>,
    },
    Notification {
        method: String,
        params: Map<String, Value>,
    },
    /// A response: the server sends no requests, so it is dropped.
    Response,
    /// Not something to act on: answered with this error, with the id
    /// when one could be read.
    Invalid {
        id: Option<Id>,
        code: i64,
        message: String,
    },
}

fn invalid(id: Option<Id>, code: i64, message: impl Into<String>) -> Message {
    Message::Invalid {
        id,
        code,
        message: message.into(),
    }
}

/// Sort one line (without its newline). None for a blank line.
pub fn parse(line: &[u8]) -> Option<Message> {
    let line = line.trim_ascii();
    if line.is_empty() {
        return None;
    }
    let value: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(invalid(None, code::PARSE_ERROR, format!("not JSON: {e}")));
        }
    };
    let obj = match value {
        Value::Object(obj) => obj,
        Value::Array(_) => {
            return Some(invalid(
                None,
                code::INVALID_REQUEST,
                "batches aren't supported: send one message a line",
            ));
        }
        _ => {
            return Some(invalid(
                None,
                code::INVALID_REQUEST,
                "a message is a JSON object",
            ));
        }
    };
    let id = match obj.get("id") {
        None => None,
        Some(v) => match Id::from_value(v) {
            Some(id) => Some(id),
            None if obj.contains_key("method") => {
                return Some(invalid(
                    None,
                    code::INVALID_REQUEST,
                    "an id is a string or an integer",
                ));
            }
            None => None,
        },
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(invalid(
            id,
            code::INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
        ));
    }
    let method = match obj.get("method") {
        None if obj.contains_key("result") || obj.contains_key("error") => {
            return Some(Message::Response);
        }
        None => return Some(invalid(id, code::INVALID_REQUEST, "no method")),
        Some(Value::String(m)) => m.clone(),
        Some(_) => {
            return Some(invalid(
                id,
                code::INVALID_REQUEST,
                "method must be a string",
            ));
        }
    };
    let params = match obj.get("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(p)) => p.clone(),
        Some(_) => {
            return Some(invalid(
                id,
                code::INVALID_REQUEST,
                "params must be an object",
            ));
        }
    };
    Some(match id {
        Some(id) => Message::Request { id, method, params },
        None => Message::Notification { method, params },
    })
}

/// An image in a tool's result.
#[derive(Debug, Clone)]
pub struct Image {
    /// `image/jpeg` or `image/png`.
    pub mime: &'static str,
    pub data: Vec<u8>,
}

/// What a tool returns: its Response as JSON, and its images.
#[derive(Debug, Clone)]
pub struct Output {
    pub result: Value,
    pub images: Vec<Image>,
}

impl Output {
    pub fn json(result: Value) -> Self {
        Self {
            result,
            images: Vec::new(),
        }
    }
}

/// The tools a server offers.
pub trait Tools: Send + Sync + 'static {
    /// The `tools` array of `tools/list`, in its fixed order.
    fn list(&self) -> Value;
    /// Whether there is a tool of this name.
    fn has(&self, name: &str) -> bool;
    /// Run tool `name` on `args`, on a worker thread.
    fn call(&self, name: &str, args: Value, cx: &CallCx) -> Result<Output, ToolError>;
}

/// The server's name, version and instructions, for `initialize`.
#[derive(Debug, Clone)]
pub struct Info {
    pub name: &'static str,
    pub title: &'static str,
    pub version: &'static str,
    pub instructions: String,
}

/// How long calls may take. [`Timing::default`] is the limits' values;
/// tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// A call's time, from its request's arrival ([`CALL_TIME`]).
    pub call_time: Duration,
    /// How long a call past its deadline has to stop ([`STUCK_GRACE`]).
    pub stuck_grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            call_time: CALL_TIME,
            stuck_grace: STUCK_GRACE,
        }
    }
}

/// One message a line: the protocol's output, shared by the reader, the
/// workers and their progress.
struct Writer {
    out: Mutex<Box<dyn Write + Send>>,
}

impl Writer {
    fn send(&self, v: &Value) {
        // serde_json escapes newlines in strings, so a message is a line.
        let mut line = v.to_string();
        line.push('\n');
        let mut out = lock(&self.out);
        if let Err(e) = out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
            tracing::warn!("mcp: can't write to the client: {e}");
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding one of these locks is the server's bug, and
    // the state behind them stays consistent, so carry on.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a running tool can ask of its call: whether to stop, how long it
/// has, and a way to report progress.
#[derive(Debug)]
pub struct CallCx {
    stop: StopSource,
    deadline: Instant,
    /// The call's time, for the message when it runs out.
    limit: Duration,
    timed_out: Arc<AtomicBool>,
    progress: Option<Progress>,
}

/// Cancels a call ([`CallCx::canceller`]).
#[derive(Debug, Clone)]
pub struct Canceller(StopSource);

impl Canceller {
    pub fn cancel(&self) {
        self.0.shutdown();
    }
}

struct Progress {
    token: Value,
    out: Arc<Writer>,
    last: Mutex<Option<(Instant, f64)>>,
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Progress")
            .field("token", &self.token)
            .finish()
    }
}

impl CallCx {
    /// A call with no client to report progress to, as `spherecut call`
    /// runs one: it stops at `deadline`, through a timer thread.
    pub fn standalone(deadline: Instant) -> Self {
        let cx = Self {
            stop: StopSource::new(),
            deadline,
            limit: deadline.saturating_duration_since(Instant::now()),
            timed_out: Arc::default(),
            progress: None,
        };
        let (stop, timed_out) = (cx.stop.clone(), cx.timed_out.clone());
        let _ = std::thread::Builder::new()
            .name("call-deadline".into())
            .spawn(move || {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                timed_out.store(true, Ordering::Release);
                stop.shutdown();
            });
        cx
    }

    /// A way to cancel the call from another thread, as a client's
    /// `notifications/cancelled` does: `spherecut call`'s on SIGINT.
    pub fn canceller(&self) -> Canceller {
        Canceller(self.stop.clone())
    }

    /// Stop the call as its deadline would, for tests of an op whose time
    /// runs out midway.
    #[cfg(test)]
    pub(crate) fn expire(&self) {
        self.timed_out.store(true, Ordering::Release);
        self.stop.shutdown();
    }

    /// A token that stops when the call does.
    pub fn token(&self) -> StopToken {
        self.stop.current()
    }

    /// The flag for a decoder's interrupt callback, so that FFmpeg stops
    /// inside a read when the call does.
    pub fn interrupt(&self) -> Arc<AtomicBool> {
        self.stop.shutdown_flag()
    }

    /// How long the call has left.
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Err when the call should stop: `timed_out` past its deadline,
    /// `cancelled` when the client cancelled it or went away.
    pub fn check(&self) -> Result<(), ToolError> {
        if self.timed_out.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(timed_out(self.limit));
        }
        if self.stop.is_shut_down() {
            return Err(ToolError::new(codes::CANCELLED, "the call was cancelled"));
        }
        Ok(())
    }

    /// Report progress: `done` of `total`, which must grow from call to
    /// call. Sent only when the request asked for progress, and at most
    /// every [`PROGRESS_EVERY`].
    pub fn progress(&self, done: f64, total: Option<f64>, message: Option<&str>) {
        let Some(p) = &self.progress else {
            return;
        };
        let now = Instant::now();
        {
            let mut last = lock(&p.last);
            if let Some((at, prev)) = *last
                && (done <= prev || now.duration_since(at) < PROGRESS_EVERY)
            {
                return;
            }
            *last = Some((now, done));
        }
        let mut params = json!({"progressToken": p.token, "progress": done});
        if let Some(t) = total {
            params["total"] = json!(t);
        }
        if let Some(m) = message {
            params["message"] = json!(m);
        }
        p.out
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));
    }
}

fn timed_out(limit: Duration) -> ToolError {
    // Whole milliseconds, shown as seconds: "50", "0.3".
    let seconds = limit.as_millis() as f64 / 1000.0;
    ToolError::new(
        codes::TIMED_OUT,
        format!("the call ran past its {seconds} s limit"),
    )
    .next("ask for less in one call: fewer files or times, or a shorter range")
}

/// A call, queued or running.
struct Entry {
    stop: StopSource,
    deadline: Instant,
    timed_out: Arc<AtomicBool>,
    /// Cancelled by the client, or by the input ending: no response.
    cancelled: bool,
    /// The retire flag of the worker running it; None while queued.
    worker: Option<Arc<AtomicBool>>,
}

struct Job {
    id: Id,
    name: String,
    args: Value,
    progress_token: Option<Value>,
    stop: StopSource,
    deadline: Instant,
    timed_out: Arc<AtomicBool>,
}

/// Why the session is ending.
enum Event {
    InputEnded,
    GaveUp,
}

struct Shared {
    out: Arc<Writer>,
    tools: Arc<dyn Tools>,
    calls: Mutex<HashMap<Id, Entry>>,
    queue: Mutex<VecDeque<Job>>,
    ready: Condvar,
    closing: AtomicBool,
    /// Wakes the reaper early when the session ends.
    reaper_sleep: (Mutex<()>, Condvar),
    timing: Timing,
    workers: Mutex<Vec<JoinHandle<()>>>,
    /// Workers given up on so far.
    abandoned: AtomicUsize,
    /// How workers are named.
    started: AtomicUsize,
    events: Sender<Event>,
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    /// Workers still inside a call when [`serve`] returned. The process
    /// should then leave without running exit handlers (`_exit`), which
    /// could wait on a driver one of them is inside.
    pub stuck: usize,
    /// The server gave up after [`MAX_ABANDONED`] stuck workers: exit 1,
    /// so that the client starts a fresh server.
    pub gave_up: bool,
}

/// Serve one session: read messages from `input` until it ends, answer on
/// `output`. Returns within [`EXIT_GRACE`] of the input's end, or of the
/// server giving up.
pub fn serve(
    input: impl BufRead + Send + 'static,
    output: impl Write + Send + 'static,
    tools: Arc<dyn Tools>,
    info: Info,
    timing: Timing,
) -> Ended {
    let (events, ending) = mpsc::channel();
    let shared = Arc::new(Shared {
        out: Arc::new(Writer {
            out: Mutex::new(Box::new(output)),
        }),
        tools,
        calls: Mutex::default(),
        queue: Mutex::default(),
        ready: Condvar::new(),
        closing: AtomicBool::new(false),
        reaper_sleep: (Mutex::new(()), Condvar::new()),
        timing,
        workers: Mutex::default(),
        abandoned: AtomicUsize::new(0),
        started: AtomicUsize::new(0),
        events,
    });
    for _ in 0..CALLS_AT_ONCE {
        start_worker(&shared);
    }
    let reaper = spawn("mcp-deadlines", {
        let s = shared.clone();
        move || reaper(&s)
    });
    let reader = spawn("mcp-read", {
        let s = shared.clone();
        move || {
            read(input, &s, &info);
            let _ = s.events.send(Event::InputEnded);
        }
    });
    let gave_up = match ending.recv() {
        Ok(Event::GaveUp) => true,
        Ok(Event::InputEnded) | Err(_) => false,
    };

    // Stop everything, answer nothing more.
    tracing::debug!("mcp: the session is ending; stopping every call");
    shared.closing.store(true, Ordering::Release);
    for e in lock(&shared.calls).values_mut() {
        e.cancelled = true;
        e.stop.shutdown();
    }
    lock(&shared.queue).clear();
    shared.ready.notify_all();
    {
        let _sleep = lock(&shared.reaper_sleep.0);
        shared.reaper_sleep.1.notify_all();
    }
    let until = Instant::now() + EXIT_GRACE;
    let finished = |t: &JoinHandle<()>| {
        while !t.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        t.is_finished()
    };
    // The reaper first, so that it starts no worker after these are taken.
    if let Some(t) = reaper
        && finished(&t)
    {
        let _ = t.join();
    }
    let mut stuck = 0;
    for w in std::mem::take(&mut *lock(&shared.workers)) {
        if finished(&w) {
            let _ = w.join();
        } else {
            stuck += 1;
        }
    }
    if stuck > 0 {
        tracing::warn!("mcp: {stuck} call(s) didn't stop in time; leaving them");
    }
    // After giving up, the reader may still be waiting for input.
    if let Some(t) = reader
        && !gave_up
    {
        let _ = t.join();
    }
    tracing::debug!("mcp: done");
    Ended { stuck, gave_up }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| tracing::error!("mcp: can't start {name}: {e}"))
        .ok()
}

fn start_worker(shared: &Arc<Shared>) {
    let n = shared.started.fetch_add(1, Ordering::Relaxed);
    let retired = Arc::new(AtomicBool::new(false));
    let thread = spawn(&format!("mcp-call-{n}"), {
        let (s, r) = (shared.clone(), retired.clone());
        move || worker(&s, &r)
    });
    if let Some(thread) = thread {
        lock(&shared.workers).push(thread);
    }
}

enum Line {
    Ok,
    TooLong,
    End,
}

/// Read one line into `buf`, newline dropped, at most
/// [`MAX_REQUEST_BYTES`]: a longer one is read to its end and dropped.
fn read_line(input: &mut impl BufRead, buf: &mut Vec<u8>) -> Line {
    buf.clear();
    let limit = MAX_REQUEST_BYTES as u64 + 1;
    match input.by_ref().take(limit).read_until(b'\n', buf) {
        Ok(0) => return Line::End,
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("mcp: can't read from the client: {e}");
            return Line::End;
        }
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Line::Ok;
    }
    if buf.len() <= MAX_REQUEST_BYTES {
        // The last line, with no newline after it.
        return Line::Ok;
    }
    loop {
        buf.clear();
        match input.by_ref().take(64 << 10).read_until(b'\n', buf) {
            Ok(0) | Err(_) => return Line::End,
            Ok(_) if buf.last() == Some(&b'\n') => {
                buf.clear();
                return Line::TooLong;
            }
            Ok(_) => {}
        }
    }
}

fn result_message(id: &Id, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id.to_value(), "result": result})
}

fn error_message(id: Option<&Id>, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let Some(d) = data {
        error["data"] = d;
    }
    json!({"jsonrpc": "2.0", "id": id.map_or(Value::Null, Id::to_value), "error": error})
}

fn read(mut input: impl BufRead, shared: &Shared, info: &Info) {
    let mut buf = Vec::new();
    // The revision `initialize` agreed on, once it has.
    let mut session: Option<&'static str> = None;
    loop {
        match read_line(&mut input, &mut buf) {
            Line::End => return,
            Line::TooLong => {
                shared.out.send(&error_message(
                    None,
                    code::INVALID_REQUEST,
                    &format!("a message is at most {MAX_REQUEST_BYTES} bytes"),
                    None,
                ));
                continue;
            }
            Line::Ok => {}
        }
        let Some(message) = parse(&buf) else {
            continue;
        };
        match message {
            Message::Response => {}
            Message::Invalid { id, code, message } => {
                shared
                    .out
                    .send(&error_message(id.as_ref(), code, &message, None));
            }
            Message::Notification { method, params } => {
                if method == "notifications/cancelled"
                    && let Some(id) = params.get("requestId").and_then(Id::from_value)
                {
                    cancel(shared, &id);
                }
                // notifications/initialized, and any other, change nothing.
            }
            Message::Request { id, method, params } => {
                let reply = request(shared, info, &mut session, &id, &method, params);
                if let Some(reply) = reply {
                    shared.out.send(&reply);
                }
            }
        }
    }
}

/// Answer one request, or queue it (None).
fn request(
    shared: &Shared,
    info: &Info,
    session: &mut Option<&'static str>,
    id: &Id,
    method: &str,
    params: Map<String, Value>,
) -> Option<Value> {
    let modern = params
        .get("_meta")
        .and_then(|m| m.get(MODERN_META))
        .cloned();
    if method == "server/discover" || (modern.is_some() && method != "initialize") {
        return Some(error_message(
            Some(id),
            code::UNSUPPORTED_PROTOCOL_VERSION,
            "Unsupported protocol version: this server speaks the initialize handshake",
            // 2026-07-28's schema.ts: `requested` is a string.
            Some(json!({
                "requested": modern.as_ref().and_then(Value::as_str).unwrap_or(""),
                "supported": SUPPORTED,
            })),
        ));
    }
    match method {
        "initialize" => {
            if session.is_some() {
                return Some(error_message(
                    Some(id),
                    code::INVALID_REQUEST,
                    "the session is already initialized",
                    None,
                ));
            }
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = SUPPORTED
                .iter()
                .find(|v| Some(**v) == asked)
                .copied()
                .unwrap_or(SUPPORTED[0]);
            let client = params.get("clientInfo");
            let part = |k: &str| {
                text::field(
                    client
                        .and_then(|c| c.get(k))
                        .and_then(Value::as_str)
                        .unwrap_or("?"),
                )
            };
            tracing::info!(
                "mcp: {} {} asked for {}; serving {version}",
                part("name"),
                part("version"),
                text::field(asked.unwrap_or("no revision")),
            );
            *session = Some(version);
            Some(result_message(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": info.name, "title": info.title, "version": info.version},
                    "instructions": info.instructions,
                }),
            ))
        }
        "ping" => Some(result_message(id, json!({}))),
        _ if session.is_none() => Some(error_message(
            Some(id),
            code::INVALID_REQUEST,
            "send initialize first",
            None,
        )),
        "tools/list" => Some(result_message(id, json!({"tools": shared.tools.list()}))),
        "tools/call" => call(shared, id, params),
        _ => Some(error_message(
            Some(id),
            code::METHOD_NOT_FOUND,
            &format!(
                "no method {}",
                text::field(&method.chars().take(64).collect::<String>())
            ),
            None,
        )),
    }
}

/// Queue a `tools/call`, or answer why not.
fn call(shared: &Shared, id: &Id, mut params: Map<String, Value>) -> Option<Value> {
    let bad = |message: &str| Some(error_message(Some(id), code::INVALID_PARAMS, message, None));
    let Some(Value::String(name)) = params.remove("name") else {
        return bad("tools/call needs the tool's name");
    };
    if !shared.tools.has(&name) {
        let shown: String = name.chars().take(64).collect();
        return bad(&format!("no tool {}", text::field(&shown)));
    }
    let args = match params.remove("arguments") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(a @ Value::Object(_)) => a,
        Some(_) => return bad("arguments must be an object"),
    };
    let progress_token = params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .filter(|t| t.is_string() || t.is_i64() || t.is_u64())
        .cloned();
    if lock(&shared.queue).len() >= MAX_QUEUED {
        let busy = ToolError::new(
            codes::BUSY,
            format!("{MAX_QUEUED} calls are already waiting"),
        )
        .next("wait for a call in flight to finish, then send this one again");
        return Some(result_message(id, tool_result(Err(busy))));
    }
    let mut calls = lock(&shared.calls);
    if calls.contains_key(id) {
        return Some(error_message(
            Some(id),
            code::INVALID_REQUEST,
            "a call with this id is still running",
            None,
        ));
    }
    let stop = StopSource::new();
    let timed_out = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + shared.timing.call_time;
    calls.insert(
        id.clone(),
        Entry {
            stop: stop.clone(),
            deadline,
            timed_out: timed_out.clone(),
            cancelled: false,
            worker: None,
        },
    );
    drop(calls);
    lock(&shared.queue).push_back(Job {
        id: id.clone(),
        name,
        args,
        progress_token,
        stop,
        deadline,
        timed_out,
    });
    shared.ready.notify_one();
    None
}

/// The client cancelled call `id`: a queued one never runs, a running one
/// is stopped, and neither gets a response. An unknown id is a call that
/// has just finished, or never was.
fn cancel(shared: &Shared, id: &Id) {
    let mut calls = lock(&shared.calls);
    let Some(e) = calls.get_mut(id) else {
        return;
    };
    e.cancelled = true;
    e.stop.shutdown();
    let mut queue = lock(&shared.queue);
    let before = queue.len();
    queue.retain(|j| &j.id != id);
    if queue.len() != before {
        calls.remove(id);
    }
}

fn worker(shared: &Shared, retired: &Arc<AtomicBool>) {
    loop {
        if retired.load(Ordering::Acquire) {
            return;
        }
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if shared.closing.load(Ordering::Acquire) {
                    return;
                }
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        // Claim the call, unless it was cancelled or answered meanwhile.
        match lock(&shared.calls).get_mut(&job.id) {
            Some(e) => e.worker = Some(retired.clone()),
            None => continue,
        }
        let cx = CallCx {
            stop: job.stop,
            deadline: job.deadline,
            limit: shared.timing.call_time,
            timed_out: job.timed_out,
            progress: job.progress_token.map(|token| Progress {
                token,
                out: shared.out.clone(),
                last: Mutex::new(None),
            }),
        };
        let outcome = match cx.check() {
            Err(e) => Err(e),
            Ok(()) => {
                let tools = shared.tools.clone();
                let (name, args) = (job.name.clone(), job.args);
                std::panic::catch_unwind(AssertUnwindSafe(|| tools.call(&name, args, &cx)))
                    .unwrap_or_else(|panic| {
                        let why = panic
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| panic.downcast_ref::<&str>().copied())
                            .unwrap_or("?");
                        tracing::error!("mcp: {} panicked: {why}", job.name);
                        Err(ToolError::internal(
                            "the tool failed unexpectedly; the server's log says why",
                        ))
                    })
            }
        };
        // Gone when the reaper has answered for it already.
        let Some(entry) = lock(&shared.calls).remove(&job.id) else {
            tracing::debug!("mcp: {} ended after its timed_out was sent", job.name);
            continue;
        };
        if entry.cancelled {
            tracing::debug!("mcp: {} was cancelled", job.name);
            continue;
        }
        shared
            .out
            .send(&result_message(&job.id, tool_result(outcome)));
    }
}

/// Stop the calls past their deadline, so that FFmpeg stops inside a read
/// too: the call then ends with `timed_out`. A call still going
/// [`STUCK_GRACE`] later is answered `timed_out` here, and its worker is
/// retired and replaced; after [`MAX_ABANDONED`] of those, the server
/// gives up.
fn reaper(shared: &Arc<Shared>) {
    loop {
        {
            let (m, cv) = &shared.reaper_sleep;
            let sleep = lock(m);
            if shared.closing.load(Ordering::Acquire) {
                return;
            }
            let _ = cv.wait_timeout(sleep, Duration::from_millis(50));
        }
        if shared.closing.load(Ordering::Acquire) {
            return;
        }
        let now = Instant::now();
        let mut answer = Vec::new();
        let mut abandoned = 0;
        lock(&shared.calls).retain(|id, e| {
            if now >= e.deadline && !e.timed_out.swap(true, Ordering::AcqRel) {
                e.stop.shutdown();
            }
            if now < e.deadline + shared.timing.stuck_grace {
                return true;
            }
            if let Some(w) = &e.worker {
                w.store(true, Ordering::Release);
                abandoned += 1;
            }
            if !e.cancelled {
                answer.push(id.clone());
            }
            false
        });
        if answer.is_empty() && abandoned == 0 {
            continue;
        }
        lock(&shared.queue).retain(|j| !answer.contains(&j.id));
        for id in &answer {
            tracing::warn!("mcp: a call didn't stop in time; answering for it");
            let e = timed_out(shared.timing.call_time);
            shared.out.send(&result_message(id, tool_result(Err(e))));
        }
        for _ in 0..abandoned {
            let n = shared.abandoned.fetch_add(1, Ordering::AcqRel) + 1;
            if n >= MAX_ABANDONED {
                tracing::error!(
                    "mcp: {n} workers are stuck inside calls; exiting so that the client \
                     starts a fresh server"
                );
                let _ = shared.events.send(Event::GaveUp);
                return;
            }
            start_worker(shared);
        }
    }
}

/// A `tools/call` result (a1-plan section 3.3): the Response as JSON
/// text, its images, and the Response again as `structuredContent` when
/// there are no images; or the error, marked `isError`.
pub fn tool_result(outcome: Result<Output, ToolError>) -> Value {
    match outcome {
        Ok(out) => {
            let mut content = vec![json!({"type": "text", "text": out.result.to_string()})];
            for image in &out.images {
                content.push(json!({
                    "type": "image",
                    "data": crate::b64::encode(&image.data),
                    "mimeType": image.mime,
                }));
            }
            let mut result = json!({ "content": content });
            if out.images.is_empty() && out.result.is_object() {
                result["structuredContent"] = out.result;
            }
            result
        }
        Err(e) => json!({
            "content": [{"type": "text", "text": e.to_json().to_string()}],
            "isError": true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_sorted() {
        let req = parse(br#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#);
        assert!(
            matches!(req, Some(Message::Request { ref id, ref method, .. }) if id.0 == "7" && method == "ping")
        );
        let req = parse(br#"  {"jsonrpc":"2.0","id":"7","method":"x","params":{"a":1}}  "#);
        assert!(
            matches!(req, Some(Message::Request { ref id, ref params, .. }) if id.0 == "\"7\"" && params["a"] == 1)
        );
        let n = parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(matches!(n, Some(Message::Notification { .. })));
        assert!(matches!(
            parse(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            Some(Message::Response)
        ));
        assert!(parse(b"   ").is_none());
        for (line, want) in [
            (&b"{oops"[..], code::PARSE_ERROR),
            (b"[{}]", code::INVALID_REQUEST),
            (b"3", code::INVALID_REQUEST),
            (
                br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
                code::INVALID_REQUEST,
            ),
            (
                br#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
                code::INVALID_REQUEST,
            ),
            (
                br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
                code::INVALID_REQUEST,
            ),
            (
                br#"{"jsonrpc":"2.0","id":1,"method":3}"#,
                code::INVALID_REQUEST,
            ),
            (
                br#"{"jsonrpc":"2.0","id":1,"method":"x","params":[1]}"#,
                code::INVALID_REQUEST,
            ),
            (br#"{"jsonrpc":"2.0","id":1}"#, code::INVALID_REQUEST),
        ] {
            match parse(line) {
                Some(Message::Invalid { code, .. }) => {
                    assert_eq!(code, want, "{}", String::from_utf8_lossy(line))
                }
                other => panic!("{}: {other:?}", String::from_utf8_lossy(line)),
            }
        }
        // The id survives where it could be read.
        let Some(Message::Invalid { id, .. }) =
            parse(br#"{"jsonrpc":"1.0","id":"a","method":"ping"}"#)
        else {
            panic!()
        };
        assert_eq!(id.map(|i| i.to_value()), Some(json!("a")));
    }

    #[test]
    fn results_carry_text_images_and_structured_content() {
        let r = tool_result(Ok(Output::json(json!({"a": 1}))));
        assert_eq!(r["content"][0]["text"], r#"{"a":1}"#);
        assert_eq!(r["structuredContent"], json!({"a": 1}));
        assert!(r.get("isError").is_none());
        let r = tool_result(Ok(Output {
            result: json!({"a": 1}),
            images: vec![Image {
                mime: "image/png",
                data: b"foo".to_vec(),
            }],
        }));
        assert_eq!(
            r["content"][1],
            json!({"type": "image", "data": "Zm9v", "mimeType": "image/png"})
        );
        assert!(r.get("structuredContent").is_none());
        let r = tool_result(Err(
            ToolError::new(codes::NOT_FOUND, "no such file").at("file")
        ));
        assert_eq!(r["isError"], true);
        let text: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap_or(""))
            .unwrap_or_default();
        assert_eq!(text["error"]["code"], "not_found");
    }
}
