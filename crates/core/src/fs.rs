//! Writing a file whole or not at all (DECISIONS D16).
//!
//! A [`TempFile`] is a new file next to the output, which is written and
//! then published under the output's name, so that the output is always
//! either absent, the old file or the whole new one. It is removed if it
//! is dropped unpublished, on an error or a panic. It is published one of
//! two ways:
//!
//! * [`TempFile::persist_replace`] renames it onto the output, replacing a
//!   regular file there: `inject`, `Mp4Writer`, and `spherecut frame
//!   --overwrite`.
//! * [`TempFile::persist_new`] publishes it only if nothing is there, with
//!   `link(2)`, which fails with `EEXIST` if anything has turned up at the
//!   output since it was checked, so an existing output is never replaced:
//!   `spherecut frame` without `--overwrite`. On a filesystem without hard
//!   links (FAT, some network filesystems), it creates the output with
//!   `O_CREAT | O_EXCL` instead and copies the file into it.
//!
//! A temporary file holds an exclusive `flock` for its life, so that
//! [`sweep_temp_files`] can tell one a killed writer left (whose lock the
//! kernel let go) from one that is still being written: it removes only
//! those whose lock it can take and that are more than a minute old.

use std::fs::{File, OpenOptions, Permissions, TryLockError};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustix::fs::OFlags;

use crate::blocking::may_block;
use crate::brand::APP_SLUG;

/// How many names [`TempFile::create`] tries before giving up.
pub const TEMP_ATTEMPTS: usize = 16;

/// How long ago a temporary file must last have been written for
/// [`sweep_temp_files`] to take it, besides its lock being free: far
/// longer than the moment between a file's creation and its lock.
pub const SWEEP_AGE: Duration = Duration::from_secs(60);

/// A name for a temporary file: hidden, recognisably ours, the same short
/// length whatever the output is called, and random, so that a file left
/// by a killed run (whose pid may well come round again) is not in the way.
fn temp_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    // RandomState is seeded from the OS's random source.
    let r = RandomState::new().hash_one((std::process::id(), n));
    format!(".{APP_SLUG}-{r:016x}.tmp")
}

/// Whether `name` is a temporary file's name, as [`TempFile::create`]
/// makes them: for tests that check none is left behind.
pub fn is_temp_name(name: &str) -> bool {
    name.strip_prefix(&format!(".{APP_SLUG}-"))
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The file that writing to `path` would write: `path` itself, or the file
/// a symlink there points to, through any chain of links. A dangling link
/// gives the file that writing through it would create.
pub fn follow_links(path: &Path) -> io::Result<PathBuf> {
    may_block("reading symbolic links");
    let mut p = path.to_path_buf();
    // The kernel's own limit for one lookup.
    for _ in 0..40 {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&p)?;
                // Relative targets are relative to the link's directory.
                p = match p.parent() {
                    Some(dir) => dir.join(target),
                    None => target,
                };
            }
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(p),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

/// What is at `dest`, if it is a regular file that a rename may replace;
/// `None` if nothing is. Anything else is refused: a rename over a
/// directory fails, but only once all the work is done, and one over a
/// pipe, device or socket (`/dev/null`, say) replaces the node instead of
/// writing to it. Links are followed.
pub fn replaceable(dest: &Path) -> io::Result<Option<std::fs::Metadata>> {
    may_block("reading a file's metadata");
    match std::fs::metadata(dest) {
        Ok(m) if m.is_dir() => Err(io::ErrorKind::IsADirectory.into()),
        Ok(m) if !m.is_file() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it is not a regular file (a pipe, device or socket, say), so it would be \
             replaced rather than written",
        )),
        Ok(m) => Ok(Some(m)),
        // Missing; any other error shows when the file is created or renamed.
        Err(_) => Ok(None),
    }
}

/// A new file next to `dest`, published onto it by
/// [`TempFile::persist_replace`] or [`TempFile::persist_new`], and removed
/// if dropped before that (on an error or a panic). `dest` itself is
/// written, so follow links first ([`follow_links`]) to write through
/// them; it must be a regular file or nothing.
#[derive(Debug)]
pub struct TempFile {
    path: PathBuf,
    dest: PathBuf,
    file: File,
    persisted: bool,
}

impl TempFile {
    /// A temporary file for `dest`, in its directory. When `dest` is a
    /// regular file it would replace, the new file has its permission
    /// bits (not its owner, ACLs or extended attributes), and until they
    /// are set it is private, so a private output is never readable while
    /// it is written.
    pub fn create(dest: &Path) -> io::Result<Self> {
        Self::create_named(dest, temp_name)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dest(&self) -> &Path {
        &self.dest
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// [`TempFile::create`] with names from `name`. A name that is taken
    /// (by another writer, or left over from a killed one) is skipped for
    /// the next, since `create_new` never opens an existing file.
    fn create_named(dest: &Path, mut name: impl FnMut() -> String) -> io::Result<Self> {
        may_block("creating a file");
        if dest.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output path has no file name",
            ));
        }
        let keep = replaceable(dest)?.map(|m| m.permissions().mode() & 0o777);
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create_new(true);
        if keep.is_some() {
            opts.mode(0o600);
        }
        for _ in 0..TEMP_ATTEMPTS {
            let path = dest.with_file_name(name());
            match opts.open(&path) {
                Ok(file) => {
                    // A sweep that opened it first holds the lock for a
                    // moment, and this waits that out. Where there are no
                    // locks (some network filesystems) a sweep can't take
                    // one either, so it leaves the file alone.
                    if let Err(e) = file.lock() {
                        tracing::debug!("no lock on {}: {e}", path.display());
                    }
                    let tmp = TempFile {
                        path,
                        dest: dest.to_path_buf(),
                        file,
                        persisted: false,
                    };
                    if let Some(mode) = keep {
                        tmp.file.set_permissions(Permissions::from_mode(mode))?;
                    }
                    return Ok(tmp);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("no free temporary file name in {TEMP_ATTEMPTS} tries"),
        ))
    }

    /// Flush to disk and rename onto `dest`, replacing the regular file
    /// there, if any (D16).
    pub fn persist_replace(mut self) -> io::Result<()> {
        may_block("writing a file to disk");
        self.file.sync_all()?;
        // Something may have turned up at `dest` while the file was written.
        replaceable(&self.dest)?;
        std::fs::rename(&self.path, &self.dest)?;
        self.persisted = true;
        Ok(())
    }

    /// Flush to disk and publish as `dest` only if nothing is there, not
    /// even a dangling symlink: an error of kind
    /// [`AlreadyExists`](io::ErrorKind::AlreadyExists) otherwise, which
    /// leaves what is there alone, whenever it turned up. The temp file is
    /// gone either way.
    pub fn persist_new(self) -> io::Result<()> {
        self.persist_new_with(|from, to| std::fs::hard_link(from, to))
    }

    /// [`TempFile::persist_new`], publishing with `link`, which tests
    /// make refuse as a filesystem without hard links does.
    fn persist_new_with(
        mut self,
        link: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        may_block("writing a file to disk");
        self.file.sync_all()?;
        match link(&self.path, &self.dest) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
            // No hard links here (EPERM on FAT, EOPNOTSUPP or ENOSYS on
            // some network filesystems), or another refusal, which the
            // exclusive create then meets too and reports.
            Err(e) => {
                tracing::debug!(
                    "no hard link to {} ({e}); copying instead",
                    self.dest.display()
                );
                self.copy_to_new()?;
            }
        }
        // `dest` holds the file now, under its own name too; the temp
        // name goes, as on a drop.
        self.persisted = true;
        if let Err(e) = std::fs::remove_file(&self.path) {
            tracing::warn!("could not remove {}: {e}", self.path.display());
        }
        Ok(())
    }

    /// Create `dest` exclusively, copy the file into it and flush it,
    /// removing it again if the copy fails: what [`persist_new`]
    /// does without hard links.
    ///
    /// [`persist_new`]: TempFile::persist_new
    fn copy_to_new(&mut self) -> io::Result<()> {
        let mode = self.file.metadata()?.permissions().mode() & 0o777;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&self.dest)?;
        let copied = (|| {
            self.file.seek(SeekFrom::Start(0))?;
            io::copy(&mut self.file, &mut out)?;
            out.sync_all()
        })();
        if let Err(e) = copied {
            // The exclusive create made it ours, so it is ours to remove.
            drop(out);
            if let Err(r) = std::fs::remove_file(&self.dest) {
                tracing::warn!("could not remove {}: {r}", self.dest.display());
            }
            return Err(e);
        }
        Ok(())
    }
}

/// Remove the temporary files that killed writers left in `dir` (D16):
/// those named as [`TempFile::create`] names them whose lock can be
/// taken, so that nothing holds them open any more, and that were last
/// written more than `older_than` ago ([`SWEEP_AGE`]). Anything else is
/// left: a file being written, another program's, a link, a pipe. The
/// paths removed; a file that can't be looked at is skipped.
pub fn sweep_temp_files(dir: &Path, older_than: Duration) -> io::Result<Vec<PathBuf>> {
    may_block("sweeping temporary files");
    let mut removed = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        if !entry.file_name().to_str().is_some_and(is_temp_name) {
            continue;
        }
        let path = entry.path();
        match sweep(&path, older_than) {
            Ok(true) => removed.push(path),
            Ok(false) => {}
            Err(e) => tracing::debug!("not sweeping {}: {e}", path.display()),
        }
    }
    Ok(removed)
}

/// Remove `path` if it is a regular file whose lock is free and that is
/// older than `older_than`: whether it was removed.
fn sweep(path: &Path, older_than: Duration) -> io::Result<bool> {
    // Never through a link, nor waiting on a pipe given a temp name.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Ok(false);
    }
    match file.try_lock() {
        Ok(()) => {}
        // Its writer is alive.
        Err(TryLockError::WouldBlock) => return Ok(false),
        Err(TryLockError::Error(e)) => return Err(e),
    }
    // A time to come (another machine's clock) counts as now.
    let age = meta.modified()?.elapsed().unwrap_or(Duration::ZERO);
    if age <= older_than {
        return Ok(false);
    }
    // The name still names the file locked.
    let now = std::fs::symlink_metadata(path)?;
    if (now.dev(), now.ino()) != (meta.dev(), meta.ino()) {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

/// Bytes free to an unprivileged writer on the filesystem holding `path`
/// (`statvfs`'s available blocks).
pub fn free_space(path: &Path) -> io::Result<u64> {
    may_block("reading a filesystem's free space");
    let s = rustix::fs::statvfs(path)?;
    Ok(s.f_bavail.saturating_mul(s.f_frsize))
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.persisted
            && let Err(e) = std::fs::remove_file(&self.path)
        {
            tracing::warn!("could not remove {}: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    use super::*;

    /// A test's own directory in the system's temp dir, removed with what
    /// it holds when dropped, so also when the test fails.
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Writing a file can block, so the UI thread may not (M1 plan
    /// section 8.6): marked as the UI thread, a thread that makes a
    /// temporary file panics before anything is created.
    #[test]
    fn the_ui_thread_may_not_write_files() {
        let dir = tmpdir("ui-thread");
        let dest = dir.join("out.png");
        let e = std::thread::spawn(move || {
            crate::blocking::mark_ui_thread_with(crate::blocking::OnBlock::Panic);
            TempFile::create(&dest)
        })
        .join()
        .unwrap_err();
        let message = e.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(
            message.starts_with("creating a file would block the UI thread"),
            "{message}"
        );
        assert_eq!(listing(&dir), Vec::<String>::new());
        // Unmarked, the same call goes ahead.
        TempFile::create(&dir.join("out.png")).unwrap();
    }

    fn tmpdir(name: &str) -> TempDir {
        let path = std::env::temp_dir().join(format!("spherecut-fs-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    /// File names in `dir`, sorted; temp files show up here.
    fn listing(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    impl TempFile {
        /// The same file, published at `dest` instead: as when something
        /// turns up at an output after its temp file was made.
        fn aimed_at(mut self, dest: &Path) -> TempFile {
            self.dest = dest.to_path_buf();
            self
        }
    }

    fn written(dest: &Path, bytes: &[u8]) -> TempFile {
        let mut t = TempFile::create(dest).unwrap();
        t.file_mut().write_all(bytes).unwrap();
        t
    }

    /// Make a unix socket at `p`; it stays when its listener is closed.
    fn socket(p: &Path) {
        std::os::unix::net::UnixListener::bind(p).map(drop).unwrap();
    }

    #[test]
    fn temp_names_are_short_and_fresh() {
        let dir = tmpdir("temp-names");
        // The longest legal name: the temp name must not grow with it.
        let dest = dir.join("o".repeat(251) + ".mp4");
        let a = TempFile::create(&dest).unwrap();
        let b = TempFile::create(&dest).unwrap();
        assert_ne!(a.path, b.path);
        for t in [&a, &b] {
            assert_eq!(t.path.parent(), dest.parent());
            let name = t.path.file_name().unwrap().to_str().unwrap();
            assert!(
                name.starts_with(".spherecut-") && name.ends_with(".tmp") && name.len() == 31,
                "{name}"
            );
            assert!(is_temp_name(name), "{name}");
        }
        assert!(!is_temp_name(".spherecut-0123.tmp"));
        assert!(!is_temp_name("out.png"));
        drop((a, b));
        assert!(listing(&dir).is_empty());
    }

    #[test]
    fn taken_temp_names_are_skipped() {
        // A killed run leaves its temp file behind, and a later one may
        // come up with the same name (pid-based names used to, every time).
        let dir = tmpdir("temp-taken");
        let dest = dir.join("out.mp4");
        for n in [".left-0.tmp", ".left-1.tmp"] {
            std::fs::write(dir.join(n), b"leftover").unwrap();
        }
        let mut names = [".left-0.tmp", ".left-1.tmp", ".fresh.tmp"].into_iter();
        let t = TempFile::create_named(&dest, || names.next().unwrap().into()).unwrap();
        assert_eq!(t.path, dir.join(".fresh.tmp"));
        t.persist_replace().unwrap();
        assert_eq!(std::fs::read(dir.join(".left-0.tmp")).unwrap(), b"leftover");
        assert_eq!(listing(&dir), [".left-0.tmp", ".left-1.tmp", "out.mp4"]);

        // It gives up after a bounded number of tries.
        let mut tries = 0;
        let r = TempFile::create_named(&dest, || {
            tries += 1;
            ".left-0.tmp".into()
        });
        assert_eq!(
            r.err().map(|e| e.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(tries, TEMP_ATTEMPTS);
        assert_eq!(listing(&dir), [".left-0.tmp", ".left-1.tmp", "out.mp4"]);
    }

    #[test]
    fn persist_replace_replaces_and_keeps_permissions() {
        let dir = tmpdir("replace");
        let out = dir.join("out.png");
        for m in [0o600, 0o640, 0o755, 0o444] {
            std::fs::remove_file(&out).ok();
            std::fs::write(&out, b"old").unwrap();
            std::fs::set_permissions(&out, Permissions::from_mode(m)).unwrap();
            // The temp file has them from the start.
            let tmp = written(&out, b"new");
            assert_eq!(mode(tmp.path()), m);
            tmp.persist_replace().unwrap();
            assert_eq!(mode(&out), m, "{m:o}");
            assert_eq!(std::fs::read(&out).unwrap(), b"new");
        }
        assert_eq!(listing(&dir), ["out.png"]);
    }

    #[test]
    fn persist_replace_refuses_what_turns_up_that_is_not_a_file() {
        // A directory, or a socket, that turns up while the file is
        // written makes the rename fail, and the temp file goes too.
        let dir = tmpdir("late");
        let late = dir.join("late.png");
        let tmp = written(&late, b"new");
        std::fs::create_dir(&late).unwrap();
        assert_eq!(
            tmp.persist_replace().unwrap_err().kind(),
            io::ErrorKind::IsADirectory
        );
        let sock = dir.join("sock.png");
        let tmp = written(&sock, b"new");
        socket(&sock);
        let e = tmp.persist_replace().unwrap_err();
        assert!(e.to_string().contains("not a regular file"), "{e}");
        assert!(sock.metadata().unwrap().file_type().is_socket());
        assert!(late.is_dir());
        assert_eq!(listing(&dir), ["late.png", "sock.png"]);
    }

    #[test]
    fn persist_new_publishes_with_a_link() {
        let dir = tmpdir("new");
        let out = dir.join("out.png");
        let tmp = written(&out, b"picture");
        let made = mode(tmp.path());
        tmp.persist_new().unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"picture");
        assert_eq!(mode(&out), made);
        // One name left: the temp name was removed after the link.
        assert_eq!(std::fs::metadata(&out).unwrap().nlink(), 1);
        assert_eq!(listing(&dir), ["out.png"]);
    }

    #[test]
    fn persist_new_refuses_an_existing_output() {
        let dir = tmpdir("exists");
        // A file, a directory, a socket, and links to a file and to
        // nothing: none is touched, and each is AlreadyExists.
        let file = dir.join("file.png");
        std::fs::write(&file, b"old").unwrap();
        let sub = dir.join("dir.png");
        std::fs::create_dir(&sub).unwrap();
        let sock = dir.join("sock.png");
        socket(&sock);
        let link = dir.join("link.png");
        std::os::unix::fs::symlink("file.png", &link).unwrap();
        let dangling = dir.join("dangling.png");
        std::os::unix::fs::symlink("nothing.png", &dangling).unwrap();
        for out in [&file, &sub, &sock, &link, &dangling] {
            // Created before or after the temp file, it is refused.
            let tmp = TempFile::create(&dir.join("x.png")).unwrap().aimed_at(out);
            let e = tmp.persist_new().unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{}", out.display());
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"old");
        assert!(sub.is_dir());
        assert!(dangling.symlink_metadata().unwrap().is_symlink());
        assert!(!dir.join("nothing.png").exists());
        assert_eq!(
            listing(&dir),
            [
                "dangling.png",
                "dir.png",
                "file.png",
                "link.png",
                "sock.png"
            ]
        );
    }

    #[test]
    fn persist_new_loses_a_race_with_eexist() {
        // The output is checked, the file written, and meanwhile another
        // writer creates the output: the publish fails and leaves theirs.
        let dir = tmpdir("race");
        let out = dir.join("out.png");
        assert!(replaceable(&out).unwrap().is_none());
        let tmp = written(&out, b"ours");
        std::fs::write(&out, b"theirs").unwrap();
        let e = tmp.persist_new().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&out).unwrap(), b"theirs");
        assert_eq!(listing(&dir), ["out.png"]);
    }

    #[test]
    fn persist_new_copies_where_hard_links_are_refused() {
        let dir = tmpdir("no-links");
        let out = dir.join("out.png");
        let refuse = |_: &Path, _: &Path| -> io::Result<()> {
            // What link(2) says on FAT.
            Err(io::Error::from_raw_os_error(1))
        };
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let tmp = written(&out, &big);
        let made = mode(tmp.path());
        tmp.persist_new_with(refuse).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), big);
        assert_eq!(mode(&out), made);
        assert_eq!(listing(&dir), ["out.png"]);

        // The exclusive create still refuses an output that turned up.
        let tmp = written(&out, b"ours");
        let e = tmp.persist_new_with(refuse).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&out).unwrap(), big);
        assert_eq!(listing(&dir), ["out.png"]);

        // And a refusal the create meets too is reported, leaving nothing.
        let nowhere = dir.join("no-such-dir").join("out.png");
        let tmp = written(&out, b"ours").aimed_at(&nowhere);
        let e = tmp.persist_new_with(refuse).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert_eq!(listing(&dir), ["out.png"]);
    }

    #[test]
    fn dropped_temp_files_are_removed() {
        let dir = tmpdir("drop");
        let tmp = written(&dir.join("out.png"), b"half");
        drop(tmp);
        let r = std::panic::catch_unwind(|| {
            let _tmp = written(&dir.join("out.png"), b"half");
            panic!("mid-write");
        });
        assert!(r.is_err());
        assert!(listing(&dir).is_empty());
    }

    /// Make `p`'s last write `secs` seconds ago.
    fn age(p: &Path, secs: u64) {
        let f = OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn a_sweep_takes_only_old_temp_files_that_nothing_holds() {
        use rustix::fs::{CWD, FileType, Mode, mknodat};
        let dir = tmpdir("sweep");
        // A writer's file, locked, however old.
        let live = written(&dir.join("live.mp4"), b"being written");
        age(live.path(), 600);
        // What a killed writer left: our name, and no lock.
        let left = dir.join(temp_name());
        std::fs::write(&left, b"left").unwrap();
        age(&left, 600);
        // Left a moment ago: its writer may be about to take the lock.
        let fresh = dir.join(temp_name());
        std::fs::write(&fresh, b"new").unwrap();
        // Not ours, though old.
        let other = dir.join(".other.tmp");
        std::fs::write(&other, b"theirs").unwrap();
        age(&other, 600);
        // A link and a pipe given our names: never opened through, nor
        // waited on.
        let target = dir.join("target.mp4");
        std::fs::write(&target, b"target").unwrap();
        age(&target, 600);
        let link = dir.join(temp_name());
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let pipe = dir.join(temp_name());
        mknodat(CWD, &pipe, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).unwrap();

        let gone = sweep_temp_files(&dir, SWEEP_AGE).unwrap();
        assert_eq!(gone, std::slice::from_ref(&left));
        assert!(left.symlink_metadata().is_err());
        for p in [live.path(), &fresh, &other, &target, &link, &pipe] {
            assert!(p.symlink_metadata().is_ok(), "{} was swept", p.display());
        }
        // The writer still publishes its file.
        live.persist_new().unwrap();
        assert_eq!(
            std::fs::read(dir.join("live.mp4")).unwrap(),
            b"being written"
        );
        // A second sweep finds nothing more to take.
        assert_eq!(
            sweep_temp_files(&dir, SWEEP_AGE).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn free_space_is_the_filesystems() {
        let dir = tmpdir("free");
        assert!(free_space(&dir).unwrap() > 0);
        let e = free_space(&dir.join("no-such-dir")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn links_are_followed_to_the_file_they_name() {
        use std::os::unix::fs::symlink;
        let dir = tmpdir("links");
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("target.png"), b"").unwrap();
        symlink("sub/hop.png", dir.join("link.png")).unwrap();
        symlink("target.png", sub.join("hop.png")).unwrap();
        symlink("sub/new.png", dir.join("dangling.png")).unwrap();
        assert_eq!(
            follow_links(&dir.join("link.png")).unwrap(),
            dir.join("sub").join("target.png")
        );
        assert_eq!(
            follow_links(&dir.join("dangling.png")).unwrap(),
            dir.join("sub/new.png")
        );
        let plain = dir.join("plain.png");
        assert_eq!(follow_links(&plain).unwrap(), plain);
        symlink("loop-b", dir.join("loop-a")).unwrap();
        symlink("loop-a", dir.join("loop-b")).unwrap();
        let e = follow_links(&dir.join("loop-a")).unwrap_err();
        assert!(e.to_string().contains("symbolic links"), "{e}");
    }
}
