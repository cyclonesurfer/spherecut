//! Rewrite an MP4's sample entries to add or replace metadata boxes
//! (`st3d`/`sv3d` on video, `SA3D` on audio), without touching the media
//! data.
//!
//! Only `moov` changes size. Its ancestors' size fields are patched in
//! place, and when `moov` sits before `mdat` ("faststart") every chunk
//! offset that points past `moov` is shifted by the size change.
//!
//! The output is written to a temporary file next to it and renamed into
//! place (`spherecut_core::fs::TempFile`, D16), so it is always either
//! absent, the old file or the whole new one. A symlinked output is
//! written through: the file it points to is replaced and the link is
//! kept. An output that is not a regular file (a directory, pipe, device
//! or socket) is refused.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use spherecut_core::fs::{TempFile, follow_links};
use thiserror::Error;

use crate::mp4::{self, Mp4Box, Mp4Error, Mp4Tree};

/// Errors name the file they are about: the input, or the real output,
/// never the temporary file it is written as.
#[derive(Debug, Error)]
pub enum InjectError {
    #[error("{}: {error}", path.display())]
    Mp4 { path: PathBuf, error: Mp4Error },
    #[error("reading {}: {error}", path.display())]
    Read { path: PathBuf, error: io::Error },
    #[error("writing {}: {error}", path.display())]
    Write { path: PathBuf, error: io::Error },
    /// Copies read one file and write the other; the error can be either.
    #[error("copying {} to {}: {error}", input.display(), output.display())]
    Copy {
        input: PathBuf,
        output: PathBuf,
        error: io::Error,
    },
    #[error("{0}")]
    Unsupported(String),
    #[error("{} and {} are the same file", input.display(), output.display())]
    SameFile { input: PathBuf, output: PathBuf },
    #[error("two edits for the {0:?} track; put all its boxes in one edit")]
    DuplicateEdit(TrackKind),
    /// Fragments carry data offsets of their own (`tfhd`, `trun`, `sidx`,
    /// `tfra`), which we don't shift.
    #[error("fragmented MP4 (it has a `{0}` box) is not supported")]
    Fragmented(String),
    /// The input did not parse cleanly. A partial parse can miss the
    /// `mdat` after `moov`, or a track, and leave chunk offsets wrong.
    #[error("{} is damaged: {}", path.display(), damage.join("; "))]
    Damaged { path: PathBuf, damage: Vec<String> },
}

/// Which track's first sample entry to edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
}

/// One edit: boxes to append to a track's sample entry, replacing any
/// existing children with the same types.
#[derive(Debug, Clone)]
pub struct EntryEdit {
    pub track: TrackKind,
    /// Concatenated boxes to add.
    pub boxes: Vec<u8>,
}

fn handler_type(trak: &Mp4Box) -> Option<[u8; 4]> {
    let h = trak.path(&[b"mdia", b"hdlr"])?;
    h.payload.get(8..12)?.try_into().ok()
}

fn first_track(tree: &Mp4Tree, kind: TrackKind) -> Option<usize> {
    let want = match kind {
        TrackKind::Video => b"vide",
        TrackKind::Audio => b"soun",
    };
    tree.tracks()
        .position(|t| handler_type(t).as_ref() == Some(want))
}

/// Box types at the top level of a list of concatenated boxes.
fn box_types(boxes: &[u8]) -> Result<Vec<[u8; 4]>, InjectError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < boxes.len() {
        let size = boxes
            .get(pos..pos + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_be_bytes)
            .ok_or_else(|| InjectError::Unsupported("malformed box to inject".into()))?
            as usize;
        let t: [u8; 4] = boxes
            .get(pos + 4..pos + 8)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| InjectError::Unsupported("malformed box to inject".into()))?;
        if size < 8 || pos + size > boxes.len() {
            return Err(InjectError::Unsupported("malformed box to inject".into()));
        }
        out.push(t);
        pos += size;
    }
    Ok(out)
}

/// Boxes that only fragmented files have: at the top level, and in `moov`.
const FRAGMENT_TOP: &[&[u8; 4]] = &[b"moof", b"sidx", b"ssix", b"mfra"];
const FRAGMENT_MOOV: &[&[u8; 4]] = &[b"mvex"];

/// Refuse an `output` that is `input` under any name (the same path, a
/// hard link, a symlink): we would read the input while overwriting it.
fn check_not_same_file(input: &Path, output: &Path) -> Result<(), InjectError> {
    let a = std::fs::metadata(input).map_err(read_err(input))?;
    match std::fs::metadata(output) {
        Ok(b) if (a.dev(), a.ino()) == (b.dev(), b.ino()) => Err(InjectError::SameFile {
            input: input.to_path_buf(),
            output: output.to_path_buf(),
        }),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(write_err(output)(e)),
    }
}

fn read_err(path: &Path) -> impl Fn(io::Error) -> InjectError + '_ {
    move |error| InjectError::Read {
        path: path.to_path_buf(),
        error,
    }
}

fn write_err(path: &Path) -> impl Fn(io::Error) -> InjectError + '_ {
    move |error| InjectError::Write {
        path: path.to_path_buf(),
        error,
    }
}

/// Write `input` to `output` with the edits applied, at most one edit per
/// track. `output` must not be `input`, and `input` must parse cleanly:
/// one with [`Mp4Tree::damage`] is refused.
///
/// `output` is replaced atomically, by renaming a new file over it. The new
/// file keeps the old one's permission bits, but not its owner, ACLs or
/// extended attributes. If `output` is a symlink, the file it points to is
/// replaced and the link is kept, as writing the file in place would. A
/// directory, pipe, device or socket at `output` is refused.
pub fn inject(input: &Path, output: &Path, edits: &[EntryEdit]) -> Result<(), InjectError> {
    spherecut_core::may_block("rewriting an MP4 file");
    check_not_same_file(input, output)?;
    let dest = follow_links(output).map_err(write_err(output))?;
    let mut tmp = TempFile::create(&dest).map_err(write_err(&dest))?;
    inject_into(input, &mut tmp, edits)?;
    tmp.persist_replace().map_err(write_err(&dest))?;
    Ok(())
}

/// [`inject`]'s rewrite of `input`, written into `out`, a temporary file
/// for the output that the caller persists (as new, or replacing).
pub fn inject_into(
    input: &Path,
    out: &mut TempFile,
    edits: &[EntryEdit],
) -> Result<(), InjectError> {
    spherecut_core::may_block("rewriting an MP4 file");
    for (i, e) in edits.iter().enumerate() {
        if edits[..i].iter().any(|o| o.track == e.track) {
            return Err(InjectError::DuplicateEdit(e.track));
        }
    }
    let tree = mp4::read_file(input).map_err(|error| InjectError::Mp4 {
        path: input.to_path_buf(),
        error,
    })?;
    let moov = tree
        .moov()
        .ok_or_else(|| InjectError::Unsupported("no moov box".into()))?;
    let fragment = tree
        .top
        .iter()
        .find(|b| FRAGMENT_TOP.contains(&&b.fourcc.0))
        .or_else(|| {
            moov.children
                .iter()
                .find(|b| FRAGMENT_MOOV.contains(&&b.fourcc.0))
        });
    if let Some(b) = fragment {
        return Err(InjectError::Fragmented(b.fourcc.to_string()));
    }
    let damage = tree.damage();
    if !damage.is_empty() {
        return Err(InjectError::Damaged {
            path: input.to_path_buf(),
            damage,
        });
    }
    let mut f = File::open(input).map_err(read_err(input))?;
    f.seek(SeekFrom::Start(moov.offset))
        .map_err(read_err(input))?;
    let mut moov_bytes = vec![0u8; moov.size as usize];
    f.read_exact(&mut moov_bytes).map_err(read_err(input))?;

    // Byte ranges are relative to the start of moov. Apply edits from the
    // highest offset down so earlier offsets stay valid.
    struct Splice {
        start: usize,
        end: usize,
        replacement: Vec<u8>,
        ancestors: Vec<(usize, u64)>, // (offset, header_len)
    }
    let mut splices = Vec::new();
    for e in edits {
        let ti = first_track(&tree, e.track)
            .ok_or_else(|| InjectError::Unsupported(format!("no {:?} track to edit", e.track)))?;
        let trak = tree
            .tracks()
            .nth(ti)
            .ok_or_else(|| InjectError::Unsupported("track vanished".into()))?;
        let mdia = trak.child(b"mdia");
        let minf = mdia.and_then(|b| b.child(b"minf"));
        let stbl = minf.and_then(|b| b.child(b"stbl"));
        let stsd = stbl.and_then(|b| b.child(b"stsd"));
        let entry = stsd.and_then(|b| b.children.first());
        let (Some(mdia), Some(minf), Some(stbl), Some(stsd), Some(entry)) =
            (mdia, minf, stbl, stsd, entry)
        else {
            return Err(InjectError::Unsupported(
                "track has no sample description".into(),
            ));
        };
        if mp4::sample_entry_fixed_len(entry.fourcc, &entry.payload) != Some(entry.payload.len()) {
            return Err(InjectError::Unsupported(format!(
                "sample entry `{}` is not a type we can edit",
                entry.fourcc
            )));
        }
        let replace = box_types(&e.boxes)?;
        let rel = |b: &Mp4Box| (b.offset - moov.offset) as usize;
        // Rebuild the entry: header + fixed fields + kept children + new.
        let start = rel(entry);
        let fixed_end = start + entry.header_len as usize + entry.payload.len();
        let mut new_entry = moov_bytes[start..fixed_end].to_vec();
        // QuickTime's terminator stays last, the only place Mp4Tree::damage
        // allows a zero type. As eight zero bytes it has size 0 and hides
        // whatever follows it from FFmpeg. Bytes after the last whole child
        // (a four-byte zero tail) are dropped.
        let mut terminator: &[u8] = &[];
        for c in &entry.children {
            let cs = rel(c);
            let bytes = &moov_bytes[cs..cs + c.size as usize];
            if c.fourcc.0 == [0; 4] {
                terminator = bytes;
            } else if !replace.contains(&c.fourcc.0) {
                new_entry.extend_from_slice(bytes);
            }
        }
        new_entry.extend_from_slice(&e.boxes);
        new_entry.extend_from_slice(terminator);
        splices.push(Splice {
            start,
            end: start + entry.size as usize,
            replacement: new_entry,
            ancestors: [moov, trak, mdia, minf, stbl, stsd, entry]
                .iter()
                .map(|b| (rel(b), b.header_len))
                .collect(),
        });
    }
    splices.sort_by_key(|s| std::cmp::Reverse(s.start));
    let mut total_delta: i64 = 0;
    for s in &splices {
        let delta = s.replacement.len() as i64 - (s.end - s.start) as i64;
        // Patch ancestor sizes (including the entry itself, which is
        // rewritten wholesale below, so skip it).
        for &(off, hlen) in &s.ancestors[..s.ancestors.len() - 1] {
            patch_size(&mut moov_bytes, off, hlen, delta)?;
        }
        moov_bytes.splice(s.start..s.end, s.replacement.iter().copied());
        // The rebuilt entry carries its old size; fix it.
        patch_size(
            &mut moov_bytes,
            s.start,
            s.ancestors[s.ancestors.len() - 1].1,
            delta,
        )?;
        total_delta += delta;
    }

    // If media data follows moov, shift chunk offsets.
    let moov_end = moov.offset + moov.size;
    let data_after = tree
        .top
        .iter()
        .any(|b| b.fourcc.0 == *b"mdat" && b.offset >= moov_end);
    if data_after && total_delta != 0 {
        shift_chunk_offsets(&mut moov_bytes, moov_end, total_delta)?;
    }

    let dest = out.dest().to_path_buf();
    let copy_err = |error| InjectError::Copy {
        input: input.to_path_buf(),
        output: dest.clone(),
        error,
    };
    let mut out = BufWriter::new(out.file_mut());
    // Copy exactly the lengths the parse saw, never "to the end": the
    // output can't outgrow the input, whatever happens to the input file.
    f.seek(SeekFrom::Start(0)).map_err(read_err(input))?;
    copy_exact(&mut f, &mut out, moov.offset).map_err(copy_err)?;
    out.write_all(&moov_bytes).map_err(write_err(&dest))?;
    f.seek(SeekFrom::Start(moov_end)).map_err(read_err(input))?;
    copy_exact(&mut f, &mut out, tree.file_size.saturating_sub(moov_end)).map_err(copy_err)?;
    out.flush().map_err(write_err(&dest))?;
    Ok(())
}

fn copy_exact(from: &mut File, to: &mut impl Write, len: u64) -> io::Result<()> {
    let n = io::copy(&mut from.take(len), to)?;
    if n != len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("the input shrank while copying ({n} of {len} bytes)"),
        ));
    }
    Ok(())
}

fn patch_size(buf: &mut [u8], off: usize, header_len: u64, delta: i64) -> Result<(), InjectError> {
    let small = u32::from_be_bytes(
        buf.get(off..off + 4)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| InjectError::Unsupported("size field out of range".into()))?,
    );
    let large = small == 1 && header_len >= 16;
    if large {
        let cur = u64::from_be_bytes(
            buf.get(off + 8..off + 16)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| InjectError::Unsupported("size field out of range".into()))?,
        );
        let new = cur
            .checked_add_signed(delta)
            .ok_or_else(|| InjectError::Unsupported("box size underflow".into()))?;
        buf[off + 8..off + 16].copy_from_slice(&new.to_be_bytes());
    } else {
        if small == 0 {
            return Err(InjectError::Unsupported(
                "box with size 0 (to end of file) inside moov".into(),
            ));
        }
        let new = (small as i64 + delta)
            .try_into()
            .map_err(|_| InjectError::Unsupported("box grows past 4 GiB".into()))?;
        buf[off..off + 4].copy_from_slice(&u32::to_be_bytes(new));
    }
    Ok(())
}

/// Shift every `stco`/`co64` entry that points at or past `moov_end`.
fn shift_chunk_offsets(
    moov_bytes: &mut [u8],
    moov_end: u64,
    delta: i64,
) -> Result<(), InjectError> {
    // Offsets in this tree are relative to the start of moov.
    let tree = mp4::read(&mut io::Cursor::new(moov_bytes.to_vec()))
        .map_err(|e| InjectError::Unsupported(format!("re-reading the edited moov: {e}")))?;
    // A parse that stopped early (the edits can take it past MAX_BOXES)
    // would miss chunk tables.
    if !tree.warnings.is_empty() {
        return Err(InjectError::Unsupported(format!(
            "re-reading the edited moov: {}",
            tree.warnings.join("; ")
        )));
    }
    let moov = tree
        .moov()
        .ok_or_else(|| InjectError::Unsupported("moov vanished".into()))?;
    for trak in moov.children_of(b"trak") {
        let Some(stbl) = trak.path(&[b"mdia", b"minf", b"stbl"]) else {
            continue;
        };
        for (t, wide) in [(b"stco", false), (b"co64", true)] {
            let Some(b) = stbl.child(t) else { continue };
            let base = (b.offset + b.header_len) as usize + 4; // version/flags
            let count = u32::from_be_bytes(
                moov_bytes
                    .get(base..base + 4)
                    .and_then(|x| x.try_into().ok())
                    .ok_or_else(|| InjectError::Unsupported("bad chunk table".into()))?,
            ) as usize;
            let w = if wide { 8 } else { 4 };
            for i in 0..count {
                let at = base + 4 + i * w;
                let field = moov_bytes
                    .get_mut(at..at + w)
                    .ok_or_else(|| InjectError::Unsupported("chunk table overruns".into()))?;
                let cur = if wide {
                    u64::from_be_bytes(field.try_into().unwrap_or([0; 8]))
                } else {
                    u32::from_be_bytes(field.try_into().unwrap_or([0; 4])) as u64
                };
                if cur < moov_end {
                    continue;
                }
                let new = cur
                    .checked_add_signed(delta)
                    .ok_or_else(|| InjectError::Unsupported("chunk offset underflow".into()))?;
                if wide {
                    field.copy_from_slice(&new.to_be_bytes());
                } else {
                    let n: u32 = new.try_into().map_err(|_| {
                        InjectError::Unsupported("chunk offset past 4 GiB in a 32-bit stco".into())
                    })?;
                    field.copy_from_slice(&n.to_be_bytes());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::mp4::tests::{bx, cat, full};
    use crate::spherical::{self, StereoMode, write};

    fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        p.extend_from_slice(kind);
        p.extend_from_slice(&[0u8; 12]);
        p.extend_from_slice(b"Handler\0");
        full(b"hdlr", 0, 0, &p)
    }

    fn words(w: &[u32]) -> Vec<u8> {
        w.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    /// A track with one 9-byte sample in one chunk at `chunk_offset`.
    fn trak(kind: &[u8; 4], entry: Vec<u8>, chunk_offset: u32) -> Vec<u8> {
        let stsd = full(b"stsd", 0, 0, &cat(&[words(&[1]), entry]));
        let stts = full(b"stts", 0, 0, &words(&[1, 1, 1]));
        let stsz = full(b"stsz", 0, 0, &words(&[0, 1, 9]));
        let stsc = full(b"stsc", 0, 0, &words(&[1, 1, 1, 1]));
        let stco = full(b"stco", 0, 0, &words(&[1, chunk_offset]));
        let stbl = bx(b"stbl", &cat(&[stsd, stts, stsz, stsc, stco]));
        bx(
            b"trak",
            &bx(b"mdia", &cat(&[hdlr(kind), bx(b"minf", &stbl)])),
        )
    }

    /// A file with video+audio tracks whose single chunks hold known bytes.
    fn file(faststart: bool) -> (Vec<u8>, usize) {
        file_padded(faststart, 0, 0)
    }

    /// [`file`] with `before` and `after` empty `free` boxes in `moov`,
    /// before and after its tracks.
    fn file_padded(faststart: bool, before: usize, after: usize) -> (Vec<u8>, usize) {
        let ventry = bx(b"hvc1", &cat(&[vec![0u8; 78], bx(b"hvcC", &[9; 5])]));
        file_with_entry(faststart, before, after, ventry)
    }

    /// [`file_padded`] with `ventry` as the video sample entry.
    fn file_with_entry(
        faststart: bool,
        before: usize,
        after: usize,
        ventry: Vec<u8>,
    ) -> (Vec<u8>, usize) {
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let aentry = bx(b"mp4a", &cat(&[vec![0u8; 28], bx(b"esds", &[7; 5])]));
        let payload = b"VIDEODATAAUDIODATA".to_vec();
        let free = bx(b"free", &[]);
        let moov = |data_start: usize| {
            bx(
                b"moov",
                &cat(&[
                    free.repeat(before),
                    trak(b"vide", ventry.clone(), data_start as u32),
                    trak(b"soun", aentry.clone(), (data_start + 9) as u32),
                    free.repeat(after),
                ]),
            )
        };
        // Compute layout: moov size doesn't depend on offset values.
        let mdat_header = 8;
        let data_start = if faststart {
            ftyp.len() + moov(0).len() + mdat_header
        } else {
            ftyp.len() + mdat_header
        };
        let moov = moov(data_start);
        let mdat = bx(b"mdat", &payload);
        let f = if faststart {
            cat(&[ftyp, moov, mdat])
        } else {
            cat(&[ftyp, mdat, moov])
        };
        (f, data_start)
    }

    fn chunk_offsets(tree: &Mp4Tree) -> Vec<u32> {
        tree.tracks()
            .filter_map(|t| t.path(&[b"mdia", b"minf", b"stbl", b"stco"]))
            .map(|b| u32::from_be_bytes(b.payload[8..12].try_into().unwrap()))
            .collect()
    }

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

    fn tmpdir(name: &str) -> TempDir {
        let path =
            std::env::temp_dir().join(format!("spherecut-inject-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    #[test]
    fn test_directories_are_removed_when_dropped() {
        let dir = tmpdir("dropped");
        std::fs::write(dir.join("left.mp4"), b"").unwrap();
        let path = dir.to_path_buf();
        drop(dir);
        assert!(!path.exists());
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

    fn spherical_edits() -> [EntryEdit; 2] {
        [
            EntryEdit {
                track: TrackKind::Video,
                boxes: cat(&[write::st3d(StereoMode::Mono), write::sv3d_equirect("test")]),
            },
            EntryEdit {
                track: TrackKind::Audio,
                boxes: write::sa3d_first_order_ambix(),
            },
        ]
    }

    fn run(faststart: bool) {
        let dir = tmpdir(&format!("run-{faststart}"));
        let (bytes, _) = file(faststart);
        let a = dir.join("in.mp4");
        let b = dir.join("out.mp4");
        let c = dir.join("out2.mp4");
        std::fs::write(&a, &bytes).unwrap();
        // An existing output is replaced.
        std::fs::write(&b, b"old contents").unwrap();
        let edits = spherical_edits();
        inject(&a, &b, &edits).unwrap();
        // Injecting again replaces rather than duplicates.
        inject(&b, &c, &edits).unwrap();
        let out = std::fs::read(&c).unwrap();
        assert_eq!(out.len(), std::fs::read(&b).unwrap().len());
        let tree = mp4::read(&mut io::Cursor::new(out.clone())).unwrap();
        assert!(tree.warnings.is_empty(), "{:?}", tree.warnings);
        let sph = spherical::read_tracks(&tree);
        assert!(sph[0].v2.is_some());
        assert!(sph[1].spatial_audio.as_ref().unwrap().is_ambix());
        let ventry = tree
            .tracks()
            .next()
            .unwrap()
            .path(&[b"mdia", b"minf", b"stbl", b"stsd", b"hvc1"])
            .unwrap();
        let kinds: Vec<String> = ventry
            .children
            .iter()
            .map(|c| c.fourcc.to_string())
            .collect();
        assert_eq!(kinds, ["hvcC", "st3d", "sv3d"]);
        // Chunk offsets still point at the right bytes.
        let offs = chunk_offsets(&tree);
        assert_eq!(&out[offs[0] as usize..offs[0] as usize + 9], b"VIDEODATA");
        assert_eq!(&out[offs[1] as usize..offs[1] as usize + 9], b"AUDIODATA");
        // No temp files left behind.
        assert_eq!(listing(&dir), ["in.mp4", "out.mp4", "out2.mp4"]);
    }

    #[test]
    fn inject_with_moov_at_end() {
        run(false);
    }

    #[test]
    fn a_terminator_stays_last_in_its_entry() {
        // Boxes added after QuickTime's terminator would be lost: FFmpeg
        // stops reading the entry there. Eight zero bytes read as a box
        // that runs to the end of the entry.
        let dir = tmpdir("terminator");
        let ventry = bx(
            b"hvc1",
            &cat(&[vec![0u8; 78], bx(b"hvcC", &[9; 5]), vec![0; 8]]),
        );
        for faststart in [false, true] {
            let (bytes, _) = file_with_entry(faststart, 0, 0, ventry.clone());
            let (a, b) = (dir.join("in.mp4"), dir.join("out.mp4"));
            std::fs::write(&a, &bytes).unwrap();
            inject(&a, &b, &spherical_edits()).unwrap();
            let tree = mp4::read_file(&b).unwrap();
            assert!(tree.damage().is_empty(), "{:?}", tree.damage());
            assert!(spherical::read_tracks(&tree)[0].v2.is_some());
            let hvc1 = tree.tracks().next().unwrap();
            let hvc1 = hvc1.path(&[b"mdia", b"minf", b"stbl", b"stsd", b"hvc1"]);
            let kinds: Vec<[u8; 4]> = hvc1.unwrap().children.iter().map(|c| c.fourcc.0).collect();
            assert_eq!(kinds, [*b"hvcC", *b"st3d", *b"sv3d", [0; 4]]);
        }
    }

    #[test]
    fn inject_with_faststart_shifts_chunk_offsets() {
        run(true);
    }

    #[test]
    fn same_file_under_any_name_is_refused_and_left_untouched() {
        // Writing over the input used to grow it until the disk was full.
        // The output is never written in place and copies are bounded by
        // the parsed size now, so even a regression here can't run away.
        let dir = tmpdir("same");
        let (bytes, _) = file(true);
        let a = dir.join("in.mp4");
        std::fs::write(&a, &bytes).unwrap();
        let hard = dir.join("hard.mp4");
        std::fs::hard_link(&a, &hard).unwrap();
        let sym = dir.join("sym.mp4");
        std::os::unix::fs::symlink(&a, &sym).unwrap();
        let dotted = dir.join(".").join("in.mp4");
        for (input, output) in [(&a, &a), (&a, &dotted), (&a, &hard), (&a, &sym), (&sym, &a)] {
            match inject(input, output, &spherical_edits()) {
                Err(InjectError::SameFile { .. }) => {}
                r => panic!("{} -> {}: {r:?}", input.display(), output.display()),
            }
            assert!(std::fs::read(&a).unwrap() == bytes, "input changed");
        }
        assert_eq!(listing(&dir), ["hard.mp4", "in.mp4", "sym.mp4"]);
    }

    #[test]
    fn two_edits_for_one_track_are_refused() {
        let dir = tmpdir("dup");
        let a = dir.join("in.mp4");
        std::fs::write(&a, file(false).0).unwrap();
        let [v, _] = spherical_edits();
        let edits = [
            v.clone(),
            EntryEdit {
                track: TrackKind::Audio,
                boxes: write::sa3d_first_order_ambix(),
            },
            v,
        ];
        let r = inject(&a, &dir.join("out.mp4"), &edits);
        assert!(
            matches!(r, Err(InjectError::DuplicateEdit(TrackKind::Video))),
            "{r:?}"
        );
        assert_eq!(listing(&dir), ["in.mp4"]);
    }

    #[test]
    fn fragmented_files_are_refused() {
        let dir = tmpdir("frag");
        let ftyp = bx(b"ftyp", b"iso5\0\0\x02\0iso5");
        let ventry = bx(b"hvc1", &cat(&[vec![0u8; 78], bx(b"hvcC", &[9; 5])]));
        let moov = |extra: Vec<u8>| bx(b"moov", &cat(&[trak(b"vide", ventry.clone(), 0), extra]));
        let mvex = bx(b"mvex", &full(b"trex", 0, 0, &[0; 20]));
        let moof = bx(b"moof", &full(b"mfhd", 0, 0, &1u32.to_be_bytes()));
        let cases = [
            ("mvex", cat(&[ftyp.clone(), moov(mvex)])),
            (
                "moof",
                cat(&[ftyp.clone(), moov(vec![]), moof, bx(b"mdat", b"x")]),
            ),
            (
                "sidx",
                cat(&[ftyp.clone(), moov(vec![]), full(b"sidx", 0, 0, &[0; 24])]),
            ),
            (
                "mfra",
                cat(&[
                    ftyp.clone(),
                    moov(vec![]),
                    bx(b"mfra", &full(b"tfra", 0, 0, &[0; 12])),
                ]),
            ),
        ];
        let a = dir.join("in.mp4");
        let out = dir.join("out.mp4");
        for (name, bytes) in cases {
            std::fs::write(&a, &bytes).unwrap();
            match inject(&a, &out, &spherical_edits()[..1]) {
                Err(InjectError::Fragmented(t)) => assert_eq!(t, name),
                r => panic!("{name}: {r:?}"),
            }
            assert!(!out.exists());
        }
    }

    #[test]
    fn damaged_inputs_are_refused() {
        // A partial parse used to drive the copy: with more boxes in `moov`
        // than the parser reads, it never saw the `mdat` after it, left
        // every chunk offset unshifted, and returned Ok.
        let dir = tmpdir("damaged");
        let a = dir.join("in.mp4");
        let out = dir.join("out.mp4");
        let (padded, _) = file_padded(true, 0, mp4::MAX_BOXES);
        let (whole, _) = file(true);
        // A cut-off `mdat` is not in the tree either.
        let cut = whole[..whole.len() - 4].to_vec();
        for (bytes, why) in [
            (padded, format!("more than {} boxes", mp4::MAX_BOXES)),
            (cut, "it was cut short".to_string()),
        ] {
            std::fs::write(&a, &bytes).unwrap();
            match inject(&a, &out, &spherical_edits()) {
                Err(e @ InjectError::Damaged { .. }) => {
                    let e = e.to_string();
                    assert!(e.contains(&*a.to_string_lossy()) && e.contains(&why), "{e}")
                }
                r => panic!("{why}: {r:?}"),
            }
        }
        assert_eq!(listing(&dir), ["in.mp4"]);
    }

    #[test]
    fn edits_that_take_moov_past_the_box_limit_are_refused() {
        // The input parses cleanly at exactly MAX_BOXES boxes, with the
        // padding ahead of the tracks. The edits add boxes, so re-reading
        // the edited `moov` would stop before the audio track's `stco`
        // and leave its chunk offset unshifted.
        fn count(b: &Mp4Box) -> usize {
            1 + b.children.iter().map(count).sum::<usize>()
        }
        let boxes = |bytes: &[u8]| {
            let tree = mp4::read(&mut io::Cursor::new(bytes.to_vec())).unwrap();
            assert!(tree.damage().is_empty(), "{:?}", tree.damage());
            tree.top.iter().map(count).sum::<usize>()
        };
        let pad = mp4::MAX_BOXES - boxes(&file(true).0);
        let (bytes, _) = file_padded(true, pad, 0);
        assert_eq!(boxes(&bytes), mp4::MAX_BOXES);
        let dir = tmpdir("box-limit");
        let a = dir.join("in.mp4");
        std::fs::write(&a, &bytes).unwrap();
        match inject(&a, &dir.join("out.mp4"), &spherical_edits()) {
            Err(InjectError::Unsupported(e)) => {
                assert!(e.contains("re-reading the edited moov: more than"), "{e}")
            }
            r => panic!("{r:?}"),
        }
        assert_eq!(listing(&dir), ["in.mp4"]);
    }

    #[test]
    fn long_output_names_work() {
        let dir = tmpdir("long");
        let a = dir.join("in.mp4");
        std::fs::write(&a, file(false).0).unwrap();
        let name = "o".repeat(251) + ".mp4";
        inject(&a, &dir.join(&name), &spherical_edits()).unwrap();
        assert_eq!(listing(&dir), ["in.mp4".to_string(), name]);
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn replaced_outputs_keep_their_permissions() {
        let dir = tmpdir("mode");
        let a = dir.join("in.mp4");
        std::fs::write(&a, file(false).0).unwrap();
        let out = dir.join("out.mp4");
        for m in [0o600, 0o640, 0o755, 0o444] {
            std::fs::remove_file(&out).ok();
            std::fs::write(&out, b"old").unwrap();
            std::fs::set_permissions(&out, Permissions::from_mode(m)).unwrap();
            // The temp file has them from the start.
            let tmp = TempFile::create(&out).unwrap();
            assert_eq!(mode(tmp.path()), m);
            drop(tmp);
            inject(&a, &out, &spherical_edits()).unwrap();
            assert_eq!(mode(&out), m, "{m:o}");
            assert_ne!(std::fs::read(&out).unwrap(), b"old");
        }
        // A new output gets the mode any new file would.
        std::fs::remove_file(&out).unwrap();
        inject(&a, &out, &spherical_edits()).unwrap();
        let fresh = dir.join("fresh");
        std::fs::write(&fresh, b"").unwrap();
        assert_eq!(mode(&out), mode(&fresh));
        assert_eq!(listing(&dir), ["fresh", "in.mp4", "out.mp4"]);
    }

    #[test]
    fn symlinked_outputs_are_written_through() {
        use std::os::unix::fs::symlink;
        let dir = tmpdir("symlink");
        let a = dir.join("in.mp4");
        std::fs::write(&a, file(false).0).unwrap();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let target = sub.join("target.mp4");
        std::fs::write(&target, b"old").unwrap();
        // link.mp4 -> sub/hop.mp4 -> target.mp4, with relative targets.
        symlink("sub/hop.mp4", dir.join("link.mp4")).unwrap();
        symlink("target.mp4", sub.join("hop.mp4")).unwrap();
        // A dangling link names the file to create.
        symlink("sub/new.mp4", dir.join("dangling.mp4")).unwrap();
        for (link, file) in [
            ("link.mp4", &target),
            ("dangling.mp4", &sub.join("new.mp4")),
        ] {
            inject(&a, &dir.join(link), &spherical_edits()).unwrap();
            let tree = mp4::read_file(file).unwrap();
            assert!(tree.warnings.is_empty(), "{:?}", tree.warnings);
            assert!(spherical::read_tracks(&tree)[0].v2.is_some());
        }
        for link in [
            dir.join("link.mp4"),
            sub.join("hop.mp4"),
            dir.join("dangling.mp4"),
        ] {
            assert!(link.symlink_metadata().unwrap().is_symlink());
        }
        assert_eq!(listing(&dir), ["dangling.mp4", "in.mp4", "link.mp4", "sub"]);
        assert_eq!(listing(&sub), ["hop.mp4", "new.mp4", "target.mp4"]);

        // A loop is an error, not a hang.
        symlink("loop-b.mp4", dir.join("loop-a.mp4")).unwrap();
        symlink("loop-a.mp4", dir.join("loop-b.mp4")).unwrap();
        match inject(&a, &dir.join("loop-a.mp4"), &spherical_edits()) {
            Err(e @ InjectError::Write { .. }) => {
                assert!(e.to_string().contains("symbolic links"), "{e}")
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn errors_name_the_file_they_are_about() {
        let dir = tmpdir("names");
        let a = dir.join("in.mp4");
        let missing = dir.join("missing.mp4");
        let out = dir.join("out.mp4");
        match inject(&missing, &out, &spherical_edits()) {
            Err(e @ InjectError::Read { .. }) => {
                assert!(e.to_string().contains(&*missing.to_string_lossy()), "{e}")
            }
            r => panic!("{r:?}"),
        }
        std::fs::write(&a, file(false).0).unwrap();
        let nowhere = dir.join("no-such-dir").join("out.mp4");
        match inject(&a, &nowhere, &spherical_edits()) {
            Err(e @ InjectError::Write { .. }) => {
                assert!(e.to_string().contains(&*nowhere.to_string_lossy()), "{e}")
            }
            r => panic!("{r:?}"),
        }
        assert_eq!(listing(&dir), ["in.mp4"]);
    }

    #[test]
    fn failed_write_removes_its_temp_file() {
        let dir = tmpdir("fail");
        let a = dir.join("in.mp4");
        std::fs::write(&a, file(false).0).unwrap();
        // A directory in the way is refused before any work is done.
        let out = dir.join("out.mp4");
        std::fs::create_dir(&out).unwrap();
        let r = inject(&a, &out, &spherical_edits());
        match &r {
            Err(e @ InjectError::Write { path, .. }) => {
                // The output, not the temp file meant to replace it.
                assert_eq!(path, &out);
                assert!(!e.to_string().contains(".tmp"), "{e}");
            }
            r => panic!("{r:?}"),
        }
        assert_eq!(listing(&dir), ["in.mp4", "out.mp4"]);
        assert!(out.is_dir());
        // One that turns up while the file is written makes the rename
        // fail, and the temp file goes too.
        let late = dir.join("late.mp4");
        let tmp = TempFile::create(&late).unwrap();
        std::fs::create_dir(&late).unwrap();
        assert!(tmp.persist_replace().is_err());
        assert_eq!(listing(&dir), ["in.mp4", "late.mp4", "out.mp4"]);
    }

    /// Make a FIFO at `p`, or say why not.
    fn mkfifo(p: &Path) -> Result<(), String> {
        match std::process::Command::new("mkfifo").arg(p).status() {
            Ok(s) if s.success() => Ok(()),
            r => Err(format!("mkfifo {}: {r:?}", p.display())),
        }
    }

    /// Make a unix socket at `p`. The socket stays when its listener is
    /// closed. A socket's path must fit in sun_path's 108 bytes with its
    /// NUL, which a long TMPDIR can use up, so a longer one is made through
    /// a short path to the same directory: `/proc/self/fd/<n>`, where `n`
    /// holds the directory open.
    fn socket(p: &Path) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        let bind = |at: &Path| {
            std::os::unix::net::UnixListener::bind(at)
                .map(drop)
                .map_err(|e| format!("bind {}: {e}", at.display()))
        };
        if p.as_os_str().len() < 108 {
            return bind(p);
        }
        let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else {
            return Err(format!("{} has no directory and name", p.display()));
        };
        let dir = std::fs::File::open(dir).map_err(|e| format!("open {}: {e}", dir.display()))?;
        bind(&PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(name))
    }

    #[test]
    fn sockets_are_made_however_long_their_path() {
        use std::os::unix::fs::FileTypeExt;
        // A socket's path must fit in sun_path's 108 bytes, and a long
        // TMPDIR takes most of them. This one is too long with any TMPDIR.
        let dir = tmpdir("socket-path");
        let deep = dir.join("d".repeat(108));
        std::fs::create_dir(&deep).unwrap();
        let p = deep.join("socket");
        socket(&p).unwrap();
        assert!(p.symlink_metadata().unwrap().file_type().is_socket());
        assert_eq!(listing(&deep), ["socket"]);
        // Binding again fails, and the error names the path that was bound:
        // the short one under /proc here, and a path that fits as it is.
        let e = socket(&p).unwrap_err();
        assert!(e.starts_with("bind /proc/self/fd/"), "{e}");
        let short = dir.join("s");
        if short.as_os_str().len() < 108 {
            socket(&short).unwrap();
            let e = socket(&short).unwrap_err();
            assert!(e.starts_with(&format!("bind {}: ", short.display())), "{e}");
        }
    }

    #[test]
    fn outputs_that_are_not_regular_files_are_refused() {
        use std::os::unix::fs::FileTypeExt;
        // Renaming over a socket, a pipe or a device node (`/dev/null`,
        // say) would replace the node with an MP4 instead of writing to it.
        // A socket needs no tool to make, so it is always checked, and a
        // FIFO is too when mkfifo is there. Not /dev/null itself: if the
        // check broke, the test would replace the system's.
        let dir = tmpdir("special");
        let a = dir.join("in.mp4");
        std::fs::write(&a, file(false).0).unwrap();
        let mut left = vec!["in.mp4".to_string()];
        for kind in ["socket", "fifo"] {
            let make = |p: &Path| match kind {
                "socket" => socket(p),
                _ => mkfifo(p),
            };
            let is_kind = |p: &Path| {
                let t = p.metadata().unwrap().file_type();
                if kind == "socket" {
                    t.is_socket()
                } else {
                    t.is_fifo()
                }
            };
            let node = dir.join(kind);
            if let Err(e) = make(&node) {
                assert_eq!(kind, "fifo", "{e}");
                println!("FIFO not checked: {e}");
                continue;
            }
            // Through a symlink too.
            let link = dir.join(format!("{kind}-link.mp4"));
            std::os::unix::fs::symlink(&node, &link).unwrap();
            for out in [&node, &link] {
                match inject(&a, out, &spherical_edits()) {
                    Err(e @ InjectError::Write { .. }) => {
                        assert!(e.to_string().contains("not a regular file"), "{e}")
                    }
                    r => panic!("{}: {r:?}", out.display()),
                }
                assert!(is_kind(&node), "{}", node.display());
            }
            assert!(link.symlink_metadata().unwrap().is_symlink());
            // One that turns up while the file is written is refused too.
            let late = dir.join(format!("late-{kind}.mp4"));
            let tmp = TempFile::create(&late).unwrap();
            make(&late).unwrap();
            assert!(tmp.persist_replace().is_err());
            assert!(is_kind(&late), "{}", late.display());
            left.extend([
                kind.to_string(),
                format!("{kind}-link.mp4"),
                format!("late-{kind}.mp4"),
            ]);
        }
        // No temp files left behind.
        left.sort();
        assert_eq!(listing(&dir), left);
    }
}
