//! A small synchronous MP4 writer: one video stream from planar YUV frames
//! and an optional audio stream from planar float samples.
//!
//! The synthetic test-media generator uses it today; the export pipeline
//! (M2) builds on it with hardware encoders, a render queue and
//! cancellation.

use std::path::{Path, PathBuf};

use spherecut_core::{FrameRate, Rational};
use thiserror::Error;

use crate::{ff, init};

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("encoder `{0}` is not available in this FFmpeg build")]
    NoEncoder(String),
    #[error("{context}: {source}")]
    Ffmpeg { context: String, source: ff::Error },
    #[error("{0}")]
    Invalid(String),
}

fn ctx<T>(r: Result<T, ff::Error>, context: impl FnOnce() -> String) -> Result<T, EncodeError> {
    r.map_err(|source| EncodeError::Ffmpeg {
        context: context(),
        source,
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
}

/// One planar YUV frame. Plane strides equal plane widths.
#[derive(Debug, Clone)]
pub enum YuvPlanes {
    U8([Vec<u8>; 3]),
    U16([Vec<u16>; 3]),
}

pub struct Mp4Writer {
    path: PathBuf,
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
    frame_size: usize,
    channels: usize,
    pending: Vec<Vec<f32>>,
    next_pts: i64,
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
        let mut octx = ctx(ff::format::output(path), || {
            format!("creating {}", path.display())
        })?;
        let global_header = octx
            .format()
            .flags()
            .contains(ff::format::Flags::GLOBAL_HEADER);

        let vcodec = ff::encoder::find_by_name(&v.encoder)
            .ok_or_else(|| EncodeError::NoEncoder(v.encoder.clone()))?;
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

        let audio = match a {
            None => None,
            Some(a) => {
                let acodec = ff::encoder::find_by_name(&a.encoder)
                    .ok_or_else(|| EncodeError::NoEncoder(a.encoder.clone()))?;
                let index = ctx(octx.add_stream(acodec), || "adding audio stream".into())?.index();
                let mut aenc = ctx(
                    ff::codec::context::Context::new_with_codec(acodec)
                        .encoder()
                        .audio(),
                    || "audio encoder context".into(),
                )?;
                let layout = ff::ChannelLayout::default(a.channels as i32);
                let rate = i32::try_from(a.sample_rate)
                    .map_err(|_| EncodeError::Invalid("sample rate too large".into()))?;
                aenc.set_rate(rate);
                aenc.set_channel_layout(layout);
                aenc.set_format(ff::format::Sample::F32(ff::format::sample::Type::Planar));
                aenc.set_bit_rate(a.bit_rate);
                let tb = ff::Rational::new(1, rate);
                aenc.set_time_base(tb);
                if global_header {
                    aenc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
                }
                let aenc = ctx(aenc.open(), || format!("opening {}", a.encoder))?;
                let frame_size = (aenc.frame_size() as usize).max(1024);
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
                    pending: vec![Vec::new(); a.channels as usize],
                    next_pts: 0,
                })
            }
        };

        ctx(octx.write_header(), || "writing MP4 header".into())?;
        Ok(Mp4Writer {
            path: path.to_path_buf(),
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

    /// Append interleaved-by-channel audio: `samples[ch][i]`.
    pub fn push_audio(&mut self, samples: &[Vec<f32>]) -> Result<(), EncodeError> {
        let Some(a) = self.audio.as_mut() else {
            return Err(EncodeError::Invalid("no audio stream".into()));
        };
        if samples.len() != a.channels {
            return Err(EncodeError::Invalid("wrong channel count".into()));
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
            let mut frame = ff::frame::Audio::new(
                ff::format::Sample::F32(ff::format::sample::Type::Planar),
                n,
                ff::ChannelLayout::default(a.channels as i32),
            );
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
        let ost_tb = self
            .octx
            .stream(self.video_index)
            .map(|s| s.time_base())
            .ok_or_else(|| EncodeError::Invalid("video stream vanished".into()))?;
        let mut pkt = ff::Packet::empty();
        while self.video.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(self.video_index);
            pkt.rescale_ts(self.video_tb, ost_tb);
            ctx(pkt.write_interleaved(&mut self.octx), || {
                "writing video packet".into()
            })?;
        }
        Ok(())
    }

    fn drain_audio(&mut self) -> Result<(), EncodeError> {
        let Some(a) = self.audio.as_mut() else {
            return Ok(());
        };
        let ost_tb = self
            .octx
            .stream(a.index)
            .map(|s| s.time_base())
            .ok_or_else(|| EncodeError::Invalid("audio stream vanished".into()))?;
        let mut pkt = ff::Packet::empty();
        while a.enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(a.index);
            pkt.rescale_ts(a.tb, ost_tb);
            ctx(pkt.write_interleaved(&mut self.octx), || {
                "writing audio packet".into()
            })?;
        }
        Ok(())
    }

    /// Flush encoders and write the trailer.
    pub fn finish(mut self) -> Result<PathBuf, EncodeError> {
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
        Ok(self.path)
    }
}
