//! A small, defensive ISO-BMFF (MP4/MOV) box reader.
//!
//! FFmpeg demuxes the media; this module reads the handful of boxes we need
//! to see for ourselves: Spherical Video V1/V2 metadata, spatial audio
//! (SA3D), handler names, and enough of the structure to tell cameras apart.
//! Export validation uses it to re-parse what we wrote.
//!
//! Every read is bounds-checked. Malformed input produces an error or a
//! partial tree with [`Mp4Tree::warnings`], never a panic.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Mp4Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("not an MP4/MOV file: {0}")]
    NotMp4(String),
    #[error("box `{fourcc}` at offset {offset} is malformed: {why}")]
    Malformed {
        fourcc: String,
        offset: u64,
        why: String,
    },
}

/// A four-character box type.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    pub const fn new(s: &[u8; 4]) -> Self {
        FourCc(*s)
    }
}

impl std::fmt::Display for FourCc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for &b in &self.0 {
            if b.is_ascii_graphic() || b == b' ' {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for FourCc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

/// One parsed box. Container boxes have `children`; leaf boxes keep their
/// payload if it is small enough to be metadata.
#[derive(Debug, Clone)]
pub struct Mp4Box {
    pub fourcc: FourCc,
    /// Offset of the box header in the file.
    pub offset: u64,
    /// Total size including the header.
    pub size: u64,
    /// Bytes from the box start to its payload: 8, 16 for a 64-bit size,
    /// plus 16 for a `uuid` extended type.
    pub header_len: u64,
    /// Payload (after the header, and after the extended type for `uuid`).
    pub payload: Vec<u8>,
    /// For `uuid` boxes, the 16-byte extended type.
    pub uuid: Option<[u8; 16]>,
    pub children: Vec<Mp4Box>,
}

impl Mp4Box {
    pub fn child(&self, t: &[u8; 4]) -> Option<&Mp4Box> {
        self.children.iter().find(|b| b.fourcc.0 == *t)
    }

    pub fn children_of<'a>(&'a self, t: &'a [u8; 4]) -> impl Iterator<Item = &'a Mp4Box> + 'a {
        self.children.iter().filter(move |b| b.fourcc.0 == *t)
    }

    /// Follow a path of child box types, e.g. `[b"mdia", b"hdlr"]`.
    pub fn path(&self, path: &[&[u8; 4]]) -> Option<&Mp4Box> {
        let mut cur = self;
        for t in path {
            cur = cur.child(t)?;
        }
        Some(cur)
    }
}

/// The top-level boxes of a file, with `moov` parsed in full.
#[derive(Debug, Clone, Default)]
pub struct Mp4Tree {
    pub top: Vec<Mp4Box>,
    pub file_size: u64,
    /// Bytes after the last complete top-level box (vendor trailers live
    /// here on some cameras).
    pub trailing_bytes: u64,
    /// Problems that did not stop parsing.
    pub warnings: Vec<String>,
}

impl Mp4Tree {
    pub fn moov(&self) -> Option<&Mp4Box> {
        self.top.iter().find(|b| b.fourcc.0 == *b"moov")
    }

    pub fn ftyp_brand(&self) -> Option<FourCc> {
        let f = self.top.iter().find(|b| b.fourcc.0 == *b"ftyp")?;
        let b: [u8; 4] = f.payload.get(0..4)?.try_into().ok()?;
        Some(FourCc(b))
    }

    pub fn tracks(&self) -> impl Iterator<Item = &Mp4Box> {
        self.moov().into_iter().flat_map(|m| m.children_of(b"trak"))
    }
}

/// Containers we descend into. Sample entries are handled separately
/// because they have fixed fields before their children.
const CONTAINERS: &[&[u8; 4]] = &[
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"udta", b"edts", b"dinf", b"sv3d", b"proj",
    b"meta", b"ilst", b"tref", b"mvex",
];

/// The largest box payload we load into memory for parsing. `moov` for an
/// hour of 8K is a few MB; anything above this is not metadata.
const MAX_LOADED: u64 = 256 * 1024 * 1024;

/// Leaf payloads above this size are not kept (we only need metadata).
const MAX_KEPT_LEAF: usize = 1024 * 1024;

pub fn read_file(path: &Path) -> Result<Mp4Tree, Mp4Error> {
    let mut f = File::open(path)?;
    read(&mut f)
}

/// Walk top-level boxes by seeking, loading only `ftyp`, `moov` and small
/// boxes into memory.
pub fn read<R: Read + Seek>(r: &mut R) -> Result<Mp4Tree, Mp4Error> {
    let file_size = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    let mut tree = Mp4Tree {
        file_size,
        ..Default::default()
    };
    let mut pos = 0u64;
    while pos < file_size {
        let remaining = file_size - pos;
        if remaining < 8 {
            tree.trailing_bytes = remaining;
            break;
        }
        r.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 16];
        let n = read_up_to(r, &mut hdr)?;
        let Some(h) = parse_header(&hdr[..n], remaining) else {
            if tree.top.is_empty() {
                return Err(Mp4Error::NotMp4("no valid box header at offset 0".into()));
            }
            tree.trailing_bytes = remaining;
            break;
        };
        if h.size > remaining {
            if tree.top.is_empty() {
                return Err(Mp4Error::NotMp4(format!(
                    "first box `{}` claims {} bytes but the file has {}",
                    h.fourcc, h.size, file_size
                )));
            }
            tree.warnings.push(format!(
                "box `{}` at {pos} is truncated ({} of {} bytes present)",
                h.fourcc, remaining, h.size
            ));
            tree.trailing_bytes = remaining;
            break;
        }
        if tree.top.is_empty() && !plausible_first_box(h.fourcc) {
            return Err(Mp4Error::NotMp4(format!(
                "first box type `{}` is not an MP4 box",
                h.fourcc
            )));
        }
        let payload_len = h.size - h.header_len;
        let load = matches!(&h.fourcc.0, b"moov" | b"ftyp" | b"uuid")
            || payload_len <= MAX_KEPT_LEAF as u64;
        let mut b = Mp4Box {
            fourcc: h.fourcc,
            offset: pos,
            size: h.size,
            header_len: h.header_len,
            payload: Vec::new(),
            uuid: None,
            children: Vec::new(),
        };
        if load && payload_len <= MAX_LOADED {
            r.seek(SeekFrom::Start(pos + h.header_len))?;
            let mut buf = vec![0u8; payload_len as usize];
            r.read_exact(&mut buf)?;
            fill_box(&mut b, buf, &mut tree.warnings);
        }
        tree.top.push(b);
        pos += h.size;
    }
    if tree.top.is_empty() {
        return Err(Mp4Error::NotMp4(if file_size == 0 {
            "the file is empty".into()
        } else {
            "no complete box found".into()
        }));
    }
    Ok(tree)
}

fn plausible_first_box(t: FourCc) -> bool {
    matches!(
        &t.0,
        b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"pnot" | b"uuid"
    )
}

fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

struct Header {
    fourcc: FourCc,
    size: u64,
    header_len: u64,
}

/// Parse a box header from `b`, given how many bytes remain in the parent.
fn parse_header(b: &[u8], remaining: u64) -> Option<Header> {
    let size32 = u32::from_be_bytes(b.get(0..4)?.try_into().ok()?);
    let fourcc = FourCc(b.get(4..8)?.try_into().ok()?);
    let (size, header_len) = match size32 {
        0 => (remaining, 8),
        1 => {
            let s = u64::from_be_bytes(b.get(8..16)?.try_into().ok()?);
            (s, 16)
        }
        s => (s as u64, 8),
    };
    let header_len = if fourcc.0 == *b"uuid" {
        header_len + 16
    } else {
        header_len
    };
    (size >= header_len).then_some(Header {
        fourcc,
        size,
        header_len,
    })
}

/// Fill payload/children of `b` from its payload bytes (which, for `uuid`,
/// still start with the extended type).
fn fill_box(b: &mut Mp4Box, mut payload: Vec<u8>, warnings: &mut Vec<String>) {
    if b.fourcc.0 == *b"uuid" {
        if payload.len() >= 16 {
            let mut u = [0u8; 16];
            u.copy_from_slice(&payload[..16]);
            b.uuid = Some(u);
            payload.drain(..16);
        } else {
            warnings.push(format!("uuid box at {} too short", b.offset));
        }
    }
    let base = b.offset + payload_start(b);
    if CONTAINERS.contains(&&b.fourcc.0) {
        // `meta` is a FullBox in MP4 but a plain box in QuickTime; tell them
        // apart by whether a plausible child header starts at 0 or 4.
        let skip = if b.fourcc.0 == *b"meta" && !looks_like_box(&payload, 0) {
            4
        } else {
            0
        };
        b.children = parse_children(&payload, skip, base, warnings);
        b.payload = Vec::new();
    } else if b.fourcc.0 == *b"stsd" {
        b.children = parse_stsd(&payload, base, warnings);
        b.payload = Vec::new();
    } else if payload.len() <= MAX_KEPT_LEAF {
        b.payload = payload;
    }
}

fn payload_start(b: &Mp4Box) -> u64 {
    b.header_len
}

fn looks_like_box(buf: &[u8], at: usize) -> bool {
    let Some(h) = buf
        .get(at..)
        .and_then(|s| parse_header(s, (s.len()) as u64))
    else {
        return false;
    };
    h.size as usize <= buf.len() - at && h.fourcc.0.iter().all(|c| c.is_ascii_graphic())
}

fn parse_children(buf: &[u8], start: usize, base: u64, warnings: &mut Vec<String>) -> Vec<Mp4Box> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos + 8 <= buf.len() {
        let rest = &buf[pos..];
        let Some(h) = parse_header(rest, rest.len() as u64) else {
            warnings.push(format!("bad child box header at {}", base + pos as u64));
            break;
        };
        if h.size as usize > rest.len() || (h.size as usize) < h.header_len as usize {
            warnings.push(format!(
                "child box `{}` at {} overruns its parent",
                h.fourcc,
                base + pos as u64
            ));
            break;
        }
        let mut b = Mp4Box {
            fourcc: h.fourcc,
            offset: base + pos as u64,
            size: h.size,
            header_len: h.header_len,
            payload: Vec::new(),
            uuid: None,
            children: Vec::new(),
        };
        let body = rest[(if h.fourcc.0 == *b"uuid" {
            h.header_len - 16
        } else {
            h.header_len
        }) as usize..h.size as usize]
            .to_vec();
        fill_box(&mut b, body, warnings);
        out.push(b);
        pos += h.size as usize;
    }
    out
}

/// `stsd` is a FullBox with an entry count, followed by sample entries whose
/// own children start after fixed per-kind fields.
fn parse_stsd(buf: &[u8], base: u64, warnings: &mut Vec<String>) -> Vec<Mp4Box> {
    let mut entries = parse_children(buf, 8.min(buf.len()), base, warnings);
    for e in &mut entries {
        let skip = sample_entry_fixed_len(e.fourcc, &e.payload);
        if let Some(skip) = skip.filter(|&s| s <= e.payload.len()) {
            let payload = std::mem::take(&mut e.payload);
            e.children = parse_children(&payload, skip, e.offset + e.header_len, warnings);
            // Keep the fixed fields; they hold width/height/channels.
            e.payload = payload[..skip].to_vec();
        }
    }
    entries
}

/// Length of a sample entry's fixed fields before its child boxes, for the
/// entry types we know.
pub(crate) fn sample_entry_fixed_len(t: FourCc, payload: &[u8]) -> Option<usize> {
    const VIDEO: &[&[u8; 4]] = &[
        b"avc1", b"avc3", b"hvc1", b"hev1", b"av01", b"vp09", b"apch", b"apcn", b"apcs", b"apco",
        b"ap4h", b"ap4x", b"mp4v", b"dvh1", b"dvhe", b"encv",
    ];
    const AUDIO: &[&[u8; 4]] = &[
        b"mp4a", b"Opus", b"lpcm", b"sowt", b"twos", b"ipcm", b"fLaC", b"ac-3", b"ec-3", b"alac",
        b"in24", b"in32", b"fl32", b"enca",
    ];
    if VIDEO.contains(&&t.0) {
        Some(78)
    } else if AUDIO.contains(&&t.0) {
        // QuickTime sound sample description versions 1 and 2 are longer.
        let version = u16::from_be_bytes(payload.get(8..10)?.try_into().ok()?);
        match version {
            0 => Some(28),
            1 => Some(28 + 16),
            2 => Some(28 + 36),
            _ => None,
        }
    } else {
        None
    }
}

/// Read-only cursor over a box payload.
#[derive(Debug)]
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn i32(&mut self) -> Option<i32> {
        Some(i32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    /// A FullBox version and flags.
    pub fn version_flags(&mut self) -> Option<(u8, u32)> {
        let v = self.u32()?;
        Some(((v >> 24) as u8, v & 0x00ff_ffff))
    }
    /// A NUL-terminated (or payload-terminated) UTF-8 string, lossy.
    pub fn cstr(&mut self) -> String {
        let rest = self.buf.get(self.pos..).unwrap_or_default();
        let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        let s = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.pos += (end + 1).min(rest.len());
        s
    }
    pub fn remaining(&self) -> &'a [u8] {
        self.buf.get(self.pos..).unwrap_or_default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a box from its type and payload.
    pub fn bx(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(payload);
        v
    }

    pub fn full(t: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut p = (((version as u32) << 24) | flags).to_be_bytes().to_vec();
        p.extend_from_slice(payload);
        bx(t, &p)
    }

    pub fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn minimal() -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isomiso2");
        let hdlr = full(
            b"hdlr",
            0,
            0,
            b"\0\0\0\0vide\0\0\0\0\0\0\0\0\0\0\0\0VideoHandler\0",
        );
        let mut visual = vec![0u8; 78];
        visual[24..26].copy_from_slice(&640u16.to_be_bytes());
        visual[26..28].copy_from_slice(&320u16.to_be_bytes());
        let entry = bx(b"hvc1", &cat(&[visual, bx(b"hvcC", &[1, 2, 3])]));
        let stsd = full(b"stsd", 0, 0, &cat(&[1u32.to_be_bytes().to_vec(), entry]));
        let stbl = bx(b"stbl", &stsd);
        let minf = bx(b"minf", &stbl);
        let mdia = bx(b"mdia", &cat(&[hdlr, minf]));
        let trak = bx(b"trak", &mdia);
        let moov = bx(b"moov", &trak);
        cat(&[ftyp, moov, bx(b"mdat", &[0; 32])])
    }

    #[test]
    fn parses_nested_sample_entries() {
        let t = read(&mut io::Cursor::new(minimal())).unwrap();
        assert_eq!(t.ftyp_brand(), Some(FourCc::new(b"isom")));
        let trak = t.tracks().next().unwrap();
        let entry = trak
            .path(&[b"mdia", b"minf", b"stbl", b"stsd", b"hvc1"])
            .unwrap();
        assert!(entry.child(b"hvcC").is_some());
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
        assert_eq!(t.trailing_bytes, 0);
    }

    #[test]
    fn trailer_after_last_box_is_reported() {
        let mut f = minimal();
        f.extend_from_slice(b"some vendor trailer bytes");
        let t = read(&mut io::Cursor::new(f)).unwrap();
        assert_eq!(t.trailing_bytes, 25);
    }

    #[test]
    fn rejects_non_mp4() {
        for junk in [&b""[..], b"hello world, not a video", &[0xff; 64]] {
            assert!(read(&mut io::Cursor::new(junk.to_vec())).is_err());
        }
    }

    #[test]
    fn every_truncation_is_handled() {
        let f = minimal();
        for len in 0..f.len() {
            // Must not panic; may error or warn.
            let _ = read(&mut io::Cursor::new(f[..len].to_vec()));
        }
    }

    #[test]
    fn random_corruption_never_panics() {
        let f = minimal();
        let mut seed = 0x1234_5678_u32;
        for _ in 0..2000 {
            let mut g = f.clone();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % g.len();
                g[i] = (seed >> 8) as u8;
            }
            let _ = read(&mut io::Cursor::new(g));
        }
    }
}
