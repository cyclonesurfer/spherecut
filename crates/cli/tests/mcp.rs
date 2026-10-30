//! `spherecut mcp` as a client runs it (a1-plan section 11.1): sessions
//! at each revision against the real tools, the stdout guard, Codex's cut
//! environment, startup before any FFmpeg or GPU setup, and the exit when
//! stdin closes; and `spherecut call`, which must give what `tools/call`
//! gives. The protocol's corners are tested in process, in
//! `crates/agent/tests/protocol.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod mcp_client;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mcp_client::{Server, assert_jsonrpc};
use serde_json::{Value, json};
use spherecut_synth::media::{self, Audio, Spec};

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A directory holding `clip.mp4`, a short equirect with stereo audio
/// and spherical metadata, and `bad.mp4`, which isn't media.
fn media_dir(name: &str) -> TempDir {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mcp-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let spec = Spec {
        name: "clip".into(),
        frames: 6,
        audio: Audio::Stereo,
        spherical: true,
        ..Spec::default()
    };
    media::write(&spec, &dir).unwrap();
    std::fs::write(dir.join("bad.mp4"), b"not a video\n").unwrap();
    TempDir(dir)
}

#[test]
fn a_session_at_each_revision() {
    let dir = media_dir("revisions");
    for version in ["2025-06-18", "2025-11-25"] {
        let mut s = Server::spawn(|c| {
            c.arg("--read-root").arg(&dir.0);
        });
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
        let tools = list["result"]["tools"].as_array().unwrap();
        let doctor = tools.iter().find(|t| t["name"] == "doctor").unwrap();
        assert_eq!(doctor["annotations"]["readOnlyHint"], true);
        // 2020-12 is the dialect either revision assumes, so the listing
        // leaves `$schema` out (a1-plan amendment 32).
        assert_eq!(doctor["inputSchema"]["type"], "object");
        assert!(doctor["inputSchema"].get("$schema").is_none());
        let look = tools.iter().find(|t| t["name"] == "look").unwrap();
        assert_eq!(look["annotations"]["readOnlyHint"], true);
        let r = s.call(3, "tools/call", json!({"name": "doctor", "arguments": {}}));
        assert!(
            r["result"]["structuredContent"]["decode_checks"].is_array(),
            "{r}"
        );
        assert!(r["result"].get("isError").is_none(), "{r}");
        let args = json!({"files": ["clip.mp4"]});
        let r = s.call(4, "tools/call", json!({"name": "probe", "arguments": args}));
        let clip = &r["result"]["structuredContent"]["files"][0];
        assert_eq!(clip["projection"], "equirect", "{r}");
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

/// Codex passes only a short list of variables to an MCP server. The
/// loader's path is kept too: `cargo xtask compat` finds the FFmpeg under
/// test through it, where an installed Spherecut finds its libraries
/// without.
#[test]
fn codex_s_environment_is_enough() {
    let keep = [
        "HOME",
        "LOGNAME",
        "PATH",
        "SHELL",
        "USER",
        "LANG",
        "LC_ALL",
        "TERM",
        "TMPDIR",
        "TZ",
        "LD_LIBRARY_PATH",
    ];
    let dir = media_dir("codex");
    let mut s = Server::spawn(|c| {
        c.env_clear();
        for k in keep {
            if let Ok(v) = std::env::var(k) {
                c.env(k, v);
            }
        }
        c.arg("--read-root").arg(&dir.0);
    });
    s.initialize("2025-06-18");
    assert!(s.call(1, "tools/list", json!({}))["result"]["tools"].is_array());
    let r = s.call(2, "tools/call", json!({"name": "doctor", "arguments": {}}));
    assert!(
        r["result"]["structuredContent"]["decode_checks"].is_array(),
        "{r}"
    );
    let args = json!({"files": ["clip.mp4"]});
    let r = s.call(3, "tools/call", json!({"name": "probe", "arguments": args}));
    assert_eq!(
        r["result"]["structuredContent"]["files"][0]["ok"], true,
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

/// `spherecut call <tool> <args>` with `dir` its read root: its exit
/// status, and its one document.
fn call(dir: &std::path::Path, tool: &str, args: &str) -> (Option<i32>, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["call", tool, args, "--read-root"])
        .arg(dir)
        .current_dir(env!("CARGO_TARGET_TMPDIR"))
        .env_remove("SPHERECUT_DEBUG_PANIC")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "one document: {stdout}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["schema"], "spherecut.call/1", "{doc}");
    assert_eq!(doc["ok"], doc.get("error").is_none(), "{doc}");
    (out.status.code(), doc)
}

/// `call` gives what `tools/call` gives, for every tool and its error
/// too (a1-plan section 3.3, amendment 7).
#[test]
fn call_and_mcp_agree() {
    let dir = media_dir("identity");
    let cases = [
        (
            "probe",
            json!({"files": ["clip.mp4", "bad.mp4", "../out.mp4"]}),
        ),
        (
            "probe",
            json!({"files": ["clip.mp4", "bad.mp4"], "detail": "full"}),
        ),
        ("probe", json!({"folder": ".", "page_size": 1})),
        ("doctor", json!({})),
        ("doctor", json!({"file": "clip.mp4"})),
        (
            "analyze",
            json!({"file": "clip.mp4", "kind": "timing", "buckets": 2}),
        ),
        (
            "analyze",
            json!({"file": "clip.mp4", "kind": "audio", "buckets": 2}),
        ),
        (
            "check",
            json!({"file": "clip.mp4", "target": "youtube_360"}),
        ),
    ];
    let mut s = Server::spawn(|c| {
        c.arg("--read-root").arg(&dir.0);
    });
    s.initialize("2025-06-18");
    for (i, (tool, args)) in cases.iter().enumerate() {
        let id = i as i64 + 1;
        let r = s.call(id, "tools/call", json!({"name": tool, "arguments": args}));
        let mcp = &r["result"]["structuredContent"];
        assert!(mcp.is_object(), "{r}");
        let (code, doc) = call(&dir.0, tool, &args.to_string());
        assert_eq!(code, Some(0), "{doc}");
        assert_eq!(&doc["result"], mcp, "{tool} {args}");
        assert_eq!(doc["warnings"], mcp["warnings"]);
    }
    let bad = json!({"files": ["clip.mp4"], "cursor": "0:00"});
    let r = s.call(99, "tools/call", json!({"name": "probe", "arguments": bad}));
    assert_eq!(r["result"]["isError"], true, "{r}");
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    let mcp: Value = serde_json::from_str(text).unwrap();
    let (code, doc) = call(&dir.0, "probe", &bad.to_string());
    assert_eq!(code, Some(2));
    assert_eq!(doc["error"], mcp["error"]);
    assert!(s.close().0.success());
}

/// RFC 4648 base64, as `tools/call` sends an image.
fn unbase64(s: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("not base64: {c}"),
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for quad in s.as_bytes().chunks(4) {
        let digits: Vec<u8> = quad
            .iter()
            .take_while(|&&c| c != b'=')
            .map(|&c| value(c))
            .collect();
        let n = digits
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &d)| n | u32::from(d) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..digits.len()]);
    }
    out
}

/// look by `call` and by `tools/call`: the same JSON, and the same image,
/// `call`'s written to `--image-dir` and named in `images` (amendment 32).
#[test]
fn look_by_call_and_mcp_agree() {
    let dir = media_dir("look");
    let images = dir.0.join("images");
    std::fs::create_dir(&images).unwrap();
    let args = json!({
        "file": "clip.mp4",
        "times": ["0", "3f"],
        "view": {"yaw": 90, "fov": 100},
        "size": 256,
    });
    let mut s = Server::spawn(|c| {
        c.arg("--read-root").arg(&dir.0);
    });
    s.initialize("2025-11-25");
    let r = s.call(1, "tools/call", json!({"name": "look", "arguments": args}));
    let result = &r["result"];
    assert!(result.get("isError").is_none(), "{r}");
    assert!(result.get("structuredContent").is_none(), "{r}");
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{r}");
    let mcp: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/jpeg");
    let sent = unbase64(content[1]["data"].as_str().unwrap());
    assert!(sent.starts_with(&[0xff, 0xd8]), "a JPEG");
    assert_eq!(mcp["image"]["bytes"], sent.len());
    assert_eq!(mcp["tiles"].as_array().unwrap().len(), 2, "{mcp}");
    assert!(s.close().0.success());

    let out = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["call", "look", &args.to_string(), "--read-root"])
        .arg(&dir.0)
        .arg("--image-dir")
        .arg(&images)
        .env_remove("SPHERECUT_DEBUG_PANIC")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["result"], mcp);
    let written = doc["images"].as_array().unwrap();
    assert_eq!(written.len(), 1, "{doc}");
    let path = std::path::Path::new(written[0].as_str().unwrap());
    assert_eq!(path.parent(), Some(images.as_path()));
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("spherecut-look-") && name.ends_with(".jpg"),
        "{name}"
    );
    assert_eq!(std::fs::read(path).unwrap(), sent, "the same JPEG");
    // A tool without images gives no `images`.
    let (_, doc) = call(&dir.0, "doctor", "{}");
    assert!(doc.get("images").is_none(), "{doc}");
}

/// D27's exit statuses: 2 for usage (arguments that aren't JSON, an
/// unknown tool, a bad argument, a bad root), 1 for a tool's error.
#[test]
fn call_exits_as_d27_says() {
    let dir = media_dir("exits");
    let cases = [
        ("probe", "{files", 2, "usage"),
        ("nope", "{}", 2, "bad_argument"),
        (
            "probe",
            r#"{"files": ["clip.mp4"], "detail": "most"}"#,
            2,
            "bad_argument",
        ),
        ("probe", r#"{"folder": "missing"}"#, 1, "not_found"),
        ("probe", r#"{"folder": "clip.mp4"}"#, 1, "not_a_folder"),
    ];
    for (tool, args, exit, code) in cases {
        let (status, doc) = call(&dir.0, tool, args);
        assert_eq!(status, Some(exit), "{tool} {args}: {doc}");
        assert_eq!(doc["error"]["code"], code, "{tool} {args}: {doc}");
    }
    // A root that can't be used.
    let (status, doc) = call(&dir.0.join("missing"), "probe", "{}");
    assert_eq!(status, Some(2), "{doc}");
    assert_eq!(doc["error"]["code"], "usage");
    // A tool's success with failed files is still the call's success.
    let (status, doc) = call(&dir.0, "probe", r#"{"files": ["bad.mp4"]}"#);
    assert_eq!(status, Some(0), "{doc}");
    assert_eq!(doc["result"]["files"][0]["ok"], false);
}

/// SIGINT during a call cancels it: exit 4 with `cancelled`.
#[test]
#[cfg(debug_assertions)]
fn sigint_cancels_a_call_with_exit_4() {
    let dir = media_dir("sigint");
    let child = Command::new(env!("CARGO_BIN_EXE_spherecut"))
        .args(["call", "probe", r#"{"files": ["clip.mp4"]}"#, "--read-root"])
        .arg(&dir.0)
        .env("SPHERECUT_DEBUG_PANIC", "call_wait")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let sent = Instant::now();
    let out = child.wait_with_output().unwrap();
    assert!(
        sent.elapsed() < Duration::from_secs(2),
        "{:?}",
        sent.elapsed()
    );
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["error"]["code"], "cancelled", "{doc}");
}
