//! `spherecut probe`: everything we can learn about a media file without
//! decoding it.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use spherecut_core::{Rational, Time};
use thiserror::Error;

use crate::detect::{self, CameraGuess};
use crate::ff;
use crate::ffi;
use crate::hwprobe;
use crate::mp4::{self, FirstBox, FourCc, Mp4Box};
use crate::spherical::{self, TrackSpherical};
use crate::{MediaError, Source, TooLarge, init};

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
    /// MP4/MOV: every top-level box, and each track's sample tables in
    /// `moov` against the media data. A fragmented file's fragments
    /// (`moof`, with its `trun` tables) are not checked.
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
#[serde(into = "Mp4SummaryJson")]
pub struct Mp4Summary {
    pub brand: Option<String>,
    /// Every top-level box, as type and size, in file order. The JSON
    /// lists the first [`MAX_LISTED_TOP_LEVEL`].
    pub top_level: Vec<(String, u64)>,
    /// Bytes after the last top-level box (vendor trailers).
    pub trailing_bytes: u64,
    /// The top-level box cut off by the end of the file, if any.
    pub truncated: Option<TruncatedBox>,
    pub warnings: Vec<String>,
}

/// How many top-level boxes the JSON lists. A fragmented recording has a
/// `moof` and an `mdat` per fragment, thousands in an hour.
pub const MAX_LISTED_TOP_LEVEL: usize = 64;

/// How [`Mp4Summary`] is written in the CLI's JSON (D27): the first
/// [`MAX_LISTED_TOP_LEVEL`] top-level boxes, and how many more there are.
#[derive(Serialize)]
#[serde(rename = "Mp4Summary")]
struct Mp4SummaryJson {
    brand: Option<String>,
    top_level: Vec<(String, u64)>,
    top_level_omitted: usize,
    trailing_bytes: u64,
    truncated: Option<TruncatedBox>,
    warnings: Vec<String>,
}

impl From<Mp4Summary> for Mp4SummaryJson {
    fn from(mut m: Mp4Summary) -> Self {
        let top_level_omitted = m.top_level.len().saturating_sub(MAX_LISTED_TOP_LEVEL);
        m.top_level.truncate(MAX_LISTED_TOP_LEVEL);
        Mp4SummaryJson {
            brand: m.brand,
            top_level: m.top_level,
            top_level_omitted,
            trailing_bytes: m.trailing_bytes,
            truncated: m.truncated,
            warnings: m.warnings,
        }
    }
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

/// A fourcc as text, with bytes that aren't printable escaped.
pub(crate) fn fourcc_str(tag: u32) -> Option<String> {
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

impl From<MediaError> for ProbeError {
    /// `error` with its own code and no FFmpeg log, for a backend that
    /// doesn't probe with FFmpeg.
    fn from(error: MediaError) -> Self {
        ProbeError {
            code: error.code(),
            error,
            ffmpeg_log: Vec::new(),
        }
    }
}

impl ProbeError {
    /// The error's stable code (D27). That is [`MediaError::code`], except
    /// for a file FFmpeg could not open: it is [`codes::TRUNCATED`] or
    /// [`codes::DAMAGED`] when probe's own checks found it so (its MP4
    /// boxes, or a Matroska file's length), [`codes::DAMAGED`] when it has
    /// MP4 structure FFmpeg calls invalid, and [`codes::NOT_MEDIA`] when it
    /// is empty or FFmpeg doesn't know it. Any other file FFmpeg can't open
    /// keeps [`MediaError::Open`]'s `open_failed`.
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
    probe_source(Source::path(path))
}

/// [`probe`] a file opened by one path and named by another: the report
/// and every message carry [`Source::name`].
pub fn probe_source(src: Source<'_>) -> Result<ProbeReport, ProbeError> {
    probe_with(src, None)
}

/// [`probe_source`], stopping when `cancel` is set: FFmpeg's open checks
/// it between reads, so that a probe of a file on a mount that hangs ends
/// when its caller gives up.
pub fn probe_cancellable(
    src: Source<'_>,
    cancel: Arc<AtomicBool>,
) -> Result<ProbeReport, ProbeError> {
    probe_with(src, Some(cancel))
}

fn probe_with(src: Source<'_>, cancel: Option<Arc<AtomicBool>>) -> Result<ProbeReport, ProbeError> {
    spherecut_core::may_block("probing a media file");
    init();
    // The CLI hides FFmpeg's log by default, and it is often the only
    // word on what FFmpeg could not read.
    let (report, log) = hwprobe::capture_ffmpeg_log(|| probe_file(src, cancel));
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

fn probe_file(src: Source<'_>, cancel: Option<Arc<AtomicBool>>) -> Result<ProbeReport, Failed> {
    // `open` is read, and `path` named.
    let Source { open, name: path } = src;
    let meta = std::fs::metadata(open).map_err(|e| MediaError::Io {
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

    let (mut tree, not_mp4) = match unless_unreadable(path, mp4::read_file(open))? {
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

    let ictx = match ffi::open_input(open, cancel) {
        Ok(ictx) => ictx,
        Err(e) => {
            if not_mp4 && matches!(e, ffi::OpenError::Ffmpeg(_)) {
                // A Matroska file cut short before FFmpeg could read its
                // tracks: its header says how long it was.
                check_matroska(open, meta.len(), &mut damage);
            }
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
            return Err(Failed {
                code: code.unwrap_or_else(|| error.code()),
                error,
            });
        }
    };
    let fmt = ictx.format();

    let mov_family = fmt.name().split(',').any(|n| n == "mov" || n == "mp4");
    if not_mp4 && mov_family {
        // FFmpeg found MP4/MOV boxes, so the first box is damaged or of a
        // type we do not know. Check the rest of the structure anyway.
        match unless_unreadable(path, mp4::read_file_with(open, FirstBox::Any))? {
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
        check_matroska(open, meta.len(), &mut damage)
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
            decoder.map(|d| Decoder {
                name: d.name().to_string(),
                channel_conf: d
                    .capabilities()
                    .contains(ff::codec::Capabilities::CHANNEL_CONF),
            }),
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
    let camera = detect::guess(src, tree.as_ref(), &streams, &container.tags);
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

/// An MP4 read's result, unless it failed to read the file: that is the
/// file's own error (permission denied, a failing card), with the code its
/// I/O error gives, never damage.
fn unless_unreadable(
    path: &Path,
    read: Result<mp4::Mp4Tree, mp4::Mp4Error>,
) -> Result<Result<mp4::Mp4Tree, mp4::Mp4Error>, MediaError> {
    match read {
        Err(mp4::Mp4Error::Io(error)) => Err(MediaError::Io {
            path: path.to_path_buf(),
            error,
        }),
        read => Ok(read),
    }
}

/// Where the data of the Matroska file at `path`'s Segment starts, and
/// its size, when the file says (live recordings don't): what
/// [`check_matroska`] and the decoder (D13) check a file's length
/// against.
pub(crate) fn matroska_segment_of(path: &Path) -> Option<(u64, u64)> {
    let f = File::open(path).ok()?;
    match matroska_segment(&mut BufReader::new(f)) {
        Ok((start, Some(size))) => Some((start, size)),
        _ => None,
    }
}

/// Check that a Matroska/WebM file is as long as its Segment says, and
/// that it gets as far as the Segment at all. Live recordings leave the
/// size unknown.
fn check_matroska(path: &Path, file_size: u64, damage: &mut Vec<Problem>) -> StructureCheck {
    let segment = match File::open(path) {
        Ok(f) => matroska_segment(&mut BufReader::new(f)),
        Err(_) => Err(NoSegment::NotMatroska),
    };
    match segment {
        Ok((start, Some(size))) => {
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
        Ok((_, None)) => StructureCheck::None {
            why: "its Matroska Segment does not record its size, as in live recordings".into(),
        },
        Err(NoSegment::CutShort) => {
            damage.push(Problem::new(
                codes::TRUNCATED,
                format!(
                    "the file ends after {file_size} bytes, before its Matroska Segment starts: it was cut short"
                ),
            ));
            StructureCheck::Length
        }
        Err(NoSegment::NotMatroska) => StructureCheck::None {
            why: "its Matroska header could not be read".into(),
        },
    }
}

const EBML_HEADER: u64 = 0x1A45_DFA3;
const EBML_VOID: u64 = 0xEC;
const MATROSKA_SEGMENT: u64 = 0x1853_8067;

/// Why [`matroska_segment`] found no Segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoSegment {
    /// The file starts with an EBML header, but ends before the Segment's
    /// data starts: it was cut short.
    CutShort,
    /// It doesn't start as a Matroska file does, or could not be read.
    NotMatroska,
}

/// Where the data of a Matroska file's Segment starts, and its size if
/// known: the EBML header comes first, then the Segment, perhaps after a
/// few Void elements.
fn matroska_segment<R: Read + Seek>(r: &mut R) -> Result<(u64, Option<u64>), NoSegment> {
    let skip = |r: &mut R, size: Option<u64>| {
        let size = size.and_then(|s| i64::try_from(s).ok());
        // Past the end of the file is fine: the next read says so.
        r.seek_relative(size.ok_or(NoSegment::NotMatroska)?)
            .map_err(|_| NoSegment::NotMatroska)
    };
    // Only a file that holds the whole EBML header ID is Matroska cut
    // short; a shorter one is just short.
    if ebml_id(r).map_err(|_| NoSegment::NotMatroska)? != EBML_HEADER {
        return Err(NoSegment::NotMatroska);
    }
    let size = ebml_size(r)?;
    skip(r, size)?;
    for _ in 0..8 {
        let id = ebml_id(r)?;
        let size = ebml_size(r)?;
        match id {
            MATROSKA_SEGMENT => {
                let start = r.stream_position().map_err(|_| NoSegment::NotMatroska)?;
                return Ok((start, size));
            }
            EBML_VOID => skip(r, size)?,
            _ => return Err(NoSegment::NotMatroska),
        }
    }
    Err(NoSegment::NotMatroska)
}

/// An EBML variable-length integer of at most `max_len` bytes, still
/// with its length marker (the first set bit), and its length.
fn ebml_vint<R: Read>(r: &mut R, max_len: u32) -> Result<(u64, u32), NoSegment> {
    let first = ebml_byte(r)?;
    let len = first.leading_zeros() + 1;
    if len > max_len {
        return Err(NoSegment::NotMatroska);
    }
    let mut v = u64::from(first);
    for _ in 1..len {
        v = v << 8 | u64::from(ebml_byte(r)?);
    }
    Ok((v, len))
}

/// The next byte, where the end of the file is [`NoSegment::CutShort`].
fn ebml_byte<R: Read>(r: &mut R) -> Result<u8, NoSegment> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b).map_err(|e| match e.kind() {
        ErrorKind::UnexpectedEof => NoSegment::CutShort,
        _ => NoSegment::NotMatroska,
    })?;
    Ok(b[0])
}

/// An element ID, which keeps its length marker.
fn ebml_id<R: Read>(r: &mut R) -> Result<u64, NoSegment> {
    ebml_vint(r, 4).map(|(id, _)| id)
}

/// An element size: None for the all-ones "unknown" size.
fn ebml_size<R: Read>(r: &mut R) -> Result<Option<u64>, NoSegment> {
    let (v, len) = ebml_vint(r, 8)?;
    let mask = (1u64 << (7 * len)) - 1;
    Ok((v & mask != mask).then_some(v & mask))
}

fn kind_name(k: StreamKind) -> String {
    format!("{k:?}").to_lowercase()
}

/// The `SA3D` box of the `trak` that FFmpeg's stream `index` (stream ID
/// `id`) comes from, for the audio decoder (D17). `None` when the file
/// isn't MP4/MOV, its boxes can't be read, or the track has none.
pub(crate) fn spatial_audio(path: &Path, index: usize, id: i32) -> Option<spherical::SpatialAudio> {
    let tree = mp4::read_file(path).ok()?;
    let traks: Vec<&Mp4Box> = tree.tracks().collect();
    let trak = traks.get(trak_matcher(&traks)(index, id)?)?;
    spherical::read_track(trak).spatial_audio
}

/// Match FFmpeg streams to `trak`s: map (stream index, stream ID) to the
/// position of the stream's `trak`. Cover art in `udta` gives FFmpeg
/// streams that have no `trak`, so match by track ID when every track has
/// a distinct one, and by position otherwise.
fn trak_matcher(traks: &[&Mp4Box]) -> impl Fn(usize, i32) -> Option<usize> + use<> {
    let by_id: HashMap<u32, usize> = traks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| Some((mp4::track_id(t)?, i)))
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

/// The decoder FFmpeg has for a stream's codec.
#[derive(Debug, Clone)]
struct Decoder {
    /// Its name, e.g. `aac`.
    name: String,
    /// It sets the channels up from the codec setup itself
    /// (`AV_CODEC_CAP_CHANNEL_CONF`), so it opens on a stream without
    /// them, where any other decoder refuses one.
    channel_conf: bool,
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
/// channels, and the pixel or sample format when it has a decoder
/// (`decoder`) to find it with. Then whether that decoder opens
/// with the stream's codec parameters (`open`), which checks the codec
/// setup that the parameters alone do not show: FFmpeg fills them in from
/// the sample entry even when its decoder refuses the `esds`.
fn undecodable(
    st: &StreamInfo,
    decoder: Option<Decoder>,
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
            // A decoder that sets the channels up itself, such as AAC's,
            // and fails to open in avformat_find_stream_info leaves the
            // stream with none in FFmpeg 7.x, 8.0 to 8.1.2, 9.0 and 9.0.1
            // (6.1 keeps the sample entry's, and 8.1.3 and 9.0.2 put them
            // back): what it says is the reason then (D2). Any other
            // decoder, such as PCM's, refuses a stream without channels
            // (EINVAL), which says only that it has none.
            if let Some(decoder) = decoder.as_ref().filter(|d| d.channel_conf)
                && let Err(error) = open()
            {
                return Some(Undecodable::Refused {
                    decoder: decoder.name.clone(),
                    error,
                });
            }
            return unconfigured("has no channels");
        }
        if a.sample_fmt.is_none() && decoder.is_some() {
            return unconfigured("has no sample format");
        }
    }
    let Some(decoder) = decoder else {
        return Some(Undecodable::NoDecoder(codec.clone()));
    };
    open().err().map(|error| Undecodable::Refused {
        decoder: decoder.name,
        error,
    })
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

/// The report as text for people.
mod text {
    use std::fmt::Write as _;

    use spherecut_core::{Rational, Time};

    use super::{ProbeReport, StreamInfo, StructureCheck, TruncatedBox};
    use crate::spherical::{Projection, StereoMode};

    /// The lines of FFmpeg's log about a file, as the report shows them.
    pub fn ffmpeg_lines(log: &[String]) -> String {
        log.iter().map(|l| format!("  ffmpeg     {l}\n")).collect()
    }

    fn secs(t: Time) -> String {
        format!("{:.3} s", t.to_secs_f64())
    }

    fn rate(r: Rational) -> String {
        if r.den() == 1 {
            format!("{} fps", r.num())
        } else {
            format!("{} ({:.3}) fps", r, r.to_f64())
        }
    }

    fn opt<T: std::fmt::Display>(o: &Option<T>) -> String {
        o.as_ref().map_or_else(|| "-".into(), |v| v.to_string())
    }

    fn truncated(t: &TruncatedBox) -> String {
        let fourcc = t.fourcc.as_ref().map(|f| format!(" `{f}`"));
        match (fourcc, t.size) {
            (Some(f), Some(size)) if t.present >= t.header_len => {
                format!(", truncated{f} ({} of {size} bytes)", t.present)
            }
            (f, _) => format!(
                ", truncated{} box header ({} of {} bytes)",
                f.unwrap_or_default(),
                t.present,
                t.header_len
            ),
        }
    }

    /// The report as text for people: what `spherecut probe` prints and the
    /// GUI's Info pane shows.
    pub fn render_text(r: &ProbeReport) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{}", r.path.display());
        let _ = writeln!(
            s,
            "  container  {} ({}), {} bytes{}{}",
            r.container.format,
            r.container.format_long,
            r.file_size,
            r.container
                .duration
                .map(|d| format!(", {}", secs(d)))
                .unwrap_or_default(),
            r.container
                .bit_rate
                .map(|b| format!(", {:.1} Mb/s", b as f64 / 1e6))
                .unwrap_or_default()
        );
        if let Some(m) = &r.mp4 {
            let boxes: Vec<String> = m.top_level.iter().map(|(t, _)| t.clone()).collect();
            let _ = writeln!(
                s,
                "  mp4        brand {}, boxes [{}]{}{}",
                opt(&m.brand),
                boxes.join(" "),
                m.truncated.as_ref().map(truncated).unwrap_or_default(),
                if m.trailing_bytes > 0 {
                    format!(", {} trailing bytes", m.trailing_bytes)
                } else {
                    String::new()
                }
            );
        }
        match &r.structure {
            StructureCheck::Full => {}
            StructureCheck::Length => {
                let _ = writeln!(
                    s,
                    "  structure  only its length is checked; damage inside goes unnoticed"
                );
            }
            StructureCheck::None { why } => {
                let _ = writeln!(
                    s,
                    "  structure  not checked ({why}); truncation and corruption go unnoticed"
                );
            }
        }
        if let Some(c) = &r.camera {
            let _ = writeln!(
                s,
                "  camera     {}{}{}  layout: {:?}",
                opt(&c.vendor),
                c.model
                    .as_ref()
                    .map(|m| format!(" {m}"))
                    .unwrap_or_default(),
                c.firmware
                    .as_ref()
                    .map(|f| format!(" (firmware {f})"))
                    .unwrap_or_default(),
                c.layout
            );
            for e in &c.evidence {
                let _ = writeln!(s, "             - {e}");
            }
        }
        for st in &r.streams {
            stream(&mut s, st);
        }
        for (k, v) in &r.container.tags {
            let _ = writeln!(s, "  tag        {k} = {v}");
        }
        for w in &r.warnings {
            let _ = writeln!(s, "  warning    {w}");
        }
        s += &ffmpeg_lines(&r.ffmpeg_log);
        for d in &r.damage {
            let _ = writeln!(s, "  DAMAGE     {d}");
        }
        s
    }

    fn stream(s: &mut String, st: &StreamInfo) {
        let _ = write!(
            s,
            "  #{:<2} {:<7} {}",
            st.index,
            format!("{:?}", st.kind).to_lowercase(),
            opt(&st.codec)
        );
        if let Some(t) = &st.codec_tag {
            let _ = write!(s, " [{t}]");
        }
        if let Some(p) = &st.profile {
            let _ = write!(s, " {p}");
        }
        if let Some(v) = &st.video {
            let _ = write!(
                s,
                ", {}x{}, {}{}",
                v.width,
                v.height,
                opt(&v.pix_fmt),
                v.bit_depth
                    .map(|b| format!(" ({b}-bit)"))
                    .unwrap_or_default()
            );
        }
        if let Some(a) = &st.audio {
            let _ = write!(
                s,
                ", {} Hz, {} ch{}",
                a.sample_rate,
                a.channels,
                a.layout
                    .as_ref()
                    .map(|l| format!(" ({l})"))
                    .unwrap_or_default()
            );
        }
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "             timebase {}, {}{}{}",
            st.time_base,
            st.avg_frame_rate
                .map(rate)
                .unwrap_or_else(|| "no frame rate".into()),
            st.duration
                .map(|d| format!(", {}", secs(d)))
                .unwrap_or_default(),
            st.frames
                .map(|f| format!(", {f} frames"))
                .unwrap_or_default()
        );
        if let Some(v) = &st.video {
            let c = &v.color;
            let _ = writeln!(
                s,
                "             color range {}, matrix {}, primaries {}, transfer {}",
                opt(&c.range),
                opt(&c.matrix),
                opt(&c.primaries),
                opt(&c.transfer)
            );
            if let Some(rot) = v.rotation_deg.filter(|r| r.abs() > 1e-6) {
                let _ = writeln!(s, "             display rotation {rot}°");
            }
        }
        if let Some(h) = &st.handler {
            let _ = writeln!(s, "             handler \"{h}\"");
        }
        if !st.disposition.is_empty() {
            let _ = writeln!(s, "             disposition {}", st.disposition.join(", "));
        }
        if let Some(v2) = &st.spherical.v2 {
            let proj = match &v2.projection {
                Some(Projection::Equirect {
                    top,
                    bottom,
                    left,
                    right,
                }) => {
                    if [top, bottom, left, right].iter().all(|v| **v == 0) {
                        "equirect (full sphere)".to_string()
                    } else {
                        format!("equirect (cropped t{top} b{bottom} l{left} r{right})")
                    }
                }
                Some(Projection::Cubemap { layout, padding }) => {
                    format!("cubemap (layout {layout}, padding {padding})")
                }
                Some(Projection::Mesh) => "mesh".into(),
                Some(Projection::Other { fourcc }) => format!("unknown `{fourcc}`"),
                None => "no projection box".into(),
            };
            let stereo = match v2.stereo {
                Some(StereoMode::Mono) => "mono".to_string(),
                Some(StereoMode::TopBottom) => "stereo top-bottom".into(),
                Some(StereoMode::LeftRight) => "stereo left-right".into(),
                Some(StereoMode::StereoCustom) => "stereo (custom)".into(),
                Some(StereoMode::Unknown(v)) => format!("unknown stereo mode {v}"),
                None => "no st3d (mono)".into(),
            };
            let _ = writeln!(
                s,
                "             spherical v2: {proj}, {stereo}, pose y{} p{} r{}{}",
                v2.pose_yaw,
                v2.pose_pitch,
                v2.pose_roll,
                v2.metadata_source
                    .as_ref()
                    .map(|m| format!(", source \"{m}\""))
                    .unwrap_or_default()
            );
        }
        if let Some(v1) = &st.spherical.v1 {
            let _ = writeln!(
                s,
                "             spherical v1: spherical={} stitched={} projection {} software {}",
                v1.spherical,
                v1.stitched,
                opt(&v1.projection_type),
                opt(&v1.stitching_software)
            );
        }
        if let Some(sa) = &st.spherical.spatial_audio {
            let _ = writeln!(
                s,
                "             spatial audio: order {}, {}, channel map {:?}",
                sa.ambisonic_order,
                if sa.is_ambix() {
                    "ambiX (ACN/SN3D)"
                } else {
                    "non-ambiX ordering/normalization"
                },
                sa.channel_map
            );
        }
    }
}

pub use text::{ffmpeg_lines, render_text};

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

    fn segment(f: Vec<u8>) -> Result<(u64, Option<u64>), NoSegment> {
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
        assert_eq!(segment(f), Ok((at, Some(300))));
        // Live recordings leave the size unknown.
        let f = [header.clone(), seg(None)].concat();
        assert_eq!(segment(f), Ok((at, None)));
        // Void elements may come first.
        let void = el(0xEC, &[0; 5], Some(5));
        let f = [header.clone(), void.clone(), seg(Some(300))].concat();
        assert_eq!(segment(f), Ok((at + void.len() as u64, Some(300))));
        // Cut short, the size still says how long it was.
        let f = [header.clone(), seg(Some(300))].concat();
        assert_eq!(segment(f[..at as usize + 10].to_vec()), Ok((at, Some(300))));
        // Cut before the Segment's data starts: in the EBML header, right
        // after it, in a Void element, or in the Segment's ID or size.
        for cut in [
            4,
            5,
            header.len() - 2,
            header.len(),
            header.len() + 2,
            at as usize - 1,
        ] {
            assert_eq!(
                segment(f[..cut].to_vec()),
                Err(NoSegment::CutShort),
                "{cut}"
            );
        }
        let f = [header.clone(), void.clone(), seg(Some(300))].concat();
        assert_eq!(
            segment(f[..header.len() + 4].to_vec()),
            Err(NoSegment::CutShort)
        );
    }

    /// A file that fails every read with the OS error `errno`, as a card
    /// going bad does.
    struct Unreadable(i32);

    impl io::Read for Unreadable {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(self.0))
        }
    }

    impl io::Seek for Unreadable {
        fn seek(&mut self, _: io::SeekFrom) -> io::Result<u64> {
            Ok(4096)
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_is_an_io_error_not_damage() {
        // Tests run as root can read any file, so the error comes from a
        // reader here: EACCES, EIO, and ENOENT for a file removed meanwhile.
        let path = Path::new("x.mp4");
        for (errno, code) in [(13, "io_error"), (5, "io_error"), (2, "not_found")] {
            let read = mp4::read(&mut Unreadable(errno));
            assert!(matches!(read, Err(mp4::Mp4Error::Io(_))), "{read:?}");
            let e = unless_unreadable(path, read).unwrap_err();
            assert_eq!(e.code(), code, "{e}");
            assert!(matches!(e, MediaError::Io { .. }), "{e:?}");
        }
        // What the box reader makes of the bytes is left to probe.
        let not_mp4 = mp4::read(&mut io::Cursor::new(b"not an mp4".to_vec()));
        assert!(matches!(
            unless_unreadable(path, not_mp4),
            Ok(Err(mp4::Mp4Error::NotMp4(_)))
        ));
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

    /// The decoder named `name`, which sets the channels up itself when
    /// `channel_conf`, as AAC's does.
    fn decoder(name: &str, channel_conf: bool) -> Option<Decoder> {
        Some(Decoder {
            name: name.into(),
            channel_conf,
        })
    }

    #[test]
    fn a_stream_ffmpeg_cannot_set_up_is_damage_only_with_samples_it_reads() {
        let not_opened = || -> Result<(), ff::Error> { panic!("the decoder was opened") };
        let h264 = || decoder("h264", false);
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
            undecodable(&a, decoder("aac", true), not_opened),
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
        let why = undecodable(&v, decoder("mjpeg", false), not_opened).unwrap();
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
        assert!(undecodable(&v, decoder("h264", false), || Ok(())).is_none());
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
        let refused = |e| undecodable(&a, decoder("aac", true), || Err(e)).unwrap();
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
    fn an_audio_stream_with_no_channels_is_judged_by_a_decoder_that_sets_them_up() {
        // FFmpeg 7.x zeroes the channels of an audio stream whose decoder,
        // one that sets them up itself, refused its setup in
        // avformat_find_stream_info, so the decoder's refusal is the
        // reason, as with the channels 6.1 and 8.1.3 keep: a feature it
        // lacks is not damage.
        crate::init();
        let mut a = stream(StreamKind::Audio);
        a.audio.as_mut().unwrap().channels = 0;
        let aac = || decoder("aac", true);
        let enosys = ff::Error::Other {
            errno: ff::util::error::ENOSYS,
        };
        let why = undecodable(&a, aac(), || Err(enosys)).unwrap();
        assert!(
            matches!(why, Undecodable::Refused { error, .. } if error == enosys),
            "{why:?}"
        );
        assert!(!why.is_damage(Some(FourCc::new(b"mp4a")), true));
        let why = undecodable(&a, aac(), || Err(ff::Error::InvalidData)).unwrap();
        assert!(why.is_damage(Some(FourCc::new(b"mp4a")), true));
        assert_eq!(
            why.describe("stream #1", true),
            "stream #1 cannot be opened by FFmpeg's aac decoder (Invalid data found when processing input), so its codec setup is damaged"
        );
        // A decoder that opens leaves the stream without channels, and
        // without a decoder there is nothing to ask. Nor is there with one
        // that needs the channels given, such as PCM's: it refuses a
        // stream without them (EINVAL), which says only that, so whether
        // that is damage goes by the track's samples.
        let pcm = || decoder("pcm_s16le", false);
        for (decoder, opens) in [(aac(), true), (None, false), (pcm(), false)] {
            let why = undecodable(&a, decoder, || {
                assert!(opens, "the decoder was opened");
                Ok(())
            });
            assert!(
                matches!(why, Some(Undecodable::Unconfigured("has no channels"))),
                "{why:?}"
            );
        }
        let why = undecodable(&a, pcm(), || Err(ff::Error::InvalidData)).unwrap();
        let sowt = Some(FourCc::new(b"sowt"));
        assert!(why.is_damage(sowt, true));
        assert!(!why.is_damage(sowt, false));
        // As FFmpeg's decoders say of themselves.
        for (name, channel_conf) in [("aac", true), ("pcm_s16le", false)] {
            let codec = ff::decoder::find_by_name(name).unwrap();
            assert_eq!(
                codec
                    .capabilities()
                    .contains(ff::codec::Capabilities::CHANNEL_CONF),
                channel_conf,
                "{name}"
            );
        }
    }

    #[test]
    fn non_matroska_input_has_no_segment() {
        let doc_type = el(0x4282, b"webm", Some(4));
        let header = el(0x1A45_DFA3, &doc_type, Some(doc_type.len() as u64));
        let cluster = el(0x1F43_B675, &[0; 8], Some(8));
        for f in [
            Vec::new(),
            b"RIFF\x24\0\0\0WAVEfmt ".to_vec(),
            // Less than the EBML header's ID is too little to tell.
            header[..3].to_vec(),
            [header.clone(), cluster].concat(),
            // An ID longer than 4 bytes.
            [header, vec![0x08, 1, 2, 3, 4]].concat(),
        ] {
            assert_eq!(segment(f.clone()), Err(NoSegment::NotMatroska), "{f:?}");
        }
    }
}
