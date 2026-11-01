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
    Io(io::Error),
    #[error("not an MP4/MOV file: {0}")]
    NotMp4(String),
    #[error("box `{fourcc}` at offset {offset} is malformed: {why}")]
    Malformed {
        fourcc: String,
        offset: u64,
        why: String,
    },
}

/// By hand rather than `#[from]`, which would also make the I/O error the
/// `source()` of a message that already includes it.
impl From<io::Error> for Mp4Error {
    fn from(e: io::Error) -> Self {
        Mp4Error::Io(e)
    }
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

/// A top-level box that runs past the end of the file: its header claims
/// more bytes than the file has left, or the file ends inside the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncated {
    /// None when the file ends before the box type.
    pub fourcc: Option<FourCc>,
    pub offset: u64,
    pub header_len: u64,
    /// The size its header claims; None when the file ends before it.
    pub size: Option<u64>,
    /// Bytes of it present in the file, fewer than `header_len` when the
    /// header itself is cut short.
    pub present: u64,
}

impl Truncated {
    fn is(&self, t: &[u8; 4]) -> bool {
        self.fourcc.is_some_and(|f| f.0 == *t)
    }

    pub(crate) fn reason(&self) -> String {
        let (present, offset) = (self.present, self.offset);
        if let (Some(f), Some(size)) = (self.fourcc, self.size)
            && present >= self.header_len
        {
            return format!(
                "the file ends {present} bytes into the {size}-byte `{f}` box at offset {offset}: it was cut short"
            );
        }
        let what = match (self.fourcc, self.size) {
            (Some(f), _) => format!("a `{f}` box"),
            (None, Some(size)) => format!("a {size}-byte box"),
            (None, None) => "a box".into(),
        };
        let present = match present {
            1 => "1 byte".into(),
            n => format!("{n} bytes"),
        };
        format!(
            "the file ends {present} into the header of {what} at offset {offset}: it was cut short"
        )
    }
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

    /// Why the file looks damaged: a box cut off by the end of the file,
    /// malformed boxes, box types that are not text where only registered
    /// types belong, a `trak` under another type, an `sv3d` FFmpeg ignores,
    /// timescales FFmpeg cannot use, and sample tables that disagree with
    /// each other or point outside the media data. Empty for a healthy
    /// file. The work is linear in the size of the sample tables.
    pub fn damage(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(t) = &self.truncated {
            out.push(t.reason());
        }
        out.extend(self.warnings.iter().cloned());
        let Some(moov) = self.moov() else {
            // The box cut short may have been `moov`.
            let cut_moov = self
                .truncated
                .is_some_and(|t| t.fourcc.is_none() || t.is(b"moov"));
            // HEIF images and other item-based files have a top-level `meta`.
            let items = self.top.iter().any(|b| b.fourcc.0 == *b"meta");
            if !cut_moov && !items {
                out.push("there is no `moov` box, so no index of the media".into());
            }
            return out;
        };
        unprintable_types(moov, &mut out);
        renamed_traks(moov, &mut out);
        lost_projections(moov, &mut out);
        if let Some(ts) = moov.child(b"mvhd").and_then(bad_timescale) {
            out.push(format!(
                "`mvhd` gives timescale {ts}, so the movie's times are wrong"
            ));
        }
        let mdats = self.media_data();
        for (i, trak) in moov.children_of(b"trak").enumerate() {
            if let Err(why) = check_track(trak, &mdats, self.file_size) {
                out.push(format!("track #{i}: {why}"));
            }
        }
        out
    }

    /// Payload byte ranges of the top-level `mdat` boxes in file order,
    /// including one cut off by the end of the file.
    fn media_data(&self) -> Vec<(u64, u64)> {
        let cut = self
            .truncated
            .filter(|t| t.is(b"mdat"))
            .and_then(|t| Some((t.offset, t.header_len, t.size?)));
        self.top
            .iter()
            .filter(|b| b.fourcc.0 == *b"mdat")
            .map(|b| (b.offset, b.header_len, b.size))
            .chain(cut)
            .map(|(offset, header_len, size)| (offset + header_len, offset.saturating_add(size)))
            .collect()
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

/// Leaf payloads above this size are not kept (we only need metadata),
/// unless they start with a track header: see [`renamed_traks`].
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

/// What [`read_with`] accepts as the first box of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstBox {
    /// A type that MP4 and MOV files start with (`ftyp`, `moov`, `mdat`,
    /// ...); anything else is [`Mp4Error::NotMp4`]. For input that may not
    /// be MP4 at all.
    Known,
    /// Any box whose header is valid, for input known to be MP4/MOV
    /// (FFmpeg's demuxer took it), so that a damaged first box does not
    /// hide the rest of the structure.
    Any,
}

pub fn read_file(path: &Path) -> Result<Mp4Tree, Mp4Error> {
    read_file_with(path, FirstBox::Known)
}

pub fn read_file_with(path: &Path, first: FirstBox) -> Result<Mp4Tree, Mp4Error> {
    spherecut_core::may_block("reading an MP4 file's boxes");
    let mut f = File::open(path)?;
    read_with(&mut f, first)
}

/// Walk top-level boxes by seeking, loading only `moov` and small metadata
/// boxes into memory.
pub fn read<R: Read + Seek>(r: &mut R) -> Result<Mp4Tree, Mp4Error> {
    read_with(r, FirstBox::Known)
}

pub fn read_with<R: Read + Seek>(r: &mut R, first: FirstBox) -> Result<Mp4Tree, Mp4Error> {
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
        r.seek(SeekFrom::Start(pos))?;
        // Room for a 64-bit size and a `uuid` extended type.
        let mut hdr = [0u8; 32];
        let n = read_up_to(r, &mut hdr)?;
        let Some(h) = parse_header(&hdr[..n], remaining) else {
            if pos == 0 {
                return Err(Mp4Error::NotMp4("no valid box header at offset 0".into()));
            }
            match cut_header(&hdr[..n], pos) {
                Some(cut) => tree.truncated = Some(cut),
                None => tree.trailing_bytes = remaining,
            }
            break;
        };
        // At the start of a file known to be MP4, any box is the first.
        let any = pos == 0 && first == FirstBox::Any;
        if pos == 0 && !any && !FIRST_BOXES.contains(&&h.fourcc.0) {
            return Err(Mp4Error::NotMp4(format!(
                "first box type `{}` is not an MP4 box",
                h.fourcc
            )));
        }
        if h.size > remaining && !any && !top_level_types().any(|t| *t == h.fourcc.0) {
            // A bare vendor trailer, as older Insta360 bodies such as the
            // ONE X2 append theirs (reported), is not a box, but its first
            // bytes can look like an oversized one. The X5 wraps its
            // trailer in a top-level `inst` box, already at firmware
            // v1.1.22 (all 14 of Pat's files; docs/formats/insta360.md),
            // so its trailer is read as a box.
            tree.trailing_bytes = remaining;
            break;
        }
        if h.size > remaining {
            tree.truncated = Some(Truncated {
                fourcc: Some(h.fourcc),
                offset: pos,
                header_len: h.header_len,
                size: Some(h.size),
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

/// Box types that MP4 and MOV files start with.
const FIRST_BOXES: &[&[u8; 4]] = &[
    b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot", b"uuid",
];

/// Other box types that appear at the top level of MP4 and MOV files.
const OTHER_TOP_LEVEL: &[&[u8; 4]] = &[
    b"moof", b"mfra", b"meta", b"udta", b"sidx", b"styp", b"inst",
];

fn top_level_types() -> impl Iterator<Item = &'static [u8; 4]> {
    FIRST_BOXES.iter().chain(OTHER_TOP_LEVEL).copied()
}

/// The box at `offset` if `b`, the rest of the file, is the start of its
/// header: fewer bytes than a header needs, which agree with one. Other
/// short leftovers are trailing bytes.
fn cut_header(b: &[u8], offset: u64) -> Option<Truncated> {
    let size32 = b
        .get(0..4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_be_bytes);
    let header_len = if size32 == Some(1) { 16 } else { 8 };
    if b.len() >= header_len {
        // A whole header that did not parse.
        return None;
    }
    // Sizes 2 to 7 are smaller than a header.
    if size32.is_some_and(|s| (2..8).contains(&s)) {
        return None;
    }
    // As much of the type as the file holds must start a type we know.
    let fourcc = b.get(4..).unwrap_or_default();
    let fourcc = &fourcc[..fourcc.len().min(4)];
    if !top_level_types().any(|t| t.starts_with(fourcc)) {
        return None;
    }
    Some(Truncated {
        fourcc: fourcc.try_into().ok().map(FourCc),
        offset,
        header_len: header_len as u64,
        size: size32.filter(|&s| s >= 8).map(u64::from),
        present: b.len() as u64,
    })
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
    /// The size field was 0: the box runs to the end of its parent.
    to_end: bool,
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
        to_end: size32 == 0,
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
    } else if body.len() <= MAX_KEPT_LEAF
        || SAMPLE_TABLES.contains(&&b.fourcc.0)
        || starts_with_box(body, b"tkhd")
    {
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
    h.size <= (buf.len() - at) as u64 && h.fourcc.0.iter().all(|c| c.is_ascii_graphic())
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
        // ISO/IEC 14496-12 allows a size of 0 only for the last top-level
        // box. Inside one, the box runs to the end of its parent, so FFmpeg
        // reads the boxes after it as its payload and loses them. Zero
        // bytes to the end are padding, or QuickTime's terminator, which
        // unprintable_types allows only at the end of a sample entry.
        if h.to_end && rest.iter().any(|&x| x != 0) {
            walk.warn(format!(
                "child box `{}` at {at} has size 0, which only the last top-level box may have; anything after it in its parent is read as part of it",
                h.fourcc
            ));
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

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// The entries of a sample table box: a u32 count at `count_at` in the
/// payload, then `count` entries of `width` bytes, checked to fit.
fn table_entries(b: &Mp4Box, count_at: usize, width: usize) -> Result<(u64, &[u8]), String> {
    let count = be_u32(&b.payload, count_at)
        .ok_or_else(|| format!("`{}` is too short to hold its entry count", b.fourcc))?;
    let data = b.payload.get(count_at + 4..).unwrap_or_default();
    let need = count as u64 * width as u64;
    if need > data.len() as u64 {
        return Err(format!(
            "`{}` claims {count} entries but has room for {}",
            b.fourcc,
            data.len() / width
        ));
    }
    Ok((count as u64, &data[..need as usize]))
}

/// Sample sizes from `stsz` or `stz2`.
enum SampleSizes<'a> {
    Constant(u64),
    /// Big-endian fields of 4, 8, 16 or 32 bits.
    Table {
        bits: u64,
        data: &'a [u8],
    },
}

impl SampleSizes<'_> {
    fn get(&self, k: u64) -> u64 {
        let (bits, data) = match self {
            SampleSizes::Constant(s) => return *s,
            SampleSizes::Table { bits, data } => (*bits, *data),
        };
        let at = (k * bits / 8) as usize;
        let byte = |i: usize| data.get(i).copied().unwrap_or(0) as u64;
        match bits {
            32 => be_u32(data, at).unwrap_or(0) as u64,
            16 => byte(at) << 8 | byte(at + 1),
            8 => byte(at),
            _ if k.is_multiple_of(2) => byte(at) >> 4,
            _ => byte(at) & 0xf,
        }
    }

    /// Bytes in samples `start..start + n`, or None where the table does not
    /// say. QuickTime gives uncompressed audio samples a nominal size of 1;
    /// their real size is in the sample description.
    fn total(&self, start: u64, n: u64) -> Option<u64> {
        match self {
            SampleSizes::Constant(s) if *s <= 1 => None,
            SampleSizes::Constant(s) => s.checked_mul(n),
            SampleSizes::Table { .. } => Some((start..start + n).map(|k| self.get(k)).sum()),
        }
    }
}

/// Read `stsz` or `stz2`: the sample count and sizes.
fn sample_sizes(stbl: &Mp4Box) -> Result<Option<(u64, SampleSizes<'_>)>, String> {
    let short = |b: &Mp4Box| format!("`{}` is too short", b.fourcc);
    if let Some(b) = stbl.child(b"stsz") {
        let constant = be_u32(&b.payload, 4).ok_or_else(|| short(b))?;
        if constant != 0 {
            let n = be_u32(&b.payload, 8).ok_or_else(|| short(b))?;
            return Ok(Some((n as u64, SampleSizes::Constant(constant as u64))));
        }
        let (n, data) = table_entries(b, 8, 4)?;
        return Ok(Some((n, SampleSizes::Table { bits: 32, data })));
    }
    let Some(b) = stbl.child(b"stz2") else {
        return Ok(None);
    };
    let bits = b.payload.get(7).copied().ok_or_else(|| short(b))? as u64;
    if !matches!(bits, 4 | 8 | 16) {
        return Err(format!("`stz2` has {bits}-bit sizes"));
    }
    let n = be_u32(&b.payload, 8).ok_or_else(|| short(b))? as u64;
    let data = b.payload.get(12..).unwrap_or_default();
    let need = (n * bits).div_ceil(8);
    if need > data.len() as u64 {
        return Err(format!(
            "`stz2` claims {n} entries but has room for {}",
            data.len() as u64 * 8 / bits
        ));
    }
    let data = &data[..need as usize];
    Ok(Some((n, SampleSizes::Table { bits, data })))
}

/// Whether every data reference of a track points into this file. Tracks
/// whose data lives elsewhere have chunk offsets into other files.
fn self_contained(minf: &Mp4Box) -> bool {
    let Some(dref) = minf.path(&[b"dinf", b"dref"]) else {
        return true;
    };
    let mut c = Cursor::new(&dref.payload);
    let (Some(_), Some(n)) = (c.version_flags(), c.u32()) else {
        return true;
    };
    for _ in 0..n.min(64) {
        // Each entry is a FullBox whose flag 1 means "in this file".
        let (Some(size), Some(_), Some((_, flags))) = (c.u32(), c.take(4), c.version_flags())
        else {
            return true;
        };
        if flags & 1 == 0 {
            return false;
        }
        if c.take((size as usize).saturating_sub(12)).is_none() {
            return true;
        }
    }
    true
}

/// Containers whose children all have registered types, which are
/// printable ASCII, as do the children of the sample entries we know. Not
/// `udta`, `meta` or `ilst`: QuickTime's `©nam` and the like start with
/// the byte 0xA9.
const REGISTERED_CHILDREN: &[&[u8; 4]] = &[
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd", b"sv3d", b"proj", b"edts", b"dinf",
    b"mvex", b"tref",
];

/// Whether a box type is text, as registered types are.
fn is_text(t: FourCc) -> bool {
    t.0.iter().all(|&x| x.is_ascii_graphic() || x == b' ')
}

/// List the children of `b`, a box that should hold only registered
/// types, whose type is not printable, and look the same way inside each
/// child that should too. Such a type is a damaged header. FFmpeg skips
/// the box as one it does not know, so a `trak` whose type reads `\xd1rak`
/// loses its stream without a word, and a damaged `sv3d` its projection.
/// Four zero bytes are allowed only as the type of a sample entry's last
/// child with nothing but zero bytes after its header, QuickTime's
/// terminator; anywhere else they are a header zeroed with the rest of
/// its box.
fn unprintable_types(b: &Mp4Box, out: &mut Vec<String>) {
    let entry = entry_handler(b.fourcc).is_some();
    for (i, c) in b.children.iter().enumerate() {
        let t = c.fourcc;
        let terminator = entry && t.0 == [0; 4] && i + 1 == b.children.len() && all_zero(c);
        if !is_text(t) && !terminator {
            out.push(format!(
                "box `{t}` at offset {} in `{}` has a type that is not text, so its header is damaged",
                c.offset, b.fourcc
            ));
        } else if REGISTERED_CHILDREN.contains(&&t.0) || entry_handler(t).is_some() {
            unprintable_types(c, out);
        }
    }
}

/// Whether every byte after the header of `b`, a leaf, is zero, as a
/// terminator's are. A box whose type was zeroed still holds its own
/// bytes, and with its size zeroed too it runs to the end of its parent
/// and holds the boxes after it, such as an `sv3d` that FFmpeg then
/// skips. A payload too large to keep counts as not zero.
fn all_zero(b: &Mp4Box) -> bool {
    b.payload.len() as u64 == b.size - b.header_len && b.payload.iter().all(|&x| x == 0)
}

/// Whether `body` starts with a box of type `t`, as the payload of a
/// `trak` starts with a track header (`tkhd`) and that of an `sv3d` with
/// a spherical video header (`svhd`).
fn starts_with_box(body: &[u8], t: &[u8; 4]) -> bool {
    body.get(4..8) == Some(&t[..]) && looks_like_box(body, 0)
}

/// List the children of `moov` with a printable type other than `trak`
/// whose payload starts with a track header. Each is a `trak` whose type
/// was damaged into another printable one, such as `Trak` or `free`,
/// which FFmpeg skips with its stream, as it does a type that is not
/// text. Their payloads are kept whatever their size, so this holds for
/// long recordings too.
fn renamed_traks(moov: &Mp4Box, out: &mut Vec<String>) {
    for c in &moov.children {
        let t = c.fourcc;
        if is_text(t) && t.0 != *b"trak" && starts_with_box(&c.payload, b"tkhd") {
            out.push(format!(
                "box `{t}` at offset {} in `moov` starts with a track header (`tkhd`), so it is a `trak` whose type is damaged and FFmpeg skips its stream",
                c.offset
            ));
        }
    }
}

/// List each `sv3d` in the tracks' sample entries that lacks a box FFmpeg
/// needs to read it: `svhd`, `proj`, or the `prhd` in `proj`. FFmpeg then
/// ignores the whole `sv3d`, so a `proj` whose type reads `qroj` loses the
/// projection with no more than a line in its log. Also list each child
/// of a sample entry with a printable type other than `sv3d` whose
/// payload starts with `svhd`, which only `sv3d` holds: an `sv3d` whose
/// type was damaged into another printable one, such as `rv3d`, which
/// FFmpeg skips with the projection, as it does a type that is not text.
fn lost_projections(moov: &Mp4Box, out: &mut Vec<String>) {
    let entries = moov
        .children_of(b"trak")
        .filter_map(|t| t.path(&[b"mdia", b"minf", b"stbl", b"stsd"]))
        .flat_map(|stsd| &stsd.children);
    for entry in entries {
        for c in &entry.children {
            let t = c.fourcc;
            if is_text(t) && t.0 != *b"sv3d" && starts_with_box(&c.payload, b"svhd") {
                out.push(format!(
                    "box `{t}` at offset {} in `{}` starts with a spherical video header (`svhd`), so it is an `sv3d` whose type is damaged and FFmpeg ignores its projection",
                    c.offset, entry.fourcc
                ));
            }
        }
        for sv3d in entry.children_of(b"sv3d") {
            let proj = sv3d.child(b"proj");
            let missing: Vec<&str> = [
                (sv3d.child(b"svhd").is_some(), "`svhd`"),
                (proj.is_some(), "`proj`"),
                (
                    proj.is_none_or(|p| p.child(b"prhd").is_some()),
                    "`proj/prhd`",
                ),
            ]
            .into_iter()
            .filter_map(|(have, name)| (!have).then_some(name))
            .collect();
            if !missing.is_empty() {
                out.push(format!(
                    "`sv3d` at offset {} in `{}` has no {}, so FFmpeg ignores its projection",
                    sv3d.offset,
                    entry.fourcc,
                    missing.join(" or ")
                ));
            }
        }
    }
}

/// The timescale of a `mvhd` or `mdhd` if FFmpeg cannot use it. FFmpeg
/// reads it as a signed 32-bit number and takes 1 for one that is not
/// positive, which puts every time in the movie or track out of place.
fn bad_timescale(b: &Mp4Box) -> Option<u32> {
    let mut c = Cursor::new(&b.payload);
    let (version, _) = c.version_flags()?;
    // Creation and modification times come first.
    c.take(if version == 1 { 16 } else { 8 })?;
    c.u32().filter(|&ts| ts == 0 || ts > i32::MAX as u32)
}

/// Check one track's timescale and sample tables against each other and
/// against the media data, and return the first problem.
fn check_track(trak: &Mp4Box, mdats: &[(u64, u64)], file_size: u64) -> Result<(), String> {
    let mdia = trak.child(b"mdia").ok_or("it has no `mdia` box")?;
    if let Some(ts) = mdia.child(b"mdhd").and_then(bad_timescale) {
        return Err(format!(
            "its `mdhd` gives timescale {ts}, so its times are wrong"
        ));
    }
    if mdia.child(b"hdlr").is_none() {
        return Err("it has no handler (`mdia/hdlr`)".into());
    }
    let minf = mdia.child(b"minf").ok_or("it has no `minf` box")?;
    let stbl = minf
        .child(b"stbl")
        .ok_or("it has no sample table (`stbl`)")?;
    // FFmpeg takes the stream kind from the handler, and then cannot read
    // a sample entry of the other kind.
    let kind = |t: FourCc| match &t.0 {
        b"vide" => Some("video"),
        b"soun" => Some("audio"),
        _ => None,
    };
    let handler = handler_type(trak).and_then(|h| Some((h, kind(h)?)));
    for entry in stbl.child(b"stsd").into_iter().flat_map(|s| &s.children) {
        let entry_kind = entry_handler(entry.fourcc).and_then(kind);
        if let (Some((h, want)), Some(is)) = (handler, entry_kind)
            && want != is
        {
            return Err(format!(
                "its handler says {want} (`{h}`), but its sample entry `{}` is {is}",
                entry.fourcc
            ));
        }
    }
    let sizes = sample_sizes(stbl)?;
    let stts = stbl.child(b"stts").map(|b| table_entries(b, 4, 8));
    let stsc = stbl.child(b"stsc").map(|b| table_entries(b, 4, 12));
    let (co, wide) = match (stbl.child(b"stco"), stbl.child(b"co64")) {
        (Some(b), _) => (Some(b), false),
        (None, b) => (b, true),
    };
    let co = co.map(|b| table_entries(b, 4, if wide { 8 } else { 4 }));
    let samples = sizes.as_ref().map_or(0, |s| s.0);
    let timed: u64 = match &stts {
        Some(Ok((_, e))) => e
            .chunks_exact(8)
            .filter_map(|e| be_u32(e, 0))
            .map(u64::from)
            .sum(),
        Some(Err(why)) => return Err(why.clone()),
        None => 0,
    };
    let (chunk_count, chunks) = co.clone().transpose()?.unwrap_or((0, &[]));
    if samples == 0 && timed == 0 && chunk_count == 0 {
        // An empty track, or the `moov` of a fragmented file.
        return Ok(());
    }
    let missing: Vec<&str> = [
        (sizes.is_some(), "`stsz`"),
        (stts.is_some(), "`stts`"),
        (stsc.is_some(), "`stsc`"),
        (co.is_some(), "`stco`"),
    ]
    .into_iter()
    .filter_map(|(have, name)| (!have).then_some(name))
    .collect();
    if !missing.is_empty() {
        return Err(format!("it has samples but no {}", missing.join(" or ")));
    }
    if timed != samples {
        return Err(format!(
            "`stts` times {timed} samples but `stsz` sizes {samples}"
        ));
    }

    let (runs, entries) = stsc.transpose()?.unwrap_or((0, &[]));
    if runs == 0 {
        return Err(format!(
            "`stsc` puts none of the {samples} samples into the {chunk_count} chunks"
        ));
    }
    let descriptions = stbl.child(b"stsd").map_or(0, |s| s.children.len() as u64);
    let runs = ChunkRuns::new(entries, chunk_count, descriptions)?;
    let placed: u128 = runs
        .iter()
        .map(|r| r.chunks as u128 * r.per_chunk as u128)
        .sum();
    if placed != samples as u128 {
        return Err(format!(
            "`stsc` and `stco` place {placed} samples but `stsz` sizes {samples}"
        ));
    }

    if !self_contained(minf) {
        return Ok(());
    }
    let Some((_, sizes)) = sizes else {
        return Ok(());
    };
    // Every chunk's bytes must be in the file and inside an `mdat`. The
    // runs place exactly `samples` samples, so `sample` stays below it.
    let mut sample = 0u64;
    for run in runs.iter() {
        for c in run.first..run.first + run.chunks {
            let at = (c - 1) as usize;
            let offset = if wide {
                let hi = be_u32(chunks, at * 8).unwrap_or(0) as u64;
                hi << 32 | be_u32(chunks, at * 8 + 4).unwrap_or(0) as u64
            } else {
                be_u32(chunks, at * 4).unwrap_or(0) as u64
            };
            check_chunk(
                c,
                offset,
                sizes.total(sample, run.per_chunk),
                mdats,
                file_size,
            )?;
            sample += run.per_chunk;
        }
    }
    Ok(())
}

/// Chunks `first..first + chunks`, counted from 1, hold `per_chunk`
/// samples each.
#[derive(Debug, Clone, Copy)]
struct ChunkRun {
    first: u64,
    chunks: u64,
    per_chunk: u64,
}

/// The entries of an `stsc` table, which map runs of chunks to samples
/// per chunk, checked so that every run covers at least one chunk.
struct ChunkRuns<'a> {
    /// 12-byte entries: first chunk, samples per chunk, sample
    /// description index.
    entries: &'a [u8],
    chunk_count: u64,
}

impl<'a> ChunkRuns<'a> {
    /// Check that the runs start at increasing chunk numbers between 1
    /// and `chunk_count`, put samples in their chunks, and use one of the
    /// `descriptions` sample descriptions.
    fn new(entries: &'a [u8], chunk_count: u64, descriptions: u64) -> Result<Self, String> {
        let mut prev = 0;
        for (k, e) in entries.chunks_exact(12).enumerate() {
            let [first, per_chunk, description] = stsc_entry(e);
            if first == 0 {
                return Err(format!(
                    "`stsc` entry {k} starts at chunk 0, but chunks count from 1"
                ));
            }
            if first <= prev {
                return Err(format!(
                    "`stsc` entry {k} starts at chunk {first}, but the entry before it starts at chunk {prev}"
                ));
            }
            if first > chunk_count {
                return Err(format!(
                    "`stsc` entry {k} starts at chunk {first}, but there are only {chunk_count} chunks"
                ));
            }
            if per_chunk == 0 {
                return Err(format!("`stsc` entry {k} puts no samples in its chunks"));
            }
            if description == 0 || description > descriptions {
                return Err(format!(
                    "`stsc` entry {k} uses sample description {description}, but `stsd` has {descriptions}"
                ));
            }
            prev = first;
        }
        Ok(ChunkRuns {
            entries,
            chunk_count,
        })
    }

    /// Each run ends where the next one starts, the last one at the last
    /// chunk.
    fn iter(&self) -> impl Iterator<Item = ChunkRun> + '_ {
        let entries = self.entries.chunks_exact(12);
        let ends = entries
            .clone()
            .skip(1)
            .map(|e| stsc_entry(e)[0])
            .chain(std::iter::once(self.chunk_count + 1));
        entries.zip(ends).enumerate().map(|(k, (e, end))| {
            let [first, per_chunk, _] = stsc_entry(e);
            // Like FFmpeg, let the first run cover any chunks before it.
            let first = if k == 0 { 1 } else { first };
            ChunkRun {
                first,
                // new() checked that runs start at increasing chunks up to
                // `chunk_count`, so this never saturates.
                chunks: end.saturating_sub(first),
                per_chunk,
            }
        })
    }
}

/// An `stsc` entry's first chunk, samples per chunk and description index.
fn stsc_entry(e: &[u8]) -> [u64; 3] {
    [0, 4, 8].map(|i| be_u32(e, i).unwrap_or(0) as u64)
}

/// Chunk `c` holds `size` bytes (None: at least one) at `offset`.
fn check_chunk(
    c: u64,
    offset: u64,
    size: Option<u64>,
    mdats: &[(u64, u64)],
    file_size: u64,
) -> Result<(), String> {
    if size == Some(0) {
        return Ok(());
    }
    let what = match size {
        Some(s) => format!("chunk {c} (bytes {offset}..{})", offset.saturating_add(s)),
        None => format!("chunk {c} (at {offset})"),
    };
    let end = offset
        .checked_add(size.unwrap_or(1))
        .ok_or_else(|| format!("{what} overflows"))?;
    // Not necessarily truncation: a corrupt offset does this too.
    if end > file_size {
        return Err(format!(
            "{what} runs past the end of the file ({file_size} bytes)"
        ));
    }
    let i = mdats.partition_point(|&(start, _)| start <= offset);
    if i == 0 || end > mdats[i - 1].1 {
        return Err(format!("{what} lies outside the media data (`mdat`)"));
    }
    Ok(())
}

/// Video sample entry types we know.
const VIDEO_ENTRIES: &[&[u8; 4]] = &[
    b"avc1", b"avc3", b"hvc1", b"hev1", b"av01", b"vp09", b"apch", b"apcn", b"apcs", b"apco",
    b"ap4h", b"ap4x", b"mp4v", b"dvh1", b"dvhe", b"encv",
];

/// Audio sample entry types we know.
const AUDIO_ENTRIES: &[&[u8; 4]] = &[
    b"mp4a", b"Opus", b"lpcm", b"sowt", b"twos", b"ipcm", b"fLaC", b"ac-3", b"ec-3", b"alac",
    b"in24", b"in32", b"fl32", b"enca",
];

/// The track ID from `tkhd`, which FFmpeg's MOV demuxer uses as the
/// stream ID.
pub fn track_id(trak: &Mp4Box) -> Option<u32> {
    let tkhd = trak.child(b"tkhd")?;
    let mut c = Cursor::new(&tkhd.payload);
    let (version, _) = c.version_flags()?;
    // Creation and modification times come first.
    c.take(if version == 1 { 16 } else { 8 })?;
    c.u32()
}

/// How long a track's samples last, from its `stts`, in ticks of its
/// `mdhd` timescale. These are decode-time deltas, which for a stream of
/// equal frames are the frame duration whatever the frame order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleTiming {
    pub timescale: u32,
    pub samples: u64,
    /// The duration of every sample but the last, when they all have the
    /// same one, which is not 0, and the samples are also shown that far
    /// apart: their composition offsets (`ctts`) put each at one of the
    /// decode times, shifted alike, so that no two tie and none is left
    /// out, and the edit list shows them once, straight through. The last
    /// sample's duration is often a muxer's guess, so it doesn't count.
    pub constant: Option<u32>,
}

/// Samples whose composition offsets are checked one by one at most:
/// 2^26, 12 days at 60 fps, in a bit set of 8 MiB.
const MAX_TIMED_SAMPLES: u64 = 1 << 26;

/// Whether `samples` samples `d` ticks apart in decode time, shown at
/// their decode times plus `stbl`'s composition offsets (`ctts`), are
/// shown `d` apart too: each offset is the same remainder of `d`, and the
/// times they are shown at are distinct and leave no slot of the grid
/// out. Without a `ctts`, every sample is shown at its decode time.
fn shown_evenly(stbl: &Mp4Box, samples: u64, d: u32) -> bool {
    let Some(ctts) = stbl.child(b"ctts") else {
        return true;
    };
    let Ok((_, entries)) = table_entries(ctts, 4, 8) else {
        return false;
    };
    if samples > MAX_TIMED_SAMPLES {
        return false;
    }
    let d = i64::from(d);
    // Offsets are signed in version 1 and, as FFmpeg reads them, in
    // version 0 too.
    let runs: Vec<(u64, i64)> = entries
        .chunks_exact(8)
        .filter_map(|e| Some((u64::from(be_u32(e, 0)?), i64::from(be_u32(e, 4)? as i32))))
        .collect();
    if runs.iter().map(|r| r.0).sum::<u64>() != samples {
        return false;
    }
    // Sample i is shown at grid slot i + (offset - r) / d; the first
    // slot a run fills is its first sample's.
    let Some(&(_, first)) = runs.first() else {
        return samples == 0;
    };
    let r = first.rem_euclid(d);
    let mut start = 0u64;
    let mut lowest = i64::MAX;
    for &(n, off) in &runs {
        if off.rem_euclid(d) != r {
            return false;
        }
        let slot = start as i64 + (off - r) / d;
        lowest = lowest.min(slot);
        start += n;
    }
    let mut seen = vec![0u64; samples.div_ceil(64) as usize];
    let mut i = 0u64;
    for &(n, off) in &runs {
        for _ in 0..n {
            let Ok(k) = u64::try_from(i as i64 + (off - r) / d - lowest) else {
                return false;
            };
            if k >= samples {
                return false;
            }
            let (word, bit) = ((k / 64) as usize, 1u64 << (k % 64));
            if seen[word] & bit != 0 {
                return false;
            }
            seen[word] |= bit;
            i += 1;
        }
    }
    true
}

/// Whether a track's edit list (`edts/elst`) shows its media once and
/// straight through: at most one edit that isn't empty (media time −1),
/// played at rate 1. Edits that splice, repeat or hold the media show
/// frames at times its `stts` and `ctts` don't say, and FFmpeg drops
/// frames where one edit meets the next. No edit list shows it all; one
/// that can't be read counts as not.
fn plays_once(trak: &Mp4Box) -> bool {
    let Some(elst) = trak.path(&[b"edts", b"elst"]) else {
        return true;
    };
    let mut c = Cursor::new(&elst.payload);
    let Some((version, _)) = c.version_flags() else {
        return false;
    };
    // Each edit: its duration, media time and rate, the first two of 4
    // bytes each in version 0 and 8 in version 1.
    let field = if version == 1 { 8 } else { 4 };
    let Ok((_, entries)) = table_entries(elst, 4, 2 * field + 4) else {
        return false;
    };
    let mut shown = 0;
    for e in entries.chunks_exact(2 * field + 4) {
        if e.get(field..2 * field)
            .is_some_and(|t| t.iter().all(|&b| b == 0xff))
        {
            continue;
        }
        // Rate 1.0: integer part 1, fraction 0.
        if be_u32(e, 2 * field) != Some(0x0001_0000) {
            return false;
        }
        shown += 1;
    }
    shown <= 1
}

/// One edit of a track's edit list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    /// How long it shows, in the movie's timescale (`mvhd`).
    pub duration: u64,
    /// Where in the media it starts, in the track's timescale (`mdhd`);
    /// −1 for an empty edit.
    pub media_time: i64,
    /// 16.16 fixed point: `0x0001_0000` plays at rate 1.
    pub rate: u32,
}

/// A track's edit list (`edts/elst`), `None` without one or when it
/// can't be read.
pub fn edit_list(trak: &Mp4Box) -> Option<Vec<Edit>> {
    let elst = trak.path(&[b"edts", b"elst"])?;
    let mut c = Cursor::new(&elst.payload);
    let (version, _) = c.version_flags()?;
    let field = if version == 1 { 8 } else { 4 };
    let (_, entries) = table_entries(elst, 4, 2 * field + 4).ok()?;
    entries
        .chunks_exact(2 * field + 4)
        .map(|e| {
            let (duration, media_time) = if version == 1 {
                let d = u64::from_be_bytes(e.get(0..8)?.try_into().ok()?);
                let t = i64::from_be_bytes(e.get(8..16)?.try_into().ok()?);
                (d, t)
            } else {
                let d = u64::from(be_u32(e, 0)?);
                let t = i64::from(be_u32(e, 4)? as i32);
                (d, t)
            };
            Some(Edit {
                duration,
                media_time,
                rate: be_u32(e, 2 * field)?,
            })
        })
        .collect()
}

/// The movie's timescale (`moov/mvhd`), ticks a second.
pub fn movie_timescale(tree: &Mp4Tree) -> Option<u32> {
    let mvhd = tree.moov()?.child(b"mvhd")?;
    let mut c = Cursor::new(&mvhd.payload);
    let (version, _) = c.version_flags()?;
    c.take(if version == 1 { 16 } else { 8 })?;
    c.u32().filter(|&t| t > 0)
}

pub fn sample_timing(trak: &Mp4Box) -> Option<SampleTiming> {
    let mdhd = trak.path(&[b"mdia", b"mdhd"])?;
    let mut c = Cursor::new(&mdhd.payload);
    let (version, _) = c.version_flags()?;
    c.take(if version == 1 { 16 } else { 8 })?;
    let timescale = c.u32().filter(|&t| t > 0)?;
    let stbl = trak.path(&[b"mdia", b"minf", b"stbl"])?;
    let stts = stbl.child(b"stts")?;
    let (_, entries) = table_entries(stts, 4, 8).ok()?;
    let mut runs: Vec<(u64, u32)> = entries
        .chunks_exact(8)
        .filter_map(|e| Some((u64::from(be_u32(e, 0)?), be_u32(e, 4)?)))
        .filter(|&(n, _)| n > 0)
        .collect();
    let samples = runs.iter().map(|r| r.0).sum();
    // Leave out the last sample, unless it is the only one.
    if samples > 1
        && let Some(last) = runs.last_mut()
    {
        last.0 -= 1;
    }
    let mut deltas = runs.iter().filter(|r| r.0 > 0).map(|r| r.1);
    let first = deltas.next();
    let constant = first
        .filter(|&d| d > 0 && deltas.all(|e| e == d))
        .filter(|&d| shown_evenly(stbl, samples, d) && plays_once(trak));
    Some(SampleTiming {
        timescale,
        samples,
        constant,
    })
}

/// The handler type of a `trak` (`mdia/hdlr`): `vide`, `soun`, `meta`, ...
pub fn handler_type(trak: &Mp4Box) -> Option<FourCc> {
    let hdlr = trak.path(&[b"mdia", b"hdlr"])?;
    Some(FourCc(hdlr.payload.get(8..12)?.try_into().ok()?))
}

/// The handler type (`vide` or `soun`) of tracks whose sample entries
/// have type `t`, for the entry types we know.
fn entry_handler(t: FourCc) -> Option<FourCc> {
    if VIDEO_ENTRIES.contains(&&t.0) {
        Some(FourCc(*b"vide"))
    } else if AUDIO_ENTRIES.contains(&&t.0) {
        Some(FourCc(*b"soun"))
    } else {
        None
    }
}

/// Whether sample entries of type `t` hold a codec that FFmpeg reads: the
/// video and audio entry types we know, except the encrypted `encv` and
/// `enca`, which need a key.
pub fn is_known_codec(t: FourCc) -> bool {
    entry_handler(t).is_some() && !matches!(&t.0, b"encv" | b"enca")
}

/// Length of a sample entry's fixed fields before its child boxes, for the
/// entry types we know.
pub(crate) fn sample_entry_fixed_len(t: FourCc, payload: &[u8]) -> Option<usize> {
    if VIDEO_ENTRIES.contains(&&t.0) {
        Some(78)
    } else if AUDIO_ENTRIES.contains(&&t.0) {
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

    /// A `trak` with track ID `id`, an `mdhd` timescale and `stts` runs
    /// of (samples, delta).
    fn timed_trak(id: u32, timescale: u32, runs: &[(u32, u32)]) -> Vec<u8> {
        timed_trak_with(id, timescale, runs, None)
    }

    /// A `trak` with `stts` runs and, if given, `ctts` runs of composition
    /// offsets.
    fn timed_trak_with(
        id: u32,
        timescale: u32,
        runs: &[(u32, u32)],
        offsets: Option<&[(u32, i32)]>,
    ) -> Vec<u8> {
        let mut tkhd = vec![0u8; 8];
        tkhd.extend_from_slice(&id.to_be_bytes());
        tkhd.extend_from_slice(&[0; 12]);
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&[0; 8]);
        let mut stts = (runs.len() as u32).to_be_bytes().to_vec();
        for (n, d) in runs {
            stts.extend_from_slice(&n.to_be_bytes());
            stts.extend_from_slice(&d.to_be_bytes());
        }
        let mut tables = full(b"stts", 0, 0, &stts);
        if let Some(offsets) = offsets {
            let mut ctts = (offsets.len() as u32).to_be_bytes().to_vec();
            for (n, o) in offsets {
                ctts.extend_from_slice(&n.to_be_bytes());
                ctts.extend_from_slice(&o.to_be_bytes());
            }
            tables.extend(full(b"ctts", 1, 0, &ctts));
        }
        let stbl = bx(b"stbl", &tables);
        let mdia = bx(
            b"mdia",
            &cat(&[full(b"mdhd", 0, 0, &mdhd), bx(b"minf", &stbl)]),
        );
        bx(b"trak", &cat(&[full(b"tkhd", 0, 0, &tkhd), mdia]))
    }

    #[test]
    fn sample_timing_leaves_out_the_last_sample() {
        let timing = |runs: &[(u32, u32)]| {
            let f = cat(&[
                bx(b"ftyp", b"isom\0\0\x02\0isom"),
                bx(b"moov", &timed_trak(7, 15360, runs)),
            ]);
            let t = read(&mut io::Cursor::new(f)).unwrap();
            let trak = t.tracks().next().unwrap();
            assert_eq!(track_id(trak), Some(7));
            sample_timing(trak).unwrap()
        };
        // 30 fps at 1/15360, with a last sample of a muxer's guess.
        assert_eq!(
            timing(&[(29, 512), (1, 300)]),
            SampleTiming {
                timescale: 15360,
                samples: 30,
                constant: Some(512)
            }
        );
        assert_eq!(timing(&[(30, 512)]).constant, Some(512));
        assert_eq!(timing(&[(1, 512)]).constant, Some(512));
        // A variable rate, and samples of no duration.
        assert_eq!(timing(&[(10, 512), (5, 1024), (1, 512)]).constant, None);
        assert_eq!(timing(&[(29, 0), (1, 512)]).constant, None);
        assert_eq!(timing(&[]).samples, 0);
    }

    #[test]
    fn a_constant_rate_needs_its_frames_shown_as_evenly_as_decoded() {
        let constant = |offsets: &[(u32, i32)]| {
            let f = cat(&[
                bx(b"ftyp", b"isom\0\0\x02\0isom"),
                bx(
                    b"moov",
                    &timed_trak_with(1, 15360, &[(4, 512)], Some(offsets)),
                ),
            ]);
            let t = read(&mut io::Cursor::new(f)).unwrap();
            sample_timing(t.tracks().next().unwrap()).unwrap().constant
        };
        // I0 P3 B1 B2 in decode order, shown a frame later than decoded:
        // at 512, 2048, 1024 and 1536.
        assert_eq!(constant(&[(1, 512), (1, 1536), (2, 0)]), Some(512));
        // The same with a negative offset (ctts version 1).
        assert_eq!(constant(&[(1, 0), (1, 1024), (2, -512)]), Some(512));
        // Two frames shown at 512, and none at 1536.
        assert_eq!(constant(&[(1, 512), (1, 0), (2, 512)]), None);
        // Shown 2048 after the decode time of the last: a slot left out.
        assert_eq!(constant(&[(3, 0), (1, 512)]), None);
        // Half a frame late.
        assert_eq!(constant(&[(3, 0), (1, 256)]), None);
        // Offsets for 3 samples of 4.
        assert_eq!(constant(&[(3, 0)]), None);
    }

    #[test]
    fn a_constant_rate_needs_an_edit_list_that_plays_the_media_once() {
        // Edits of (duration, media time, rate), in version 0 or 1.
        let constant = |version: u8, edits: &[(u32, i32, u32)]| {
            let mut elst = (edits.len() as u32).to_be_bytes().to_vec();
            for &(d, t, rate) in edits {
                match version {
                    1 => {
                        elst.extend_from_slice(&u64::from(d).to_be_bytes());
                        elst.extend_from_slice(&i64::from(t).to_be_bytes());
                    }
                    _ => {
                        elst.extend_from_slice(&d.to_be_bytes());
                        elst.extend_from_slice(&t.to_be_bytes());
                    }
                }
                elst.extend_from_slice(&rate.to_be_bytes());
            }
            let trak = timed_trak(1, 15360, &[(90, 512)]);
            let edts = bx(b"edts", &full(b"elst", version, 0, &elst));
            let f = cat(&[
                bx(b"ftyp", b"isom\0\0\x02\0isom"),
                bx(b"moov", &bx(b"trak", &cat(&[trak[8..].to_vec(), edts]))),
            ]);
            let t = read(&mut io::Cursor::new(f)).unwrap();
            sample_timing(t.tracks().next().unwrap()).unwrap().constant
        };
        let one = 0x0001_0000;
        for version in [0, 1] {
            // A lead-in hidden, after an empty edit that delays the start.
            assert_eq!(constant(version, &[(1000, 1024, one)]), Some(512));
            assert_eq!(
                constant(version, &[(500, -1, one), (1000, 0, one)]),
                Some(512)
            );
            // Two pieces of the media spliced, a piece held, and one played
            // at half speed.
            assert_eq!(
                constant(version, &[(1500, 1024, one), (1500, 39424, one)]),
                None
            );
            assert_eq!(constant(version, &[(1500, 1024, 0)]), None);
            assert_eq!(constant(version, &[(1500, 1024, 0x8000)]), None);
        }
        // An edit list too short for the edits it claims.
        let trak = timed_trak(1, 15360, &[(90, 512)]);
        let edts = bx(b"edts", &full(b"elst", 0, 0, &2u32.to_be_bytes()));
        let f = cat(&[
            bx(b"ftyp", b"isom\0\0\x02\0isom"),
            bx(b"moov", &bx(b"trak", &cat(&[trak[8..].to_vec(), edts]))),
        ]);
        let t = read(&mut io::Cursor::new(f)).unwrap();
        assert_eq!(
            sample_timing(t.tracks().next().unwrap()).unwrap().constant,
            None
        );
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
        assert_eq!(cut.fourcc, Some(FourCc::new(b"moov")));
        assert_eq!(cut.offset, moov_at as u64);
        assert_eq!(
            (cut.size, cut.present),
            (Some(moov.len() as u64), moov.len() as u64 - 10)
        );
        assert!(t.moov().is_none());

        // A cut-short first box is still an MP4, just an unusable one.
        let t = read(&mut io::Cursor::new(f[..20].to_vec())).unwrap();
        assert!(t.top.is_empty());
        assert_eq!(
            t.truncated.and_then(|c| c.fourcc),
            Some(FourCc::new(b"ftyp"))
        );
    }

    #[test]
    fn file_ending_inside_a_box_header_is_truncation() {
        let f = minimal();
        let at = f.len() as u64;
        let next = bx(b"inst", &[7; 100]);
        for cut in 1..8 {
            let g = cat(&[f.clone(), next[..cut].to_vec()]);
            let t = read(&mut io::Cursor::new(g.clone())).unwrap();
            assert_eq!(t.trailing_bytes, 0, "{cut}");
            let c = t.truncated.unwrap();
            assert_eq!((c.offset, c.present, c.fourcc), (at, cut as u64, None));
            assert_eq!(c.size, (cut >= 4).then_some(108), "{cut}");
            let size = if cut >= 4 { "a 108-byte box" } else { "a box" };
            let bytes = if cut == 1 { "byte" } else { "bytes" };
            assert_eq!(
                t.damage(),
                [format!(
                    "the file ends {cut} {bytes} into the header of {size} at offset {at}: it was cut short"
                )]
            );
        }
        // A 64-bit header cut after the type.
        let large = cat(&[1u32.to_be_bytes().to_vec(), b"mdat".to_vec(), vec![0; 7]]);
        let t = read(&mut io::Cursor::new(cat(&[f.clone(), large]))).unwrap();
        let c = t.truncated.unwrap();
        assert_eq!(
            (c.fourcc, c.size, c.header_len, c.present),
            (Some(FourCc::new(b"mdat")), None, 16, 15)
        );
        // Leftovers that cannot start a header are trailing bytes: a size
        // below 8, a type we do not know, a 64-bit size below 16.
        for junk in [
            &b"\0\0\0\x05"[..],
            b"\0\0\0\x10zz",
            b"\0\0\0\x01mdat\0\0\0\0\0\0\0\x0f",
        ] {
            let t = read(&mut io::Cursor::new(cat(&[f.clone(), junk.to_vec()]))).unwrap();
            assert_eq!(t.truncated, None, "{junk:?}");
            assert_eq!(t.trailing_bytes, junk.len() as u64, "{junk:?}");
        }
    }

    #[test]
    fn any_first_box_is_accepted_when_the_file_is_known_to_be_mp4() {
        let mut f = minimal();
        f[4..8].copy_from_slice(b"ftyq");
        assert!(matches!(
            read(&mut io::Cursor::new(f.clone())),
            Err(Mp4Error::NotMp4(_))
        ));
        let t = read_with(&mut io::Cursor::new(f.clone()), FirstBox::Any).unwrap();
        assert_eq!(t.top[0].fourcc, FourCc::new(b"ftyq"));
        assert!(t.moov().is_some());
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
        // Cut short, it is truncation, whatever its type.
        let t = read_with(&mut io::Cursor::new(f[..12].to_vec()), FirstBox::Any).unwrap();
        assert!(t.truncated.is_some_and(|c| c.is(b"ftyq")));
        // A header that is not one is still not MP4.
        f[0..4].copy_from_slice(&5u32.to_be_bytes());
        assert!(read_with(&mut io::Cursor::new(f), FirstBox::Any).is_err());
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

    /// Reset this process's peak resident memory through `clear_refs`
    /// (`/proc/self/clear_refs`), run `f`, and return how far the peak
    /// rose above the starting level, in KiB. `f` runs in any case; the
    /// result is None where the peak cannot be reset or read.
    fn peak_rss_growth_kib(clear_refs: &Path, f: impl FnOnce()) -> Option<u64> {
        fn hwm() -> Option<u64> {
            let s = std::fs::read_to_string("/proc/self/status").ok()?;
            let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
            line.split_whitespace().nth(1)?.parse().ok()
        }
        let before = std::fs::write(clear_refs, "5").ok().and_then(|()| hwm());
        f();
        Some(hwm()?.saturating_sub(before?))
    }

    #[test]
    fn memory_growth_runs_its_closure_when_the_peak_cannot_be_reset() {
        let mut ran = false;
        let grew = peak_rss_growth_kib(Path::new("/nonexistent/clear_refs"), || ran = true);
        assert!(ran);
        assert_eq!(grew, None);
    }

    #[test]
    fn deep_nesting_is_fast_and_small() {
        // The 160 KB file used 1.4 GB and overflowed the stack before the
        // depth limit; the 1.6 MB one ran out of memory.
        for n in [20_000, 200_000] {
            let f = nested_udta(n, &[]);
            let mut result = None;
            let start = std::time::Instant::now();
            let grew = peak_rss_growth_kib(Path::new("/proc/self/clear_refs"), || {
                result = Some(read(&mut io::Cursor::new(&f)))
            });
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

    fn words(w: &[u32]) -> Vec<u8> {
        w.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    /// Sample-table payloads for one video track.
    #[derive(Debug, Clone)]
    struct Tables {
        stts: Vec<u8>,
        stsz: Vec<u8>,
        stsc: Vec<u8>,
        /// Chunk offsets from the start of the `mdat` payload.
        chunks: Vec<u64>,
        co64: bool,
        data_len: usize,
    }

    /// Consistent tables for samples of `sizes` bytes, `per_chunk` to a
    /// chunk, stored back to back.
    fn tables(sizes: &[u32], per_chunk: usize) -> Tables {
        let n = sizes.len() as u32;
        let chunk_count = sizes.len().div_ceil(per_chunk);
        let last = (sizes.len() - (chunk_count - 1) * per_chunk) as u32;
        let stsc = match (chunk_count as u32, per_chunk as u32) {
            (1, _) => words(&[0, 1, 1, last, 1]),
            (_, p) if last == p => words(&[0, 1, 1, p, 1]),
            (c, p) => words(&[0, 2, 1, p, 1, c, last, 1]),
        };
        let mut chunks = Vec::new();
        let mut at = 0u64;
        for chunk in sizes.chunks(per_chunk) {
            chunks.push(at);
            at += chunk.iter().map(|&s| s as u64).sum::<u64>();
        }
        Tables {
            stts: words(&[0, 1, n, 1000]),
            stsz: [words(&[0, 0, n]), words(sizes)].concat(),
            stsc,
            chunks,
            co64: false,
            data_len: at as usize,
        }
    }

    /// `ftyp`, `mdat` and `moov` (or `moov` before `mdat`) for `t`.
    fn file_with(t: &Tables, faststart: bool) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let moov = |base: u64| {
            let n = t.chunks.len() as u32;
            // Fuzzed offsets can be anywhere, so let them wrap.
            let co = if t.co64 {
                let offs: Vec<u8> = t
                    .chunks
                    .iter()
                    .flat_map(|c| c.wrapping_add(base).to_be_bytes())
                    .collect();
                bx(b"co64", &cat(&[words(&[0, n]), offs]))
            } else {
                let offs: Vec<u32> = t
                    .chunks
                    .iter()
                    .map(|c| c.wrapping_add(base) as u32)
                    .collect();
                bx(b"stco", &cat(&[words(&[0, n]), words(&offs)]))
            };
            let hdlr = full(b"hdlr", 0, 0, b"\0\0\0\0vide\0\0\0\0\0\0\0\0\0\0\0\0V\0");
            let entry = bx(b"hvc1", &[0u8; 78]);
            let stsd = full(b"stsd", 0, 0, &cat(&[words(&[1]), entry]));
            let stbl = bx(
                b"stbl",
                &cat(&[
                    stsd,
                    bx(b"stts", &t.stts),
                    bx(b"stsz", &t.stsz),
                    bx(b"stsc", &t.stsc),
                    co,
                ]),
            );
            // Timescale 1000; the times and durations do not matter here.
            let mdhd = full(b"mdhd", 0, 0, &words(&[0, 0, 1000, 0, 0x55c4_0000]));
            let mdia = bx(b"mdia", &cat(&[mdhd, hdlr, bx(b"minf", &stbl)]));
            let mvhd = full(b"mvhd", 0, 0, &cat(&[words(&[0, 0, 1000, 0]), vec![0; 80]]));
            let tkhd = full(b"tkhd", 0, 3, &[0; 80]);
            bx(b"moov", &cat(&[mvhd, bx(b"trak", &cat(&[tkhd, mdia]))]))
        };
        let mdat = bx(b"mdat", &vec![0xee; t.data_len]);
        if faststart {
            let base = (ftyp.len() + moov(0).len() + 8) as u64;
            cat(&[ftyp, moov(base), mdat])
        } else {
            let base = (ftyp.len() + 8) as u64;
            cat(&[ftyp, mdat, moov(base)])
        }
    }

    fn damage_of(f: Vec<u8>) -> Vec<String> {
        read(&mut io::Cursor::new(f)).unwrap().damage()
    }

    #[test]
    fn consistent_sample_tables_are_not_damage() {
        let t = tables(&[10, 20, 30, 40, 50], 2);
        for faststart in [false, true] {
            assert!(damage_of(file_with(&t, faststart)).is_empty());
            let wide = Tables {
                co64: true,
                ..t.clone()
            };
            assert!(damage_of(file_with(&wide, faststart)).is_empty());
        }
        // Constant-size samples and compact sizes describe the same bytes.
        let constant = Tables {
            stsz: words(&[0, 25, 4]),
            ..tables(&[25; 4], 3)
        };
        assert!(damage_of(file_with(&constant, false)).is_empty());
        let stz2 = Tables {
            stsz: Vec::new(),
            ..tables(&[1, 2, 3], 1)
        };
        let mut f = file_with(&stz2, false);
        let compact = bx(b"stz2", &cat(&[words(&[0, 8, 3]), vec![1, 2, 3]]));
        let at = f.windows(4).position(|w| w == b"stsz").unwrap() - 4;
        f.splice(at..at + 8, compact.iter().copied());
        // Patch the sizes of the boxes around it: stbl, minf, mdia, trak, moov.
        let grow = compact.len() as u32 - 8;
        for t in [b"stbl", b"minf", b"mdia", b"trak", b"moov"] {
            let at = f.windows(4).position(|w| w == t).unwrap() - 4;
            let size = u32::from_be_bytes(f[at..at + 4].try_into().unwrap()) + grow;
            f[at..at + 4].copy_from_slice(&size.to_be_bytes());
        }
        assert!(damage_of(f).is_empty());
    }

    #[test]
    fn inconsistent_sample_tables_are_damage() {
        let good = tables(&[10, 20, 30, 40], 2);
        let cases: Vec<(Tables, &str)> = vec![
            (
                Tables {
                    stsz: words(&[0, 0, 5, 10, 20, 30, 40]),
                    ..good.clone()
                },
                "`stsz` claims 5 entries but has room for 4",
            ),
            (
                Tables {
                    stts: words(&[0, 1, 3, 1000]),
                    ..good.clone()
                },
                "`stts` times 3 samples but `stsz` sizes 4",
            ),
            (
                Tables {
                    stsc: words(&[0, 1, 1, 3, 1]),
                    ..good.clone()
                },
                "place 6 samples but `stsz` sizes 4",
            ),
            (
                Tables {
                    stsc: words(&[0, 1, 1, 2, 2]),
                    ..good.clone()
                },
                "uses sample description 2, but `stsd` has 1",
            ),
            (
                Tables {
                    stsc: words(&[0, 2, 1, 2, 1, 3, 2, 1]),
                    ..good.clone()
                },
                "starts at chunk 3, but there are only 2 chunks",
            ),
            (
                Tables {
                    chunks: vec![0, 1_000_000],
                    ..good.clone()
                },
                "chunk 2 (bytes 1000028..1000098) runs past the end of the file",
            ),
            (
                // The second chunk starts where `moov` does.
                Tables {
                    chunks: vec![0, 100],
                    ..good.clone()
                },
                "chunk 2 (bytes 128..198) lies outside the media data",
            ),
        ];
        for (t, want) in cases {
            let d = damage_of(file_with(&t, false));
            assert!(
                d.len() == 1 && d[0].contains(want),
                "want {want:?}, got {d:?}"
            );
            assert!(d[0].starts_with("track #0: "), "{d:?}");
        }
    }

    #[test]
    fn stsc_runs_out_of_order_are_damage() {
        // Each run used to be measured up to the next one's first chunk
        // before that was checked, which overflowed when it went backwards.
        let good = tables(&[10, 20, 30, 40, 50, 60], 1);
        let cases = [
            (
                words(&[0, 2, 1, 1, 1, 0, 1, 1]),
                "`stsc` entry 1 starts at chunk 0, but chunks count from 1",
            ),
            (
                words(&[0, 3, 1, 1, 1, 3, 1, 1, 2, 1, 1]),
                "`stsc` entry 2 starts at chunk 2, but the entry before it starts at chunk 3",
            ),
            (
                words(&[0, 2, 1, 1, 1, 1, 1, 1]),
                "`stsc` entry 1 starts at chunk 1, but the entry before it starts at chunk 1",
            ),
        ];
        for (stsc, want) in cases {
            let t = Tables {
                stsc,
                ..good.clone()
            };
            assert_eq!(
                damage_of(file_with(&t, false)),
                [format!("track #0: {want}")]
            );
        }
    }

    #[test]
    fn handler_and_sample_entry_of_other_kinds_are_damage() {
        let f = file_with(&tables(&[10], 1), false);
        let handler = |f: &[u8]| {
            f.windows(16)
                .position(|w| w.starts_with(b"hdlr") && w.ends_with(b"vide"))
                .unwrap()
                + 12
        };
        let with = |at: usize, t: &[u8; 4]| {
            let mut g = f.clone();
            g[at..at + 4].copy_from_slice(t);
            damage_of(g)
        };
        assert_eq!(
            with(handler(&f), b"soun"),
            ["track #0: its handler says audio (`soun`), but its sample entry `hvc1` is video"]
        );
        // Image sequences (`pict`) and alpha planes (`auxv`) use video
        // sample entries too.
        assert!(with(handler(&f), b"pict").is_empty());
        let entry = f.windows(4).position(|w| w == b"hvc1").unwrap();
        assert_eq!(
            with(entry, b"mp4a"),
            ["track #0: its handler says video (`vide`), but its sample entry `mp4a` is audio"]
        );
        // An entry type we do not know says nothing.
        assert!(with(entry, b"xxxx").is_empty());
    }

    #[test]
    fn missing_boxes_and_truncation_are_damage() {
        let f = file_with(&tables(&[10, 20], 1), true);
        let d = damage_of(f[..f.len() - 5].to_vec());
        assert!(
            d[0].contains("ends 33 bytes into the 38-byte `mdat` box"),
            "{d:?}"
        );
        assert!(d[1].contains("track #0: chunk 2 (bytes"), "{d:?}");
        assert!(d[1].contains("runs past the end of the file"), "{d:?}");

        let no_hdlr = file_with(&tables(&[10], 1), false);
        let at = no_hdlr.windows(4).position(|w| w == b"hdlr").unwrap();
        let mut f = no_hdlr.clone();
        f[at..at + 4].copy_from_slice(b"hdlx");
        assert_eq!(damage_of(f), ["track #0: it has no handler (`mdia/hdlr`)"]);

        let no_moov = cat(&[bx(b"ftyp", b"isom\0\0\x02\0isom"), bx(b"mdat", &[1; 8])]);
        assert_eq!(
            damage_of(no_moov),
            ["there is no `moov` box, so no index of the media"]
        );
    }

    #[test]
    fn timescales_ffmpeg_cannot_use_are_damage() {
        let f = file_with(&tables(&[10], 1), false);
        let with = |t: &[u8; 4], ts: u32| {
            // After the type: version and flags, then two times.
            let at = f.windows(4).position(|w| w == t).unwrap() + 16;
            let mut g = f.clone();
            g[at..at + 4].copy_from_slice(&ts.to_be_bytes());
            damage_of(g)
        };
        assert!(with(b"mdhd", 1).is_empty());
        assert!(with(b"mvhd", i32::MAX as u32).is_empty());
        assert_eq!(
            with(b"mdhd", 0),
            ["track #0: its `mdhd` gives timescale 0, so its times are wrong"]
        );
        // FFmpeg reads it as a signed number.
        assert_eq!(
            with(b"mdhd", 1 << 31),
            ["track #0: its `mdhd` gives timescale 2147483648, so its times are wrong"]
        );
        assert_eq!(
            with(b"mvhd", 0),
            ["`mvhd` gives timescale 0, so the movie's times are wrong"]
        );
    }

    /// `f` with `bytes` put in at offset `at`, and the size of the first
    /// box of each type in `around` grown to hold them.
    fn insert(f: &[u8], at: usize, bytes: &[u8], around: &[&[u8; 4]]) -> Vec<u8> {
        let mut g = f.to_vec();
        g.splice(at..at, bytes.iter().copied());
        for t in around {
            let at = g.windows(4).position(|w| w == *t).unwrap() - 4;
            let size = u32::from_be_bytes(g[at..at + 4].try_into().unwrap()) + bytes.len() as u32;
            g[at..at + 4].copy_from_slice(&size.to_be_bytes());
        }
        g
    }

    /// The boxes around the `hvc1` sample entry of [`minimal`], innermost
    /// first.
    const AROUND_ENTRY: &[&[u8; 4]] = &[
        b"hvc1", b"stsd", b"stbl", b"minf", b"mdia", b"trak", b"moov",
    ];

    /// A Spherical Video V2 `sv3d` box with an equirectangular projection.
    fn sv3d() -> Vec<u8> {
        let proj = bx(
            b"proj",
            &cat(&[full(b"prhd", 0, 0, &[0; 12]), full(b"equi", 0, 0, &[0; 16])]),
        );
        bx(b"sv3d", &cat(&[full(b"svhd", 0, 0, b"\0"), proj]))
    }

    /// [`minimal`] with an [`sv3d`] in its sample entry, before `hvcC`.
    fn spherical() -> Vec<u8> {
        let f = minimal();
        let entry = f.windows(4).position(|w| w == b"hvc1").unwrap() - 4;
        insert(&f, entry + 8 + 78, &sv3d(), AROUND_ENTRY)
    }

    #[test]
    fn box_types_that_are_not_text_are_damage_where_types_are_registered() {
        let f = file_with(&tables(&[10], 1), false);
        let with = |t: &[u8; 4], first: u8| {
            let at = f.windows(4).position(|w| w == t).unwrap();
            let mut g = f.clone();
            g[at] = first;
            (at - 4, damage_of(g))
        };
        // One byte of `trak` flipped: FFmpeg skips the track, and its
        // stream is gone.
        let (at, d) = with(b"trak", b't' ^ 0xa5);
        assert_eq!(
            d,
            [format!(
                "box `\\xd1rak` at offset {at} in `moov` has a type that is not text, so its header is damaged"
            )]
        );
        for (t, parent) in [
            (b"mvhd", "moov"),
            (b"mdia", "trak"),
            (b"mdhd", "mdia"),
            (b"minf", "mdia"),
            (b"stbl", "minf"),
            (b"stsz", "stbl"),
            (b"hvc1", "stsd"),
        ] {
            let (at, d) = with(t, 0);
            let want = format!(
                "box `\\x00{}` at offset {at} in `{parent}`",
                t[1..].escape_ascii()
            );
            assert!(d.iter().any(|d| d.starts_with(&want)), "{want}: {d:?}");
        }
        // A printable type we do not know says nothing.
        assert!(with(b"mvhd", b'x').1.is_empty());

        // Inside the sample entries we know, and in the Spherical Video V2
        // boxes, where a damaged type loses the projection.
        let g = spherical();
        assert!(damage_of(g.clone()).is_empty(), "{:?}", damage_of(g));
        for (t, parent) in [
            (b"hvcC", "hvc1"),
            (b"sv3d", "hvc1"),
            (b"proj", "sv3d"),
            (b"equi", "proj"),
        ] {
            let at = g.windows(4).position(|w| w == t).unwrap();
            let mut h = g.clone();
            h[at + 3] ^= 0xa5;
            let want = format!(
                "box `{}\\x{:02x}` at offset {} in `{parent}`",
                t[..3].escape_ascii(),
                t[3] ^ 0xa5,
                at - 4
            );
            let d = damage_of(h);
            assert!(d.iter().any(|d| d.starts_with(&want)), "{want}: {d:?}");
        }

        // QuickTime metadata in `udta` and iTunes metadata in `meta/ilst`
        // have types that start with 0xA9.
        let hdlr = full(b"hdlr", 0, 0, b"\0\0\0\0mdirappl\0\0\0\0\0\0\0\0\0");
        let ilst = bx(
            b"ilst",
            &bx(b"\xa9too", &bx(b"data", b"\0\0\0\x01\0\0\0\0x")),
        );
        let meta = full(b"meta", 0, 0, &cat(&[hdlr, ilst]));
        let udta = bx(b"udta", &cat(&[bx(b"\xa9nam", b"\0\x05\0\0title"), meta]));
        let f = cat(&[bx(b"ftyp", b"isom\0\0\x02\0isom"), bx(b"moov", &udta)]);
        let t = read(&mut io::Cursor::new(&f)).unwrap();
        let moov = t.moov().unwrap();
        assert!(
            moov.path(&[b"udta", b"meta", b"ilst", b"\xa9too"])
                .is_some()
        );
        assert!(t.damage().is_empty(), "{:?}", t.damage());
    }

    #[test]
    fn a_trak_under_another_printable_type_is_damage() {
        let f = file_with(&tables(&[10], 1), false);
        let at = f.windows(4).position(|w| w == b"trak").unwrap();
        let with = |t: &[u8; 4]| {
            let mut g = f.clone();
            g[at..at + 4].copy_from_slice(t);
            damage_of(g)
        };
        let renamed = |t: &str| {
            format!(
                "box `{t}` at offset {} in `moov` starts with a track header (`tkhd`), so it is a `trak` whose type is damaged and FFmpeg skips its stream",
                at - 4
            )
        };
        // Every one-bit flip of the type is found: those that stay
        // printable, such as `Trak`, here, and the rest as types that are
        // not text.
        let mut printable = 0;
        for bit in 0..32 {
            let mut t = *b"trak";
            t[bit / 8] ^= 1 << (bit % 8);
            let d = with(&t);
            if is_text(FourCc(t)) {
                printable += 1;
                assert_eq!(d, [renamed(&FourCc(t).to_string())]);
            } else {
                assert_eq!(d.len(), 1, "{d:?}");
                assert!(d[0].ends_with("has a type that is not text, so its header is damaged"));
            }
        }
        assert_eq!(printable, 28);
        assert_eq!(with(b"free"), [renamed("free")]);

        // A track too large to keep as an unknown leaf is found too.
        let big = file_with(&tables(&vec![1; MAX_KEPT_LEAF / 4], 1000), false);
        let at = big.windows(4).position(|w| w == b"trak").unwrap();
        let mut g = big.clone();
        g[at..at + 4].copy_from_slice(b"free");
        let d = damage_of(g);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].starts_with("box `free` at offset"), "{d:?}");

        // A renamed `mdia` loses the stream too, and the track says so.
        let at = f.windows(4).position(|w| w == b"mdia").unwrap();
        let mut g = f.clone();
        g[at..at + 4].copy_from_slice(b"mdiA");
        assert_eq!(damage_of(g), ["track #0: it has no `mdia` box"]);
    }

    #[test]
    fn an_sv3d_ffmpeg_ignores_is_damage() {
        let f = spherical();
        let with = |t: &[u8; 4], to: &[u8; 4]| {
            let at = f.windows(4).position(|w| w == t).unwrap();
            let mut g = f.clone();
            g[at..at + 4].copy_from_slice(to);
            damage_of(g)
        };
        let sv3d = f.windows(4).position(|w| w == b"sv3d").unwrap() - 4;
        let lost = |missing: &str| {
            [format!(
                "`sv3d` at offset {sv3d} in `hvc1` has no {missing}, so FFmpeg ignores its projection"
            )]
        };
        assert_eq!(with(b"proj", b"qroj"), lost("`proj`"));
        assert_eq!(with(b"svhd", b"wvhd"), lost("`svhd`"));
        assert_eq!(with(b"prhd", b"pshd"), lost("`proj/prhd`"));
        // A projection we do not know is not damage: FFmpeg says so in its
        // log, and the file may use a newer one.
        assert!(with(b"equi", b"mshp").is_empty());

        // FFmpeg skips an `sv3d` under another type, and the projection
        // with it, without a word. Every one-bit flip of the type is found:
        // those that stay printable, such as `rv3d`, by the `svhd` the box
        // starts with, and the rest as types that are not text.
        let renamed = |t: &str| {
            [format!(
                "box `{t}` at offset {sv3d} in `hvc1` starts with a spherical video header (`svhd`), so it is an `sv3d` whose type is damaged and FFmpeg ignores its projection"
            )]
        };
        assert_eq!(with(b"sv3d", b"rv3d"), renamed("rv3d"));
        let mut printable = 0;
        for bit in 0..32 {
            let mut t = *b"sv3d";
            t[bit / 8] ^= 1 << (bit % 8);
            let d = with(b"sv3d", &t);
            if is_text(FourCc(t)) {
                printable += 1;
                assert_eq!(d, renamed(&FourCc(t).to_string()));
            } else {
                assert_eq!(d.len(), 1, "{d:?}");
                assert!(d[0].ends_with("has a type that is not text, so its header is damaged"));
            }
        }
        assert_eq!(printable, 27);
    }

    #[test]
    fn zero_types_are_allowed_only_at_the_end_of_a_sample_entry() {
        let f = minimal();
        let entry = f.windows(4).position(|w| w == b"hvc1").unwrap() - 4;
        let size = u32::from_be_bytes(f[entry..entry + 4].try_into().unwrap());
        // An eight-byte terminator, and eight zero bytes, which read as a
        // box that runs to the end of its parent.
        for terminator in [words(&[8, 0]), vec![0; 8]] {
            let g = insert(&f, entry + size as usize, &terminator, AROUND_ENTRY);
            let t = read(&mut io::Cursor::new(&g)).unwrap();
            let hvc1 = t.tracks().next().unwrap();
            let hvc1 = hvc1.path(&[b"mdia", b"minf", b"stbl", b"stsd", b"hvc1"]);
            assert_eq!(hvc1.unwrap().children.last().unwrap().fourcc.0, [0; 4]);
            assert!(t.damage().is_empty(), "{:?}", t.damage());
        }
        let zeroed_at = |at: usize, parent: &str| {
            vec![format!(
                "box `\\x00\\x00\\x00\\x00` at offset {at} in `{parent}` has a type that is not text, so its header is damaged"
            )]
        };
        // With its size zeroed too, the box is also sized to the end.
        let zeroed = |range: &std::ops::Range<usize>, parent: &str| {
            let at = range.start - if range.len() == 4 { 4 } else { 0 };
            let mut d = zeroed_at(at, parent);
            if range.len() == 8 {
                d.insert(0, sized_to_end(at, "\\x00\\x00\\x00\\x00"));
            }
            d
        };
        // Before the entry's other children, it is damage.
        let at = entry + 8 + 78;
        let g = insert(&f, at, &words(&[8, 0]), AROUND_ENTRY);
        assert_eq!(damage_of(g), zeroed_at(at, "hvc1"));

        // A last box that holds anything but zero bytes is no terminator.
        // With an `sv3d` last, as FFmpeg writes it, zeroing its type, or
        // its size and type, loses the projection, and so does zeroing the
        // size and type of the `hvcC` before it, which then runs to the end
        // of the entry and holds the `sv3d`.
        let g = insert(&f, entry + size as usize, &sv3d(), AROUND_ENTRY);
        assert!(damage_of(g.clone()).is_empty(), "{:?}", damage_of(g));
        for t in [b"sv3d", b"hvcC"] {
            let at = g.windows(4).position(|w| w == t).unwrap() - 4;
            for range in [at + 4..at + 8, at..at + 8] {
                let mut h = g.clone();
                h[range.clone()].fill(0);
                assert_eq!(damage_of(h), zeroed(&range, "hvc1"));
            }
        }
        // Nor is one whose bytes are too many to keep and look at.
        let big = MAX_KEPT_LEAF + 1;
        let last = cat(&[words(&[8 + big as u32, 0]), vec![1; big]]);
        let g = insert(&f, entry + size as usize, &last, AROUND_ENTRY);
        assert_eq!(damage_of(g), zeroed_at(entry + size as usize, "hvc1"));

        // A track whose type, or size and type, are zeroed loses its
        // stream. With size 0 it runs to the end of `moov`, so it is the
        // last child there.
        let f = file_with(&tables(&[10], 1), false);
        let at = f.windows(4).position(|w| w == b"trak").unwrap() - 4;
        for range in [at + 4..at + 8, at..at + 8] {
            let mut g = f.clone();
            g[range.clone()].fill(0);
            assert_eq!(damage_of(g), zeroed(&range, "moov"));
        }
    }

    #[test]
    fn a_zeroed_edit_list_is_damage() {
        // FFmpeg skips an `elst` whose header was zeroed, and the track then
        // starts at the wrong time. `edts`, `dinf`, `mvex` and `tref` hold
        // only registered types, so the same goes for their children.
        let f = minimal();
        let elst = full(b"elst", 0, 0, &words(&[1, 1000, 2000, 0x10000]));
        let dref = full(
            b"dref",
            0,
            0,
            &cat(&[words(&[1]), full(b"url ", 0, 1, &[])]),
        );
        let trex = full(b"trex", 0, 0, &words(&[1, 1, 0, 0, 0]));
        let chap = bx(b"chap", &words(&[2]));
        let cases = [
            (b"trak", vec![b"trak", b"moov"], b"edts", elst),
            (
                b"minf",
                vec![b"minf", b"mdia", b"trak", b"moov"],
                b"dinf",
                dref,
            ),
            (b"moov", vec![b"moov"], b"mvex", trex),
            (b"trak", vec![b"trak", b"moov"], b"tref", chap),
        ];
        for (parent, around, container, child) in cases {
            let p = f.windows(4).position(|w| w == parent).unwrap() - 4;
            let g = insert(&f, p + 8, &bx(container, &child), &around);
            assert!(damage_of(g.clone()).is_empty(), "{:?}", damage_of(g));
            let at = p + 16;
            for range in [at + 4..at + 8, at..at + child.len()] {
                let mut h = g.clone();
                h[range].fill(0);
                assert_eq!(
                    damage_of(h),
                    [format!(
                        "box `\\x00\\x00\\x00\\x00` at offset {at} in `{}` has a type that is not text, so its header is damaged",
                        FourCc(*container)
                    )]
                );
            }
        }
    }

    /// The warning for a child box `t` at `at` whose size field is 0.
    fn sized_to_end(at: usize, t: &str) -> String {
        format!(
            "child box `{t}` at {at} has size 0, which only the last top-level box may have; anything after it in its parent is read as part of it"
        )
    }

    #[test]
    fn a_box_inside_moov_with_size_0_is_damage() {
        // Size 0 runs a box to the end of its parent, so FFmpeg reads the
        // boxes after it as its payload and drops them: an `hvcC` takes the
        // `sv3d` with it, an `stsd` the sample tables, a `trak` the tracks
        // after it.
        let f = minimal();
        let entry = f.windows(4).position(|w| w == b"hvc1").unwrap() - 4;
        let size = u32::from_be_bytes(f[entry..entry + 4].try_into().unwrap());
        let g = insert(&f, entry + size as usize, &sv3d(), AROUND_ENTRY);
        let f2 = file_with(&tables(&[10], 1), false);
        let trak = f2.windows(4).position(|w| w == b"trak").unwrap() - 4;
        let trak_size = u32::from_be_bytes(f2[trak..trak + 4].try_into().unwrap());
        let two = insert(
            &f2,
            trak + trak_size as usize,
            &f2[trak..trak + trak_size as usize],
            &[b"moov"],
        );
        for (file, t) in [(&g, "hvcC"), (&g, "svhd"), (&f2, "stsd"), (&two, "trak")] {
            let at = file.windows(4).position(|w| w == t.as_bytes()).unwrap() - 4;
            let mut h = file.clone();
            h[at..at + 4].fill(0);
            let d = damage_of(h);
            assert!(d.contains(&sized_to_end(at, t)), "{t}: {d:?}");
        }
        // The last top-level box may run to the end of the file.
        let h = cat(&[f, words(&[0]), b"free".to_vec(), vec![1; 10]]);
        assert!(damage_of(h.clone()).is_empty(), "{:?}", damage_of(h));
    }

    /// Structure-aware fuzzing: random box trees, encoded with 32-bit,
    /// 64-bit and to-the-end sizes, then cut and corrupted.
    mod fuzz {
        use std::time::{Duration, Instant};

        use proptest::collection::vec;
        use proptest::prelude::*;

        use super::super::*;
        use super::{Tables, bx, file_with, tables, tree_stats, words};

        #[derive(Debug, Clone, Copy)]
        enum Size {
            Short,
            Large,
            /// Size 0: to the end of the parent (or file). Only the last box
            /// of a list is encoded this way; others fall back to `Short`.
            /// Below the top level the parser warns about it (see
            /// [`sized_to_end`]).
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

        /// How many boxes below the top level `list` encodes with size 0:
        /// the last of each list of children whose size is `ToEnd`.
        fn sized_to_end(list: &[Gen]) -> usize {
            list.iter()
                .map(|g| {
                    let kids: &[Gen] = match &g.body {
                        Body::Container(_, c) | Body::Stsd(c) | Body::Entry(_, c) => c,
                        Body::Leaf(..) | Body::Uuid(..) => &[],
                    };
                    let last = matches!(
                        kids.last(),
                        Some(Gen {
                            size: Size::ToEnd,
                            ..
                        })
                    );
                    usize::from(last) + sized_to_end(kids)
                })
                .sum()
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
            let damage = r.as_ref().map(|t| t.damage());
            let elapsed = start.elapsed();
            prop_assert!(
                elapsed < Duration::from_secs(1),
                "parse and check took {elapsed:?}"
            );
            if let Ok(d) = &damage {
                prop_assert!(d.len() <= MAX_WARNINGS + 3 + f.len() / 8);
            }
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
                // The only warnings are for boxes below the top level with
                // size 0, one each, up to the limit and a count of the rest.
                let n = sized_to_end(&top);
                let listed = n.min(MAX_WARNINGS) + usize::from(n > MAX_WARNINGS);
                prop_assert_eq!(t.warnings.len(), listed, "{:?}", t.warnings);
                prop_assert!(
                    t.warnings.iter().take(MAX_WARNINGS).all(|w| w.contains("has size 0")),
                    "{:?}",
                    t.warnings
                );
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

        fn sample_tables() -> impl Strategy<Value = (Tables, bool)> {
            (
                vec(0u32..300, 1..40),
                1usize..6,
                any::<bool>(),
                any::<bool>(),
            )
                .prop_map(|(sizes, per_chunk, co64, faststart)| {
                    let t = tables(&sizes, per_chunk);
                    (Tables { co64, ..t }, faststart)
                })
        }

        /// A table field: usually small, so that entries interact, but
        /// sometimes at or near the limits of its 32 bits.
        fn field(small: u32) -> impl Strategy<Value = u32> {
            prop_oneof![
                8 => 0..small,
                1 => Just(u32::MAX),
                1 => any::<u32>(),
            ]
        }

        /// Consistent tables for runs of (chunks, samples per chunk), one
        /// `stsc` entry per run, with sample sizes taken in turn from
        /// `sizes`.
        fn tables_of_runs(runs: &[(u32, u32)], sizes: &[u32]) -> Tables {
            let (mut stsc, mut all, mut chunks) = (Vec::new(), Vec::new(), Vec::new());
            let (mut first, mut at) = (1, 0u64);
            for &(n, per_chunk) in runs {
                stsc.extend([first, per_chunk, 1]);
                for _ in 0..n {
                    chunks.push(at);
                    for _ in 0..per_chunk {
                        let s = sizes[all.len() % sizes.len()];
                        all.push(s);
                        at += s as u64;
                    }
                }
                first += n;
            }
            let n = all.len() as u32;
            Tables {
                stts: words(&[0, 1, n, 1000]),
                stsz: [words(&[0, 0, n]), words(&all)].concat(),
                stsc: [words(&[0, runs.len() as u32]), words(&stsc)].concat(),
                chunks,
                co64: false,
                data_len: at as usize,
            }
        }

        /// Sample tables as a few changed bytes inside `moov` leave them:
        /// consistent tables with some of their words, entry counts
        /// included, set to arbitrary values.
        fn damaged_tables() -> impl Strategy<Value = (Tables, bool)> {
            let runs = vec((1u32..4, 1u32..4), 1..5);
            let sizes = vec(0u32..300, 1..4);
            let edits = vec((0usize..4, any::<prop::sample::Index>(), field(8)), 1..4);
            (runs, sizes, edits, any::<bool>(), any::<bool>()).prop_map(
                |(runs, sizes, edits, co64, faststart)| {
                    let mut t = Tables {
                        co64,
                        ..tables_of_runs(&runs, &sizes)
                    };
                    for (table, at, value) in edits {
                        let payload = match table {
                            0 => &mut t.stts,
                            1 => &mut t.stsz,
                            2 => &mut t.stsc,
                            _ => {
                                let i = at.index(t.chunks.len());
                                t.chunks[i] = value as u64;
                                continue;
                            }
                        };
                        // Any word after the version and flags.
                        let i = 4 + 4 * at.index(payload.len() / 4 - 1);
                        payload[i..i + 4].copy_from_slice(&value.to_be_bytes());
                    }
                    (t, faststart)
                },
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// `cargo test` builds with overflow checks, so this also
            /// catches arithmetic on table values that would wrap.
            #[test]
            fn damaged_sample_tables_never_panic((t, faststart) in damaged_tables()) {
                parse_bounded(&file_with(&t, faststart))?;
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            #[test]
            fn consistent_tables_are_never_damage((t, faststart) in sample_tables()) {
                let f = file_with(&t, faststart);
                let d = parse_bounded(&f)?.map(|t| t.damage());
                prop_assert_eq!(d, Some(vec![]));
            }

            #[test]
            fn every_cut_is_an_error_or_damage(
                (t, faststart) in sample_tables(),
                cut in any::<prop::sample::Index>(),
            ) {
                let f = file_with(&t, faststart);
                let g = &f[..cut.index(f.len())];
                if let Some(t) = parse_bounded(g)? {
                    prop_assert!(!t.damage().is_empty(), "{} of {} bytes", g.len(), f.len());
                }
            }

            #[test]
            fn damaged_tables_never_panic(
                (t, faststart) in sample_tables(),
                flips in vec((any::<prop::sample::Index>(), any::<u8>()), 1..6),
            ) {
                let mut f = file_with(&t, faststart);
                for (i, x) in &flips {
                    let i = i.index(f.len());
                    f[i] ^= x;
                }
                parse_bounded(&f)?;
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
