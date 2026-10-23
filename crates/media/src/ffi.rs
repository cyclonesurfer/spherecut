//! The only module in this crate that touches FFmpeg's C API directly.
//!
//! ffmpeg-next covers demuxing and coding; what it doesn't expose safely
//! (color and channel-layout fields, display matrices, hardware device
//! creation, codec hardware configs) is read here and handed out as plain
//! Rust values. Every pointer read is from an object whose lifetime is tied
//! to a borrowed ffmpeg-next wrapper, and every allocation made here is
//! released before returning.

use std::ffi::{CStr, CString, c_char};

use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;

/// Convert a C string returned by an FFmpeg `*_name` function.
fn cstr_opt(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        // SAFETY: FFmpeg's name functions return pointers to static,
        // NUL-terminated strings (or NULL, handled above).
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// Codec-parameter fields that ffmpeg-next doesn't wrap.
#[derive(Debug, Clone, Default)]
pub struct CodecParFields {
    pub codec_name: Option<String>,
    pub codec_tag: u32,
    pub profile: Option<String>,
    pub level: Option<i32>,
    pub width: i32,
    pub height: i32,
    pub sample_aspect: (i32, i32),
    pub pix_fmt: Option<String>,
    /// Bits per component of the pixel format (max over components).
    pub bit_depth: Option<u32>,
    pub chroma_subsampling: Option<(u32, u32)>,
    pub color_range: Option<String>,
    pub color_matrix: Option<String>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub chroma_location: Option<String>,
    pub field_order: Option<String>,
    pub sample_rate: i32,
    pub channels: i32,
    pub channel_layout: Option<String>,
    pub sample_fmt: Option<String>,
    /// Display rotation in degrees (counter-clockwise), from the display
    /// matrix side data, if present.
    pub rotation_deg: Option<f64>,
}

pub fn codecpar_fields(p: &ff::codec::Parameters) -> CodecParFields {
    // SAFETY: `p` borrows a live AVCodecParameters owned by its stream or
    // by the Parameters value; we only read from it within this function.
    let par = unsafe { &*p.as_ptr() };
    let mut f = CodecParFields {
        // SAFETY: avcodec_get_name accepts any codec id.
        codec_name: cstr_opt(unsafe { sys::avcodec_get_name(par.codec_id) }),
        codec_tag: par.codec_tag,
        // SAFETY: avcodec_profile_name returns NULL for unknown profiles.
        profile: cstr_opt(unsafe { sys::avcodec_profile_name(par.codec_id, par.profile) }),
        level: (par.level >= 0).then_some(par.level),
        width: par.width,
        height: par.height,
        sample_aspect: (par.sample_aspect_ratio.num, par.sample_aspect_ratio.den),
        sample_rate: par.sample_rate,
        channels: par.ch_layout.nb_channels,
        ..Default::default()
    };
    match par.codec_type {
        sys::AVMediaType::AVMEDIA_TYPE_VIDEO => {
            if par.format >= 0 {
                // SAFETY: `format` holds an AVPixelFormat for video streams;
                // values unknown to this libavutil yield NULL descriptors.
                let fmt: sys::AVPixelFormat = unsafe { std::mem::transmute(par.format) };
                // SAFETY: as above.
                f.pix_fmt = cstr_opt(unsafe { sys::av_get_pix_fmt_name(fmt) });
                // SAFETY: returns NULL for unknown formats, checked below.
                let desc = unsafe { sys::av_pix_fmt_desc_get(fmt) };
                if !desc.is_null() {
                    // SAFETY: non-NULL descriptors point at static tables.
                    let d = unsafe { &*desc };
                    let n = d.nb_components as usize;
                    f.bit_depth = d.comp[..n.min(4)].iter().map(|c| c.depth as u32).max();
                    f.chroma_subsampling = Some((d.log2_chroma_w as u32, d.log2_chroma_h as u32));
                }
            }
            // SAFETY: name functions accept any enum value.
            unsafe {
                f.color_range = cstr_opt(sys::av_color_range_name(par.color_range));
                f.color_matrix = cstr_opt(sys::av_color_space_name(par.color_space));
                f.color_primaries = cstr_opt(sys::av_color_primaries_name(par.color_primaries));
                f.color_transfer = cstr_opt(sys::av_color_transfer_name(par.color_trc));
                f.chroma_location = cstr_opt(sys::av_chroma_location_name(par.chroma_location));
            }
            f.field_order = Some(
                match par.field_order {
                    sys::AVFieldOrder::AV_FIELD_PROGRESSIVE => "progressive",
                    sys::AVFieldOrder::AV_FIELD_TT => "tt",
                    sys::AVFieldOrder::AV_FIELD_BB => "bb",
                    sys::AVFieldOrder::AV_FIELD_TB => "tb",
                    sys::AVFieldOrder::AV_FIELD_BT => "bt",
                    _ => "unknown",
                }
                .to_string(),
            );
            f.rotation_deg = display_rotation(par);
        }
        sys::AVMediaType::AVMEDIA_TYPE_AUDIO => {
            if par.format >= 0 {
                // SAFETY: `format` holds an AVSampleFormat for audio streams.
                let fmt: sys::AVSampleFormat = unsafe { std::mem::transmute(par.format) };
                // SAFETY: returns NULL for unknown formats.
                f.sample_fmt = cstr_opt(unsafe { sys::av_get_sample_fmt_name(fmt) });
            }
            f.channel_layout = describe_layout(&par.ch_layout);
        }
        _ => {}
    }
    f
}

fn describe_layout(l: &sys::AVChannelLayout) -> Option<String> {
    let mut buf = [0 as c_char; 256];
    // SAFETY: `l` is a valid layout borrowed from live codec parameters and
    // `buf` is a writable buffer of the size we pass.
    let n = unsafe { sys::av_channel_layout_describe(l, buf.as_mut_ptr(), buf.len()) };
    if n <= 0 {
        return None;
    }
    // SAFETY: on success FFmpeg NUL-terminates within `buf`.
    Some(
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn display_rotation(par: &sys::AVCodecParameters) -> Option<f64> {
    // SAFETY: coded_side_data/nb_coded_side_data describe an array owned by
    // `par`; av_packet_side_data_get only reads it.
    let sd = unsafe {
        sys::av_packet_side_data_get(
            par.coded_side_data,
            par.nb_coded_side_data,
            sys::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        )
    };
    if sd.is_null() {
        return None;
    }
    // SAFETY: non-NULL entry from the array above.
    let sd = unsafe { &*sd };
    if sd.size < 36 || sd.data.is_null() {
        return None;
    }
    // SAFETY: a display matrix is nine int32 values, size checked above.
    let r = unsafe { sys::av_display_rotation_get(sd.data as *const i32) };
    r.is_finite().then_some(r)
}

/// Hardware device types compiled into this FFmpeg.
pub fn hw_device_types() -> Vec<String> {
    let mut out = Vec::new();
    let mut t = sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE;
    loop {
        // SAFETY: iterating from NONE is the documented way to list types.
        t = unsafe { sys::av_hwdevice_iterate_types(t) };
        if t == sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
            break;
        }
        // SAFETY: accepts any type value.
        if let Some(n) = cstr_opt(unsafe { sys::av_hwdevice_get_type_name(t) }) {
            out.push(n);
        }
    }
    out
}

/// Try to create a hardware device context, then release it. Returns the
/// FFmpeg error text on failure.
pub fn try_hw_device(kind: &str, device: Option<&str>) -> Result<(), String> {
    let kind_c = CString::new(kind).map_err(|_| "bad device type".to_string())?;
    // SAFETY: valid NUL-terminated string.
    let t = unsafe { sys::av_hwdevice_find_type_by_name(kind_c.as_ptr()) };
    if t == sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
        return Err(format!("this FFmpeg was built without {kind} support"));
    }
    let dev_c = device
        .map(CString::new)
        .transpose()
        .map_err(|_| "bad device path".to_string())?;
    let mut ctx: *mut sys::AVBufferRef = std::ptr::null_mut();
    // SAFETY: `ctx` is an out-pointer; device may be NULL (default device).
    let ret = unsafe {
        sys::av_hwdevice_ctx_create(
            &mut ctx,
            t,
            dev_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            std::ptr::null_mut(),
            0,
        )
    };
    if !ctx.is_null() {
        // SAFETY: we own the reference returned above.
        unsafe { sys::av_buffer_unref(&mut ctx) };
    }
    if ret < 0 {
        Err(ff::Error::from(ret).to_string())
    } else {
        Ok(())
    }
}

/// Hardware device types a decoder can use through `hw_device_ctx`.
pub fn decoder_hw_configs(codec: &ff::Codec) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0.. {
        // SAFETY: `codec` wraps a static AVCodec; the call returns NULL past
        // the last config.
        let cfg = unsafe { sys::avcodec_get_hw_config(codec.as_ptr(), i) };
        if cfg.is_null() {
            break;
        }
        // SAFETY: non-NULL configs point at static tables.
        let cfg = unsafe { &*cfg };
        if cfg.methods & sys::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0 {
            // SAFETY: accepts any type value.
            if let Some(n) = cstr_opt(unsafe { sys::av_hwdevice_get_type_name(cfg.device_type) }) {
                out.push(n);
            }
        }
    }
    out
}

/// Versions of the FFmpeg libraries we are linked against, as
/// `(name, major, minor, micro)`.
pub fn lib_versions() -> Vec<(&'static str, u32, u32, u32)> {
    let split = |v: u32| (v >> 16, (v >> 8) & 0xff, v & 0xff);
    // SAFETY: the version functions take no arguments and have no
    // preconditions.
    let (c, f, u, s, r) = unsafe {
        (
            sys::avcodec_version(),
            sys::avformat_version(),
            sys::avutil_version(),
            sys::swscale_version(),
            sys::swresample_version(),
        )
    };
    [
        ("libavcodec", c),
        ("libavformat", f),
        ("libavutil", u),
        ("libswscale", s),
        ("libswresample", r),
    ]
    .into_iter()
    .map(|(n, v)| {
        let (a, b, m) = split(v);
        (n, a, b, m)
    })
    .collect()
}

/// FFmpeg's own version string, e.g. "6.1.1-3ubuntu5" or "n7.1".
pub fn ffmpeg_version() -> String {
    // SAFETY: returns a static string.
    cstr_opt(unsafe { sys::av_version_info() }).unwrap_or_else(|| "unknown".into())
}

/// `AVFormatContext.start_time` in `AV_TIME_BASE` units.
pub fn format_start_time(ictx: &ff::format::context::Input) -> i64 {
    // SAFETY: `ictx` owns a live AVFormatContext; reading a plain field.
    unsafe { (*ictx.as_ptr()).start_time }
}

/// A reference to an FFmpeg hardware device context. Dropping it releases
/// the reference; FFmpeg's buffer refcounts are atomic, so it can move
/// between threads.
#[derive(Debug)]
pub struct HwDevice {
    ptr: *mut sys::AVBufferRef,
    pix_fmt: sys::AVPixelFormat,
    kind: String,
}

// SAFETY: AVBufferRef reference counting is thread-safe, and the device
// context it points to is only mutated by FFmpeg under its own locking.
unsafe impl Send for HwDevice {}
// SAFETY: as above; &HwDevice only allows taking new references.
unsafe impl Sync for HwDevice {}

impl Drop for HwDevice {
    fn drop(&mut self) {
        // SAFETY: we own exactly one reference.
        unsafe { sys::av_buffer_unref(&mut self.ptr) };
    }
}

impl HwDevice {
    /// Open a device: `kind` is an FFmpeg device type name ("vaapi",
    /// "vulkan"); `device` is e.g. a DRM render node, or `None` for the
    /// default.
    pub fn open(kind: &str, device: Option<&str>) -> Result<Self, String> {
        let kind_c = CString::new(kind).map_err(|_| "bad device type".to_string())?;
        // SAFETY: valid NUL-terminated string.
        let t = unsafe { sys::av_hwdevice_find_type_by_name(kind_c.as_ptr()) };
        if t == sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
            return Err(format!("this FFmpeg was built without {kind} support"));
        }
        let pix_fmt = match t {
            sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => sys::AVPixelFormat::AV_PIX_FMT_VAAPI,
            sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN => sys::AVPixelFormat::AV_PIX_FMT_VULKAN,
            _ => return Err(format!("{kind} decoding is not supported by Spherecut")),
        };
        let dev_c = device
            .map(CString::new)
            .transpose()
            .map_err(|_| "bad device path".to_string())?;
        let mut ptr: *mut sys::AVBufferRef = std::ptr::null_mut();
        // SAFETY: `ptr` is an out-pointer; device may be NULL.
        let ret = unsafe {
            sys::av_hwdevice_ctx_create(
                &mut ptr,
                t,
                dev_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                std::ptr::null_mut(),
                0,
            )
        };
        if ret < 0 || ptr.is_null() {
            return Err(ff::Error::from(ret).to_string());
        }
        Ok(HwDevice {
            ptr,
            pix_fmt,
            kind: kind.to_string(),
        })
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The pixel format of frames decoded on this device.
    pub fn hw_pixel(&self) -> ff::format::Pixel {
        self.pix_fmt.into()
    }
}

/// `get_format` callback: pick the hardware format stored in `opaque`, or
/// fail. Failing (instead of FFmpeg's silent software fallback) lets the
/// caller decide and report what happened.
unsafe extern "C" fn pick_hw_format(
    ctx: *mut sys::AVCodecContext,
    fmts: *const sys::AVPixelFormat,
) -> sys::AVPixelFormat {
    // SAFETY: FFmpeg passes a live context whose `opaque` we set to the
    // wanted format, and a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        let want = (*ctx).opaque as isize as i32;
        let mut p = fmts;
        while *p != sys::AVPixelFormat::AV_PIX_FMT_NONE {
            if *p as i32 == want {
                return *p;
            }
            p = p.add(1);
        }
    }
    sys::AVPixelFormat::AV_PIX_FMT_NONE
}

/// Configure an unopened decoder context to decode on `dev`.
pub fn attach_hw_device(
    ctx: &mut ff::codec::context::Context,
    dev: &HwDevice,
) -> Result<(), String> {
    // SAFETY: the context is not yet opened, so setting hw_device_ctx and
    // get_format is allowed; we hand FFmpeg its own new reference.
    unsafe {
        let c = ctx.as_mut_ptr();
        let r = sys::av_buffer_ref(dev.ptr);
        if r.is_null() {
            return Err("out of memory".into());
        }
        if !(*c).hw_device_ctx.is_null() {
            sys::av_buffer_unref(&mut (*c).hw_device_ctx);
        }
        (*c).hw_device_ctx = r;
        (*c).opaque = dev.pix_fmt as i32 as isize as *mut std::ffi::c_void;
        (*c).get_format = Some(pick_hw_format);
    }
    Ok(())
}

/// Copy a hardware frame into system memory (NV12 / P010).
pub fn download_frame(src: &ff::frame::Video) -> Result<ff::frame::Video, ff::Error> {
    let mut dst = ff::frame::Video::empty();
    // SAFETY: both frames are valid; transfer_data allocates dst's buffers.
    let ret = unsafe { sys::av_hwframe_transfer_data(dst.as_mut_ptr(), src.as_ptr(), 0) };
    if ret < 0 {
        return Err(ff::Error::from(ret));
    }
    // SAFETY: copy timestamps and properties onto the new frame.
    let ret = unsafe { sys::av_frame_copy_props(dst.as_mut_ptr(), src.as_ptr()) };
    if ret < 0 {
        return Err(ff::Error::from(ret));
    }
    Ok(dst)
}
