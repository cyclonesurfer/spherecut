//! A spawned `spherecut mcp` and a client for it, line by line, for the
//! tests that run the server as a client does (`mcp.rs`, `tier2_mcp.rs`).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A spawned `spherecut mcp`, with every stdout line kept.
pub struct Server {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub lines: Receiver<String>,
    pub stderr: Receiver<String>,
    pub started: Instant,
}

impl Server {
    pub fn spawn(configure: impl FnOnce(&mut Command)) -> Server {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
        cmd.arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("SPHERECUT_DEBUG_PANIC")
            .env_remove("SPHERECUT_LOG");
        configure(&mut cmd);
        let started = Instant::now();
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take();
        let lines = lines_of(child.stdout.take().unwrap());
        let stderr = lines_of(child.stderr.take().unwrap());
        Server {
            child,
            stdin,
            lines,
            stderr,
            started,
        }
    }

    pub fn send(&mut self, v: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    pub fn recv(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(30))
            .expect("no line from the server in time");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout isn't JSON-RPC ({e}): {line}"))
    }

    pub fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let r = self.recv();
        assert_eq!(r["jsonrpc"], "2.0", "{r}");
        assert_eq!(r["id"], id, "{r}");
        r
    }

    pub fn initialize(&mut self, version: &str) -> Value {
        self.call(
            0,
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "mcp-test", "version": "1"},
            }),
        )
    }

    /// Close stdin; the exit status, how long the exit took, and every
    /// line left on stdout and stderr.
    pub fn close(mut self) -> (std::process::ExitStatus, Duration, Vec<String>, String) {
        drop(self.stdin.take());
        let closed = Instant::now();
        let status = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s;
            }
            if closed.elapsed() > Duration::from_secs(10) {
                let _ = self.child.kill();
                panic!("spherecut mcp didn't exit after stdin closed");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let took = closed.elapsed();
        let rest: Vec<String> = self.lines.iter().collect();
        let stderr: Vec<String> = self.stderr.iter().collect();
        (status, took, rest, stderr.join("\n"))
    }
}

fn lines_of(r: impl Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(r).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

pub fn assert_jsonrpc(line: &str) {
    let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
    assert_eq!(v["jsonrpc"], "2.0", "{line}");
}
