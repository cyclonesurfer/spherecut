//! A project's folder across processes (a2-plan 9.1): the lock one
//! process holds keeps another from writing, and a file another process
//! rewrote between a load and a save isn't saved over.
//!
//! The other process is this test binary run again with
//! `SPHERECUT_FOLDER_CHILD` saying which part to play.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

use spherecut_core::project::{By, FolderError, History, Holder, Origin, ProjectFolder};

const CHILD: &str = "SPHERECUT_FOLDER_CHILD";
const NAME: &str = "a.spherecut";

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "spherecut-folder-{tag}-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// This binary again, running only `test`, as the child `part`.
fn child(test: &str, part: &str, dir: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, part)
        .env("SPHERECUT_FOLDER_DIR", dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

/// What the child does, when this process is one; true if it was.
fn play_child() -> bool {
    let Ok(part) = std::env::var(CHILD) else {
        return false;
    };
    let dir = PathBuf::from(std::env::var_os("SPHERECUT_FOLDER_DIR").unwrap());
    match part.as_str() {
        // Hold the GUI's lock until the parent closes stdin.
        "hold" => {
            let folder = ProjectFolder::open(&dir).unwrap();
            let _lock = folder
                .lock(OsStr::new(NAME), By::Gui, Duration::ZERO)
                .unwrap();
            println!("locked");
            std::io::stdout().flush().unwrap();
            let mut rest = Vec::new();
            std::io::stdin().read_to_end(&mut rest).unwrap();
        }
        // Rewrite the project as a text editor would, in place.
        "rewrite" => {
            let path = dir.join(NAME);
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, text.replacen("VID_1", "VID_9", 1)).unwrap();
            println!("rewritten");
        }
        other => panic!("no part {other}"),
    }
    true
}

/// The child's stdout, which the test harness writes to as well.
fn output(child: &mut Child) -> BufReader<ChildStdout> {
    BufReader::new(child.stdout.take().unwrap())
}

/// Wait for the child to say `word` at the end of a line (the harness
/// writes the test's name before it).
fn said(out: &mut BufReader<ChildStdout>, word: &str) {
    let mut line = String::new();
    loop {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "no {word:?}");
        if line.trim_end().ends_with(word) {
            return;
        }
    }
}

/// Read the rest, so that the child's harness never writes to a closed
/// pipe, and wait for it to pass.
fn finish(mut child: Child, mut out: BufReader<ChildStdout>) {
    let mut rest = Vec::new();
    out.read_to_end(&mut rest).unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn another_process_holding_the_lock_keeps_writers_out() {
    if play_child() {
        return;
    }
    let dir = temp_dir("lock");
    let folder = ProjectFolder::open(&dir).unwrap();
    let name = OsStr::new(NAME);
    folder
        .create(name, &common::project(), &History::new())
        .unwrap();
    let mut held = child(
        "another_process_holding_the_lock_keeps_writers_out",
        "hold",
        &dir,
    );
    let mut out = output(&mut held);
    said(&mut out, "locked");
    match folder.lock(name, By::Mcp, Duration::from_millis(100)) {
        Err(FolderError::Locked { holder, .. }) => assert_eq!(
            holder,
            Some(Holder {
                by: By::Gui,
                pid: held.id()
            })
        ),
        other => panic!("{other:?}"),
    }
    drop(held.stdin.take());
    finish(held, out);
    // The holder has gone, and with it the lock; its line is left, and
    // harmless.
    let _lock = folder.lock(name, By::Mcp, Duration::ZERO).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_save_after_another_process_rewrote_the_file_is_refused() {
    if play_child() {
        return;
    }
    let dir = temp_dir("rewrite");
    let folder = ProjectFolder::open(&dir).unwrap();
    let name = OsStr::new(NAME);
    folder
        .create(name, &common::project(), &History::new())
        .unwrap();
    let loaded = folder.load(name).unwrap();
    let mut rewrite = child(
        "a_save_after_another_process_rewrote_the_file_is_refused",
        "rewrite",
        &dir,
    );
    let mut out = output(&mut rewrite);
    said(&mut out, "rewritten");
    finish(rewrite, out);
    let rewritten = std::fs::read_to_string(dir.join(NAME)).unwrap();
    let mut project = loaded.project.clone();
    let mut history = loaded.history.clone().into_history();
    let id = project.the_sequence().id.0;
    history
        .record(
            &mut project,
            "Rename",
            Origin::User,
            &[spherecut_core::project::Edit::Rename {
                id,
                name: "b".into(),
            }],
        )
        .unwrap();
    let e = folder
        .save(name, &project, &history, Some(&loaded.identity))
        .unwrap_err();
    assert!(matches!(e, FolderError::Changed { .. }), "{e}");
    assert_eq!(std::fs::read_to_string(dir.join(NAME)).unwrap(), rewritten);
    // The rewrite left the revision as it was, but the history knows the
    // bytes it was written with.
    assert!(matches!(
        folder.load(name).unwrap().history,
        spherecut_core::project::Kept::Stale { revision: 0 }
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}
