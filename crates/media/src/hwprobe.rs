//! The FFmpeg and Vulkan calls behind `spherecut doctor`'s capability
//! checks.
//!
//! A device that opens can still be useless: Fedora's Mesa VA-API driver
//! opens with H.264 and HEVC turned off, and FFmpeg's Vulkan device opens
//! on llvmpipe, which renders on the CPU. So doctor decodes and encodes one
//! small frame on each device, never falling back to software. This module
//! makes those calls; `doctor` decides what the results mean.
//!
//! What FFmpeg logs while a check runs is often the only real explanation
//! ("No support for codec hevc profile 1."), so [`capture_ffmpeg_log`]
//! keeps it.
//!
//! As in `ffi`, C enums are read as `int`s, never as bindgen's Rust enums.

use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ffmpeg_sys_next as sys;
use thiserror::Error;
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Dispatch, Event, Level, Metadata, Subscriber, field, span};

use crate::decode::DecodedFrame;
use crate::ff;
use crate::ffi::{self, HwDevice, HwError};

/// Why a check failed.
#[derive(Debug, Error)]
pub(crate) enum ProbeError {
    #[error("FFmpeg has no {0} decoder")]
    NoDecoder(String),
    #[error("FFmpeg has no {0} encoder")]
    NoEncoder(String),
    /// The decoder never offered the device's format: this FFmpeg was
    /// built without that hwaccel, or it doesn't cover this profile.
    #[error("FFmpeg's {decoder} decoder has no {device} support for this format")]
    NoHwFormat { decoder: String, device: String },
    /// FFmpeg offered the device's format, and setting up the hardware
    /// decoder for it failed: the driver can't decode this profile.
    #[error("the {device} driver cannot decode this format")]
    DriverRefused { device: String },
    #[error("the decoder produced {0} frames instead of device frames")]
    NotOnDevice(String),
    #[error("{what}: {source}")]
    Ffmpeg {
        what: &'static str,
        source: ff::Error,
    },
    #[error(transparent)]
    Hw(#[from] HwError),
}

impl ProbeError {
    /// Whether the FFmpeg build is what's missing, rather than the driver
    /// or the hardware.
    pub(crate) fn is_ffmpeg_build(&self) -> bool {
        matches!(
            self,
            Self::NoDecoder(_)
                | Self::NoEncoder(_)
                | Self::NoHwFormat { .. }
                | Self::Hw(HwError::NotBuiltIn(_))
        )
    }
}

/// What to call an FFmpeg device type in messages.
pub(crate) fn device_label(kind: &str) -> &str {
    match kind {
        "vaapi" => "VA-API",
        "vulkan" => "Vulkan",
        k => k,
    }
}

fn ffmpeg(what: &'static str) -> impl FnOnce(ff::Error) -> ProbeError {
    move |source| ProbeError::Ffmpeg { what, source }
}

fn check(ret: c_int, what: &'static str) -> Result<(), ProbeError> {
    if ret < 0 {
        Err(ffmpeg(what)(ff::Error::from(ret)))
    } else {
        Ok(())
    }
}

fn no_memory(what: &'static str) -> ProbeError {
    ffmpeg(what)(ff::Error::Other {
        errno: ff::util::error::ENOMEM,
    })
}

const PIX_FMT_NONE: c_int = sys::AVPixelFormat::AV_PIX_FMT_NONE as c_int;

/// What [`choose_format`] saw while a decoder set itself up.
struct FormatChoice {
    /// The device's pixel format, the only one we accept.
    want: c_int,
    /// FFmpeg asked for a format at all.
    asked: AtomicBool,
    /// The device's format was among those offered.
    offered: AtomicBool,
    /// FFmpeg asked again without it after we took it, which it does when
    /// setting up the hardware decoder failed.
    withdrawn: AtomicBool,
}

/// `get_format` for the checks: take the device's format or nothing, and
/// note whether FFmpeg offered it and whether it took it back.
unsafe extern "C" fn choose_format(
    ctx: *mut sys::AVCodecContext,
    fmts: *const sys::AVPixelFormat,
) -> sys::AVPixelFormat {
    const { assert!(size_of::<sys::AVPixelFormat>() == size_of::<c_int>()) };
    // SAFETY: FFmpeg passes the context whose `opaque` decode_one pointed
    // at a FormatChoice that outlives the decoder, and a list ended by
    // AV_PIX_FMT_NONE. Entries are read as ints, since the list can hold
    // formats our headers lack; the one returned equals `want`, which is
    // a format they have.
    unsafe {
        let choice = &*(*ctx).opaque.cast::<FormatChoice>();
        choice.asked.store(true, Ordering::Relaxed);
        let mut p = fmts;
        loop {
            let v = p.cast::<c_int>().read();
            if v == PIX_FMT_NONE {
                break;
            }
            if v == choice.want {
                choice.offered.store(true, Ordering::Relaxed);
                return *p;
            }
            p = p.add(1);
        }
        if choice.offered.load(Ordering::Relaxed) {
            choice.withdrawn.store(true, Ordering::Relaxed);
        }
    }
    sys::AVPixelFormat::AV_PIX_FMT_NONE
}

/// Decode the single frame in `data` with FFmpeg's decoder `decoder`, on
/// `dev` if given, and return it in system memory.
///
/// `data` is one access unit (Annex B H.264/HEVC with its parameter sets,
/// or AV1 OBUs with the sequence header), sent as one packet, so no
/// demuxer or parser is involved. On a device the frame must come out as
/// device frames, as `VideoDecoder` requires.
pub(crate) fn decode_one(
    decoder: &str,
    data: &[u8],
    dev: Option<&HwDevice>,
) -> Result<DecodedFrame, ProbeError> {
    let codec =
        ff::decoder::find_by_name(decoder).ok_or_else(|| ProbeError::NoDecoder(decoder.into()))?;
    // Declared before the decoder, so that it outlives it.
    let choice = FormatChoice {
        want: dev.map_or(PIX_FMT_NONE, HwDevice::hw_pix_fmt),
        asked: AtomicBool::new(false),
        offered: AtomicBool::new(false),
        withdrawn: AtomicBool::new(false),
    };
    let mut ctx = ff::codec::context::Context::new_with_codec(codec);
    // One thread: FFmpeg then sets up the hardware decoder, and logs why
    // it can't, on this thread, where capture_ffmpeg_log listens.
    ctx.set_threading(ff::codec::threading::Config {
        kind: ff::codec::threading::Type::Frame,
        count: 1,
    });
    if let Some(dev) = dev {
        ffi::attach_hw_device(&mut ctx, dev)?;
        // SAFETY: the context is not opened yet, so get_format may still
        // change, and `choice` outlives the decoder (above).
        unsafe {
            let c = ctx.as_mut_ptr();
            (*c).opaque = (&raw const choice).cast_mut().cast();
            (*c).get_format = Some(choose_format);
        }
    }
    let mut dec = ctx
        .decoder()
        .open_as(codec)
        .and_then(|d| d.video())
        .map_err(ffmpeg("opening the decoder"))?;
    let mut frame = ff::frame::Video::empty();
    let decoded = dec
        .send_packet(&ff::Packet::copy(data))
        .and_then(|()| dec.send_eof())
        .and_then(|()| dec.receive_frame(&mut frame));
    if let Err(e) = decoded {
        let seen = |b: &AtomicBool| b.load(Ordering::Relaxed);
        return Err(match dev {
            Some(d) if seen(&choice.withdrawn) => ProbeError::DriverRefused {
                device: device_label(d.kind()).into(),
            },
            Some(d) if seen(&choice.asked) && !seen(&choice.offered) => ProbeError::NoHwFormat {
                decoder: decoder.into(),
                device: device_label(d.kind()).into(),
            },
            _ => ffmpeg("decoding")(e),
        });
    }
    let Some(dev) = dev else {
        return Ok(DecodedFrame {
            pts: frame.pts(),
            time: None,
            frame,
            hw_decoded: false,
        });
    };
    let format = ffi::frame_format(&frame);
    if format != dev.hw_pix_fmt() {
        return Err(ProbeError::NotOnDevice(
            ffi::pix_fmt_name(format).unwrap_or_else(|| format!("format {format}")),
        ));
    }
    let frame = ffi::download_frame(&frame).map_err(ffmpeg("copying the frame from the device"))?;
    Ok(DecodedFrame {
        pts: frame.pts(),
        time: None,
        frame,
        hw_decoded: true,
    })
}

/// Width and height of the frame [`encode_one`] encodes.
pub(crate) const ENCODE_SIZE: u32 = 256;

/// An `AVBufferRef` we own one reference to.
struct BufferRef(*mut sys::AVBufferRef);

impl Drop for BufferRef {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference.
        unsafe { sys::av_buffer_unref(&mut self.0) };
    }
}

/// A frames context on `dev` for `w`x`h` frames of `sw_format`.
fn hw_frames(
    dev: &HwDevice,
    sw_format: ff::format::Pixel,
    w: u32,
    h: u32,
) -> Result<BufferRef, ProbeError> {
    // SAFETY: the device reference is live; this returns a new reference
    // that we own, or NULL.
    let frames = BufferRef(unsafe { sys::av_hwframe_ctx_alloc(dev.as_ptr()) });
    if frames.0.is_null() {
        return Err(no_memory("setting up frames on the device"));
    }
    // SAFETY: a frames reference's data is its AVHWFramesContext, whose
    // fields are set before av_hwframe_ctx_init as FFmpeg requires, with
    // formats our headers have. Nothing is read.
    unsafe {
        let fc = (*frames.0).data.cast::<sys::AVHWFramesContext>();
        (*fc).format = dev.hw_pixel().into();
        (*fc).sw_format = sw_format.into();
        (*fc).width = w as c_int;
        (*fc).height = h as c_int;
    }
    // SAFETY: a frames context allocated and filled in above.
    let ret = unsafe { sys::av_hwframe_ctx_init(frames.0) };
    check(ret, "setting up frames on the device")?;
    Ok(frames)
}

/// A system-memory frame with a diagonal luma ramp and neutral chroma, in
/// NV12 or P010.
fn ramp(sw_format: ff::format::Pixel, w: u32, h: u32) -> ff::frame::Video {
    let mut f = ff::frame::Video::new(sw_format, w, h);
    let (w, h) = (w as usize, h as usize);
    let ten_bit = sw_format == ff::format::Pixel::P010LE;
    // Limited range: 16..235 at 8 bits, 64..940 at 10. P010 keeps the 10
    // bits at the top of each little-endian u16.
    let (lo, span, mid) = if ten_bit {
        (64, 876, 512)
    } else {
        (16, 219, 128)
    };
    let put = |row: &mut [u8], i: usize, v: usize| {
        if ten_bit {
            row[2 * i..2 * i + 2].copy_from_slice(&((v as u16) << 6).to_le_bytes());
        } else {
            row[i] = v as u8;
        }
    };
    let stride = f.stride(0);
    for (y, row) in f.data_mut(0).chunks_mut(stride).take(h).enumerate() {
        for x in 0..w {
            put(row, x, lo + (x + y) * span / (w + h));
        }
    }
    // Interleaved U and V: `w` samples per row of half height.
    let stride = f.stride(1);
    for row in f.data_mut(1).chunks_mut(stride).take(h.div_ceil(2)) {
        for i in 0..w {
            put(row, i, mid);
        }
    }
    f
}

/// Encode one [`ENCODE_SIZE`]² frame with the hardware encoder `encoder`
/// (e.g. "hevc_vaapi") on `dev`: 8-bit NV12, or 10-bit P010 when
/// `ten_bit`. Returns the encoded bytes; FFmpeg's VA-API encoders put the
/// parameter sets in-band, so they decode on their own.
pub(crate) fn encode_one(
    encoder: &str,
    dev: &HwDevice,
    ten_bit: bool,
) -> Result<Vec<u8>, ProbeError> {
    let codec =
        ff::encoder::find_by_name(encoder).ok_or_else(|| ProbeError::NoEncoder(encoder.into()))?;
    let (w, h) = (ENCODE_SIZE, ENCODE_SIZE);
    let sw_format = if ten_bit {
        ff::format::Pixel::P010LE
    } else {
        ff::format::Pixel::NV12
    };
    let frames = hw_frames(dev, sw_format, w, h)?;
    let src = ramp(sw_format, w, h);
    let mut surface = ff::frame::Video::empty();
    // SAFETY: `frames` is an initialised frames context, and get_buffer
    // gives `surface` a frame from it.
    let ret = unsafe { sys::av_hwframe_get_buffer(frames.0, surface.as_mut_ptr(), 0) };
    check(ret, "getting a frame on the device")?;
    // SAFETY: `src` is a system-memory frame of the frames context's
    // sw_format and size, and `surface` one of its device frames.
    let ret = unsafe { sys::av_hwframe_transfer_data(surface.as_mut_ptr(), src.as_ptr(), 0) };
    check(ret, "uploading the frame")?;
    surface.set_pts(Some(0));

    let mut enc = ff::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(ffmpeg("setting up the encoder"))?;
    enc.set_width(w);
    enc.set_height(h);
    enc.set_format(dev.hw_pixel());
    enc.set_time_base((1, 30));
    enc.set_frame_rate(Some((30, 1)));
    // SAFETY: takes a new reference to a live frames context.
    let frames_ref = unsafe { sys::av_buffer_ref(frames.0) };
    if frames_ref.is_null() {
        return Err(no_memory("setting up the encoder"));
    }
    // SAFETY: the encoder is not opened yet, so hw_frames_ctx may be set;
    // the context takes over our new reference.
    unsafe { (*enc.as_mut_ptr()).hw_frames_ctx = frames_ref };
    let mut enc = enc.open_as(codec).map_err(ffmpeg("opening the encoder"))?;
    enc.send_frame(&surface).map_err(ffmpeg("encoding"))?;
    enc.send_eof().map_err(ffmpeg("encoding"))?;
    let mut out = Vec::new();
    let mut pkt = ff::Packet::empty();
    loop {
        match enc.receive_packet(&mut pkt) {
            Ok(()) => out.extend_from_slice(pkt.data().unwrap_or_default()),
            Err(ff::Error::Eof) => break,
            Err(e) => return Err(ffmpeg("encoding")(e)),
        }
    }
    Ok(out)
}

/// Captures running now, in any thread. FFmpeg's log level goes back to
/// following the subscriber only when the last one ends; the lock also
/// keeps one capture's start and another's end from interleaving.
static CAPTURES: Mutex<usize> = Mutex::new(0);

/// Run `f`, and return the warnings and errors that FFmpeg logged on this
/// thread meanwhile, whether or not the `tracing` subscriber shows them.
///
/// A thread-local subscriber listens and passes everything on to the
/// current one. FFmpeg's log level is process-wide, so while any capture
/// runs it is at least "warning" (other threads' warnings then get
/// formatted and dropped), and afterwards it follows the subscriber again.
pub(crate) fn capture_ffmpeg_log<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let tee = Dispatch::new(Tee {
        outer: tracing::dispatcher::get_default(Dispatch::clone),
        lines: lines.clone(),
        max: LevelFilter::current().max(LevelFilter::WARN),
    });
    let r = tracing::dispatcher::with_default(&tee, || {
        {
            let mut n = CAPTURES.lock().unwrap_or_else(PoisonError::into_inner);
            *n += 1;
            // Sees the tee, so the level becomes at least "warning".
            ffi::sync_log_level();
        }
        f()
    });
    drop(tee);
    let mut n = CAPTURES.lock().unwrap_or_else(PoisonError::into_inner);
    *n -= 1;
    if *n == 0 {
        // Forget the tee's interest in callsites and its level hint, and
        // let FFmpeg follow the subscriber again.
        tracing_core::callsite::rebuild_interest_cache();
        ffi::sync_log_level();
    }
    drop(n);
    let lines = std::mem::take(&mut *lines.lock().unwrap_or_else(PoisonError::into_inner));
    (r, lines)
}

/// Records FFmpeg's warnings and errors, and hands everything to `outer`.
struct Tee {
    outer: Dispatch,
    lines: Arc<Mutex<Vec<String>>>,
    max: LevelFilter,
}

fn is_captured(m: &Metadata<'_>) -> bool {
    m.target() == "ffmpeg" && *m.level() <= Level::WARN
}

impl Subscriber for Tee {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.max)
    }
    fn enabled(&self, m: &Metadata<'_>) -> bool {
        is_captured(m) || self.outer.enabled(m)
    }
    fn new_span(&self, attrs: &span::Attributes<'_>) -> span::Id {
        self.outer.new_span(attrs)
    }
    fn record(&self, id: &span::Id, values: &span::Record<'_>) {
        self.outer.record(id, values)
    }
    fn record_follows_from(&self, id: &span::Id, follows: &span::Id) {
        self.outer.record_follows_from(id, follows)
    }
    fn event(&self, e: &Event<'_>) {
        let m = e.metadata();
        if is_captured(m) {
            let mut msg = Message(String::new());
            e.record(&mut msg);
            self.lines
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(without_address(&msg.0));
        }
        if self.outer.enabled(m) {
            self.outer.event(e);
        }
    }
    fn enter(&self, id: &span::Id) {
        self.outer.enter(id)
    }
    fn exit(&self, id: &span::Id) {
        self.outer.exit(id)
    }
    fn clone_span(&self, id: &span::Id) -> span::Id {
        self.outer.clone_span(id)
    }
    fn try_close(&self, id: span::Id) -> bool {
        self.outer.try_close(id)
    }
    fn current_span(&self) -> tracing_core::span::Current {
        self.outer.current_span()
    }
}

/// FFmpeg's "[hevc @ 0x55d0c0ffee00] msg" as "[hevc] msg": the address
/// means nothing in a report.
fn without_address(line: &str) -> String {
    if let Some(rest) = line.strip_prefix('[')
        && let Some((name, msg)) = rest.split_once("] ")
        && let Some((name, addr)) = name.split_once(" @ 0x")
        && addr.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return format!("[{name}] {msg}");
    }
    line.to_string()
}

struct Message(String);

impl field::Visit for Message {
    fn record_debug(&mut self, f: &field::Field, v: &dyn std::fmt::Debug) {
        if f.name() == "message" {
            self.0 = format!("{v:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines_lose_their_context_address() {
        assert_eq!(
            without_address("[hevc @ 0x55d0c0ffee00] Error parsing NAL unit #3."),
            "[hevc] Error parsing NAL unit #3."
        );
        assert_eq!(
            without_address("[AVHWDeviceContext @ 0x1] No VA display found."),
            "[AVHWDeviceContext] No VA display found."
        );
        for plain in ["no prefix", "[x] no address", "[x @ 0xzz] not hex"] {
            assert_eq!(without_address(plain), plain);
        }
    }

    #[test]
    fn ramps_are_limited_range_nv12_and_p010() {
        let f = ramp(ff::format::Pixel::NV12, 64, 32);
        let (y, uv) = (f.stride(0), f.stride(1));
        assert_eq!(f.data(0)[0], 16);
        assert_eq!(f.data(0)[31 * y + 63], (16 + 94 * 219 / 96) as u8);
        assert_eq!(f.data(1)[15 * uv..15 * uv + 64], [128; 64]);

        let f = ramp(ff::format::Pixel::P010LE, 64, 32);
        let (y, uv) = (f.stride(0) / 2, f.stride(1) / 2);
        // P010 keeps the 10 bits at the top of each little-endian u16.
        let at = |plane: usize, i: usize| {
            u16::from_le_bytes([f.data(plane)[2 * i], f.data(plane)[2 * i + 1]])
        };
        assert_eq!(at(0, 0), 64 << 6);
        assert_eq!(at(0, 31 * y + 63), ((64 + 94 * 876 / 96_usize) as u16) << 6);
        assert!((0..64).all(|i| at(1, 15 * uv + i) == 512 << 6));
    }
}
