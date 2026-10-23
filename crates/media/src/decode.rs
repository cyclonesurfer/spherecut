//! Sequential video decoding, software or on a hardware device.
//!
//! This is the first slice of the M1 `MediaBackend`: it opens a file,
//! decodes one video stream in order, and hands out frames in system memory.
//! Hardware decoding never silently falls back to software: if the device
//! can't decode the stream, opening or decoding fails with
//! [`MediaError::HwUnsupported`] and the reason, as does every call after
//! it, and the caller chooses what to do.
//!
//! A read error or a cancelled read ends the stream with an error, after
//! the frames that could still be decoded, never with `None`. So does a
//! file that ends before the samples its demuxer's index lists for the
//! stream, which is how an MP4 or MOV with its `moov` first looks when it
//! is cut before the end of the stream's data. The sample the cut falls
//! in is dropped, not handed to the decoder. A cut that the index doesn't
//! reach goes unnoticed: the stream ends with `None` after the last whole
//! frame, and only FFmpeg's log mentions it. That is the case for a
//! Matroska file cut in a cluster, and for most cuts in a fragmented MP4,
//! whose index only grows as its fragments are read.
//!
//! Other streams in the file are not read, so damage in them doesn't end
//! this one. A packet the demuxer reports as invalid data is skipped,
//! since the demuxer can resync after one and later frames keep their
//! timestamps, but with a warning, and counted in
//! [`VideoDecoder::skipped_packets`]; after [`MAX_UNREADABLE_IN_A_ROW`] in
//! a row the file counts as unreadable.

use std::ffi::c_int;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::{Rational, Time};

use crate::ffi::{self, HwDevice, HwRefusal, OpenDecoderError};
use crate::hwprobe::device_label;
use crate::{MediaError, ff, init};

/// A decoded frame in system memory.
pub struct DecodedFrame {
    pub pts: Option<i64>,
    /// `pts` in seconds, exact.
    pub time: Option<Time>,
    /// System-memory frame (YUV420P/YUV420P10LE in software; NV12/P010
    /// from hardware).
    pub frame: ff::frame::Video,
    /// Whether the hardware device decoded this frame.
    pub hw_decoded: bool,
}

impl std::fmt::Debug for DecodedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedFrame")
            .field("pts", &self.pts)
            .field("format", &ffi::pix_fmt_name(ffi::frame_format(&self.frame)))
            .field("size", &(self.frame.width(), self.frame.height()))
            .field("hw_decoded", &self.hw_decoded)
            .finish()
    }
}

impl DecodedFrame {
    /// Copies of the Y, U and V planes as `u16` samples at their native
    /// bit depth, whether the frame is planar (software) or semi-planar
    /// (NV12/P010 from hardware). For comparisons and tests; the render
    /// path uploads planes without copying them like this. `None` for
    /// formats other than 4:2:0 8/10-bit, and for a frame whose rows don't
    /// fit its buffers, such as one whose size or format was changed after
    /// they were allocated.
    pub fn planes_u16(&self) -> Option<[Vec<u16>; 3]> {
        use ff::ffi::AVPixelFormat as P;
        let f = &self.frame;
        let is = |p: P| ffi::frame_format(f) == p as c_int;
        let (w, h) = (f.width() as usize, f.height() as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        // `ffi::plane` holds every row of the frame's width and height,
        // `stride` bytes apart, or is `None`, so the reads below stay in
        // it. Semi-planar formats have no third plane; it is never read
        // for them.
        let planes = [0, 1, 2].map(|i| ffi::plane(f, i));
        let semi_planar = is(P::AV_PIX_FMT_NV12) || is(P::AV_PIX_FMT_P010LE);
        let n = if semi_planar { 2 } else { 3 };
        if planes[..n].iter().any(Option::is_none) {
            return None;
        }
        let planes = planes.map(Option::unwrap_or_default);
        let row = |plane: usize, y: usize| -> &[u8] { &planes[plane][y * f.stride(plane)..] };
        let rd16 = |b: &[u8], i: usize| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]);
        let collect = |n: usize, h: usize, px: &dyn Fn(usize, usize) -> u16| -> Vec<u16> {
            (0..h)
                .flat_map(|y| (0..n).map(move |x| (y, x)))
                .map(|(y, x)| px(y, x))
                .collect()
        };
        Some(if is(P::AV_PIX_FMT_YUV420P) || is(P::AV_PIX_FMT_YUVJ420P) {
            [
                collect(w, h, &|y, x| row(0, y)[x] as u16),
                collect(cw, ch, &|y, x| row(1, y)[x] as u16),
                collect(cw, ch, &|y, x| row(2, y)[x] as u16),
            ]
        } else if is(P::AV_PIX_FMT_YUV420P10LE) {
            [
                collect(w, h, &|y, x| rd16(row(0, y), x)),
                collect(cw, ch, &|y, x| rd16(row(1, y), x)),
                collect(cw, ch, &|y, x| rd16(row(2, y), x)),
            ]
        } else if is(P::AV_PIX_FMT_NV12) {
            [
                collect(w, h, &|y, x| row(0, y)[x] as u16),
                collect(cw, ch, &|y, x| row(1, y)[2 * x] as u16),
                collect(cw, ch, &|y, x| row(1, y)[2 * x + 1] as u16),
            ]
        } else if is(P::AV_PIX_FMT_P010LE) {
            // P010 keeps the 10 bits in the high end of each u16.
            [
                collect(w, h, &|y, x| rd16(row(0, y), x) >> 6),
                collect(cw, ch, &|y, x| rd16(row(1, y), 2 * x) >> 6),
                collect(cw, ch, &|y, x| rd16(row(1, y), 2 * x + 1) >> 6),
            ]
        } else {
            return None;
        })
    }
}

/// Invalid-data packets in a row after which a file is unreadable rather
/// than damaged in places. A demuxer that can't get past a sample keeps
/// failing on it, so this also bounds that.
pub const MAX_UNREADABLE_IN_A_ROW: u32 = 100;

pub struct VideoDecoder {
    path: PathBuf,
    ictx: ff::format::context::Input,
    stream_index: usize,
    time_base: Rational,
    dec: ff::decoder::Video,
    hw: Option<Arc<HwDevice>>,
    /// A packet read ahead, after one flagged corrupt (see `next_packet`).
    lookahead: Option<ff::Packet>,
    /// The demuxer has nothing more to give.
    read_done: bool,
    eof_sent: bool,
    done: bool,
    /// Why reading stopped before a clean end of file; reported once the
    /// decoder has given up the frames it holds.
    read_error: Option<String>,
    /// The error that ended the stream, returned by every later call.
    failed: Option<String>,
    /// Why the device turned the stream down, returned by every later
    /// call.
    refused: Option<HwRefusal>,
    frames: u64,
    skipped: u64,
}

impl std::fmt::Debug for VideoDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoDecoder")
            .field("path", &self.path)
            .field("stream", &self.stream_index)
            .field("hw", &self.hw.as_ref().map(|h| h.kind().to_string()))
            .finish_non_exhaustive()
    }
}

fn err(path: &Path, what: &str, e: impl std::fmt::Display) -> MediaError {
    MediaError::Decode {
        path: path.to_path_buf(),
        reason: format!("{what}: {e}"),
    }
}

fn open_err(path: &Path, e: ff::Error) -> MediaError {
    MediaError::Open {
        path: path.to_path_buf(),
        reason: e.to_string(),
    }
}

fn hw_unsupported(path: &Path, dev: &HwDevice, refusal: HwRefusal) -> MediaError {
    MediaError::HwUnsupported {
        path: path.to_path_buf(),
        kind: device_label(dev.kind()).into(),
        refusal,
    }
}

impl VideoDecoder {
    /// Open the best video stream of `path` (or stream `index`).
    pub fn open(
        path: &Path,
        index: Option<usize>,
        hw: Option<Arc<HwDevice>>,
    ) -> Result<Self, MediaError> {
        init();
        let ictx = ffi::open_input(path, None).map_err(|e| open_err(path, e))?;
        Self::with_input(path, ictx, index, hw)
    }

    /// Like [`open`](Self::open), but reading stops once `cancel` is set:
    /// FFmpeg checks it before each read from the file, and
    /// [`next_frame`](Self::next_frame) then ends the stream with an error
    /// saying the read was cancelled.
    pub fn open_cancellable(
        path: &Path,
        index: Option<usize>,
        hw: Option<Arc<HwDevice>>,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, MediaError> {
        init();
        let ictx = ffi::open_input(path, Some(cancel)).map_err(|e| open_err(path, e))?;
        Self::with_input(path, ictx, index, hw)
    }

    fn with_input(
        path: &Path,
        mut ictx: ff::format::context::Input,
        index: Option<usize>,
        hw: Option<Arc<HwDevice>>,
    ) -> Result<Self, MediaError> {
        tracing::debug!(
            path = %path.display(),
            format = ictx.format().name(),
            streams = ictx.nb_streams(),
            "opened file"
        );
        let st = match index {
            Some(i) => ictx.stream(i),
            None => ictx.streams().best(ff::media::Type::Video),
        }
        .ok_or_else(|| err(path, "no video stream", "file has none"))?;
        let stream_index = st.index();
        let tb = st.time_base();
        let time_base = Rational::new(tb.numerator() as i64, tb.denominator() as i64)
            .map_err(|e| err(path, "bad stream timebase", e))?;
        let mut ctx = ff::codec::context::Context::from_parameters(st.parameters())
            .map_err(|e| err(path, "decoder parameters", e))?;
        ffi::discard_other_streams(&mut ictx, stream_index);
        match &hw {
            Some(dev) => {
                ffi::attach_hw_device(&mut ctx, dev).map_err(|e| err(path, "hardware device", e))?
            }
            // libavcodec defaults to one thread; let it pick per core.
            None => ctx.set_threading(ff::codec::threading::Config {
                kind: ff::codec::threading::Type::Frame,
                count: 0,
            }),
        }
        let dec = ffi::open_video_decoder(ctx, hw.as_deref()).map_err(|e| match (e, &hw) {
            (OpenDecoderError::NotOnDevice, Some(dev)) => {
                hw_unsupported(path, dev, HwRefusal::NotOffered)
            }
            (e, _) => err(path, "opening decoder", e),
        })?;
        let codec = dec.codec();
        tracing::debug!(
            path = %path.display(),
            stream = stream_index,
            decoder = codec.as_ref().map_or("?", |c| c.name()),
            size = %format_args!("{}x{}", dec.width(), dec.height()),
            on = hw.as_ref().map_or("software", |h| h.kind()),
            "decoding video stream"
        );
        Ok(VideoDecoder {
            path: path.to_path_buf(),
            ictx,
            stream_index,
            time_base,
            dec,
            hw,
            lookahead: None,
            read_done: false,
            eof_sent: false,
            done: false,
            read_error: None,
            failed: None,
            refused: None,
            frames: 0,
            skipped: 0,
        })
    }

    pub fn width(&self) -> u32 {
        self.dec.width()
    }

    pub fn height(&self) -> u32 {
        self.dec.height()
    }

    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    /// Packets skipped so far because the demuxer reported invalid data.
    pub fn skipped_packets(&self) -> u64 {
        self.skipped
    }

    /// The next frame in decode-output (presentation) order, or `None` at
    /// the end of the stream.
    ///
    /// When reading the file fails (see the module docs), the frames
    /// decoded before that come first, then the error, which every later
    /// call returns too. So is [`MediaError::HwUnsupported`], once the
    /// device turns the stream down: nothing more is decoded. Any other
    /// error from the decoder itself is returned once; calling again
    /// carries on with the next packet.
    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        if let Some(reason) = &self.failed {
            return Err(err(&self.path, "reading", reason));
        }
        if let Some(e) = self.refusal() {
            return Err(e);
        }
        let mut frame = ff::frame::Video::empty();
        loop {
            if self.done {
                return Ok(None);
            }
            match self.dec.receive_frame(&mut frame) {
                Ok(()) => {
                    let f = self.finish_frame(frame)?;
                    self.frames += 1;
                    return Ok(Some(f));
                }
                Err(ff::Error::Eof) => {
                    self.done = true;
                    tracing::debug!(
                        path = %self.path.display(),
                        frames = self.frames,
                        "end of video stream"
                    );
                    if self.skipped > 0 {
                        tracing::warn!(
                            path = %self.path.display(),
                            "skipped {} unreadable packets",
                            self.skipped
                        );
                    }
                    return match self.read_error.take() {
                        Some(reason) => {
                            let reason = format!("{reason} (after {} frames)", self.frames);
                            self.failed = Some(reason.clone());
                            Err(err(&self.path, "reading", reason))
                        }
                        None => Ok(None),
                    };
                }
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
                Err(e) => return Err(self.decode_error(e)),
            }
            if self.eof_sent {
                // Drained; receive_frame will report Eof next.
                continue;
            }
            match self.next_packet() {
                Some(pkt) => self
                    .dec
                    .send_packet(&pkt)
                    .map_err(|e| self.decode_error(e))?,
                None => {
                    self.dec
                        .send_eof()
                        .map_err(|e| err(&self.path, "flushing decoder", e))?;
                    self.eof_sent = true;
                }
            }
        }
    }

    /// A decoder error, or why the device can't decode this stream when
    /// that is the cause: the decoder's own error then is just "Operation
    /// not permitted".
    fn decode_error(&mut self, e: ff::Error) -> MediaError {
        self.refused = ffi::hw_refusal(&self.dec);
        self.refusal()
            .unwrap_or_else(|| err(&self.path, "decoding", e))
    }

    /// `HwUnsupported`, once the device has turned the stream down.
    fn refusal(&self) -> Option<MediaError> {
        Some(hw_unsupported(&self.path, self.hw.as_ref()?, self.refused?))
    }

    /// The next packet to decode, or `None` once there are no more (see
    /// `read_packet`).
    ///
    /// A sample cut short, by the end of a truncated file or by a read
    /// that failed, comes back flagged corrupt. So such a packet waits for
    /// the read after it: if reading stops there with an error, the packet
    /// is part of that failure and is dropped, rather than sent to the
    /// decoder for an error of its own that would come first.
    fn next_packet(&mut self) -> Option<ff::Packet> {
        let pkt = match self.lookahead.take() {
            Some(pkt) => pkt,
            None => self.read_packet()?,
        };
        if !pkt.is_corrupt() {
            return Some(pkt);
        }
        match self.read_packet() {
            Some(next) => {
                self.lookahead = Some(next);
                Some(pkt)
            }
            None if self.read_error.is_some() => None,
            None => Some(pkt),
        }
    }

    /// The next packet of our stream from the demuxer, or `None` once
    /// there are no more: at a clean end of file, or with `read_error`
    /// saying why not.
    fn read_packet(&mut self) -> Option<ff::Packet> {
        if self.read_done {
            return None;
        }
        let pkt = self.demux();
        self.read_done = pkt.is_none();
        pkt
    }

    fn demux(&mut self) -> Option<ff::Packet> {
        let mut unreadable = 0;
        loop {
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.ictx) {
                Ok(()) if pkt.stream() == self.stream_index => return Some(pkt),
                Ok(()) => unreadable = 0,
                // FFmpeg reports a read error that made the demuxer stop
                // as that error, not as the end of the file.
                Err(ff::Error::Eof) => {
                    self.read_error = self.truncation();
                    return None;
                }
                Err(ff::Error::InvalidData) => {
                    self.skipped += 1;
                    unreadable += 1;
                    if unreadable == MAX_UNREADABLE_IN_A_ROW {
                        self.read_error = Some(format!("{unreadable} unreadable packets in a row"));
                        return None;
                    }
                    // One warning per file; the total comes at the end.
                    if self.skipped == 1 {
                        tracing::warn!(
                            path = %self.path.display(),
                            "skipping a packet the demuxer could not read (invalid data)"
                        );
                    } else {
                        tracing::debug!(
                            path = %self.path.display(),
                            skipped = self.skipped,
                            "skipping another unreadable packet"
                        );
                    }
                }
                // AVERROR_EXIT only comes from the interrupt callback.
                Err(ff::Error::Exit) => {
                    self.read_error = Some("cancelled".into());
                    return None;
                }
                Err(e) => {
                    self.read_error = Some(e.to_string());
                    return None;
                }
            }
        }
    }

    /// At the end of the file: whether it ends before the data the
    /// demuxer's index lists for our stream, i.e. it was cut short. The
    /// other streams don't count, since the demuxer skips them.
    fn truncation(&self) -> Option<String> {
        let end = ffi::index_end(&self.ictx, self.stream_index)?;
        let size = std::fs::metadata(&self.path).ok()?.len();
        (end > size).then(|| {
            format!(
                "the file is truncated: it ends at byte {size}, but its index \
                 lists video data up to byte {end}"
            )
        })
    }

    fn finish_frame(&mut self, frame: ff::frame::Video) -> Result<DecodedFrame, MediaError> {
        let format = ffi::frame_format(&frame);
        let on_device = self.hw.as_ref().is_some_and(|h| format == h.hw_pix_fmt());
        if self.hw.is_some() && !on_device {
            // A decoder that never asked for a format doesn't use the
            // device. One that turned the device down may go on in
            // software.
            self.refused = match ffi::hw_refusal(&self.dec) {
                None if !ffi::hw_format_asked(&self.dec) => Some(HwRefusal::NotOffered),
                r => r,
            };
            return Err(self.refusal().unwrap_or_else(|| {
                err(
                    &self.path,
                    "hardware decode",
                    format!(
                        "decoder produced {} frames instead of device frames",
                        ffi::pix_fmt_name(format).unwrap_or_else(|| format!("format {format}"))
                    ),
                )
            }));
        }
        if on_device {
            ffi::note_device_frame(&mut self.dec);
        }
        let pts = frame.pts();
        let frame = if on_device {
            ffi::download_frame(&frame).map_err(|e| err(&self.path, "downloading frame", e))?
        } else {
            frame
        };
        Ok(DecodedFrame {
            pts,
            time: pts.and_then(|p| Time::from_pts(p, self.time_base).ok()),
            frame,
            hw_decoded: on_device,
        })
    }
}

/// Open a hardware device for decoding. `kind` is "vaapi" or "vulkan".
pub fn open_hw_device(kind: &str, device: Option<&str>) -> Result<Arc<HwDevice>, MediaError> {
    init();
    HwDevice::open(kind, device)
        .map(Arc::new)
        .map_err(|reason| MediaError::Hardware {
            kind: kind.to_string(),
            reason,
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use spherecut_core::FrameRate;

    use super::*;
    use crate::encode::{Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};

    /// A test's own directory in the system's temp dir (unit tests get no
    /// CARGO_TARGET_TMPDIR), removed with what it holds when dropped, so
    /// also when the test fails.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("spherecut-decode-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Ten 256x128 frames of H.264, in `dir`.
    fn clip(dir: &TempDir) -> PathBuf {
        let settings = VideoSettings {
            encoder: "libx264".into(),
            width: 256,
            height: 128,
            rate: FrameRate::FPS_30,
            layout: PixelLayout::Yuv420p,
            options: Vec::new(),
            bt709_limited: true,
        };
        let mut w = Mp4Writer::create(&dir.0.join("clip.mp4"), &settings, None).unwrap();
        for i in 0..10 {
            let luma = vec![16 + 20 * i; 256 * 128];
            let chroma = vec![128; 128 * 64];
            w.push_video(&YuvPlanes::U8([luma, chroma.clone(), chroma]))
                .unwrap();
        }
        w.finish().unwrap()
    }

    #[test]
    fn software_frames_from_a_decoder_that_never_asked_for_a_format_are_refused() {
        // A decoder that ignores its device decodes in software without
        // asking get_format; one opened in software and then given a
        // device is the same.
        let dir = TempDir::new("never-asked");
        let mut d = VideoDecoder::open(&clip(&dir), None, None).unwrap();
        d.hw = Some(Arc::new(HwDevice::vulkan_without_context()));
        for _ in 0..2 {
            match d.next_frame() {
                Err(MediaError::HwUnsupported {
                    refusal: HwRefusal::NotOffered,
                    ..
                }) => {}
                r => panic!("expected HwUnsupported, got {r:?}"),
            }
        }
        // The clip goes with its directory.
        let path = dir.0.clone();
        drop(dir);
        assert!(!path.exists());
    }

    /// A grey 64x16 YUV420P frame, in buffers from av_frame_get_buffer.
    fn grey_frame() -> DecodedFrame {
        let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, 64, 16);
        for i in 0..3 {
            frame.data_mut(i).fill(128);
        }
        DecodedFrame {
            pts: None,
            time: None,
            frame,
            hw_decoded: false,
        }
    }

    #[test]
    fn planes_of_a_frame_changed_after_its_buffers_are_none() {
        use ff::format::Pixel;
        assert!(grey_frame().planes_u16().is_some());
        // ffmpeg-next's safe setters change the frame's size and format,
        // not its buffers, which keep 16 rows of 64 luma and 32 chroma
        // bytes.
        let mut f = grey_frame();
        f.frame.set_height(4096);
        assert!(f.planes_u16().is_none());
        // Rows wider than the bytes from one row to the next: luma rows of
        // 100 bytes, and of 128 at 10 bits.
        let mut f = grey_frame();
        f.frame.set_width(100);
        assert!(f.planes_u16().is_none());
        let mut f = grey_frame();
        f.frame.set_format(Pixel::YUV420P10LE);
        assert!(f.planes_u16().is_none());
        // NV12 keeps U and V in one plane, in chroma rows of 64 bytes.
        let mut f = grey_frame();
        f.frame.set_format(Pixel::NV12);
        assert!(f.planes_u16().is_none());
    }
}
