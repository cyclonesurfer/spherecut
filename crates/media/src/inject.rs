//! Rewrite an MP4's sample entries to add or replace metadata boxes
//! (`st3d`/`sv3d` on video, `SA3D` on audio), without touching the media
//! data.
//!
//! Only `moov` changes size. Its ancestors' size fields are patched in
//! place, and when `moov` sits before `mdat` ("faststart") every chunk
//! offset that points past `moov` is shifted by the size change.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use thiserror::Error;

use crate::mp4::{self, Mp4Box, Mp4Error, Mp4Tree};

#[derive(Debug, Error)]
pub enum InjectError {
    #[error(transparent)]
    Mp4(#[from] Mp4Error),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Unsupported(String),
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

/// Write `input` to `output` with the edits applied. `input` and `output`
/// must differ.
pub fn inject(input: &Path, output: &Path, edits: &[EntryEdit]) -> Result<(), InjectError> {
    let tree = mp4::read_file(input)?;
    let moov = tree
        .moov()
        .ok_or_else(|| InjectError::Unsupported("no moov box".into()))?;
    let mut f = File::open(input)?;
    f.seek(SeekFrom::Start(moov.offset))?;
    let mut moov_bytes = vec![0u8; moov.size as usize];
    f.read_exact(&mut moov_bytes)?;

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
        for c in &entry.children {
            if !replace.contains(&c.fourcc.0) {
                let cs = rel(c);
                new_entry.extend_from_slice(&moov_bytes[cs..cs + c.size as usize]);
            }
        }
        new_entry.extend_from_slice(&e.boxes);
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

    let mut out = BufWriter::new(File::create(output)?);
    f.seek(SeekFrom::Start(0))?;
    io::copy(&mut (&mut f).take(moov.offset), &mut out)?;
    out.write_all(&moov_bytes)?;
    f.seek(SeekFrom::Start(moov_end))?;
    io::copy(&mut f, &mut out)?;
    out.flush()?;
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
    let tree = mp4::read(&mut io::Cursor::new(moov_bytes.to_vec()))?;
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

    fn trak(kind: &[u8; 4], entry: Vec<u8>, chunk_offset: u32) -> Vec<u8> {
        let stsd = full(b"stsd", 0, 0, &cat(&[1u32.to_be_bytes().to_vec(), entry]));
        let stco = full(
            b"stco",
            0,
            0,
            &cat(&[
                1u32.to_be_bytes().to_vec(),
                chunk_offset.to_be_bytes().to_vec(),
            ]),
        );
        let stbl = bx(b"stbl", &cat(&[stsd, stco]));
        bx(
            b"trak",
            &bx(b"mdia", &cat(&[hdlr(kind), bx(b"minf", &stbl)])),
        )
    }

    /// A file with video+audio tracks whose single chunks hold known bytes.
    fn file(faststart: bool) -> (Vec<u8>, usize) {
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let ventry = bx(b"hvc1", &cat(&[vec![0u8; 78], bx(b"hvcC", &[9; 5])]));
        let aentry = bx(b"mp4a", &cat(&[vec![0u8; 28], bx(b"esds", &[7; 5])]));
        let payload = b"VIDEODATAAUDIODATA".to_vec();
        // Compute layout: moov size doesn't depend on offset values.
        let probe_moov = bx(
            b"moov",
            &cat(&[
                trak(b"vide", ventry.clone(), 0),
                trak(b"soun", aentry.clone(), 0),
            ]),
        );
        let mdat_header = 8;
        let data_start = if faststart {
            ftyp.len() + probe_moov.len() + mdat_header
        } else {
            ftyp.len() + mdat_header
        };
        let moov = bx(
            b"moov",
            &cat(&[
                trak(b"vide", ventry, data_start as u32),
                trak(b"soun", aentry, (data_start + 9) as u32),
            ]),
        );
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

    fn run(faststart: bool) {
        let dir = std::env::temp_dir().join(format!(
            "spherecut-inject-{}-{faststart}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (bytes, _) = file(faststart);
        let a = dir.join("in.mp4");
        let b = dir.join("out.mp4");
        let c = dir.join("out2.mp4");
        std::fs::write(&a, &bytes).unwrap();
        let edits = [
            EntryEdit {
                track: TrackKind::Video,
                boxes: cat(&[write::st3d(StereoMode::Mono), write::sv3d_equirect("test")]),
            },
            EntryEdit {
                track: TrackKind::Audio,
                boxes: write::sa3d_first_order_ambix(),
            },
        ];
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
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inject_with_moov_at_end() {
        run(false);
    }

    #[test]
    fn inject_with_faststart_shifts_chunk_offsets() {
        run(true);
    }
}
