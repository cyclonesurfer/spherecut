//! SIGINT for `spherecut frame` (M1 plan section 5, DECISIONS D27 and
//! D28): the binary's one FFI module, and its only `unsafe`.
//!
//! [`install`] sets a handler with `sigaction(2)` that does one thing,
//! the only kind of thing a signal handler may do here: it stores `true`
//! in a lock-free atomic, which [`interrupted`] reads. frame's main thread
//! polls it every 10 ms while a worker thread does the work, and on the
//! first Ctrl+C stops the work (`StopSource::shutdown`, which also raises
//! FFmpeg's interrupt flag), so the run ends with exit 4 and no output
//! file.
//!
//! The handler is installed with `SA_RESETHAND`: the kernel restores the
//! default action as it runs it, so a second Ctrl+C kills the process at
//! once, as it would have without the handler (a temporary file may then
//! be left behind; nothing else). `SA_RESTART` restarts the system calls
//! the signal interrupts in other threads, so that FFmpeg's reads don't
//! fail with `EINTR` and look like damage.
//!
//! probe and doctor never install it: they keep the default action,
//! which ends them at once.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the handler.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

// A lock-free atomic store is async-signal-safe; a lock or an allocation
// wouldn't be.
const _: () = assert!(cfg!(target_has_atomic = "8"));

/// The handler: it only records the signal.
extern "C" fn on_sigint(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Install the SIGINT handler, once per process (`frame` only).
pub fn install() -> std::io::Result<()> {
    // SAFETY: `sigaction` is a plain C struct, for which all zeroes is a
    // valid value (no handler, no flags, an empty mask) before the fields
    // below are set.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESETHAND | libc::SA_RESTART;
    // SAFETY: `sa_mask` is a valid sigset_t owned by `action`, which
    // sigemptyset only writes.
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `action` is fully initialised, its handler is an `extern
    // "C"` function of the signature SA_SIGINFO-less handlers take, which
    // touches only an atomic; the old action isn't asked for (null).
    if unsafe { libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Whether SIGINT has arrived since [`install`].
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}
