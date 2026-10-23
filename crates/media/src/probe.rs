//! `spherecut probe`: everything we can learn about a media file without
//! decoding it.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use serde::Serialize;
use spherecut_core::Rational;

use crate::detect::{self, CameraGuess};
use crate::ff;
use crate::ffi;
use crate::mp4::{self, Cursor, FirstBox, Mp4Box};
use crate::spherical::{self, TrackSpherical};
use crate::{MediaError, init};

#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub path: PathBuf,
    pub file_size: u64,
    pub container: Container,
    pub streams: Vec<StreamInfo>,
    /// Present for MP4/MOV files, which is every camera format we support.
    pub mp4: Option<Mp4Summary>,
    pub camera: Option<CameraGuess>,
    /// How much of the file's structure was checked for `damage`.
    pub structure: StructureCheck,
    /// Why the file looks truncated or corrupt. A healthy file has none;
    /// `spherecut probe` fails when there are any.
    pub damage: Vec<String>,
    pub warnings: Vec<String>,
}

/// How much of a file's structure probe checks for damage, beyond what
/// FFmpeg needs to open it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum StructureCheck {
    /// MP4/MOV: every top-level box, and each track's sample tables
    /// against the media data.
    Full,
    /// Matroska/WebM: only that the file is as long as its header says.
    /// Damage inside goes unnoticed.
    Length,
    /// Nothing, for the reason given: truncation and corruption go
    /// unnoticed.
    None { why: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Container {
    pub format: String,
    pub format_long: String,
    /// Seconds, exact.
    pub duration: Option<Rational>,
    pub start_time: Option<Rational>,
    pub bit_rate: Option<i64>,
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Video,
    Audio,
    Data,
    Subtitle,
    Attachment,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamInfo {
    pub index: usize,
    pub kind: StreamKind,
    pub codec: Option<String>,
    /// The container's four-character code, e.g. "hvc1".
    pub codec_tag: Option<String>,
    pub profile: Option<String>,
    pub level: Option<i32>,
    /// Seconds per tick.
    pub time_base: Rational,
    pub start_pts: Option<i64>,
    pub duration: Option<Rational>,
    pub frames: Option<i64>,
    pub avg_frame_rate: Option<Rational>,
    /// FFmpeg's guess at the "real" base frame rate.
    pub r_frame_rate: Option<Rational>,
    pub bit_rate: Option<i64>,
    pub disposition: Vec<String>,
    pub handler: Option<String>,
    pub language: Option<String>,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    #[serde(skip_serializing_if = "TrackSpherical::is_empty")]
    pub spherical: TrackSpherical,
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub sample_aspect: Option<Rational>,
    pub pix_fmt: Option<String>,
    pub bit_depth: Option<u32>,
    /// log2 of horizontal and vertical chroma subsampling, e.g. (1, 1) for
    /// 4:2:0.
    pub chroma_subsampling: Option<(u32, u32)>,
    pub color: ColorInfo,
    pub field_order: Option<String>,
    pub rotation_deg: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColorInfo {
    pub range: Option<String>,
    pub matrix: Option<String>,
    pub primaries: Option<String>,
    pub transfer: Option<String>,
    pub chroma_location: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u32,
    pub layout: Option<String>,
    pub sample_fmt: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mp4Summary {
    pub brand: Option<String>,
    pub top_level: Vec<(String, u64)>,
    /// Bytes after the last top-level box (vendor trailers).
    pub trailing_bytes: u64,
    /// The top-level box cut off by the end of the file, if any.
    pub truncated: Option<TruncatedBox>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TruncatedBox {
    /// None when the file ends before the box type.
    pub fourcc: Option<String>,
    pub offset: u64,
    pub header_len: u64,
    /// The size its header claims; None when the file ends before it.
    pub size: Option<u64>,
    /// Bytes of it present in the file, fewer than `header_len` when the
    /// header itself is cut short.
    pub present: u64,
}

fn rational(r: ff::Rational) -> Option<Rational> {
    if r.denominator() == 0 || (r.numerator() == 0 && r.denominator() == 1) {
        return None;
    }
    Rational::new(r.numerator() as i64, r.denominator() as i64).ok()
}

fn fourcc_str(tag: u32) -> Option<String> {
    if tag == 0 {
        return None;
    }
    let b = tag.to_le_bytes();
    Some(
        b.iter()
            .map(|&c| {
                if c.is_ascii_graphic() || c == b' ' {
                    (c as char).to_string()
                } else {
                    format!("\\x{c:02x}")
                }
            })
            .collect(),
    )
}

fn tags(d: ff::DictionaryRef<'_>) -> BTreeMap<String, String> {
    ffi::dict_entries(&d).into_iter().collect()
}

/// Time `pts * tb`, or `None` for FFmpeg's "no value" marker.
fn pts_time(pts: i64, tb: Rational) -> Option<Rational> {
    (pts != ff::ffi::AV_NOPTS_VALUE)
        .then(|| Rational::from_integer(pts).checked_mul(tb).ok())
        .flatten()
}

fn disposition_names(d: ff::format::stream::Disposition) -> Vec<String> {
    use ff::format::stream::Disposition as D;
    [
        (D::DEFAULT, "default"),
        (D::DUB, "dub"),
        (D::ORIGINAL, "original"),
        (D::COMMENT, "comment"),
        (D::ATTACHED_PIC, "attached_pic"),
    ]
    .into_iter()
    .filter(|(f, _)| d.contains(*f))
    .map(|(_, n)| n.to_string())
    .collect()
}

/// Probe a file. Never panics on bad input; returns
/// [`MediaError::Open`] for files FFmpeg can't read at all, and lists in
/// [`ProbeReport::damage`] the structural damage of files it can.
pub fn probe(path: &Path) -> Result<ProbeReport, MediaError> {
    init();
    let meta = std::fs::metadata(path).map_err(|e| MediaError::Io {
        path: path.to_path_buf(),
        error: e,
    })?;
    if !meta.is_file() {
        return Err(MediaError::NotAFile(path.to_path_buf()));
    }
    if meta.len() == 0 {
        return Err(MediaError::Open {
            path: path.to_path_buf(),
            reason: "the file is empty".into(),
        });
    }
    let mut warnings = Vec::new();
    let mut damage = Vec::new();

    let (mut tree, not_mp4) = match mp4::read_file(path) {
        Ok(t) => (Some(t), false),
        Err(mp4::Mp4Error::NotMp4(_)) => (None, true),
        Err(e) => {
            damage.push(format!("MP4 structure: {e}"));
            (None, false)
        }
    };
    if let Some(t) = &tree {
        damage.extend(t.damage());
    }

    let ictx = ff::format::input(path).map_err(|e| MediaError::Open {
        path: path.to_path_buf(),
        reason: describe_open_error(e, tree.is_some(), &damage),
    })?;
    let fmt = ictx.format();

    let mov_family = fmt.name().split(',').any(|n| n == "mov" || n == "mp4");
    if not_mp4 && mov_family {
        // FFmpeg found MP4/MOV boxes, so the first box is damaged or of a
        // type we do not know. Check the rest of the structure anyway.
        match mp4::read_file_with(path, FirstBox::Any) {
            Ok(t) => {
                let first = t.top.first().map(|b| b.fourcc);
                let first = first.or(t.truncated.and_then(|c| c.fourcc));
                warnings.push(format!(
                    "FFmpeg reads this as MP4/MOV, but its first box{} is not of a type MP4/MOV files start with",
                    first.map(|f| format!(" `{f}`")).unwrap_or_default()
                ));
                damage.extend(t.damage());
                tree = Some(t);
            }
            Err(e) => damage.push(format!(
                "FFmpeg reads this as MP4/MOV, but its structure cannot be checked: {}",
                match e {
                    mp4::Mp4Error::NotMp4(why) => why,
                    e => e.to_string(),
                }
            )),
        }
    }

    let structure = if tree.is_some() {
        StructureCheck::Full
    } else if mov_family {
        StructureCheck::None {
            why: "its MP4/MOV boxes could not be read".into(),
        }
    } else if fmt.name().split(',').any(|n| n == "matroska") {
        check_matroska(path, meta.len(), &mut damage)
    } else {
        StructureCheck::None {
            why: format!(
                "spherecut checks MP4/MOV and Matroska files, not {}",
                fmt.description()
            ),
        }
    };

    let traks: Vec<&Mp4Box> = tree
        .as_ref()
        .map(|t| t.tracks().collect())
        .unwrap_or_default();
    let trak_of = trak_matcher(&traks);

    let av_tb = Rational::new(1, ff::ffi::AV_TIME_BASE as i64).unwrap_or(Rational::ONE);
    let container = Container {
        format: fmt.name().to_string(),
        format_long: fmt.description().to_string(),
        duration: (ictx.duration() > 0)
            .then(|| pts_time(ictx.duration(), av_tb))
            .flatten(),
        start_time: pts_time(ffi::format_start_time(&ictx), av_tb),
        bit_rate: (ictx.bit_rate() > 0).then_some(ictx.bit_rate()),
        tags: tags(ictx.metadata()),
    };

    let mut streams = Vec::new();
    // For each `trak`, the first stream that comes from it.
    let mut trak_stream: Vec<Option<usize>> = vec![None; traks.len()];
    for st in ictx.streams() {
        let par = st.parameters();
        let f = ffi::codecpar_fields(&par);
        let kind = match ffi::media_type(&par) {
            ff::media::Type::Video => StreamKind::Video,
            ff::media::Type::Audio => StreamKind::Audio,
            ff::media::Type::Data => StreamKind::Data,
            ff::media::Type::Subtitle => StreamKind::Subtitle,
            ff::media::Type::Attachment => StreamKind::Attachment,
            _ => StreamKind::Unknown,
        };
        let tb = rational(st.time_base()).unwrap_or(Rational::ONE);
        let st_tags = tags(st.metadata());
        let idx = st.index();
        let trak_index = trak_of(idx, st.id());
        if let Some(i) = trak_index {
            trak_stream[i].get_or_insert(streams.len());
        }
        let trak = trak_index.and_then(|i| traks.get(i));
        let track_sph = trak.map(|t| spherical::read_track(t)).unwrap_or_default();
        let handler = trak
            .and_then(|t| detect::handler_name(t))
            .or_else(|| st_tags.get("handler_name").cloned());
        streams.push(StreamInfo {
            index: idx,
            kind,
            codec: f.codec_name.clone().filter(|n| n != "none"),
            codec_tag: fourcc_str(f.codec_tag),
            profile: f.profile.clone(),
            level: f.level,
            time_base: tb,
            start_pts: (st.start_time() != ff::ffi::AV_NOPTS_VALUE).then_some(st.start_time()),
            duration: (st.duration() > 0)
                .then(|| pts_time(st.duration(), tb))
                .flatten(),
            frames: (st.frames() > 0).then_some(st.frames()),
            avg_frame_rate: rational(st.avg_frame_rate()),
            r_frame_rate: rational(st.rate()),
            bit_rate: (par.bit_rate() > 0).then_some(par.bit_rate()),
            disposition: disposition_names(st.disposition()),
            handler,
            language: st_tags.get("language").cloned().filter(|l| l != "und"),
            video: (kind == StreamKind::Video).then(|| VideoInfo {
                width: f.width.max(0) as u32,
                height: f.height.max(0) as u32,
                sample_aspect: Rational::new(f.sample_aspect.0 as i64, f.sample_aspect.1 as i64)
                    .ok()
                    .filter(|r| r.is_positive()),
                pix_fmt: f.pix_fmt.clone(),
                bit_depth: f.bit_depth,
                chroma_subsampling: f.chroma_subsampling,
                color: ColorInfo {
                    range: f.color_range.clone(),
                    matrix: f.color_matrix.clone(),
                    primaries: f.color_primaries.clone(),
                    transfer: f.color_transfer.clone(),
                    chroma_location: f.chroma_location.clone(),
                },
                field_order: f.field_order.clone(),
                rotation_deg: f.rotation_deg,
            }),
            audio: (kind == StreamKind::Audio).then(|| AudioInfo {
                sample_rate: f.sample_rate.max(0) as u32,
                channels: f.channels.max(0) as u32,
                layout: f.channel_layout.clone(),
                sample_fmt: f.sample_fmt.clone(),
            }),
            spherical: track_sph,
            tags: st_tags,
        });
    }

    // A video or audio `trak` that FFmpeg reads as another kind of stream
    // (often "data") has a track description FFmpeg could not follow.
    for (i, trak) in traks.iter().enumerate() {
        let handler = mp4::handler_type(trak);
        let want = match handler.map(|t| t.0) {
            Some(t) if t == *b"vide" => StreamKind::Video,
            Some(t) if t == *b"soun" => StreamKind::Audio,
            _ => continue,
        };
        let Some(st) = trak_stream[i].and_then(|s| streams.get(s)) else {
            warnings.push(format!("track #{i} has no FFmpeg stream"));
            continue;
        };
        if st.kind != want {
            damage.push(format!(
                "track #{i} is {} (handler `{}`, sample entry `{}`) but FFmpeg reads stream #{} as {}, so its track description is damaged",
                kind_name(want),
                handler.map(|t| t.to_string()).unwrap_or_default(),
                sample_entry_type(trak).unwrap_or_else(|| "none".into()),
                st.index,
                kind_name(st.kind),
            ));
        }
    }
    let camera = detect::guess(path, tree.as_ref(), &streams, &container.tags);
    let mp4 = tree.as_ref().map(|t| Mp4Summary {
        brand: t.ftyp_brand().map(|b| b.to_string()),
        top_level: t
            .top
            .iter()
            .map(|b| (b.fourcc.to_string(), b.size))
            .collect(),
        trailing_bytes: t.trailing_bytes,
        truncated: t.truncated.map(|c| TruncatedBox {
            fourcc: c.fourcc.map(|f| f.to_string()),
            offset: c.offset,
            header_len: c.header_len,
            size: c.size,
            present: c.present,
        }),
        warnings: t.warnings.clone(),
    });

    Ok(ProbeReport {
        path: path.to_path_buf(),
        file_size: meta.len(),
        container,
        streams,
        mp4,
        camera,
        structure,
        damage,
        warnings,
    })
}

/// Check that a Matroska/WebM file is as long as its Segment says. Live
/// recordings leave the size unknown.
fn check_matroska(path: &Path, file_size: u64, damage: &mut Vec<String>) -> StructureCheck {
    let segment = File::open(path)
        .ok()
        .and_then(|f| matroska_segment(&mut BufReader::new(f)));
    match segment {
        Some((start, Some(size))) => {
            if start.saturating_add(size) > file_size {
                damage.push(format!(
                    "the file holds {} of the {size} bytes its Matroska Segment says it has: it was cut short",
                    file_size.saturating_sub(start)
                ));
            }
            StructureCheck::Length
        }
        Some((_, None)) => StructureCheck::None {
            why: "its Matroska Segment does not record its size, as in live recordings".into(),
        },
        None => StructureCheck::None {
            why: "its Matroska header could not be read".into(),
        },
    }
}

const EBML_HEADER: u64 = 0x1A45_DFA3;
const EBML_VOID: u64 = 0xEC;
const MATROSKA_SEGMENT: u64 = 0x1853_8067;

/// Where the data of a Matroska file's Segment starts, and its size if
/// known: the EBML header comes first, then the Segment, perhaps after a
/// few Void elements.
fn matroska_segment<R: Read + Seek>(r: &mut R) -> Option<(u64, Option<u64>)> {
    let skip = |r: &mut R, size: Option<u64>| r.seek_relative(i64::try_from(size?).ok()?).ok();
    if ebml_id(r)? != EBML_HEADER {
        return None;
    }
    let size = ebml_size(r)?;
    skip(r, size)?;
    for _ in 0..8 {
        let id = ebml_id(r)?;
        let size = ebml_size(r)?;
        match id {
            MATROSKA_SEGMENT => return Some((r.stream_position().ok()?, size)),
            EBML_VOID => skip(r, size)?,
            _ => return None,
        }
    }
    None
}

/// An EBML variable-length integer of at most `max_len` bytes, still
/// with its length marker (the first set bit), and its length.
fn ebml_vint<R: Read>(r: &mut R, max_len: u32) -> Option<(u64, u32)> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b).ok()?;
    let len = b[0].leading_zeros() + 1;
    if len > max_len {
        return None;
    }
    let mut v = u64::from(b[0]);
    for _ in 1..len {
        r.read_exact(&mut b).ok()?;
        v = v << 8 | u64::from(b[0]);
    }
    Some((v, len))
}

/// An element ID, which keeps its length marker.
fn ebml_id<R: Read>(r: &mut R) -> Option<u64> {
    ebml_vint(r, 4).map(|(id, _)| id)
}

/// An element size: Some(None) for the all-ones "unknown" size.
fn ebml_size<R: Read>(r: &mut R) -> Option<Option<u64>> {
    let (v, len) = ebml_vint(r, 8)?;
    let mask = (1u64 << (7 * len)) - 1;
    Some((v & mask != mask).then_some(v & mask))
}

fn kind_name(k: StreamKind) -> String {
    format!("{k:?}").to_lowercase()
}

/// The track ID from `tkhd`, which FFmpeg's MOV demuxer uses as the
/// stream ID.
fn track_id(trak: &Mp4Box) -> Option<u32> {
    let tkhd = trak.child(b"tkhd")?;
    let mut c = Cursor::new(&tkhd.payload);
    let (version, _) = c.version_flags()?;
    // Creation and modification times come first.
    c.take(if version == 1 { 16 } else { 8 })?;
    c.u32()
}

/// Match FFmpeg streams to `trak`s: map (stream index, stream ID) to the
/// position of the stream's `trak`. Cover art in `udta` gives FFmpeg
/// streams that have no `trak`, so match by track ID when every track has
/// a distinct one, and by position otherwise.
fn trak_matcher(traks: &[&Mp4Box]) -> impl Fn(usize, i32) -> Option<usize> + use<> {
    let by_id: HashMap<u32, usize> = traks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| Some((track_id(t)?, i)))
        .collect();
    let count = traks.len();
    let use_ids = by_id.len() == count;
    move |index, id| {
        if use_ids {
            by_id.get(&u32::try_from(id).ok()?).copied()
        } else {
            (index < count).then_some(index)
        }
    }
}

/// The type of a track's first sample entry, e.g. "hvc1".
fn sample_entry_type(trak: &Mp4Box) -> Option<String> {
    let stsd = trak.path(&[b"mdia", b"minf", b"stbl", b"stsd"])?;
    Some(stsd.children.first()?.fourcc.to_string())
}

fn describe_open_error(e: ff::Error, looked_like_mp4: bool, damage: &[String]) -> String {
    let base = e.to_string();
    if let Some(why) = damage.first() {
        return format!("{base}: {why}");
    }
    match e {
        ff::Error::InvalidData if looked_like_mp4 => {
            format!(
                "{base}: the file has MP4 structure but its media data is unreadable (truncated or corrupt?)"
            )
        }
        ff::Error::InvalidData => format!("{base}: not a recognised media file"),
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    /// An EBML element; `size` None writes the 8-byte "unknown" size.
    fn el(id: u32, payload: &[u8], size: Option<u64>) -> Vec<u8> {
        let id = id.to_be_bytes();
        let skip = id.iter().position(|&b| b != 0).unwrap_or(3);
        let mut v = id[skip..].to_vec();
        match size {
            // The 1-byte form where it fits.
            Some(n) if n < 127 => v.push(0x80 | n as u8),
            Some(n) => v.extend_from_slice(&(n | 1 << 56).to_be_bytes()),
            None => v.extend_from_slice(&[0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
        }
        v.extend_from_slice(payload);
        v
    }

    fn segment(f: Vec<u8>) -> Option<(u64, Option<u64>)> {
        matroska_segment(&mut io::Cursor::new(f))
    }

    #[test]
    fn matroska_segment_is_found_after_the_header() {
        let doc_type = el(0x4282, b"webm", Some(4));
        let header = el(0x1A45_DFA3, &doc_type, Some(doc_type.len() as u64));
        let at = header.len() as u64 + 4 + 8;
        let body = vec![0; 300];
        let seg = |size| el(0x1853_8067, &body, size);

        let f = [header.clone(), seg(Some(300))].concat();
        assert_eq!(segment(f), Some((at, Some(300))));
        // Live recordings leave the size unknown.
        let f = [header.clone(), seg(None)].concat();
        assert_eq!(segment(f), Some((at, None)));
        // Void elements may come first.
        let void = el(0xEC, &[0; 5], Some(5));
        let f = [header.clone(), void.clone(), seg(Some(300))].concat();
        assert_eq!(segment(f), Some((at + void.len() as u64, Some(300))));
        // Cut short, the size still says how long it was.
        let f = [header.clone(), seg(Some(300))].concat();
        assert_eq!(
            segment(f[..at as usize + 10].to_vec()),
            Some((at, Some(300)))
        );
    }

    #[test]
    fn non_matroska_input_has_no_segment() {
        let doc_type = el(0x4282, b"webm", Some(4));
        let header = el(0x1A45_DFA3, &doc_type, Some(doc_type.len() as u64));
        let cluster = el(0x1F43_B675, &[0; 8], Some(8));
        for f in [
            Vec::new(),
            b"RIFF\x24\0\0\0WAVEfmt ".to_vec(),
            header[..header.len() - 2].to_vec(),
            [header.clone(), cluster].concat(),
            // An ID longer than 4 bytes.
            [header, vec![0x08, 1, 2, 3, 4]].concat(),
        ] {
            assert_eq!(segment(f.clone()), None, "{f:?}");
        }
    }
}
