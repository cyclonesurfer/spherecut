//! The roots (a1-plan sections 5 and 11.1, D45): what an agent's path
//! may reach, what the kernel refuses and with which code, that what was
//! opened is what is read, and that outputs never replace anything.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use spherecut_agent::error::codes;
use spherecut_agent::limits::ERROR_CHARS;
use spherecut_agent::roots::EntryKind;
use spherecut_agent::{Roots, ToolError};

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("roots-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    fn file(&self, rel: &str, content: &[u8]) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        p
    }

    fn folder(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

/// A tree with a root `media` holding `clip.mp4` and `sub/deep.mp4`, and
/// `secret.txt` beside it, outside.
fn tree(name: &str) -> (TempDir, Roots) {
    let t = TempDir::new(name);
    t.file("media/clip.mp4", b"clip");
    t.file("media/sub/deep.mp4", b"deep");
    t.file("secret.txt", b"secret");
    let roots = Roots::new(&[t.path("media")], None).unwrap();
    (t, roots)
}

fn read(roots: &Roots, path: &Path) -> Result<Vec<u8>, ToolError> {
    let f = roots.open_file("file", path)?;
    Ok(std::fs::read(f.source().open).unwrap())
}

fn code(r: Result<impl std::fmt::Debug, ToolError>) -> &'static str {
    match r {
        Ok(v) => panic!("opened: {v:?}"),
        Err(e) => {
            assert_eq!(e.argument.as_deref(), Some("file"), "{e:?}");
            let len = e.to_json().to_string().chars().count();
            assert!(len <= ERROR_CHARS, "{len} characters: {e:?}");
            e.code
        }
    }
}

#[test]
fn paths_under_a_root_open_and_keep_the_agent_s_name() {
    let (t, roots) = tree("under");
    assert_eq!(read(&roots, Path::new("clip.mp4")).unwrap(), b"clip");
    assert_eq!(read(&roots, Path::new("./sub/deep.mp4")).unwrap(), b"deep");
    assert_eq!(
        read(&roots, &t.path("media/sub/deep.mp4")).unwrap(),
        b"deep"
    );
    // `..` that stays inside is the kernel's to resolve, and it does.
    assert_eq!(read(&roots, Path::new("sub/../clip.mp4")).unwrap(), b"clip");
    let f = roots.open_file("file", Path::new("sub/deep.mp4")).unwrap();
    assert_eq!(f.name(), Path::new("sub/deep.mp4"));
    assert_eq!(f.source().name, Path::new("sub/deep.mp4"));
    assert!(f.source().open.starts_with("/proc/self/fd/"));
    assert_eq!(f.size(), 4);
    // Close-on-exec, so that no process a library starts inherits it.
    let flags = rustix::io::fcntl_getfd(f.as_fd()).unwrap();
    assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    // Blocking again, for media's reads.
    let fl = rustix::fs::fcntl_getfl(f.as_fd()).unwrap();
    assert!(!fl.contains(rustix::fs::OFlags::NONBLOCK));
}

#[test]
fn dot_dot_and_absolute_paths_out_are_outside_the_roots() {
    let (t, roots) = tree("out");
    for p in [
        PathBuf::from("../secret.txt"),
        PathBuf::from("sub/../../secret.txt"),
        t.path("secret.txt"),
        t.path("media/../secret.txt"),
        PathBuf::from("/etc/passwd"),
        PathBuf::from("/proc/self/fd/0"),
        PathBuf::from("/proc/self/root/etc/passwd"),
    ] {
        assert_eq!(
            code(read(&roots, &p)),
            codes::OUTSIDE_ROOTS,
            "{}",
            p.display()
        );
    }
    let e = roots
        .open_file("files[1]", &t.path("secret.txt"))
        .unwrap_err();
    assert_eq!(e.argument.as_deref(), Some("files[1]"));
    let valid = e.valid.unwrap();
    assert!(
        valid.contains(&*t.path("media").to_string_lossy()),
        "{valid}"
    );
    // No text from the file system's names past the roots themselves.
    assert!(!e.message.contains("secret"), "{}", e.message);
}

#[test]
fn symlinks_out_are_refused_and_links_inside_followed() {
    let (t, roots) = tree("links");
    symlink(t.path("secret.txt"), t.path("media/absolute")).unwrap();
    symlink("../secret.txt", t.path("media/relative")).unwrap();
    symlink("/proc/self/fd/0", t.path("media/magic")).unwrap();
    symlink("../../secret.txt", t.path("media/sub/up")).unwrap();
    symlink("clip.mp4", t.path("media/inside")).unwrap();
    symlink("sub", t.path("media/subdir")).unwrap();
    symlink("nowhere", t.path("media/dangling")).unwrap();
    symlink("loop", t.path("media/loop")).unwrap();
    for name in ["absolute", "relative", "magic", "sub/up"] {
        assert_eq!(
            code(read(&roots, Path::new(name))),
            codes::OUTSIDE_ROOTS,
            "{name}"
        );
    }
    assert_eq!(read(&roots, Path::new("inside")).unwrap(), b"clip");
    assert_eq!(read(&roots, Path::new("subdir/deep.mp4")).unwrap(), b"deep");
    assert_eq!(code(read(&roots, Path::new("dangling"))), codes::NOT_FOUND);
    assert_eq!(code(read(&roots, Path::new("loop"))), codes::NOT_FOUND);
    assert_eq!(
        code(read(&roots, Path::new("missing.mp4"))),
        codes::NOT_FOUND
    );
}

#[test]
fn folders_fifos_and_paths_through_files_are_refused() {
    let (t, roots) = tree("kinds");
    assert_eq!(code(read(&roots, Path::new("sub"))), codes::NOT_A_FILE);
    assert_eq!(code(read(&roots, Path::new("."))), codes::NOT_A_FILE);
    assert_eq!(
        code(read(&roots, Path::new("clip.mp4/x"))),
        codes::NOT_A_FOLDER
    );
    assert_eq!(code(read(&roots, Path::new(""))), codes::BAD_ARGUMENT);
    // A FIFO with no writer would block an open for reading forever.
    rustix::fs::mknodat(
        rustix::fs::CWD,
        t.path("media/fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    let fifo_roots = Roots::new(&[t.path("media")], None).unwrap();
    std::thread::spawn(move || {
        let r = fifo_roots.open_file("file", Path::new("fifo")).map(drop);
        tx.send(r.map_err(|e| e.code)).ok();
    });
    let r = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("opening a FIFO blocked");
    assert_eq!(r, Err(codes::NOT_A_FILE));
    // A file that can't be read is not_found, with why.
    let locked = t.file("media/locked.mp4", b"x");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked).is_err() {
        // (root reads it anyway.)
        let e = roots
            .open_file("file", Path::new("locked.mp4"))
            .unwrap_err();
        assert_eq!(e.code, codes::NOT_FOUND);
        assert!(e.message.contains("permission"), "{}", e.message);
    }
}

#[test]
fn what_was_opened_is_what_is_read() {
    let (t, roots) = tree("swap");
    let f = roots.open_file("file", Path::new("clip.mp4")).unwrap();
    // Replaced by another file, then by a symlink out, after the open.
    let other = t.file("media/other.mp4", b"other");
    std::fs::rename(&other, t.path("media/clip.mp4")).unwrap();
    assert_eq!(std::fs::read(f.source().open).unwrap(), b"clip");
    std::fs::remove_file(t.path("media/clip.mp4")).unwrap();
    symlink(t.path("secret.txt"), t.path("media/clip.mp4")).unwrap();
    assert_eq!(std::fs::read(f.source().open).unwrap(), b"clip");
    // The next call sees the symlink, and refuses it.
    assert_eq!(
        code(read(&roots, Path::new("clip.mp4"))),
        codes::OUTSIDE_ROOTS
    );
}

#[test]
fn a_symlinked_root_matches_both_of_its_names() {
    let t = TempDir::new("linked-root");
    t.file("real/clip.mp4", b"clip");
    symlink(t.path("real"), t.path("link")).unwrap();
    let roots = Roots::new(&[t.path("link")], None).unwrap();
    assert_eq!(read(&roots, &t.path("link/clip.mp4")).unwrap(), b"clip");
    let canonical = std::fs::canonicalize(t.path("real")).unwrap();
    assert_eq!(read(&roots, &canonical.join("clip.mp4")).unwrap(), b"clip");
    assert_eq!(read(&roots, Path::new("clip.mp4")).unwrap(), b"clip");
    assert_eq!(roots.read_roots().collect::<Vec<_>>(), [t.path("link")]);
}

#[test]
fn the_first_root_takes_relative_paths_and_any_root_absolute_ones() {
    let t = TempDir::new("two-roots");
    t.file("a/one.mp4", b"one");
    t.file("b/two.mp4", b"two");
    let roots = Roots::new(&[t.path("a"), t.path("b")], None).unwrap();
    assert_eq!(read(&roots, Path::new("one.mp4")).unwrap(), b"one");
    let e = read(&roots, Path::new("two.mp4")).unwrap_err();
    assert_eq!(e.code, codes::NOT_FOUND);
    let next = e.next.clone().unwrap_or_default();
    assert!(
        next.contains("relative path") && !next.contains('/'),
        "{e:?}"
    );
    assert_eq!(read(&roots, &t.path("b/two.mp4")).unwrap(), b"two");
    let e = read(&roots, &t.path("b/three.mp4")).unwrap_err();
    assert!(e.next.is_none(), "{e:?}");
}

#[test]
fn roots_that_can_t_be_used_stop_the_server() {
    let t = TempDir::new("bad-roots");
    let file = t.file("file", b"x");
    let missing = t.path("missing");
    for (read, write) in [
        (vec![missing.clone()], None),
        (vec![file.clone()], None),
        (vec![t.0.clone()], Some(missing.as_path())),
        (vec![t.0.clone()], Some(file.as_path())),
    ] {
        let e = Roots::new(&read, write).unwrap_err().to_string();
        assert!(
            e.starts_with("--read-root") || e.starts_with("--write-root"),
            "{e}"
        );
    }
    // None given is the working directory.
    let roots = Roots::new(&[], None).unwrap();
    assert_eq!(
        roots.read_roots().collect::<Vec<_>>(),
        [std::env::current_dir().unwrap()]
    );
    assert!(!roots.writes());
}

#[test]
fn many_roots_still_fit_in_an_error() {
    let t = TempDir::new("many-roots");
    let dirs: Vec<PathBuf> = (0..40)
        .map(|i| t.folder(&format!("{}{i}", "a-long-folder-name-".repeat(3))))
        .collect();
    let roots = Roots::new(&dirs, None).unwrap();
    let e = roots
        .open_file("file", Path::new("/elsewhere"))
        .unwrap_err();
    assert_eq!(e.code, codes::OUTSIDE_ROOTS);
    assert!(e.valid.as_deref().unwrap().contains("more"), "{e:?}");
    assert!(e.to_json().to_string().chars().count() <= ERROR_CHARS);
}

#[test]
fn a_folder_lists_its_entries_by_name_without_following_them() {
    let (t, roots) = tree("list");
    t.folder("media/a-folder");
    symlink("clip.mp4", t.path("media/b-link")).unwrap();
    let odd = t.path("media").join(OsStr::from_bytes(b"caf\xe9.mp4"));
    std::fs::write(&odd, b"").unwrap();
    let l = roots.list("folder", Path::new(".")).unwrap();
    let got: Vec<(&OsStr, EntryKind)> = l.entries.iter().map(|e| (&*e.name, e.kind)).collect();
    assert_eq!(
        got,
        [
            (OsStr::new("a-folder"), EntryKind::Folder),
            (OsStr::new("b-link"), EntryKind::Link),
            (OsStr::from_bytes(b"caf\xe9.mp4"), EntryKind::File),
            (OsStr::new("clip.mp4"), EntryKind::File),
            (OsStr::new("sub"), EntryKind::Folder),
        ]
    );
    assert_eq!(l.left_out, 0);
    assert_eq!(
        roots
            .list("folder", &t.path("media/sub"))
            .unwrap()
            .entries
            .len(),
        1
    );
    let e = roots.list("folder", Path::new("clip.mp4")).unwrap_err();
    assert_eq!(e.code, codes::NOT_A_FOLDER);
    let e = roots.list("folder", Path::new("..")).unwrap_err();
    assert_eq!(e.code, codes::OUTSIDE_ROOTS);
}

#[test]
fn writing_is_off_unless_asked_for() {
    let (_t, roots) = tree("write-off");
    let e = roots.create("output", Path::new("new.jpg")).unwrap_err();
    assert_eq!(e.code, codes::WRITE_REFUSED);
    assert_eq!(e.argument.as_deref(), Some("output"));
}

#[test]
fn outputs_are_new_files_under_the_write_root_only() {
    let t = TempDir::new("write");
    t.folder("media");
    t.folder("out/sub");
    t.file("out/taken.jpg", b"keep me");
    t.file("secret.txt", b"secret");
    symlink(t.path("secret.txt"), t.path("out/link.jpg")).unwrap();
    symlink(t.path("nowhere"), t.path("out/dangling.jpg")).unwrap();
    let roots = Roots::new(&[t.path("media")], Some(&t.path("out"))).unwrap();
    assert!(roots.writes());

    let mut c = roots.create("output", Path::new("sub/new.jpg")).unwrap();
    assert_eq!(c.path(), Path::new("sub/new.jpg"));
    c.write_all(b"jpeg").unwrap();
    c.keep().unwrap();
    assert_eq!(std::fs::read(t.path("out/sub/new.jpg")).unwrap(), b"jpeg");
    let mode = std::fs::metadata(t.path("out/sub/new.jpg"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7022, 0, "{mode:o}");
    // And readable from, as a root.
    assert_eq!(
        std::fs::read(
            roots
                .open_file("file", &t.path("out/sub/new.jpg"))
                .unwrap()
                .source()
                .open
        )
        .unwrap(),
        b"jpeg"
    );

    let mut abs = roots.create("output", &t.path("out/abs.jpg")).unwrap();
    abs.write_all(b"x").unwrap();
    abs.keep().unwrap();
    assert!(t.path("out/abs.jpg").exists());

    for name in ["taken.jpg", "link.jpg", "dangling.jpg", "sub/new.jpg"] {
        let e = roots.create("output", Path::new(name)).unwrap_err();
        assert_eq!(e.code, codes::OUTPUT_EXISTS, "{name}");
    }
    assert_eq!(std::fs::read(t.path("out/taken.jpg")).unwrap(), b"keep me");
    assert_eq!(std::fs::read(t.path("secret.txt")).unwrap(), b"secret");
    assert!(!t.path("nowhere").exists());

    for p in [
        PathBuf::from("../media/x.jpg"),
        PathBuf::from("../escape.jpg"),
        t.path("media/x.jpg"),
        t.path("x.jpg"),
    ] {
        let e = roots.create("output", &p).unwrap_err();
        assert_eq!(e.code, codes::OUTSIDE_ROOTS, "{}", p.display());
    }
    for p in [".", "sub/.."] {
        let e = roots.create("output", Path::new(p)).unwrap_err();
        assert_eq!(e.code, codes::BAD_ARGUMENT, "{p}");
    }
    let e = roots.create("output", Path::new("sub")).unwrap_err();
    assert_eq!(e.code, codes::OUTPUT_EXISTS);
    let e = roots
        .create("output", Path::new("no/such/x.jpg"))
        .unwrap_err();
    assert_eq!(e.code, codes::NOT_FOUND);
    assert!(!t.path("escape.jpg").exists() && !t.path("media/x.jpg").exists());
}

#[test]
fn an_output_not_kept_is_removed() {
    let t = TempDir::new("unkept");
    t.folder("out");
    let roots = Roots::new(&[t.path("out")], Some(&t.path("out"))).unwrap();
    let mut c = roots.create("output", Path::new("half.jpg")).unwrap();
    c.write_all(b"half").unwrap();
    assert!(t.path("out/half.jpg").exists());
    drop(c);
    assert!(!t.path("out/half.jpg").exists());
    // So a retry under the same name works.
    let c = roots.create("output", Path::new("half.jpg")).unwrap();
    c.keep().unwrap();
    assert!(t.path("out/half.jpg").exists());
}

#[test]
fn a_listing_stops_at_folder_entries() {
    use spherecut_agent::limits::FOLDER_ENTRIES;
    let t = TempDir::new("huge-folder");
    let dir = t.folder("many");
    for i in 0..FOLDER_ENTRIES + 5 {
        std::fs::File::create(dir.join(format!("{i:05}.mp4"))).unwrap();
    }
    let roots = Roots::new(&[dir], None).unwrap();
    let l = roots.list("folder", Path::new(".")).unwrap();
    assert_eq!(l.entries.len(), FOLDER_ENTRIES);
    assert_eq!(l.left_out, 5);
    assert!(l.entries.windows(2).all(|w| w[0].name < w[1].name));
}
