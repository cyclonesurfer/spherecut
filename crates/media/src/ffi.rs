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
//! and [`open_video_decoder`] instead. For the same reason they open files
//! with [`open_input`] (ffmpeg-next panics on a path that isn't UTF-8) and
//! read metadata with [`dict_entries`] (ffmpeg-next makes `&str`s of it
//! without checking that it is UTF-8).

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    for t in decoder_device_types(codec) {
        // SAFETY: accepts any type value.
        if let Some(n) = cstr_opt(unsafe { c::av_hwdevice_get_type_name(t) }) {
            out.push(n);
        }
    }
    out
}

/// [`decoder_hw_configs`] as FFmpeg's ints.
fn decoder_device_types(codec: &ff::Codec) -> Vec<c_int> {
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
            out.push(device_type);
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

/// Open a file for demuxing and read its stream info, as
/// `ff::format::input` does, or `input_with_interrupt` with `cancel`
/// (FFmpeg stops reading once it is set). FFmpeg gets the path's bytes as
/// they are: ffmpeg-next's functions panic on a path that is not UTF-8,
/// and Linux file names need not be.
pub fn open_input(
    path: &Path,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<ff::format::context::Input, ff::Error> {
    let url = CString::new(path.as_os_str().as_bytes()).map_err(|_| ff::Error::Other {
        errno: ff::util::error::EINVAL,
    })?;
    // SAFETY: allocates a context, which we own until it is handed to
    // avformat_open_input below.
    let mut ps = unsafe { sys::avformat_alloc_context() };
    if ps.is_null() {
        return Err(ff::Error::Other {
            errno: ff::util::error::ENOMEM,
        });
    }
    // The guard frees the closure when dropped, which is after FFmpeg has
    // freed the context on every path below.
    let guard = cancel.map(|cancel| {
        let i = ff::util::interrupt::new(Box::new(move || cancel.load(Ordering::Relaxed)));
        // SAFETY: `ps` is live and ours; FFmpeg calls the callback with
        // its opaque, the closure, which the guard keeps alive.
        unsafe { (*ps).interrupt_callback = i.interrupt };
        i.guard
    });
    // SAFETY: `ps` is a context from avformat_alloc_context and `url` a
    // NUL-terminated string that outlives the call. On failure FFmpeg
    // frees the context.
    let ret = unsafe {
        sys::avformat_open_input(
            &mut ps,
            url.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ret < 0 {
        return Err(ff::Error::from(ret));
    }
    // SAFETY: an open input context.
    let ret = unsafe { sys::avformat_find_stream_info(ps, std::ptr::null_mut()) };
    if ret < 0 {
        // SAFETY: closes and frees the context opened above, which nothing
        // else holds.
        unsafe { sys::avformat_close_input(&mut ps) };
        return Err(ff::Error::from(ret));
    }
    // SAFETY: an open input context, which the wrapper now owns: it closes
    // the context when dropped, and only then drops the guard.
    Ok(unsafe {
        match guard {
            Some(guard) => ff::format::context::Input::wrap_with_interrupt(ps, guard),
            None => ff::format::context::Input::wrap(ps),
        }
    })
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
    /// FFmpeg's device type.
    device_type: c_int,
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
                device_type: t,
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

    /// A Vulkan "device" with no device context, for tests that only look
    /// at its kind and pixel format. Dropping it unrefs NULL, which FFmpeg
    /// allows.
    #[cfg(test)]
    pub(crate) fn vulkan_without_context() -> Self {
        HwDevice {
            ptr: std::ptr::null_mut(),
            device_type: sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN as c_int,
            pix_fmt: sys::AVPixelFormat::AV_PIX_FMT_VULKAN,
            kind: "vulkan".into(),
        }
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
/// it at the latest `get_format` call. Kept in the context itself, it
/// lives exactly as long as the decoder. (Frame threads would each get a
/// copy; hardware decoding here uses one thread.)
mod hw_choice {
    /// The pixel format, in the low bits.
    pub const FORMAT: usize = (1 << 24) - 1;
    /// FFmpeg asked for a format at all.
    pub const ASKED: usize = 1 << 28;
    /// The latest call offered the device's format, and we took it.
    pub const TAKEN: usize = 1 << 29;
    /// A frame came out on the device since then, so FFmpeg's hardware
    /// decoder for that format works.
    pub const DECODED: usize = 1 << 30;
    /// The latest call lacked the format right after a call that took it,
    /// with no frame in between. FFmpeg asks again like that when setting
    /// up its hardware decoder failed.
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
        let prev = (*ctx).opaque.addr();
        let retry = prev & TAKEN != 0 && prev & DECODED == 0;
        let mut state = (prev & FORMAT) | ASKED;
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
            state |= TAKEN;
        } else if retry {
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

/// The [`hw_choice`] state of a decoder set up by [`attach_hw_device`].
fn hw_state(dec: &ff::codec::context::Context) -> usize {
    // SAFETY: the wrapper owns a live context; only `opaque` is read, which
    // attach_hw_device and pick_hw_format set.
    unsafe { (*dec.as_ptr()).opaque.addr() }
}

/// Why a decoder set up by [`attach_hw_device`] turned down its device's
/// format at the latest `get_format` call, if it did.
pub(crate) fn hw_refusal(dec: &ff::codec::context::Context) -> Option<HwRefusal> {
    use hw_choice::*;
    let state = hw_state(dec);
    if state & ASKED == 0 || state & TAKEN != 0 {
        None
    } else if state & WITHDRAWN != 0 {
        Some(HwRefusal::Withdrawn)
    } else {
        Some(HwRefusal::NotOffered)
    }
}

/// Whether a decoder set up by [`attach_hw_device`] has asked for a pixel
/// format yet. One that decodes without asking never uses the device.
pub(crate) fn hw_format_asked(dec: &ff::codec::context::Context) -> bool {
    hw_state(dec) & hw_choice::ASKED != 0
}

/// Note that a decoder set up by [`attach_hw_device`] gave a frame on its
/// device: the hardware decoder FFmpeg set up last works. A later list
/// without the device's format is then a stream that changed format, not
/// FFmpeg withdrawing it.
pub(crate) fn note_device_frame(dec: &mut ff::codec::context::Context) {
    // SAFETY: the wrapper owns a live context; only `opaque` is changed,
    // which FFmpeg never reads.
    unsafe {
        let c = dec.as_mut_ptr();
        (*c).opaque = std::ptr::without_provenance_mut((*c).opaque.addr() | hw_choice::DECODED);
    }
}

/// Why [`open_video_decoder`] opened no decoder.
#[derive(Debug, Error)]
pub(crate) enum OpenDecoderError {
    /// FFmpeg has decoders for the codec, but none decodes on the device.
    #[error("no decoder for this codec decodes on the device")]
    NotOnDevice,
    #[error(transparent)]
    Ffmpeg(#[from] ff::Error),
}

/// Open the decoder for a codec context made from a video stream's
/// parameters. ffmpeg-next's `Decoder::video` would look the decoder up
/// from the codec id, and check the media type, as bindgen enums; a file
/// with a codec newer than our headers would make an invalid one.
///
/// For decoding on `dev`, the decoder is the first one for the codec that
/// can use such a device. FFmpeg's own choice may not: for AV1 it is
/// libdav1d, which only decodes in software.
pub(crate) fn open_video_decoder(
    ctx: ff::codec::context::Context,
    dev: Option<&HwDevice>,
) -> Result<ff::decoder::Video, OpenDecoderError> {
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
        return Err(ff::Error::InvalidData.into());
    }
    // SAFETY: accepts any id; NULL when no decoder for it is built in.
    let mut codec = unsafe { c::avcodec_find_decoder(id) };
    if codec.is_null() {
        return Err(ff::Error::DecoderNotFound.into());
    }
    if let Some(dev) = dev {
        codec = device_decoder(id, dev.device_type).ok_or(OpenDecoderError::NotOnDevice)?;
    }
    // SAFETY: registered codecs are static.
    let codec = unsafe { ff::Codec::wrap(codec) };
    // The media type is checked above, which is all `Opened::video` checks.
    Ok(ctx.decoder().open_as(codec).map(ff::decoder::Video)?)
}

/// The decoder for codec `id` that can decode on a device of type
/// `device_type`, chosen as avcodec_find_decoder chooses among all of
/// them: the first registered, unless it is experimental and another is
/// not.
fn device_decoder(id: c_int, device_type: c_int) -> Option<*const sys::AVCodec> {
    let mut iter: *mut c_void = std::ptr::null_mut();
    let mut experimental = None;
    loop {
        // SAFETY: `iter` is the state av_codec_iterate keeps, started at
        // NULL. It returns static codecs, then NULL.
        let codec = unsafe { sys::av_codec_iterate(&mut iter) };
        if codec.is_null() {
            return experimental;
        }
        // SAFETY: a static codec; `id` is an enum field, read as an int.
        let (decoder, codec_id, caps) = unsafe {
            (
                sys::av_codec_is_decoder(codec) != 0,
                enum_int(&raw const (*codec).id),
                (*codec).capabilities,
            )
        };
        if !decoder || codec_id != id {
            continue;
        }
        // SAFETY: registered codecs are static.
        if !decoder_device_types(&unsafe { ff::Codec::wrap(codec) }).contains(&device_type) {
            continue;
        }
        if caps & sys::AV_CODEC_CAP_EXPERIMENTAL as c_int == 0 {
            return Some(codec);
        }
        experimental.get_or_insert(codec);
    }
}

/// A frame's pixel format as FFmpeg's int, to compare and name it without
/// making a bindgen enum of it (as `ff::frame::Video::format` does).
pub fn frame_format(frame: &ff::frame::Video) -> c_int {
    // SAFETY: `frame` wraps a live AVFrame; `format` is a plain int field.
    unsafe { (*frame.as_ptr()).format }
}

/// Plane `index` of a frame in system memory, as `ff::frame::Video::data`
/// gives it, but sized from the pixel format read as an int (`data` calls
/// `Frame::format`), and never past the room the plane has: up to the end
/// of the buffer that holds it, or to the start of another of the frame's
/// planes in that buffer if one comes first. av_frame_get_buffer puts all
/// of a frame's planes in one buffer.
///
/// The slice holds every row of the frame's width and height: row `y`
/// starts `y * linesize` bytes in, and the bytes the width covers from
/// there are all in the slice. It has `linesize` bytes for each row, but
/// ends where the plane's room does if that comes first, so the last row
/// may be cut to the bytes the width covers. That happens for a frame the
/// decoder cropped on the left: its `data` moves right, and its last row
/// has no padding after it to cover the move.
///
/// `None` for a plane the format doesn't have, for a hardware frame, for
/// a format this libavutil doesn't describe, for rows wider than
/// `linesize`, and for rows that run past the plane's room, as they do in
/// most frames whose size or format was changed after their buffers were
/// allocated. The frame doesn't record the height its buffers were made
/// for, though, so one whose height was raised by a little still gives
/// `Some`: its extra rows are the plane's padding, never another plane.
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
    // A row wider than `linesize` runs into the next one, and the last row
    // would run past the slice, which has `linesize` bytes a row.
    if row > linesize {
        return None;
    }
    let needed = linesize.checked_mul(rows - 1)?.checked_add(row)?;
    let room = plane_room(f, data)?;
    if needed > room {
        return None;
    }
    // SAFETY: the slice starts at `data` and ends inside the buffer that
    // holds it (`room`), which the frame keeps a reference to. It lives as
    // long as the frame, which stays borrowed.
    Some(unsafe { std::slice::from_raw_parts(data, linesize.saturating_mul(rows).min(room)) })
}

/// How many bytes a plane of `frame` that starts at `p` can have: up to
/// the end of the buffer that holds `p`, or to the next of the frame's
/// data pointers in that buffer, since planes don't overlap. `None` when
/// no buffer of the frame holds `p`.
fn plane_room(frame: *const sys::AVFrame, p: *const u8) -> Option<usize> {
    let room = buffer_room(frame, p)?;
    // `buffer_room` found `p + room`, the buffer's end, without overflow.
    let (start, end) = (p.addr(), p.addr() + room);
    // SAFETY: `frame` is a live AVFrame; `data` is a plain array, and its
    // pointers are only compared.
    let data = unsafe { (*frame).data };
    Some(
        data.iter()
            .map(|d| d.addr())
            .filter(|d| start < *d && *d < end)
            .min()
            .map_or(room, |next| next - start),
    )
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

/// A line of FFmpeg's log while its calls come in.
#[derive(Debug)]
struct LogLine {
    /// The "[hevc @ 0x…] " of the call that started the line.
    prefix: String,
    text: String,
    /// The most severe level among its calls.
    level: c_int,
}

/// A line longer than this goes out without waiting for its end.
const MAX_LOG_LINE: usize = 4096;

thread_local! {
    /// The line FFmpeg is logging on this thread, until its end comes.
    /// FFmpeg may log one line in several calls, of which only the first
    /// names the context. Threads log independently, so each keeps its own
    /// and no line holds two threads' text.
    static PENDING: RefCell<Option<LogLine>> = const { RefCell::new(None) };
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

/// Send the lines of one call of FFmpeg's log to `tracing`, each whole
/// and with its context, as FFmpeg's own log prints them: a line ends at
/// "\n" or "\r", one call may hold several lines, and a line begun in one
/// call waits on this thread for the call that ends it. Lines after the
/// first of one call get the call's context too.
///
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
    let mut buf = [0 as c_char; 1024];
    // Only the message: the prefix is added to each line below.
    let mut print_prefix = 0;
    // SAFETY: `avcl`, `fmt` and `vl` are FFmpeg's own and `vl` is consumed
    // only here; `buf` is writable for the size passed, and FFmpeg
    // truncates to it and NUL-terminates.
    let n = unsafe {
        sys::av_log_format_line2(
            avcl,
            level,
            fmt,
            vl,
            buf.as_mut_ptr(),
            buf.len() as c_int,
            &mut print_prefix,
        )
    };
    if n < 0 {
        return;
    }
    // SAFETY: NUL-terminated within `buf` (above).
    let msg = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
    // SAFETY: `avcl` is the context FFmpeg passed with this message.
    let prefix = unsafe { log_prefix(avcl) };
    // Taken out of the cell before any event, so that a subscriber that
    // logs through FFmpeg finds the cell free.
    let lines = PENDING
        .try_with(|p| match p.try_borrow_mut() {
            Ok(mut p) => end_lines(&mut p, &prefix, plain, &msg),
            Err(_) => Vec::new(),
        })
        .unwrap_or_default();
    for (level, line) in lines {
        emit_log_line(level, &line);
    }
}

/// End the line FFmpeg left unfinished on this thread, if any, and send
/// it to `tracing` as it is.
pub(crate) fn end_log_line() {
    let line = PENDING
        .try_with(|p| p.try_borrow_mut().ok().and_then(|mut p| p.take()))
        .ok()
        .flatten();
    if let Some((level, line)) = line.and_then(finished) {
        emit_log_line(level, &line);
    }
}

fn emit_log_line(level: c_int, line: &str) {
    use tracing::Level;
    match tracing_level(level) {
        Level::ERROR => tracing::error!(target: "ffmpeg", "{line}"),
        Level::WARN => tracing::warn!(target: "ffmpeg", "{line}"),
        Level::INFO => tracing::info!(target: "ffmpeg", "{line}"),
        Level::DEBUG => tracing::debug!(target: "ffmpeg", "{line}"),
        _ => tracing::trace!(target: "ffmpeg", "{line}"),
    }
}

/// Add `msg`, the text of one call at `level` from the context `prefix`
/// names, to the line `pending` holds, and return the lines it ends with
/// their levels. What is left of `msg` after its last line end waits in
/// `pending`.
fn end_lines(
    pending: &mut Option<LogLine>,
    prefix: &str,
    level: c_int,
    msg: &str,
) -> Vec<(c_int, String)> {
    let mut out = Vec::new();
    let mut parts = msg.split(['\n', '\r']).peekable();
    while let Some(part) = parts.next() {
        let ended = parts.peek().is_some();
        if part.is_empty() && pending.is_none() {
            // An empty line, or nothing after the last line end.
            continue;
        }
        let line = pending.get_or_insert_with(|| LogLine {
            prefix: prefix.to_string(),
            text: String::new(),
            level,
        });
        line.text.push_str(part);
        line.level = line.level.min(level);
        if ended || line.text.len() >= MAX_LOG_LINE {
            out.extend(pending.take().and_then(finished));
        }
    }
    out
}

/// A line as it goes to `tracing`, or None for one with no text.
fn finished(line: LogLine) -> Option<(c_int, String)> {
    let text = line.text.trim_end();
    (!text.is_empty()).then(|| (line.level, format!("{}{text}", line.prefix)))
}

/// The "[hevc @ 0x…] " that FFmpeg's own log puts before a line from the
/// context `avcl`, after its parent's when it has one, made as
/// av_log_format_line2 makes it.
///
/// # Safety
///
/// `avcl` is NULL or a context that starts with its `AVClass` pointer, as
/// av_log requires of the contexts it is given.
unsafe fn log_prefix(avcl: *mut c_void) -> String {
    // SAFETY: the caller's guarantee.
    let Some(class) = (unsafe { context_class(avcl) }) else {
        return String::new();
    };
    // SAFETY: a context's class is a static AVClass; only this int field
    // is read, never the enum fields.
    let offset = unsafe { (*class).parent_log_context_offset };
    let mut prefix = String::new();
    if offset != 0 {
        // SAFETY: the class says that a pointer to the parent context, or
        // NULL, lies at this offset in the context, and FFmpeg reads it
        // the same way. The parent is a context like `avcl`.
        prefix = unsafe {
            let parent = avcl
                .byte_offset(offset as isize)
                .cast::<*mut c_void>()
                .read();
            context_tag(parent)
        };
    }
    // SAFETY: the caller's guarantee.
    prefix + &unsafe { context_tag(avcl) }
}

/// The class of a context, or None for a NULL context or class.
///
/// # Safety
///
/// As for [`log_prefix`].
unsafe fn context_class(ctx: *mut c_void) -> Option<*const sys::AVClass> {
    if ctx.is_null() {
        return None;
    }
    // SAFETY: the caller's guarantee: the context starts with its class.
    let class = unsafe { ctx.cast::<*const sys::AVClass>().read() };
    (!class.is_null()).then_some(class)
}

/// "[name @ 0x…] " for a context, or "" for a NULL one.
///
/// # Safety
///
/// As for [`log_prefix`].
unsafe fn context_tag(ctx: *mut c_void) -> String {
    // SAFETY: the caller's guarantee.
    let Some(class) = (unsafe { context_class(ctx) }) else {
        return String::new();
    };
    // SAFETY: a static AVClass. FFmpeg's log calls `item_name` on any
    // thread for every message; it returns a string that lives as long
    // as the context, copied out here.
    let name = unsafe {
        match (*class).item_name {
            Some(item_name) => cstr_opt(item_name(ctx)),
            None => cstr_opt((*class).class_name),
        }
    };
    format!("[{} @ {ctx:p}] ", name.unwrap_or_default())
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
        let ctx = wanting(vaapi);
        assert_eq!(hw_refusal(&ctx), None);
        assert!(!hw_format_asked(&ctx));

        // Offered, among formats our headers may lack: taken.
        let mut ctx = wanting(vaapi);
        assert_eq!(offer(&mut ctx, &[yuv, 100_000, vaapi]), vaapi);
        assert_eq!(hw_refusal(&ctx), None);
        assert!(hw_format_asked(&ctx));
        // Then withdrawn, as when the hardware decoder fails to start.
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::Withdrawn));
        // The latest call counts: a list that never had it, as after a
        // new SPS, isn't a withdrawal.
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::NotOffered));
        // Taken again, it clears.
        assert_eq!(offer(&mut ctx, &[vaapi, yuv]), vaapi);
        assert_eq!(hw_refusal(&ctx), None);

        // Taken, and frames decode on the device. Then the stream changes
        // to a format FFmpeg has no hardware decoder for: not offered, not
        // withdrawn.
        let mut ctx = wanting(vaapi);
        assert_eq!(offer(&mut ctx, &[vaapi, yuv]), vaapi);
        note_device_frame(&mut ctx);
        assert_eq!(hw_refusal(&ctx), None);
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::NotOffered));
        // A new format the driver can't decode is still withdrawn.
        assert_eq!(offer(&mut ctx, &[vaapi, yuv]), vaapi);
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::Withdrawn));

        // Never offered.
        let mut ctx = wanting(vaapi);
        assert_eq!(offer(&mut ctx, &[yuv]), none);
        assert_eq!(hw_refusal(&ctx), Some(HwRefusal::NotOffered));
    }

    #[test]
    fn a_device_gets_a_decoder_that_can_use_it() {
        use sys::{AVCodecID as Id, AVHWDeviceType as T};
        // FFmpeg's choice for AV1 is libdav1d, which never decodes on a
        // device. Where FFmpeg's own decoder can use the device, the choice
        // is that one, so AV1 is not refused at open when this FFmpeg has
        // Vulkan Video for it (hw_refusal.rs accepts either).
        for (name, id) in [
            ("h264", Id::AV_CODEC_ID_H264),
            ("hevc", Id::AV_CODEC_ID_HEVC),
            ("av1", Id::AV_CODEC_ID_AV1),
        ] {
            let own = ff::decoder::find_by_name(name);
            for t in [T::AV_HWDEVICE_TYPE_VAAPI, T::AV_HWDEVICE_TYPE_VULKAN] {
                let t = t as c_int;
                let chosen = device_decoder(id as c_int, t);
                if let Some(c) = chosen {
                    // SAFETY: registered codecs are static.
                    let c = unsafe { ff::Codec::wrap(c) };
                    assert!(
                        decoder_device_types(&c).contains(&t),
                        "{name}: {}",
                        c.name()
                    );
                }
                if let Some(own) = own.filter(|c| decoder_device_types(c).contains(&t)) {
                    // SAFETY: the pointer is only compared.
                    assert_eq!(chosen, Some(unsafe { own.as_ptr() }), "{name} on {t}");
                }
            }
        }
        // No FFmpeg has Vulkan Video for MPEG-4 Part 2.
        let vulkan = T::AV_HWDEVICE_TYPE_VULKAN as c_int;
        assert!(device_decoder(Id::AV_CODEC_ID_MPEG4 as c_int, vulkan).is_none());
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
        // Rows wider than `linesize` fit the buffer, but would overlap.
        f.set_width(100);
        assert!(plane(&f, 0).is_none());
        f.set_width(64);
        f.set_format(Pixel::YUV420P10LE);
        assert!(plane(&f, 0).is_none());
        f.set_format(Pixel::NV12);
        assert!(plane(&f, 1).is_none());
    }

    #[test]
    fn a_plane_never_reaches_into_the_next() {
        use ff::format::Pixel;
        // av_frame_get_buffer puts all three planes in one buffer, so the
        // buffer's bounds alone would let a plane run on into the next.
        let mut f = ff::frame::Video::new(Pixel::YUV420P, 64, 16);
        let starts = [0, 1, 2].map(|i| f.data(i).as_ptr());
        for h in 16..=64 {
            f.set_height(h);
            for i in 0..3 {
                let Some(p) = plane(&f, i) else { continue };
                let span = p.as_ptr_range();
                assert!(
                    !starts.iter().any(|s| span.start < *s && *s < span.end),
                    "plane {i} at height {h}"
                );
            }
        }
    }

    #[test]
    fn log_lines_are_put_together_from_their_calls() {
        let (w, e) = (sys::AV_LOG_WARNING, sys::AV_LOG_ERROR);
        let mut pending = None;
        let mut add = |prefix: &str, level, msg: &str| end_lines(&mut pending, prefix, level, msg);
        // A line in two calls keeps the first call's context, and comes
        // out at the more severe level.
        assert!(add("[aac @ 0x1] ", w, "Audio object type 0").is_empty());
        assert_eq!(
            add("[mov @ 0x2] ", e, " is not implemented.\n"),
            [(
                e,
                "[aac @ 0x1] Audio object type 0 is not implemented.".into()
            )]
        );
        // Several lines in one call each get its context; empty lines and
        // trailing spaces are dropped, and "\r" ends a line too.
        assert_eq!(
            add("[mov @ 0x2] ", w, "one  \n\ntwo\rthree"),
            [
                (w, "[mov @ 0x2] one".to_string()),
                (w, "[mov @ 0x2] two".to_string())
            ]
        );
        assert_eq!(add("", w, "\n"), [(w, "[mov @ 0x2] three".to_string())]);
        // A line that never ends still goes out once it is long.
        let long = "x".repeat(MAX_LOG_LINE / 2);
        assert!(add("", w, &long).is_empty());
        let out = add("", w, &long);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.len(), MAX_LOG_LINE);
        assert!(pending.is_none());
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

        // Several threads at once, each logging its lines in two calls:
        // every line arrives once and whole, with no other thread's text.
        std::thread::scope(|s| {
            for t in 0..8 {
                s.spawn(move || {
                    for i in 0..200 {
                        log(sys::AV_LOG_WARNING, &format!("thread-test {t}/{i} "));
                        log(sys::AV_LOG_WARNING, "end\n");
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

        // A line logged in several calls, as FFmpeg logs a feature it lacks,
        // arrives once and whole, with the context of the call that started
        // it and the most severe level among its calls. A call that holds
        // several lines gives each of them its context.
        // SAFETY: allocates a context to log with, freed below.
        let fctx = unsafe { sys::avformat_alloc_context() };
        assert!(!fctx.is_null());
        for (level, part) in [
            (sys::AV_LOG_INFO, c"prefix-test Audio object type 0"),
            (sys::AV_LOG_WARNING, c" is not implemented."),
            (sys::AV_LOG_INFO, c" Update your FFmpeg.\n"),
            (sys::AV_LOG_ERROR, c"prefix-test one\nprefix-test two\r\n\n"),
            (sys::AV_LOG_ERROR, c"prefix-test cut short"),
        ] {
            // SAFETY: `fctx` starts with its AVClass, as av_log requires;
            // the format has no conversions.
            unsafe { sys::av_log(fctx.cast(), level, part.as_ptr()) };
        }
        // The last line has no end yet, until a capture ends it.
        assert_eq!(captured("prefix-test").len(), 3);
        end_log_line();
        // SAFETY: frees the context allocated above, which nothing else
        // holds.
        unsafe { sys::avformat_free_context(fctx) };
        let at = format!("[AVFormatContext @ {fctx:p}] ");
        assert_eq!(
            captured("prefix-test"),
            [
                (
                    Level::WARN,
                    format!(
                        "{at}prefix-test Audio object type 0 is not implemented. Update your FFmpeg."
                    )
                ),
                (Level::ERROR, format!("{at}prefix-test one")),
                (Level::ERROR, format!("{at}prefix-test two")),
                (Level::ERROR, format!("{at}prefix-test cut short")),
            ]
        );

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
