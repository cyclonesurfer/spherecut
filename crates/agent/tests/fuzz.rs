//! The JSON-RPC reader under arbitrary input (a1-plan section 11.1):
//! bytes and JSON values never panic it, a line gets at most one
//! response, and every well-formed request's id comes back once, of the
//! same type.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Fake, Session};
use proptest::prelude::*;
use serde_json::{Value, json};
use spherecut_agent::rpc;

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        any::<f64>()
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(|f| json!(f)),
        ".{0,12}".prop_map(Value::String),
        prop_oneof![
            Just("2.0"),
            Just("ping"),
            Just("tools/call"),
            Just("id"),
            Just("method")
        ]
        .prop_map(|s| json!(s)),
    ];
    leaf.prop_recursive(3, 24, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![
                    Just("jsonrpc".to_owned()),
                    Just("id".to_owned()),
                    Just("method".to_owned()),
                    Just("params".to_owned()),
                    Just("result".to_owned()),
                    "[a-z]{1,6}",
                ],
                inner,
                0..5
            )
            .prop_map(|m: BTreeMap<String, Value>| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// One line the fuzzer sends, newline excluded.
#[derive(Debug, Clone)]
enum Line {
    /// Anything at all, newlines aside.
    Bytes(Vec<u8>),
    /// Any JSON value.
    Json(Value),
    /// A request that must be answered: the method, and whether its id
    /// is a string.
    Request(&'static str, bool),
}

fn line() -> impl Strategy<Value = Line> {
    prop_oneof![
        prop::collection::vec(
            any::<u8>().prop_map(|b| if b == b'\n' { b' ' } else { b }),
            0..64
        )
        .prop_map(Line::Bytes),
        json_value().prop_map(Line::Json),
        (
            prop_oneof![
                Just("ping"),
                Just("tools/list"),
                Just("tools/call"),
                Just("initialize"),
                Just("no/such"),
                Just("server/discover"),
            ],
            any::<bool>()
        )
            .prop_map(|(m, s)| Line::Request(m, s)),
    ]
}

proptest! {
    #[test]
    fn parse_never_panics_on_bytes(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = rpc::parse(&bytes);
    }

    #[test]
    fn parse_never_panics_on_json(v in json_value()) {
        let _ = rpc::parse(v.to_string().as_bytes());
    }
}

proptest! {
    // A server per case: its cost is threads, so the count is its own.
    #![proptest_config(ProptestConfig::with_cases(
        std::env::var("PROPTEST_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(64).min(20_000)
    ))]

    #[test]
    fn every_request_is_answered_once(lines in prop::collection::vec(line(), 1..24)) {
        let mut s = Session::start(Arc::new(Fake::default()));
        s.initialize("2025-06-18");
        let mut expected: Vec<Value> = Vec::new();
        // Every id a line carried, which an error may echo.
        let mut carried: Vec<Value> = Vec::new();
        let mut sent = 0usize;
        for (i, l) in lines.iter().enumerate() {
            let bytes = match l {
                Line::Bytes(b) => b.clone(),
                Line::Json(v) => v.to_string().into_bytes(),
                Line::Request(method, string_id) => {
                    let id = if *string_id { json!(format!("r{i}")) } else { json!(1000 + i) };
                    let params = match *method {
                        "tools/call" => json!({"name": "echo", "arguments": {"i": i}}),
                        "initialize" => json!({"protocolVersion": "2025-06-18"}),
                        _ => json!({}),
                    };
                    expected.push(id.clone());
                    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
                        .to_string()
                        .into_bytes()
                }
            };
            if !bytes.trim_ascii().is_empty() {
                sent += 1;
            }
            if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(&bytes)
                && let Some(id) = m.get("id")
            {
                carried.push(id.clone());
            }
            s.send_raw(&bytes);
            s.send_raw(b"\n");
        }
        // Every expected id once, then a last ping: anything the reader
        // answers itself is written before that ping's answer.
        let mut seen: Vec<Value> = Vec::new();
        let mut responses = 0usize;
        let until = Instant::now() + Duration::from_secs(10);
        let collect = |s: &Session, seen: &mut Vec<Value>, responses: &mut usize, stop: &dyn Fn(&[Value]) -> bool| {
            while !stop(seen) {
                prop_assert!(Instant::now() < until, "waiting for {expected:?}, have {seen:?}");
                if let Some(m) = s.recv_for(Duration::from_millis(100))
                    && m.get("id").is_some()
                {
                    *responses += 1;
                    seen.push(m["id"].clone());
                }
            }
            Ok(())
        };
        collect(&s, &mut seen, &mut responses, &|seen| expected.iter().all(|id| seen.contains(id)))?;
        s.send(&json!({"jsonrpc": "2.0", "id": "end", "method": "ping"}));
        collect(&s, &mut seen, &mut responses, &|seen| seen.contains(&json!("end")))?;
        prop_assert!(responses <= sent + 1, "{responses} responses to {sent} lines");
        for id in &expected {
            prop_assert_eq!(seen.iter().filter(|s| *s == id).count(), 1, "{} in {:?}", id, seen);
        }
        for id in &seen {
            let ok = id.is_null() || *id == json!("end") || carried.contains(id);
            prop_assert!(ok, "an id never sent: {}", id);
        }
        s.close();
    }
}
