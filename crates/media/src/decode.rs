//! Sequential video decoding, software or on a hardware device.
//!
//! This is the first slice of the M1 `MediaBackend`: it opens a file,
//! decodes one video stream in order, and hands out frames in system memory.
//! Hardware decoding never silently falls back to software: if the device
//! can't decode the stream, opening or decoding fails with
//! [`MediaError::HwUnsupported`] and the reason, and if it fails while
//! decoding or copying a frame, with [`MediaError::HwFailed`], as does
//! every call after it, and the caller chooses what to do. Neither is ever
//! reported as damage in the file.
//!
//! A read error or a cancelled read ends the stream with an error, after
//! the frames that could still be decoded, never with `None`. So does a
//! file that ends before the samples its demuxer's index lists for the
//! stream, which is how an MP4 or MOV with its `moov` first looks when it
//! is cut before the end of the stream's data, and so does a Matroska
//! file that ends before its Segment says it does. The sample the cut
//! falls in is dropped, not handed to the decoder. A cut that neither
//! reaches goes unnoticed: the stream ends with `None` after the last
//! whole frame, and only FFmpeg's log mentions it. That is the case for
//! a Matroska file whose Segment doesn't say its size (a live
//! recording's), and for most cuts in a fragmented MP4, whose index only
//! grows as its fragments are read.
//!
//! Other streams in the file are not read, so damage in them doesn't end
//! this one. A packet the demuxer reports as invalid data is skipped,
//! since the demuxer can resync after one and later frames keep their
//! timestamps, but with a warning, and counted in
//! [`VideoDecoder::skipped_packets`]; after [`MAX_UNREADABLE_IN_A_ROW`] in
//! a row the file counts as unreadable.

use std::collections::BTreeSet;
use std::ffi::c_int;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::{Rational, StopToken, Time};

use crate::backend::DamageCause;
use crate::doctor::{VulkanDevice, VulkanDeviceType};
use crate::ffi::{self, DownloadPool, HwDevice, HwError, HwRefusal, OpenDecoderError};
use crate::hwprobe::{self, VkDevice, device_label, vk_device_type};
use crate::{MediaError, TooLarge, ff, init};

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
    /// fit its buffers. That covers most frames whose size or format was
    /// changed after their buffers were allocated, but not all: `ffi::plane`
    /// says which.
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

/// Frames a decoder can hold ready to hand out: more than a picture
/// buffer (16 in H.264 and HEVC) and FFmpeg's most frame threads (16)
/// together. A bound for [`VideoDecoder::drop_held_frames`].
const MAX_HELD_FRAMES: usize = 64;

pub struct VideoDecoder {
    path: PathBuf,
    ictx: ff::format::context::Input,
    stream_index: usize,
    time_base: Rational,
    dec: ff::decoder::Video,
    hw: Option<Arc<HwDevice>>,
    /// How `dec` was set up, to set it up again ([`reopen`](Self::reopen)).
    options: Options,
    /// Packets have been sent to `dec` since it was opened.
    sent: bool,
    /// Whether FFmpeg flagged the first frame decoded since opening a
    /// keyframe; `None` before one comes out.
    first_key: Option<bool>,
    /// `dec` is to be opened afresh before the next packet is sent: it is
    /// the stream's first, after packets were sent ([`reopen`](Self::reopen)).
    reopening: bool,
    /// Times `dec` has been opened afresh so.
    reopens: u64,
    /// A packet read ahead, after one flagged corrupt (see `next_packet`).
    lookahead: Option<ff::Packet>,
    /// The demuxer has nothing more to give.
    read_done: bool,
    eof_sent: bool,
    done: bool,
    /// Why reading stopped before a clean end of file; reported once the
    /// decoder has given up the frames it holds.
    read_error: Option<String>,
    /// What kind of failure `read_error`, then `failed`, is.
    read_cause: Option<DamageCause>,
    /// The error that ended the stream, returned by every later call.
    failed: Option<String>,
    /// Why the device turned the stream down, returned by every later
    /// call.
    refused: Option<HwRefusal>,
    /// Why decoding or downloading on the device failed, returned by every
    /// later call: the device's failure, never the file's damage.
    hw_failed: Option<String>,
    frames: u64,
    skipped: u64,
    /// Since opening or the last seek, no packet of the stream has been
    /// read.
    awaiting_landing: bool,
    /// The first packet of the stream read since opening or the last
    /// seek: its dts, or its pts when it has no dts (Matroska), which is
    /// what the demuxer's index and `av_seek_frame` compare.
    landing: Option<i64>,
    /// The pts of that packet, the keyframe decoding starts from, whether
    /// or not the demuxer flags it one: FFmpeg's H.264 parser flags an
    /// I-frame that isn't IDR only by its recovery point SEI, though the
    /// index (an MP4's `stss`) lists it and the seek lands on it.
    landing_key: Option<i64>,
    /// The greatest of the same timestamps of the packets of the stream
    /// read since opening or the last seek. An MP4's dts increase in
    /// decode order, but a Matroska stream's pts go back at its B-frames,
    /// and in a stream that ends on a keyframe and its leading B-frames,
    /// the last packet read is shown before the keyframe.
    read_position: Option<i64>,
    /// Once a packet has been read, the landing since opening: the
    /// stream's first packet, by the timestamp the index compares.
    first_landing: Option<Option<i64>>,
    /// The keyframes the index lists, by its timestamp, seen not to start
    /// an exact decode (bar the stream's first packet): those whose
    /// packets the demuxer didn't flag keyframes, of those read so far
    /// (for H.264, I-frames that aren't IDR and have no recovery point
    /// SEI), and those the caller found so ([`no_start_at`](Self::no_start_at)).
    no_starts: BTreeSet<i64>,
    /// The least that a keyframe's pts has been after its dts, over the
    /// keyframes read (and the packets landed on, which are the index's
    /// keyframes): how far the index's decode times lie before the times
    /// keyframes are shown.
    key_lag: Option<i64>,
    /// Frames nothing refers to are skipped until a packet with a pts at
    /// or after this one is sent.
    skip_before: Option<i64>,
    /// Packets sent while frames nothing refers to were being skipped:
    /// the decoder may have dropped any of them without a frame.
    skip_sent: u64,
    /// What device frames are downloaded into.
    pool: DownloadPool,
}

/// How [`VideoDecoder::with_options`] sets up the decoder.
#[derive(Debug, Clone, Copy, Default)]
struct Options {
    /// Software decoding threads; `None` lets FFmpeg pick.
    threads: Option<NonZeroU32>,
    /// Hand out frames flagged corrupt (`AV_CODEC_FLAG_OUTPUT_CORRUPT`).
    output_corrupt: bool,
    /// Surfaces a hardware decoder keeps beyond its own needs.
    extra_hw_frames: Option<u8>,
}

/// What [`VideoDecoder::receive`] got.
pub(crate) enum Received {
    Frame(ff::frame::Video),
    End,
    /// The stop token said to stop, at a packet or frame boundary.
    Stopped,
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

fn open_err(path: &Path, e: ffi::OpenError) -> MediaError {
    MediaError::opening(path, e, |e| e.to_string())
}

fn hw_unsupported(path: &Path, dev: &HwDevice, refusal: HwRefusal) -> MediaError {
    MediaError::HwUnsupported {
        path: path.to_path_buf(),
        kind: device_label(dev.kind()).into(),
        refusal,
    }
}

fn hw_failed(path: &Path, dev: &HwDevice, reason: &str) -> MediaError {
    MediaError::HwFailed {
        path: path.to_path_buf(),
        kind: device_label(dev.kind()).into(),
        reason: reason.into(),
    }
}

/// A decoder for a stream of `path` with parameters `par`, set up as
/// `options` say, on `hw` when given.
fn open_codec(
    path: &Path,
    par: ff::codec::Parameters,
    hw: Option<&HwDevice>,
    options: Options,
) -> Result<ff::decoder::Video, MediaError> {
    let fields = ffi::codecpar_fields(&par);
    let mut ctx = ff::codec::context::Context::from_parameters(par)
        .map_err(|e| err(path, "decoder parameters", e))?;
    if options.output_corrupt {
        ctx.set_flags(ff::codec::Flags::OUTPUT_CORRUPT);
    }
    match hw {
        // One thread, libavcodec's default (D3).
        Some(dev) => {
            ffi::attach_hw_device(&mut ctx, dev).map_err(|e| err(path, "hardware device", e))?;
            if let Some(n) = options.extra_hw_frames {
                ffi::set_extra_hw_frames(&mut ctx, n);
            }
        }
        // libavcodec defaults to one thread; let it pick per core
        // unless told how many.
        None => ctx.set_threading(ff::codec::threading::Config {
            kind: ff::codec::threading::Type::Frame,
            count: options.threads.map_or(0, |n| n.get() as usize),
        }),
    }
    ffi::open_video_decoder(ctx, hw).map_err(|e| match (e, hw) {
        (OpenDecoderError::NotOnDevice, Some(dev)) => {
            hw_unsupported(path, dev, HwRefusal::NotOffered)
        }
        (OpenDecoderError::Ffmpeg(ff::Error::DecoderNotFound), _) => MediaError::NoDecoder {
            path: path.to_path_buf(),
            codec: match fields.codec_name.as_deref() {
                // FFmpeg's names for a codec it doesn't know at all:
                // the sample entry's fourcc says more.
                Some(name) if name != "none" && name != "unknown_codec" => name.to_string(),
                _ => crate::probe::fourcc_str(fields.codec_tag)
                    .map_or_else(|| "this codec's".into(), |tag| format!("\"{tag}\"")),
            },
        },
        (e, _) => err(path, "opening decoder", e),
    })
}

impl VideoDecoder {
    /// Open the best video stream of `path` (or stream `index`). A stream
    /// whose pictures are larger than [`crate::MAX_PIXELS`] is refused with
    /// [`MediaError::PictureTooLarge`].
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

    /// Open stream `index` (or the best video stream) of `path` to decode
    /// in software on `threads` threads, or as many as FFmpeg picks
    /// (D3). Reading stops once `interrupt` is set, as with
    /// [`open_cancellable`](Self::open_cancellable).
    ///
    /// The decoder hands out frames it flags corrupt
    /// (`AV_CODEC_FLAG_OUTPUT_CORRUPT`), such as the leading pictures of
    /// an open GOP decoded after a seek without their references, for the
    /// caller to drop. Dropping them itself, FFmpeg 6.1's H.264 decoder
    /// also loses the frames still in its reorder buffer at the end of the
    /// stream: after a seek to the last keyframe of an open-GOP clip, its
    /// last frame never comes out. It doesn't flag the frames it gives up
    /// there either, so a leading picture decoded without its references
    /// that comes out as it drains is unflagged: the caller drops the
    /// leading pictures of the keyframe a seek landed on by their pts
    /// ([`landing_key`](Self::landing_key)).
    pub(crate) fn open_software(
        path: &Path,
        index: Option<usize>,
        threads: Option<NonZeroU32>,
        interrupt: Arc<AtomicBool>,
    ) -> Result<Self, MediaError> {
        init();
        let ictx = ffi::open_input(path, Some(interrupt)).map_err(|e| open_err(path, e))?;
        let options = Options {
            threads,
            output_corrupt: true,
            extra_hw_frames: None,
        };
        Self::with_options(path, ictx, index, None, options)
    }

    /// Like [`open_software`](Self::open_software), but decoding on `dev`,
    /// in one thread (D3), with `extra_hw_frames` surfaces in its pool
    /// beyond what the decoder needs, for the frames the caller holds
    /// before they are downloaded.
    pub(crate) fn open_hardware(
        path: &Path,
        index: Option<usize>,
        dev: Arc<HwDevice>,
        extra_hw_frames: u8,
        interrupt: Arc<AtomicBool>,
    ) -> Result<Self, MediaError> {
        init();
        let ictx = ffi::open_input(path, Some(interrupt)).map_err(|e| open_err(path, e))?;
        let options = Options {
            threads: None,
            output_corrupt: true,
            extra_hw_frames: Some(extra_hw_frames),
        };
        Self::with_options(path, ictx, index, Some(dev), options)
    }

    fn with_input(
        path: &Path,
        ictx: ff::format::context::Input,
        index: Option<usize>,
        hw: Option<Arc<HwDevice>>,
    ) -> Result<Self, MediaError> {
        Self::with_options(path, ictx, index, hw, Options::default())
    }

    fn with_options(
        path: &Path,
        mut ictx: ff::format::context::Input,
        index: Option<usize>,
        hw: Option<Arc<HwDevice>>,
        options: Options,
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
        // Said here, by size, where the decoder would fail to open with
        // "Invalid argument" or fail on every frame.
        let par = ffi::codecpar_fields(&st.parameters());
        let dim = |n: i32| u32::try_from(n).unwrap_or(0);
        if let Some(size) = TooLarge::of(dim(par.width), dim(par.height)) {
            return Err(MediaError::PictureTooLarge {
                path: path.to_path_buf(),
                stream: stream_index,
                size,
            });
        }
        let dec = open_codec(path, st.parameters(), hw.as_deref(), options)?;
        ffi::discard_other_streams(&mut ictx, stream_index);
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
            options,
            sent: false,
            first_key: None,
            reopening: false,
            reopens: 0,
            lookahead: None,
            read_done: false,
            eof_sent: false,
            done: false,
            read_error: None,
            read_cause: None,
            failed: None,
            refused: None,
            hw_failed: None,
            frames: 0,
            skipped: 0,
            awaiting_landing: true,
            landing: None,
            landing_key: None,
            read_position: None,
            first_landing: None,
            no_starts: BTreeSet::new(),
            key_lag: None,
            skip_before: None,
            skip_sent: 0,
            pool: DownloadPool::default(),
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
    /// device turns the stream down, and [`MediaError::HwFailed`], once
    /// decoding or copying a frame on it fails: nothing more is decoded.
    /// In software, any other error from the decoder itself is returned
    /// once; calling again carries on with the next packet.
    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        match self.receive(None)? {
            Received::Frame(frame) => self.finish_frame(frame).map(Some),
            Received::End | Received::Stopped => Ok(None),
        }
    }

    /// The next frame as the decoder made it, as [`next_frame`]
    /// describes, but without a download from the device; or the end; or
    /// [`Received::Stopped`] once `stop` says to, checked before each
    /// packet is read and before each frame is taken. Stopping loses
    /// nothing: the next call goes on from there.
    ///
    /// [`next_frame`]: Self::next_frame
    pub(crate) fn receive(&mut self, stop: Option<&StopToken>) -> Result<Received, MediaError> {
        if let Some(e) = self.device_error() {
            return Err(e);
        }
        if let Some(reason) = &self.failed {
            return Err(err(&self.path, "reading", reason));
        }
        let stopped = || stop.is_some_and(StopToken::is_stopped);
        let mut frame = ff::frame::Video::empty();
        loop {
            if self.done {
                return Ok(Received::End);
            }
            if stopped() {
                return Ok(Received::Stopped);
            }
            match self.dec.receive_frame(&mut frame) {
                Ok(()) => {
                    self.frames += 1;
                    self.first_key
                        .get_or_insert_with(|| ffi::frame_fields(&frame).key);
                    self.check_on_device(&frame)?;
                    return Ok(Received::Frame(frame));
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
                        None => Ok(Received::End),
                    };
                }
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
                Err(e) => return Err(self.decode_error(e)),
            }
            if self.eof_sent {
                // Drained; receive_frame will report Eof next.
                continue;
            }
            if stopped() {
                return Ok(Received::Stopped);
            }
            match self.next_packet() {
                Some(pkt) => {
                    if let Some(until) = self.skip_before
                        && pkt.pts().is_none_or(|p| p >= until)
                    {
                        self.skip_nonref_before(None);
                    }
                    if self.skip_before.is_some() {
                        self.skip_sent += 1;
                    }
                    if std::mem::take(&mut self.reopening) {
                        self.reopen()?;
                    }
                    self.sent = true;
                    self.dec
                        .send_packet(&pkt)
                        .map_err(|e| self.decode_error(e))?
                }
                None => {
                    if let Err(e) = self.dec.send_eof() {
                        return Err(match &self.hw {
                            Some(_) => self.fail_device(&format!("flushing the decoder: {e}")),
                            None => err(&self.path, "flushing decoder", e),
                        });
                    }
                    self.eof_sent = true;
                }
            }
        }
    }

    /// Whether reading has ended the stream with an error that every
    /// later call returns, until a seek: a read that failed or was
    /// cancelled, or a cut. A device that turned the stream down or failed
    /// on it doesn't count: that is never the file's damage, and its error
    /// says so ([`MediaError::is_device_failure`]).
    pub(crate) fn has_failed(&self) -> bool {
        self.failed.is_some()
    }

    /// The error reading stopped with since the last seek, as soon as it
    /// has: frames that come out after it are the decoder giving up the
    /// ones it holds, and the frames the damage lost may come before
    /// them in presentation order. `None` while reading goes on, and
    /// after a clean end.
    pub(crate) fn read_failure(&self) -> Option<MediaError> {
        let reason = self.read_error.as_ref().or(self.failed.as_ref())?;
        Some(err(&self.path, "reading", reason))
    }

    /// What kind of failure [`read_failure`](Self::read_failure)'s is.
    pub(crate) fn read_cause(&self) -> Option<DamageCause> {
        self.read_cause.filter(|_| self.reading_failed())
    }

    /// Whether reading has stopped with an error (see
    /// [`read_failure`](Self::read_failure)).
    pub(crate) fn reading_failed(&self) -> bool {
        self.read_error.is_some() || self.failed.is_some()
    }

    /// Seek the demuxer to the keyframe at or before `ts` in the stream's
    /// time base (`AVSEEK_FLAG_BACKWARD`), and flush the decoder; one that
    /// lands on the stream's first packet may open it afresh instead
    /// ([`reopen`](Self::reopen)), so that decoding from there gives what
    /// decoding from opening did. Reading failures and the end of the
    /// stream are cleared, so a seek back into the good part of a
    /// truncated file decodes again (D29, updating D13); a device's
    /// refusal is not.
    pub(crate) fn seek(&mut self, ts: i64) -> Result<(), MediaError> {
        ffi::seek_frame(&mut self.ictx, self.stream_index, ts, true)
            .map_err(|e| err(&self.path, "seeking", e))?;
        self.after_seek();
        Ok(())
    }

    /// The pts of the stream's first `n` packets, read before anything is
    /// decoded, and whether the stream ended within them; then back to
    /// the start. The seek back also has a Matroska demuxer read its
    /// index (Cues) where it comes after the clusters, as FFmpeg writes
    /// it: until a first seek, the index lists only the keyframes read.
    pub(crate) fn scan_timestamps(&mut self, n: usize) -> Result<(Vec<i64>, bool), MediaError> {
        let mut pts = Vec::with_capacity(n);
        let mut ended = false;
        for _ in 0..n {
            match self.demux() {
                Some(pkt) => pts.extend(pkt.pts()),
                None => {
                    ended = true;
                    break;
                }
            }
        }
        self.seek_to_start()?;
        Ok((pts, ended))
    }

    /// Seek back to the start of the stream: to the first entry of the
    /// demuxer's index, or the stream's start, or byte 0.
    pub(crate) fn seek_to_start(&mut self) -> Result<(), MediaError> {
        let start = ffi::index_first(&self.ictx, self.stream_index).or_else(|| {
            let st = self.ictx.stream(self.stream_index)?;
            let t = st.start_time();
            (t != ff::ffi::AV_NOPTS_VALUE).then_some(t)
        });
        let by_time = start.map(|ts| ffi::seek_frame(&mut self.ictx, self.stream_index, ts, true));
        match by_time {
            Some(Ok(())) => {}
            _ => ffi::seek_to_byte_zero(&mut self.ictx)
                .map_err(|e| err(&self.path, "seeking to the start", e))?,
        }
        self.after_seek();
        Ok(())
    }

    fn after_seek(&mut self) {
        self.drop_held_frames();
        self.dec.flush();
        self.lookahead = None;
        self.read_done = false;
        self.eof_sent = false;
        self.done = false;
        self.read_error = None;
        self.read_cause = None;
        self.failed = None;
        self.awaiting_landing = true;
        self.landing = None;
        self.landing_key = None;
        self.reopening = false;
        self.read_position = None;
    }

    /// Replace the decoder with one opened afresh, as
    /// [`with_options`](Self::with_options) opened it, before the stream's
    /// first packet is sent again. A flush doesn't leave FFmpeg's H.264
    /// decoder as opening does: its frame pool keeps the pictures it
    /// decoded, and where the stream starts at an I-frame that isn't IDR
    /// (a cut there), the stand-ins it makes for the frames before it
    /// are those pictures rather than the zeroed buffers of a fresh pool,
    /// and so are the frames that refer to them. So the stream's start
    /// decodes afresh, unless its first frame was a keyframe (an IDR
    /// picture, or a recovery point with a count of 0), after which the
    /// decoder needs nothing from before.
    fn reopen(&mut self) -> Result<(), MediaError> {
        let par = self
            .ictx
            .stream(self.stream_index)
            .map(|s| s.parameters())
            .ok_or_else(|| err(&self.path, "reopening the decoder", "the stream is gone"))?;
        self.dec = open_codec(&self.path, par, self.hw.as_deref(), self.options)?;
        ffi::skip_nonref(&mut self.dec, self.skip_before.is_some());
        self.sent = false;
        self.reopens += 1;
        tracing::debug!(
            path = %self.path.display(),
            "decoding the stream's start afresh: its first frame is no keyframe"
        );
        Ok(())
    }

    /// Takes, and drops, the frames the decoder holds ready to hand out,
    /// so that the flush after a seek leaves none behind. FFmpeg 7.1 and
    /// later's HEVC decoder queues the frames it outputs (all it has left
    /// once the stream has ended, several at once where a keyframe empties
    /// its picture buffer) and its flush leaves the queue as it is: on one
    /// thread, as hardware decoders run, a frame from before a seek came
    /// out after it (D2, D29). With no packet sent, a decoder only hands
    /// out what it has made, or what its frame threads are finishing, then
    /// says it needs input or has ended. These frames don't count as
    /// decoded (`frames`): a flush would have dropped them.
    fn drop_held_frames(&mut self) {
        let mut frame = ff::frame::Video::empty();
        for _ in 0..MAX_HELD_FRAMES {
            if self.dec.receive_frame(&mut frame).is_err() {
                return;
            }
        }
    }

    /// The first packet read since opening or the last seek, by the
    /// timestamp the index compares: the stream's first, or where the
    /// seek landed. `None` before one is read, or when it had no
    /// timestamp.
    pub(crate) fn landing(&self) -> Option<i64> {
        self.landing
    }

    /// The pts of the keyframe the last seek landed on, or decoding began
    /// at since opening: the first packet read since, flagged a keyframe
    /// or not (see the field).
    pub(crate) fn landing_key(&self) -> Option<i64> {
        self.landing_key
    }

    /// Whether the last seek landed on the stream's first packet, as
    /// opening does: decoding from there is decoding from the start.
    pub(crate) fn landed_at_start(&self) -> bool {
        self.landing.is_some() && Some(self.landing) == self.first_landing
    }

    /// Whether `ts` is a keyframe the index lists, by its timestamp, seen
    /// not to start an exact decode (see the field).
    pub(crate) fn no_start(&self, ts: i64) -> bool {
        self.no_starts.contains(&ts)
    }

    /// Note that the keyframe at `ts`, by the index's timestamp, doesn't
    /// start an exact decode: a seek landed on it and the frames decoded
    /// from it may not be those a decode from the start gives.
    pub(crate) fn no_start_at(&mut self, ts: i64) {
        self.no_starts.insert(ts);
    }

    /// How far the packets read reach, by the same timestamp (see the
    /// field); `None` when none has been read since opening or seeking.
    pub(crate) fn read_position(&self) -> Option<i64> {
        self.read_position
    }

    /// How far a keyframe's pts is after its dts, at least, over those
    /// read so far; 0 before one is read, and for Matroska, whose packets
    /// mostly have no dts and whose index is by pts.
    pub(crate) fn key_lag(&self) -> i64 {
        self.key_lag.unwrap_or(0).max(0)
    }

    /// `Some(pts)`: skip frames that nothing refers to
    /// (`AVDISCARD_NONREF`) until a packet with a pts at or after `pts`
    /// is sent. Packets go in in decode order, ahead of the frames coming
    /// out, so stopping at the first frame out at or after `pts` would
    /// already have skipped B-frames after it. `None`: skip nothing.
    pub(crate) fn skip_nonref_before(&mut self, pts: Option<i64>) {
        self.skip_before = pts;
        ffi::skip_nonref(&mut self.dec, pts.is_some());
    }

    /// How many packets have been sent while skipping frames nothing
    /// refers to, since opening: when it grows, a frame may be missing
    /// from what comes out, with nothing to say so.
    pub(crate) fn skipping_sent(&self) -> u64 {
        self.skip_sent
    }

    pub(crate) fn input(&self) -> &ff::format::context::Input {
        &self.ictx
    }

    pub(crate) fn stream_index(&self) -> usize {
        self.stream_index
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Frames the decoder has made since opening.
    pub(crate) fn frames_decoded(&self) -> u64 {
        self.frames
    }

    /// Times the decoder has been opened afresh to decode the stream's
    /// start again ([`reopen`](Self::reopen)).
    pub(crate) fn reopens(&self) -> u64 {
        self.reopens
    }

    /// FFmpeg's name for the decoder's codec, such as `h264`.
    pub(crate) fn codec_name(&self) -> String {
        ffi::codecpar_fields(
            &self
                .ictx
                .stream(self.stream_index)
                .map(|s| s.parameters())
                .unwrap_or_default(),
        )
        .codec_name
        .unwrap_or_default()
    }

    /// A decoder error, or why the device can't decode this stream when
    /// that is the cause: the decoder's own error then is just "Operation
    /// not permitted". On a device, any other decoder error is the
    /// device's failure (`HwFailed`), not damage: a hardware decoder's
    /// errors can't be told apart from damage it met, and software, after
    /// a fallback, decodes past damage and says so (D30).
    fn decode_error(&mut self, e: ff::Error) -> MediaError {
        self.refused = ffi::hw_refusal(&self.dec);
        if let Some(e) = self.refusal() {
            return e;
        }
        match &self.hw {
            Some(_) => self.fail_device(&format!("decoding: {e}")),
            None => err(&self.path, "decoding", e),
        }
    }

    /// `HwUnsupported`, once the device has turned the stream down.
    fn refusal(&self) -> Option<MediaError> {
        Some(hw_unsupported(&self.path, self.hw.as_ref()?, self.refused?))
    }

    /// The device's refusal or failure, once there is one: every later
    /// call returns it, and a seek doesn't clear it.
    fn device_error(&self) -> Option<MediaError> {
        self.refusal().or_else(|| {
            let reason = self.hw_failed.as_deref()?;
            Some(hw_failed(&self.path, self.hw.as_ref()?, reason))
        })
    }

    /// Record that the device failed, as `reason` says (decoding, or
    /// copying a frame from it), and return the `HwFailed` that this and
    /// every later call returns. Without a device, a decode error.
    pub(crate) fn fail_device(&mut self, reason: &str) -> MediaError {
        if self.hw.is_none() {
            return err(&self.path, "decoding", reason);
        }
        if self.hw_failed.is_none() {
            tracing::warn!(path = %self.path.display(), "hardware decoding failed: {reason}");
            self.hw_failed = Some(reason.into());
        }
        self.device_error()
            .unwrap_or_else(|| err(&self.path, "decoding", reason))
    }

    /// A frame decoded with a device must have been decoded on it (D3). A
    /// decoder that never asked for a format doesn't use the device, and
    /// one that turned the device down may go on in software: either way
    /// the stream is refused. A software frame after the device's format
    /// was taken is the device failing.
    fn check_on_device(&mut self, frame: &ff::frame::Video) -> Result<(), MediaError> {
        let Some(dev) = &self.hw else {
            return Ok(());
        };
        let format = ffi::frame_format(frame);
        if format == dev.hw_pix_fmt() {
            ffi::note_device_frame(&mut self.dec);
            return Ok(());
        }
        self.refused = match ffi::hw_refusal(&self.dec) {
            None if !ffi::hw_format_asked(&self.dec) => Some(HwRefusal::NotOffered),
            r => r,
        };
        Err(match self.refusal() {
            Some(e) => e,
            None => self.fail_device(&format!(
                "the decoder produced {} frames instead of device frames",
                ffi::pix_fmt_name(format).unwrap_or_else(|| format!("format {format}"))
            )),
        })
    }

    /// Whether `frame` is on this decoder's device, not yet copied to
    /// system memory.
    pub(crate) fn on_device(&self, frame: &ff::frame::Video) -> bool {
        self.hw
            .as_ref()
            .is_some_and(|h| ffi::frame_format(frame) == h.hw_pix_fmt())
    }

    /// Copy a frame from the device into the decoder's [`DownloadPool`]:
    /// rows 256 bytes apart, and buffers that go back to the pool when
    /// the frame drops (D30). Failing is the device failing.
    pub(crate) fn download(
        &mut self,
        frame: &ff::frame::Video,
    ) -> Result<ff::frame::Video, MediaError> {
        ffi::download_into_pool(&mut self.pool, frame)
            .map_err(|e| self.fail_device(&format!("copying a frame from it: {e}")))
    }

    /// Buffers its [`DownloadPool`] has allocated so far (0 in software).
    pub(crate) fn download_buffers(&self) -> u64 {
        self.pool.allocated()
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
                Ok(()) if pkt.stream() == self.stream_index => {
                    let ts = pkt.dts().or(pkt.pts());
                    let landed = std::mem::take(&mut self.awaiting_landing);
                    if landed {
                        self.landing = ts;
                        self.landing_key = pkt.pts();
                        let first = *self.first_landing.get_or_insert(ts);
                        self.reopening = first == ts && self.sent && self.first_key != Some(true);
                    }
                    if !pkt.is_key()
                        && let Some(ts) = ts
                        && self.first_landing != Some(Some(ts))
                        && ffi::index_keyframe_before(&self.ictx, self.stream_index, ts) == Some(ts)
                    {
                        self.no_starts.insert(ts);
                    }
                    self.read_position = ts.max(self.read_position);
                    // The landing is the index's keyframe, flagged one or
                    // not (see `landing_key`).
                    if (pkt.is_key() || landed)
                        && let (Some(pts), Some(dts)) = (pkt.pts(), pkt.dts())
                    {
                        let lag = pts.saturating_sub(dts);
                        self.key_lag = Some(self.key_lag.map_or(lag, |l| l.min(lag)));
                    }
                    return Some(pkt);
                }
                Ok(()) => unreadable = 0,
                // FFmpeg reports a read error that made the demuxer stop
                // as that error, not as the end of the file.
                Err(ff::Error::Eof) => {
                    self.read_error = self.truncation();
                    self.read_cause = self.read_error.as_ref().map(|_| DamageCause::Cut);
                    return None;
                }
                Err(ff::Error::InvalidData) => {
                    self.skipped += 1;
                    unreadable += 1;
                    if unreadable == MAX_UNREADABLE_IN_A_ROW {
                        self.read_error = Some(format!("{unreadable} unreadable packets in a row"));
                        self.read_cause = Some(DamageCause::Corrupt);
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
                    self.read_cause = Some(DamageCause::Cancelled);
                    return None;
                }
                Err(e) => {
                    self.read_error = Some(e.to_string());
                    self.read_cause = Some(DamageCause::Read);
                    return None;
                }
            }
        }
    }

    /// At the end of the file: whether it was cut short. It ends before
    /// the data the demuxer's index lists for our stream (the other
    /// streams don't count, since the demuxer skips them), or, for a
    /// Matroska file, before the end its Segment says: its demuxer reads
    /// a cut as the end of the file, and its index (Cues, written last)
    /// is lost with the cut (D13).
    fn truncation(&self) -> Option<String> {
        let size = std::fs::metadata(&self.path).ok()?.len();
        if let Some(end) = ffi::index_end(&self.ictx, self.stream_index)
            && end > size
        {
            return Some(format!(
                "the file is truncated: it ends at byte {size}, but its index \
                 lists video data up to byte {end}"
            ));
        }
        if !self.ictx.format().name().starts_with("matroska") {
            return None;
        }
        let (start, segment) = crate::probe::matroska_segment_of(&self.path)?;
        (start.saturating_add(segment) > size).then(|| {
            format!(
                "the file is truncated: it holds {} of the {segment} bytes its \
                 Matroska Segment says it has",
                size.saturating_sub(start)
            )
        })
    }

    /// Where to seek in a file cut short of the data its index lists, as
    /// [`truncation`](Self::truncation) finds: the index timestamp (as
    /// [`landing`](Self::landing) has it) of the last keyframe whose data
    /// the file holds whole, before `before` when given. `None` when the
    /// file isn't cut so, or holds no such keyframe.
    pub(crate) fn keyframe_inside(&self, before: Option<i64>) -> Option<i64> {
        let size = std::fs::metadata(&self.path).ok()?.len();
        let end = ffi::index_end(&self.ictx, self.stream_index)?;
        (end > size)
            .then(|| ffi::index_keyframe_inside(&self.ictx, self.stream_index, size, before))
            .flatten()
    }

    /// A frame from [`receive`](Self::receive), which has checked that a
    /// frame decoded with a device is on it, copied to system memory.
    fn finish_frame(&mut self, frame: ff::frame::Video) -> Result<DecodedFrame, MediaError> {
        let on_device = self.on_device(&frame);
        let pts = frame.pts();
        let frame = if on_device {
            self.download(&frame)?
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

/// Open a hardware device for decoding. `kind` is "vaapi" or "vulkan";
/// an error names it "VA-API" or "Vulkan".
pub fn open_hw_device(kind: &str, device: Option<&str>) -> Result<Arc<HwDevice>, MediaError> {
    init();
    HwDevice::open(kind, device)
        .map(Arc::new)
        .map_err(|reason| MediaError::Hardware {
            kind: hwprobe::device_label(kind).into(),
            reason,
        })
}

/// Why [`open_vulkan_device`] chose the device it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VulkanPick {
    /// It is the GPU behind this DRM node, VA-API's render node.
    Node(PathBuf),
    /// It is the first device listed that isn't a CPU. None of them is
    /// listed as the one behind the DRM node, if one was given.
    FirstGpu { unmatched: Option<PathBuf> },
    /// Every device listed is a CPU device, so it is FFmpeg's default.
    NoGpu,
}

/// FFmpeg's Vulkan device as [`open_vulkan_device`] opened it.
#[derive(Debug)]
pub struct VulkanChoice {
    pub device: Arc<HwDevice>,
    /// The physical device it runs on.
    pub used: VulkanDevice,
    pub why: VulkanPick,
    /// FFmpeg's default device, when that was another one.
    pub instead_of: Option<VulkanDevice>,
    /// Every physical device FFmpeg's Vulkan instance lists, in its order.
    pub devices: Vec<VulkanDevice>,
}

impl std::fmt::Display for VulkanChoice {
    /// e.g. "AMD Radeon RX 7600 (RADV NAVI33) (discrete GPU, driver
    /// radv), the GPU behind /dev/dri/renderD128".
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", describe(&self.used))?;
        match &self.why {
            VulkanPick::Node(node) => write!(f, ", the GPU behind {}", node.display())?,
            VulkanPick::FirstGpu { unmatched: None } => write!(f, ", the first GPU listed")?,
            VulkanPick::FirstGpu {
                unmatched: Some(node),
            } => write!(
                f,
                ", the first GPU listed (none is listed as the one behind {})",
                node.display()
            )?,
            VulkanPick::NoGpu => write!(
                f,
                ", FFmpeg's default: every Vulkan device listed runs on the CPU"
            )?,
        }
        if let Some(d) = &self.instead_of {
            write!(f, "; FFmpeg's default was {}", describe(d))?;
        }
        Ok(())
    }
}

/// A device's name, kind and driver.
fn describe(d: &VulkanDevice) -> String {
    let kind = match d.device_type {
        VulkanDeviceType::DiscreteGpu => "discrete GPU",
        VulkanDeviceType::IntegratedGpu => "integrated GPU",
        VulkanDeviceType::VirtualGpu => "virtual GPU",
        VulkanDeviceType::Cpu => "CPU",
        VulkanDeviceType::Other => "other device",
    };
    match &d.driver {
        Some(driver) => format!("{} ({kind}, driver {driver})", d.name),
        None => format!("{} ({kind})", d.name),
    }
}

/// Open FFmpeg's Vulkan device on a GPU, and on the same GPU as VA-API,
/// rather than where FFmpeg's default lands (DECISIONS D38).
///
/// Without a device string, FFmpeg (6.1 to 8.1 at least) opens the first
/// physical device its Vulkan instance lists, and the Vulkan loader and
/// Mesa's device-select layer decide that order. The loader lists discrete
/// GPUs, then integrated ones, then CPU devices; the layer then moves the
/// GPU the desktop runs on, or else the boot VGA device, to the front. On
/// a machine with two GPUs that need not be the one behind VA-API's render
/// node, and with no GPU driver working it is llvmpipe. So this lists the
/// devices and opens the one behind `render_node`, else the first that
/// isn't a CPU, else FFmpeg's default, which then runs on the CPU
/// (`used.is_cpu()`).
///
/// The error is [`MediaError::Hardware`]: with FFmpeg's error when it
/// can't open a Vulkan device at all, or with [`HwError::Vulkan`] when its
/// devices can't be listed, or it can't open the one chosen, or it opened
/// another one.
pub fn open_vulkan_device(render_node: Option<&Path>) -> Result<VulkanChoice, MediaError> {
    let fail = |reason: String| MediaError::Hardware {
        kind: hwprobe::device_label("vulkan").into(),
        reason: HwError::Vulkan(reason),
    };
    let list = |dev: &HwDevice| {
        hwprobe::vulkan_devices(dev).map_err(|e| fail(format!("listing its devices: {e}")))
    };
    let default = open_hw_device("vulkan", None)?;
    let devices = list(&default)?;
    let Some(default_at) = devices.iter().position(|d| d.ffmpeg_uses) else {
        return Err(fail(
            "FFmpeg's device is not among those its instance lists".into(),
        ));
    };
    let (at, pick) = choose(&devices, render_node.and_then(device_numbers), default_at);
    let why = match (pick, render_node) {
        (Pick::Node, Some(node)) => VulkanPick::Node(node.to_path_buf()),
        (Pick::Default, _) => VulkanPick::NoGpu,
        (_, node) => VulkanPick::FirstGpu {
            unmatched: node.map(Path::to_path_buf),
        },
    };
    let (device, used) = if at == default_at {
        (default, devices[at].clone())
    } else {
        drop(default);
        // FFmpeg reads a number as an index into the same list. Its
        // default opened, so failing to open this one is an error too.
        let dev = open_hw_device("vulkan", Some(&at.to_string()))
            .map_err(|e| fail(format!("opening device {at}, {}: {e}", devices[at].name)))?;
        let now = list(&dev)?;
        let opened = now.iter().position(|d| d.ffmpeg_uses);
        let Some(used) = opened
            .filter(|&i| i == at)
            .and_then(|i| now.get(i))
            .filter(|d| d.same_device(&devices[at]))
        else {
            return Err(fail(format!(
                "asked FFmpeg for device {at}, {}, and it opened {}",
                devices[at].name,
                opened
                    .and_then(|i| now.get(i))
                    .map_or("none of those listed", |d| d.name.as_str())
            )));
        };
        (dev, used.clone())
    };
    let instead_of = (at != default_at).then(|| VulkanDevice {
        used_by_ffmpeg: false,
        ..VulkanDevice::from(devices[default_at].clone())
    });
    let choice = VulkanChoice {
        device,
        used: used.into(),
        why,
        instead_of,
        devices: devices
            .into_iter()
            .enumerate()
            .map(|(i, d)| VulkanDevice {
                used_by_ffmpeg: i == at,
                ..VulkanDevice::from(d)
            })
            .collect(),
    };
    tracing::debug!("FFmpeg's Vulkan device: {choice}");
    Ok(choice)
}

/// How [`choose`] chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Node,
    FirstGpu,
    Default,
}

/// Which of `devices` to open: the one with the DRM node numbered `node`,
/// else the first that isn't a CPU, else FFmpeg's default at `default`.
fn choose(devices: &[VkDevice], node: Option<(i64, i64)>, default: usize) -> (usize, Pick) {
    if let Some(n) = node
        && let Some(i) = devices.iter().position(|d| d.drm_nodes.contains(&n))
    {
        return (i, Pick::Node);
    }
    match devices
        .iter()
        .position(|d| d.device_type != vk_device_type::CPU)
    {
        Some(i) => (i, Pick::FirstGpu),
        None => (default, Pick::Default),
    }
}

/// The `(major, minor)` numbers of the character device at `path`.
fn device_numbers(path: &Path) -> Option<(i64, i64)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let m = std::fs::metadata(path).ok()?;
    m.file_type()
        .is_char_device()
        .then(|| major_minor(m.rdev()))
}

/// Linux's `dev_t` split as glibc's `major()` and `minor()` do.
fn major_minor(dev: u64) -> (i64, i64) {
    let major = ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0xfff);
    let minor = ((dev >> 12) & 0xffff_ff00) | (dev & 0xff);
    (major as i64, minor as i64)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use spherecut_core::FrameRate;

    use super::*;
    use crate::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};

    #[test]
    fn a_device_that_does_not_open_is_named_as_the_other_errors_name_it() {
        let e = open_hw_device("vaapi", Some("/nonexistent/renderD128")).unwrap_err();
        assert!(
            matches!(&e, MediaError::Hardware { kind, .. } if kind == "VA-API"),
            "{e:?}"
        );
        assert!(
            e.to_string().starts_with("VA-API device unavailable: "),
            "{e}"
        );
        let e = open_hw_device("cuda-ish", None).unwrap_err();
        assert!(
            matches!(&e, MediaError::Hardware { kind, .. } if kind == "cuda-ish"),
            "{e:?}"
        );
    }

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
            color: ColorTags::BT709_LIMITED,
            edit_start: 0,
            time_base: None,
            crop_left: 0,
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
    }

    #[test]
    fn a_device_failure_is_sticky_and_never_reading_damage() {
        // As when the driver fails a frame: every later call says so, a
        // seek doesn't clear it, and it never reads as a failed read.
        let dir = TempDir::new("device-failed");
        let mut d = VideoDecoder::open(&clip(&dir), None, None).unwrap();
        d.hw = Some(Arc::new(HwDevice::vulkan_without_context()));
        let e = d.fail_device("decoding: Input/output error");
        assert!(e.is_device_failure(), "{e:?}");
        assert_eq!(e.code(), "hw_failed");
        assert_eq!(
            e.to_string(),
            format!(
                "decoding {} on Vulkan failed: decoding: Input/output error",
                clip(&dir).display()
            )
        );
        for seek in [false, true] {
            if seek {
                d.seek(0).unwrap();
            }
            match d.next_frame() {
                Err(e @ MediaError::HwFailed { .. }) => assert!(e.is_device_failure()),
                r => panic!("expected HwFailed, got {r:?}"),
            }
            assert!(!d.has_failed());
        }
    }

    #[test]
    fn test_directories_are_removed_when_dropped() {
        // The clip is made in the test's directory, and goes with it.
        let dir = TempDir::new("dropped");
        let clip = clip(&dir);
        drop(dir);
        assert!(!clip.exists());
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

    fn vk(name: &str, device_type: i32, drm_nodes: &[(i64, i64)]) -> VkDevice {
        VkDevice {
            name: name.into(),
            device_type,
            vendor_id: 0x1002,
            device_id: 0,
            api_version: 0,
            driver_name: None,
            driver_info: None,
            drm_nodes: drm_nodes.to_vec(),
            ffmpeg_uses: false,
        }
    }

    #[test]
    fn vulkan_devices_are_chosen_by_drm_node_then_gpu_then_default() {
        use vk_device_type::{CPU, DISCRETE_GPU, INTEGRATED_GPU};
        // As on a desktop with an iGPU driving the display: Mesa's
        // device-select layer lists it first, and FFmpeg takes it.
        let igpu = vk("iGPU", INTEGRATED_GPU, &[(226, 1), (226, 129)]);
        let dgpu = vk("dGPU", DISCRETE_GPU, &[(226, 0), (226, 128)]);
        let cpu = vk("llvmpipe", CPU, &[]);
        let three = [igpu.clone(), dgpu.clone(), cpu.clone()];
        assert_eq!(choose(&three, Some((226, 128)), 0), (1, Pick::Node));
        assert_eq!(choose(&three, Some((226, 129)), 0), (0, Pick::Node));
        // A primary node works too, as in FFmpeg's own DRM match.
        assert_eq!(choose(&three, Some((226, 0)), 0), (1, Pick::Node));
        // A node no device is listed with, or none: the first GPU.
        assert_eq!(choose(&three, Some((226, 130)), 0), (0, Pick::FirstGpu));
        assert_eq!(choose(&three, None, 0), (0, Pick::FirstGpu));
        // llvmpipe listed first, as an old loader without the layer could.
        let cpu_first = [cpu.clone(), dgpu.clone()];
        assert_eq!(choose(&cpu_first, None, 0), (1, Pick::FirstGpu));
        // A GPU without VK_EXT_physical_device_drm.
        let no_drm = [cpu.clone(), vk("GPU", DISCRETE_GPU, &[])];
        assert_eq!(choose(&no_drm, Some((226, 128)), 0), (1, Pick::FirstGpu));
        // Only CPU devices: FFmpeg's default stays.
        let cpus = [cpu.clone(), vk("other llvmpipe", CPU, &[])];
        assert_eq!(choose(&cpus, Some((226, 128)), 1), (1, Pick::Default));
    }

    #[test]
    fn ffmpeg_opens_the_vulkan_device_at_the_index_it_is_given() {
        // What open_vulkan_device relies on to open the device it chose.
        // Tier 1 needs FFmpeg's Vulkan device, on lavapipe at least.
        let default = open_hw_device("vulkan", None).unwrap();
        let listed = hwprobe::vulkan_devices(&default).unwrap();
        drop(default);
        for (i, want) in listed.iter().enumerate() {
            let dev = open_hw_device("vulkan", Some(&i.to_string())).unwrap();
            let now = hwprobe::vulkan_devices(&dev).unwrap();
            assert_eq!(now.iter().position(|d| d.ffmpeg_uses), Some(i));
            assert!(now[i].same_device(want), "{:?} is not {want:?}", now[i]);
        }
    }

    /// glibc's `makedev()`.
    fn makedev(major: u64, minor: u64) -> u64 {
        ((major & 0xffff_f000) << 32)
            | ((major & 0xfff) << 8)
            | ((minor & 0xffff_ff00) << 12)
            | (minor & 0xff)
    }

    #[test]
    fn device_numbers_split_as_glibc_does() {
        // /dev/dri/renderD128.
        assert_eq!(makedev(226, 128), 57_984);
        assert_eq!(major_minor(57_984), (226, 128));
        assert_eq!(
            major_minor(makedev(0x1234_5678, 0x9abc_def0)),
            (0x1234_5678, 0x9abc_def0)
        );
        // /dev/null is 1:3 on Linux; a regular file has no device numbers.
        assert_eq!(device_numbers(Path::new("/dev/null")), Some((1, 3)));
        assert_eq!(
            device_numbers(Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/Cargo.toml"
            ))),
            None
        );
        assert_eq!(device_numbers(Path::new("/nonexistent/renderD128")), None);
    }

    fn device(name: &str, device_type: VulkanDeviceType, driver: &str) -> VulkanDevice {
        VulkanDevice {
            name: name.into(),
            device_type,
            vendor_id: "0x1002".into(),
            device_id: "0x7480".into(),
            api_version: "1.4.318".into(),
            driver: Some(driver.into()),
            driver_info: None,
            used_by_ffmpeg: true,
        }
    }

    #[test]
    fn a_vulkan_choice_says_which_device_and_why() {
        let dgpu = device(
            "AMD Radeon RX 7600 (RADV NAVI33)",
            VulkanDeviceType::DiscreteGpu,
            "radv",
        );
        let igpu = device(
            "AMD Radeon Graphics (RADV RAPHAEL_MENDOCINO)",
            VulkanDeviceType::IntegratedGpu,
            "radv",
        );
        let choice = |why, instead_of| VulkanChoice {
            device: Arc::new(HwDevice::vulkan_without_context()),
            used: dgpu.clone(),
            why,
            instead_of,
            devices: Vec::new(),
        };
        let node = PathBuf::from("/dev/dri/renderD128");
        assert_eq!(
            choice(VulkanPick::Node(node.clone()), Some(igpu)).to_string(),
            "AMD Radeon RX 7600 (RADV NAVI33) (discrete GPU, driver radv), the GPU behind \
             /dev/dri/renderD128; FFmpeg's default was AMD Radeon Graphics (RADV \
             RAPHAEL_MENDOCINO) (integrated GPU, driver radv)"
        );
        assert_eq!(
            choice(
                VulkanPick::FirstGpu {
                    unmatched: Some(node)
                },
                None
            )
            .to_string(),
            "AMD Radeon RX 7600 (RADV NAVI33) (discrete GPU, driver radv), the first GPU \
             listed (none is listed as the one behind /dev/dri/renderD128)"
        );
        let c = VulkanChoice {
            used: device(
                "llvmpipe (LLVM 20.1.2, 256 bits)",
                VulkanDeviceType::Cpu,
                "llvmpipe",
            ),
            ..choice(VulkanPick::NoGpu, None)
        };
        assert_eq!(
            c.to_string(),
            "llvmpipe (LLVM 20.1.2, 256 bits) (CPU, driver llvmpipe), FFmpeg's default: \
             every Vulkan device listed runs on the CPU"
        );
    }
}
