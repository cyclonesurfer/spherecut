//! Running the `spherecut` binary under strace, for the tests that check
//! what it opens (DECISIONS D24).

// Test helpers; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

/// How to install strace, which Tier 1 needs. `cargo xtask check` says
/// the same.
pub const INSTALL_STRACE: &str = "sudo dnf install strace (Fedora), sudo pacman -S strace (Arch) or sudo apt install strace (Ubuntu)";

/// A command that runs what is appended to it under `strace -f`, tracing
/// `syscalls` into `log`. Without strace this fails, never skips: the test
/// would otherwise pass without having checked anything.
pub fn strace(log: &Path, syscalls: &str) -> Command {
    let found = Command::new("strace")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        found,
        "strace is not installed, and this Tier 1 test needs it: {INSTALL_STRACE}"
    );
    let mut cmd = Command::new("strace");
    // --seccomp-bpf stops the process only for the traced calls.
    cmd.args(["-f", "--seccomp-bpf", "-qq", "-e"])
        .arg(format!("trace={syscalls}"))
        .arg("-o")
        .arg(log)
        .env_remove("SPHERECUT_LOG");
    cmd
}

/// The lines of an strace log that create an IPv4 or IPv6 socket.
pub fn inet_sockets(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|l| l.contains("socket(AF_INET"))
        .collect()
}
