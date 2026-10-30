//! Every limit the server holds an agent to, in one place (PLAN A1,
//! a1-plan section 9). `docs/agents.md` lists them, and each has a test.

use std::time::Duration;

/// The longest message the server reads, in bytes, newline excluded. A
/// longer line is read to its end, dropped, and answered with -32600.
pub const MAX_REQUEST_BYTES: usize = 1 << 20;

/// Tool calls that run at once. Later ones wait their turn, in the order
/// they came; `ping` and `tools/list` are answered meanwhile.
pub const CALLS_AT_ONCE: usize = 2;

/// How long a tool call may take, from its request, before it stops with
/// `timed_out`: under Codex's default of 60 s.
pub const CALL_TIME: Duration = Duration::from_secs(50);

/// How long past its deadline a call may go on before the server
/// answers `timed_out` for it and gives up on its worker (amendment 20):
/// the time a call has to notice that it should stop.
pub const STUCK_GRACE: Duration = Duration::from_secs(2);

/// Workers the server gives up on before it exits with status 1, so that
/// the client starts a fresh one: past this, something below Spherecut
/// (a driver, a network file system) is wedged.
pub const MAX_ABANDONED: usize = 4;

/// Tool calls that may wait for a worker. One more gets `busy`.
pub const MAX_QUEUED: usize = 8;

/// How long the server waits for running calls to stop once stdin
/// closes, before it exits anyway (PLAN: within 2 s).
pub const EXIT_GRACE: Duration = Duration::from_millis(1500);

/// The fewest milliseconds between two progress notifications of a call.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Files in one `probe` call.
pub const FILES_PER_CALL: usize = 64;

/// The most a string read from a file may take in a result, in bytes
/// (a1-plan section 4.3). Longer ones are cut, and end in `…`.
pub const FIELD_BYTES: usize = 1024;

/// The longest error a tool returns, in characters of its JSON.
pub const ERROR_CHARS: usize = 500;
