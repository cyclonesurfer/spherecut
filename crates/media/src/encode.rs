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

use spherecut_core::{FrameRate, Rational};
use thiserror::Error;

use crate::inject::{self, EntryEdit, InjectError, TempFile, follow_links};
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
}

fn ctx<T>(r: Result<T, ff::Error>, context: impl FnOnce() -> String) -> Result<T, EncodeError> {
    r.map_err(|error| EncodeError::Ffmpeg {
        context: context(),
        error,
    })
}

/// Pixel layouts the writer accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayout {
    /// 8-bit 4:2:0 planar.
    Yuv420p,
    /// 10-bit 4:2:0 planar, little-endian u16 samples in the low bits.
    Yuv420p10,
}

impl PixelLayout {
    fn ff(self) -> ff::format::Pixel {
        match self {
            PixelLayout::Yuv420p => ff::format::Pixel::YUV420P,
            PixelLayout::Yuv420p10 => ff::format::Pixel::YUV420P10LE,
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
    /// Signal BT.709 matrix/primaries/transfer and limited range.
    pub bt709_limited: bool,
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

pub struct Mp4Writer {
    /// The destination as given to `create`.
    path: PathBuf,
    /// What FFmpeg writes to; `finish` renames it onto the destination.
    tmp: TempFile,
    octx: ff::format::context::Output,
    video: ff::encoder::Video,
    video_index: usize,
    video_tb: ff::Rational,
    width: u32,
    height: u32,
    layout: PixelLayout,
    next_video_pts: i64,
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

/// Write every packet the encoder has ready. EAGAIN (it wants more input)
/// and EOF (it is fully flushed) end the loop; any other error is real.
fn drain(
    enc: &mut ff::encoder::Encoder,
    octx: &mut ff::format::context::Output,
    index: usize,
    enc_tb: ff::Rational,
    what: &str,
) -> Result<(), EncodeError> {
    let ost_tb = octx
        .stream(index)
        .map(|s| s.time_base())
        .ok_or_else(|| EncodeError::Invalid(format!("{what} stream vanished")))?;
    let mut pkt = ff::Packet::empty();
    loop {
        match enc.receive_packet(&mut pkt) {
            Ok(()) => {}
            Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => return Ok(()),
            Err(ff::Error::Eof) => return Ok(()),
            Err(error) => {
                return Err(EncodeError::Ffmpeg {
                    context: format!("encoding {what}"),
                    error,
                });
            }
        }
        pkt.set_stream(index);
        pkt.rescale_ts(enc_tb, ost_tb);
        ctx(pkt.write_interleaved(octx), || {
            format!("writing {what} packet")
        })?;
    }
}

/// The only sample format the writer sends.
const PLANAR_F32: ff::format::Sample = ff::format::Sample::F32(ff::format::sample::Type::Planar);

/// The encoder for `a`, if this FFmpeg has it and it takes what we send.
fn audio_encoder(a: &AudioSettings) -> Result<ff::Codec, EncodeError> {
    let codec = ff::encoder::find_by_name(&a.encoder)
        .ok_or_else(|| EncodeError::NoEncoder(a.encoder.clone()))?;
    // We only send planar float; say so rather than let the encoder fail to
    // open with EINVAL (libopus takes packed).
    if let Some(mut formats) = codec.audio().ok().and_then(|c| c.formats())
        && !formats.any(|f| f == PLANAR_F32)
    {
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
    /// written through.
    pub fn create(
        path: &Path,
        v: &VideoSettings,
        a: Option<&AudioSettings>,
    ) -> Result<Self, EncodeError> {
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
        let video_tb = fps.invert();
        venc.set_width(v.width);
        venc.set_height(v.height);
        venc.set_format(v.layout.ff());
        venc.set_time_base(video_tb);
        venc.set_frame_rate(Some(fps));
        if v.bt709_limited {
            venc.set_colorspace(ff::color::Space::BT709);
            venc.set_color_range(ff::color::Range::MPEG);
            venc.set_color_primaries(ff::color::Primaries::BT709);
            venc.set_color_transfer_characteristic(ff::color::TransferCharacteristic::BT709);
        }
        if global_header {
            venc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        }
        let venc = ctx(venc.open_with(dict(&v.options)), || {
            format!("opening {} for {}x{}", v.encoder, v.width, v.height)
        })?;
        {
            let mut ost = octx
                .stream_mut(video_index)
                .ok_or_else(|| EncodeError::Invalid("video stream vanished".into()))?;
            ost.set_parameters(&venc);
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
            video_index,
            video_tb,
            width: v.width,
            height: v.height,
            layout: v.layout,
            next_video_pts: 0,
            audio,
        })
    }

    /// Append the next video frame.
    pub fn push_video(&mut self, planes: &YuvPlanes) -> Result<(), EncodeError> {
        let mut frame = ff::frame::Video::new(self.layout.ff(), self.width, self.height);
        let dims = [
            (self.width as usize, self.height as usize),
            (self.width as usize / 2, self.height as usize / 2),
            (self.width as usize / 2, self.height as usize / 2),
        ];
        for (i, &(w, h)) in dims.iter().enumerate() {
            let stride = frame.stride(i);
            let data = frame.data_mut(i);
            match (planes, self.layout) {
                (YuvPlanes::U8(p), PixelLayout::Yuv420p) => {
                    if p[i].len() != w * h {
                        return Err(EncodeError::Invalid(format!("plane {i} has wrong size")));
                    }
                    for y in 0..h {
                        data[y * stride..y * stride + w].copy_from_slice(&p[i][y * w..(y + 1) * w]);
                    }
                }
                (YuvPlanes::U16(p), PixelLayout::Yuv420p10) => {
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
        frame.set_pts(Some(self.next_video_pts));
        self.next_video_pts += 1;
        ctx(self.video.send_frame(&frame), || {
            "sending video frame".into()
        })?;
        self.drain_video()
    }

    /// Append planar audio, `samples[ch][i]`: one slice per channel, all
    /// the same length.
    pub fn push_audio(&mut self, samples: &[Vec<f32>]) -> Result<(), EncodeError> {
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
        )
    }

    fn drain_audio(&mut self) -> Result<(), EncodeError> {
        let Some(a) = self.audio.as_mut() else {
            return Ok(());
        };
        drain(&mut a.enc, &mut self.octx, a.index, a.tb, "audio")
    }

    /// Flush the encoders, write the trailer and rename the file into place.
    /// Returns the path it was created with.
    pub fn finish(self) -> Result<PathBuf, EncodeError> {
        let (path, tmp) = self.close()?;
        let dest = tmp.dest().to_path_buf();
        tmp.persist()
            .map_err(|error| EncodeError::Io { path: dest, error })?;
        Ok(path)
    }

    /// Like [`finish`](Self::finish), but the file goes into place through
    /// [`inject::inject`] with `edits` applied, so it appears with its
    /// metadata boxes (spherical video and audio, say) already in it.
    pub fn finish_with_edits(self, edits: &[EntryEdit]) -> Result<PathBuf, EncodeError> {
        let (path, tmp) = self.close()?;
        inject::inject(tmp.path(), tmp.dest(), edits)?;
        // `tmp` is removed as it drops.
        Ok(path)
    }

    /// Flush everything into the temporary file and close the muxer.
    fn close(mut self) -> Result<(PathBuf, TempFile), EncodeError> {
        ctx(self.video.send_eof(), || "flushing video".into())?;
        self.drain_video()?;
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
