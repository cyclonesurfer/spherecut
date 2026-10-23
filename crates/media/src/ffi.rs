//! The only module in this crate that touches FFmpeg's C API directly.
//!
//! ffmpeg-next covers demuxing and coding; what it doesn't expose safely
//! (color and channel-layout fields, display matrices, hardware device
//! creation, codec hardware configs) is read here and handed out as plain
//! Rust values. Every pointer read is from an object whose lifetime is tied
//! to a borrowed ffmpeg-next wrapper, and every allocation made here is
//! released before returning.
//!
//! C enums never become bindgen's Rust enums here. Those may only hold the
//! values in the headers ffmpeg-sys-next was generated from, and a newer
//! FFmpeg at run time can hand back others (a new pixel format, colour
//! transfer or device type); making a Rust enum from such a value is
//! undefined behaviour. So the functions in [`c`] are declared with the
//! `int` the C ABI passes, and enum fields are read with [`enum_int`].

use std::ffi::{CStr, CString, c_char, c_int};

use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;

/// FFmpeg functions that take or return a C enum, declared with `int`s.
/// The name functions return NULL for values they don't know.
mod c {
    use std::ffi::{c_char, c_int};

    use ffmpeg_sys_next::{AVBufferRef, AVDictionary, AVPixFmtDescriptor};

    unsafe extern "C" {
        pub fn avcodec_get_name(id: c_int) -> *const c_char;
        pub fn avcodec_profile_name(id: c_int, profile: c_int) -> *const c_char;
        pub fn av_get_pix_fmt_name(fmt: c_int) -> *const c_char;
        pub fn av_pix_fmt_desc_get(fmt: c_int) -> *const AVPixFmtDescriptor;
        pub fn av_get_sample_fmt_name(fmt: c_int) -> *const c_char;
        pub fn av_color_range_name(range: c_int) -> *const c_char;
        pub fn av_color_space_name(space: c_int) -> *const c_char;
        pub fn av_color_primaries_name(primaries: c_int) -> *const c_char;
        pub fn av_color_transfer_name(transfer: c_int) -> *const c_char;
        pub fn av_chroma_location_name(location: c_int) -> *const c_char;
        pub fn av_hwdevice_iterate_types(prev: c_int) -> c_int;
        pub fn av_hwdevice_find_type_by_name(name: *const c_char) -> c_int;
        pub fn av_hwdevice_get_type_name(kind: c_int) -> *const c_char;
        pub fn av_hwdevice_ctx_create(
            device_ctx: *mut *mut AVBufferRef,
            kind: c_int,
            device: *const c_char,
            opts: *mut AVDictionary,
            flags: c_int,
        ) -> c_int;
    }
}

/// Read a C enum field as the `int` it is, whatever value it holds.
///
/// # Safety
///
/// `field` must point at an initialised enum field of a live FFmpeg struct.
unsafe fn enum_int<E>(field: *const E) -> c_int {
    const { assert!(size_of::<E>() == size_of::<c_int>()) };
    // SAFETY: the caller's guarantee; C enums are `int`-sized (checked
    // above), and any bit pattern is a valid `c_int`.
    unsafe { field.cast::<c_int>().read() }
}

const HW_NONE: c_int = sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE as c_int;

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
    // by the Parameters value. Its fields are read through this pointer and
    // never through a reference, because some of them are enums.
    let par = unsafe { p.as_ptr() };
    // SAFETY: `par` is live (above); these are its enum fields.
    let (codec_id, codec_type, field_order) = unsafe {
        (
            enum_int(&raw const (*par).codec_id),
            enum_int(&raw const (*par).codec_type),
            enum_int(&raw const (*par).field_order),
        )
    };
    // SAFETY: as above; these are plain integer fields.
    let mut f = unsafe {
        CodecParFields {
            codec_tag: (*par).codec_tag,
            level: ((*par).level >= 0).then_some((*par).level),
            width: (*par).width,
            height: (*par).height,
            sample_aspect: (
                (*par).sample_aspect_ratio.num,
                (*par).sample_aspect_ratio.den,
            ),
            sample_rate: (*par).sample_rate,
            channels: (*par).ch_layout.nb_channels,
            ..Default::default()
        }
    };
    // SAFETY: `par` is live; the name functions accept any codec id and
    // return NULL for unknown ids and profiles.
    unsafe {
        f.codec_name = cstr_opt(c::avcodec_get_name(codec_id));
        f.profile = cstr_opt(c::avcodec_profile_name(codec_id, (*par).profile));
    }
    // SAFETY: `par` is live; a plain integer field.
    let format = unsafe { (*par).format };
    if codec_type == sys::AVMediaType::AVMEDIA_TYPE_VIDEO as c_int {
        if format >= 0 {
            // SAFETY: `format` holds a pixel format for video streams, and
            // both functions return NULL for values this libavutil doesn't
            // know.
            let (name, desc) = unsafe {
                (
                    c::av_get_pix_fmt_name(format),
                    c::av_pix_fmt_desc_get(format),
                )
            };
            f.pix_fmt = cstr_opt(name);
            if !desc.is_null() {
                // SAFETY: non-NULL descriptors point at static tables, and
                // AVPixFmtDescriptor has no enum fields.
                let d = unsafe { &*desc };
                let n = d.nb_components as usize;
                f.bit_depth = d.comp[..n.min(4)].iter().map(|c| c.depth as u32).max();
                f.chroma_subsampling = Some((d.log2_chroma_w as u32, d.log2_chroma_h as u32));
            }
        }
        // SAFETY: `par` is live and these are its enum fields; the name
        // functions accept any value.
        unsafe {
            f.color_range = cstr_opt(c::av_color_range_name(enum_int(
                &raw const (*par).color_range,
            )));
            f.color_matrix = cstr_opt(c::av_color_space_name(enum_int(
                &raw const (*par).color_space,
            )));
            f.color_primaries = cstr_opt(c::av_color_primaries_name(enum_int(
                &raw const (*par).color_primaries,
            )));
            f.color_transfer = cstr_opt(c::av_color_transfer_name(enum_int(
                &raw const (*par).color_trc,
            )));
            f.chroma_location = cstr_opt(c::av_chroma_location_name(enum_int(
                &raw const (*par).chroma_location,
            )));
        }
        use sys::AVFieldOrder as F;
        f.field_order = Some(
            match field_order {
                o if o == F::AV_FIELD_PROGRESSIVE as c_int => "progressive",
                o if o == F::AV_FIELD_TT as c_int => "tt",
                o if o == F::AV_FIELD_BB as c_int => "bb",
                o if o == F::AV_FIELD_TB as c_int => "tb",
                o if o == F::AV_FIELD_BT as c_int => "bt",
                _ => "unknown",
            }
            .to_string(),
        );
        f.rotation_deg = display_rotation(par);
    } else if codec_type == sys::AVMediaType::AVMEDIA_TYPE_AUDIO as c_int {
        if format >= 0 {
            // SAFETY: `format` holds a sample format for audio streams;
            // returns NULL for values this libavutil doesn't know.
            f.sample_fmt = cstr_opt(unsafe { c::av_get_sample_fmt_name(format) });
        }
        // SAFETY: `par` is live; this only takes the field's address.
        f.channel_layout = describe_layout(unsafe { &raw const (*par).ch_layout });
    }
    f
}

/// `l` points at the channel layout of live codec parameters.
fn describe_layout(l: *const sys::AVChannelLayout) -> Option<String> {
    let mut buf = [0 as c_char; 256];
    // SAFETY: `l` is a valid layout in live codec parameters (FFmpeg reads
    // its `order` field as the int it is), and `buf` is a writable buffer
    // of the size we pass.
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

/// `par` points at live codec parameters.
fn display_rotation(par: *const sys::AVCodecParameters) -> Option<f64> {
    // SAFETY: coded_side_data/nb_coded_side_data describe an array owned by
    // `par`; av_packet_side_data_get only reads it.
    let sd = unsafe {
        sys::av_packet_side_data_get(
            (*par).coded_side_data,
            (*par).nb_coded_side_data,
            sys::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        )
    };
    if sd.is_null() {
        return None;
    }
    // SAFETY: non-NULL entry of the array above; only its plain fields are
    // read.
    let (size, data) = unsafe { ((*sd).size, (*sd).data) };
    if size < 36 || data.is_null() {
        return None;
    }
    // SAFETY: a display matrix is nine int32 values, size checked above.
    let r = unsafe { sys::av_display_rotation_get(data as *const i32) };
    r.is_finite().then_some(r)
}

/// Hardware device types compiled into this FFmpeg.
pub fn hw_device_types() -> Vec<String> {
    let mut out = Vec::new();
    let mut t = HW_NONE;
    loop {
        // SAFETY: iterating from NONE is the documented way to list types.
        t = unsafe { c::av_hwdevice_iterate_types(t) };
        if t == HW_NONE {
            break;
        }
        // SAFETY: accepts any type value.
        if let Some(n) = cstr_opt(unsafe { c::av_hwdevice_get_type_name(t) }) {
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
    let t = unsafe { c::av_hwdevice_find_type_by_name(kind_c.as_ptr()) };
    if t == HW_NONE {
        return Err(format!("this FFmpeg was built without {kind} support"));
    }
    let dev_c = device
        .map(CString::new)
        .transpose()
        .map_err(|_| "bad device path".to_string())?;
    let mut ctx: *mut sys::AVBufferRef = std::ptr::null_mut();
    // SAFETY: `ctx` is an out-pointer; device may be NULL (default device).
    let ret = unsafe {
        c::av_hwdevice_ctx_create(
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
        // SAFETY: non-NULL configs point at static tables; the enum field
        // is read as an int.
        let (methods, device_type) =
            unsafe { ((*cfg).methods, enum_int(&raw const (*cfg).device_type)) };
        if methods & sys::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as c_int != 0 {
            // SAFETY: accepts any type value.
            if let Some(n) = cstr_opt(unsafe { c::av_hwdevice_get_type_name(device_type) }) {
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
        let t = unsafe { c::av_hwdevice_find_type_by_name(kind_c.as_ptr()) };
        if t == HW_NONE {
            return Err(format!("this FFmpeg was built without {kind} support"));
        }
        use sys::AVHWDeviceType as T;
        let pix_fmt = match t {
            t if t == T::AV_HWDEVICE_TYPE_VAAPI as c_int => sys::AVPixelFormat::AV_PIX_FMT_VAAPI,
            t if t == T::AV_HWDEVICE_TYPE_VULKAN as c_int => sys::AVPixelFormat::AV_PIX_FMT_VULKAN,
            _ => return Err(format!("{kind} decoding is not supported by Spherecut")),
        };
        let dev_c = device
            .map(CString::new)
            .transpose()
            .map_err(|_| "bad device path".to_string())?;
        let mut ptr: *mut sys::AVBufferRef = std::ptr::null_mut();
        // SAFETY: `ptr` is an out-pointer; device may be NULL.
        let ret = unsafe {
            c::av_hwdevice_ctx_create(
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

    /// [`hw_pixel`](Self::hw_pixel) as FFmpeg's int, to compare with
    /// [`frame_format`].
    pub fn hw_pix_fmt(&self) -> c_int {
        self.pix_fmt as c_int
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
    // wanted format, and a list terminated by AV_PIX_FMT_NONE. Entries are
    // read as ints, since the list can hold formats our headers lack; the
    // one returned equals `want`, which is a format they have.
    unsafe {
        let want = (*ctx).opaque as isize as c_int;
        let mut p = fmts;
        loop {
            let v = enum_int(p);
            if v == sys::AVPixelFormat::AV_PIX_FMT_NONE as c_int {
                break;
            }
            if v == want {
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
        (*c).opaque = dev.pix_fmt as c_int as isize as *mut std::ffi::c_void;
        (*c).get_format = Some(pick_hw_format);
    }
    Ok(())
}

/// A frame's pixel format as FFmpeg's int, to compare and name it without
/// making a bindgen enum of it (as `ff::frame::Video::format` does).
pub fn frame_format(frame: &ff::frame::Video) -> c_int {
    // SAFETY: `frame` wraps a live AVFrame; `format` is a plain int field.
    unsafe { (*frame.as_ptr()).format }
}

/// FFmpeg's name for a pixel format, e.g. "yuv420p" or "vaapi".
pub fn pix_fmt_name(fmt: c_int) -> Option<String> {
    // SAFETY: accepts any value and returns NULL for unknown ones.
    cstr_opt(unsafe { c::av_get_pix_fmt_name(fmt) })
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
