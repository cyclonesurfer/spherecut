//! One of the two modules in this crate that touch FFmpeg's C API
//! directly; the other is `hwprobe`, the doctor's probes.
//!
//! ffmpeg-next covers demuxing and coding; what it doesn't expose safely
//! (color and channel-layout fields, display matrices, hardware device
//! creation, codec hardware configs) is read here and handed out as plain
//! Rust values. Every pointer read is from an object whose lifetime is tied
//! to a borrowed ffmpeg-next wrapper, and every allocation made here is
//! released before returning. FFmpeg's log is routed to `tracing` from
//! here too.
//!
//! C enums never become bindgen's Rust enums here. Those may only hold the
//! values in the headers ffmpeg-sys-next was generated from, and a newer
//! FFmpeg at run time can hand back others (a new pixel format, colour
//! transfer or device type); making a Rust enum from such a value is
//! undefined behaviour. So the functions in [`c`] are declared with the
//! `int` the C ABI passes, and enum fields are read with [`enum_int`].
//! Some of ffmpeg-next's own accessors make such enums (`Frame::format`,
//! `Parameters::medium`, `Context::id` when opening a decoder), so the
//! decode and probe paths use [`frame_format`], [`plane`], [`media_type`]
//! and [`open_video_decoder`] instead. For the same reason they read
//! metadata with [`dict_entries`] (ffmpeg-next makes `&str`s of it
//! without checking that it is UTF-8).

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::AssertUnwindSafe;

use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;
use thiserror::Error;

/// FFmpeg functions that take or return a C enum, declared with `int`s.
/// All of them accept any value. The name functions return static strings:
/// `avcodec_get_name` names an id it doesn't know "unknown_codec", and the
/// others return NULL for values they don't know.
mod c {
    use std::ffi::{c_char, c_int};

    use ffmpeg_sys_next::{AVBufferRef, AVCodec, AVDictionary, AVPixFmtDescriptor};

    unsafe extern "C" {
        pub fn avcodec_find_decoder(id: c_int) -> *const AVCodec;
        pub fn avcodec_get_name(id: c_int) -> *const c_char;
        pub fn avcodec_profile_name(id: c_int, profile: c_int) -> *const c_char;
        pub fn av_get_pix_fmt_name(fmt: c_int) -> *const c_char;
        pub fn av_pix_fmt_desc_get(fmt: c_int) -> *const AVPixFmtDescriptor;
        pub fn av_image_get_linesize(fmt: c_int, width: c_int, plane: c_int) -> c_int;
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
        #[cfg(ffmpeg_7_1)]
        pub fn avcodec_get_supported_config(
            avctx: *const ffmpeg_sys_next::AVCodecContext,
            codec: *const AVCodec,
            config: c_int,
            flags: std::ffi::c_uint,
            out_configs: *mut *const std::ffi::c_void,
            out_num_configs: *mut c_int,
        ) -> c_int;
    }
}

/// Planar 32-bit float, the only sample format `Mp4Writer` sends.
pub(crate) const SAMPLE_FMT_FLTP: c_int = sys::AVSampleFormat::AV_SAMPLE_FMT_FLTP as c_int;

/// The sample formats encoder `codec` takes, as FFmpeg's ints, or `None`
/// when it doesn't say. Ints, because a newer FFmpeg may list formats our
/// headers lack.
pub(crate) fn sample_formats(codec: &ff::Codec) -> Option<Vec<c_int>> {
    // SAFETY: the codec is a static AVCodec FFmpeg owns, and its list is
    // static too.
    unsafe { supported_sample_formats(codec.as_ptr()) }
}

#[cfg(ffmpeg_7_1)]
unsafe fn supported_sample_formats(codec: *const sys::AVCodec) -> Option<Vec<c_int>> {
    let mut list: *const c_void = std::ptr::null();
    let mut n: c_int = 0;
    // SAFETY: a live codec and out-pointers to locals; a NULL context asks
    // for the defaults. The list FFmpeg returns is static.
    let ret = unsafe {
        c::avcodec_get_supported_config(
            std::ptr::null(),
            codec,
            sys::AVCodecConfig::AV_CODEC_CONFIG_SAMPLE_FORMAT as c_int,
            0,
            &mut list,
            &mut n,
        )
    };
    if ret < 0 || list.is_null() {
        return None;
    }
    let n = usize::try_from(n).ok()?;
    // SAFETY: FFmpeg says the list holds `n` sample formats, which are C
    // ints; they are copied out as ints.
    Some(unsafe { std::slice::from_raw_parts(list.cast::<c_int>(), n) }.to_vec())
}

#[cfg(not(ffmpeg_7_1))]
unsafe fn supported_sample_formats(codec: *const sys::AVCodec) -> Option<Vec<c_int>> {
    // SAFETY: a live codec; `sample_fmts` is NULL or a static list ended by
    // AV_SAMPLE_FMT_NONE, read as ints.
    unsafe {
        let mut p = (*codec).sample_fmts;
        if p.is_null() {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let v = enum_int(p);
            if v == sys::AVSampleFormat::AV_SAMPLE_FMT_NONE as c_int {
                return Some(out);
            }
            out.push(v);
            p = p.add(1);
        }
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
    // SAFETY: `par` is live. Both functions accept any id and profile and
    // return static strings: avcodec_get_name never returns NULL (an
    // unknown id is "unknown_codec"), and avcodec_profile_name returns NULL
    // for a profile it doesn't know.
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

/// What a stream holds: `Parameters::medium` without making a bindgen enum
/// of the field. A type our headers lack is `Unknown`.
pub fn media_type(p: &ff::codec::Parameters) -> ff::media::Type {
    use ff::media::Type;
    use sys::AVMediaType as M;
    // SAFETY: `p` borrows live codec parameters; the enum field is read as
    // an int.
    let t = unsafe { enum_int(&raw const (*p.as_ptr()).codec_type) };
    [
        (M::AVMEDIA_TYPE_VIDEO, Type::Video),
        (M::AVMEDIA_TYPE_AUDIO, Type::Audio),
        (M::AVMEDIA_TYPE_DATA, Type::Data),
        (M::AVMEDIA_TYPE_SUBTITLE, Type::Subtitle),
        (M::AVMEDIA_TYPE_ATTACHMENT, Type::Attachment),
    ]
    .into_iter()
    .find(|(m, _)| *m as c_int == t)
    .map_or(Type::Unknown, |(_, kind)| kind)
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

/// Why a hardware device could not be opened or attached to a decoder.
#[derive(Debug, Error)]
pub enum HwError {
    #[error("this FFmpeg was built without {0} support")]
    NotBuiltIn(String),
    #[error("{0} decoding is not supported by Spherecut")]
    Unsupported(String),
    #[error("{what} {name:?} contains a NUL byte")]
    Nul { what: &'static str, name: String },
    /// FFmpeg's own error, e.g. for a missing render node or driver.
    #[error(transparent)]
    Ffmpeg(#[from] ff::Error),
}

/// FFmpeg's device type for a name such as "vaapi".
fn device_type(kind: &str) -> Result<c_int, HwError> {
    let kind_c = CString::new(kind).map_err(|_| HwError::Nul {
        what: "device type",
        name: kind.into(),
    })?;
    // SAFETY: valid NUL-terminated string.
    let t = unsafe { c::av_hwdevice_find_type_by_name(kind_c.as_ptr()) };
    if t == HW_NONE {
        return Err(HwError::NotBuiltIn(kind.into()));
    }
    Ok(t)
}

/// Create a device context of type `t` on `device` (`None` for the
/// default). The caller owns the returned reference.
fn create_device(t: c_int, device: Option<&str>) -> Result<*mut sys::AVBufferRef, HwError> {
    let dev_c = device
        .map(|d| {
            CString::new(d).map_err(|_| HwError::Nul {
                what: "device path",
                name: d.into(),
            })
        })
        .transpose()?;
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
    if ret < 0 {
        if !ctx.is_null() {
            // SAFETY: we own the reference returned above.
            unsafe { sys::av_buffer_unref(&mut ctx) };
        }
        return Err(ff::Error::from(ret).into());
    }
    if ctx.is_null() {
        return Err(ff::Error::Bug.into());
    }
    Ok(ctx)
}

fn log_device(kind: &str, device: Option<&str>, e: Option<&HwError>) {
    let device = device.unwrap_or("default");
    match e {
        None => tracing::debug!(kind, device, "opened hardware device"),
        Some(e) => tracing::debug!(kind, device, "hardware device refused: {e}"),
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

/// The entries of a dictionary, such as a file's or a stream's metadata,
/// in order. FFmpeg doesn't check that metadata is UTF-8 (the MOV demuxer
/// copies a title's bytes as they are), so bytes that aren't become
/// U+FFFD. ffmpeg-next's own iterator would make `&str`s of them.
pub fn dict_entries(d: &ff::DictionaryRef<'_>) -> Vec<(String, String)> {
    // SAFETY: `d` borrows a live dictionary, or NULL for an empty one.
    let m = unsafe { d.as_ptr() };
    let mut out = Vec::new();
    let mut e: *const sys::AVDictionaryEntry = std::ptr::null();
    loop {
        // SAFETY: av_dict_get accepts NULL for an empty dictionary. An
        // empty key with AV_DICT_IGNORE_SUFFIX matches every entry, and `e`
        // is NULL or the entry it returned last; the dictionary doesn't
        // change while `d` is borrowed.
        e = unsafe { sys::av_dict_get(m, c"".as_ptr(), e, sys::AV_DICT_IGNORE_SUFFIX) };
        if e.is_null() {
            return out;
        }
        // SAFETY: an entry's key and value are NUL-terminated strings the
        // dictionary owns.
        let (k, v) = unsafe { (CStr::from_ptr((*e).key), CStr::from_ptr((*e).value)) };
        out.push((
            k.to_string_lossy().into_owned(),
            v.to_string_lossy().into_owned(),
        ));
    }
}

/// `AVFormatContext.start_time` in `AV_TIME_BASE` units.
pub fn format_start_time(ictx: &ff::format::context::Input) -> i64 {
    // SAFETY: `ictx` owns a live AVFormatContext; reading a plain field.
    unsafe { (*ictx.as_ptr()).start_time }
}

/// Where the last sample that the demuxer's index lists for stream `index`
/// ends, in bytes from the start of the file. `None` without an index.
pub fn index_end(ictx: &ff::format::context::Input, index: usize) -> Option<u64> {
    let st = ictx.stream(index)?;
    // SAFETY: the stream lives as long as `ictx`, which stays borrowed. The
    // index functions only read it, though avformat_index_get_entry's
    // parameter isn't const.
    let st = unsafe { st.as_ptr() }.cast_mut();
    // SAFETY: as above.
    let n = unsafe { sys::avformat_index_get_entries_count(st) };
    (0..n)
        .filter_map(|i| {
            // SAFETY: `i` is in range, and the entry is read before anything
            // else can touch the stream; AVIndexEntry has no enum fields.
            let e = unsafe { sys::avformat_index_get_entry(st, i).as_ref() }?;
            let pos = u64::try_from(e.pos).ok()?;
            Some(pos + u64::try_from(e.size()).unwrap_or(0))
        })
        .max()
}

/// Have the demuxer skip every stream but `keep`: it then neither reads
/// their data nor stops at damage in it.
pub fn discard_other_streams(ictx: &mut ff::format::context::Input, keep: usize) {
    // SAFETY: `ictx` owns a live AVFormatContext whose `streams` array
    // holds `nb_streams` live streams, and a stream's `discard` may be set
    // between reads. The value written is one our headers define.
    unsafe {
        let fc = ictx.as_mut_ptr();
        for i in 0..(*fc).nb_streams as usize {
            if i != keep {
                (**(*fc).streams.add(i)).discard = sys::AVDiscard::AVDISCARD_ALL;
            }
        }
    }
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
    pub fn open(kind: &str, device: Option<&str>) -> Result<Self, HwError> {
        let r = device_type(kind).and_then(|t| {
            use sys::AVHWDeviceType as T;
            let pix_fmt = match t {
                t if t == T::AV_HWDEVICE_TYPE_VAAPI as c_int => {
                    sys::AVPixelFormat::AV_PIX_FMT_VAAPI
                }
                t if t == T::AV_HWDEVICE_TYPE_VULKAN as c_int => {
                    sys::AVPixelFormat::AV_PIX_FMT_VULKAN
                }
                _ => return Err(HwError::Unsupported(kind.into())),
            };
            Ok(HwDevice {
                ptr: create_device(t, device)?,
                pix_fmt,
                kind: kind.to_string(),
            })
        });
        log_device(kind, device, r.as_ref().err());
        r
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

    /// The device context reference, still owned by `self`, for the
    /// capability probes in `hwprobe`.
    pub(crate) fn as_ptr(&self) -> *mut sys::AVBufferRef {
        self.ptr
    }
}

/// Why a decoder could not decode on its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HwRefusal {
    /// The decoder never offered the device's format: this FFmpeg was
    /// built without that hwaccel, or it doesn't cover this profile.
    #[error("this FFmpeg has no hardware decoder for this format")]
    NotOffered,
    /// FFmpeg offered the format, then setting up its hardware decoder
    /// failed: the driver can't decode this profile.
    #[error("the driver does not support this format")]
    Withdrawn,
}

/// What [`pick_hw_format`] keeps in a decoder context's `opaque`, which
/// FFmpeg never reads: the device's pixel format, and what FFmpeg did with
/// it. Kept in the context itself, it lives exactly as long as the
/// decoder. (Frame threads would each get a copy; hardware decoding here
/// uses one thread.)
mod hw_choice {
    /// The pixel format, in the low bits.
    pub const FORMAT: usize = (1 << 24) - 1;
    /// FFmpeg asked for a format at all.
    pub const ASKED: usize = 1 << 29;
    /// The device's format was among those offered.
    pub const OFFERED: usize = 1 << 30;
    /// FFmpeg asked again without it after we took it, which it does when
    /// setting up its hardware decoder failed.
    pub const WITHDRAWN: usize = 1 << 31;
}

/// `get_format` callback: take the device's format, or fail. Failing
/// (instead of FFmpeg's silent software fallback) lets the caller decide
/// and report what happened. The decoder then fails with AV_PIX_FMT_NONE
/// as its error, "Operation not permitted", so [`hw_refusal`] says why.
unsafe extern "C" fn pick_hw_format(
    ctx: *mut sys::AVCodecContext,
    fmts: *const sys::AVPixelFormat,
) -> sys::AVPixelFormat {
    use hw_choice::*;
    // SAFETY: FFmpeg passes a live context, whose `opaque` attach_hw_device
    // set, and a list terminated by AV_PIX_FMT_NONE. Entries are read as
    // ints, since the list can hold formats our headers lack; the one
    // returned equals the device's, which is a format they have.
    unsafe {
        let mut state = (*ctx).opaque.addr() | ASKED;
        let want = (state & FORMAT) as c_int;
        let mut p = fmts;
        let mut chosen = sys::AVPixelFormat::AV_PIX_FMT_NONE;
        loop {
            let v = enum_int(p);
            if v == sys::AVPixelFormat::AV_PIX_FMT_NONE as c_int {
                break;
            }
            if v == want {
                chosen = *p;
                break;
            }
            p = p.add(1);
        }
        if chosen != sys::AVPixelFormat::AV_PIX_FMT_NONE {
            state |= OFFERED;
        } else if state & OFFERED != 0 {
            state |= WITHDRAWN;
        }
        (*ctx).opaque = std::ptr::without_provenance_mut(state);
        chosen
    }
}

/// Configure an unopened decoder context to decode on `dev`.
pub(crate) fn attach_hw_device(
    ctx: &mut ff::codec::context::Context,
    dev: &HwDevice,
) -> Result<(), HwError> {
    let want = dev.pix_fmt as c_int as usize;
    debug_assert!(want <= hw_choice::FORMAT);
    // SAFETY: the context is not yet opened, so setting hw_device_ctx and
    // get_format is allowed; we hand FFmpeg its own new reference.
    unsafe {
        let c = ctx.as_mut_ptr();
        let r = sys::av_buffer_ref(dev.ptr);
        if r.is_null() {
            return Err(ff::Error::Other {
                errno: ff::util::error::ENOMEM,
            }
            .into());
        }
        if !(*c).hw_device_ctx.is_null() {
            sys::av_buffer_unref(&mut (*c).hw_device_ctx);
        }
        (*c).hw_device_ctx = r;
        (*c).opaque = std::ptr::without_provenance_mut(want & hw_choice::FORMAT);
        (*c).get_format = Some(pick_hw_format);
    }
    Ok(())
}

/// Why a decoder set up by [`attach_hw_device`] turned down its device's
/// format, if it did.
pub(crate) fn hw_refusal(dec: &ff::codec::context::Context) -> Option<HwRefusal> {
    use hw_choice::*;
    // SAFETY: the wrapper owns a live context; only `opaque` is read, which
    // attach_hw_device and pick_hw_format set.
    let state = unsafe { (*dec.as_ptr()).opaque.addr() };
    if state & WITHDRAWN != 0 {
        Some(HwRefusal::Withdrawn)
    } else if state & ASKED != 0 && state & OFFERED == 0 {
        Some(HwRefusal::NotOffered)
    } else {
        None
    }
}

/// Open the decoder for a codec context made from a video stream's
/// parameters. ffmpeg-next's `Decoder::video` would look the decoder up
/// from the codec id, and check the media type, as bindgen enums; a file
/// with a codec newer than our headers would make an invalid one.
pub fn open_video_decoder(
    ctx: ff::codec::context::Context,
) -> Result<ff::decoder::Video, ff::Error> {
    // SAFETY: `ctx` owns a live AVCodecContext; its enum fields are read as
    // ints.
    let (kind, id) = unsafe {
        let c = ctx.as_ptr();
        (
            enum_int(&raw const (*c).codec_type),
            enum_int(&raw const (*c).codec_id),
        )
    };
    if kind != sys::AVMediaType::AVMEDIA_TYPE_VIDEO as c_int {
        return Err(ff::Error::InvalidData);
    }
    // SAFETY: accepts any id; NULL when no decoder for it is built in.
    let codec = unsafe { c::avcodec_find_decoder(id) };
    if codec.is_null() {
        return Err(ff::Error::DecoderNotFound);
    }
    // SAFETY: registered codecs are static.
    let codec = unsafe { ff::Codec::wrap(codec) };
    // The media type is checked above, which is all `Opened::video` checks.
    ctx.decoder().open_as(codec).map(ff::decoder::Video)
}

/// A frame's pixel format as FFmpeg's int, to compare and name it without
/// making a bindgen enum of it (as `ff::frame::Video::format` does).
pub fn frame_format(frame: &ff::frame::Video) -> c_int {
    // SAFETY: `frame` wraps a live AVFrame; `format` is a plain int field.
    unsafe { (*frame.as_ptr()).format }
}

/// Plane `index` of a frame in system memory, as `ff::frame::Video::data`
/// gives it, but sized from the pixel format read as an int (`data` calls
/// `Frame::format`), and never past the buffer that holds the plane.
///
/// The slice has `linesize` bytes for each row, but ends at the end of
/// that buffer if it comes first, so the last row may be cut to the bytes
/// the frame's width covers. That happens for a frame the decoder cropped
/// on the left: its `data` moves right, and its last row has no padding
/// after it to cover the move.
///
/// `None` for a plane the format doesn't have, for a hardware frame, for
/// a format this libavutil doesn't describe, and for a plane whose rows
/// don't fit in its buffer (a frame whose size was changed after its
/// buffers were allocated).
pub fn plane(frame: &ff::frame::Video, index: usize) -> Option<&[u8]> {
    // SAFETY: only takes the pointer to the AVFrame that `frame` wraps.
    let f = unsafe { frame.as_ptr() };
    // SAFETY: `f` is a live AVFrame; these are plain fields.
    let (format, width, height, data, linesize) = unsafe {
        (
            (*f).format,
            (*f).width,
            (*f).height,
            *(*f).data.get(index)?,
            *(*f).linesize.get(index)?,
        )
    };
    // SAFETY: accepts any value and returns NULL for unknown ones; non-NULL
    // descriptors point at static tables with no enum fields.
    let d = unsafe { c::av_pix_fmt_desc_get(format).as_ref() }?;
    let planes = d.comp[..usize::from(d.nb_components).min(4)]
        .iter()
        .map(|c| c.plane as usize + 1)
        .max()?;
    let hw = d.flags & sys::AV_PIX_FMT_FLAG_HWACCEL as u64 != 0;
    if hw || index >= planes || data.is_null() || linesize <= 0 || width <= 0 || height <= 0 {
        return None;
    }
    // Planes 1 and 2 hold chroma, subsampled vertically (rounding up).
    let rows = match index {
        1 | 2 => -((-height) >> d.log2_chroma_h),
        _ => height,
    };
    // SAFETY: accepts any format and width (negative for a bad one), and
    // `index` is a plane of the format, so below 4.
    let row = unsafe { c::av_image_get_linesize(format, width, index as c_int) };
    let (row, rows, linesize) = (usize::try_from(row).ok()?, rows as usize, linesize as usize);
    let needed = linesize.checked_mul(rows - 1)?.checked_add(row)?;
    let room = buffer_room(f, data)?;
    if needed > room {
        return None;
    }
    // SAFETY: the slice starts at `data` and ends inside the buffer that
    // holds it (`room`), which the frame keeps a reference to. It lives as
    // long as the frame, which stays borrowed.
    Some(unsafe { std::slice::from_raw_parts(data, linesize.saturating_mul(rows).min(room)) })
}

/// How many bytes there are from `p` to the end of the buffer of `frame`
/// that holds `p`. `None` when no buffer of the frame holds it.
fn buffer_room(frame: *const sys::AVFrame, p: *const u8) -> Option<usize> {
    // SAFETY: `frame` is a live AVFrame. `buf` holds NULL or references
    // the frame owns, and `extended_buf` holds `nb_extended_buf` more.
    let bufs = unsafe {
        let extended = match usize::try_from((*frame).nb_extended_buf) {
            Ok(n) if n > 0 && !(*frame).extended_buf.is_null() => {
                std::slice::from_raw_parts((*frame).extended_buf, n)
            }
            _ => &[],
        };
        (*frame).buf.iter().chain(extended).copied()
    };
    let p = p.addr();
    bufs.filter(|b| !b.is_null()).find_map(|b| {
        // SAFETY: a live buffer reference; plain fields.
        let (start, size) = unsafe { ((*b).data.addr(), (*b).size) };
        (start..start.checked_add(size)?)
            .contains(&p)
            .then(|| start + size - p)
    })
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

/// The `va_list` of FFmpeg's log callback, as bindgen spells the parameter:
/// on x86-64 `va_list` is an array type, so it is passed as a pointer. It is
/// only ever handed back to FFmpeg untouched.
#[cfg(target_arch = "x86_64")]
type VaList = *mut sys::__va_list_tag;
#[cfg(not(target_arch = "x86_64"))]
type VaList = sys::va_list;

/// Send FFmpeg's log to `tracing`, under the target "ffmpeg".
pub fn route_log_to_tracing() {
    sync_log_level();
    // SAFETY: `log_callback` has the signature FFmpeg expects, can be called
    // from any thread, and never unwinds into C.
    unsafe { sys::av_log_set_callback(Some(log_callback)) };
}

/// Set FFmpeg's log level to the most verbose one the current `tracing`
/// subscriber accepts for target "ffmpeg", so that FFmpeg's messages
/// nobody would see are not even formatted.
pub fn sync_log_level() {
    use tracing::{Level, enabled};
    let level = if enabled!(target: "ffmpeg", Level::TRACE) {
        sys::AV_LOG_TRACE
    } else if enabled!(target: "ffmpeg", Level::DEBUG) {
        sys::AV_LOG_VERBOSE
    } else if enabled!(target: "ffmpeg", Level::INFO) {
        sys::AV_LOG_INFO
    } else if enabled!(target: "ffmpeg", Level::WARN) {
        sys::AV_LOG_WARNING
    } else if enabled!(target: "ffmpeg", Level::ERROR) {
        sys::AV_LOG_ERROR
    } else {
        sys::AV_LOG_QUIET
    };
    // SAFETY: sets a global int; any level is allowed.
    unsafe { sys::av_log_set_level(level) };
}

/// The `tracing` level for an FFmpeg level: FFmpeg's verbose is debug, and
/// its debug and trace are trace.
fn tracing_level(level: c_int) -> tracing::Level {
    use tracing::Level;
    if level <= sys::AV_LOG_ERROR {
        Level::ERROR
    } else if level <= sys::AV_LOG_WARNING {
        Level::WARN
    } else if level <= sys::AV_LOG_INFO {
        Level::INFO
    } else if level <= sys::AV_LOG_VERBOSE {
        Level::DEBUG
    } else {
        Level::TRACE
    }
}

thread_local! {
    /// av_log_format_line2's "at the start of a line" flag. FFmpeg may log
    /// one line in several calls, and only the first gets the
    /// "[hevc @ 0x…]" prefix. Threads log independently, so each keeps its
    /// own.
    static PRINT_PREFIX: Cell<c_int> = const { Cell::new(1) };
}

/// FFmpeg's log callback. FFmpeg calls it for every message, whatever the
/// log level, from whichever thread logs (decoder threads included).
unsafe extern "C" fn log_callback(avcl: *mut c_void, level: c_int, fmt: *const c_char, vl: VaList) {
    // A panic must not unwind into C; dropping the message is enough.
    let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the arguments of this callback, passed on unchanged.
        unsafe { forward_log(avcl, level, fmt, vl) }
    }));
}

/// # Safety
///
/// The arguments of one call of FFmpeg's log callback, with `vl` unused.
unsafe fn forward_log(avcl: *mut c_void, level: c_int, fmt: *const c_char, vl: VaList) {
    // The low byte is the level; FFmpeg may put colour hints above it.
    let plain = if level >= 0 { level & 0xff } else { level };
    // SAFETY: reads a global int.
    if plain > unsafe { sys::av_log_get_level() } {
        return;
    }
    let mut line = [0 as c_char; 1024];
    let mut prefix = PRINT_PREFIX.try_with(Cell::get).unwrap_or(1);
    // SAFETY: `avcl`, `fmt` and `vl` are FFmpeg's own and `vl` is consumed
    // only here; `line` is writable for the size passed, and FFmpeg
    // truncates to it and NUL-terminates.
    let n = unsafe {
        sys::av_log_format_line2(
            avcl,
            level,
            fmt,
            vl,
            line.as_mut_ptr(),
            line.len() as c_int,
            &mut prefix,
        )
    };
    let _ = PRINT_PREFIX.try_with(|p| p.set(prefix));
    if n < 0 {
        return;
    }
    // SAFETY: NUL-terminated within `line` (above).
    let msg = unsafe { CStr::from_ptr(line.as_ptr()) }.to_string_lossy();
    let msg = msg.trim_end();
    if msg.is_empty() {
        return;
    }
    use tracing::Level;
    match tracing_level(plain) {
        Level::ERROR => tracing::error!(target: "ffmpeg", "{msg}"),
        Level::WARN => tracing::warn!(target: "ffmpeg", "{msg}"),
        Level::INFO => tracing::info!(target: "ffmpeg", "{msg}"),
        Level::DEBUG => tracing::debug!(target: "ffmpeg", "{msg}"),
        _ => tracing::trace!(target: "ffmpeg", "{msg}"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::fmt::Debug;
    use std::sync::Mutex;

    use tracing::subscriber::Interest;
    use tracing::{Event, Level, Metadata, Subscriber, field, span};

    use super::*;

    /// The most verbose "ffmpeg" level [`Capture`] accepts, `None` for none.
    static MAX: Mutex<Option<Level>> = Mutex::new(Some(Level::TRACE));
    static EVENTS: Mutex<Vec<(Level, String)>> = Mutex::new(Vec::new());

    /// Records "ffmpeg" events up to [`MAX`], asking on every event.
    struct Capture;

    impl Subscriber for Capture {
        fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
            Interest::sometimes()
        }
        fn enabled(&self, m: &Metadata<'_>) -> bool {
            m.target() == "ffmpeg" && MAX.lock().unwrap().is_some_and(|max| *m.level() <= max)
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, e: &Event<'_>) {
            let mut m = Message(String::new());
            e.record(&mut m);
            EVENTS.lock().unwrap().push((*e.metadata().level(), m.0));
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    struct Message(String);

    impl field::Visit for Message {
        fn record_debug(&mut self, f: &field::Field, v: &dyn Debug) {
            if f.name() == "message" {
                self.0 = format!("{v:?}");
            }
        }
    }

    fn log(level: c_int, msg: &str) {
        let msg = CString::new(msg).unwrap();
        // SAFETY: a NULL context is allowed, and the format takes the one
        // string passed.
        unsafe { sys::av_log(std::ptr::null_mut(), level, c"%s".as_ptr(), msg.as_ptr()) };
    }

    /// Events whose message contains `tag`, in order.
    fn captured(tag: &str) -> Vec<(Level, String)> {
        let events = EVENTS.lock().unwrap();
        events
            .iter()
            .filter(|(_, m)| m.contains(tag))
            .cloned()
            .collect()
    }

    fn set_max(max: Option<Level>) {
        *MAX.lock().unwrap() = max;
        sync_log_level();
    }

    /// A decoder context whose get_format wants `want`, as
    /// attach_hw_device leaves it (without a device).
    fn wanting(want: c_int) -> ff::codec::context::Context {
        let mut ctx = ff::codec::context::Context::new();
        // SAFETY: a fresh context that nothing else holds; FFmpeg never
        // reads `opaque`.
        unsafe {
            (*ctx.as_mut_ptr()).opaque = std::ptr::without_provenance_mut(want as usize);
        }
        ctx
    }

    /// Offer `fmts` to `ctx`'s get_format, as a decoder does.
    fn offer(ctx: &mut ff::codec::context::Context, fmts: &[c_int]) -> c_int {
        let none = sys::AVPixelFormat::AV_PIX_FMT_NONE as c_int;
        let list: Vec<c_int> = fmts.iter().copied().chain([none]).collect();
        // SAFETY: a live context set up as attach_hw_device does, and a
        // list ended by AV_PIX_FMT_NONE, which is read as ints.
        unsafe { pick_hw_format(ctx.as_mut_ptr(), list.as_ptr().cast()) as c_int }
    }

    #[test]
    fn get_format_takes_only_the_device_format_and_notes_why_not() {
        use sys::AVPixelFormat as P;
        let (vaapi, yuv, none) = (
            P::AV_PIX_FMT_VAAPI as c_int,
            P::AV_PIX_FMT_YUV420P as c_int,
            P::AV_PIX_FMT_NONE as c_int,
        );
        // Not asked yet: nothing to say.
        assert_eq!(hw_refusal(&wanting(vaapi)), None);

        // Offered, among formats our headers may lack: taken.
        let mut ctx = wanting(vaapi);
        assert_eq!(offer(&mut ctx, &[yuv, 100_000, vaapi]), vaapi);
        assert_eq!(hw_refusal(&ctx), None);
        // Then withdrawn, as when the hardware decoder fails to start.
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::Withdrawn));
        // It stays withdrawn.
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::Withdrawn));

        // Never offered.
        let mut ctx = wanting(vaapi);
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::NotOffered));
    }

    /// One mid-grey 512x128 frame, encoded as Annex B HEVC by libx265.
    fn hevc_frame() -> Vec<ff::Packet> {
        use ff::format::Pixel;
        let codec = ff::encoder::find_by_name("libx265").unwrap();
        let mut enc = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .unwrap();
        enc.set_width(512);
        enc.set_height(128);
        enc.set_format(Pixel::YUV420P);
        enc.set_time_base((1, 30));
        let mut opts = ff::Dictionary::new();
        opts.set("preset", "ultrafast");
        opts.set("x265-params", "log-level=error");
        let mut enc = enc.open_as_with(codec, opts).unwrap();
        let mut frame = ff::frame::Video::new(Pixel::YUV420P, 512, 128);
        for i in 0..3 {
            frame.data_mut(i).fill(128);
        }
        frame.set_pts(Some(0));
        enc.send_frame(&frame).unwrap();
        enc.send_eof().unwrap();
        let mut out = Vec::new();
        let mut pkt = ff::Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            out.push(std::mem::replace(&mut pkt, ff::Packet::empty()));
        }
        out
    }

    /// `packets` through FFmpeg's hevc_metadata filter, which sets the
    /// SPS conformance window to crop `left` luma samples on the left.
    fn crop_left(packets: Vec<ff::Packet>, left: i64) -> Vec<ff::Packet> {
        use ff::codec::packet::Mut;
        let mut out = Vec::new();
        // SAFETY: the filter context is allocated, set up before
        // av_bsf_init as FFmpeg requires (a codec id our headers have, and
        // an option of the filter), and freed at the end. Each packet sent
        // is a live one whose data the filter takes; each received goes
        // into a fresh packet that we own.
        unsafe {
            let filter = sys::av_bsf_get_by_name(c"hevc_metadata".as_ptr());
            assert!(!filter.is_null());
            let mut bsf = std::ptr::null_mut();
            assert!(sys::av_bsf_alloc(filter, &mut bsf) >= 0);
            (*(*bsf).par_in).codec_id = sys::AVCodecID::AV_CODEC_ID_HEVC;
            let set = sys::av_opt_set_int((*bsf).priv_data, c"crop_left".as_ptr(), left, 0);
            assert!(set >= 0);
            assert!(sys::av_bsf_init(bsf) >= 0);
            let drain = |out: &mut Vec<ff::Packet>| loop {
                let mut p = ff::Packet::empty();
                if sys::av_bsf_receive_packet(bsf, p.as_mut_ptr()) < 0 {
                    break;
                }
                out.push(p);
            };
            for mut p in packets {
                assert!(sys::av_bsf_send_packet(bsf, p.as_mut_ptr()) >= 0);
                drain(&mut out);
            }
            assert!(sys::av_bsf_send_packet(bsf, std::ptr::null_mut()) >= 0);
            drain(&mut out);
            sys::av_bsf_free(&mut bsf);
        }
        out
    }

    /// Where the buffer that holds plane `index` of `frame` ends, as
    /// FFmpeg finds that buffer.
    fn buffer_end(frame: &ff::frame::Video, index: c_int) -> *const u8 {
        // SAFETY: a live frame, which the call only reads (FFmpeg before
        // 7.0 declares it non-const). The buffer returned is the frame's
        // own, with `size` bytes from `data`.
        unsafe {
            let b = sys::av_frame_get_plane_buffer(frame.as_ptr().cast_mut(), index);
            assert!(!b.is_null());
            (*b).data.add((*b).size)
        }
    }

    #[test]
    fn planes_of_a_frame_cropped_on_the_left_end_inside_their_buffers() {
        let codec = ff::decoder::find_by_name("hevc").unwrap();
        let mut dec = ff::codec::context::Context::new_with_codec(codec)
            .decoder()
            .open_as(codec)
            .and_then(|d| d.video())
            .unwrap();
        // 128 luma bytes: more than the padding FFmpeg leaves after a
        // plane, and the frame has no bottom crop to make up for it.
        for p in crop_left(hevc_frame(), 128) {
            dec.send_packet(&p).unwrap();
        }
        dec.send_eof().unwrap();
        let mut frame = ff::frame::Video::empty();
        dec.receive_frame(&mut frame).unwrap();
        assert_eq!((frame.width(), frame.height()), (384, 128));

        for (i, rows, row) in [(0, 128, 384), (1, 64, 192), (2, 64, 192)] {
            let p = plane(&frame, i).unwrap();
            assert!(
                p.as_ptr_range().end <= buffer_end(&frame, i as c_int),
                "plane {i} runs past its buffer"
            );
            // Every row of the cropped picture is there.
            assert!(p.len() >= frame.stride(i) * (rows - 1) + row, "plane {i}");
        }
        // The luma plane is the one cut short at its buffer's end.
        assert!(plane(&frame, 0).unwrap().len() < frame.stride(0) * 128);
    }

    #[test]
    fn a_plane_whose_rows_do_not_fit_its_buffer_is_none() {
        use ff::format::Pixel;
        let mut f = ff::frame::Video::new(Pixel::YUV420P, 64, 16);
        assert_eq!(plane(&f, 0).unwrap().len(), f.stride(0) * 16);
        // ffmpeg-next's safe setters change the size, not the buffers.
        f.set_height(4096);
        for i in 0..3 {
            assert!(plane(&f, i).is_none(), "plane {i}");
        }
        f.set_height(16);
        f.set_width(1 << 20);
        assert!(plane(&f, 0).is_none());
    }

    // One test, because the subscriber and FFmpeg's level are global.
    #[test]
    fn ffmpeg_log_reaches_tracing_at_the_level_it_asks_for() {
        tracing::subscriber::set_global_default(Capture).unwrap();
        crate::init();
        set_max(Some(Level::TRACE));
        // SAFETY: reads a global int.
        assert_eq!(unsafe { sys::av_log_get_level() }, sys::AV_LOG_TRACE);

        // Each FFmpeg level lands on its tracing level; colour hints above
        // the low byte don't change it.
        for (av, want) in [
            (sys::AV_LOG_PANIC, Level::ERROR),
            (sys::AV_LOG_FATAL, Level::ERROR),
            (sys::AV_LOG_ERROR, Level::ERROR),
            (sys::AV_LOG_WARNING, Level::WARN),
            (sys::AV_LOG_INFO | (134 << 8), Level::INFO),
            (sys::AV_LOG_VERBOSE, Level::DEBUG),
            (sys::AV_LOG_DEBUG, Level::TRACE),
            (sys::AV_LOG_TRACE, Level::TRACE),
        ] {
            let tag = format!("level-test {av}");
            log(av, &format!("{tag}\n"));
            assert_eq!(captured(&tag), [(want, tag.clone())]);
        }

        // Several threads at once: every line arrives once and whole.
        std::thread::scope(|s| {
            for t in 0..8 {
                s.spawn(move || {
                    for i in 0..200 {
                        log(sys::AV_LOG_WARNING, &format!("thread-test {t}/{i} end\n"));
                    }
                });
            }
        });
        let got = captured("thread-test");
        assert_eq!(got.len(), 8 * 200);
        for t in 0..8 {
            for i in 0..200 {
                let line = format!("thread-test {t}/{i} end");
                assert_eq!(got.iter().filter(|(_, m)| *m == line).count(), 1, "{line}");
            }
        }

        // A line logged in two calls is prefixed with its context once.
        // SAFETY: allocates a context to log with, freed below.
        let fctx = unsafe { sys::avformat_alloc_context() };
        assert!(!fctx.is_null());
        for part in [
            c"prefix-test start, ",
            c"prefix-test end\n",
            c"prefix-test next\n",
        ] {
            // SAFETY: `fctx` starts with its AVClass, as av_log requires;
            // the format has no conversions.
            unsafe { sys::av_log(fctx.cast(), sys::AV_LOG_ERROR, part.as_ptr()) };
        }
        // SAFETY: frees the context allocated above, which nothing else
        // holds.
        unsafe { sys::avformat_free_context(fctx) };
        let got: Vec<_> = captured("prefix-test")
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].starts_with("[AVFormatContext @ 0x"), "{got:?}");
        assert!(got[0].ends_with("] prefix-test start,"), "{got:?}");
        assert_eq!(got[1], "prefix-test end");
        assert!(got[2].starts_with("[AVFormatContext @ 0x"), "{got:?}");

        // FFmpeg's level follows the subscriber, so it skips what nobody
        // reads.
        set_max(Some(Level::WARN));
        // SAFETY: reads a global int.
        assert_eq!(unsafe { sys::av_log_get_level() }, sys::AV_LOG_WARNING);
        log(sys::AV_LOG_INFO, "quiet-test info\n");
        log(sys::AV_LOG_WARNING, "quiet-test warning\n");
        assert_eq!(
            captured("quiet-test"),
            [(Level::WARN, "quiet-test warning".to_string())]
        );
        set_max(None);
        // SAFETY: reads a global int.
        assert_eq!(unsafe { sys::av_log_get_level() }, sys::AV_LOG_QUIET);
        log(sys::AV_LOG_PANIC, "off-test\n");
        assert!(captured("off-test").is_empty());
    }
}
