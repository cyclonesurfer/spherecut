//! `spherecut mcp` as a client runs it (a1-plan section 11.1): sessions
//! at each revision against the real tools, the stdout guard, Codex's cut
//! environment, startup before any FFmpeg or GPU setup, and the exit when
//! stdin closes. The protocol's corners are tested in process, in
//! `crates/agent/tests/protocol.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A spawned `spherecut mcp`, with every stdout line kept.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Receiver<String>,
    started: Instant,
}

impl Server {
    fn spawn(configure: impl FnOnce(&mut Command)) -> Server {
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

    fn send(&mut self, v: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(30))
            .expect("no line from the server in time");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout isn't JSON-RPC ({e}): {line}"))
    }

    fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let r = self.recv();
        assert_eq!(r["jsonrpc"], "2.0", "{r}");
        assert_eq!(r["id"], id, "{r}");
        r
    }

    fn initialize(&mut self, version: &str) -> Value {
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
    fn close(mut self) -> (std::process::ExitStatus, Duration, Vec<String>, String) {
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

fn assert_jsonrpc(line: &str) {
    let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
    assert_eq!(v["jsonrpc"], "2.0", "{line}");
}

#[test]
fn a_session_at_each_revision() {
    for version in ["2025-06-18", "2025-11-25"] {
        let mut s = Server::spawn(|_| {});
        let init = s.initialize(version);
        assert_eq!(init["result"]["protocolVersion"], version);
        assert_eq!(init["result"]["serverInfo"]["name"], "spherecut");
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("yaw")
        );
        s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        assert_eq!(s.call(1, "ping", json!({}))["result"], json!({}));
        let list = s.call(2, "tools/list", json!({}));
        let doctor = &list["result"]["tools"][0];
        assert_eq!(doctor["name"], "doctor");
        assert_eq!(doctor["annotations"]["readOnlyHint"], true);
        assert_eq!(
            doctor["inputSchema"]["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        let r = s.call(3, "tools/call", json!({"name": "doctor", "arguments": {}}));
        assert!(
            r["result"]["structuredContent"]["decode_checks"].is_array(),
            "{r}"
        );
        assert!(r["result"].get("isError").is_none(), "{r}");
        let (status, _, rest, _) = s.close();
        assert!(status.success(), "{status}");
        rest.iter().for_each(|l| assert_jsonrpc(l));
    }
}

/// A stray print to stdout lands on stderr: the protocol's stdout is a
/// copy of the descriptor, and fd 1 itself is stderr from the start.
#[test]
#[cfg(debug_assertions)]
fn a_stray_print_reaches_stderr_not_the_protocol() {
    let mut s = Server::spawn(|c| {
        c.env("SPHERECUT_DEBUG_PANIC", "mcp_print");
    });
    s.initialize("2025-06-18");
    assert_eq!(s.call(1, "ping", json!({}))["result"], json!({}));
    let (status, _, rest, stderr) = s.close();
    assert!(status.success());
    rest.iter().for_each(|l| assert_jsonrpc(l));
    assert!(
        stderr.contains("SPHERECUT_DEBUG_PANIC=mcp_print"),
        "{stderr}"
    );
}

/// Codex passes only a short list of variables to an MCP server.
#[test]
fn codex_s_environment_is_enough() {
    let keep = [
        "HOME", "LOGNAME", "PATH", "SHELL", "USER", "LANG", "LC_ALL", "TERM", "TMPDIR", "TZ",
    ];
    let mut s = Server::spawn(|c| {
        c.env_clear();
        for k in keep {
            if let Ok(v) = std::env::var(k) {
                c.env(k, v);
            }
        }
    });
    s.initialize("2025-06-18");
    assert!(s.call(1, "tools/list", json!({}))["result"]["tools"].is_array());
    let r = s.call(2, "tools/call", json!({"name": "doctor", "arguments": {}}));
    assert!(
        r["result"]["structuredContent"]["decode_checks"].is_array(),
        "{r}"
    );
    assert!(s.close().0.success());
}

/// `initialize` is answered within 1 s of the spawn, before FFmpeg is set
/// up: the first tool call does that.
#[test]
fn initialize_comes_before_any_media_setup() {
    let mut s = Server::spawn(|c| {
        c.env("SPHERECUT_LOG", "debug");
    });
    let init = s.initialize("2025-06-18");
    let took = s.started.elapsed();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(took < Duration::from_secs(1), "initialize took {took:?}");
    s.call(1, "tools/call", json!({"name": "doctor", "arguments": {}}));
    let (_, _, _, stderr) = s.close();
    let asked = stderr
        .find("asked for 2025-06-18")
        .expect("the initialize log line");
    let ffmpeg = stderr
        .find("FFmpeg initialised")
        .expect("FFmpeg's log line");
    assert!(
        asked < ffmpeg,
        "FFmpeg was set up before initialize:\n{stderr}"
    );
}

/// stdin closing with a call in flight: the server exits within 2 s, and
/// sends nothing for the call.
#[test]
fn the_server_exits_when_stdin_closes() {
    let mut s = Server::spawn(|_| {});
    s.initialize("2025-06-18");
    s.send(&json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "doctor", "arguments": {}}}));
    let (status, took, rest, _) = s.close();
    assert!(status.success(), "{status}");
    assert!(took < Duration::from_secs(2), "exit took {took:?}");
    for l in &rest {
        assert_jsonrpc(l);
        assert!(!l.contains("\"id\":9"), "answered after EOF: {l}");
    }
}

/// A root that can't be used is the command line's fault: exit 2, a line
/// on stderr, nothing on stdout, before any session.
#[test]
fn a_bad_root_stops_the_server() {
    let missing = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-such-root");
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let cases: [Vec<std::ffi::OsString>; 4] = [
        vec!["--read-root".into(), missing.clone().into()],
        vec!["--read-root".into(), file.into()],
        vec!["--allow-write".into()],
        vec!["--write-root".into(), missing.into()],
    ];
    for args in cases {
        let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
            .arg("mcp")
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(!stderr.is_empty(), "{args:?}");
    }
}

/// Roots given on the command line serve a session.
#[test]
fn roots_from_the_command_line() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mcp-roots-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = Server::spawn(|c| {
        c.arg("--read-root")
            .arg(&dir)
            .arg("--allow-write")
            .arg("--write-root")
            .arg(&dir);
    });
    s.initialize("2025-06-18");
    assert_eq!(s.call(1, "ping", json!({}))["result"], json!({}));
    assert!(s.close().0.success());
    std::fs::remove_dir_all(&dir).ok();
}
