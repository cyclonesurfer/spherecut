//! `spherecut call <tool> '<json>'`: one of the agent tools run from the
//! command line as `spherecut mcp` runs it (a1-plan section 8, amendment
//! 7), with the same schema, op, roots and limits. stdout gets D27's
//! envelope, `spherecut.call/1`, with the tool's Response as `result` and
//! its warnings again as the envelope's, or the tool's error as `error`.
//!
//! Exit statuses are D27's: 1 for a tool's error, 2 for usage (arguments
//! that aren't JSON, an unknown tool, `bad_argument`), 3 for `no_gpu`, 4
//! for SIGINT.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::rpc::CallCx;
use spherecut_agent::{ToolError, limits};
use spherecut_cli::contract::{self, Exit};

use crate::{RootArgs, emit_document, interrupt};

/// How often the main thread looks for SIGINT while the tool runs.
const POLL: Duration = Duration::from_millis(10);

/// The one document `call` prints.
#[derive(Debug, Serialize)]
struct Document {
    schema: &'static str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
    warnings: Value,
}

impl Document {
    fn failed(error: Value) -> Self {
        Document {
            schema: contract::CALL,
            ok: false,
            result: None,
            error: Some(error),
            warnings: json!([]),
        }
    }
}

pub fn run(tool: &str, args: &str, roots: &RootArgs) -> Result<Exit> {
    let (document, exit) = call(tool, args, roots);
    let mut line = serde_json::to_string(&document)?;
    line.push('\n');
    emit_document(&line)?;
    Ok(exit)
}

/// A usage error, in D27's words.
fn usage(message: String) -> (Document, Exit) {
    let error = json!({"code": contract::codes::USAGE, "message": message});
    (Document::failed(error), Exit::Usage)
}

fn call(tool: &str, args: &str, roots: &RootArgs) -> (Document, Exit) {
    let args: Value = match serde_json::from_str(args) {
        Ok(v) => v,
        Err(e) => return usage(format!("the arguments aren't JSON: {e}")),
    };
    let roots = match roots.roots() {
        Ok(roots) => roots,
        Err(e) => return usage(e.to_string()),
    };
    if let Err(e) = interrupt::install() {
        let message = format!("cannot install the SIGINT handler: {e}");
        let error = json!({"code": codes::INTERNAL, "message": message});
        return (Document::failed(error), Exit::Failed);
    }
    let cx = CallCx::standalone(Instant::now() + limits::CALL_TIME);
    let cancel = cx.canceller();
    let options = spherecut_agent::Options { roots };
    let outcome = std::thread::scope(|s| {
        let worker = std::thread::Builder::new()
            .name("call".into())
            .spawn_scoped(s, || {
                #[cfg(debug_assertions)]
                debug_wait(&cx);
                spherecut_agent::call(tool, args, options, &cx)
            });
        let worker = match worker {
            Ok(w) => w,
            Err(e) => {
                return Err(ToolError::internal(format!(
                    "cannot start the worker thread: {e}"
                )));
            }
        };
        while !worker.is_finished() {
            if interrupt::interrupted() {
                tracing::info!("interrupted: stopping");
                cancel.cancel();
            }
            std::thread::sleep(POLL);
        }
        // A panic on the worker is the run's, as on this thread: the
        // panic hook has already written the document.
        worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    });
    match outcome {
        // Whatever stopped the tool, the interrupt did.
        Err(_) if interrupt::interrupted() => {
            let e = ToolError::new(codes::CANCELLED, "interrupted: the call was cancelled");
            (
                Document::failed(e.to_json()["error"].take()),
                Exit::Cancelled,
            )
        }
        Err(e) => {
            let exit = match e.code {
                codes::BAD_ARGUMENT => Exit::Usage,
                codes::NO_GPU => Exit::Missing,
                codes::CANCELLED => Exit::Cancelled,
                _ => Exit::Failed,
            };
            let error = e.to_json()["error"].take();
            (Document::failed(error), exit)
        }
        Ok(out) => {
            // look's images, from A1.4, go to files named in the result.
            let warnings = out.result.get("warnings").cloned().unwrap_or(json!([]));
            let document = Document {
                schema: contract::CALL,
                ok: true,
                result: Some(out.result),
                error: None,
                warnings,
            };
            (document, Exit::Ok)
        }
    }
}

/// In debug builds, with `SPHERECUT_DEBUG_PANIC=call_wait`, hold the
/// call until it is stopped, so that the SIGINT test has a call to
/// interrupt. Release builds have no such switch.
#[cfg(debug_assertions)]
fn debug_wait(cx: &CallCx) {
    if std::env::var("SPHERECUT_DEBUG_PANIC").as_deref() == Ok("call_wait") {
        while cx.check().is_ok() {
            std::thread::sleep(POLL);
        }
    }
}
