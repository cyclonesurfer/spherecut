//! A small synchronous MP4 writer: one video stream from planar YUV frames
//! and an optional audio stream from planar float samples.
//!
//! The file is written under a temporary name next to its destination and
//! renamed into place when it is finished, the way [`crate::inject`]
//! replaces files, so a failed or abandoned export never leaves a partial
//! file at the destination.
//!
//! The synthetic test-media generator uses it today; the export pipeline
//! (M2) builds on it with hardware encoders, a render queue and
//! cancellation.

use std::io;
use std::path::{Path, PathBuf};

use spherecut_core::fs::{TempFile, follow_links};
use spherecut_core::video::ColorCodes;
use spherecut_core::{FrameRate, Rational, may_block};
use thiserror::Error;

use crate::ffi;
use crate::inject::{self, EntryEdit, InjectError};
use crate::{ff, init};

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder `{0}` is not available in this FFmpeg build")]
    NoEncoder(String),
    #[error("{context}: {error}")]
    Ffmpeg { context: String, error: ff::Error },
    #[error("{0}")]
    Invalid(String),
    /// On the destination (with links followed), never the temporary file.
    #[error("writing {}: {error}", path.display())]
    Io { path: PathBuf, error: io::Error },
    #[error(transparent)]
    Inject(#[from] InjectError),
    /// Opening a file to read from it.
    #[error(transparent)]
    Media(#[from] crate::MediaError),
}

fn ctx<T>(r: Result<T, ff::Error>, context: impl FnOnce() -> String) -> Result<T, EncodeError> {
    r.map_err(|error| EncodeError::Ffmpeg {
        context: context(),
        error,
    })
}

/// Pixel layouts the writer accepts: planar YUV, 8-bit or 10-bit in the
/// low bits of little-endian u16 samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayout {
    /// 8-bit 4:2:0.
    Yuv420p,
    /// 10-bit 4:2:0.
    Yuv420p10,
    /// 8-bit 4:2:2.
    Yuv422p,
    /// 10-bit 4:2:2.
    Yuv422p10,
    /// 8-bit 4:4:4.
    Yuv444p,
    /// 10-bit 4:4:4.
    Yuv444p10,
}

impl PixelLayout {
    fn ff(self) -> ff::format::Pixel {
        use ff::format::Pixel;
        match self {
            PixelLayout::Yuv420p => Pixel::YUV420P,
            PixelLayout::Yuv420p10 => Pixel::YUV420P10LE,
            PixelLayout::Yuv422p => Pixel::YUV422P,
            PixelLayout::Yuv422p10 => Pixel::YUV422P10LE,
            PixelLayout::Yuv444p => Pixel::YUV444P,
            PixelLayout::Yuv444p10 => Pixel::YUV444P10LE,
        }
    }

    /// Whether its samples are 10-bit, in u16s.
    pub fn ten_bit(self) -> bool {
        matches!(
            self,
            PixelLayout::Yuv420p10 | PixelLayout::Yuv422p10 | PixelLayout::Yuv444p10
        )
    }

    /// log2 of the chroma planes' horizontal and vertical subsampling.
    pub fn chroma_shift(self) -> (u32, u32) {
        match self {
            PixelLayout::Yuv420p | PixelLayout::Yuv420p10 => (1, 1),
            PixelLayout::Yuv422p | PixelLayout::Yuv422p10 => (1, 0),
            PixelLayout::Yuv444p | PixelLayout::Yuv444p10 => (0, 0),
        }
    }

    /// The size of each plane of a `width` × `height` frame.
    pub fn plane_sizes(self, width: u32, height: u32) -> [(usize, usize); 3] {
        let (sx, sy) = self.chroma_shift();
        let (w, h) = (width as usize, height as usize);
        let c = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
        [(w, h), c, c]
    }
}

/// The colour a stream is tagged with, as FFmpeg's ints:
/// [`ColorCodes`] says what they mean. libx264 and libx265 write the
/// first four to the stream's VUI, and `chroma_location` only for 4:2:0
/// (checked with FFmpeg 6.1.1's `trace_headers`, DECISIONS D30). For
/// 4:2:2 and 4:4:4, FFmpeg's H.264 decoder reports left, its default,
/// and its HEVC decoder unspecified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorTags {
    pub range: i32,
    pub matrix: i32,
    pub transfer: i32,
    pub primaries: i32,
    pub chroma_location: i32,
}

impl ColorTags {
    /// No tags at all: the file leaves every field unspecified.
    pub const NONE: ColorTags = ColorTags {
        range: 0,
        matrix: 2,
        transfer: 2,
        primaries: 2,
        chroma_location: 0,
    };

    /// BT.709 matrix, primaries and transfer, limited range, and no
    /// chroma location.
    pub const BT709_LIMITED: ColorTags = ColorTags {
        range: 1,
        matrix: 1,
        transfer: 1,
        primaries: 1,
        chroma_location: 0,
    };

    /// The same tags with chroma location `loc` (FFmpeg's
    /// `AVChromaLocation`: 1 left, 2 center, 3 top left).
    pub const fn with_chroma_location(self, loc: i32) -> ColorTags {
        ColorTags {
            chroma_location: loc,
            ..self
        }
    }
}

impl From<ColorTags> for ColorCodes {
    fn from(t: ColorTags) -> Self {
        ColorCodes {
            primaries: t.primaries,
            transfer: t.transfer,
            matrix: t.matrix,
            range: t.range,
            chroma_location: t.chroma_location,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VideoSettings {
    /// FFmpeg encoder name, e.g. "libx264", "libx265".
    pub encoder: String,
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    pub layout: PixelLayout,
    /// Encoder private options, e.g. `[("crf", "18"), ("preset", "fast")]`.
    pub options: Vec<(String, String)>,
    /// What the stream says its colour is.
    pub color: ColorTags,
    /// Frames at the start that the file's edit list (`elst`) hides: they
    /// are encoded and stored, and a player decodes them as references
    /// but doesn't show them, as in a file trimmed without re-encoding.
    /// They get negative timestamps, from which FFmpeg's MP4 muxer writes
    /// the edit. Each frame given to [`Mp4Writer::push_video`] counts;
    /// [`Mp4Writer::push_video_at`] sets its own timestamps.
    pub edit_start: u32,
    /// The stream's time base, in seconds per tick. `None` is one tick
    /// per frame, 1 / `rate`. With another, `push_video` needs a frame to
    /// be a whole number of ticks.
    pub time_base: Option<Rational>,
    /// Luma columns the stream crops on the left, an even number below
    /// `width`, or 0. Frames are pushed `width` wide and decode
    /// `crop_left` narrower: FFmpeg's h264_metadata or hevc_metadata
    /// filter writes the crop into the encoder's SPS (H.264's frame
    /// cropping, HEVC's conformance window), as no encoder does itself.
    /// For test media.
    pub crop_left: u32,
}

#[derive(Debug, Clone)]
pub struct AudioSettings {
    pub encoder: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bit_rate: usize,
    /// Encoder options, e.g. `[("strict", "experimental")]` for FFmpeg's
    /// own Opus encoder.
    pub options: Vec<(String, String)>,
}

/// One planar YUV frame. Plane strides equal plane widths.
#[derive(Debug, Clone)]
pub enum YuvPlanes {
    U8([Vec<u8>; 3]),
    U16([Vec<u16>; 3]),
}

/// One planar YUV frame in one buffer, each plane's rows `stride` bytes
/// apart from `offset`: the layout render's YUV pass reads back (I420,
/// or I010 with each sample a little-endian `u16` in its low 10 bits).
#[derive(Debug, Clone, Copy)]
pub struct StridedPlanes<'a> {
    pub data: &'a [u8],
    /// Each plane's first byte and the bytes from one row to the next.
    pub planes: [(usize, usize); 3],
}

/// What a frame is pushed as.
#[derive(Debug, Clone, Copy)]
enum Pushed<'a> {
    Planes(&'a YuvPlanes),
    Strided(&'a StridedPlanes<'a>),
}

/// A frame of `layout` holding `pushed`, `width` × `height`.
fn video_frame(
    layout: PixelLayout,
    width: u32,
    height: u32,
    pushed: Pushed<'_>,
) -> Result<ff::frame::Video, EncodeError> {
    let mut frame = ff::frame::Video::new(layout.ff(), width, height);
    let dims = layout.plane_sizes(width, height);
    let ten = layout.ten_bit();
    let planes = match pushed {
        Pushed::Planes(p) => p,
        Pushed::Strided(s) => {
            copy_strided(&mut frame, dims, if ten { 2 } else { 1 }, s)?;
            return Ok(frame);
        }
    };
    for (i, &(w, h)) in dims.iter().enumerate() {
        let stride = frame.stride(i);
        let data = frame.data_mut(i);
        match planes {
            YuvPlanes::U8(p) if !ten => {
                if p[i].len() != w * h {
                    return Err(EncodeError::Invalid(format!("plane {i} has wrong size")));
                }
                for y in 0..h {
                    data[y * stride..y * stride + w].copy_from_slice(&p[i][y * w..(y + 1) * w]);
                }
            }
            YuvPlanes::U16(p) if ten => {
                if p[i].len() != w * h {
                    return Err(EncodeError::Invalid(format!("plane {i} has wrong size")));
                }
                for y in 0..h {
                    let row = &mut data[y * stride..y * stride + 2 * w];
                    for (x, px) in p[i][y * w..(y + 1) * w].iter().enumerate() {
                        row[2 * x..2 * x + 2].copy_from_slice(&px.to_le_bytes());
                    }
                }
            }
            _ => {
                return Err(EncodeError::Invalid(
                    "frame bit depth does not match the stream".into(),
                ));
            }
        }
    }
    Ok(frame)
}

/// `s`'s planes into `frame`, a row at a time: `dims` samples of
/// `sample` bytes each.
fn copy_strided(
    frame: &mut ff::frame::Video,
    dims: [(usize, usize); 3],
    sample: usize,
    s: &StridedPlanes<'_>,
) -> Result<(), EncodeError> {
    for (i, &(w, h)) in dims.iter().enumerate() {
        let (offset, from_stride) = s.planes[i];
        let row = w * sample;
        let last = h
            .checked_sub(1)
            .and_then(|r| r.checked_mul(from_stride))
            .and_then(|b| b.checked_add(offset)?.checked_add(row));
        if from_stride < row || last.is_none_or(|end| end > s.data.len()) {
            return Err(EncodeError::Invalid(format!(
                "plane {i}: {h} rows of {row} bytes, {from_stride} apart from byte {offset}, \
                 don't fit {} bytes",
                s.data.len()
            )));
        }
        let to_stride = frame.stride(i);
        let to = frame.data_mut(i);
        for y in 0..h {
            let from = offset + y * from_stride;
            to[y * to_stride..y * to_stride + row].copy_from_slice(&s.data[from..from + row]);
        }
    }
    Ok(())
}

/// What swscale makes of `planes`, a `width` × `height` picture in
/// `layout`, as RGB at 16 bits per channel (R, G, B per pixel, rows top
/// to bottom), decoding its Y'CbCr with `matrix` and `range` as given,
/// whatever a file would tag: swscale takes an untagged picture as
/// BT.601. It is the independent conversion that `spherecut frame`'s
/// swscale cross-check holds the shader to (M1 plan section 6), so it
/// offers only BT.601 and BT.709.
pub fn swscale_rgb48(
    planes: &YuvPlanes,
    layout: PixelLayout,
    (width, height): (u32, u32),
    matrix: spherecut_core::video::Matrix,
    range: spherecut_core::video::Range,
) -> Result<Vec<u16>, EncodeError> {
    use spherecut_core::video::{Matrix, Range};
    init();
    let bt709 = match matrix {
        Matrix::Bt709 => true,
        Matrix::Bt601 => false,
        other => {
            return Err(EncodeError::Invalid(format!(
                "swscale_rgb48 takes BT.601 or BT.709, not {other:?}"
            )));
        }
    };
    let frame = video_frame(layout, width, height, Pushed::Planes(planes))?;
    ffi::swscale_rgb48(&frame, bt709, range == Range::Full)
        .map_err(|e| EncodeError::Invalid(format!("swscale: {e}")))
}

/// Whether this FFmpeg has the encoder FFmpeg calls `name`, such as
/// `libx265`.
pub fn has_encoder(name: &str) -> bool {
    init();
    ff::encoder::find_by_name(name).is_some()
}

/// The quantizer scale [`encode_jpeg`] takes: 2 is FFmpeg's best, 31
/// its worst.
pub const JPEG_QSCALE: std::ops::RangeInclusive<u8> = 2..=31;

/// `rgb`, `width` × `height` pixels of 8-bit sRGB, rows packed and top
/// first, as a baseline JPEG from FFmpeg's `mjpeg` encoder at quantizer
/// scale `qscale` ([`JPEG_QSCALE`]): 4:2:0, full range, BT.601, as JFIF
/// has it. Every FFmpeg build has the encoder: it is FFmpeg's own and
/// unpatented, in Fedora's `ffmpeg-free` too (a1-plan section 7.3).
pub fn encode_jpeg(
    rgb: &[u8],
    width: u32,
    height: u32,
    qscale: u8,
) -> Result<Vec<u8>, EncodeError> {
    use ffmpeg_sys_next::AVPixelFormat as F;
    init();
    if width == 0 || height == 0 || rgb.len() as u64 != u64::from(width) * u64::from(height) * 3 {
        return Err(EncodeError::Invalid(format!(
            "{} bytes aren't a {width}x{height} RGB picture",
            rgb.len()
        )));
    }
    if !JPEG_QSCALE.contains(&qscale) {
        return Err(EncodeError::Invalid(format!(
            "a JPEG quantizer scale is 2 to 31, not {qscale}"
        )));
    }
    let codec = ff::encoder::find(ff::codec::Id::MJPEG)
        .ok_or_else(|| EncodeError::NoEncoder("mjpeg".into()))?;
    let mut enc = ctx(
        ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video(),
        || "JPEG encoder context".into(),
    )?;
    let lambda = i32::from(qscale) * ffi::QP2LAMBDA;
    enc.set_width(width);
    enc.set_height(height);
    enc.set_format(ff::format::Pixel::YUVJ420P);
    enc.set_time_base(ff::Rational::new(1, 25));
    enc.set_flags(ff::codec::Flags::QSCALE);
    enc.set_global_quality(lambda);
    let mut enc = ctx(enc.open(), || "opening mjpeg".into())?;

    let mut src = ff::frame::Video::new(ff::format::Pixel::RGB24, width, height);
    let (w, h) = (width as usize * 3, height as usize);
    let stride = src.stride(0);
    let data = src.data_mut(0);
    for (y, row) in rgb.chunks_exact(w).enumerate().take(h) {
        data[y * stride..y * stride + w].copy_from_slice(row);
    }
    let mut scaler = ffi::Scaler::new(
        F::AV_PIX_FMT_RGB24 as i32,
        F::AV_PIX_FMT_YUVJ420P as i32,
        width,
        height,
    )
    .map_err(|e| EncodeError::Invalid(format!("swscale: {e}")))?;
    let mut yuv = ctx(scaler.scale(&src), || "converting to YUV".into())?;
    yuv.set_pts(Some(0));
    // The encoder takes a fixed quantizer from the frame (QSCALE).
    ffi::set_frame_quality(&mut yuv, lambda);
    ctx(enc.send_frame(&yuv), || "encoding the JPEG".into())?;
    ctx(enc.send_eof(), || "finishing the JPEG".into())?;
    let mut out = Vec::new();
    let mut packet = ff::Packet::empty();
    loop {
        match enc.receive_packet(&mut packet) {
            Ok(()) => out.extend_from_slice(packet.data().unwrap_or_default()),
            Err(ff::Error::Eof) => break,
            Err(e) => {
                return Err(EncodeError::Ffmpeg {
                    context: "encoding the JPEG".into(),
                    error: e,
                });
            }
        }
    }
    Ok(out)
}

pub struct Mp4Writer {
    /// The destination as given to `create`.
    path: PathBuf,
    /// What FFmpeg writes to; `finish` renames it onto the destination.
    tmp: TempFile,
    octx: ff::format::context::Output,
    video: ff::encoder::Video,
    /// What crops the video stream on the left, when it is.
    crop: Option<ffi::CropFilter>,
    video_index: usize,
    video_tb: ff::Rational,
    width: u32,
    height: u32,
    layout: PixelLayout,
    /// The timestamp `push_video` gives its next frame.
    next_video_pts: i64,
    /// Ticks of `video_tb` per frame at the stream's rate, when whole.
    frame_ticks: Option<i64>,
    /// The timestamp of the last frame pushed.
    last_video_pts: Option<i64>,
    audio: Option<AudioState>,
}

impl std::fmt::Debug for Mp4Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp4Writer")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

struct AudioState {
    enc: ff::encoder::Audio,
    index: usize,
    tb: ff::Rational,
    /// Samples per frame sent to the encoder (the last may be shorter).
    frame_size: usize,
    channels: usize,
    layout: ff::ChannelLayout,
    pending: Vec<Vec<f32>>,
    next_pts: i64,
}

/// FFmpeg's default layout for `channels`.
///
/// Four channels are first-order ambiX here, and they get 4.0, which is
/// not what they are. Nothing better exists for AAC: FFmpeg's native
/// encoder refuses the ambisonic and unspecified channel orders (checked on
/// 6.1, 8.1 and 9.0), and an AAC stream can only name speaker positions.
/// 4.0 is channel configuration 4, which every decoder handles and which
/// keeps the channel order (the synth crate's writers test checks each
/// channel); quad needs a PCE and 3.1 low-passes a channel as LFE. The
/// `SA3D` box that `inject` adds is what marks the track as ambiX, as
/// YouTube's spatial audio spec intends. FFmpeg's mp4 demuxer reads it as
/// "ambisonic 1", until its AAC decoder reports 4.0 again.
fn channel_layout(channels: u32) -> Result<ff::ChannelLayout, EncodeError> {
    let n = i32::try_from(channels)
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| EncodeError::Invalid(format!("{channels} audio channels")))?;
    Ok(ff::ChannelLayout::default(n))
}

/// Samples per frame for an encoder that takes frames of any length: 20 ms,
/// a common packet duration.
fn variable_frame_size(sample_rate: u32) -> usize {
    (sample_rate as usize / 50).max(1)
}

/// Write every packet the encoder has ready, through `crop` when there is
/// one. EAGAIN (it wants more input) and EOF (it is fully flushed) end the
/// loop; any other error is real.
fn drain(
    enc: &mut ff::encoder::Encoder,
    octx: &mut ff::format::context::Output,
    index: usize,
    enc_tb: ff::Rational,
    what: &str,
    mut crop: Option<&mut ffi::CropFilter>,
) -> Result<(), EncodeError> {
    let mut pkt = ff::Packet::empty();
    loop {
        match enc.receive_packet(&mut pkt) {
            Ok(()) => {}
            Err(e) if ends_drain(&e) => return Ok(()),
            Err(error) => {
                return Err(EncodeError::Ffmpeg {
                    context: format!("encoding {what}"),
                    error,
                });
            }
        }
        match crop.as_deref_mut() {
            None => write_packet(&mut pkt, octx, index, enc_tb, what)?,
            Some(f) => {
                ctx(f.send(Some(&mut pkt)), || format!("cropping {what}"))?;
                drain_crop(f, octx, index, enc_tb, what)?;
            }
        }
    }
}

/// Whether `e` is EAGAIN or EOF, which end a drain.
fn ends_drain(e: &ff::Error) -> bool {
    match e {
        ff::Error::Other { errno } => *errno == ff::util::error::EAGAIN,
        ff::Error::Eof => true,
        _ => false,
    }
}

/// Write every packet `crop` has ready.
fn drain_crop(
    crop: &mut ffi::CropFilter,
    octx: &mut ff::format::context::Output,
    index: usize,
    enc_tb: ff::Rational,
    what: &str,
) -> Result<(), EncodeError> {
    let mut out = ff::Packet::empty();
    loop {
        match crop.receive(&mut out) {
            Ok(()) => write_packet(&mut out, octx, index, enc_tb, what)?,
            Err(e) if ends_drain(&e) => return Ok(()),
            Err(error) => {
                return Err(EncodeError::Ffmpeg {
                    context: format!("cropping {what}"),
                    error,
                });
            }
        }
    }
}

/// Write `pkt`, in the encoder's time base `enc_tb`, to stream `index`.
fn write_packet(
    pkt: &mut ff::Packet,
    octx: &mut ff::format::context::Output,
    index: usize,
    enc_tb: ff::Rational,
    what: &str,
) -> Result<(), EncodeError> {
    let ost_tb = octx
        .stream(index)
        .map(|s| s.time_base())
        .ok_or_else(|| EncodeError::Invalid(format!("{what} stream vanished")))?;
    pkt.set_stream(index);
    pkt.rescale_ts(enc_tb, ost_tb);
    ctx(pkt.write_interleaved(octx), || {
        format!("writing {what} packet")
    })
}

/// The only sample format the writer sends.
const PLANAR_F32: ff::format::Sample = ff::format::Sample::F32(ff::format::sample::Type::Planar);

/// The encoder for `a`, if this FFmpeg has it and it takes what we send.
fn audio_encoder(a: &AudioSettings) -> Result<ff::Codec, EncodeError> {
    let codec = ff::encoder::find_by_name(&a.encoder)
        .ok_or_else(|| EncodeError::NoEncoder(a.encoder.clone()))?;
    // We only send planar float; say so rather than let the encoder fail to
    // open with EINVAL (libopus takes packed).
    if ffi::sample_formats(&codec).is_some_and(|f| !f.contains(&ffi::SAMPLE_FMT_FLTP)) {
        return Err(EncodeError::Invalid(format!(
            "audio encoder `{}` does not take planar float samples",
            a.encoder
        )));
    }
    Ok(codec)
}

fn ff_rational(r: Rational) -> Result<ff::Rational, EncodeError> {
    let n = i32::try_from(r.num()).map_err(|_| EncodeError::Invalid("rate too large".into()))?;
    let d = i32::try_from(r.den()).map_err(|_| EncodeError::Invalid("rate too large".into()))?;
    Ok(ff::Rational::new(n, d))
}

fn dict(opts: &[(String, String)]) -> ff::Dictionary<'static> {
    let mut d = ff::Dictionary::new();
    for (k, v) in opts {
        d.set(k, v);
    }
    d
}

impl Mp4Writer {
    /// Start an MP4 file (whatever `path`'s extension) to be written to
    /// `path` by [`finish`](Self::finish). Until then it lives under a
    /// temporary name in the same directory, removed if the writer is
    /// dropped unfinished. An existing file at `path` is replaced as
    /// [`inject::inject`] replaces its output: permissions kept, symlinks
    /// written through, anything but a regular file refused.
    pub fn create(
        path: &Path,
        v: &VideoSettings,
        a: Option<&AudioSettings>,
    ) -> Result<Self, EncodeError> {
        may_block("creating a file to encode into");
        init();
        if !v.width.is_multiple_of(2)
            || !v.height.is_multiple_of(2)
            || v.width == 0
            || v.height == 0
        {
            return Err(EncodeError::Invalid(format!(
                "frame size {}x{} must be non-zero and even",
                v.width, v.height
            )));
        }
        if !v.crop_left.is_multiple_of(2) || v.crop_left >= v.width {
            return Err(EncodeError::Invalid(format!(
                "cropping {} of {} columns: it must be even and leave some",
                v.crop_left, v.width
            )));
        }
        // Both encoders exist before anything touches the disk.
        let vcodec = ff::encoder::find_by_name(&v.encoder)
            .ok_or_else(|| EncodeError::NoEncoder(v.encoder.clone()))?;
        let acodec = a.map(audio_encoder).transpose()?;

        let dest = follow_links(path).map_err(|error| EncodeError::Io {
            path: path.to_path_buf(),
            error,
        })?;
        let io_err = |error| EncodeError::Io {
            path: dest.clone(),
            error,
        };
        let tmp = TempFile::create(&dest).map_err(io_err)?;
        // FFmpeg writes through our handle rather than reopening the file by
        // name, so it writes exactly the file we created.
        let file = tmp.file().try_clone().map_err(io_err)?;
        let stream = ctx(ff::format::context::StreamIo::from_write_seek(file), || {
            format!("opening {}", dest.display())
        })?;
        let mut octx = ctx(
            ff::format::output_to_stream(stream, None, Some("mp4")),
            || format!("creating {}", dest.display()),
        )?;
        let global_header = octx
            .format()
            .flags()
            .contains(ff::format::Flags::GLOBAL_HEADER);

        let video_index = ctx(octx.add_stream(vcodec), || "adding video stream".into())?.index();
        let mut venc = ctx(
            ff::codec::context::Context::new_with_codec(vcodec)
                .encoder()
                .video(),
            || "video encoder context".into(),
        )?;
        let fps = ff_rational(v.rate.as_rational())?;
        let tb = match v.time_base {
            Some(tb) if tb.is_positive() => tb,
            Some(tb) => return Err(EncodeError::Invalid(format!("time base {tb}"))),
            None => v.rate.frame_duration().as_rational(),
        };
        let video_tb = ff_rational(tb)?;
        // Ticks per frame: (1 / rate) / tb, when that is whole.
        let frame_ticks = v
            .rate
            .frame_duration()
            .as_rational()
            .checked_div(tb)
            .ok()
            .filter(|t| t.den() == 1)
            .map(|t| t.num());
        let first_pts = match frame_ticks {
            Some(t) => -i64::from(v.edit_start) * t,
            None if v.edit_start == 0 => 0,
            None => {
                return Err(EncodeError::Invalid(format!(
                    "an edit list needs whole frames of the time base {tb}"
                )));
            }
        };
        venc.set_width(v.width);
        venc.set_height(v.height);
        venc.set_format(v.layout.ff());
        venc.set_time_base(video_tb);
        venc.set_frame_rate(Some(fps));
        ffi::set_color_tags(&mut venc, v.color.into());
        if global_header {
            venc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        }
        let venc = ctx(venc.open_with(dict(&v.options)), || {
            format!("opening {} for {}x{}", v.encoder, v.width, v.height)
        })?;
        let crop = match v.crop_left {
            0 => None,
            left => Some(ctx(
                ffi::CropFilter::left(&ff::codec::Parameters::from(&venc), video_tb, left),
                || format!("cropping {left} columns on the left of {}", v.encoder),
            )?),
        };
        {
            let mut ost = octx
                .stream_mut(video_index)
                .ok_or_else(|| EncodeError::Invalid("video stream vanished".into()))?;
            match &crop {
                None => ost.set_parameters(&venc),
                Some(f) => ctx(f.parameters_to(&mut ost, v.crop_left), || {
                    "setting the cropped stream's parameters".into()
                })?,
            }
            ost.set_time_base(video_tb);
            ost.set_avg_frame_rate(fps);
        }

        let audio = match a.zip(acodec) {
            None => None,
            Some((a, acodec)) => {
                let index = ctx(octx.add_stream(acodec), || "adding audio stream".into())?.index();
                let mut aenc = ctx(
                    ff::codec::context::Context::new_with_codec(acodec)
                        .encoder()
                        .audio(),
                    || "audio encoder context".into(),
                )?;
                let layout = channel_layout(a.channels)?;
                let rate = i32::try_from(a.sample_rate)
                    .map_err(|_| EncodeError::Invalid("sample rate too large".into()))?;
                aenc.set_rate(rate);
                aenc.set_channel_layout(layout);
                aenc.set_format(PLANAR_F32);
                aenc.set_bit_rate(a.bit_rate);
                let tb = ff::Rational::new(1, rate);
                aenc.set_time_base(tb);
                if global_header {
                    aenc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
                }
                let aenc = ctx(aenc.open_with(dict(&a.options)), || {
                    format!("opening {}", a.encoder)
                })?;
                // A non-zero frame size is fixed (AAC 1024, Opus 960, Vorbis
                // 64): every frame but the last must have exactly that many.
                let frame_size = match aenc.frame_size() as usize {
                    0 => variable_frame_size(a.sample_rate),
                    n => n,
                };
                {
                    let mut ost = octx
                        .stream_mut(index)
                        .ok_or_else(|| EncodeError::Invalid("audio stream vanished".into()))?;
                    ost.set_parameters(&aenc);
                    ost.set_time_base(tb);
                }
                Some(AudioState {
                    enc: aenc,
                    index,
                    tb,
                    frame_size,
                    channels: a.channels as usize,
                    layout,
                    pending: vec![Vec::new(); a.channels as usize],
                    next_pts: 0,
                })
            }
        };

        ctx(octx.write_header(), || "writing MP4 header".into())?;
        Ok(Mp4Writer {
            path: path.to_path_buf(),
            tmp,
            octx,
            video: venc,
            crop,
            video_index,
            video_tb,
            width: v.width,
            height: v.height,
            layout: v.layout,
            next_video_pts: first_pts,
            frame_ticks,
            last_video_pts: None,
            audio,
        })
    }

    /// Append the next video frame, one frame after the last.
    pub fn push_video(&mut self, planes: &YuvPlanes) -> Result<(), EncodeError> {
        self.push_next(Pushed::Planes(planes), false)
    }

    /// Append the next video frame, one frame after the last, from one
    /// buffer of strided planes.
    pub fn push_video_strided(&mut self, planes: &StridedPlanes<'_>) -> Result<(), EncodeError> {
        self.push_next(Pushed::Strided(planes), false)
    }

    /// Append the next video frame at timestamp `pts`, in ticks of the
    /// stream's time base, for a variable frame rate. Timestamps must
    /// increase; negative ones are before the edit list's start.
    pub fn push_video_at(&mut self, planes: &YuvPlanes, pts: i64) -> Result<(), EncodeError> {
        self.push_frame(Pushed::Planes(planes), pts, false)
    }

    /// Append the next video frame, one frame after the last, and have
    /// the encoder code it as an I-frame (`AV_PICTURE_TYPE_I`). x264 makes
    /// it a keyframe only when it comes at least `min-keyint` frames after
    /// the last one, or in a closed GOP; otherwise it is an I-frame with
    /// no recovery point that the frames after it may refer across, as
    /// x264 codes a flash. For test media.
    pub fn push_video_intra(&mut self, planes: &YuvPlanes) -> Result<(), EncodeError> {
        self.push_next(Pushed::Planes(planes), true)
    }

    fn push_next(&mut self, planes: Pushed<'_>, intra: bool) -> Result<(), EncodeError> {
        let Some(ticks) = self.frame_ticks else {
            return Err(EncodeError::Invalid(
                "a frame is not a whole number of ticks of the time base: use push_video_at".into(),
            ));
        };
        let pts = self.next_video_pts;
        self.push_frame(planes, pts, intra)?;
        self.next_video_pts = pts + ticks;
        Ok(())
    }

    /// Append a video frame at `pts`, coded as an I-frame when `intra`.
    fn push_frame(&mut self, planes: Pushed<'_>, pts: i64, intra: bool) -> Result<(), EncodeError> {
        may_block("encoding a frame to a file");
        if let Some(last) = self.last_video_pts
            && pts <= last
        {
            return Err(EncodeError::Invalid(format!(
                "video timestamp {pts} after {last}: timestamps must increase"
            )));
        }
        let mut frame = video_frame(self.layout, self.width, self.height, planes)?;
        frame.set_pts(Some(pts));
        if intra {
            frame.set_kind(ff::picture::Type::I);
        }
        self.last_video_pts = Some(pts);
        ctx(self.video.send_frame(&frame), || {
            "sending video frame".into()
        })?;
        self.drain_video()
    }

    /// Append planar audio, `samples[ch][i]`: one slice per channel, all
    /// the same length.
    pub fn push_audio(&mut self, samples: &[Vec<f32>]) -> Result<(), EncodeError> {
        may_block("encoding audio to a file");
        let Some(a) = self.audio.as_mut() else {
            return Err(EncodeError::Invalid("no audio stream".into()));
        };
        if samples.len() != a.channels {
            return Err(EncodeError::Invalid(format!(
                "{} audio channels pushed to a {}-channel stream",
                samples.len(),
                a.channels
            )));
        }
        let n = samples.first().map_or(0, Vec::len);
        if let Some((ch, s)) = samples.iter().enumerate().find(|(_, s)| s.len() != n) {
            return Err(EncodeError::Invalid(format!(
                "audio channel {ch} has {} samples, channel 0 has {n}",
                s.len()
            )));
        }
        for (p, s) in a.pending.iter_mut().zip(samples) {
            p.extend_from_slice(s);
        }
        self.flush_audio_frames(false)
    }

    fn flush_audio_frames(&mut self, final_partial: bool) -> Result<(), EncodeError> {
        loop {
            let Some(a) = self.audio.as_mut() else {
                return Ok(());
            };
            let avail = a.pending.first().map_or(0, Vec::len);
            let n = if avail >= a.frame_size {
                a.frame_size
            } else if final_partial && avail > 0 {
                avail
            } else {
                return Ok(());
            };
            let mut frame = ff::frame::Audio::new(PLANAR_F32, n, a.layout);
            frame.set_rate(a.tb.denominator() as u32);
            for ch in 0..a.channels {
                let plane = frame.plane_mut::<f32>(ch);
                plane[..n].copy_from_slice(&a.pending[ch][..n]);
                a.pending[ch].drain(..n);
            }
            frame.set_pts(Some(a.next_pts));
            a.next_pts += n as i64;
            ctx(a.enc.send_frame(&frame), || "sending audio frame".into())?;
            self.drain_audio()?;
        }
    }

    fn drain_video(&mut self) -> Result<(), EncodeError> {
        drain(
            &mut self.video,
            &mut self.octx,
            self.video_index,
            self.video_tb,
            "video",
            self.crop.as_mut(),
        )
    }

    fn drain_audio(&mut self) -> Result<(), EncodeError> {
        let Some(a) = self.audio.as_mut() else {
            return Ok(());
        };
        drain(&mut a.enc, &mut self.octx, a.index, a.tb, "audio", None)
    }

    /// Flush the encoders, write the trailer and rename the file into place.
    /// Returns the path it was created with.
    pub fn finish(self) -> Result<PathBuf, EncodeError> {
        may_block("finishing a file");
        let (path, tmp) = self.close()?;
        let dest = tmp.dest().to_path_buf();
        tmp.persist_replace()
            .map_err(|error| EncodeError::Io { path: dest, error })?;
        Ok(path)
    }

    /// Like [`finish`](Self::finish), but the file goes into place through
    /// [`inject::inject`] with `edits` applied, so it appears with its
    /// metadata boxes (spherical video and audio, say) already in it.
    pub fn finish_with_edits(self, edits: &[EntryEdit]) -> Result<PathBuf, EncodeError> {
        may_block("finishing a file");
        let (path, tmp) = self.close()?;
        inject::inject(tmp.path(), tmp.dest(), edits)?;
        // `tmp` is removed as it drops.
        Ok(path)
    }

    /// Like [`finish_with_edits`](Self::finish_with_edits) (or
    /// [`finish`](Self::finish) with no edits), but the finished file is
    /// handed back still under its temporary name, for the caller to
    /// check and then put in place with `persist_new` or
    /// `persist_replace` (D16), or to drop, which removes it.
    pub fn finish_temp(self, edits: &[EntryEdit]) -> Result<TempFile, EncodeError> {
        may_block("finishing a file");
        let (_, tmp) = self.close()?;
        if edits.is_empty() {
            return Ok(tmp);
        }
        let dest = tmp.dest().to_path_buf();
        let mut out = TempFile::create(&dest).map_err(|error| EncodeError::Io {
            path: dest.clone(),
            error,
        })?;
        inject::inject_into(tmp.path(), &mut out, edits)?;
        Ok(out)
    }

    /// Flush everything into the temporary file and close the muxer.
    fn close(mut self) -> Result<(PathBuf, TempFile), EncodeError> {
        ctx(self.video.send_eof(), || "flushing video".into())?;
        self.drain_video()?;
        if let Some(f) = self.crop.as_mut() {
            ctx(f.send(None), || "flushing the video's cropping".into())?;
            drain_crop(f, &mut self.octx, self.video_index, self.video_tb, "video")?;
        }
        if self.audio.is_some() {
            self.flush_audio_frames(true)?;
            if let Some(a) = self.audio.as_mut() {
                ctx(a.enc.send_eof(), || "flushing audio".into())?;
            }
            self.drain_audio()?;
        }
        ctx(self.octx.write_trailer(), || "writing MP4 trailer".into())?;
        let Mp4Writer {
            path, tmp, octx, ..
        } = self;
        // The trailer flushed FFmpeg's buffer; this lets go of its handle.
        drop(octx);
        Ok((path, tmp))
    }
}

/// Copy the first video stream of each of `srcs` into one new Matroska
/// file at `dst`, in that order, packets as they are, and return `dst`:
/// a camera's raw file, which holds one video stream a lens (the X5's
/// `.insv`), for tests. The packets are read from each source in turn,
/// so that the muxer interleaves them as it goes. Timestamps are
/// rounded to the millisecond, as [`remux_to_matroska`]'s are.
pub fn mux_video_streams(srcs: &[&Path], dst: &Path) -> Result<PathBuf, EncodeError> {
    may_block("muxing files");
    init();
    let mut inputs = Vec::with_capacity(srcs.len());
    for src in srcs {
        let ictx = ffi::open_input(src, None)
            .map_err(|e| crate::MediaError::opening(src, e, |e| e.to_string()))?;
        inputs.push(ictx);
    }
    let dest = follow_links(dst).map_err(|error| EncodeError::Io {
        path: dst.to_path_buf(),
        error,
    })?;
    let io_err = |error| EncodeError::Io {
        path: dest.clone(),
        error,
    };
    let tmp = TempFile::create(&dest).map_err(io_err)?;
    let file = tmp.file().try_clone().map_err(io_err)?;
    let stream = ctx(ff::format::context::StreamIo::from_write_seek(file), || {
        format!("opening {}", dest.display())
    })?;
    let mut octx = ctx(
        ff::format::output_to_stream(stream, None, Some("matroska")),
        || format!("creating {}", dest.display()),
    )?;
    // Per input: its video stream's index, the output stream's, and the
    // input's time base.
    let mut map = Vec::with_capacity(inputs.len());
    for (ictx, src) in inputs.iter().zip(srcs) {
        let ist = ictx
            .streams()
            .find(|s| ffi::media_type(&s.parameters()) == ff::media::Type::Video)
            .ok_or_else(|| EncodeError::Invalid(format!("{} has no video", src.display())))?;
        let mut ost = ctx(octx.add_stream(None::<ff::Codec>), || {
            "adding a stream".into()
        })?;
        ost.set_parameters(ist.parameters());
        ost.set_avg_frame_rate(ist.avg_frame_rate());
        ffi::clear_codec_tag(&mut ost);
        map.push((ist.index(), ost.index(), ist.time_base()));
    }
    ctx(octx.write_header(), || "writing Matroska header".into())?;
    let out_tb: Vec<ff::Rational> = octx.streams().map(|s| s.time_base()).collect();
    let mut packets: Vec<_> = inputs.iter_mut().map(|i| i.packets()).collect();
    let mut open = vec![true; packets.len()];
    while open.iter().any(|o| *o) {
        for (i, it) in packets.iter_mut().enumerate() {
            if !open[i] {
                continue;
            }
            let (from, to, in_tb) = map[i];
            // The next packet of this input's video stream, if any.
            let next = it.by_ref().find(|(s, _)| s.index() == from);
            let Some((_, mut pkt)) = next else {
                open[i] = false;
                continue;
            };
            pkt.rescale_ts(in_tb, out_tb[to]);
            pkt.set_position(-1);
            pkt.set_stream(to);
            ctx(pkt.write_interleaved(&mut octx), || {
                "writing Matroska packet".into()
            })?;
        }
    }
    drop(packets);
    ctx(octx.write_trailer(), || "writing Matroska trailer".into())?;
    drop(octx);
    tmp.persist_replace().map_err(|error| EncodeError::Io {
        path: dest.clone(),
        error,
    })?;
    Ok(dst.to_path_buf())
}

/// Copy the video and audio streams of `src` into a new Matroska file at
/// `dst`, packets as they are, and return `dst`. FFmpeg's Matroska muxer
/// writes through our own handle, as [`Mp4Writer`] does, and keeps its
/// default time base of 1/1000 s, so every timestamp is rounded to the
/// millisecond. MP4 edit lists are not carried over: `src` should have
/// none. The file appears at `dst` only once it is complete.
pub fn remux_to_matroska(src: &Path, dst: &Path) -> Result<PathBuf, EncodeError> {
    remux_to_matroska_at(src, dst, None)
}

/// [`remux_to_matroska`], with the video track's default duration that of
/// `rate` rather than of the source's average rate, whatever the frames'
/// timestamps: a file whose default duration says a rate its blocks don't
/// keep, as recorders that set a nominal rate write.
pub fn remux_to_matroska_at(
    src: &Path,
    dst: &Path,
    rate: Option<FrameRate>,
) -> Result<PathBuf, EncodeError> {
    may_block("remuxing a file");
    init();
    let mut ictx = ffi::open_input(src, None)
        .map_err(|e| crate::MediaError::opening(src, e, |e| e.to_string()))?;
    let dest = follow_links(dst).map_err(|error| EncodeError::Io {
        path: dst.to_path_buf(),
        error,
    })?;
    let io_err = |error| EncodeError::Io {
        path: dest.clone(),
        error,
    };
    let tmp = TempFile::create(&dest).map_err(io_err)?;
    let file = tmp.file().try_clone().map_err(io_err)?;
    let stream = ctx(ff::format::context::StreamIo::from_write_seek(file), || {
        format!("opening {}", dest.display())
    })?;
    let mut octx = ctx(
        ff::format::output_to_stream(stream, None, Some("matroska")),
        || format!("creating {}", dest.display()),
    )?;
    // Output stream and input time base, for each input stream copied.
    let mut map: Vec<Option<(usize, ff::Rational)>> = vec![None; ictx.nb_streams() as usize];
    for ist in ictx.streams() {
        let par = ist.parameters();
        if !matches!(
            ffi::media_type(&par),
            ff::media::Type::Video | ff::media::Type::Audio
        ) {
            continue;
        }
        let video = ffi::media_type(&par) == ff::media::Type::Video;
        let mut ost = ctx(octx.add_stream(None::<ff::Codec>), || {
            "adding a stream".into()
        })?;
        ost.set_parameters(par);
        // The Matroska muxer writes a video track's DefaultDuration from it.
        match rate {
            Some(r) if video => ost.set_avg_frame_rate(ff_rational(r.as_rational())?),
            _ => ost.set_avg_frame_rate(ist.avg_frame_rate()),
        }
        // MP4's sample entry type (`avc1`, `hvc1`) means nothing in
        // Matroska, which names codecs its own way.
        ffi::clear_codec_tag(&mut ost);
        map[ist.index()] = Some((ost.index(), ist.time_base()));
    }
    ctx(octx.write_header(), || "writing Matroska header".into())?;
    let out_tb: Vec<ff::Rational> = octx.streams().map(|s| s.time_base()).collect();
    for (ist, mut pkt) in ictx.packets() {
        let Some((index, in_tb)) = map.get(ist.index()).copied().flatten() else {
            continue;
        };
        pkt.rescale_ts(in_tb, out_tb[index]);
        pkt.set_position(-1);
        pkt.set_stream(index);
        ctx(pkt.write_interleaved(&mut octx), || {
            "writing Matroska packet".into()
        })?;
    }
    ctx(octx.write_trailer(), || "writing Matroska trailer".into())?;
    drop(octx);
    tmp.persist_replace().map_err(|error| EncodeError::Io {
        path: dest.clone(),
        error,
    })?;
    Ok(dst.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture like a tile of look's: a label band, white on near-black,
    /// above a smooth picture with a soft grid drawn over it.
    fn picture(w: u32, h: u32) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let px = if y < 22 {
                    let ink = (3..19).contains(&y) && x % 8 < 5 && (x / 8) % 3 != 2;
                    if ink { [255, 255, 255] } else { [16, 16, 16] }
                } else {
                    let base = [(x * 255 / w) as f64, (y * 255 / h) as f64, 96.0];
                    // A line 3 px wide, its middle at half strength.
                    let d = (x % 60).min(60 - x % 60).min((y % 60).min(60 - y % 60));
                    let a = match d {
                        0 => 0.5,
                        1 => 0.25,
                        _ => 0.0,
                    };
                    base.map(|c| (c + (255.0 - c) * a).round() as u8)
                };
                rgb.extend_from_slice(&px);
            }
        }
        rgb
    }

    /// `jpeg` decoded by FFmpeg's own decoder, as RGB.
    fn decode(jpeg: &[u8], w: u32, h: u32) -> Vec<u8> {
        use ffmpeg_sys_next::AVPixelFormat as F;
        let codec = ff::decoder::find(ff::codec::Id::MJPEG).expect("an mjpeg decoder");
        let ctx = ff::codec::context::Context::new_with_codec(codec);
        let mut dec = ff::decoder::Video(ffi::open_decoder(ctx, codec).expect("a decoder"));
        dec.send_packet(&ff::Packet::copy(jpeg)).expect("a packet");
        dec.send_eof().expect("eof");
        let mut frame = ff::frame::Video::empty();
        dec.receive_frame(&mut frame).expect("a frame");
        let mut scaler =
            ffi::Scaler::new(ffi::frame_format(&frame), F::AV_PIX_FMT_RGB24 as i32, w, h)
                .expect("a scaler");
        let rgb = scaler.scale(&frame).expect("RGB");
        let (plane, stride) = ffi::plane_and_stride(&rgb, 0).expect("a plane");
        (0..h as usize)
            .flat_map(|y| plane[y * stride..y * stride + w as usize * 3].to_vec())
            .collect()
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        10.0 * (255.0 * 255.0 / mse).log10()
    }

    #[test]
    fn a_jpeg_decodes_back_to_its_picture() {
        let (w, h) = (320, 180);
        let rgb = picture(w, h);
        let jpeg = encode_jpeg(&rgb, w, h, 3).expect("a JPEG");
        assert_eq!(&jpeg[..3], &[0xFF, 0xD8, 0xFF], "a JPEG starts with SOI");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "and ends with EOI");
        let back = decode(&jpeg, w, h);
        let db = psnr(&rgb, &back);
        assert!(db > 35.0, "PSNR {db:.1} dB");
    }

    #[test]
    fn a_higher_scale_makes_a_smaller_jpeg_and_odd_sizes_encode() {
        let (w, h) = (321, 181);
        let rgb = picture(w, h);
        let fine = encode_jpeg(&rgb, w, h, 3).expect("q3");
        let coarse = encode_jpeg(&rgb, w, h, 12).expect("q12");
        assert!(
            coarse.len() < fine.len(),
            "{} vs {}",
            coarse.len(),
            fine.len()
        );
        let back = decode(&coarse, w, h);
        assert!(psnr(&rgb, &back) > 25.0);
    }

    #[test]
    fn what_isn_t_a_picture_is_refused() {
        assert!(matches!(
            encode_jpeg(&[0; 10], 2, 2, 3),
            Err(EncodeError::Invalid(_))
        ));
        assert!(matches!(
            encode_jpeg(&[0; 12], 2, 2, 1),
            Err(EncodeError::Invalid(_))
        ));
        assert!(matches!(
            encode_jpeg(&[], 0, 0, 3),
            Err(EncodeError::Invalid(_))
        ));
    }
}
