//! What probe reads of a camera's own records beside what FFmpeg reads:
//! an Insta360 file's trailer, through the camera crate (M3 plan 5.4).
//! M3.2's unit (c).
//!
//! The reads run where probe already reads, on its caller's thread and
//! never the UI's: camera's readers call `may_block` (D34). Which lens
//! each track holds, the geometry and the screen side are the built-in
//! table's entry for the file's camera (plan 5.3), through
//! [`spherecut_camera::insta360::stitching`], the one entry the stitch
//! reads too (M3.5).

use std::fs::File;
use std::path::{Path, PathBuf};

use serde::Serialize;
use spherecut_camera::insta360::calib::{self, Generation};
use spherecut_camera::insta360::known::Known;
use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{
    Metadata, NameRole, Pairing, Trailer, companions, parse_exposure, parse_metadata, read_record,
    read_trailer, stitching,
};
use spherecut_camera::{CameraError, Part};
use spherecut_sphere::rig::{Binding, Status};

use crate::Source;
use crate::detect::{CameraGuess, Layout};
use crate::probe::{ProbeReport, Problem, StreamInfo, StreamKind, codes};

/// The most of record 4 probe reads to count its items, and that
/// [`verify_companion`] compares: 64 MiB, four million frames, past any
/// recording a card holds. Pat's longest is 30 KB.
const MAX_EXPOSURE: usize = 64 << 20;
/// The ids of the records probe reads: the metadata and the exposure.
const METADATA: u8 = 1;
const EXPOSURE: u8 = 4;

/// What probe found in a camera's own records (M3 plan 5.4); each field
/// is null for a file without them, which is every file but an Insta360
/// camera's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CameraFile {
    /// A dual-fisheye master's two lens streams.
    pub lenses: Option<Lenses>,
    /// The calibration the trailer holds, when a string of it parses.
    pub calibration: Option<Calibration>,
    /// How sure the lenses' geometry is, from the built-in table's entry
    /// for the camera, when the calibration is one a stitch reads: on a
    /// master and on its LRV, which holds the master's strings.
    pub geometry: Option<GeometryInfo>,
    /// The recording's other file by name, and whether the bytes agree:
    /// filled by [`verify_companion`], which the caller runs, since only
    /// it can open a file beside this one (D45).
    pub companion: Option<Companion>,
    /// How many items record 4 holds: one a frame the camera exposed,
    /// from a few before the tracks' first (insta360.md §3.4).
    pub exposure_entries: Option<u64>,
    /// The records the trailer's index lists that lie inside the
    /// trailer; a slot whose record runs into the index or past the file
    /// is left out, with a [`codes::BAD_TRAILER`] warning. None when the
    /// file has no trailer or its trailer can't be read.
    pub records: Option<Vec<RecordInfo>>,
    /// Why the calibration gives no stitch, when a string of it parses
    /// but [`stitching`] refuses it or finds no V3 or V6 string: what
    /// `doctor --file` says is missing. Not in the JSON, where
    /// `geometry` null and a [`codes::BAD_TRAILER`] warning, or
    /// `calibration.in_use` null or `generations`, say as much.
    #[serde(skip)]
    pub unstitched: Option<String>,
    /// Why the built-in table doesn't stitch the camera's files, when
    /// the calibration is one a stitch reads and the table says so
    /// ([`Known::refusal`]): what `doctor --file` says is missing,
    /// in the words frame's refusal of the pair gives. Not in the JSON,
    /// which says nothing of it yet; no entry refuses until M3.6's fit.
    #[serde(skip)]
    pub table_refusal: Option<String>,
}

/// A dual-fisheye file's lens streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lenses {
    /// The two video streams, by FFmpeg's index, in file order.
    pub tracks: [usize; 2],
    /// Which track each calibration block describes, as the table's
    /// entry says: `direct` (block b, track b) or `reversed` (block b,
    /// track 1 − b; the X5's). None without a calibration a stitch reads.
    pub binding: Option<&'static str>,
    /// How the entry knows: its geometry's evidence ("placeholder until
    /// the fit" on the X5 until M3.6). None with `binding`.
    pub evidence: Option<String>,
    /// The stream, by FFmpeg's index, of the lens on the screen's side,
    /// once a take settles it (M3.12); None until then.
    pub screen_side: Option<usize>,
}

/// The lenses' geometry as the built-in table's entry gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeometryInfo {
    /// How far it is to be trusted: [`status_word`]'s `provisional`,
    /// `settled`, `fitted` or `user`.
    pub status: &'static str,
    /// A sentence: why, from the entry, never the file's own text.
    pub evidence: String,
}

/// A geometry's status as probe and `doctor --file` word it.
pub fn status_word(s: Status) -> &'static str {
    match s {
        Status::Provisional => "provisional",
        Status::Settled => "settled",
        Status::Fitted => "fitted",
        Status::User => "user",
    }
}

/// A binding as probe words it.
fn binding_word(b: Binding) -> &'static str {
    match b {
        Binding::Direct => "direct",
        Binding::Reversed => "reversed",
    }
}

/// The calibration in a file's trailer, as a project's snapshot of it
/// will record it (M3 plan 11.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Calibration {
    /// The generations whose working string parses, oldest first: `v1`,
    /// `v2`, `v3`, `v6`.
    pub generations: Vec<String>,
    /// The generation field 136 says the camera uses, when its string
    /// parses: `v6` on the X5.
    pub in_use: Option<String>,
    /// The side-by-side canvas the strings are in, width and height: the
    /// one in use's, else the newest's.
    pub canvas: [u32; 2],
    /// Field 27, the part of each lens's square the tracks show.
    pub window: Option<WindowInfo>,
    /// [`spherecut_camera::insta360::Calibrations::hash`]: 16 hex digits
    /// of FNV-1a 64 over the working strings.
    pub hash: String,
    /// Field 136 as the trailer gives it, whether or not it names a
    /// generation whose string parses: what `doctor --file` tells a
    /// calibration the camera names but that doesn't parse from one it
    /// doesn't name. Not in the JSON, where `in_use` and a
    /// [`codes::BAD_TRAILER`] warning say as much.
    #[serde(skip)]
    pub offset_version: Option<u64>,
}

/// Field 27: `src`, the square on the canvas; `dst`, the track's; and
/// `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WindowInfo {
    pub src: [u32; 2],
    pub dst: [u32; 2],
    pub offset: [u32; 2],
}

/// A master's LRV, or an LRV's master, by name (insta360.md §2.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Companion {
    /// As text, with bytes that are not UTF-8 replaced by U+FFFD.
    #[serde(serialize_with = "lossy_path")]
    pub path: PathBuf,
    /// Whether the two files' exposure records (4) are byte for byte the
    /// same: the bytes say the two are one recording's.
    pub verified: bool,
    /// When not verified, why: paired by name only.
    pub why: Option<String>,
}

/// A record the trailer's index lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RecordInfo {
    pub id: u8,
    /// Its payload's size in bytes.
    pub size: u32,
}

/// A path as text, so that any file name can be reported.
fn lossy_path<S: serde::Serializer>(p: &Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&p.to_string_lossy())
}

fn bad_trailer(message: String) -> Problem {
    Problem::new(codes::BAD_TRAILER, message)
}

/// Reads an Insta360 file's trailer through `open`, `len` bytes long:
/// its records, the metadata (record 1) into `guess`'s model, firmware
/// and serial, the calibration strings, and record 4's length. A trailer,
/// a record or a string that can't be read is a [`codes::BAD_TRAILER`]
/// warning in `warnings`, and what it would have given is left out; the
/// file plays without it. A file with no trailer gives nothing.
pub(crate) fn read(
    open: &Path,
    len: u64,
    guess: &mut CameraGuess,
    streams: &[StreamInfo],
    warnings: &mut Vec<Problem>,
) -> CameraFile {
    let mut out = CameraFile::default();
    // A file that can't be opened again is said so in words of its own:
    // no byte of the trailer was read.
    let mut f = match File::open(open) {
        Ok(f) => f,
        Err(e) => {
            warnings.push(bad_trailer(format!(
                "the file can't be opened to read its Insta360 trailer: {e}"
            )));
            return out;
        }
    };
    let trailer = match read_trailer(&mut f, len) {
        Ok(Some(t)) => t,
        Ok(None) => return out,
        Err(e) => {
            warnings.push(bad_trailer(format!(
                "the Insta360 trailer can't be read: {e}"
            )));
            return out;
        }
    };
    for s in &trailer.skipped {
        warnings.push(bad_trailer(format!(
            "the Insta360 trailer's record {}: {}",
            s.id,
            CameraError::SlotPastEnd {
                id: s.id,
                offset: s.slot
            }
        )));
    }
    out.records = Some(
        trailer
            .records
            .iter()
            .map(|r| RecordInfo {
                id: r.id,
                size: r.size,
            })
            .collect(),
    );
    if guess.layout == Layout::Insta360DualFisheye {
        let mut video = streams.iter().filter(|s| {
            s.kind == StreamKind::Video && !s.disposition.iter().any(|d| d == "attached_pic")
        });
        if let (Some(a), Some(b)) = (video.next(), video.next()) {
            out.lenses = Some(Lenses {
                tracks: [a.index, b.index],
                binding: None,
                evidence: None,
                screen_side: None,
            });
        }
    }
    let mut record = |id: u8, max: usize| match read_record(&mut f, &trailer, id, max) {
        Ok(p) => p,
        // Each skipped slot is warned of above.
        Err(CameraError::SlotPastEnd { .. }) => None,
        Err(e) => {
            warnings.push(bad_trailer(format!(
                "the Insta360 trailer's record {id} can't be read: {e}"
            )));
            None
        }
    };
    let metadata = record(METADATA, MAX_METADATA);
    let exposure = record(EXPOSURE, MAX_EXPOSURE);
    match exposure.as_deref().map(parse_exposure) {
        Some(Ok(items)) => out.exposure_entries = Some(items.len() as u64),
        Some(Err(e)) => warnings.push(bad_trailer(format!(
            "the Insta360 trailer's exposure record: {e}"
        ))),
        None => {}
    }
    let metadata = match metadata.as_deref().map(parse_metadata) {
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            warnings.push(bad_trailer(format!(
                "the Insta360 trailer's metadata (record 1): {e}"
            )));
            return out;
        }
        None => return out,
    };
    name_camera(guess, &metadata);
    out.calibration = calibration(&metadata, warnings);
    match stitching(&metadata) {
        Ok(Some(s)) => bind(&mut out, &s.known),
        // A trailer of V1 and V2 strings alone, as older cameras write:
        // nothing wrong with it, but nothing a stitch reads.
        Ok(None) => {
            if out.calibration.is_some() {
                out.unstitched =
                    Some("the trailer holds neither a V3 nor a V6 calibration string".into());
            }
        }
        Err(e) => {
            if out
                .calibration
                .as_ref()
                .is_some_and(|c| warn_unstitched(&e, c))
            {
                warnings.push(bad_trailer(format!(
                    "the Insta360 trailer's calibration can't be stitched: {e}"
                )));
            }
            out.unstitched = Some(e.to_string());
        }
    }
    out
}

/// The table's entry `known` into `out`: the lenses' binding, its
/// evidence and the screen side, the geometry, and the table's refusal
/// of the camera's files, if it gives one.
fn bind(out: &mut CameraFile, known: &Known) {
    let g = &known.geometry;
    if let Some(l) = out.lenses.as_mut() {
        l.binding = Some(binding_word(g.binding));
        l.evidence = Some(g.evidence.clone());
        l.screen_side = known
            .screen_side_track
            .and_then(|t| l.tracks.get(usize::from(t)).copied());
    }
    out.geometry = Some(GeometryInfo {
        status: status_word(g.status),
        evidence: g.evidence.clone(),
    });
    out.table_refusal = known.refusal();
}

/// Whether [`stitching`]'s refusal `e` of the calibration `c` needs a
/// warning of its own: a string that doesn't parse has its warning from
/// [`calibration`], and field 136 when it names no generation that
/// parses has `in_use` null (and its own warning when it names one the
/// trailer doesn't hold or that doesn't parse).
fn warn_unstitched(e: &CameraError, c: &Calibration) -> bool {
    match e {
        CameraError::NotCalibration { part, .. } => *part != Part::Field(136) || c.in_use.is_some(),
        _ => false,
    }
}

/// The camera as the metadata names it, over what the container's tags
/// said.
fn name_camera(guess: &mut CameraGuess, m: &Metadata) {
    let mut named = Vec::new();
    if let Some(model) = &m.model {
        guess.model = Some(model.clone());
        named.push(format!("model {model}"));
    }
    if let Some(firmware) = &m.firmware {
        guess.firmware = Some(firmware.clone());
        named.push(format!("firmware {firmware}"));
    }
    if let Some(serial) = &m.serial {
        guess.serial = Some(serial.clone());
        named.push("its serial".into());
    }
    if !named.is_empty() {
        guess.evidence.push(format!(
            "the trailer's metadata (record 1) gives {}",
            named.join(", ")
        ));
    }
}

/// The calibration the metadata's strings give, with a warning for each
/// working string that doesn't parse; None when none does.
fn calibration(m: &Metadata, warnings: &mut Vec<Problem>) -> Option<Calibration> {
    let mut parsed = Vec::new();
    for g in Generation::ALL {
        let Some(s) = m.calib.get(g).working.as_deref() else {
            continue;
        };
        match calib::parse(g, s) {
            Ok(c) => parsed.push(c),
            Err(e) => warnings.push(bad_trailer(format!(
                "the Insta360 trailer's {g} calibration string (field {}): {e}",
                g.fields()[0]
            ))),
        }
    }
    let in_use = m.offset_version.and_then(Generation::of_offset_version);
    if let Some(g) = in_use
        && !parsed.iter().any(|c| c.generation == g)
    {
        warnings.push(bad_trailer(format!(
            "the Insta360 trailer's field 136 says the camera uses its {g} calibration, which it doesn't hold or which doesn't parse"
        )));
    }
    let used = in_use.and_then(|g| parsed.iter().find(|c| c.generation == g));
    let canvas = used.or(parsed.last())?.canvas;
    Some(Calibration {
        generations: parsed
            .iter()
            .map(|c| c.generation.to_string().to_lowercase())
            .collect(),
        in_use: used.map(|c| c.generation.to_string().to_lowercase()),
        canvas: [canvas.0, canvas.1],
        window: m.window.map(|w| WindowInfo {
            src: w.src,
            dst: w.dst,
            offset: w.offset,
        }),
        hash: m.calib.hash(),
        offset_version: m.offset_version,
    })
}

/// The recording's other file that `report`'s name implies, as
/// [`verify_companion`] compares it: an Insta360 master's LRV, or an
/// LRV's master (insta360.md §2.3). None for a file with no trailer, a
/// name that isn't an Insta360 camera's, and lens 1's file of a two-file
/// camera. Names only: whether the file is there is the caller's to find.
pub fn companion_path(report: &ProbeReport) -> Option<PathBuf> {
    report.camera_file.records.as_ref()?;
    let c = companions(&report.path);
    match c.name?.role {
        NameRole::Lens0 => c.lrv,
        NameRole::Proxy => c.master,
        NameRole::Lens1 => None,
    }
}

/// Compares `report`'s file, opened through `own`, with its companion,
/// `other` (the path [`companion_path`] gives, opened by the caller, as
/// an agent opens files beneath its roots, D45), by their exposure
/// records ([`spherecut_camera::insta360::verify_companion`]): verified
/// when they are byte for byte the same, and otherwise paired by name
/// only, with why, which a trailer of either that can't be read is too,
/// and records over `MAX_EXPOSURE` are, uncompared. The why names the
/// file at fault, master or LRV.
///
/// It reads both files, so it runs where probe does, never on the UI
/// thread (D34): it calls [`spherecut_core::may_block`] first, so on the
/// UI thread it panics in a debug build.
pub fn verify_companion(report: &ProbeReport, own: &Path, other: Source<'_>) -> Companion {
    spherecut_core::may_block("comparing an Insta360 file with its companion");
    let lrv_is_other = companions(&report.path)
        .name
        .is_none_or(|n| n.role != NameRole::Proxy);
    let (this, that) = if lrv_is_other {
        ("master", "LRV")
    } else {
        ("LRV", "master")
    };
    let pairing = trailer_of(own, this).and_then(|(mut a, at)| {
        let (mut b, bt) = trailer_of(other.open, that)?;
        let pairing = if lrv_is_other {
            spherecut_camera::insta360::verify_companion((&mut a, &at), (&mut b, &bt), MAX_EXPOSURE)
        } else {
            spherecut_camera::insta360::verify_companion((&mut b, &bt), (&mut a, &at), MAX_EXPOSURE)
        };
        pairing.map_err(|e| format!("the exposure records (4) can't be compared: {e}"))
    });
    let (verified, why) = match pairing {
        Ok(Pairing::Verified) => (true, None),
        Ok(Pairing::NameOnly { why }) => (false, Some(why)),
        Err(why) => (false, Some(why)),
    };
    Companion {
        path: other.name.to_path_buf(),
        verified,
        why,
    }
}

/// The file at `open` with its trailer; why not, in words naming it as
/// the `what` of the pair, when it has none or it can't be read.
fn trailer_of(open: &Path, what: &str) -> Result<(File, Trailer), String> {
    let mut f = File::open(open).map_err(|e| format!("the {what} can't be opened: {e}"))?;
    let len = f
        .metadata()
        .map_err(|e| format!("the {what} can't be read: {e}"))?
        .len();
    match read_trailer(&mut f, len) {
        Ok(Some(t)) => Ok((f, t)),
        Ok(None) => Err(format!("the {what} has no Insta360 trailer")),
        Err(e) => Err(format!("the {what}'s trailer can't be read: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use spherecut_testkit::control::must_fail;

    use super::*;

    /// The warnings [`read`] gives for an Insta360 master at `open`, said
    /// to be `len` bytes long, with what it found, which is nothing.
    fn warned(open: &Path, len: u64) -> Vec<String> {
        let mut guess = CameraGuess {
            vendor: Some("Insta360".into()),
            model: None,
            firmware: None,
            serial: None,
            layout: Layout::Insta360DualFisheye,
            evidence: Vec::new(),
        };
        let mut warnings = Vec::new();
        let found = read(open, len, &mut guess, &[], &mut warnings);
        assert_eq!(found, CameraFile::default(), "{}", open.display());
        assert!(warnings.iter().all(|w| w.code == codes::BAD_TRAILER));
        warnings.into_iter().map(|w| w.message).collect()
    }

    /// A file that can't be opened again for its trailer is warned of in
    /// words of its own, not as an I/O error in the trailer's tail at byte
    /// 0, which nothing read; one that opens and can't be read, a
    /// directory here, is the trailer's, at the tail's byte.
    ///
    /// Control: the directory's warning isn't the open's.
    #[test]
    fn a_file_that_cant_be_opened_is_said_so() {
        let gone =
            std::env::temp_dir().join(format!("spherecut-camera-gone-{}.insv", std::process::id()));
        let error = File::open(&gone).unwrap_err();
        assert_eq!(
            warned(&gone, 4096),
            [format!(
                "the file can't be opened to read its Insta360 trailer: {error}"
            )]
        );
        let dir = warned(&std::env::temp_dir(), 4096);
        assert_eq!(dir.len(), 1, "{dir:?}");
        assert!(
            dir[0].starts_with(
                "the Insta360 trailer can't be read: I/O error reading the trailer's tail at byte 4024: "
            ),
            "{dir:?}"
        );
        must_fail(
            "a directory, warned of as a file that can't be opened",
            dir[0].starts_with("the file can't be opened"),
        );
    }

    /// A camera the table says isn't stitchable has its refusal kept for
    /// `doctor --file`, in the words [`Known::refusal`] gives frame's;
    /// no entry says so until M3.6's fit, so the entry is made by hand.
    ///
    /// Control: the X5's own entry, which the table stitches, keeps none.
    #[test]
    fn bind_keeps_the_table_s_refusal() {
        let x5 = spherecut_camera::insta360::known::lookup("Insta360 X5", "v1.1.22_build1");
        let refused = Known {
            stitchable: false,
            ..x5.clone()
        };
        let kept = |known: &Known| {
            let mut out = CameraFile::default();
            bind(&mut out, known);
            out.table_refusal
        };
        let why = kept(&refused).expect("the refusal is kept");
        assert_eq!(Some(why.clone()), refused.refusal());
        assert!(why.contains("X5"), "{why}");
        must_fail("the X5's own entry keeps a refusal", kept(&x5).is_some());
    }
}
