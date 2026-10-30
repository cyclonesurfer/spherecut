//! Tier 2: an agent's MCP session decodes one of Pat's samples on the GPU
//! (a1-plan section 11.4 and amendment 3). `spherecut mcp` is spawned as
//! a client spawns it, on the folder of the first `.lrv` proxy under the
//! samples (the X5's, H.264), and asked for `doctor` with the file and a
//! `raw` look of it: both must say `vaapi` wherever doctor says VA-API
//! decodes the file's codec. Where it doesn't (no device, no driver for
//! the codec), the test skips with doctor's reason, and never passes.
//!
//! Run with `cargo xtask hw-test`; plain `cargo test` lists it as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod mcp_client;

use std::path::{Path, PathBuf};

use mcp_client::{Server, assert_jsonrpc};
use serde_json::{Value, json};
use spherecut_testkit::tier2;

/// The first `.lrv` under `dir`, by path.
fn first_proxy(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lrv")) {
                out.push(p);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found.sort();
    found.into_iter().next()
}

/// `tools/call` of `name`; its result, which must not be an error.
fn call(s: &mut Server, id: i64, name: &str, args: Value) -> Value {
    let r = s.call(id, "tools/call", json!({"name": name, "arguments": args}));
    assert!(r["result"].get("isError").is_none(), "{name}: {r}");
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

#[test]
#[ignore = "tier2: needs a VA-API GPU and the samples"]
fn mcp_session_decodes_with_vaapi() {
    let samples = tier2::require_samples_dir();
    let Some(proxy) = first_proxy(&samples) else {
        tier2::skip(format!("no .lrv proxy under {}", samples.display()));
    };
    let folder = proxy.parent().unwrap();
    let name = proxy.file_name().unwrap().to_str().unwrap();
    let mut s = Server::spawn(|c| {
        c.arg("--read-root").arg(folder);
    });
    let init = s.initialize("2025-11-25");
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let doctor = call(&mut s, 1, "doctor", json!({"file": name}));
    let file = &doctor["file"];
    let codec = file["codec"].as_str().unwrap().to_owned();
    let vaapi: Vec<&Value> = doctor["decode_checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["codec"] == codec.as_str() && c["path"] == "vaapi")
        .collect();
    if !vaapi.iter().any(|c| c["result"] == "works") {
        let why: Vec<String> = vaapi
            .iter()
            .map(|c| {
                let s = |k: &str| c[k].as_str().unwrap_or("?").to_owned();
                format!("{}: {} ({})", s("format"), s("result"), s("reason"))
            })
            .collect();
        tier2::skip(format!(
            "doctor says VA-API doesn't decode {codec} here: {}",
            if why.is_empty() {
                "no check".to_owned()
            } else {
                why.join("; ")
            }
        ));
    }
    assert_eq!(file["decode"]["path"], "vaapi", "doctor: {file}");

    let look = call(&mut s, 2, "look", json!({"file": name, "mode": "raw"}));
    let tiles = look["tiles"].as_array().unwrap();
    assert!(!tiles.is_empty(), "{look}");
    for t in tiles {
        assert_eq!(t["decode"]["path"], "vaapi", "look: {t}");
    }
    eprintln!(
        "{name}: {codec} {} on vaapi; look {}x{}, {} bytes",
        file["size"], look["image"]["width"], look["image"]["height"], look["image"]["bytes"]
    );
    let (status, _, rest, _) = s.close();
    assert!(status.success(), "{status}");
    rest.iter().for_each(|l| assert_jsonrpc(l));
}
