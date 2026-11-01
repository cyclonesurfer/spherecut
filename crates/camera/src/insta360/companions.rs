//! A recording's files: named from a file's name alone, and a master and
//! its LRV checked by their bytes (insta360.md §2.3, §2.4 and §6; M3 plan
//! 5.4). M3.2's unit (c), moving the naming rule from the agent's
//! `ops/clips.rs`, which now calls [`companions`] and whose tests still
//! pass.

use std::fmt;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use spherecut_core::may_block;
use thiserror::Error;

use super::trailer::read_at;
use super::{RecordRef, Trailer};
use crate::{CameraError, Part};

/// The exposure record's id (§3.3), which [`verify_companion`] compares.
const EXPOSURE: u8 = 4;
/// How much of each file's exposure record [`verify_companion`] holds at
/// once: 64 KiB, so that a record of any size is compared with no
/// allocation larger than this. Pat's longest is 30 KB.
const CHUNK: usize = 64 << 10;

/// A recording, as an Insta360 file's name gives it:
/// `[PRO_](VID|LRV)_YYYYMMDD_HHMMSS_LK_NNN[-trim].ext` (§2.3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Recording {
    /// FreeFrame's `PRO_` prefix.
    pub pro: bool,
    /// `YYYYMMDD`, the camera's local date.
    pub date: String,
    /// `HHMMSS`, its local time.
    pub time: String,
    /// The sequence number with any suffix the name has (`-trim`), so a
    /// trimmed copy is a recording of its own.
    pub seq: String,
}

/// What a file is to its recording, by its name's `LK` digits (L the
/// lens, K 0 for a master and 1 for a proxy), its prefix and its
/// extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameRole {
    /// A master of lens 0, `_00_`: both lenses, on a one-file camera such
    /// as the X5.
    Lens0,
    /// Lens 1's master on a two-file camera, `_10_`.
    Lens1,
    /// A proxy: `LRV_`, a K of 1, or `.lrv`.
    Proxy,
}

/// An Insta360 file's name, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsvName {
    pub recording: Recording,
    pub role: NameRole,
}

/// What a file's name says about its recording's other files, with no
/// file opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Companions {
    /// The name, read; None when it isn't an Insta360 camera's, and then
    /// so are the paths.
    pub name: Option<InsvName>,
    /// The master beside it, `[PRO_]VID_…_00_NNN.insv`: the file itself
    /// when it is the master.
    pub master: Option<PathBuf>,
    /// The proxy beside it, `[PRO_]LRV_…_01_NNN.lrv`: the file itself when
    /// it is the proxy.
    pub lrv: Option<PathBuf>,
}

/// The recording's files that `path`'s name implies, master to LRV and
/// back, by the X5's names (§2.3). Names only: nothing is opened, so
/// whether the files are there, and whether they belong together, is for
/// the caller and [`verify_companion`].
pub fn companions(path: &Path) -> Companions {
    let Some(name) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(read_name)
    else {
        return Companions {
            name: None,
            master: None,
            lrv: None,
        };
    };
    let r = &name.recording;
    let beside = |kind: &str, lk: &str, ext: &str| {
        let pro = if r.pro { "PRO_" } else { "" };
        path.with_file_name(format!(
            "{pro}{kind}_{}_{}_{lk}_{}.{ext}",
            r.date, r.time, r.seq
        ))
    };
    let (master, lrv) = match name.role {
        NameRole::Lens0 => (path.to_path_buf(), beside("LRV", "01", "lrv")),
        NameRole::Lens1 => (beside("VID", "00", "insv"), beside("LRV", "01", "lrv")),
        NameRole::Proxy => (beside("VID", "00", "insv"), path.to_path_buf()),
    };
    Companions {
        name: Some(name),
        master: Some(master),
        lrv: Some(lrv),
    }
}

/// A file name read as an Insta360 camera's,
/// `[PRO_](VID|LRV)_YYYYMMDD_HHMMSS_LK_NNN[suffix].ext` with `ext` one of
/// `insv`, `lrv` and `mp4` in any case; None for any other. The sequence
/// number must start with digits, and whatever follows a `-`, a `(` or a
/// space after them (`-trim`, ` (1)`) stays in [`Recording::seq`].
fn read_name(file: &str) -> Option<InsvName> {
    let (stem, ext) = file.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    if !matches!(ext.as_str(), "insv" | "lrv" | "mp4") {
        return None;
    }
    let (pro, rest) = match stem.strip_prefix("PRO_") {
        Some(rest) => (true, rest),
        None => (false, stem),
    };
    let mut parts = rest.splitn(5, '_');
    let kind = parts.next()?;
    let (date, time, lk, seq) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let proxy_name = match kind {
        "VID" => false,
        "LRV" => true,
        _ => return None,
    };
    let number = seq.split(['-', '(', ' ']).next().unwrap_or("");
    if !digits(date, 8) || !digits(time, 6) || number.is_empty() || !digits(number, number.len()) {
        return None;
    }
    let &[lens, kind_digit] = lk.as_bytes() else {
        return None;
    };
    if !lens.is_ascii_digit() || !kind_digit.is_ascii_digit() {
        return None;
    }
    let role = if proxy_name || kind_digit == b'1' || ext == "lrv" {
        NameRole::Proxy
    } else if lens == b'1' {
        NameRole::Lens1
    } else {
        NameRole::Lens0
    };
    Some(InsvName {
        recording: Recording {
            pro,
            date: date.into(),
            time: time.into(),
            seq: seq.into(),
        },
        role,
    })
}

/// Whether `s` is `n` ASCII digits.
fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a master and its LRV are one recording's, by their bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pairing {
    /// Their exposure records (4) are byte for byte the same, as on all 7
    /// of Pat's pairs.
    Verified,
    /// Paired by name only: `why`, in words, is what the bytes didn't
    /// show, for the notice the caller gives.
    NameOnly { why: String },
}

/// Which file of a master and its LRV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Master,
    Lrv,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Master => "master",
            Side::Lrv => "LRV",
        })
    }
}

/// Why [`verify_companion`] couldn't compare a master with its LRV: the
/// error, with the file it is in, since both files' record 4 is
/// [`Part::Record`] 4 and the byte alone doesn't say which.
#[derive(Debug, Error)]
#[error("in the {side}, {error}")]
pub struct CompanionError {
    pub side: Side,
    pub error: CameraError,
}

/// Compares record 4 of a master and of its LRV, each with its trailer
/// ([`super::read_trailer`]). Equal is [`Pairing::Verified`]; different,
/// missing from either, empty in both or over `max` bytes is
/// [`Pairing::NameOnly`], saying which. A record that can't be read is an
/// error naming its file.
///
/// The records' sizes are compared first, from the trailers, and then,
/// when they are equal and at most `max` bytes (as [`super::read_record`]
/// takes its `max`), their bytes, 64 KiB of each file at a time, so
/// nothing larger is allocated whatever the trailers say. A record 4
/// whose slot runs into the index or past the file ([`Trailer::skipped`])
/// is [`CameraError::SlotPastEnd`] at its slot, as `read_record` gives;
/// bytes that end inside it are [`CameraError::Truncated`] in
/// [`Part::Record`] 4, and a failed read [`CameraError::Io`], each at the
/// byte of its own file it was for, in a [`CompanionError`] that names
/// the file.
///
/// It calls [`spherecut_core::may_block`] first, so on the UI thread it
/// panics in a debug build.
pub fn verify_companion<M: Read + Seek, L: Read + Seek>(
    master: (&mut M, &Trailer),
    lrv: (&mut L, &Trailer),
    max: usize,
) -> Result<Pairing, CompanionError> {
    may_block("comparing an Insta360 master's exposure record with its LRV's");
    let (m, mt) = master;
    let (l, lt) = lrv;
    let name_only = |why: String| Ok(Pairing::NameOnly { why });
    let in_file = |side: Side| move |error: CameraError| CompanionError { side, error };
    let found = (
        exposure(mt).map_err(in_file(Side::Master))?,
        exposure(lt).map_err(in_file(Side::Lrv))?,
    );
    let (a, b) = match found {
        (Some(a), Some(b)) => (a, b),
        (None, None) => {
            return name_only("neither file has an exposure record (4) to compare".into());
        }
        (None, Some(_)) => {
            return name_only(
                "the master has no exposure record (4) to compare with its LRV's".into(),
            );
        }
        (Some(_), None) => {
            return name_only(
                "the LRV has no exposure record (4) to compare with its master's".into(),
            );
        }
    };
    if a.size != b.size {
        return name_only(format!(
            "their exposure records (4) differ in size: {} bytes in the master, {} in the LRV",
            a.size, b.size
        ));
    }
    let size = u64::from(a.size);
    if size == 0 {
        // No bytes to show the two are one recording's.
        return name_only("their exposure records (4) are empty".into());
    }
    if size > u64::try_from(max).unwrap_or(u64::MAX) {
        return name_only(format!(
            "their exposure records (4) are {size} bytes each, over the {max} compared"
        ));
    }
    let chunk = usize::try_from(size).map_or(CHUNK, |s| s.min(CHUNK));
    let (mut x, mut y) = (vec![0; chunk], vec![0; chunk]);
    let mut done = 0u64;
    while done < size {
        let n = usize::try_from(size - done).map_or(chunk, |left| left.min(chunk));
        let (x, y) = (&mut x[..n], &mut y[..n]);
        read_at(m, a.offset + done, x, Part::Record(EXPOSURE)).map_err(in_file(Side::Master))?;
        read_at(l, b.offset + done, y, Part::Record(EXPOSURE)).map_err(in_file(Side::Lrv))?;
        if let Some(at) = x.iter().zip(y.iter()).position(|(p, q)| p != q) {
            return name_only(format!(
                "their exposure records (4) differ from byte {} of the record",
                done + at as u64
            ));
        }
        done += n as u64;
    }
    Ok(Pairing::Verified)
}

/// The trailer's exposure record, or None when it lists none; a slot for
/// one that runs into the index or past the file is
/// [`CameraError::SlotPastEnd`] at the slot.
fn exposure(t: &Trailer) -> Result<Option<&RecordRef>, CameraError> {
    if let Some(rec) = t.records.iter().find(|r| r.id == EXPOSURE) {
        return Ok(Some(rec));
    }
    match t.skipped.iter().find(|r| r.id == EXPOSURE) {
        Some(rec) => Err(CameraError::SlotPastEnd {
            id: EXPOSURE,
            offset: rec.slot,
        }),
        None => Ok(None),
    }
}
