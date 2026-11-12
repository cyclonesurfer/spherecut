//! A synthetic `.insv` and its LRV for the CLI's tests (M3 plan 7.2):
//! synth's `write_insv`, which codes each lens alone (HEVC, lossless)
//! and muxes the two tracks with a silent AAC stream, as the X5 writes
//! its masters, with synth's X5-like trailer after them; and a one-track
//! LRV of both lenses side by side whose trailer holds the master's
//! records with the LRV's own metadata fields (`Records::x5_lrv`), its
//! exposure record among them. The trailer's calibrations are Pat's V3
//! and V6, as the boundary crate's `pats` reads them from
//! `testkit::pats`, the one copy of Pat's strings (F137): none of Pat's
//! numbers is copied here.
//!
//! The pattern doesn't matter to the tests that use these files, but
//! the corner codes `write_insv` draws on each lens do: `frame_insv.rs`
//! and `frame.rs` read them, or hold `--lens`'s pictures to one another,
//! to tell the lenses' tracks and frames apart. Each lens shows
//! `SeamLines` through Pat's V3 composed under synth's `x5_like_planted`
//! at SIDE², whatever the trailer says.

use std::path::{Path, PathBuf};

use spherecut_boundary::pats;
use spherecut_core::FrameRate;
use spherecut_synth::Pattern;
use spherecut_synth::insv::{
    self, InsvFault, InsvSpec, LrvSpec, Quality, Records, TrailerLayout, write_insv,
};
use spherecut_synth::lens::{
    Angle, Axis, CalibTruth, Generation, Planted, QuarterTurn, Sign, TruthBinding, TruthCanvas,
    TruthEuler, TruthTurn, TruthTz, TruthV6, compose, x5_like_planted,
};

/// The frames each file holds, and their rate.
pub const FRAMES: u32 = 6;
pub const RATE: FrameRate = FrameRate::FPS_25;
/// Each lens's square, in pixels.
pub const SIDE: u32 = 128;
/// The names the files get: a recording of 22 October 2026 at 10:15,
/// number 001, as the X5 names its files (insta360.md §2.3).
pub const MASTER: &str = "VID_20261022_101500_00_001.insv";
pub const LRV: &str = "LRV_20261022_101500_01_001.lrv";

/// A master written into a folder, and where synth put its trailer.
pub struct Written {
    pub path: PathBuf,
    pub layout: TrailerLayout,
}

/// The two calibrations Pat's X5 writes, V3 and V6, as the boundary
/// crate reads them from `testkit::pats`.
pub fn pats_calibrations() -> [CalibTruth; 2] {
    [pats(Generation::V3), pats(Generation::V6)]
}

/// The records [`master`] writes in its trailer: `Records::x5_like` for
/// Pat's two calibrations, which a test may change before
/// [`master_with`] writes them.
pub fn records() -> Records {
    Records::x5_like(&pats_calibrations(), &planted(), FRAMES, RATE)
}

/// The spec of a master named `name` of `frames` frames with the trailer
/// of `records` and `fault` planted: two SIDE² lenses, lossless.
fn spec(name: &str, frames: u32, records: &Records, fault: Option<InsvFault>) -> InsvSpec {
    let calib = x5_like_planted();
    InsvSpec {
        name: name.into(),
        rig: compose(&pats(Generation::V3), &calib, 0, [SIDE; 2]),
        pattern: Pattern::SeamLines,
        frames,
        rate: RATE,
        quality: Quality::Lossless,
        calib,
        records: records.clone(),
        fault,
        lrv: None,
        keyint: None,
        ss: 1,
    }
}

/// An X5-like master in `dir` named `name`, as `write_insv` writes it:
/// two HEVC lens tracks, each SIDE square, a silent AAC track, and the
/// trailer of [`records`], with `fault` planted.
pub fn master(dir: &Path, name: &str, fault: Option<InsvFault>) -> Written {
    master_with(dir, name, &records(), fault)
}

/// [`master`] with the trailer of `records`. It also leaves the
/// master's MP4 without its trailer beside it ([`tracks`]).
pub fn master_with(dir: &Path, name: &str, records: &Records, fault: Option<InsvFault>) -> Written {
    let files = write_insv(&spec(name, FRAMES, records, fault), dir).unwrap();
    let layout = files.layout;
    let bytes = std::fs::read(&files.master).unwrap();
    std::fs::write(tracks(dir, name), &bytes[..inst_at(&layout)]).unwrap();
    Written {
        path: files.master,
        layout,
    }
}

/// Where the `inst` box that holds a trailer laid out as `layout` starts:
/// the MP4's length before it.
fn inst_at(layout: &TrailerLayout) -> usize {
    usize::try_from(layout.payload_start - 8).unwrap()
}

/// The MP4 that [`master`] wrote in `dir` for the master `name`, before
/// the trailer: a plain file of the master's tracks, its AAC track
/// among them, with no trailer.
pub fn tracks(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.tracks.mp4"))
}

/// A master in `dir` named `name`, of the tracks [`master`] wrote there
/// for the master `from` ([`tracks`]), with the trailer of `records` and
/// `fault` planted in it: another trailer on the same tracks, which
/// codes nothing more.
pub fn retrailed(
    dir: &Path,
    from: &str,
    name: &str,
    records: &Records,
    fault: Option<InsvFault>,
) -> Written {
    let path = dir.join(name);
    let layout = insv::append_trailer(&tracks(dir, from), &path, records, fault).unwrap();
    Written { path, layout }
}

/// The records [`lrv`] writes in its trailer for an LRV of `frames`
/// frames: [`records`] as `Records::x5_lrv` gives the LRV them, so that
/// its exposure record (4) is the master's whatever `frames` is, as the
/// X5's is. A test may change them before [`lrv_with`] writes them.
pub fn lrv_records(frames: u32) -> Records {
    Records::x5_lrv(&records(), frames)
}

/// An LRV in `dir` named `name` (`LRV_<date>_<time>_01_<NNN>.lrv`): one
/// H.264 track of `frames` frames, both lenses side by side at SIDE², a
/// silent AAC track, and the trailer of [`lrv_records`].
pub fn lrv(dir: &Path, name: &str, frames: u32) -> PathBuf {
    lrv_with(dir, name, frames, &lrv_records(frames))
}

/// [`lrv`] with the trailer of `records`: `write_insv`'s LRV of a master
/// of `frames` frames at 25 fps, which has as many, written in a folder
/// of its own, its trailer cut off and that of `records` appended.
pub fn lrv_with(dir: &Path, name: &str, frames: u32, records: &Records) -> PathBuf {
    let work = dir.join(format!(".{name}-master"));
    let spec = InsvSpec {
        lrv: Some(LrvSpec {
            side: SIDE,
            quality: Quality::Lossless,
        }),
        ..spec(&master_of(name), frames, &self::records(), None)
    };
    let written = write_insv(&spec, &work).unwrap().lrv.unwrap();
    assert_eq!(written.file_name().unwrap(), name);
    let bytes = std::fs::read(&written).unwrap();
    let mp4 = work.join("lrv.mp4");
    std::fs::write(&mp4, &bytes[..trailer_start(&bytes)]).unwrap();
    let path = dir.join(name);
    insv::append_trailer(&mp4, &path, records, None).unwrap();
    std::fs::remove_dir_all(&work).unwrap();
    path
}

/// The master's name an LRV's name follows from (insta360.md §2.3):
/// `LRV_<date>_<time>_01_<NNN>.lrv` is `VID_<date>_<time>_00_<NNN>.insv`'s.
fn master_of(lrv: &str) -> String {
    let stem = lrv
        .strip_prefix("LRV_")
        .and_then(|s| s.strip_suffix(".lrv"))
        .unwrap_or_else(|| panic!("{lrv} isn't an LRV's name"));
    let (head, number) = stem.rsplit_once('_').unwrap();
    let (when, _) = head.rsplit_once('_').unwrap();
    format!("VID_{when}_00_{number}.insv")
}

/// Where the `inst` box of a trailer with no fault starts in `bytes`: 8
/// bytes before its size E, which the tail holds at EOF − 40.
fn trailer_start(bytes: &[u8]) -> usize {
    let at = bytes.len() - 40;
    let e = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    bytes.len() - e as usize - 8
}

/// One value of each convention synth plants; M3.2's `print` reads none.
pub fn planted() -> Planted {
    Planted {
        binding: TruthBinding::Direct,
        canvas: TruthCanvas::A,
        euler: TruthEuler {
            axes: [Axis::Z, Axis::Y, Axis::X],
            order: [Angle::Yaw, Angle::Pitch, Angle::Roll],
            signs: [Sign::Plus; 3],
            roll_offset: QuarterTurn::Minus90,
        },
        turn: TruthTurn::Y,
        upright: QuarterTurn::Zero,
        tz: TruthTz::AsStored,
        v6: TruthV6::RADIAL_ONLY,
    }
}
