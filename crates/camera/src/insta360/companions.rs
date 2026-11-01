//! A recording's files: named from a file's name alone, and a master and
//! its LRV checked by their bytes (insta360.md §2.3, §2.4 and §6; M3 plan
//! 5.4). M3.2's unit (c), moving the naming rule from the agent's
//! `ops/clips.rs`, whose tests still pass.

#![expect(
    unused_variables,
    reason = "the bodies are todo!() until M3.2's unit (c) writes them"
)]

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use super::Trailer;
use crate::CameraError;

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
    todo!()
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

/// Compares record 4 of a master and of its LRV, each with its trailer
/// ([`super::read_trailer`]). Equal is [`Pairing::Verified`]; different,
/// or missing from either, is [`Pairing::NameOnly`]. A record that can't
/// be read is an error.
///
/// It calls [`spherecut_core::may_block`] first, so on the UI thread it
/// panics in a debug build.
pub fn verify_companion<M: Read + Seek, L: Read + Seek>(
    master: (&mut M, &Trailer),
    lrv: (&mut L, &Trailer),
) -> Result<Pairing, CameraError> {
    todo!()
}
