//! Tier 2: agents' MCP sessions on the GPU, with one of Pat's samples.
//! `spherecut mcp` is spawned as a client spawns it, on the folder of the
//! first `.lrv` proxy under the samples (the X5's, H.264).
//!
//! - `mcp_session_decodes_with_vaapi` (a1-plan section 11.4 and amendment
//!   3): `doctor` with the file and a `raw` look of it must both say
//!   `vaapi` wherever doctor says VA-API decodes the file's codec.
//! - `mcp_render_vaapi` (a2-plan 9.6): a project of the file, 3 s of it
//!   rendered on VA-API's H.264 encoder, followed with `job`, and the file
//!   compared with the project.
//!
//! Where doctor says the GPU can't (no device, no driver for the codec),
//! a test skips with doctor's reason, and never passes. Run with `cargo
//! xtask hw-test`; plain `cargo test` lists them as ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod mcp_client;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// A folder for the session's outputs, removed with what it holds when
/// dropped.
struct Outputs(PathBuf);

impl Drop for Outputs {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// The finished job of the render `r` started: `job` with `wait` until
/// it ends, the ids from `id` on.
fn follow(s: &mut Server, id: &mut i64, r: &Value) -> Value {
    let end = Instant::now() + Duration::from_secs(600);
    loop {
        let j = call(s, *id, "job", json!({"job": r["job"], "wait": 30}));
        *id += 1;
        match j["state"].as_str() {
            Some("done") => return j,
            Some("failed" | "cancelled") => panic!("the render ended: {j}"),
            _ => assert!(Instant::now() < end, "timed out: {j}"),
        }
    }
}

/// a2-plan section 9.6: an agent's MCP session makes a project of the
/// first `.lrv` proxy under the samples, renders 3 s of it on the VA-API
/// H.264 encoder, follows the job, and compares the file with the
/// project: the job's check passes and every time is above PLAN's least
/// for any encoder (40 dB, 0.98). Where doctor says VA-API doesn't
/// encode H.264 here, it skips with doctor's reason.
#[test]
#[ignore = "tier2: needs a VA-API GPU and the samples"]
fn mcp_render_vaapi() {
    let samples = tier2::require_samples_dir();
    let Some(proxy) = first_proxy(&samples) else {
        tier2::skip(format!("no .lrv proxy under {}", samples.display()));
    };
    let out = Outputs(
        Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("tier2-mcp-render-{}", std::process::id())),
    );
    std::fs::remove_dir_all(&out.0).ok();
    std::fs::create_dir_all(&out.0).unwrap();
    let mut s = Server::spawn(|c| {
        c.arg("--read-root")
            .arg(proxy.parent().unwrap())
            .arg("--allow-write")
            .arg("--write-root")
            .arg(&out.0);
    });
    s.initialize("2025-11-25");
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let doctor = call(&mut s, 1, "doctor", json!({}));
    let of = |list: &str| {
        doctor[list]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["format"] == "H.264")
            .cloned()
            .unwrap_or(Value::Null)
    };
    if of("export_encoders")["encoder"] != "h264_vaapi" {
        let c = of("encode_checks");
        tier2::skip(format!(
            "doctor says VA-API doesn't encode H.264 here: {} ({})",
            c["result"].as_str().unwrap_or("no check"),
            c["reason"].as_str().unwrap_or("no reason")
        ));
    }

    let project = out.0.join("lrv.spherecut");
    let summary = call(
        &mut s,
        2,
        "project",
        json!({"file": project, "create": {"media": proxy}}),
    );
    call(
        &mut s,
        3,
        "edit",
        json!({"file": project, "edits": [{"op": "set_export", "sequence": summary["sequence"],
            "export": {"codec": "h264", "quality": "high", "encoder": "vaapi", "audio": "copy",
                       "motion_blur": false}}]}),
    );
    let r = call(
        &mut s,
        4,
        "render",
        json!({"project": project, "output": out.0.join("lrv.mp4"),
               "range": {"from": "2", "to": "5"}}),
    );
    assert_eq!(r["encoder"], "h264_vaapi", "{r}");
    let mut id = 5;
    let job = follow(&mut s, &mut id, &r);
    assert_eq!(job["report"]["encoder"], "h264_vaapi", "{job}");
    assert!(job.get("fallback").is_none(), "{job}");
    assert_eq!(job["check"]["pass"], true, "{job}");

    let c = call(
        &mut s,
        id,
        "compare",
        json!({"file": job["report"]["file"], "against_project": project, "from": "2"}),
    );
    let psnr = c["worst"]["psnr"]["psnr"].as_f64().unwrap_or(f64::INFINITY);
    let ssim = c["worst"]["ssim"]["ssim"].as_f64().unwrap();
    assert!(psnr >= 40.0 && ssim >= 0.98, "{c}");
    eprintln!(
        "{}: 3 s on h264_vaapi at {} fps, {} bytes; against the project worst {psnr:.2} dB, \
         SSIM {ssim:.5}, mean {}",
        proxy.display(),
        job["fps"],
        job["report"]["bytes"],
        c["mean"]
    );
    let (status, _, rest, _) = s.close();
    assert!(status.success(), "{status}");
    rest.iter().for_each(|l| assert_jsonrpc(l));
}
