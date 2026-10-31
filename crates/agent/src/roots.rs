//! Read and write roots (a1-plan section 5 and its amendments, D45).
//!
//! Every path an agent gives is opened with `openat2` beneath a root's
//! descriptor, with `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS`, so the
//! kernel keeps it inside: `..`, an absolute symlink and a link out are
//! refused there, not by a check on the string. Media then reads what was
//! opened through its descriptor's `/proc/self/fd/N`, so a file renamed,
//! replaced or swapped for a symlink after the check isn't what is read.
//! Outputs are created `O_EXCL` under the write root only, so nothing is
//! ever replaced or followed.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use spherecut_core::project::ProjectFolder;
use spherecut_media::Source;

use crate::error::{ToolError, codes};
use crate::{limits, text};

/// How often `openat2` is tried again on `EAGAIN`, which a rename racing
/// a `..` lookup gives (amendment 17).
const AGAIN_TRIES: usize = 3;

/// How much of an `outside_roots` error the list of roots may take, so
/// that the whole stays under [`limits::ERROR_CHARS`].
const ROOTS_LISTED_CHARS: usize = 250;

/// A folder an agent's paths may lead into.
#[derive(Debug)]
struct Root {
    /// `O_PATH | O_DIRECTORY`, opened at start.
    fd: OwnedFd,
    /// The path it was given, made absolute without resolving anything.
    given: PathBuf,
    /// Where its descriptor is, symlinks resolved.
    canonical: PathBuf,
}

impl Root {
    fn open(path: &Path, flag: &str) -> Result<Root, RootsError> {
        let fail = |why: String| RootsError(format!("{flag} {}: {why}", path.display()));
        let given = std::path::absolute(path).map_err(|e| fail(e.to_string()))?;
        let fd = rustix::fs::open(
            path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| {
            fail(match e {
                Errno::NOENT => "doesn't exist".into(),
                Errno::NOTDIR => "isn't a folder".into(),
                e => io::Error::from(e).to_string(),
            })
        })?;
        // The kernel's own name for what was opened, which also shows that
        // /proc is there: media reads every agent's file through it.
        let canonical = std::fs::read_link(proc_path(fd.as_fd())).map_err(|e| {
            fail(format!(
                "/proc/self/fd can't be read ({e}); Spherecut reads an agent's files through it, so it won't serve without it"
            ))
        })?;
        Ok(Root {
            fd,
            given,
            canonical,
        })
    }

    /// Both forms an absolute path may start with.
    fn forms(&self) -> [&Path; 2] {
        [&self.given, &self.canonical]
    }
}

/// A root that can't be used. The server doesn't start: the roots are the
/// user's configuration, not the agent's mistake.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RootsError(String);

/// One server's roots: the folders an agent's files are read from, and
/// the one its outputs may be written to, if writing is on. The write
/// root is read from too.
#[derive(Debug)]
pub struct Roots {
    /// Never empty: the working directory when none was given.
    read: Vec<Root>,
    write: Option<Root>,
}

/// What an `openat2` refusal means, which depends on what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    File,
    Folder,
    /// The folder an output goes in.
    OutputFolder,
}

impl Roots {
    /// Open the roots: `read`, or the working directory when it is empty,
    /// and `write`, where outputs may go. Each must be a folder now, and
    /// the kernel must have `openat2` (Linux 5.6), or the roots couldn't
    /// be enforced.
    pub fn new(read: &[PathBuf], write: Option<&Path>) -> Result<Roots, RootsError> {
        let read = if read.is_empty() {
            vec![Root::open(Path::new("."), "the working directory")?]
        } else {
            read.iter()
                .map(|p| Root::open(p, "--read-root"))
                .collect::<Result<_, _>>()?
        };
        let write = write.map(|p| Root::open(p, "--write-root")).transpose()?;
        match beneath(
            read[0].fd.as_fd(),
            Path::new("."),
            OFlags::PATH | OFlags::DIRECTORY,
        ) {
            Ok(_) => {}
            Err(Errno::NOSYS) => {
                return Err(RootsError(
                    "this kernel has no openat2 (Linux 5.6 and later have it), so the roots \
                     can't be enforced, and Spherecut won't serve without them"
                        .into(),
                ));
            }
            Err(e) => {
                return Err(RootsError(format!(
                    "openat2 failed on the first root: {}",
                    io::Error::from(e)
                )));
            }
        }
        Ok(Roots { read, write })
    }

    /// Whether outputs may be written.
    pub fn writes(&self) -> bool {
        self.write.is_some()
    }

    /// The roots, as an agent may name them: each as it was given.
    pub fn read_roots(&self) -> impl Iterator<Item = &Path> {
        self.read.iter().map(|r| r.given.as_path())
    }

    pub fn write_root(&self) -> Option<&Path> {
        self.write.as_ref().map(|r| r.given.as_path())
    }

    /// Open `path`, the argument `arg`, as a file to read: beneath its
    /// root, without following a symlink out, and without blocking on a
    /// FIFO or device, which are refused.
    pub fn open_file(&self, arg: &str, path: &Path) -> Result<Opened, ToolError> {
        let (root, rest) = self.locate_read(arg, path)?;
        let fd = beneath(
            root.fd.as_fd(),
            rest,
            OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| self.refused(arg, e, Want::File, path))?;
        let st = rustix::fs::fstat(&fd).map_err(|e| unreadable(arg, e))?;
        match FileType::from_raw_mode(st.st_mode) {
            FileType::RegularFile => {}
            FileType::Directory => {
                return Err(ToolError::new(codes::NOT_A_FILE, "is a folder, not a file").at(arg));
            }
            _ => {
                return Err(ToolError::new(
                    codes::NOT_A_FILE,
                    "isn't a regular file: it is a FIFO, a device or a socket",
                )
                .at(arg));
            }
        }
        // Reads of the file itself may block, as reads of a file do.
        let flags = rustix::fs::fcntl_getfl(&fd).map_err(|e| unreadable(arg, e))?;
        rustix::fs::fcntl_setfl(&fd, flags - OFlags::NONBLOCK).map_err(|e| unreadable(arg, e))?;
        Ok(Opened {
            proc: proc_path(fd.as_fd()),
            fd,
            name: path.to_path_buf(),
            size: u64::try_from(st.st_size).unwrap_or(0),
        })
    }

    /// List the folder `path`, the argument `arg`, opened as
    /// [`open_file`](Self::open_file) opens a file.
    pub fn list(&self, arg: &str, path: &Path) -> Result<Listing, ToolError> {
        let (root, rest) = self.locate_read(arg, path)?;
        let fd = beneath(
            root.fd.as_fd(),
            rest,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOCTTY,
        )
        .map_err(|e| self.refused(arg, e, Want::Folder, path))?;
        let mut dir = Dir::new(fd).map_err(|e| unreadable(arg, e))?;
        let mut entries = Vec::new();
        let mut left_out = 0;
        while let Some(entry) = dir.read() {
            let entry = entry.map_err(|e| unreadable(arg, e))?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if entries.len() == limits::FOLDER_ENTRIES {
                left_out += 1;
                continue;
            }
            // The entry itself, not what a link points to.
            let st = dir.fd().ok().and_then(|fd| {
                rustix::fs::statat(fd, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW).ok()
            });
            let file_type = match (entry.file_type(), &st) {
                (FileType::Unknown, Some(st)) => FileType::from_raw_mode(st.st_mode),
                (t, _) => t,
            };
            entries.push(Entry {
                name: OsStr::from_bytes(name).to_owned(),
                kind: match file_type {
                    FileType::RegularFile => EntryKind::File,
                    FileType::Directory => EntryKind::Folder,
                    FileType::Symlink => EntryKind::Link,
                    _ => EntryKind::Other,
                },
                modified: st.map(|st| st.st_mtime),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Listing { entries, left_out })
    }

    /// Create `path`, the argument `arg`, as a new file under the write
    /// root. Nothing at that name is replaced or followed, a symlink
    /// included (`output_exists`), and the file is removed again unless
    /// [`Created::keep`] is called.
    pub fn create(&self, arg: &str, path: &Path) -> Result<Created, ToolError> {
        let Some(root) = &self.write else {
            return Err(ToolError::new(codes::WRITE_REFUSED, "writing is off")
                .at(arg)
                .next(
                    "leave the output out to get the result inline, or ask the user to start \
                     Spherecut with --allow-write --write-root DIR",
                ));
        };
        let rest = if path.is_absolute() {
            match under(std::iter::once(root), path) {
                Some((_, rest)) => rest,
                None => return Err(outside(arg, "write to", std::iter::once(root))),
            }
        } else {
            path
        };
        let (Some(folder), Some(name)) = (rest.parent(), rest.file_name()) else {
            return Err(ToolError::new(codes::BAD_ARGUMENT, "names a folder, not a file").at(arg));
        };
        let folder = if folder.as_os_str().is_empty() {
            Path::new(".")
        } else {
            folder
        };
        let parent = beneath(root.fd.as_fd(), folder, OFlags::PATH | OFlags::DIRECTORY)
            .map_err(|e| self.refused(arg, e, Want::OutputFolder, path))?;
        let fd = rustix::fs::openat(
            &parent,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOCTTY,
            Mode::from_raw_mode(0o644),
        )
        .map_err(|e| match e {
            Errno::EXIST => ToolError::new(
                codes::OUTPUT_EXISTS,
                "something is at that path already, and nothing is replaced",
            )
            .at(arg)
            .next("choose a new name"),
            Errno::ISDIR => {
                ToolError::new(codes::BAD_ARGUMENT, "names a folder, not a file").at(arg)
            }
            Errno::ACCESS | Errno::PERM | Errno::ROFS => {
                ToolError::new(codes::WRITE_REFUSED, "can't be written: permission denied").at(arg)
            }
            e => ToolError::new(
                codes::WRITE_REFUSED,
                format!("can't be created: {}", io::Error::from(e)),
            )
            .at(arg),
        })?;
        Ok(Created {
            file: std::fs::File::from(fd),
            parent,
            name: name.to_owned(),
            path: path.to_path_buf(),
            kept: false,
        })
    }

    /// The folder `path`, the argument `arg`, is to be written in,
    /// opened beneath the write root, and its name there, for a file
    /// that another thread writes later (a render, a2-plan 5.3): nothing
    /// is created, and the name is refused if anything is there now, a
    /// link included (`output_exists`). The writer goes through
    /// [`Folder::exact`], so the file lands in this folder whatever
    /// happens to its path meanwhile.
    pub fn output(&self, arg: &str, path: &Path) -> Result<Folder, ToolError> {
        let Some(root) = &self.write else {
            return Err(ToolError::new(codes::WRITE_REFUSED, "writing is off")
                .at(arg)
                .next(
                    "ask the user to start Spherecut with --allow-write --write-root DIR, \
                     and render beneath DIR",
                ));
        };
        let rest = if path.is_absolute() {
            match under(std::iter::once(root), path) {
                Some((_, rest)) => rest,
                None => return Err(outside(arg, "write to", std::iter::once(root))),
            }
        } else {
            path
        };
        let name = match rest.components().next_back() {
            Some(std::path::Component::Normal(name)) => name.to_owned(),
            _ => {
                return Err(
                    ToolError::new(codes::BAD_ARGUMENT, "names a folder, not a file").at(arg),
                );
            }
        };
        let folder = rest.parent().unwrap_or(Path::new(""));
        let fd = beneath(root.fd.as_fd(), folder, OFlags::PATH | OFlags::DIRECTORY)
            .map_err(|e| self.refused(arg, e, Want::OutputFolder, path))?;
        match rustix::fs::statat(&fd, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {}
            Ok(_) => {
                return Err(ToolError::new(
                    codes::OUTPUT_EXISTS,
                    "something is at that path already, and nothing is replaced",
                )
                .at(arg)
                .next("choose a new name"));
            }
            Err(e) => {
                return Err(ToolError::new(
                    codes::WRITE_REFUSED,
                    format!("can't be written: {}", io::Error::from(e)),
                )
                .at(arg));
            }
        }
        let proc = PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()));
        Ok(Folder {
            fd,
            proc,
            name,
            path: spherecut_core::project::file::lexical(&root.given.join(rest)),
        })
    }

    /// The folder of the project at `path`, the argument `arg`, opened
    /// beneath its root, and the project's file name (a2-plan 3.1): to
    /// write, beneath the write root, where a relative path is from, as
    /// for [`create`](Self::create); to read, beneath any root, as for
    /// [`open_file`](Self::open_file). The folder is named as its root was
    /// given joined with the rest, lexically, so media paths are read the
    /// way the agent names them (D47). The project, its history and its
    /// lock are then opened by name in the folder, never through a link.
    pub fn project_folder(
        &self,
        arg: &str,
        path: &Path,
        write: bool,
    ) -> Result<(ProjectFolder, OsString), ToolError> {
        let (root, rest, want) = if write {
            let Some(root) = &self.write else {
                return Err(ToolError::new(codes::WRITE_REFUSED, "writing is off")
                    .at(arg)
                    .next(
                        "ask the user to start Spherecut with --allow-write --write-root DIR, \
                         with the project under DIR",
                    ));
            };
            let rest = if path.is_absolute() {
                match under(std::iter::once(root), path) {
                    Some((_, rest)) => rest,
                    None => return Err(outside(arg, "write to", std::iter::once(root))),
                }
            } else {
                path
            };
            (root, rest, Want::OutputFolder)
        } else {
            let (root, rest) = self.locate_read(arg, path)?;
            (root, rest, Want::File)
        };
        let name = match rest.components().next_back() {
            Some(std::path::Component::Normal(name)) => name,
            _ => {
                return Err(
                    ToolError::new(codes::BAD_ARGUMENT, "names a folder, not a file").at(arg),
                );
            }
        };
        let folder = rest.parent().unwrap_or(Path::new(""));
        let fd = beneath(root.fd.as_fd(), folder, OFlags::PATH | OFlags::DIRECTORY)
            .map_err(|e| self.refused(arg, e, want, path))?;
        let named = if folder.as_os_str().is_empty() {
            root.given.clone()
        } else {
            root.given.join(folder)
        };
        Ok((ProjectFolder::from_fd(fd, &named), name.to_owned()))
    }

    /// `path`, the argument `arg`, as the agent names it: its root's
    /// given path joined with the rest, lexically, as a project records a
    /// media file (D47). Nothing is opened: the caller opens it with
    /// [`open_file`](Self::open_file).
    pub fn named(&self, arg: &str, path: &Path) -> Result<PathBuf, ToolError> {
        let (root, rest) = self.locate_read(arg, path)?;
        Ok(spherecut_core::project::file::lexical(
            &root.given.join(rest),
        ))
    }

    /// The root `path` is under, and the rest of it. A relative path is
    /// under the first read root. An absolute one is under the root, read
    /// or write, whose given or canonical form is its longest prefix:
    /// matching is by component, and lexical only, since the kernel does
    /// the resolving.
    fn locate_read<'a>(&self, arg: &str, path: &'a Path) -> Result<(&Root, &'a Path), ToolError> {
        if path.as_os_str().is_empty() {
            return Err(ToolError::new(codes::BAD_ARGUMENT, "is empty").at(arg));
        }
        if !path.is_absolute() {
            return Ok((&self.read[0], path));
        }
        under(self.all(), path).ok_or_else(|| outside(arg, "read", self.all()))
    }

    fn all(&self) -> impl Iterator<Item = &Root> + Clone {
        self.read.iter().chain(&self.write)
    }

    /// An `openat2` refusal of `path` as the agent gets it (a1-plan
    /// section 5).
    fn refused(&self, arg: &str, e: Errno, want: Want, path: &Path) -> ToolError {
        let e = match (e, want) {
            (Errno::XDEV | Errno::AGAIN, Want::OutputFolder) => {
                return outside(arg, "write to", self.write.iter());
            }
            (Errno::XDEV | Errno::AGAIN, _) => return outside(arg, "read", self.all()),
            (Errno::NOENT, Want::OutputFolder) => {
                ToolError::new(codes::NOT_FOUND, "is in a folder that doesn't exist")
                    .next("write it to a folder that exists: none is made")
            }
            (Errno::NOENT, _) if path.is_relative() => {
                ToolError::new(codes::NOT_FOUND, "doesn't exist").next(
                    "a relative path is read under the first read root; probe a folder to see \
                     what is there, or give an absolute path",
                )
            }
            (Errno::NOENT, _) => ToolError::new(codes::NOT_FOUND, "doesn't exist"),
            (Errno::LOOP, _) => ToolError::new(
                codes::NOT_FOUND,
                "leads through a symlink Spherecut won't follow: a loop, or a link into /proc",
            ),
            (Errno::NOTDIR, Want::Folder) => ToolError::new(codes::NOT_A_FOLDER, "isn't a folder"),
            (Errno::NOTDIR, _) => {
                ToolError::new(codes::NOT_A_FOLDER, "has a part that isn't a folder")
            }
            (Errno::ACCESS | Errno::PERM, _) => {
                ToolError::new(codes::NOT_FOUND, "can't be read: permission denied")
            }
            (Errno::NAMETOOLONG, _) => ToolError::new(codes::BAD_ARGUMENT, "is too long"),
            (e, _) => ToolError::new(
                codes::NOT_FOUND,
                format!("can't be opened: {}", io::Error::from(e)),
            ),
        };
        e.at(arg)
    }
}

/// `rest` opened beneath `dir`, close-on-exec, with `EAGAIN` tried again.
fn beneath(dir: BorrowedFd<'_>, rest: &Path, flags: OFlags) -> Result<OwnedFd, Errno> {
    let rest = if rest.as_os_str().is_empty() {
        Path::new(".")
    } else {
        rest
    };
    let mut tries = 0;
    loop {
        match rustix::fs::openat2(
            dir,
            rest,
            flags | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
        ) {
            Err(Errno::AGAIN | Errno::INTR) if tries < AGAIN_TRIES => tries += 1,
            r => return r,
        }
    }
}

/// The root whose form is `path`'s longest prefix among `roots`, and the
/// rest of `path`.
fn under<'r, 'p>(
    roots: impl Iterator<Item = &'r Root>,
    path: &'p Path,
) -> Option<(&'r Root, &'p Path)> {
    roots
        .flat_map(|r| r.forms().map(|form| (r, form)))
        .filter_map(|(r, form)| {
            let rest = path.strip_prefix(form).ok()?;
            Some((form.components().count(), r, rest))
        })
        .max_by_key(|(n, _, _)| *n)
        .map(|(_, r, rest)| (r, rest))
}

/// `outside_roots`, listing `roots` as given, to [`ROOTS_LISTED_CHARS`].
fn outside<'r>(arg: &str, verb: &str, roots: impl Iterator<Item = &'r Root>) -> ToolError {
    let mut listed = String::new();
    let mut more = 0;
    for r in roots {
        let p = text::field(&r.given.to_string_lossy());
        if more > 0 || listed.chars().count() + p.chars().count() + 2 > ROOTS_LISTED_CHARS {
            more += 1;
            continue;
        }
        if !listed.is_empty() {
            listed.push_str(", ");
        }
        listed.push_str(&p);
    }
    if more > 0 {
        listed.push_str(&format!(" and {more} more"));
    }
    ToolError::new(
        codes::OUTSIDE_ROOTS,
        format!("is outside the folders Spherecut may {verb}, or leads out of them"),
    )
    .at(arg)
    .valid(format!("a path under {listed}"))
    .next("use a path under one of these, without `..` or a symlink out, or ask the user to add the folder")
}

fn unreadable(arg: &str, e: Errno) -> ToolError {
    ToolError::new(
        codes::NOT_FOUND,
        format!("can't be read: {}", io::Error::from(e)),
    )
    .at(arg)
}

/// The `/proc` path of `fd`, which opens the file `fd` is open on.
fn proc_path(fd: BorrowedFd<'_>) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

/// A file opened beneath a root, held open for the call.
#[derive(Debug)]
pub struct Opened {
    fd: OwnedFd,
    /// The path the agent gave.
    name: PathBuf,
    /// `/proc/self/fd/N` for `fd`.
    proc: PathBuf,
    size: u64,
}

impl Opened {
    /// The file for media to read: its descriptor's `/proc` path, named by
    /// the agent's. The number names this file only while the `Opened`
    /// lives, so the path is lent, never handed out (amendment 24).
    pub fn source(&self) -> Source<'_> {
        Source {
            open: &self.proc,
            name: &self.name,
        }
    }

    /// The path the agent gave.
    pub fn name(&self) -> &Path {
        &self.name
    }

    /// Its size when it was opened, in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Its descriptor's `/proc` path, for a reader on another thread that
    /// the holder of this `Opened` outlives (a render's media, a2-plan
    /// 5.3).
    pub fn proc_path(&self) -> &Path {
        &self.proc
    }
}

impl AsFd for Opened {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// A folder's entries.
#[derive(Debug)]
pub struct Listing {
    /// Sorted by name, without `.` and `..`, at most
    /// [`limits::FOLDER_ENTRIES`] of them.
    pub entries: Vec<Entry>,
    /// How many more the folder held, past the limit. Which ones were
    /// kept is then the file system's order, not the names'.
    pub left_out: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    pub kind: EntryKind,
    /// When it was last modified, in seconds since 1970 (UTC).
    pub modified: Option<i64>,
}

/// What an entry is, without following it: a symlink is a `Link`, which
/// opening it as a file resolves beneath the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
    Link,
    Other,
}

/// A folder beneath the write root, held open, and the name a file is to
/// take in it ([`Roots::output`]).
#[derive(Debug)]
pub struct Folder {
    fd: OwnedFd,
    /// `/proc/self/fd/N` for `fd`.
    proc: PathBuf,
    name: OsString,
    /// The file's path as the agent names it: the write root as it was
    /// given, joined with the rest, lexically.
    path: PathBuf,
}

impl Folder {
    /// The path to write the file at: through the folder's descriptor,
    /// so it names this folder only while the `Folder` lives.
    pub fn exact(&self) -> PathBuf {
        self.proc.join(&self.name)
    }

    /// The folder, through its descriptor.
    pub fn dir(&self) -> &Path {
        &self.proc
    }

    /// The file's path as the agent names it, absolute.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's name in the folder.
    pub fn name(&self) -> &OsStr {
        &self.name
    }

    /// The folder, and the name: two renders to the same file are the
    /// same pair.
    pub fn key(&self) -> io::Result<(u64, u64, OsString)> {
        let st = rustix::fs::fstat(&self.fd)?;
        Ok((st.st_dev, st.st_ino, self.name.clone()))
    }

    /// Whether anything is at the name now.
    pub fn taken(&self) -> bool {
        rustix::fs::statat(&self.fd, self.name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW).is_ok()
    }
}

impl AsFd for Folder {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// A new file under the write root, removed again on drop unless kept, so
/// that a failed or cancelled call leaves nothing for a retry to trip on
/// (amendment 23).
#[derive(Debug)]
pub struct Created {
    file: std::fs::File,
    /// The folder it is in, `O_PATH`.
    parent: OwnedFd,
    name: OsString,
    /// The path the agent gave.
    path: PathBuf,
    kept: bool,
}

impl Created {
    /// The file's path as the agent names it, absolute.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the file, written in full.
    pub fn keep(mut self) -> io::Result<()> {
        self.file.flush()?;
        self.kept = true;
        Ok(())
    }
}

impl Write for Created {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for Created {
    fn drop(&mut self) {
        if !self.kept
            && let Err(e) = rustix::fs::unlinkat(&self.parent, &self.name, AtFlags::empty())
        {
            tracing::warn!("mcp: an unfinished output wasn't removed: {e}");
        }
    }
}
