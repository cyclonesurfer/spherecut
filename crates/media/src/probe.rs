//! `spherecut probe`: everything we can learn about a media file without
//! decoding it.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use serde::Serialize;
use spherecut_core::{Rational, Time};
use thiserror::Error;

use crate::detect::{self, CameraGuess};
use crate::ff;
use crate::ffi;
use crate::hwprobe;
use crate::mp4::{self, Cursor, FirstBox, FourCc, Mp4Box};
use crate::spherical::{self, TrackSpherical};
use crate::{MediaError, TooLarge, init};

#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    /// As text, with bytes that are not UTF-8 replaced by U+FFFD.
    #[serde(serialize_with = "lossy_path")]
    pub path: PathBuf,
    pub file_size: u64,
    pub container: Container,
    pub streams: Vec<StreamInfo>,
    /// Present for MP4/MOV files, which is every camera format we support.
    pub mp4: Option<Mp4Summary>,
    pub camera: Option<CameraGuess>,
    /// How much of the file's structure was checked for `damage`.
    pub structure: StructureCheck,
    /// Why the file looks truncated or corrupt, each [`codes::TRUNCATED`]
    /// or [`codes::DAMAGED`]. A healthy file has none; `spherecut probe`
    /// fails when there are any.
    pub damage: Vec<Problem>,
    /// What may keep a stream from playing, or looks odd, without the
    /// file being damaged.
    pub warnings: Vec<Problem>,
    /// The warnings and errors FFmpeg logged while reading the file. They
    /// often say what it could not read, but healthy files can have some
    /// too, so `spherecut probe` does not fail on them.
    pub ffmpeg_log: Vec<String>,
}

/// A path as text, so that any file name can be reported.
fn lossy_path<S: serde::Serializer>(p: &Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&p.to_string_lossy())
}

impl ProbeReport {
    /// Why `spherecut probe` fails this file although FFmpeg opened it:
    /// its damage ([`codes::TRUNCATED`] when it was cut short, else
    /// [`codes::DAMAGED`]), or that it has no video or audio stream
    /// ([`codes::NO_VIDEO_OR_AUDIO`]). None for a file that passes.
    pub fn failure(&self) -> Option<Problem> {
        if let (Some(first), Some(code)) = (self.damage.first(), damage_code(&self.damage)) {
            let more = match self.damage.len() - 1 {
                0 => String::new(),
                1 => " (and 1 more problem)".into(),
                n => format!(" (and {n} more problems)"),
            };
            return Some(Problem::new(
                code,
                format!("{} is damaged: {first}{more}", self.path.display()),
            ));
        }
        // An MP4 or Matroska file can hold only subtitles or data.
        if !self
            .streams
            .iter()
            .any(|s| matches!(s.kind, StreamKind::Video | StreamKind::Audio))
        {
            return Some(Problem::new(
                codes::NO_VIDEO_OR_AUDIO,
                format!(
                    "{} has no video or audio stream; FFmpeg reads it as {}",
                    self.path.display(),
                    self.container.format_long
                ),
            ));
        }
        None
    }
}

/// Something probe found wrong with a file, or worth a warning, with a
/// stable code from [`codes`], as the CLI's JSON gives it (D27).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
}

impl Problem {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Problem {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The codes of probe's [`Problem`]s, and those of [`ProbeError::code`]
/// that don't come from [`MediaError::code`]. Within a schema version of
/// the CLI's JSON a code keeps its name and meaning (D27);
/// `docs/cli-json.md` lists them all.
pub mod codes {
    /// The file ends before its structure does: a top-level MP4 box, or
    /// the Matroska Segment, runs past the end of the file.
    pub const TRUNCATED: &str = "truncated";
    /// Its structure is damaged in another way.
    pub const DAMAGED: &str = "damaged";
    /// FFmpeg can't read it as media, or it is empty.
    pub const NOT_MEDIA: &str = "not_media";
    /// It has no video or audio stream.
    pub const NO_VIDEO_OR_AUDIO: &str = "no_video_or_audio";
    /// A warning: a stream's pictures are larger than Spherecut decodes
    /// (`MAX_PIXELS`); `MediaError::PictureTooLarge` has the same code.
    pub const TOO_LARGE: &str = "too_large";
    /// A warning: this FFmpeg has no decoder for a stream's codec.
    pub const NO_DECODER: &str = "no_decoder";
    /// A warning: FFmpeg could not set up a stream, or its decoder lacks
    /// a feature the stream uses.
    pub const CANNOT_DECODE: &str = "cannot_decode";
    /// A warning: FFmpeg reads the file as MP4/MOV although its first box
    /// is of a type such files don't start with.
    pub const UNEXPECTED_FIRST_BOX: &str = "unexpected_first_box";
    /// A warning: a `trak` that FFmpeg gives no stream.
    pub const TRACK_WITHOUT_STREAM: &str = "track_without_stream";

    /// Every code above.
    pub const ALL: &[&str] = &[
        TRUNCATED,
        DAMAGED,
        NOT_MEDIA,
        NO_VIDEO_OR_AUDIO,
        TOO_LARGE,
        NO_DECODER,
        CANNOT_DECODE,
        UNEXPECTED_FIRST_BOX,
        TRACK_WITHOUT_STREAM,
    ];
}

/// The code for a list of damage: [`codes::TRUNCATED`] when any of it is
/// truncation, the likelier cause of the rest, else [`codes::DAMAGED`].
/// None for no damage.
fn damage_code(damage: &[Problem]) -> Option<&'static str> {
    if damage.iter().any(|d| d.code == codes::TRUNCATED) {
        Some(codes::TRUNCATED)
    } else if damage.is_empty() {
        None
    } else {
        Some(codes::DAMAGED)
    }
}

/// An MP4 tree's damage, with its truncation told apart from the rest.
fn mp4_damage(t: &mp4::Mp4Tree) -> impl Iterator<Item = Problem> + use<> {
    let cut = t.truncated.map(|c| c.reason());
    t.damage().into_iter().map(move |d| {
        let code = if cut.as_ref() == Some(&d) {
            codes::TRUNCATED
        } else {
            codes::DAMAGED
        };
        Problem::new(code, d)
    })
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
    pub duration: Option<Time>,
    pub start_time: Option<Time>,
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
    pub duration: Option<Time>,
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
fn pts_time(pts: i64, tb: Rational) -> Option<Time> {
    (pts != ff::ffi::AV_NOPTS_VALUE)
        .then(|| Time::from_pts(pts, tb).ok())
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

/// Why [`probe`] failed, with what FFmpeg logged meanwhile.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct ProbeError {
    pub error: MediaError,
    /// FFmpeg's warnings and errors, as in [`ProbeReport::ffmpeg_log`].
    /// For a file FFmpeg cannot open they often say why ("moov atom not
    /// found").
    pub ffmpeg_log: Vec<String>,
    code: &'static str,
}

impl ProbeError {
    /// The error's stable code (D27). That is [`MediaError::code`], except
    /// for a file FFmpeg could not read: it is [`codes::TRUNCATED`] or
    /// [`codes::DAMAGED`] when probe's own checks found it so,
    /// [`codes::DAMAGED`] when it has MP4 structure FFmpeg calls invalid,
    /// and [`codes::NOT_MEDIA`] when it is empty or FFmpeg doesn't know it.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

/// [`probe_file`]'s error, with its code.
struct Failed {
    error: MediaError,
    code: &'static str,
}

impl From<MediaError> for Failed {
    fn from(error: MediaError) -> Self {
        Failed {
            code: error.code(),
            error,
        }
    }
}

/// Probe a file. Never panics on bad input; fails with
/// [`MediaError::Open`] for files FFmpeg can't read at all, and lists in
/// [`ProbeReport::damage`] the structural damage of files it can.
pub fn probe(path: &Path) -> Result<ProbeReport, ProbeError> {
    init();
    // The CLI hides FFmpeg's log by default, and it is often the only
    // word on what FFmpeg could not read.
    let (report, log) = hwprobe::capture_ffmpeg_log(|| probe_file(path));
    let ffmpeg_log = tally(log);
    match report {
        Ok(r) => Ok(ProbeReport { ffmpeg_log, ..r }),
        Err(Failed { error, code }) => Err(ProbeError {
            error,
            ffmpeg_log,
            code,
        }),
    }
}

/// FFmpeg's log lines listed past this many are counted instead.
const MAX_LOG_LINES: usize = 32;

/// Each of FFmpeg's log lines once, in order, with how often it came when
/// more than once. A damaged stream can make it log the same line for
/// every packet it reads.
fn tally(lines: Vec<String>) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut dropped = 0;
    for line in lines {
        if let Some(&i) = seen.get(&line) {
            counts[i].1 += 1;
        } else if counts.len() < MAX_LOG_LINES {
            seen.insert(line.clone(), counts.len());
            counts.push((line, 1));
        } else {
            dropped += 1;
        }
    }
    let mut out: Vec<String> = counts
        .into_iter()
        .map(|(l, n)| if n > 1 { format!("{l} ({n} times)") } else { l })
        .collect();
    if dropped > 0 {
        out.push(format!("{dropped} more lines not listed"));
    }
    out
}

fn probe_file(path: &Path) -> Result<ProbeReport, Failed> {
    let meta = std::fs::metadata(path).map_err(|e| MediaError::Io {
        path: path.to_path_buf(),
        error: e,
    })?;
    if !meta.is_file() {
        return Err(MediaError::NotAFile(path.to_path_buf()).into());
    }
    if meta.len() == 0 {
        return Err(Failed {
            error: MediaError::Open {
                path: path.to_path_buf(),
                reason: "the file is empty".into(),
            },
            code: codes::NOT_MEDIA,
        });
    }
    let mut warnings = Vec::new();
    let mut damage = Vec::new();

    let (mut tree, not_mp4) = match mp4::read_file(path) {
        Ok(t) => (Some(t), false),
        Err(mp4::Mp4Error::NotMp4(_)) => (None, true),
        Err(e) => {
            damage.push(Problem::new(codes::DAMAGED, format!("MP4 structure: {e}")));
            (None, false)
        }
    };
    if let Some(t) = &tree {
        damage.extend(mp4_damage(t));
    }

    let ictx = ffi::open_input(path, None).map_err(|e| {
        // What probe found says more than FFmpeg's error.
        let code = match &e {
            ffi::OpenError::Ffmpeg(e) => damage_code(&damage).or(match e {
                ff::Error::InvalidData if tree.is_some() => Some(codes::DAMAGED),
                ff::Error::InvalidData => Some(codes::NOT_MEDIA),
                _ => None,
            }),
            _ => None,
        };
        let error =
            MediaError::opening(path, e, |e| describe_open_error(e, tree.is_some(), &damage));
        Failed {
            code: code.unwrap_or_else(|| error.code()),
            error,
        }
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
                warnings.push(Problem::new(
                    codes::UNEXPECTED_FIRST_BOX,
                    format!(
                        "FFmpeg reads this as MP4/MOV, but its first box{} is not of a type MP4/MOV files start with",
                        first.map(|f| format!(" `{f}`")).unwrap_or_default()
                    ),
                ));
                damage.extend(mp4_damage(&t));
                tree = Some(t);
            }
            Err(e) => damage.push(Problem::new(
                codes::DAMAGED,
                format!(
                    "FFmpeg reads this as MP4/MOV, but its structure cannot be checked: {}",
                    match e {
                        mp4::Mp4Error::NotMp4(why) => why,
                        e => e.to_string(),
                    }
                ),
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
        // open_input refuses other containers, so this is only reached if
        // its list of demuxers grows without a check here.
        StructureCheck::None {
            why: format!("spherecut has no structure check for {}", fmt.description()),
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
    // For each stream, its `trak`.
    let mut stream_trak = Vec::new();
    // For each stream, why FFmpeg cannot decode it, if it cannot.
    let mut stream_undecodable = Vec::new();
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
        stream_trak.push(trak_index);
        let trak = trak_index.and_then(|i| traks.get(i));
        let track_sph = trak.map(|t| spherical::read_track(t)).unwrap_or_default();
        let handler = trak
            .and_then(|t| detect::handler_name(t))
            .or_else(|| st_tags.get("handler_name").cloned());
        let info = StreamInfo {
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
        };
        let decoder = ffi::find_decoder(&par);
        stream_undecodable.push(undecodable(
            &info,
            decoder.map(|d| d.name().to_string()),
            || open_decoder(&par, decoder),
        ));
        streams.push(info);
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
            warnings.push(Problem::new(
                codes::TRACK_WITHOUT_STREAM,
                format!("track #{i} has no FFmpeg stream"),
            ));
            continue;
        };
        if st.kind != want {
            damage.push(Problem::new(codes::DAMAGED, format!(
                "track #{i} is {} (handler `{}`, sample entry `{}`) but FFmpeg reads stream #{} as {}, so its track description is damaged",
                kind_name(want),
                handler.map(|t| t.to_string()).unwrap_or_default(),
                sample_entry_type(trak).map_or_else(|| "none".into(), |t| t.to_string()),
                st.index,
                kind_name(st.kind),
            )));
        }
    }
    // A video or audio stream FFmpeg cannot decode: damage when the codec
    // setup (`avcC`, `hvcC`, `esds`, ...) or the sample entry must be
    // damaged, a warning when this FFmpeg may just lack the codec or a
    // feature of it.
    for ((st, trak), why) in streams.iter().zip(&stream_trak).zip(&stream_undecodable) {
        let Some(why) = why else {
            continue;
        };
        let entry = trak
            .and_then(|i| traks.get(i))
            .and_then(|t| sample_entry_type(t));
        let what = format!(
            "stream #{}{}",
            st.index,
            entry
                .map(|e| format!(" (sample entry `{e}`)"))
                .unwrap_or_default()
        );
        if why.is_damage(entry, st.frames.is_some()) {
            damage.push(Problem::new(codes::DAMAGED, why.describe(&what, true)));
        } else {
            warnings.push(Problem::new(why.warning_code(), why.describe(&what, false)));
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
        ffmpeg_log: Vec::new(),
    })
}

/// Check that a Matroska/WebM file is as long as its Segment says. Live
/// recordings leave the size unknown.
fn check_matroska(path: &Path, file_size: u64, damage: &mut Vec<Problem>) -> StructureCheck {
    let segment = File::open(path)
        .ok()
        .and_then(|f| matroska_segment(&mut BufReader::new(f)));
    match segment {
        Some((start, Some(size))) => {
            if start.saturating_add(size) > file_size {
                damage.push(Problem::new(
                    codes::TRUNCATED,
                    format!(
                        "the file holds {} of the {size} bytes its Matroska Segment says it has: it was cut short",
                        file_size.saturating_sub(start)
                    ),
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

/// The type of a track's first sample entry, e.g. `hvc1`.
fn sample_entry_type(trak: &Mp4Box) -> Option<FourCc> {
    let stsd = trak.path(&[b"mdia", b"minf", b"stbl", b"stsd"])?;
    Some(stsd.children.first()?.fourcc)
}

/// Why FFmpeg cannot decode a video or audio stream.
#[derive(Debug)]
enum Undecodable {
    /// Its pictures are larger than Spherecut's decoders make (D24).
    TooLarge(TooLarge),
    /// FFmpeg could not set it up, for the reason given.
    Unconfigured(&'static str),
    /// This FFmpeg has no decoder for its codec, which it names.
    NoDecoder(String),
    /// The decoder, which it names, would not open with the stream's codec
    /// parameters.
    Refused { decoder: String, error: ff::Error },
}

impl Undecodable {
    /// Whether the file must be damaged, rather than this FFmpeg lacking
    /// the codec or a feature of it. `entry` is the stream's sample entry
    /// type, and `has_samples` says whether its track has samples.
    fn is_damage(&self, entry: Option<FourCc>, has_samples: bool) -> bool {
        match self {
            // A limit of Spherecut's, not of the file.
            Undecodable::TooLarge(_) => false,
            // FFmpeg sets a stream up from its sample entry and samples.
            // Without samples it may have had too little to go on, and
            // with a sample entry it does not read, it lacks the codec.
            Undecodable::Unconfigured(_) => entry.is_some_and(mp4::is_known_codec) && has_samples,
            Undecodable::NoDecoder(_) => false,
            Undecodable::Refused { error, .. } => !lacks_feature(*error),
        }
    }

    /// The code of the warning about it, when it is not damage.
    fn warning_code(&self) -> &'static str {
        match self {
            Undecodable::TooLarge(_) => codes::TOO_LARGE,
            Undecodable::NoDecoder(_) => codes::NO_DECODER,
            Undecodable::Unconfigured(_) | Undecodable::Refused { .. } => codes::CANNOT_DECODE,
        }
    }

    /// What to say about stream `what`, as damage or as a warning.
    fn describe(&self, what: &str, damage: bool) -> String {
        match (self, damage) {
            (Undecodable::TooLarge(size), _) => format!("{what} is {size}"),
            (Undecodable::Unconfigured(why), true) => {
                format!("{what} {why}, so its codec setup is damaged")
            }
            (Undecodable::Unconfigured(why), false) => {
                format!("{what} {why} and cannot be decoded")
            }
            (Undecodable::NoDecoder(codec), _) => {
                format!("{what} cannot be decoded: this FFmpeg has no {codec} decoder")
            }
            (Undecodable::Refused { decoder, error }, true) => format!(
                "{what} cannot be opened by FFmpeg's {decoder} decoder ({error}), so its codec setup is damaged"
            ),
            (Undecodable::Refused { decoder, error }, false) => format!(
                "{what} uses a feature FFmpeg's {decoder} decoder lacks ({error}) and cannot be decoded"
            ),
        }
    }
}

/// Whether a decoder's error says it lacks a feature the stream uses,
/// rather than that the stream is wrong.
fn lacks_feature(e: ff::Error) -> bool {
    matches!(
        e,
        ff::Error::PatchWelcome
            | ff::Error::Experimental
            | ff::Error::Other {
                errno: ff::util::error::ENOSYS
            }
    )
}

/// Why FFmpeg cannot decode the stream `st`, if it is a video or audio
/// stream it cannot decode. First whether its pictures are over
/// [`MAX_PIXELS`](crate::MAX_PIXELS), which every decoder refuses, so the
/// rest would only say that FFmpeg could not set the stream up or that
/// the decoder refused it. Then as FFmpeg judges whether it set the
/// stream up: it needs the codec, the frame size or the sample rate and
/// channels, and the pixel or sample format when it has a decoder (the
/// one named `decoder`) to find it with. Then whether that decoder opens
/// with the stream's codec parameters (`open`), which checks the codec
/// setup that the parameters alone do not show: FFmpeg fills them in from
/// the sample entry even when its decoder refuses the `esds`.
fn undecodable(
    st: &StreamInfo,
    decoder: Option<String>,
    open: impl FnOnce() -> Result<(), ff::Error>,
) -> Option<Undecodable> {
    if !matches!(st.kind, StreamKind::Video | StreamKind::Audio) {
        return None;
    }
    let Some(codec) = &st.codec else {
        return Some(Undecodable::Unconfigured("has no codec FFmpeg knows"));
    };
    let unconfigured = |why| Some(Undecodable::Unconfigured(why));
    if let Some(v) = &st.video {
        if let Some(size) = TooLarge::of(v.width, v.height) {
            return Some(Undecodable::TooLarge(size));
        }
        if v.width == 0 || v.height == 0 {
            return unconfigured("has no frame size");
        }
        if v.pix_fmt.is_none() && decoder.is_some() {
            return unconfigured("has no pixel format");
        }
    }
    if let Some(a) = &st.audio {
        if a.sample_rate == 0 {
            return unconfigured("has no sample rate");
        }
        if a.channels == 0 {
            return unconfigured("has no channels");
        }
        if a.sample_fmt.is_none() && decoder.is_some() {
            return unconfigured("has no sample format");
        }
    }
    let Some(decoder) = decoder else {
        return Some(Undecodable::NoDecoder(codec.clone()));
    };
    open()
        .err()
        .map(|error| Undecodable::Refused { decoder, error })
}

/// Open `decoder` with a stream's codec parameters, as a player would,
/// on one thread, and close it again.
fn open_decoder(par: &ff::codec::Parameters, decoder: Option<ff::Codec>) -> Result<(), ff::Error> {
    let decoder = decoder.ok_or(ff::Error::DecoderNotFound)?;
    let mut ctx = ff::codec::context::Context::from_parameters(par.clone())?;
    ctx.set_threading(ff::codec::threading::Config::count(1));
    ffi::open_decoder(ctx, decoder).map(drop)
}

fn describe_open_error(e: ff::Error, looked_like_mp4: bool, damage: &[Problem]) -> String {
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
    fn ffmpeg_log_lines_are_counted_not_repeated() {
        // Whole lines, each with its context, as the capture gives them.
        let aac = "[aac] Audio object type 0 is not implemented. Update your FFmpeg version.";
        let mov = "[mov,mp4,m4a,3gp,3g2,mj2] Failed to open codec in avformat_find_stream_info";
        let lines = [aac, mov, aac, aac].map(String::from).to_vec();
        assert_eq!(tally(lines), [format!("{aac} (3 times)"), mov.into()]);
        let many = (0..40)
            .map(|i| format!("line {i}"))
            .chain(["line 0".into(), "line 39".into()])
            .collect();
        let t = tally(many);
        assert_eq!(t.len(), MAX_LOG_LINES + 1);
        assert_eq!(t[0], "line 0 (2 times)");
        assert_eq!(t[MAX_LOG_LINES], "9 more lines not listed");
    }

    /// A video or audio stream with everything FFmpeg needs to decode it.
    fn stream(kind: StreamKind) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind,
            codec: Some("h264".into()),
            codec_tag: None,
            profile: None,
            level: None,
            time_base: Rational::ONE,
            start_pts: None,
            duration: None,
            frames: Some(10),
            avg_frame_rate: None,
            r_frame_rate: None,
            bit_rate: None,
            disposition: Vec::new(),
            handler: None,
            language: None,
            video: (kind == StreamKind::Video).then(|| VideoInfo {
                width: 64,
                height: 32,
                sample_aspect: None,
                pix_fmt: Some("yuv420p".into()),
                bit_depth: None,
                chroma_subsampling: None,
                color: ColorInfo {
                    range: None,
                    matrix: None,
                    primaries: None,
                    transfer: None,
                    chroma_location: None,
                },
                field_order: None,
                rotation_deg: None,
            }),
            audio: (kind == StreamKind::Audio).then(|| AudioInfo {
                sample_rate: 48000,
                channels: 2,
                layout: None,
                sample_fmt: Some("fltp".into()),
            }),
            spherical: TrackSpherical::default(),
            tags: BTreeMap::new(),
        }
    }

    #[test]
    fn a_stream_ffmpeg_cannot_set_up_is_damage_only_with_samples_it_reads() {
        let not_opened = || -> Result<(), ff::Error> { panic!("the decoder was opened") };
        let h264 = || Some("h264".to_string());
        let mut v = stream(StreamKind::Video);
        assert!(undecodable(&v, h264(), || Ok(())).is_none());
        v.video.as_mut().unwrap().pix_fmt = None;
        let why = undecodable(&v, h264(), not_opened).unwrap();
        assert!(matches!(
            why,
            Undecodable::Unconfigured("has no pixel format")
        ));
        // Damage when its track has samples of a codec FFmpeg reads.
        let avc1 = Some(FourCc::new(b"avc1"));
        assert!(why.is_damage(avc1, true));
        // Without samples FFmpeg may have had too little to go on, and with
        // a sample entry it does not read, it lacks the codec.
        assert!(!why.is_damage(avc1, false));
        assert!(!why.is_damage(Some(FourCc::new(b"zzzz")), true));
        assert!(!why.is_damage(None, true));
        assert_eq!(
            why.describe("stream #0", false),
            "stream #0 has no pixel format and cannot be decoded"
        );
        assert_eq!(why.warning_code(), codes::CANNOT_DECODE);
        let mut a = stream(StreamKind::Audio);
        a.audio.as_mut().unwrap().sample_fmt = None;
        assert!(matches!(
            undecodable(&a, Some("aac".into()), not_opened),
            Some(Undecodable::Unconfigured("has no sample format"))
        ));
    }

    #[test]
    fn a_stream_over_max_pixels_is_a_warning_that_names_the_limit() {
        // Spherecut's limit, not damage, whatever else FFmpeg could not
        // find out about the stream once its decoder refused the size.
        let not_opened = || -> Result<(), ff::Error> { panic!("the decoder was opened") };
        let mut v = stream(StreamKind::Video);
        let video = v.video.as_mut().unwrap();
        (video.width, video.height) = (16400, 8200);
        video.pix_fmt = None;
        let why = undecodable(&v, Some("mjpeg".into()), not_opened).unwrap();
        assert!(
            matches!(
                why,
                Undecodable::TooLarge(TooLarge {
                    width: 16400,
                    height: 8200
                })
            ),
            "{why:?}"
        );
        assert!(!why.is_damage(Some(FourCc::new(b"jpeg")), true));
        assert_eq!(why.warning_code(), codes::TOO_LARGE);
        assert_eq!(
            why.describe("stream #0 (sample entry `jpeg`)", false),
            "stream #0 (sample entry `jpeg`) is 16400x8200, more than the 2^27 pixels (16384x8192) per picture that Spherecut decodes"
        );
        // Without a decoder too.
        assert!(matches!(
            undecodable(&v, None, not_opened),
            Some(Undecodable::TooLarge(_))
        ));
        // At the limit, the decoder is opened as for any stream.
        let video = v.video.as_mut().unwrap();
        (video.width, video.height) = (16384, 8192);
        video.pix_fmt = Some("yuv420p".into());
        assert!(undecodable(&v, Some("h264".into()), || Ok(())).is_none());
    }

    #[test]
    fn a_stream_without_a_decoder_in_this_build_is_a_warning() {
        // FFmpeg finds the pixel or sample format with the decoder, so
        // without one, the missing format is not the problem.
        let not_opened = || -> Result<(), ff::Error> { panic!("the decoder was opened") };
        let mut v = stream(StreamKind::Video);
        v.video.as_mut().unwrap().pix_fmt = None;
        let mut a = stream(StreamKind::Audio);
        a.audio.as_mut().unwrap().sample_fmt = None;
        for st in [v, a, stream(StreamKind::Video)] {
            let why = undecodable(&st, None, not_opened).unwrap();
            assert!(
                matches!(&why, Undecodable::NoDecoder(c) if c == "h264"),
                "{why:?}"
            );
            assert!(!why.is_damage(Some(FourCc::new(b"avc1")), true));
            assert_eq!(why.warning_code(), codes::NO_DECODER);
            assert_eq!(
                why.describe("stream #0", false),
                "stream #0 cannot be decoded: this FFmpeg has no h264 decoder"
            );
        }
    }

    #[test]
    fn a_decoder_that_refuses_the_codec_setup_is_damage_unless_it_lacks_a_feature() {
        // ffmpeg-next words its errors once FFmpeg is set up.
        crate::init();
        let a = stream(StreamKind::Audio);
        let refused = |e| undecodable(&a, Some("aac".into()), || Err(e)).unwrap();
        let why = refused(ff::Error::InvalidData);
        // Whatever the sample entry, and with no samples too.
        assert!(why.is_damage(None, false));
        assert_eq!(
            why.describe("stream #1", true),
            "stream #1 cannot be opened by FFmpeg's aac decoder (Invalid data found when processing input), so its codec setup is damaged"
        );
        for e in [
            ff::Error::PatchWelcome,
            ff::Error::Experimental,
            ff::Error::Other {
                errno: ff::util::error::ENOSYS,
            },
        ] {
            assert!(
                !refused(e).is_damage(Some(FourCc::new(b"mp4a")), true),
                "{e}"
            );
            assert_eq!(refused(e).warning_code(), codes::CANNOT_DECODE);
        }
        // Only video and audio are checked.
        let mut d = stream(StreamKind::Data);
        d.codec = None;
        assert!(undecodable(&d, None, || Err(ff::Error::InvalidData)).is_none());
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
