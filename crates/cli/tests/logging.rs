//! What the CLI logs to stderr: FFmpeg's own messages stay out of normal
//! output and show when `SPHERECUT_LOG` asks for them.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// An MP4 that FFmpeg rejects with an error of its own ("moov atom not
/// found"): an `ftyp` and a `free` box, no `moov`.
fn no_moov() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spherecut-cli-log-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("no-moov.mp4");
    std::fs::write(&p, b"\0\0\0\x14ftypisom\0\0\x02\0isom\0\0\0\x08free").unwrap();
    p
}

/// stderr of `spherecut probe file`, with `SPHERECUT_LOG` set to `log`.
fn probe_stderr(file: &Path, log: Option<&str>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spherecut"));
    cmd.arg("probe").arg(file);
    match log {
        Some(v) => cmd.env("SPHERECUT_LOG", v),
        None => cmd.env_remove("SPHERECUT_LOG"),
    };
    let out = cmd.output().unwrap();
    assert!(
        !out.status.success(),
        "probe of {} succeeded",
        file.display()
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ffmpeg_lines(stderr: &str) -> Vec<&str> {
    stderr.lines().filter(|l| l.contains(" ffmpeg: ")).collect()
}

#[test]
fn ffmpeg_messages_are_quiet_by_default_and_shown_on_request() {
    let f = no_moov();

    let quiet = probe_stderr(&f, None);
    assert!(quiet.contains("error: cannot open"), "{quiet}");
    assert!(ffmpeg_lines(&quiet).is_empty(), "{quiet}");
    assert!(!quiet.contains("moov atom not found"), "{quiet}");

    for v in [
        "ffmpeg=debug",
        "ffmpeg=error",
        "debug",
        "info,spherecut_media=trace",
    ] {
        let loud = probe_stderr(&f, Some(v));
        assert!(
            ffmpeg_lines(&loud)
                .iter()
                .any(|l| l.contains("ERROR ffmpeg: ") && l.contains("moov atom not found")),
            "SPHERECUT_LOG={v}: {loud}"
        );
        assert!(loud.contains("error: cannot open"), "{loud}");
    }

    // Asking for our own debug output leaves FFmpeg's out.
    let ours = probe_stderr(&f, Some("spherecut_media=debug"));
    assert!(ours.contains("DEBUG spherecut_media: "), "{ours}");
    assert!(ffmpeg_lines(&ours).is_empty(), "{ours}");

    // A filter that doesn't parse is reported, then the default applies.
    let bad = probe_stderr(&f, Some("ffmpeg=loud"));
    assert!(
        bad.starts_with("warning: ignoring SPHERECUT_LOG=ffmpeg=loud"),
        "{bad}"
    );
    assert!(ffmpeg_lines(&bad).is_empty(), "{bad}");

    std::fs::remove_dir_all(f.parent().unwrap()).ok();
}
