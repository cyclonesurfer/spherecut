//! The `spherecut` command's library half: what other front ends share
//! with it. That is the JSON contract of `--json` (D27), which lives in
//! `spherecut-agent` since A1.4, so that `spherecut call`, `spherecut
//! mcp` and frame's core there use it too (the CLI depends on that crate,
//! and it can't depend back); it is re-exported here.

#![forbid(unsafe_code)]

pub use spherecut_agent::contract;
