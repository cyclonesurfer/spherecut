//! Sequential video decoding, software or on a hardware device.
//!
//! This is the first slice of the M1 `MediaBackend`: it opens a file,
//! decodes one video stream in order, and hands out frames in system memory.
//! Hardware decoding never silently falls back to software: if the device
//! can't decode the stream, opening or the first frame fails with the
//! reason, and the caller chooses what to do.

use std::ffi::c_int;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use spherecut_core::{Rational, Time};

use crate::ffi::{self, HwDevice};
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
            .field("format", &self.frame.format())
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
    /// formats other than 4:2:0 8/10-bit.
    pub fn planes_u16(&self) -> Option<[Vec<u16>; 3]> {
        use ff::ffi::AVPixelFormat as P;
        let f = &self.frame;
        let is = |p: P| ffi::frame_format(f) == p as c_int;
        let (w, h) = (f.width() as usize, f.height() as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let row = |plane: usize, y: usize| -> &[u8] { &f.data(plane)[y * f.stride(plane)..] };
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

pub struct VideoDecoder {
    path: PathBuf,
    ictx: ff::format::context::Input,
    stream_index: usize,
    time_base: Rational,
    dec: ff::decoder::Video,
    hw: Option<Arc<HwDevice>>,
    eof_sent: bool,
    done: bool,
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

impl VideoDecoder {
    /// Open the best video stream of `path` (or stream `index`).
    pub fn open(
        path: &Path,
        index: Option<usize>,
        hw: Option<Arc<HwDevice>>,
    ) -> Result<Self, MediaError> {
        init();
        let ictx = ff::format::input(path).map_err(|e| MediaError::Open {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
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
        let dec = ctx
            .decoder()
            .video()
            .map_err(|e| err(path, "opening decoder", e))?;
        Ok(VideoDecoder {
            path: path.to_path_buf(),
            ictx,
            stream_index,
            time_base,
            dec,
            hw,
            eof_sent: false,
            done: false,
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

    /// The next frame in decode-output (presentation) order, or `None` at
    /// the end of the stream.
    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>, MediaError> {
        let mut frame = ff::frame::Video::empty();
        loop {
            if self.done {
                return Ok(None);
            }
            match self.dec.receive_frame(&mut frame) {
                Ok(()) => return self.finish_frame(frame).map(Some),
                Err(ff::Error::Eof) => {
                    self.done = true;
                    return Ok(None);
                }
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
                Err(e) => return Err(err(&self.path, "decoding", e)),
            }
            if self.eof_sent {
                // Drained; receive_frame will report Eof next.
                continue;
            }
            // Feed the next packet of our stream.
            let mut fed = false;
            for (s, pkt) in self.ictx.packets() {
                if s.index() == self.stream_index {
                    self.dec
                        .send_packet(&pkt)
                        .map_err(|e| err(&self.path, "decoding", e))?;
                    fed = true;
                    break;
                }
            }
            if !fed {
                self.dec
                    .send_eof()
                    .map_err(|e| err(&self.path, "flushing decoder", e))?;
                self.eof_sent = true;
            }
        }
    }

    fn finish_frame(&self, frame: ff::frame::Video) -> Result<DecodedFrame, MediaError> {
        let format = ffi::frame_format(&frame);
        let on_device = self.hw.as_ref().is_some_and(|h| format == h.hw_pix_fmt());
        if self.hw.is_some() && !on_device {
            return Err(err(
                &self.path,
                "hardware decode",
                format!(
                    "decoder produced {} frames instead of device frames",
                    ffi::pix_fmt_name(format).unwrap_or_else(|| format!("format {format}"))
                ),
            ));
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
        .map_err(|source| MediaError::Hardware {
            kind: kind.to_string(),
            source,
        })
}
