//! Running the `spherecut` binary under strace, for the tests that check
//! what it opens (DECISIONS D24).

// Test helpers; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

use spherecut_testkit::control::must_fail;

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

/// The lines of an strace log that reach a network: an IPv4 or IPv6
/// socket ([`inet_sockets`]), or a connection to anything but a Unix
/// socket.
fn network(log: &str) -> Result<(), Vec<&str>> {
    let mut lines = inet_sockets(log);
    lines.extend(
        log.lines()
            .filter(|l| l.contains("connect(") && !l.contains("AF_UNIX")),
    );
    if lines.is_empty() { Ok(()) } else { Err(lines) }
}

/// That the strace log `log` shows no network access (DECISIONS D24): no
/// IPv4 or IPv6 socket, and every connection to a Unix socket. It first
/// runs its own controls, a DNS lookup's socket and its connection as
/// strace prints them, which must fail it.
pub fn assert_no_network(log: &str) {
    for planted in [
        "4242 socket(AF_INET6, SOCK_DGRAM|SOCK_CLOEXEC, IPPROTO_IP) = 9",
        "4242 connect(9, {sa_family=AF_INET, sin_port=htons(53), \
         sin_addr=inet_addr(\"127.0.0.53\")}, 16) = 0",
    ] {
        let found = must_fail(&format!("the planted line `{planted}`"), network(planted));
        assert_eq!(found, [planted]);
    }
    if let Err(lines) = network(log) {
        panic!("the trace shows network access (D24):\n{lines:#?}");
    }
}
