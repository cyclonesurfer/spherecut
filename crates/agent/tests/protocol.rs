//! The MCP layer in process (a1-plan section 11.1, amendments 15, 20 and
//! 26): sessions over pipes against fake tools, so that slow, stuck,
//! panicking and progress-reporting calls can be had on demand. The
//! spawned `spherecut mcp` is tested in `crates/cli/tests/mcp.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Fake, Session, request};
use serde_json::{Value, json};
use spherecut_agent::limits::{EXIT_GRACE, MAX_ABANDONED, MAX_QUEUED, MAX_REQUEST_BYTES};
use spherecut_agent::rpc::{SUPPORTED, Timing, code};

fn fake() -> Arc<Fake> {
    Arc::new(Fake::default())
}

fn error_code(v: &Value) -> i64 {
    v["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("not an error: {v}"))
}

/// The tool error inside an `isError` result.
fn tool_error(v: &Value) -> Value {
    assert_eq!(v["result"]["isError"], true, "{v}");
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str::<Value>(text).unwrap()["error"].clone()
}

#[test]
fn both_revisions_and_an_unknown_one_negotiate() {
    for (asked, served) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("2025-03-26", "2025-03-26"),
        ("2024-11-05", "2024-11-05"),
        ("2099-01-01", "2025-11-25"),
    ] {
        let mut s = Session::start(fake());
        let init = s.initialize(asked);
        assert_eq!(init["result"]["protocolVersion"], served, "{init}");
        assert_eq!(init["result"]["serverInfo"]["name"], "fake");
        assert_eq!(
            init["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
        s.notify("notifications/initialized", json!({}));
        assert_eq!(s.call_method(2, "ping", json!({}))["result"], json!({}));
        let list = s.call_method(3, "tools/list", json!({}));
        assert!(list["result"]["tools"].is_array(), "{list}");
        let echo = s.call_tool(4, "echo", json!({"a": 1}));
        assert_eq!(
            echo["result"]["structuredContent"],
            json!({"a": 1}),
            "{echo}"
        );
        s.close();
    }
}

#[test]
fn the_lifecycle_is_enforced() {
    let mut s = Session::start(fake());
    // ping is fine before initialize; nothing else is.
    assert_eq!(s.call_method(1, "ping", json!({}))["result"], json!({}));
    let early = s.call_method(2, "tools/list", json!({}));
    assert_eq!(error_code(&early), code::INVALID_REQUEST, "{early}");
    s.initialize("2025-06-18");
    let again = s.call_method(9, "initialize", json!({"protocolVersion": "2025-06-18"}));
    assert_eq!(error_code(&again), code::INVALID_REQUEST, "{again}");
    // Some clients send tools/list before notifications/initialized.
    assert!(s.call_method(3, "tools/list", json!({}))["result"]["tools"].is_array());
    s.close();
}

#[test]
fn a_modern_request_learns_the_supported_revisions() {
    let mut s = Session::start(fake());
    let meta = json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}});
    for (id, method, params, requested) in [
        (1, "server/discover", meta.clone(), "2026-07-28"),
        (2, "server/discover", json!({}), ""),
        (3, "tools/list", meta.clone(), "2026-07-28"),
    ] {
        let r = s.call_method(id, method, params);
        assert_eq!(error_code(&r), code::UNSUPPORTED_PROTOCOL_VERSION, "{r}");
        assert_eq!(r["error"]["data"]["supported"], json!(SUPPORTED));
        assert_eq!(r["error"]["data"]["requested"], requested);
    }
    // Then the legacy handshake works on the same connection.
    assert_eq!(
        s.initialize("2025-11-25")["result"]["protocolVersion"],
        "2025-11-25"
    );
    s.close();
}

#[test]
fn unknown_tools_methods_and_bad_params() {
    let mut s = Session::start(fake());
    s.initialize("2025-06-18");
    let r = s.call_tool(1, "no_such_tool", json!({}));
    assert_eq!(error_code(&r), code::INVALID_PARAMS, "{r}");
    let r = s.call_method(2, "resources/list", json!({}));
    assert_eq!(error_code(&r), code::METHOD_NOT_FOUND, "{r}");
    let r = s.call_method(3, "tools/call", json!({"name": "echo", "arguments": [1]}));
    assert_eq!(error_code(&r), code::INVALID_PARAMS, "{r}");
    let r = s.call_method(4, "tools/call", json!({"arguments": {}}));
    assert_eq!(error_code(&r), code::INVALID_PARAMS, "{r}");
    // A tool's own refusal is a result, not a protocol error.
    let r = s.call_tool(5, "refuse", json!({}));
    let e = tool_error(&r);
    assert_eq!(e["code"], "bad_argument");
    assert_eq!(e["argument"], "times[3]");
    s.close();
}

#[test]
fn malformed_lines_are_answered_and_the_session_goes_on() {
    let mut s = Session::start(fake());
    s.initialize("2025-06-18");
    s.send_raw(b"{oops\n");
    let r = s.recv();
    assert_eq!(error_code(&r), code::PARSE_ERROR);
    assert_eq!(r["id"], Value::Null);
    s.send_raw(br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#);
    s.send_raw(b"\n");
    let r = s.recv();
    assert_eq!(error_code(&r), code::INVALID_REQUEST, "a batch: {r}");
    // A line one byte over the limit, then a ping.
    let mut long = br#"{"jsonrpc":"2.0","id":2,"method":"ping","params":{"pad":""#.to_vec();
    long.resize(MAX_REQUEST_BYTES + 1 - 3, b'x');
    long.extend_from_slice(b"\"}}");
    assert_eq!(long.len(), MAX_REQUEST_BYTES + 1);
    long.push(b'\n');
    s.send_raw(&long);
    let r = s.recv();
    assert_eq!(error_code(&r), code::INVALID_REQUEST, "{r}");
    assert_eq!(r["id"], Value::Null);
    // One at the limit is read.
    let mut at = br#"{"jsonrpc":"2.0","id":3,"method":"ping","params":{"pad":""#.to_vec();
    at.resize(MAX_REQUEST_BYTES - 3, b'x');
    at.extend_from_slice(b"\"}}\n");
    s.send_raw(&at);
    assert_eq!(s.recv()["id"], 3);
    // Blank lines and responses from the client get nothing.
    s.send_raw(b"\n   \n");
    s.send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":77,\"result\":{}}\n");
    assert_eq!(s.call_method(4, "ping", json!({}))["id"], 4);
    s.close();
}

#[test]
fn ids_keep_their_type() {
    let mut s = Session::start(fake());
    s.initialize("2025-06-18");
    s.send(&json!({"jsonrpc": "2.0", "id": "7", "method": "ping"}));
    assert_eq!(s.recv()["id"], json!("7"));
    s.send(&json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}));
    assert_eq!(s.recv()["id"], json!(7));
    s.close();
}

#[test]
fn a_cancelled_call_gets_no_response_and_stops() {
    let tools = fake();
    let mut s = Session::start(tools.clone());
    s.initialize("2025-06-18");
    s.send(&request(
        1,
        "tools/call",
        json!({"name": "slow", "arguments": {}}),
    ));
    tools.wait_started("slow", Duration::from_secs(5));
    let sent = Instant::now();
    s.notify(
        "notifications/cancelled",
        json!({"requestId": 1, "reason": "test"}),
    );
    let stopped = tools.wait_stopped("slow", Duration::from_secs(5));
    assert!(
        stopped.duration_since(sent) < Duration::from_secs(1),
        "stopped after {:?}",
        stopped.duration_since(sent)
    );
    // A ping after is answered, and nothing ever comes for id 1.
    assert_eq!(s.call_method(2, "ping", json!({}))["id"], 2);
    assert!(s.recv_for(Duration::from_millis(300)).is_none());
    s.close();
}

#[test]
fn a_cancelled_queued_call_never_runs() {
    let tools = fake();
    let mut s = Session::start(tools.clone());
    s.initialize("2025-06-18");
    for id in 1..=2 {
        s.send(&request(
            id,
            "tools/call",
            json!({"name": "slow", "arguments": {}}),
        ));
    }
    tools.wait_runs("slow", 2, Duration::from_secs(5));
    s.send(&request(
        3,
        "tools/call",
        json!({"name": "echo", "arguments": {"x": 3}}),
    ));
    s.notify("notifications/cancelled", json!({"requestId": 3}));
    for id in 1..=2 {
        s.notify("notifications/cancelled", json!({"requestId": id}));
    }
    tools.wait_stopped_count("slow", 2, Duration::from_secs(5));
    assert_eq!(s.call_method(4, "ping", json!({}))["id"], 4);
    assert!(s.recv_for(Duration::from_millis(300)).is_none());
    assert_eq!(tools.runs("echo"), 0, "the cancelled call ran");
    s.close();
}

#[test]
fn a_ping_is_answered_while_calls_run_and_a_full_queue_is_busy() {
    let tools = fake();
    let mut s = Session::start(tools.clone());
    s.initialize("2025-06-18");
    // Two running, MAX_QUEUED waiting, then one too many.
    for id in 1..=2 {
        s.send(&request(
            id,
            "tools/call",
            json!({"name": "slow", "arguments": {}}),
        ));
    }
    tools.wait_runs("slow", 2, Duration::from_secs(5));
    assert_eq!(s.call_method(100, "ping", json!({}))["id"], 100);
    let queued = 3..3 + MAX_QUEUED as i64;
    for id in queued.clone() {
        s.send(&request(
            id,
            "tools/call",
            json!({"name": "echo", "arguments": {"id": id}}),
        ));
    }
    let r = s.call_tool(999, "echo", json!({}));
    assert_eq!(tool_error(&r)["code"], "busy", "{r}");
    // Let them all go: each queued call is answered once.
    for id in 1..=2 {
        s.notify("notifications/cancelled", json!({"requestId": id}));
    }
    let mut answered: Vec<i64> = queued
        .clone()
        .map(|_| s.recv()["id"].as_i64().unwrap())
        .collect();
    answered.sort_unstable();
    assert_eq!(answered, queued.collect::<Vec<_>>());
    s.close();
}

#[test]
fn a_duplicate_id_in_flight_is_refused() {
    let tools = fake();
    let mut s = Session::start(tools.clone());
    s.initialize("2025-06-18");
    s.send(&request(
        1,
        "tools/call",
        json!({"name": "slow", "arguments": {}}),
    ));
    tools.wait_started("slow", Duration::from_secs(5));
    let r = s.call_tool(1, "echo", json!({}));
    assert_eq!(error_code(&r), code::INVALID_REQUEST, "{r}");
    s.notify("notifications/cancelled", json!({"requestId": 1}));
    s.close();
}

#[test]
fn a_call_past_its_time_ends_with_timed_out() {
    let timing = Timing {
        call_time: Duration::from_millis(300),
        stuck_grace: Duration::from_secs(5),
    };
    let tools = fake();
    let mut s = Session::start_with(tools.clone(), timing);
    s.initialize("2025-06-18");
    let started = Instant::now();
    let r = s.call_tool(1, "slow", json!({}));
    let e = tool_error(&r);
    assert_eq!(e["code"], "timed_out", "{r}");
    assert!(e["message"].as_str().unwrap().contains("0.3 s"), "{e}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    s.close();
}

#[test]
fn a_stuck_call_is_answered_and_its_worker_replaced() {
    let timing = Timing {
        call_time: Duration::from_millis(200),
        stuck_grace: Duration::from_millis(200),
    };
    let tools = fake();
    let mut s = Session::start_with(tools.clone(), timing);
    s.initialize("2025-06-18");
    let started = Instant::now();
    s.send(&request(
        1,
        "tools/call",
        json!({"name": "stuck", "arguments": {"ms": 1500}}),
    ));
    let r = s.recv();
    assert_eq!(r["id"], 1);
    assert_eq!(tool_error(&r)["code"], "timed_out", "{r}");
    let after = started.elapsed();
    assert!(after < Duration::from_secs(1), "answered after {after:?}");
    // Both workers are there still: two slow calls run at once.
    for id in 2..=3 {
        s.send(&request(
            id,
            "tools/call",
            json!({"name": "slow", "arguments": {}}),
        ));
    }
    tools.wait_runs("slow", 2, Duration::from_secs(2));
    for id in 2..=3 {
        s.notify("notifications/cancelled", json!({"requestId": id}));
    }
    // The stuck call ends later; nothing more comes for id 1.
    tools.wait_stopped("stuck", Duration::from_secs(5));
    assert_eq!(s.call_method(4, "ping", json!({}))["id"], 4);
    assert!(s.recv_for(Duration::from_millis(300)).is_none());
    let ended = s.close();
    assert!(!ended.gave_up);
}

#[test]
fn the_server_gives_up_after_too_many_stuck_workers() {
    let timing = Timing {
        call_time: Duration::from_millis(100),
        stuck_grace: Duration::from_millis(100),
    };
    let tools = fake();
    let mut s = Session::start_with(tools.clone(), timing);
    s.initialize("2025-06-18");
    // Two at a time, one a worker, so that each runs and is stuck rather
    // than timing out in the queue; the reaper answers each pair before
    // the next goes.
    let mut id = 0;
    while id < MAX_ABANDONED as i64 {
        let pair = [id + 1, id + 2];
        for id in pair {
            s.send(&request(
                id,
                "tools/call",
                json!({"name": "stuck", "arguments": {"ms": 3000}}),
            ));
        }
        tools.wait_runs("stuck", pair[1] as usize, Duration::from_secs(5));
        id += 2;
        if id < MAX_ABANDONED as i64 {
            for _ in pair {
                assert_eq!(tool_error(&s.recv())["code"], "timed_out");
            }
        }
    }
    // serve returns with the input still open.
    let ended = s.wait_ended(Duration::from_secs(10));
    assert!(ended.gave_up, "{ended:?}");
}

#[test]
fn the_input_ending_stops_calls_in_time() {
    // A call that stops when asked: none left behind.
    let tools = fake();
    let mut s = Session::start(tools.clone());
    s.initialize("2025-06-18");
    s.send(&request(
        1,
        "tools/call",
        json!({"name": "slow", "arguments": {}}),
    ));
    tools.wait_started("slow", Duration::from_secs(5));
    let closed = Instant::now();
    let (ended, lines) = s.close_with_lines();
    assert!(
        closed.elapsed() < Duration::from_millis(500),
        "{:?}",
        closed.elapsed()
    );
    assert_eq!(ended.stuck, 0);
    assert!(
        lines.iter().all(|l| l["id"] != 1),
        "a response after EOF: {lines:?}"
    );

    // A call that doesn't: serve returns after the grace, and says so.
    let tools = fake();
    let mut s = Session::start(tools.clone());
    s.initialize("2025-06-18");
    s.send(&request(
        1,
        "tools/call",
        json!({"name": "stuck", "arguments": {"ms": 4000}}),
    ));
    tools.wait_started("stuck", Duration::from_secs(5));
    let closed = Instant::now();
    let ended = s.close();
    let took = closed.elapsed();
    assert!(
        took >= EXIT_GRACE && took < EXIT_GRACE + Duration::from_millis(400),
        "{took:?}"
    );
    assert_eq!(ended.stuck, 1);
}

#[test]
fn a_panicking_tool_is_an_internal_error_and_the_server_goes_on() {
    let mut s = Session::start(fake());
    s.initialize("2025-06-18");
    let r = s.call_tool(1, "panic", json!({}));
    let e = tool_error(&r);
    assert_eq!(e["code"], "internal");
    assert!(
        !e["message"].as_str().unwrap().contains("boom"),
        "the panic's text leaked: {e}"
    );
    assert_eq!(
        s.call_tool(2, "echo", json!({"ok": true}))["result"]["structuredContent"]["ok"],
        true
    );
    s.close();
}

#[test]
fn progress_only_with_a_token_and_always_increasing() {
    let timing = Timing {
        call_time: Duration::from_millis(1200),
        stuck_grace: Duration::from_secs(5),
    };
    let mut s = Session::start_with(fake(), timing);
    s.initialize("2025-06-18");
    s.send(&request(
        1,
        "tools/call",
        json!({"name": "slow", "arguments": {}, "_meta": {"progressToken": "p1"}}),
    ));
    let mut progress = Vec::new();
    let result = loop {
        let m = s.recv();
        if m["method"] == "notifications/progress" {
            assert_eq!(m["params"]["progressToken"], "p1");
            progress.push(m["params"]["progress"].as_f64().unwrap());
        } else {
            break m;
        }
    };
    assert_eq!(result["id"], 1);
    // 1.2 s at most every 250 ms: 2 to 5 notifications.
    assert!((2..=5).contains(&progress.len()), "{progress:?}");
    assert!(progress.windows(2).all(|w| w[1] > w[0]), "{progress:?}");
    // Without a token, none.
    s.send(&request(
        2,
        "tools/call",
        json!({"name": "slow", "arguments": {}}),
    ));
    let m = s.recv();
    assert_eq!(m["id"], 2, "{m}");
    s.close();
}

#[test]
fn spherecut_lists_its_tools_and_runs_doctor() {
    let roots = spherecut_agent::Roots::new(&[], None).unwrap();
    let mut s = Session::start(Arc::new(spherecut_agent::tools::Spherecut::new(roots)));
    s.initialize("2025-11-25");
    let list = s.call_method(1, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["doctor"]);
    let r = s.call_tool(2, "doctor", json!({"x": 1}));
    let e = tool_error(&r);
    assert_eq!(
        (e["code"].as_str(), e["argument"].as_str()),
        (Some("bad_argument"), Some("x")),
        "{e}"
    );
    let r = s.call_tool(3, "doctor", json!({}));
    let report = &r["result"]["structuredContent"];
    assert!(report["decode_checks"].is_array(), "{r}");
    assert_eq!(report["warnings"], json!([]));
    assert_eq!(
        r["result"]["content"][0]["text"]
            .as_str()
            .map(|t| serde_json::from_str::<Value>(t).unwrap()),
        Some(report.clone())
    );
    s.close();
}
