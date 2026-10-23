//! A small, defensive ISO-BMFF (MP4/MOV) box reader.
//!
//! FFmpeg demuxes the media; this module reads the handful of boxes we need
//! to see for ourselves: Spherical Video V1/V2 metadata, spatial audio
//! (SA3D), handler names, and enough of the structure to tell cameras apart.
//! Export validation uses it to re-parse what we wrote.
//!
//! Every read is bounds-checked. Malformed input produces an error or a
//! partial tree with [`Mp4Tree::warnings`], never a panic.
//!
//! Time and memory are linear in the input, whatever it holds: each
//! top-level box is read into one buffer and its children are parsed from
//! offsets into it, nesting stops at [`MAX_DEPTH`], parsing stops at
//! [`MAX_BOXES`], and only small leaf payloads are copied out.

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
    /// here on some cameras). A box cut off by the end of the file is
    /// [`Mp4Tree::truncated`] instead.
    pub trailing_bytes: u64,
    /// The top-level box that runs past the end of the file, if the file
    /// was cut short. It is not in `top`.
    pub truncated: Option<Truncated>,
    /// Problems that did not stop parsing. Each one means the structure is
    /// damaged (or hostile); a healthy file has none.
    pub warnings: Vec<String>,
}

/// A top-level box whose header claims more bytes than the file has left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncated {
    pub fourcc: FourCc,
    pub offset: u64,
    pub header_len: u64,
    /// The size its header claims.
    pub size: u64,
    /// Bytes of it present in the file.
    pub present: u64,
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

/// The largest `moov` we load into memory for parsing. `moov` for an hour
/// of 8K is a few MB; anything above this is not metadata.
const MAX_LOADED: u64 = 256 * 1024 * 1024;

/// Leaf payloads above this size are not kept (we only need metadata).
const MAX_KEPT_LEAF: usize = 1024 * 1024;

/// Sample tables are kept whatever their size, so the structure can be
/// checked against the media data. They are part of `moov`, so
/// [`MAX_LOADED`] bounds them.
const SAMPLE_TABLES: &[&[u8; 4]] = &[b"stts", b"stsz", b"stz2", b"stsc", b"stco", b"co64"];

/// Other top-level boxes are vendor metadata or fragments, read only when
/// this small and while their total stays under [`MAX_KEPT_TOP_TOTAL`].
/// Media data and padding (`mdat`, `free`, ...) are never read.
const MAX_KEPT_TOP: u64 = 64 * 1024;
const MAX_KEPT_TOP_TOTAL: u64 = 1024 * 1024;

/// Real files nest boxes about ten deep (`moov/trak/mdia/minf/stbl/stsd/
/// hvc1/sv3d/proj/equi`). Boxes deeper than this are skipped with a
/// warning.
pub const MAX_DEPTH: usize = 32;

/// Far more boxes than any real file has. A parsed box takes about 100
/// bytes, so without a limit a file of empty 8-byte boxes would need
/// twelve times its size in memory.
pub const MAX_BOXES: usize = 250_000;

/// Warnings past this many are counted, not listed.
const MAX_WARNINGS: usize = 32;

pub fn read_file(path: &Path) -> Result<Mp4Tree, Mp4Error> {
    let mut f = File::open(path)?;
    read(&mut f)
}

/// Walk top-level boxes by seeking, loading only `moov` and small metadata
/// boxes into memory.
pub fn read<R: Read + Seek>(r: &mut R) -> Result<Mp4Tree, Mp4Error> {
    let file_size = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    let mut tree = Mp4Tree {
        file_size,
        ..Default::default()
    };
    let mut walk = Walk::default();
    let mut top_budget = MAX_KEPT_TOP_TOTAL;
    let mut pos = 0u64;
    while pos < file_size {
        let remaining = file_size - pos;
        if remaining < 8 {
            tree.trailing_bytes = remaining;
            break;
        }
        r.seek(SeekFrom::Start(pos))?;
        // Room for a 64-bit size and a `uuid` extended type.
        let mut hdr = [0u8; 32];
        let n = read_up_to(r, &mut hdr)?;
        let Some(h) = parse_header(&hdr[..n], remaining) else {
            if pos == 0 {
                return Err(Mp4Error::NotMp4("no valid box header at offset 0".into()));
            }
            tree.trailing_bytes = remaining;
            break;
        };
        if pos == 0 && !plausible_first_box(h.fourcc) {
            return Err(Mp4Error::NotMp4(format!(
                "first box type `{}` is not an MP4 box",
                h.fourcc
            )));
        }
        if h.size > remaining && !known_top_level(h.fourcc) {
            // Vendor trailers (Insta360 before X5 firmware 1.7) are not
            // boxes, but their first bytes can look like an oversized one.
            tree.trailing_bytes = remaining;
            break;
        }
        if h.size > remaining {
            tree.truncated = Some(Truncated {
                fourcc: h.fourcc,
                offset: pos,
                header_len: h.header_len,
                size: h.size,
                present: remaining,
            });
            break;
        }
        if !walk.add_box(pos) {
            break;
        }
        let payload_len = h.size - h.header_len;
        let load = match &h.fourcc.0 {
            b"moov" if payload_len > MAX_LOADED => {
                walk.warn(format!(
                    "`moov` at {pos} is {payload_len} bytes, too large to parse"
                ));
                false
            }
            b"moov" => true,
            b"mdat" | b"free" | b"skip" | b"wide" => false,
            _ => payload_len <= MAX_KEPT_TOP.min(top_budget),
        };
        let hl = h.header_len as usize;
        let mut b = Mp4Box {
            fourcc: h.fourcc,
            offset: pos,
            size: h.size,
            header_len: h.header_len,
            payload: Vec::new(),
            uuid: extended_type(h.fourcc, &hdr, hl),
            children: Vec::new(),
        };
        if load {
            if h.fourcc.0 != *b"moov" {
                top_budget -= payload_len;
            }
            r.seek(SeekFrom::Start(pos + h.header_len))?;
            let mut buf = vec![0u8; payload_len as usize];
            r.read_exact(&mut buf)?;
            fill_box(&mut b, &buf, 0, &mut walk);
        }
        tree.top.push(b);
        pos += h.size;
    }
    if tree.top.is_empty() && tree.truncated.is_none() {
        return Err(Mp4Error::NotMp4(if file_size == 0 {
            "the file is empty".into()
        } else {
            "no complete box found".into()
        }));
    }
    tree.warnings = walk.finish();
    Ok(tree)
}

fn plausible_first_box(t: FourCc) -> bool {
    matches!(
        &t.0,
        b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"pnot" | b"uuid"
    )
}

/// Box types that appear at the top level of MP4 and MOV files.
fn known_top_level(t: FourCc) -> bool {
    plausible_first_box(t)
        || matches!(
            &t.0,
            b"moof" | b"mfra" | b"meta" | b"udta" | b"sidx" | b"styp" | b"inst"
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

/// Limits and problems shared by every level of one parse.
#[derive(Default)]
struct Walk {
    warnings: Vec<String>,
    dropped: usize,
    boxes: usize,
}

impl Walk {
    fn warn(&mut self, w: String) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(w);
        } else {
            self.dropped += 1;
        }
    }

    /// Count the box at `offset`; false once [`MAX_BOXES`] is reached.
    fn add_box(&mut self, offset: u64) -> bool {
        if self.boxes >= MAX_BOXES {
            if self.boxes == MAX_BOXES {
                self.warn(format!(
                    "more than {MAX_BOXES} boxes; stopped parsing at offset {offset}"
                ));
                self.boxes += 1;
            }
            return false;
        }
        self.boxes += 1;
        true
    }

    fn finish(mut self) -> Vec<String> {
        if self.dropped > 0 {
            self.warnings
                .push(format!("{} more problems not listed", self.dropped));
        }
        self.warnings
    }
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

/// The 16-byte extended type of a `uuid` box: the last 16 bytes of its
/// `header_len`-byte header `b`.
fn extended_type(t: FourCc, b: &[u8], header_len: usize) -> Option<[u8; 16]> {
    if t.0 != *b"uuid" {
        return None;
    }
    b.get(header_len.checked_sub(16)?..header_len)?
        .try_into()
        .ok()
}

/// Fill the payload or children of `b` from `body`, its bytes after the
/// header. `depth` is the depth of `b` itself: 0 at the top level.
fn fill_box(b: &mut Mp4Box, body: &[u8], depth: usize, walk: &mut Walk) {
    let base = b.offset + b.header_len;
    if CONTAINERS.contains(&&b.fourcc.0) {
        // `meta` is a FullBox in MP4 but a plain box in QuickTime; tell them
        // apart by whether a plausible child header starts at 0 or 4.
        let skip = if b.fourcc.0 == *b"meta" && !looks_like_box(body, 0) {
            4
        } else {
            0
        };
        b.children = parse_children(body, skip, base, depth + 1, walk, fill_box);
    } else if b.fourcc.0 == *b"stsd" {
        // A FullBox with an entry count, then the sample entries.
        b.children = parse_children(body, 8, base, depth + 1, walk, fill_entry);
    } else if body.len() <= MAX_KEPT_LEAF || SAMPLE_TABLES.contains(&&b.fourcc.0) {
        b.payload = body.to_vec();
    }
}

/// Fill a sample entry, whose child boxes start after fixed per-kind
/// fields.
fn fill_entry(e: &mut Mp4Box, body: &[u8], depth: usize, walk: &mut Walk) {
    match sample_entry_fixed_len(e.fourcc, body).filter(|&s| s <= body.len()) {
        Some(skip) => {
            // Keep the fixed fields; they hold width/height/channels.
            e.payload = body[..skip].to_vec();
            let base = e.offset + e.header_len;
            e.children = parse_children(body, skip, base, depth + 1, walk, fill_box);
        }
        None => fill_box(e, body, depth, walk),
    }
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

type Fill = fn(&mut Mp4Box, &[u8], usize, &mut Walk);

/// Parse the boxes in `buf[start..]`, at file offset `base + start` and
/// nesting depth `depth`, filling each with `fill`.
fn parse_children(
    buf: &[u8],
    start: usize,
    base: u64,
    depth: usize,
    walk: &mut Walk,
    fill: Fill,
) -> Vec<Mp4Box> {
    let mut out = Vec::new();
    if depth > MAX_DEPTH {
        if buf.len() >= start.saturating_add(8) {
            walk.warn(format!(
                "boxes at {} are nested more than {MAX_DEPTH} deep; skipped them",
                base + start as u64
            ));
        }
        return out;
    }
    let mut pos = start;
    while pos.saturating_add(8) <= buf.len() {
        let rest = &buf[pos..];
        let at = base + pos as u64;
        let Some(h) = parse_header(rest, rest.len() as u64) else {
            walk.warn(format!("bad child box header at {at}"));
            break;
        };
        if h.size > rest.len() as u64 {
            walk.warn(format!(
                "child box `{}` at {at} overruns its parent",
                h.fourcc
            ));
            break;
        }
        if !walk.add_box(at) {
            break;
        }
        // parse_header guarantees header_len <= size, and size fits in rest.
        let (hl, size) = (h.header_len as usize, h.size as usize);
        let mut b = Mp4Box {
            fourcc: h.fourcc,
            offset: at,
            size: h.size,
            header_len: h.header_len,
            payload: Vec::new(),
            uuid: extended_type(h.fourcc, rest, hl),
            children: Vec::new(),
        };
        fill(&mut b, &rest[hl..size], depth, walk);
        out.push(b);
        pos += size;
    }
    out
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

    #[test]
    fn top_level_uuid_keeps_its_extended_type_and_payload() {
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let uuid = bx(b"uuid", b"0123456789ABCDEFPAYLOAD-BYTES-HERE-XYZ");
        let t = read(&mut io::Cursor::new(cat(&[ftyp, uuid]))).unwrap();
        let u = &t.top[1];
        assert_eq!(u.uuid, Some(*b"0123456789ABCDEF"));
        assert_eq!(u.payload, b"PAYLOAD-BYTES-HERE-XYZ");
        assert_eq!(u.header_len, 24);
    }

    #[test]
    fn truncated_box_is_not_trailing_bytes() {
        // moov last, as most cameras write it, then cut 10 bytes short.
        let f = minimal();
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isomiso2");
        let moov_at = ftyp.len() + 40;
        let moov = &f[ftyp.len()..f.len() - 40];
        let g = cat(&[ftyp, bx(b"mdat", &[0; 32]), moov.to_vec()]);
        let t = read(&mut io::Cursor::new(g[..g.len() - 10].to_vec())).unwrap();
        assert_eq!(t.trailing_bytes, 0);
        let cut = t.truncated.unwrap();
        assert_eq!(cut.fourcc, FourCc::new(b"moov"));
        assert_eq!(cut.offset, moov_at as u64);
        assert_eq!(
            (cut.size, cut.present),
            (moov.len() as u64, moov.len() as u64 - 10)
        );
        assert!(t.moov().is_none());

        // A cut-short first box is still an MP4, just an unusable one.
        let t = read(&mut io::Cursor::new(f[..20].to_vec())).unwrap();
        assert!(t.top.is_empty());
        assert_eq!(t.truncated.map(|c| c.fourcc), Some(FourCc::new(b"ftyp")));
    }

    #[test]
    fn media_data_and_large_top_level_payloads_are_not_kept() {
        let f = cat(&[
            bx(b"ftyp", b"isom\0\0\x02\0isom"),
            bx(b"mdat", &[7; 1000]),
            bx(b"free", &[0; 100]),
            bx(b"blob", &vec![1; MAX_KEPT_TOP as usize + 1]),
            bx(b"camd", b"vendor"),
        ]);
        let t = read(&mut io::Cursor::new(f)).unwrap();
        let kept: Vec<usize> = t.top.iter().map(|b| b.payload.len()).collect();
        assert_eq!(kept, [12, 0, 0, 0, 6]);
    }

    /// `ftyp` and a `moov` holding `n` nested `udta` boxes, the innermost
    /// holding `inner`.
    fn nested_udta(n: usize, inner: &[u8]) -> Vec<u8> {
        let mut f = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let total = 8 + 8 * n + inner.len();
        f.extend_from_slice(&(total as u32).to_be_bytes());
        f.extend_from_slice(b"moov");
        for i in 0..n {
            f.extend_from_slice(&((8 * (n - i) + inner.len()) as u32).to_be_bytes());
            f.extend_from_slice(b"udta");
        }
        f.extend_from_slice(inner);
        f
    }

    /// (boxes, payload bytes kept, deepest depth) of a parsed tree.
    fn tree_stats(t: &Mp4Tree) -> (usize, usize, usize) {
        fn walk(b: &Mp4Box, depth: usize, s: &mut (usize, usize, usize)) {
            s.0 += 1;
            s.1 += b.payload.len();
            s.2 = s.2.max(depth);
            for c in &b.children {
                walk(c, depth + 1, s);
            }
        }
        let mut s = (0, 0, 0);
        for b in &t.top {
            walk(b, 0, &mut s);
        }
        s
    }

    #[test]
    fn nesting_stops_at_max_depth() {
        let leaf = bx(b"abcd", b"x");
        // The leaf sits at depth n + 1.
        let t = read(&mut io::Cursor::new(nested_udta(MAX_DEPTH - 1, &leaf))).unwrap();
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
        assert_eq!(tree_stats(&t).2, MAX_DEPTH);
        let t = read(&mut io::Cursor::new(nested_udta(MAX_DEPTH, &leaf))).unwrap();
        assert_eq!(tree_stats(&t).2, MAX_DEPTH);
        let deep = format!("nested more than {MAX_DEPTH} deep");
        assert!(t.warnings[0].contains(&deep), "{:?}", t.warnings);
    }

    /// Reset this process's peak resident memory, run `f`, and return how
    /// far the peak rose above the starting level, in KiB. None where
    /// /proc cannot reset the peak.
    fn peak_rss_growth_kib(f: impl FnOnce()) -> Option<u64> {
        fn hwm() -> Option<u64> {
            let s = std::fs::read_to_string("/proc/self/status").ok()?;
            let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
            line.split_whitespace().nth(1)?.parse().ok()
        }
        std::fs::write("/proc/self/clear_refs", "5").ok()?;
        let before = hwm()?;
        f();
        Some(hwm()?.saturating_sub(before))
    }

    #[test]
    fn deep_nesting_is_fast_and_small() {
        // The 160 KB file used 1.4 GB and overflowed the stack before the
        // depth limit; the 1.6 MB one never finished.
        for n in [20_000, 200_000] {
            let f = nested_udta(n, &[]);
            let mut result = None;
            let start = std::time::Instant::now();
            let grew = peak_rss_growth_kib(|| result = Some(read(&mut io::Cursor::new(&f))));
            let elapsed = start.elapsed();
            let t = result.unwrap().unwrap();
            assert!(elapsed.as_secs_f64() < 2.0, "{n} levels took {elapsed:?}");
            let (boxes, _, depth) = tree_stats(&t);
            assert_eq!((boxes, depth), (2 + MAX_DEPTH, MAX_DEPTH));
            assert!(
                t.warnings[0].contains("nested more than"),
                "{:?}",
                t.warnings
            );
            // The input is 1.6 MB at most and `moov` is read once; other
            // tests running at the same time share the process.
            if let Some(kib) = grew {
                assert!(
                    kib < 64 * 1024,
                    "{n} levels raised peak memory by {kib} KiB"
                );
            }
        }
    }

    #[test]
    fn box_count_is_limited() {
        let moov = bx(b"free", &[]).repeat(MAX_BOXES + 10);
        let f = cat(&[bx(b"ftyp", b"isom\0\0\x02\0isom"), bx(b"moov", &moov)]);
        let t = read(&mut io::Cursor::new(f)).unwrap();
        assert_eq!(tree_stats(&t).0, MAX_BOXES);
        let many = format!("more than {MAX_BOXES} boxes");
        assert!(t.warnings[0].contains(&many), "{:?}", t.warnings);
    }

    /// Structure-aware fuzzing: random box trees, encoded with 32-bit,
    /// 64-bit and to-the-end sizes, then cut and corrupted.
    mod fuzz {
        use std::time::{Duration, Instant};

        use proptest::collection::vec;
        use proptest::prelude::*;

        use super::super::*;
        use super::{bx, tree_stats};

        #[derive(Debug, Clone, Copy)]
        enum Size {
            Short,
            Large,
            /// Size 0: to the end of the parent (or file). Only the last box
            /// of a list is encoded this way; others fall back to `Short`.
            ToEnd,
        }

        #[derive(Debug, Clone)]
        enum Body {
            Leaf([u8; 4], Vec<u8>),
            Uuid([u8; 16], Vec<u8>),
            Container([u8; 4], Vec<Gen>),
            Stsd(Vec<Gen>),
            /// A sample entry with zeroed fixed fields; only inside `stsd`.
            Entry([u8; 4], Vec<Gen>),
        }

        #[derive(Debug, Clone)]
        struct Gen {
            size: Size,
            body: Body,
        }

        fn encode_list(list: &[Gen]) -> Vec<u8> {
            let mut out = Vec::new();
            for (i, g) in list.iter().enumerate() {
                encode(g, i + 1 == list.len(), &mut out);
            }
            out
        }

        fn encode(g: &Gen, last: bool, out: &mut Vec<u8>) {
            let (t, body) = match &g.body {
                Body::Leaf(t, p) => (*t, p.clone()),
                Body::Uuid(u, p) => (*b"uuid", [&u[..], p].concat()),
                Body::Container(t, c) => (*t, encode_list(c)),
                Body::Stsd(c) => {
                    let mut p = vec![0u8; 4];
                    p.extend_from_slice(&(c.len() as u32).to_be_bytes());
                    p.extend_from_slice(&encode_list(c));
                    (*b"stsd", p)
                }
                Body::Entry(t, c) => {
                    let fixed = if t == b"hvc1" { 78 } else { 28 };
                    (*t, [vec![0u8; fixed], encode_list(c)].concat())
                }
            };
            match (g.size, last) {
                (Size::ToEnd, true) => out.extend_from_slice(&0u32.to_be_bytes()),
                (Size::Large, _) => {
                    out.extend_from_slice(&1u32.to_be_bytes());
                    out.extend_from_slice(&t);
                    out.extend_from_slice(&(16 + body.len() as u64).to_be_bytes());
                    out.extend_from_slice(&body);
                    return;
                }
                _ => out.extend_from_slice(&(8 + body.len() as u32).to_be_bytes()),
            }
            out.extend_from_slice(&t);
            out.extend_from_slice(&body);
        }

        /// Whether the parse of `g` matches what was generated.
        fn same(g: &Gen, b: &Mp4Box, top: bool) -> bool {
            let kids = |c: &[Gen]| {
                c.len() == b.children.len()
                    && c.iter().zip(&b.children).all(|(g, b)| same(g, b, false))
            };
            match &g.body {
                Body::Leaf(t, p) => {
                    let dropped = top && matches!(t, b"mdat" | b"free" | b"skip" | b"wide");
                    b.fourcc.0 == *t && b.payload == if dropped { &[][..] } else { &p[..] }
                }
                Body::Uuid(u, p) => b.uuid == Some(*u) && b.payload == *p,
                Body::Container(t, c) => b.fourcc.0 == *t && kids(c),
                Body::Stsd(c) => b.fourcc.0 == *b"stsd" && kids(c),
                Body::Entry(t, c) => {
                    b.fourcc.0 == *t && b.payload.iter().all(|&x| x == 0) && kids(c)
                }
            }
        }

        fn size() -> impl Strategy<Value = Size> {
            prop_oneof![4 => Just(Size::Short), 1 => Just(Size::Large), 1 => Just(Size::ToEnd)]
        }

        fn leaf_type() -> impl Strategy<Value = [u8; 4]> {
            // Printable, so QuickTime-style `meta` is recognised.
            prop_oneof![
                Just(*b"hdlr"),
                Just(*b"stsz"),
                Just(*b"stco"),
                Just(*b"mdat"),
                Just(*b"free"),
                Just(*b"st3d"),
                Just(*b"SA3D"),
                proptest::array::uniform4(b'a'..=b'z').prop_filter("a leaf type", |t| {
                    !CONTAINERS.contains(&t) && !matches!(t, b"stsd" | b"uuid")
                }),
            ]
        }

        fn container_type() -> impl Strategy<Value = [u8; 4]> {
            proptest::sample::select(CONTAINERS.iter().map(|t| **t).collect::<Vec<_>>())
        }

        fn tree() -> impl Strategy<Value = Gen> {
            let leaf = prop_oneof![
                (leaf_type(), vec(any::<u8>(), 0..24)).prop_map(|(t, p)| Body::Leaf(t, p)),
                (any::<[u8; 16]>(), vec(any::<u8>(), 0..24)).prop_map(|(u, p)| Body::Uuid(u, p)),
            ];
            let leaf = (size(), leaf).prop_map(|(size, body)| Gen { size, body });
            leaf.prop_recursive(6, 48, 4, |inner| {
                let entry = (
                    size(),
                    prop_oneof![Just(*b"hvc1"), Just(*b"mp4a")],
                    vec(inner.clone(), 0..3),
                )
                    .prop_map(|(size, t, c)| Gen {
                        size,
                        body: Body::Entry(t, c),
                    });
                prop_oneof![
                    3 => (size(), container_type(), vec(inner, 0..4)).prop_map(
                        |(size, t, c)| Gen { size, body: Body::Container(t, c) }
                    ),
                    1 => (size(), vec(entry, 0..3))
                        .prop_map(|(size, c)| Gen { size, body: Body::Stsd(c) }),
                ]
            })
        }

        /// A file: `ftyp`, then random top-level boxes.
        fn file() -> impl Strategy<Value = (Vec<Gen>, Vec<u8>)> {
            vec(tree(), 1..6).prop_map(|top| {
                let mut f = bx(b"ftyp", b"isom\0\0\x02\0isom");
                f.extend_from_slice(&encode_list(&top));
                (top, f)
            })
        }

        /// Parse `f`; check it took little time and that the tree is no
        /// bigger than the input could describe.
        fn parse_bounded(f: &[u8]) -> Result<Option<Mp4Tree>, TestCaseError> {
            let start = Instant::now();
            let r = read(&mut io::Cursor::new(f));
            let elapsed = start.elapsed();
            prop_assert!(elapsed < Duration::from_secs(1), "parse took {elapsed:?}");
            let Ok(t) = r else { return Ok(None) };
            let (boxes, kept, depth) = tree_stats(&t);
            prop_assert!(boxes <= f.len() / 8, "{boxes} boxes from {} bytes", f.len());
            prop_assert!(kept <= f.len(), "kept {kept} of {} bytes", f.len());
            prop_assert!(depth <= MAX_DEPTH);
            prop_assert!(t.warnings.len() <= MAX_WARNINGS + 1);
            Ok(Some(t))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn well_formed_trees_parse_exactly((top, f) in file()) {
                let t = parse_bounded(&f)?.unwrap_or_default();
                prop_assert!(t.warnings.is_empty(), "{:?}", t.warnings);
                prop_assert!(t.truncated.is_none());
                prop_assert_eq!(t.trailing_bytes, 0);
                prop_assert_eq!(t.top.len(), top.len() + 1);
                for (g, b) in top.iter().zip(&t.top[1..]) {
                    prop_assert!(same(g, b, true), "{g:?}\nparsed as\n{b:?}");
                }
            }

            #[test]
            fn damaged_trees_never_panic(
                (_, f) in file(),
                cut in any::<prop::sample::Index>(),
                flips in vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
            ) {
                let mut g = f.clone();
                for (i, x) in &flips {
                    let i = i.index(g.len());
                    g[i] ^= x;
                }
                parse_bounded(&g)?;
                parse_bounded(&g[..cut.index(g.len() + 1)])?;
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(48))]

            #[test]
            fn every_truncation_of_small_trees((_, f) in file()) {
                for len in 0..f.len().min(512) {
                    parse_bounded(&f[..len])?;
                }
            }
        }
    }
}
