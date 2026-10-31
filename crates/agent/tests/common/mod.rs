//! A scripted MCP client over pipes, and fake tools that are slow, stuck
//! or broken on demand.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::error::{ToolError, codes};
use spherecut_agent::rpc::{self, CallCx, Ended, Info, Output, Timing, Tools};

/// How long a test waits for any one message before failing.
pub const WAIT: Duration = Duration::from_secs(10);

pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// What a fake tool did, and when.
#[derive(Debug, Default)]
struct Log {
    started: HashMap<&'static str, Vec<Instant>>,
    stopped: HashMap<&'static str, Vec<Instant>>,
}

/// Tools for the protocol's tests:
///
/// - `echo` returns its arguments;
/// - `slow` reports progress every 20 ms until its call stops;
/// - `stuck` sleeps `ms` milliseconds whatever happens;
/// - `panic` panics;
/// - `refuse` fails with `bad_argument` at `times[3]`.
#[derive(Debug, Default)]
pub struct Fake {
    log: Mutex<Log>,
    changed: Condvar,
}

const NAMES: &[&str] = &["echo", "slow", "stuck", "panic", "refuse"];

impl Fake {
    fn record(&self, name: &str, started: bool) {
        let name = NAMES.iter().find(|n| **n == name).copied().unwrap_or("?");
        let mut log = self.log.lock().unwrap();
        let map = if started {
            &mut log.started
        } else {
            &mut log.stopped
        };
        map.entry(name).or_default().push(Instant::now());
        self.changed.notify_all();
    }

    fn wait_for(&self, within: Duration, done: impl Fn(&Log) -> bool) {
        let until = Instant::now() + within;
        let mut log = self.log.lock().unwrap();
        while !done(&log) {
            let left = until.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting: {log:?}");
            log = self.changed.wait_timeout(log, left).unwrap().0;
        }
    }

    pub fn runs(&self, name: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .started
            .get(name)
            .map_or(0, Vec::len)
    }

    pub fn wait_runs(&self, name: &str, n: usize, within: Duration) {
        self.wait_for(within, |l| l.started.get(name).map_or(0, Vec::len) >= n);
    }

    pub fn wait_started(&self, name: &str, within: Duration) {
        self.wait_runs(name, 1, within);
    }

    pub fn wait_stopped_count(&self, name: &str, n: usize, within: Duration) {
        self.wait_for(within, |l| l.stopped.get(name).map_or(0, Vec::len) >= n);
    }

    /// When `name` last stopped.
    pub fn wait_stopped(&self, name: &str, within: Duration) -> Instant {
        self.wait_stopped_count(name, 1, within);
        *self.log.lock().unwrap().stopped[name].last().unwrap()
    }
}

impl Tools for Fake {
    fn list(&self, _version: &str) -> Value {
        NAMES
            .iter()
            .map(|n| json!({"name": n, "description": n, "inputSchema": {"type": "object"}}))
            .collect()
    }

    fn has(&self, name: &str) -> bool {
        NAMES.contains(&name)
    }

    fn call(&self, name: &str, args: Value, cx: &CallCx) -> Result<Output, ToolError> {
        self.record(name, true);
        let out = match name {
            "echo" => Ok(Output::json(args)),
            "slow" => {
                let mut done = 0.0;
                loop {
                    if let Err(e) = cx.check() {
                        break Err(e);
                    }
                    done += 1.0;
                    cx.progress(done, None, Some("working"));
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            "stuck" => {
                let ms = args["ms"].as_u64().unwrap_or(1000);
                std::thread::sleep(Duration::from_millis(ms));
                Ok(Output::json(json!({"slept": ms})))
            }
            "panic" => panic!("boom"),
            _ => Err(
                ToolError::new(codes::BAD_ARGUMENT, "is after the clip's last frame")
                    .at("times[3]"),
            ),
        };
        self.record(name, false);
        out
    }
}

/// A server on pipes, and the client's end of them.
pub struct Session {
    to: Option<std::io::PipeWriter>,
    lines: Receiver<Value>,
    ended: Receiver<Ended>,
}

impl Session {
    pub fn start(tools: Arc<dyn Tools>) -> Session {
        Session::start_with(tools, Timing::default())
    }

    pub fn start_with(tools: Arc<dyn Tools>, timing: Timing) -> Session {
        let (in_r, in_w) = std::io::pipe().unwrap();
        let (out_r, out_w) = std::io::pipe().unwrap();
        let (ended_tx, ended) = mpsc::channel();
        std::thread::spawn(move || {
            let info = Info {
                name: "fake",
                title: "Fake",
                version: "0",
                instructions: "Use the fake tools.".into(),
            };
            let ended = rpc::serve(BufReader::new(in_r), out_w, tools, info, timing);
            let _ = ended_tx.send(ended);
        });
        let (lines_tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out_r).lines() {
                let Ok(line) = line else { break };
                // Every line is one JSON-RPC message.
                let v: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
                assert_eq!(v["jsonrpc"], "2.0", "{line}");
                let response = v.get("id").is_some()
                    && (v.get("result").is_some() != v.get("error").is_some());
                let notification = v.get("method").is_some() && v.get("id").is_none();
                assert!(
                    response || notification,
                    "not a response or notification: {line}"
                );
                if lines_tx.send(v).is_err() {
                    break;
                }
            }
        });
        Session {
            to: Some(in_w),
            lines,
            ended,
        }
    }

    pub fn send_raw(&mut self, bytes: &[u8]) {
        let to = self.to.as_mut().expect("the input is open");
        to.write_all(bytes).unwrap();
        to.flush().unwrap();
    }

    pub fn send(&mut self, v: &Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.send_raw(line.as_bytes());
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn recv_for(&self, within: Duration) -> Option<Value> {
        match self.lines.recv_timeout(within) {
            Ok(v) => Some(v),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("the server's output closed"),
        }
    }

    pub fn recv(&self) -> Value {
        self.recv_for(WAIT).expect("no message in time")
    }

    pub fn call_method(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&request(id, method, params));
        let r = self.recv();
        assert_eq!(r["id"], id, "{r}");
        r
    }

    pub fn call_tool(&mut self, id: i64, name: &str, args: Value) -> Value {
        self.call_method(id, "tools/call", json!({"name": name, "arguments": args}))
    }

    pub fn initialize(&mut self, version: &str) -> Value {
        self.call_method(
            0,
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "protocol-test", "version": "1"},
            }),
        )
    }

    /// Wait for the server to return, with its input still open.
    pub fn wait_ended(&self, within: Duration) -> Ended {
        self.ended
            .recv_timeout(within)
            .expect("serve didn't return")
    }

    /// Close the input, and wait for the server to return.
    pub fn close(self) -> Ended {
        self.close_with_lines().0
    }

    /// [`close`](Self::close), and the lines written after the input
    /// closed.
    pub fn close_with_lines(mut self) -> (Ended, Vec<Value>) {
        drop(self.to.take());
        let ended = self.wait_ended(WAIT);
        let mut lines = Vec::new();
        while let Ok(v) = self.lines.recv_timeout(Duration::from_millis(100)) {
            lines.push(v);
        }
        (ended, lines)
    }
}
