//! `job` (a2-plan 5.2): a render's state and progress, waited for or
//! cancelled; once done, its file's report and `check` of it against
//! the target its output fits.

use std::time::Instant;

use serde::Deserialize;
use serde_json::{Value, json};
use spherecut_export::{Fallback, Report};

use super::Ctx;
use super::project::time;
use crate::error::{ToolError, codes};
use crate::jobs::{Jobs, State, Status};
use crate::limits::{CANCEL_WAIT, JOB_WAIT, JOB_WAIT_MARGIN};
use crate::text;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub job: String,
    pub wait: Option<f64>,
    #[serde(default)]
    pub cancel: bool,
}

pub fn run(req: Request, cx: &Ctx, jobs: &Jobs) -> Result<Value, ToolError> {
    cx.call.check()?;
    if req.cancel && req.wait.is_some() {
        return Err(ToolError::new(
            codes::BAD_ARGUMENT,
            "cancel waits for the render to stop already",
        )
        .at("wait")
        .next("leave wait out"));
    }
    let n = jobs.find("job", &req.job)?;
    let gone = || ToolError::new(codes::NOT_FOUND, "isn't a job this server keeps").at("job");
    let mut waited_out = false;
    let status = if req.cancel {
        jobs.cancel(n, Instant::now() + CANCEL_WAIT)
            .ok_or_else(gone)?
    } else if let Some(w) = req.wait {
        let w = std::time::Duration::try_from_secs_f64(w)
            .ok()
            .filter(|w| *w <= JOB_WAIT)
            .ok_or_else(|| {
                ToolError::new(codes::OUT_OF_RANGE, "isn't a wait job takes")
                    .at("wait")
                    .valid(format!("0 to {} seconds", JOB_WAIT.as_secs()))
            })?;
        let until = Instant::now() + w.min(cx.call.remaining().saturating_sub(JOB_WAIT_MARGIN));
        let (s, waited) = jobs
            .wait(n, until, || cx.call.check().is_err(), |s| progress(cx, s))
            .ok_or_else(gone)?;
        waited_out = !waited;
        s
    } else {
        jobs.status(n).ok_or_else(gone)?
    };
    cx.call.check()?;
    let mut v = respond(&status);
    if waited_out {
        v["note"] = json!("another call is waiting on a job, so this one answered at once");
    }
    if status.state == State::Done {
        let check = match status.check {
            Some(c) => c,
            None => {
                let c = super::check::run(
                    super::check::Request {
                        file: status.output.clone(),
                        target: status.target,
                    },
                    cx,
                )
                .unwrap_or_else(|e| json!({"error": e.to_json()["error"]}));
                jobs.checked(n, c.clone());
                c
            }
        };
        v["check"] = check;
    }
    Ok(v)
}

/// MCP progress while a call waits: frames done of all.
fn progress(cx: &Ctx, s: &Status) {
    if let Some(p) = &s.progress {
        cx.call.progress(
            p.frames_done as f64,
            Some(p.frames_total as f64),
            Some(&format!("{} of {} frames", p.frames_done, p.frames_total)),
        );
    }
}

fn respond(s: &Status) -> Value {
    let mut v = json!({
        "job": s.id,
        "state": s.state.name(),
        "output": text::field(&s.output.to_string_lossy()),
    });
    let done = s.progress.as_ref().map_or(0, |p| p.frames_done);
    v["frames"] = json!({"done": done, "total": s.frames});
    match s.state {
        State::Queued => {
            v["ahead"] = json!(s.ahead);
            v["encoder"] = json!(s.encoder);
        }
        State::Running | State::Finishing => {
            if let Some(p) = &s.progress {
                v["fps"] = json!((p.fps * 10.0).round() / 10.0);
                if let Some(eta) = p.eta {
                    v["seconds_left"] = json!(eta.as_secs_f64().round());
                }
                v["encoder"] = json!(p.encoder);
                if let Some(f) = &p.fallback {
                    v["fallback"] = fallback(f);
                }
                if let Some(f) = &p.decode_fallback {
                    v["decode_fallback"] = fallback(f);
                }
                if let Some((done, all)) = p.finishing {
                    v["finishing"] = json!({"bytes_done": done, "bytes_total": all});
                }
            }
        }
        State::Done => {
            if let Some(r) = &s.report {
                v["frames"] = json!({"done": r.frames, "total": r.frames});
                v["report"] = report(r);
            }
        }
        State::Failed => {
            if let Some(e) = &s.error {
                v["error"] = e.to_json()["error"].clone();
            }
        }
        State::Cancelled => {
            v["removed"] = json!("nothing of it is left at the output");
        }
    }
    v
}

fn fallback(f: &Fallback) -> Value {
    json!({"from": f.from, "code": f.code, "reason": text::field(&f.reason)})
}

/// The finished file, as the agent needs it: what it is and what the
/// export did that the project didn't say.
fn report(r: &Report) -> Value {
    let mut v = json!({
        "file": text::field(&r.output.to_string_lossy()),
        "bytes": r.bytes,
        "frames": r.frames,
        "duration": time(r.duration),
        "size": [r.size.0, r.size.1],
        "rate": serde_json::to_value(r.rate).unwrap_or(Value::Null),
        "codec": serde_json::to_value(r.codec).unwrap_or(Value::Null),
        "ten_bit": r.ten_bit,
        "draft": r.draft,
        "encoder": r.encoder,
        "decode": r.decode.path,
        "audio": r.audio.as_ref().map(|a| json!({
            "codec": a.codec,
            "rate": a.rate,
            "channels": a.channels,
            "ambix": a.ambix,
        })),
        "seconds": (r.elapsed.as_secs_f64() * 10.0).round() / 10.0,
    });
    if let Some(f) = &r.fallback {
        v["fallback"] = fallback(f);
    }
    if let Some(f) = &r.decode.fallback {
        v["decode_fallback"] = fallback(f);
    }
    if !r.warnings.is_empty() {
        v["warnings"] = r
            .warnings
            .iter()
            .map(|w| json!({"code": w.code, "message": text::field(&w.message)}))
            .collect();
    }
    if r.blur_capped > 0 {
        v["blur_capped"] = json!(r.blur_capped);
    }
    v
}
