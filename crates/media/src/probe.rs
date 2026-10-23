//! `spherecut probe`: everything we can learn about a media file without
//! decoding it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use spherecut_core::Rational;

use crate::detect::{self, CameraGuess};
use crate::ff;
use crate::ffi;
use crate::mp4::{self, Mp4Tree};
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
    pub warnings: Vec<String>,
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
    pub warnings: Vec<String>,
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
    d.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
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
/// [`MediaError::Open`] for files FFmpeg can't read at all.
pub fn probe(path: &Path) -> Result<ProbeReport, MediaError> {
    init();
    let meta = std::fs::metadata(path).map_err(|e| MediaError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if !meta.is_file() {
        return Err(MediaError::NotAFile(path.to_path_buf()));
    }
    let mut warnings = Vec::new();

    let tree: Option<Mp4Tree> = match mp4::read_file(path) {
        Ok(t) => Some(t),
        Err(mp4::Mp4Error::NotMp4(_)) => None,
        Err(e) => {
            warnings.push(format!("MP4 structure: {e}"));
            None
        }
    };
    if let Some(t) = &tree {
        warnings.extend(t.warnings.iter().map(|w| format!("MP4 structure: {w}")));
    }

    let ictx = ff::format::input(path).map_err(|e| MediaError::Open {
        path: path.to_path_buf(),
        reason: describe_open_error(e, tree.is_some()),
    })?;

    let sph: Vec<TrackSpherical> = tree
        .as_ref()
        .map(spherical::read_tracks)
        .unwrap_or_default();
    let handlers: Vec<Option<String>> = tree
        .as_ref()
        .map(|t| t.tracks().map(detect::handler_name).collect())
        .unwrap_or_default();

    let fmt = ictx.format();
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
        // The MOV demuxer creates one stream per `trak`, in file order.
        let track_sph = sph.get(idx).cloned().unwrap_or_default();
        let handler = handlers
            .get(idx)
            .cloned()
            .flatten()
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

    let camera = detect::guess(path, tree.as_ref(), &streams, &container.tags);
    let mp4 = tree.map(|t| Mp4Summary {
        brand: t.ftyp_brand().map(|b| b.to_string()),
        top_level: t
            .top
            .iter()
            .map(|b| (b.fourcc.to_string(), b.size))
            .collect(),
        trailing_bytes: t.trailing_bytes,
        warnings: t.warnings,
    });

    Ok(ProbeReport {
        path: path.to_path_buf(),
        file_size: meta.len(),
        container,
        streams,
        mp4,
        camera,
        warnings,
    })
}

fn describe_open_error(e: ff::Error, looked_like_mp4: bool) -> String {
    let base = e.to_string();
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
