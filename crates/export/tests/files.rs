//! An export's temporary files (M2 plan section 8.6, amendment 2 item
//! 10, D16): each holds a lock while it is written, and an export sweeps
//! its folder of the ones killed writers left, whose locks are free, so
//! two exports into one folder both succeed, a frame being written beside
//! an export is left alone, and a SIGKILLed writer's file goes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use common::{Scratch, export, gpu, grid, project};
use spherecut_core::fs::{SWEEP_AGE, TempFile, is_temp_name, sweep_temp_files};
use spherecut_core::project::{Aspect, Codec, Output, Project, Range};
use spherecut_core::time::Time;
use spherecut_export::{Export, ExportJob};
use spherecut_synth::media as synth;

/// Tells this test binary, run again, to be the killed writer.
const HOLD: &str = "SPHERECUT_TEST_HOLD_TEMP_IN";

fn flat_project() -> Project {
    let src = grid("export-grid-h264-8", synth::Codec::H264, 8);
    let trim = Range {
        start: Time::ZERO,
        end: Time::from_secs(1),
    };
    let flat = Output::Flat {
        aspect: Aspect::Wide,
        short_side: 1080,
    };
    project(&src, trim, flat, Codec::H264, 0.0)
}

/// File names in `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// Make `p`'s last write `secs` seconds ago, past the sweep's age.
fn age(p: &Path, secs: u64) {
    let f = OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

#[test]
fn two_exports_into_one_folder_both_succeed() {
    let scratch = Scratch::new("two");
    let p = flat_project();
    let mut a = Export::start(ExportJob::new(
        p.clone(),
        scratch.0.join("a.mp4"),
        gpu().clone(),
    ))
    .unwrap();
    // Once a's file is being written, made to look old: only its lock
    // keeps b's sweep off it.
    while a.progress().frames_done < 1 {
        assert!(a.wait(Duration::from_millis(5)).is_none(), "a ended early");
    }
    for name in listing(&scratch.0) {
        if is_temp_name(&name) {
            age(&scratch.0.join(name), 600);
        }
    }
    let mut b = Export::start(ExportJob::new(p, scratch.0.join("b.mp4"), gpu().clone())).unwrap();
    for e in [&mut a, &mut b] {
        e.wait(Duration::from_secs(1800))
            .expect("the export ends")
            .unwrap();
    }
    assert_eq!(listing(&scratch.0), ["a.mp4", "b.mp4"]);
}

#[test]
fn a_frame_being_written_beside_an_export_survives_it() {
    let scratch = Scratch::new("frame-beside");
    // What `spherecut frame` holds while it writes a picture, as old as
    // a slow one's could be.
    let mut frame = TempFile::create(&scratch.0.join("frame.png")).unwrap();
    frame.file_mut().write_all(b"a picture").unwrap();
    age(frame.path(), 600);
    export(&flat_project(), &scratch.0.join("out.mp4"));
    assert!(frame.path().exists());
    frame.persist_new().unwrap();
    assert_eq!(listing(&scratch.0), ["frame.png", "out.mp4"]);
    assert_eq!(
        std::fs::read(scratch.0.join("frame.png")).unwrap(),
        b"a picture"
    );
}

/// The killed writer, when [`a_killed_writers_file_is_swept_by_the_next_export`]
/// runs this binary again: it makes a temporary file in the folder named
/// by [`HOLD`], says where, and waits to be killed. Run otherwise, it
/// does nothing.
#[test]
fn child_holds_a_temp_file() {
    let Some(dir) = std::env::var_os(HOLD) else {
        return;
    };
    let mut t = TempFile::create(&Path::new(&dir).join("killed.mp4")).unwrap();
    t.file_mut().write_all(b"half an export").unwrap();
    println!("holding {}", t.path().display());
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

/// A child process, killed and reaped when dropped, so that a failing
/// test leaves none waiting.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn a_killed_writers_file_is_swept_by_the_next_export() {
    let scratch = Scratch::new("killed");
    let mut child = Child(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_holds_a_temp_file", "--nocapture"])
            .env(HOLD, &scratch.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let out = BufReader::new(child.0.stdout.take().unwrap());
    let held = out
        .lines()
        .map(Result::unwrap)
        .find_map(|l| l.strip_prefix("holding ").map(PathBuf::from))
        .expect("the child holds a file");
    age(&held, 600);
    // While its writer lives, the file stays, however old.
    assert_eq!(
        sweep_temp_files(&scratch.0, SWEEP_AGE).unwrap(),
        Vec::<PathBuf>::new()
    );
    // SIGKILL: the writer's Drop never runs, and the kernel lets go of
    // its lock.
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(held.exists());
    export(&flat_project(), &scratch.0.join("next.mp4"));
    assert!(!held.exists());
    assert_eq!(listing(&scratch.0), ["next.mp4"]);
}
