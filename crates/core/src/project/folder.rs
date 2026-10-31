//! A project's folder (a2-plan section 3, D57): the project file, the
//! undo history beside it and its lock, each opened through the folder's
//! descriptor.
//!
//! The GUI opens the folder by its path; `spherecut mcp` gets it from its
//! roots, beneath the write root (D45). From there the code is the same:
//! the three files are opened with `openat(O_NOFOLLOW)`, so they are what
//! is in that folder and never what a link there points to, and written
//! through `/proc/self/fd/N`, so a write lands in the folder that was
//! opened whatever has happened to its path since. The descriptor is for
//! I/O only: media paths are made relative to, and read against, the
//! folder's path as it was named, lexically (D47), so the GUI and an agent
//! read a project's paths alike.
//!
//! ```text
//! trip.spherecut            the project (D47)
//! .trip.spherecut.history   its undo history, written after each save
//! .trip.spherecut.lock      flock'd by whoever writes the project
//! ```
//!
//! A history is kept only while it matches its project: the revision and
//! a hash of the file's bytes it was written with must be the project's.
//! Any other writer (a text editor, `git checkout`, an older build) makes
//! it stale, and a stale history is never applied (a2-plan 3.3).

use std::ffi::{OsStr, OsString};
use std::fs::{File, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::Project;
use super::file::{self, FileError, MAX_FILE_BYTES, fnv1a64};
use super::history::History;
use crate::blocking::may_block;
use crate::fs::TempFile;

/// The `format` a history file names.
pub const HISTORY_FORMAT: &str = concat!(crate::brand::slug!(), ".history");
/// The history schema this build writes, and the newest it reads.
pub const HISTORY_SCHEMA: u64 = 1;
/// The most a history file may take; the oldest steps go first to fit.
pub const MAX_HISTORY_BYTES: usize = 16 << 20;
/// How often a lock held by another call is tried again.
pub const LOCK_POLL: Duration = Duration::from_millis(20);

/// What failed, with the file it was about.
#[derive(Debug, Error)]
pub enum FolderError {
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A symbolic link where the project, its history or its lock goes:
    /// a rename would replace the link and leave its target as it was.
    #[error("{} is a symbolic link; give the path of the file it points to", .path.display())]
    Link { path: PathBuf },
    #[error("{} is not a regular file", .path.display())]
    NotAFile { path: PathBuf },
    /// The project file itself: not there, not a project, invalid.
    #[error(transparent)]
    File(#[from] FileError),
    /// A new project's file name is taken.
    #[error("{} is there already; nothing was replaced", .path.display())]
    Exists { path: PathBuf },
    /// Another writer holds the project's lock.
    #[error("{} is being written by {}", .path.display(), .holder.as_ref().map_or("another program".to_owned(), Holder::to_string))]
    Locked {
        path: PathBuf,
        holder: Option<Holder>,
    },
    /// The file isn't what was loaded: someone else wrote it since.
    #[error("{} changed on disk since it was loaded; nothing was saved", .path.display())]
    Changed { path: PathBuf },
}

/// Who holds a project's lock, as the holder wrote it in the lock file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holder {
    pub by: By,
    pub pid: u32,
}

/// Which of Spherecut's writers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    /// The GUI, which holds the lock while the project is open.
    Gui,
    /// `spherecut mcp`, for one call.
    Mcp,
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.by {
            By::Gui => write!(f, "Spherecut's window (pid {})", self.pid),
            By::Mcp => write!(f, "an agent's call (pid {})", self.pid),
        }
    }
}

/// What a project file was when it was loaded or saved: changed if any of
/// these differ (a2-plan 3.4). The hash catches a rewrite in place within
/// one tick of the clock that leaves the size alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    /// FNV-1a 64 of the file's bytes.
    hash: u64,
}

impl Identity {
    fn of(meta: &std::fs::Metadata, bytes: &[u8]) -> Identity {
        Identity {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            hash: fnv1a64(bytes),
        }
    }
}

/// The history found beside a project.
#[derive(Debug, Clone, PartialEq)]
pub enum Kept {
    /// No history file: a project from before A2, or one whose history
    /// was removed. An empty history, not a stale one.
    Missing,
    Kept(History),
    /// Written with the project at another revision, or with other bytes:
    /// something else wrote the project since. `revision` is the one the
    /// history was written with.
    Stale {
        revision: u64,
    },
    /// There, but not a history this build reads.
    Unreadable(String),
}

impl Kept {
    /// The history to go on with: the kept one, or a fresh one.
    pub fn into_history(self) -> History {
        match self {
            Kept::Kept(h) => h,
            _ => History::new(),
        }
    }
}

/// A project read from its folder.
#[derive(Debug)]
pub struct Loaded {
    pub project: Project,
    pub identity: Identity,
    pub history: Kept,
}

/// A save's outcome beyond the project file itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub identity: Identity,
    /// Oldest steps dropped to keep the history under
    /// [`MAX_HISTORY_BYTES`].
    pub dropped: usize,
    /// Why the history couldn't be put in place, if it couldn't (its
    /// temporary file is written before the project is, so this is only
    /// the last rename failing): the project is saved, and its history
    /// reads as stale next time.
    pub history_error: Option<String>,
}

#[derive(Serialize)]
struct HistoryOut<'a> {
    format: &'static str,
    schema: u64,
    project_revision: u64,
    project_hash: String,
    history: &'a History,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryIn {
    format: String,
    schema: u64,
    project_revision: u64,
    project_hash: String,
    history: History,
}

/// The folder a project is in.
#[derive(Debug)]
pub struct ProjectFolder {
    /// `O_PATH | O_DIRECTORY`.
    fd: OwnedFd,
    /// Its path as named, absolute and lexical (D47): what media paths
    /// are made relative to and read against.
    path: PathBuf,
    /// `/proc/self/fd/N`, what writes go through.
    proc: PathBuf,
}

/// A project's lock, held until dropped.
#[derive(Debug)]
pub struct ProjectLock {
    _file: File,
}

impl ProjectFolder {
    /// The folder at `path`, opened by its path (the GUI's way).
    pub fn open(path: &Path) -> Result<ProjectFolder, FolderError> {
        may_block("opening a project's folder");
        let named = std::path::absolute(path).map_err(|source| FolderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let fd = rustix::fs::open(
            path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| FolderError::Io {
            path: path.to_path_buf(),
            source: e.into(),
        })?;
        Ok(ProjectFolder::from_fd(fd, &named))
    }

    /// A folder opened already (`O_PATH | O_DIRECTORY`), as an agent's
    /// roots open one beneath the write root, named `path` (absolute).
    pub fn from_fd(fd: OwnedFd, path: &Path) -> ProjectFolder {
        let proc = PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()));
        ProjectFolder {
            fd,
            path: file::lexical(path),
            proc,
        }
    }

    /// The folder the project at `path` is in, and the project's name: a
    /// link at `path` is followed, so the project is written where it
    /// points and its paths are from that folder (D47), and its lock is
    /// the one an agent takes there.
    pub fn of_file(path: &Path) -> Result<(ProjectFolder, OsString), FolderError> {
        let io = |source| FolderError::Io {
            path: path.to_path_buf(),
            source,
        };
        let path = std::path::absolute(path).map_err(io)?;
        let path = file::lexical(&crate::fs::follow_links(&path).map_err(io)?);
        let name = path
            .file_name()
            .ok_or_else(|| io(io::Error::new(io::ErrorKind::InvalidInput, "no file name")))?
            .to_os_string();
        let parent = path.parent().unwrap_or(Path::new("/"));
        Ok((ProjectFolder::open(parent)?, name))
    }

    /// The folder's path as named.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `name` in the folder, as a path to show.
    pub fn path_of(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }

    /// Load the project `name` and the history beside it.
    pub fn load(&self, name: &OsStr) -> Result<Loaded, FolderError> {
        may_block("loading a project");
        self.check_name(name)?;
        let path = self.path_of(name);
        let Some((meta, bytes)) = self.read(name, MAX_FILE_BYTES)? else {
            return Err(FileError::Io {
                path,
                source: io::ErrorKind::NotFound.into(),
            }
            .into());
        };
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(FileError::TooLarge { path }.into());
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| FileError::NotProject("it isn't UTF-8 text".into()))?;
        let project = file::from_json(text, &self.path)?;
        let identity = Identity::of(&meta, &bytes);
        let history = self.read_history(name, &project, &identity);
        Ok(Loaded {
            project,
            identity,
            history,
        })
    }

    /// Take the lock for writing `name`, waiting up to `wait` for another
    /// call that holds it. The GUI's hold is never waited for: it lasts
    /// until Pat closes the project.
    pub fn lock(&self, name: &OsStr, by: By, wait: Duration) -> Result<ProjectLock, FolderError> {
        may_block("taking a project's lock");
        self.check_name(name)?;
        let lock_name = side(name, "lock");
        let path = self.path_of(&lock_name);
        let io = |source| FolderError::Io {
            path: path.clone(),
            source,
        };
        let until = Instant::now() + wait;
        let mut seen_gui = false;
        let mut file = loop {
            let fd = self
                .openat(
                    &lock_name,
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC
                        | OFlags::NONBLOCK,
                    Mode::RUSR | Mode::WUSR,
                )
                .map_err(|e| self.refusal(&lock_name, e))?;
            let mut file = File::from(fd);
            let meta = file.metadata().map_err(io)?;
            if !meta.is_file() {
                return Err(FolderError::NotAFile { path });
            }
            match file.try_lock() {
                Ok(()) => {
                    // A lock file removed (by `git clean`, say) or replaced
                    // while its holder held it: the name now names another
                    // file, which a writer after us would lock instead.
                    let now = rustix::fs::statat(&self.fd, &lock_name, AtFlags::SYMLINK_NOFOLLOW);
                    match now {
                        Ok(st) if (st.st_dev, st.st_ino) == (meta.dev(), meta.ino()) => {
                            break file;
                        }
                        _ if Instant::now() < until => continue,
                        _ => {
                            return Err(FolderError::Locked { path, holder: None });
                        }
                    }
                }
                Err(TryLockError::WouldBlock) => {
                    let holder = read_holder(&mut file);
                    let gui = holder.as_ref().is_some_and(|h| h.by == By::Gui);
                    // A reader's look at the lock ([`ProjectFolder::holder`])
                    // holds it for a moment under a line a closed window
                    // left: the GUI's hold is refused once seen twice.
                    if (gui && seen_gui) || Instant::now() >= until {
                        return Err(FolderError::Locked { path, holder });
                    }
                    seen_gui |= gui;
                    std::thread::sleep(LOCK_POLL);
                }
                Err(TryLockError::Error(e)) => return Err(io(e)),
            }
        };
        let line = serde_json::to_string(&Holder {
            by,
            pid: std::process::id(),
        })
        .map_err(|e| io(io::Error::other(e)))?;
        file.set_len(0).map_err(io)?;
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        file.write_all(format!("{line}\n").as_bytes()).map_err(io)?;
        Ok(ProjectLock { _file: file })
    }

    /// Who holds the lock for writing `name` now, without taking it:
    /// `None` when no one does, `Some(None)` when someone does without
    /// saying who. It takes a shared lock for a moment to find out, which
    /// a writer trying then sees as held and tries again.
    pub fn holder(&self, name: &OsStr) -> Result<Option<Option<Holder>>, FolderError> {
        may_block("reading a project's lock");
        self.check_name(name)?;
        let lock_name = side(name, "lock");
        let fd = match self.openat(
            &lock_name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(e) => return Err(self.refusal(&lock_name, e)),
        };
        let mut file = File::from(fd);
        match file.try_lock_shared() {
            Ok(()) => Ok(None),
            Err(TryLockError::WouldBlock) => Ok(Some(read_holder(&mut file))),
            Err(TryLockError::Error(source)) => Err(FolderError::Io {
                path: self.path_of(&lock_name),
                source,
            }),
        }
    }

    /// Save `project` as `name` and `history` beside it. With `expect`,
    /// refused if the file there isn't what was loaded or last saved
    /// ([`FolderError::Changed`]); without, a file there is replaced. The
    /// project goes first, so a history never claims a project it wasn't
    /// written with; both are written to temporary files before either is
    /// renamed into place, so a history that can't be written (a full
    /// disk) leaves the project as it was.
    pub fn save(
        &self,
        name: &OsStr,
        project: &Project,
        history: &History,
        expect: Option<&Identity>,
    ) -> Result<Saved, FolderError> {
        may_block("saving a project");
        self.check_name(name)?;
        let text = file::to_json(project, &self.path)?;
        let check = || -> Result<(), FolderError> {
            let now = self.identity(name)?;
            match expect {
                Some(e) if now.as_ref() != Some(e) => Err(FolderError::Changed {
                    path: self.path_of(name),
                }),
                _ => Ok(()),
            }
        };
        check()?;
        let (tmp, identity) = self.write_temp(name, text.as_bytes(), None)?;
        let (side, dropped) = self.history_temp(name, project, history, &identity, &tmp)?;
        check()?;
        tmp.persist_replace().map_err(|source| FolderError::Io {
            path: self.path_of(name),
            source,
        })?;
        Ok(publish_history(
            side,
            identity,
            dropped,
            &self.path_of(name),
        ))
    }

    /// Save a new project as `name`, refused if anything is there
    /// ([`FolderError::Exists`]), and `history` beside it; nothing is
    /// there if the history can't be written.
    pub fn create(
        &self,
        name: &OsStr,
        project: &Project,
        history: &History,
    ) -> Result<Saved, FolderError> {
        may_block("saving a project");
        self.check_name(name)?;
        let path = self.path_of(name);
        let text = file::to_json(project, &self.path)?;
        if self.identity(name)?.is_some() {
            return Err(FolderError::Exists { path });
        }
        let (tmp, identity) = self.write_temp(name, text.as_bytes(), None)?;
        let (side, dropped) = self.history_temp(name, project, history, &identity, &tmp)?;
        tmp.persist_new().map_err(|source| {
            if source.kind() == io::ErrorKind::AlreadyExists {
                FolderError::Exists { path: path.clone() }
            } else {
                FolderError::Io {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        // persist_new renames or links the same inode in, so the identity
        // holds.
        Ok(publish_history(side, identity, dropped, &path))
    }

    /// What is at `name` now: `None` if nothing is.
    pub fn identity(&self, name: &OsStr) -> Result<Option<Identity>, FolderError> {
        Ok(self
            .read(name, MAX_FILE_BYTES)?
            .map(|(meta, bytes)| Identity::of(&meta, &bytes)))
    }

    /// The file `name`, read whole up to `limit` bytes and one more (so a
    /// larger one shows), with its metadata; `None` if nothing is there.
    fn read(
        &self,
        name: &OsStr,
        limit: u64,
    ) -> Result<Option<(std::fs::Metadata, Vec<u8>)>, FolderError> {
        let path = self.path_of(name);
        let fd = match self.openat(
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(e) => return Err(self.refusal(name, e)),
        };
        let file = File::from(fd);
        let io = |source| FolderError::Io {
            path: path.clone(),
            source,
        };
        let meta = file.metadata().map_err(io)?;
        if !meta.is_file() {
            return Err(FolderError::NotAFile { path });
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes).map_err(io)?;
        Ok(Some((meta, bytes)))
    }

    /// A project's name is one name in the folder, short enough for its
    /// history's name beside it (`.` + name + `.history`) to fit in 255
    /// bytes.
    fn check_name(&self, name: &OsStr) -> Result<(), FolderError> {
        const ROOM: usize = 255 - ".".len() - ".history".len();
        if name.len() > ROOM {
            return Err(FolderError::Io {
                path: self.path_of(name),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "a project's name may take at most {ROOM} bytes, so that its history fits beside it"
                    ),
                ),
            });
        }
        Ok(())
    }

    fn openat(&self, name: &OsStr, flags: OFlags, mode: Mode) -> Result<OwnedFd, Errno> {
        if name.is_empty() || name.as_bytes().contains(&b'/') || name == ".." || name == "." {
            return Err(Errno::INVAL);
        }
        rustix::fs::openat(&self.fd, name, flags, mode)
    }

    fn refusal(&self, name: &OsStr, e: Errno) -> FolderError {
        let path = self.path_of(name);
        match e {
            Errno::LOOP => FolderError::Link { path },
            Errno::INVAL => FolderError::Io {
                path,
                source: io::Error::new(io::ErrorKind::InvalidInput, "not a file name in a folder"),
            },
            e => FolderError::Io {
                path,
                source: e.into(),
            },
        }
    }

    /// `bytes` written to a temporary file beside `name` and flushed to
    /// disk, so that publishing it is a rename, with the identity it will
    /// have once published.
    /// With `mode`, the file has those permission bits before anything is
    /// written to it.
    fn write_temp(
        &self,
        name: &OsStr,
        bytes: &[u8],
        mode: Option<u32>,
    ) -> Result<(TempFile, Identity), FolderError> {
        let dest = self.proc.join(name);
        let io = |source| FolderError::Io {
            path: self.path_of(name),
            source,
        };
        let mut tmp = TempFile::create(&dest).map_err(io)?;
        if let Some(mode) = mode {
            tmp.file()
                .set_permissions(std::fs::Permissions::from_mode(mode))
                .map_err(io)?;
        }
        tmp.file_mut().write_all(bytes).map_err(io)?;
        tmp.file().sync_all().map_err(io)?;
        let meta = tmp.file().metadata().map_err(io)?;
        Ok((tmp, Identity::of(&meta, bytes)))
    }

    /// The history file for `project` saved as `identity`, written beside
    /// `name` but not yet published, with the oldest steps dropped to fit
    /// (the newest steps are kept, or none), with how many were. It has
    /// the permission bits of `project_tmp`, the project's own temporary
    /// file: a private project's history holds its edits too.
    fn history_temp(
        &self,
        name: &OsStr,
        project: &Project,
        history: &History,
        identity: &Identity,
        project_tmp: &TempFile,
    ) -> Result<(TempFile, usize), FolderError> {
        let mut history = history.clone();
        // Room for the envelope around the history.
        let dropped = history.keep_bytes(MAX_HISTORY_BYTES - 256);
        let out = HistoryOut {
            format: HISTORY_FORMAT,
            schema: HISTORY_SCHEMA,
            project_revision: project.revision(),
            project_hash: format!("{:016x}", identity.hash),
            history: &history,
        };
        let side = side(name, "history");
        let io = |source| FolderError::Io {
            path: self.path_of(&side),
            source,
        };
        let text = serde_json::to_string(&out)
            .map_err(|e| io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        let mode = project_tmp
            .file()
            .metadata()
            .map_err(io)?
            .permissions()
            .mode()
            & 0o777;
        let (tmp, _) = self.write_temp(&side, text.as_bytes(), Some(mode))?;
        Ok((tmp, dropped))
    }

    fn read_history(&self, name: &OsStr, project: &Project, identity: &Identity) -> Kept {
        let side = side(name, "history");
        let (_, bytes) = match self.read(&side, MAX_HISTORY_BYTES as u64) {
            Ok(Some(found)) => found,
            Ok(None) => return Kept::Missing,
            Err(e) => return Kept::Unreadable(e.to_string()),
        };
        if bytes.len() > MAX_HISTORY_BYTES {
            return Kept::Unreadable(format!("larger than {MAX_HISTORY_BYTES} bytes"));
        }
        let h: HistoryIn = match serde_json::from_slice(&bytes) {
            Ok(h) => h,
            Err(e) => return Kept::Unreadable(e.to_string()),
        };
        if h.format != HISTORY_FORMAT {
            return Kept::Unreadable(format!("its format is {:?}", h.format));
        }
        if h.schema > HISTORY_SCHEMA {
            return Kept::Unreadable(format!(
                "written by a newer Spherecut (schema {})",
                h.schema
            ));
        }
        if h.project_revision != project.revision()
            || h.project_hash != format!("{:016x}", identity.hash)
        {
            return Kept::Stale {
                revision: h.project_revision,
            };
        }
        Kept::Kept(h.history)
    }
}

/// Publish the history written beside a project just published as
/// `identity`: after the project, so a history never claims a project it
/// wasn't written with, and only a crash between the two renames leaves
/// it stale.
fn publish_history(tmp: TempFile, identity: Identity, dropped: usize, project: &Path) -> Saved {
    let written = tmp.persist_replace().map_err(|e| e.to_string());
    if let Err(e) = &written {
        tracing::warn!("{}'s history wasn't saved: {e}", project.display());
    }
    Saved {
        identity,
        dropped,
        history_error: written.err(),
    }
}

/// `.<name>.<what>`: the history or lock file beside a project.
pub fn side(name: &OsStr, what: &str) -> OsString {
    let mut s = OsString::from(".");
    s.push(name);
    s.push(".");
    s.push(what);
    s
}

fn read_holder(file: &mut File) -> Option<Holder> {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.take(256).read_to_string(&mut text).ok()?;
    serde_json::from_str(text.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::testutil::{media, view};
    use crate::project::{Edit, Origin};

    fn temp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "spherecut-folder-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn project() -> Project {
        Project::untitled(media("/clips/VID_1.mp4", 10), view()).unwrap()
    }

    fn rename(p: &mut Project, h: &mut History, name: &str) {
        let id = p.the_sequence().id.0;
        h.record(
            p,
            "Rename",
            Origin::Agent {
                name: "test".into(),
            },
            &[Edit::Rename {
                id,
                name: name.into(),
            }],
        )
        .unwrap();
    }

    #[test]
    fn saves_and_loads_with_its_history() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let name = OsStr::new("a.spherecut");
        let (mut p, mut h) = (project(), History::new());
        let saved = folder.create(name, &p, &h).unwrap();
        assert_eq!(saved.history_error, None);
        let loaded = folder.load(name).unwrap();
        assert_eq!(loaded.project, p);
        assert_eq!(loaded.identity, saved.identity);
        assert_eq!(loaded.history, Kept::Kept(History::new()));
        rename(&mut p, &mut h, "b");
        let saved = folder.save(name, &p, &h, Some(&loaded.identity)).unwrap();
        let loaded = folder.load(name).unwrap();
        assert_eq!(loaded.identity, saved.identity);
        assert_eq!(loaded.history, Kept::Kept(h.clone()));
        // A second create of the same name is refused.
        let e = folder.create(name, &p, &h).unwrap_err();
        assert!(matches!(e, FolderError::Exists { .. }), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The history beside a private project is as private: it holds the
    /// project's edits too.
    #[test]
    fn a_private_projects_history_is_private() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let name = OsStr::new("a.spherecut");
        let (mut p, mut h) = (project(), History::new());
        let saved = folder.create(name, &p, &h).unwrap();
        let path = dir.join("a.spherecut");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        // A new history file, not one whose bits would be kept.
        std::fs::remove_file(dir.join(".a.spherecut.history")).unwrap();
        rename(&mut p, &mut h, "b");
        folder.save(name, &p, &h, Some(&saved.identity)).unwrap();
        let mode = |f: &str| std::fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("a.spherecut"), 0o600);
        assert_eq!(mode(".a.spherecut.history"), 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A history that can't be written (here a folder where it goes; on a
    /// full disk, the same) stops the save before the project is touched,
    /// so the steps beside it still match it.
    #[test]
    fn a_history_that_cant_be_written_leaves_the_project_as_it_was() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let name = OsStr::new("a.spherecut");
        let side = dir.join(".a.spherecut.history");
        let (mut p, mut h) = (project(), History::new());
        std::fs::create_dir(&side).unwrap();
        let e = folder.create(name, &p, &h).unwrap_err();
        assert!(matches!(e, FolderError::Io { .. }), "{e}");
        assert!(!dir.join("a.spherecut").exists());
        std::fs::remove_dir(&side).unwrap();

        rename(&mut p, &mut h, "b");
        let saved = folder.create(name, &p, &h).unwrap();
        let before = std::fs::read(dir.join("a.spherecut")).unwrap();
        std::fs::remove_file(&side).unwrap();
        std::fs::create_dir(&side).unwrap();
        rename(&mut p, &mut h, "c");
        let e = folder
            .save(name, &p, &h, Some(&saved.identity))
            .unwrap_err();
        assert!(matches!(e, FolderError::Io { .. }), "{e}");
        assert_eq!(std::fs::read(dir.join("a.spherecut")).unwrap(), before);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [".a.spherecut.history", "a.spherecut"],
            "no temporary file is left"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_land_in_the_folder_opened_whatever_its_path_does() {
        let dir = temp_dir();
        let sub = dir.join("p");
        std::fs::create_dir(&sub).unwrap();
        let folder = ProjectFolder::open(&sub).unwrap();
        std::fs::rename(&sub, dir.join("q")).unwrap();
        // A new folder where the old one was is not where writes go.
        std::fs::create_dir(&sub).unwrap();
        folder
            .create(OsStr::new("a.spherecut"), &project(), &History::new())
            .unwrap();
        assert!(dir.join("q/a.spherecut").exists());
        assert!(dir.join("q/.a.spherecut.history").exists());
        assert!(!sub.join("a.spherecut").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn media_paths_are_from_the_folder_as_named_not_from_proc() {
        let dir = temp_dir();
        // a/link is a symlink to real: `..` from it is a/, lexically, as
        // the GUI reads it (D47), not dir/ as the kernel would have it.
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("a/link")).unwrap();
        let folder = ProjectFolder::open(&dir.join("a/link")).unwrap();
        let mut m = media("/clips/VID_1.mp4", 10);
        m.path = dir.join("a/VID_1.mp4");
        let p = Project::untitled(m, view()).unwrap();
        let name = OsStr::new("p.spherecut");
        folder.create(name, &p, &History::new()).unwrap();
        let text = std::fs::read_to_string(dir.join("real/p.spherecut")).unwrap();
        assert!(text.contains("\"path\": \"../VID_1.mp4\""), "{text}");
        assert_eq!(folder.load(name).unwrap().project, p);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_gui_writes_through_a_link_where_it_points() {
        let dir = temp_dir();
        let real = ProjectFolder::open(&dir).unwrap();
        real.create(OsStr::new("real.spherecut"), &project(), &History::new())
            .unwrap();
        std::os::unix::fs::symlink(dir.join("real.spherecut"), dir.join("link.spherecut")).unwrap();
        let (folder, name) = ProjectFolder::of_file(&dir.join("link.spherecut")).unwrap();
        assert_eq!(name, "real.spherecut");
        assert_eq!(folder.path(), dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_name_too_long_for_its_history_is_refused() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let long = "x".repeat(247);
        let e = folder
            .create(OsStr::new(&long), &project(), &History::new())
            .unwrap_err();
        assert!(e.to_string().contains("at most 246 bytes"), "{e}");
        folder
            .create(OsStr::new(&long[1..]), &project(), &History::new())
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_lock_file_removed_while_held_is_not_shared() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let name = OsStr::new("a.spherecut");
        let held = folder.lock(name, By::Mcp, Duration::ZERO).unwrap();
        // `git clean` removes it while it is held. The next writer makes a
        // new one and gets in: a lock can't outlive its file. What keeps
        // the two from saving over each other then is the identity check
        // (a2-plan 3.4); this pins that the second holds the file now at
        // the name, so a third is refused.
        std::fs::remove_file(dir.join(".a.spherecut.lock")).unwrap();
        let second = folder.lock(name, By::Mcp, Duration::ZERO).unwrap();
        assert!(matches!(
            folder.lock(name, By::Mcp, Duration::ZERO),
            Err(FolderError::Locked { .. })
        ));
        drop((held, second));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_at_the_name_is_refused() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        folder
            .create(OsStr::new("real.spherecut"), &project(), &History::new())
            .unwrap();
        std::os::unix::fs::symlink("real.spherecut", dir.join("link.spherecut")).unwrap();
        let name = OsStr::new("link.spherecut");
        assert!(matches!(folder.load(name), Err(FolderError::Link { .. })));
        let e = folder
            .save(name, &project(), &History::new(), None)
            .unwrap_err();
        assert!(matches!(e, FolderError::Link { .. }), "{e}");
        // The link and its target are as they were.
        assert!(
            std::fs::symlink_metadata(dir.join("link.spherecut"))
                .unwrap()
                .is_symlink()
        );
        // Names that aren't one name in the folder are refused.
        for bad in ["", ".", "..", "a/b"] {
            assert!(folder.load(OsStr::new(bad)).is_err(), "{bad:?}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_changed_on_disk_is_not_saved_over() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let name = OsStr::new("a.spherecut");
        folder.create(name, &project(), &History::new()).unwrap();
        let loaded = folder.load(name).unwrap();
        // An editor rewrites it in place, the same size, at once.
        let path = dir.join("a.spherecut");
        let text = std::fs::read_to_string(&path).unwrap();
        let edited = text.replacen("VID_1", "VID_2", 1);
        assert_eq!(edited.len(), text.len());
        std::fs::write(&path, &edited).unwrap();
        let (mut p, mut h) = (loaded.project.clone(), History::new());
        rename(&mut p, &mut h, "b");
        let e = folder
            .save(name, &p, &h, Some(&loaded.identity))
            .unwrap_err();
        assert!(matches!(e, FolderError::Changed { .. }), "{e}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_history_is_kept_only_with_the_project_it_was_written_with() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let name = OsStr::new("a.spherecut");
        let (mut p, mut h) = (project(), History::new());
        rename(&mut p, &mut h, "b");
        folder.create(name, &p, &h).unwrap();
        assert_eq!(folder.load(name).unwrap().history, Kept::Kept(h.clone()));
        let path = dir.join("a.spherecut");
        let text = std::fs::read_to_string(&path).unwrap();
        // A hand edit that leaves the revision alone.
        std::fs::write(&path, text.replacen("\"b\"", "\"c\"", 1)).unwrap();
        assert_eq!(
            folder.load(name).unwrap().history,
            Kept::Stale { revision: 1 }
        );
        // A project from before A2 has no history, which is no fault.
        std::fs::remove_file(dir.join(".a.spherecut.history")).unwrap();
        assert_eq!(folder.load(name).unwrap().history, Kept::Missing);
        std::fs::write(dir.join(".a.spherecut.history"), "{}").unwrap();
        assert!(matches!(
            folder.load(name).unwrap().history,
            Kept::Unreadable(_)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_lock_is_one_writers_at_a_time() {
        let dir = temp_dir();
        let folder = ProjectFolder::open(&dir).unwrap();
        let name = OsStr::new("a.spherecut");
        assert_eq!(folder.holder(name).unwrap(), None, "no lock file yet");
        let held = folder.lock(name, By::Gui, Duration::ZERO).unwrap();
        let gui = Holder {
            by: By::Gui,
            pid: std::process::id(),
        };
        assert_eq!(folder.holder(name).unwrap(), Some(Some(gui.clone())));
        // Another descriptor's flock is refused, naming the holder; the
        // GUI's hold isn't waited for.
        let started = Instant::now();
        let e = folder
            .lock(name, By::Mcp, Duration::from_secs(5))
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        match e {
            FolderError::Locked { holder, .. } => assert_eq!(
                holder,
                Some(Holder {
                    by: By::Gui,
                    pid: std::process::id()
                })
            ),
            e => panic!("{e}"),
        }
        drop(held);
        // The line the window left is no hold.
        assert_eq!(folder.holder(name).unwrap(), None);
        let _again = folder.lock(name, By::Mcp, Duration::ZERO).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_history_too_large_loses_its_oldest_steps() {
        let mut p = project();
        let mut h = History::new();
        for i in 0..10 {
            rename(&mut p, &mut h, &format!("name {i}"));
        }
        let size = |s: &crate::project::Step| serde_json::to_string(s).unwrap().len() + 1;
        let last4: usize = h.steps()[6..].iter().map(size).sum();
        let mut kept = h.clone();
        assert_eq!(kept.keep_bytes(64 + last4), 6);
        assert_eq!(kept.steps(), &h.steps()[6..]);
        assert_eq!(kept.cursor(), 4);
        // With steps undone, dropping into them drops them all.
        let mut undone = h.clone();
        for _ in 0..8 {
            undone.undo(&mut p).unwrap();
        }
        assert_eq!(undone.cursor(), 2);
        assert_eq!(undone.keep_bytes(64 + last4), 10);
        assert!(undone.steps().is_empty());
        assert_eq!(undone.cursor(), 0);
    }
}
