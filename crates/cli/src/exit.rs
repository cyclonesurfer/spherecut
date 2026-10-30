//! Leaving the process without exit handlers (a1-plan amendment 21).

/// End the process now with `status`, running no `atexit` handler and no
/// destructor. For `spherecut mcp` when a worker is still inside a call:
/// Mesa's and libva's exit handlers could wait on a lock that worker
/// holds, and the session would never end.
pub fn now(status: u8) -> ! {
    // SAFETY: `_exit` takes no pointers and touches no Rust state; it
    // ends the process without returning, which is what is wanted. The
    // protocol's writes are flushed already: the server writes each
    // message whole and unbuffered.
    unsafe { libc::_exit(i32::from(status)) }
}
