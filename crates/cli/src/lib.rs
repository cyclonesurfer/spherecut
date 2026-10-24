//! The `spherecut` command's library half: what other front ends share
//! with it. For now that is the JSON contract of `--json` (D27), which
//! A1's `spherecut call` and `spherecut mcp` are to reuse. A1 moves it
//! into `spherecut-agent`, or a crate below both, since the CLI will
//! depend on that crate and it can't depend back; the CLI re-exports it.

pub mod contract;
