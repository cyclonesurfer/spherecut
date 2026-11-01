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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;
use spherecut_core::video::{ColorCodes, ColorFamily, PixelLayout, PlaneDesc, SampleBytes};
use thiserror::Error;

mod vulkan;
pub(crate) use vulkan::{VkFrame, upload as upload_to_vulkan};

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
        pub fn av_image_fill_plane_sizes(
            sizes: *mut usize,
            fmt: c_int,
            height: c_int,
            linesizes: *const isize,
        ) -> c_int;
        pub fn av_pix_fmt_count_planes(fmt: c_int) -> c_int;
        pub fn av_get_sample_fmt_name(fmt: c_int) -> *const c_char;
        pub fn av_color_range_name(range: c_int) -> *const c_char;
        pub fn av_color_space_name(space: c_int) -> *const c_char;
        pub fn av_color_primaries_name(primaries: c_int) -> *const c_char;
        pub fn av_color_transfer_name(transfer: c_int) -> *const c_char;
        pub fn av_chroma_location_name(location: c_int) -> *const c_char;
        pub fn av_hwdevice_iterate_types(prev: c_int) -> c_int;
        pub fn sws_isSupportedInput(fmt: c_int) -> c_int;
        pub fn sws_isSupportedOutput(fmt: c_int) -> c_int;
        pub fn sws_getContext(
            src_w: c_int,
            src_h: c_int,
            src_format: c_int,
            dst_w: c_int,
            dst_h: c_int,
            dst_format: c_int,
            flags: c_int,
            src_filter: *mut ffmpeg_sys_next::SwsFilter,
            dst_filter: *mut ffmpeg_sys_next::SwsFilter,
            param: *const f64,
        ) -> *mut ffmpeg_sys_next::SwsContext;
        pub fn av_hwdevice_find_type_by_name(name: *const c_char) -> c_int;
        pub fn av_hwdevice_get_type_name(kind: c_int) -> *const c_char;
        pub fn av_hwdevice_ctx_alloc(kind: c_int) -> *mut AVBufferRef;
        pub fn av_get_pix_fmt(name: *const c_char) -> c_int;
        pub fn av_hwdevice_ctx_create(
            device_ctx: *mut *mut AVBufferRef,
            kind: c_int,
            device: *const c_char,
            opts: *mut AVDictionary,
            flags: c_int,
        ) -> c_int;
        pub fn av_sample_fmt_is_planar(fmt: c_int) -> c_int;
        pub fn av_get_bytes_per_sample(fmt: c_int) -> c_int;
        pub fn swr_alloc_set_opts2(
            ps: *mut *mut ffmpeg_sys_next::SwrContext,
            out_ch_layout: *const ffmpeg_sys_next::AVChannelLayout,
            out_sample_fmt: c_int,
            out_sample_rate: c_int,
            in_ch_layout: *const ffmpeg_sys_next::AVChannelLayout,
            in_sample_fmt: c_int,
            in_sample_rate: c_int,
            log_offset: c_int,
            log_ctx: *mut std::ffi::c_void,
        ) -> c_int;
        // FFmpeg 7.0 made both arrays `* const`, which changes bindgen's
        // types but not the ABI.
        pub fn swr_convert(
            s: *mut ffmpeg_sys_next::SwrContext,
            out: *const *mut u8,
            out_count: c_int,
            in_: *const *const u8,
            in_count: c_int,
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

/// Tag an unopened encoder's stream with `tags`: range, matrix,
/// primaries, transfer and chroma location, written as the ints they are.
/// ffmpeg-next has no setter for the chroma location, and its others take
/// its own enums, which can't hold a value our headers lack.
pub(crate) fn set_color_tags(enc: &mut ff::encoder::video::Video, tags: ColorCodes) {
    // SAFETY: the encoder's context is live and not yet opened, when these
    // fields may be set. Each is a C enum, which is int-sized, and FFmpeg
    // accepts any value in it (an unknown one is written as coded).
    unsafe {
        let c = enc.as_mut_ptr();
        (&raw mut (*c).color_range)
            .cast::<c_int>()
            .write(tags.range);
        (&raw mut (*c).colorspace)
            .cast::<c_int>()
            .write(tags.matrix);
        (&raw mut (*c).color_primaries)
            .cast::<c_int>()
            .write(tags.primaries);
        (&raw mut (*c).color_trc)
            .cast::<c_int>()
            .write(tags.transfer);
        (&raw mut (*c).chroma_sample_location)
            .cast::<c_int>()
            .write(tags.chroma_location);
    }
}

/// Clear an output stream's codec tag, so that the muxer picks its own.
pub(crate) fn clear_codec_tag(st: &mut ff::format::stream::StreamMut<'_>) {
    // SAFETY: the stream belongs to a live output context that is
    // mutably borrowed with it, and `codec_tag` is a plain integer.
    unsafe { (*(*st.as_mut_ptr()).codecpar).codec_tag = 0 };
}

/// Set an output stream's codec tag, a four-character code with its first
/// character in the low byte, for a muxer that takes more than one for
/// the codec.
pub(crate) fn set_codec_tag(st: &mut ff::format::stream::StreamMut<'_>, tag: u32) {
    // SAFETY: as in `clear_codec_tag`.
    unsafe { (*(*st.as_mut_ptr()).codecpar).codec_tag = tag };
}

/// FFmpeg's h264_metadata or hevc_metadata bitstream filter, set to crop
/// the pictures of an encoder's stream on the left: it rewrites H.264's
/// frame cropping or HEVC's conformance window in every SPS, in the
/// extradata and in packets alike. No encoder crops on the left itself;
/// test media does, as some streams do.
pub(crate) struct CropFilter(*mut sys::AVBSFContext);

// SAFETY: the filter context is only used through `&mut self` (or read
// through `&self`), and a bitstream filter keeps no state outside it.
unsafe impl Send for CropFilter {}

impl Drop for CropFilter {
    fn drop(&mut self) {
        // SAFETY: NULL or the context this owns.
        unsafe { sys::av_bsf_free(&mut self.0) };
    }
}

impl CropFilter {
    /// A filter for packets of the H.264 or HEVC stream with parameters
    /// `par`, in time base `tb`, that crops `left` more luma columns on
    /// the left, an even number.
    pub(crate) fn left(
        par: &ff::codec::Parameters,
        tb: ff::Rational,
        left: u32,
    ) -> Result<Self, ff::Error> {
        let check = |ret: c_int| {
            if ret < 0 {
                Err(ff::Error::from(ret))
            } else {
                Ok(())
            }
        };
        // SAFETY: `par` wraps live codec parameters; the id is read as an
        // int.
        let id = unsafe { enum_int(&raw const (*par.as_ptr()).codec_id) };
        let name = match id {
            i if i == sys::AVCodecID::AV_CODEC_ID_H264 as c_int => c"h264_metadata",
            i if i == sys::AVCodecID::AV_CODEC_ID_HEVC as c_int => c"hevc_metadata",
            _ => return Err(ff::Error::BsfNotFound),
        };
        // SAFETY: a static name; the filter found is static too.
        let filter = unsafe { sys::av_bsf_get_by_name(name.as_ptr()) };
        if filter.is_null() {
            return Err(ff::Error::BsfNotFound);
        }
        let mut me = CropFilter(std::ptr::null_mut());
        // SAFETY: allocates a context into `me.0`, which frees it on any
        // error below. The context is set up as av_bsf_init requires
        // before it is called: its input parameters copied from `par`, its
        // input time base, and an option the filter has, with a value in
        // its range.
        unsafe {
            check(sys::av_bsf_alloc(filter, &mut me.0))?;
            check(sys::avcodec_parameters_copy((*me.0).par_in, par.as_ptr()))?;
            (*me.0).time_base_in = tb.into();
            let left = i64::from(left);
            check(sys::av_opt_set_int(
                (*me.0).priv_data,
                c"crop_left".as_ptr(),
                left,
                0,
            ))?;
            check(sys::av_bsf_init(me.0))?;
        }
        Ok(me)
    }

    /// Give `stream` the filter's output parameters, whose SPS crops
    /// `left` columns more, and the width of the picture they leave.
    pub(crate) fn parameters_to(
        &self,
        stream: &mut ff::format::stream::StreamMut<'_>,
        left: u32,
    ) -> Result<(), ff::Error> {
        // SAFETY: the stream belongs to a live output context that is
        // mutably borrowed with it, and its codecpar is allocated; the
        // filter's output parameters are set by av_bsf_init. The width is
        // a plain int.
        unsafe {
            let par = (*stream.as_mut_ptr()).codecpar;
            let ret = sys::avcodec_parameters_copy(par, (*self.0).par_out);
            if ret < 0 {
                return Err(ff::Error::from(ret));
            }
            let left = c_int::try_from(left).map_err(|_| ff::Error::InvalidData)?;
            (*par).width = (*par).width.saturating_sub(left);
        }
        Ok(())
    }

    /// Send `packet` through the filter, which takes its data, or with
    /// `None` the end of the stream.
    pub(crate) fn send(&mut self, packet: Option<&mut ff::Packet>) -> Result<(), ff::Error> {
        use ff::codec::packet::Mut;
        let p = packet.map_or(std::ptr::null_mut(), |p| p.as_mut_ptr());
        // SAFETY: a context set up in `left`, and NULL or a live packet,
        // whose reference the filter takes, leaving it blank.
        let ret = unsafe { sys::av_bsf_send_packet(self.0, p) };
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
        Ok(())
    }

    /// The next packet out of the filter into `packet`: EAGAIN when it
    /// needs another, EOF once the end of the stream is out.
    pub(crate) fn receive(&mut self, packet: &mut ff::Packet) -> Result<(), ff::Error> {
        use ff::codec::packet::Mut;
        // SAFETY: a context set up in `left`, and a live packet, which is
        // given the filtered packet's reference.
        let ret = unsafe { sys::av_bsf_receive_packet(self.0, packet.as_mut_ptr()) };
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
        Ok(())
    }
}

/// Planar 32-bit float, the sample format `Mp4Writer` sends an encoder.
pub(crate) const SAMPLE_FMT_FLTP: c_int = sys::AVSampleFormat::AV_SAMPLE_FMT_FLTP as c_int;
/// Packed 16- and 32-bit integers, which it sends a PCM encoder.
pub(crate) const SAMPLE_FMT_S16: c_int = sys::AVSampleFormat::AV_SAMPLE_FMT_S16 as c_int;
pub(crate) const SAMPLE_FMT_S32: c_int = sys::AVSampleFormat::AV_SAMPLE_FMT_S32 as c_int;

/// `FF_COMPLIANCE_NORMAL`, which FFmpeg's headers define as 0 in every
/// version.
const COMPLIANCE_NORMAL: c_int = 0;

/// Whether FFmpeg's MP4 muxer stores codec `id` (`avformat_query_codec`):
/// PCM, for one, only from 6.1, as ISO `ipcm` and `fpcm`.
pub(crate) fn mp4_takes(id: ff::codec::Id) -> bool {
    // SAFETY: av_guess_format returns one of FFmpeg's static muxers or
    // NULL, and avformat_query_codec only reads it.
    unsafe {
        let mp4 = sys::av_guess_format(c"mp4".as_ptr(), std::ptr::null(), std::ptr::null());
        !mp4.is_null() && sys::avformat_query_codec(mp4, id.into(), COMPLIANCE_NORMAL) == 1
    }
}

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
const PICT_I: c_int = sys::AVPictureType::AV_PICTURE_TYPE_I as c_int;

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
    /// FFmpeg's spherical mapping side data, as its demuxer read it: how
    /// a Matroska or WebM file's projection reaches us.
    pub spherical: Option<FfSpherical>,
}

/// `AVSphericalMapping` and `AVStereo3D`'s leading fields, as numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfSpherical {
    /// `AV_SPHERICAL_*`: 0 equirectangular, 1 cubemap, 2 equirectangular
    /// with bounds (a tile), others newer.
    pub projection: c_int,
    /// Yaw, pitch and roll in 16.16 fixed-point degrees.
    pub pose: [i32; 3],
    /// Left, top, right, bottom: 0.32 fixed-point crops for a tile.
    pub bounds: [u32; 4],
    pub padding: u32,
    /// `AV_STEREO3D_*` and its flags, when the stream has that side data.
    pub stereo: Option<(c_int, c_int)>,
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
        f.spherical = spherical(par);
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

/// The decoder FFmpeg would pick for a stream's codec, or None when this
/// build has none. The codec id is read as an int, so a codec our headers
/// lack is looked up too.
pub fn find_decoder(p: &ff::codec::Parameters) -> Option<ff::Codec> {
    // SAFETY: `p` borrows live codec parameters; the enum field is read as
    // an int.
    let id = unsafe { enum_int(&raw const (*p.as_ptr()).codec_id) };
    // SAFETY: accepts any id; NULL when no decoder for it is built in.
    let codec = unsafe { c::avcodec_find_decoder(id) };
    // SAFETY: registered codecs are static.
    (!codec.is_null()).then(|| unsafe { ff::Codec::wrap(codec) })
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

/// A stream's extradata (for H.264 and HEVC in MP4 or Matroska, its
/// `avcC` or `hvcC`); empty when it has none.
pub(crate) fn extradata(p: &ff::codec::Parameters) -> Vec<u8> {
    // SAFETY: `p` borrows live codec parameters; plain fields.
    let (data, size) = unsafe { ((*p.as_ptr()).extradata, (*p.as_ptr()).extradata_size) };
    let n = usize::try_from(size).unwrap_or(0);
    if n == 0 || data.is_null() {
        return Vec::new();
    }
    // SAFETY: a non-NULL `extradata` holds `extradata_size` bytes, owned
    // by the parameters `p` borrows; they are copied out.
    unsafe { std::slice::from_raw_parts(data, n) }.to_vec()
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

/// `par` points at live codec parameters. A copy of its side data of type
/// `t`, when it has some.
fn side_data(par: *const sys::AVCodecParameters, t: sys::AVPacketSideDataType) -> Option<Vec<u8>> {
    // SAFETY: coded_side_data/nb_coded_side_data describe an array owned by
    // `par`; av_packet_side_data_get only reads it.
    let sd = unsafe {
        sys::av_packet_side_data_get((*par).coded_side_data, (*par).nb_coded_side_data, t)
    };
    if sd.is_null() {
        return None;
    }
    // SAFETY: non-NULL entry of the array above; only its plain fields are
    // read.
    let (size, data) = unsafe { ((*sd).size, (*sd).data) };
    if data.is_null() {
        return None;
    }
    // SAFETY: `data` holds `size` bytes, owned by `par`; they are copied
    // out.
    Some(unsafe { std::slice::from_raw_parts(data, size) }.to_vec())
}

/// The first `n` native-endian 32-bit words of `bytes`, if it has them.
fn words(bytes: &[u8], n: usize) -> Option<Vec<u32>> {
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .take(n)
        .map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]]))
        .collect();
    (words.len() == n).then_some(words)
}

/// `par` points at live codec parameters. Read by offset rather than as
/// bindgen's structs, whose enum fields may not hold a newer FFmpeg's
/// values: the fields read have kept their places since FFmpeg 3.3.
fn spherical(par: *const sys::AVCodecParameters) -> Option<FfSpherical> {
    // projection, yaw, pitch, roll, bound_left, bound_top, bound_right,
    // bound_bottom, padding: nine 32-bit fields.
    let m = words(
        &side_data(par, sys::AVPacketSideDataType::AV_PKT_DATA_SPHERICAL)?,
        9,
    )?;
    // type, flags.
    let stereo = side_data(par, sys::AVPacketSideDataType::AV_PKT_DATA_STEREO3D)
        .and_then(|b| words(&b, 2))
        .map(|w| (w[0] as c_int, w[1] as c_int));
    Some(FfSpherical {
        projection: m[0] as c_int,
        pose: [m[1] as i32, m[2] as i32, m[3] as i32],
        bounds: [m[4], m[5], m[6], m[7]],
        padding: m[8],
        stereo,
    })
}

/// `par` points at live codec parameters.
fn display_rotation(par: *const sys::AVCodecParameters) -> Option<f64> {
    let m = words(
        &side_data(par, sys::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX)?,
        9,
    )?;
    // SAFETY: a display matrix is nine int32 values, as `m` holds.
    let r = unsafe { sys::av_display_rotation_get(m.as_ptr().cast::<i32>()) };
    r.is_finite().then_some(r)
}

/// Hardware device types compiled into this FFmpeg.
pub fn hw_device_types() -> Vec<String> {
    hw_device_type_ids()
        .into_iter()
        // SAFETY: accepts any type value.
        .filter_map(|t| cstr_opt(unsafe { c::av_hwdevice_get_type_name(t) }))
        .collect()
}

/// [`hw_device_types`] as FFmpeg's device type values.
fn hw_device_type_ids() -> Vec<c_int> {
    let mut out = Vec::new();
    let mut t = HW_NONE;
    loop {
        // SAFETY: iterating from NONE is the documented way to list types.
        t = unsafe { c::av_hwdevice_iterate_types(t) };
        if t == HW_NONE {
            break;
        }
        out.push(t);
    }
    out
}

/// Why a hardware device could not be opened or attached to a decoder.
#[derive(Debug, Clone, Error)]
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
    /// FFmpeg's Vulkan devices could not be listed, or FFmpeg could not
    /// open the one chosen or opened another (`decode::open_vulkan_device`).
    #[error("{0}")]
    Vulkan(String),
    /// No device of the kind on the render node hardware decoding uses
    /// (`backend::FfmpegBackend::device`): the node, and why.
    #[error("{node}: {reason}")]
    NoDevice { node: String, reason: String },
}

/// FFmpeg's device type for a name such as "vaapi", when this FFmpeg was
/// built with it.
fn device_type(kind: &str) -> Result<c_int, HwError> {
    let kind_c = CString::new(kind).map_err(|_| HwError::Nul {
        what: "device type",
        name: kind.into(),
    })?;
    // SAFETY: valid NUL-terminated string.
    let t = unsafe { c::av_hwdevice_find_type_by_name(kind_c.as_ptr()) };
    built_in(kind, t, &hw_device_type_ids())
}

/// `t`, the type FFmpeg names `kind`, if it is among the types `built`
/// into this FFmpeg. FFmpeg knows every type's name, built in or not, and
/// fails to create a device of one it lacks with ENOMEM ("Cannot allocate
/// memory"), as doctor knows (`doctor::open_device`).
fn built_in(kind: &str, t: c_int, built: &[c_int]) -> Result<c_int, HwError> {
    if t == HW_NONE || !built.contains(&t) {
        return Err(HwError::NotBuiltIn(
            crate::hwprobe::device_label(kind).into(),
        ));
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

/// The demuxers FFmpeg may read a file with, by FFmpeg's names for them:
/// the MOV/MP4 family and Matroska/WebM, which hold every camera format
/// Spherecut reads. FFmpeg 6.1 to 9.0 name them so. Playlists, concat
/// scripts and the like are read by demuxers that open the files they
/// list, so a file FFmpeg would read with any other demuxer is refused
/// before that demuxer runs. Widening this list takes a DECISIONS entry
/// (D24).
pub(crate) const DEMUXERS: [&str; 2] = [MOV_DEMUXER, "matroska,webm"];

/// FFmpeg's name for its MOV/MP4 demuxer.
const MOV_DEMUXER: &str = "mov,mp4,m4a,3gp,3g2,mj2";

/// The containers of [`DEMUXERS`], as errors name them.
pub const CONTAINERS: &str = "MP4/MOV and Matroska/WebM";

/// Why [`open_input`] opened nothing.
#[derive(Debug, Error)]
pub enum OpenError {
    #[error(transparent)]
    Io(std::io::Error),
    #[error("not a regular file")]
    NotAFile,
    /// FFmpeg would read the file with a demuxer outside [`DEMUXERS`].
    #[error("FFmpeg reads it as {description} (`{format}`)")]
    Container { format: String, description: String },
    /// The demuxer asked to open another file or URL, which was refused.
    #[error(
        "FFmpeg tried to open `{url}` from inside it, which is refused: Spherecut lets FFmpeg read only the file it was given"
    )]
    Nested { url: String },
    #[error(transparent)]
    Ffmpeg(ff::Error),
}

/// Open a file for demuxing and read its stream info. Every file
/// Spherecut demuxes is opened here, with FFmpeg confined to that one
/// file (D24):
///
/// - `path` must be a regular file. FFmpeg gets `file:` and the path's
///   bytes, so its file protocol opens exactly that path: a name such as
///   `concat:a.mp4` or `http://host/x.mp4` is never read as a URL. (The
///   bytes go as they are because ffmpeg-next's own functions panic on a
///   path that is not UTF-8, and Linux file names need not be.)
/// - The demuxer is the one FFmpeg picks from the file's first bytes, as
///   `avformat_open_input` would, and must be one of [`DEMUXERS`];
///   otherwise the error names the container.
/// - The format context's `protocol_whitelist` is `file` and its
///   `format_whitelist` is [`DEMUXERS`], and its `io_open` refuses every
///   open but the file's own ([`open_only_the_file`]). The MOV demuxer's
///   `enable_drefs` is 0, so it does not follow references to external
///   data.
/// - The decoders `avformat_find_stream_info` opens get [`MAX_PIXELS`].
///
/// With `cancel`, FFmpeg stops reading once it is set.
pub fn open_input(
    path: &Path,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<ff::format::context::Input, OpenError> {
    open_input_with(path, cancel, &DEMUXERS, MAX_PIXELS)
}

/// [`open_input`] with other demuxers and another picture size limit, so
/// that tests can show what the confinement does with a demuxer that
/// opens other files.
fn open_input_with(
    path: &Path,
    cancel: Option<Arc<AtomicBool>>,
    demuxers: &[&str],
    max_pixels: i64,
) -> Result<ff::format::context::Input, OpenError> {
    spherecut_core::may_block("opening a media file");
    let meta = std::fs::metadata(path).map_err(OpenError::Io)?;
    if !meta.is_file() {
        return Err(OpenError::NotAFile);
    }
    let url = file_url(path).map_err(OpenError::Ffmpeg)?;
    // The guard frees the closure when dropped, which is after FFmpeg has
    // freed the context on every path below.
    let interrupt = cancel
        .map(|cancel| ff::util::interrupt::new(Box::new(move || cancel.load(Ordering::Relaxed))));
    let int_cb = interrupt
        .as_ref()
        .map_or(std::ptr::null(), |i| &raw const i.interrupt);
    // SAFETY: `int_cb` is NULL or the callback in `interrupt`, which lives
    // to the end of this function.
    let fmt = unsafe { identify(&url, int_cb) }.map_err(OpenError::Ffmpeg)?;
    // SAFETY: demuxers are static, and so are their names.
    let (name, long_name) = unsafe { (cstr_opt((*fmt).name), cstr_opt((*fmt).long_name)) };
    let name = name.unwrap_or_default();
    if !demuxers.contains(&name.as_str()) {
        return Err(OpenError::Container {
            description: long_name.unwrap_or_else(|| name.clone()),
            format: name,
        });
    }

    // SAFETY: allocates a context, which we own until it is handed to
    // avformat_open_input below.
    let mut ps = unsafe { sys::avformat_alloc_context() };
    if ps.is_null() {
        return Err(OpenError::Ffmpeg(ff::Error::Other {
            errno: ff::util::error::ENOMEM,
        }));
    }
    // SAFETY: `ps` is live and ours. FFmpeg calls the interrupt callback
    // with its opaque, the closure, which `interrupt` keeps alive. Every
    // context starts with FFmpeg's own io_open, kept for the one open
    // open_only_the_file lets through.
    unsafe {
        if let Some(i) = &interrupt {
            (*ps).interrupt_callback = i.interrupt;
        }
        if let Some(own) = (*ps).io_open {
            FFMPEG_IO_OPEN.get_or_init(|| own);
        }
        (*ps).io_open = Some(open_only_the_file);
    }
    let mut opts = ff::Dictionary::new();
    opts.set("protocol_whitelist", "file");
    opts.set("format_whitelist", &demuxers.join(","));
    if name == MOV_DEMUXER {
        opts.set("enable_drefs", "0");
    }
    let watch = RefusalWatch::start();
    // SAFETY: we own the dictionary until it comes back below.
    let mut opts = unsafe { opts.disown() };
    // SAFETY: `ps` is a context from avformat_alloc_context, `url` a
    // NUL-terminated string that outlives the call, `fmt` a static demuxer
    // and `opts` a dictionary FFmpeg may change. On failure FFmpeg frees
    // the context.
    let ret = unsafe { sys::avformat_open_input(&mut ps, url.as_ptr(), fmt, &mut opts) };
    // SAFETY: what FFmpeg left of the options, ours again and freed here.
    drop(unsafe { ff::Dictionary::own(opts) });
    if ret < 0 {
        return Err(watch
            .finish()
            .unwrap_or(OpenError::Ffmpeg(ff::Error::from(ret))));
    }
    // One dictionary per stream, for the decoders FFmpeg opens to find
    // the streams' parameters.
    // SAFETY: an open input context.
    let streams = unsafe { (*ps).nb_streams } as usize;
    // The frame size the demuxer read for each stream from the container.
    // SAFETY: an open input context's `streams` holds `nb_streams` live
    // streams, each with live codec parameters; width and height are
    // plain ints.
    let declared: Vec<(c_int, c_int)> = unsafe {
        (0..streams)
            .map(|i| {
                let par = (**(*ps).streams.add(i)).codecpar;
                ((*par).width, (*par).height)
            })
            .collect()
    };
    let mut decoder_opts: Vec<*mut sys::AVDictionary> = (0..streams)
        .map(|_| {
            let mut d = ff::Dictionary::new();
            d.set("max_pixels", &max_pixels.to_string());
            // SAFETY: owned by `decoder_opts` until it is taken back below.
            unsafe { d.disown() }
        })
        .collect();
    // SAFETY: an open input context and, when it has streams, an array of
    // one dictionary per stream, as FFmpeg asks.
    let ret = unsafe {
        sys::avformat_find_stream_info(
            ps,
            if streams == 0 {
                std::ptr::null_mut()
            } else {
                decoder_opts.as_mut_ptr()
            },
        )
    };
    for d in decoder_opts {
        // SAFETY: each is one of the dictionaries made above, which FFmpeg
        // may have changed but not freed.
        drop(unsafe { ff::Dictionary::own(d) });
    }
    let refused = watch.finish();
    if ret < 0 || refused.is_some() {
        // SAFETY: closes and frees the context opened above, which nothing
        // else holds.
        unsafe { sys::avformat_close_input(&mut ps) };
        return Err(refused.unwrap_or(OpenError::Ffmpeg(ff::Error::from(ret))));
    }
    // A decoder that refuses a picture over `max_pixels` leaves its stream
    // at 0x0, as if the file gave no size ("Ignoring invalid width/height
    // values"), unless a parser sets it again from the bitstream, as
    // H.264's does. The container's size is put back, so that callers can
    // say the picture is too large (`TooLarge`) rather than missing.
    for (i, &(width, height)) in declared.iter().enumerate() {
        let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
            continue;
        };
        if !over_max_pixels(w, h, max_pixels) {
            continue;
        }
        // SAFETY: the context is still open, and avformat_find_stream_info
        // only adds streams, so stream `i` is one of those read above;
        // width and height are plain ints.
        unsafe {
            let par = (**(*ps).streams.add(i)).codecpar;
            if (*par).width == 0 && (*par).height == 0 {
                (*par).width = width;
                (*par).height = height;
            }
        }
    }
    tracing::debug!(path = %path.display(), demuxer = name, "opened for demuxing");
    // SAFETY: an open input context, which the wrapper now owns: it closes
    // the context when dropped, and only then drops the guard.
    Ok(unsafe {
        match interrupt {
            Some(i) => ff::format::context::Input::wrap_with_interrupt(ps, i.guard),
            None => ff::format::context::Input::wrap(ps),
        }
    })
}

/// `file:` and the bytes of `path`. FFmpeg's file protocol takes the rest
/// of the URL as the path, as it is.
fn file_url(path: &Path) -> Result<CString, ff::Error> {
    let bytes = [b"file:", path.as_os_str().as_bytes()].concat();
    CString::new(bytes).map_err(|_| ff::Error::Other {
        errno: ff::util::error::EINVAL,
    })
}

/// The demuxer FFmpeg picks for the file at `url`, from its first bytes,
/// as `avformat_open_input` picks one (it calls the same function).
///
/// # Safety
///
/// `int_cb` is NULL or points at an interrupt callback that stays valid
/// during the call.
unsafe fn identify(
    url: &CStr,
    int_cb: *const sys::AVIOInterruptCB,
) -> Result<*const sys::AVInputFormat, ff::Error> {
    let mut opts = ff::Dictionary::new();
    opts.set("protocol_whitelist", "file");
    // SAFETY: we own the dictionary until it comes back below.
    let mut opts = unsafe { opts.disown() };
    let mut pb = std::ptr::null_mut();
    // SAFETY: `url` is NUL-terminated, `int_cb` is as the caller says, and
    // `opts` is a dictionary FFmpeg may change.
    let ret = unsafe {
        sys::avio_open2(
            &mut pb,
            url.as_ptr(),
            sys::AVIO_FLAG_READ,
            int_cb,
            &mut opts,
        )
    };
    // SAFETY: what FFmpeg left of the options, ours again and freed here.
    drop(unsafe { ff::Dictionary::own(opts) });
    if ret < 0 {
        return Err(ff::Error::from(ret));
    }
    let mut fmt = std::ptr::null();
    // SAFETY: `pb` is the context opened above; FFmpeg reads up to its
    // default probe size from it. A NULL log context is allowed.
    let ret = unsafe {
        sys::av_probe_input_buffer2(pb, &mut fmt, url.as_ptr(), std::ptr::null_mut(), 0, 0)
    };
    // SAFETY: closes and frees the context avio_open2 opened.
    unsafe { sys::avio_closep(&mut pb) };
    if ret < 0 {
        Err(ff::Error::from(ret))
    } else if fmt.is_null() {
        Err(ff::Error::InvalidData)
    } else {
        Ok(fmt)
    }
}

/// `AVFormatContext.io_open`, as FFmpeg declares it.
type IoOpen = unsafe extern "C" fn(
    *mut sys::AVFormatContext,
    *mut *mut sys::AVIOContext,
    *const c_char,
    c_int,
    *mut *mut sys::AVDictionary,
) -> c_int;

/// FFmpeg's own `io_open`, which `avformat_alloc_context` gives every
/// context, for the one open [`open_only_the_file`] lets through.
static FFMPEG_IO_OPEN: OnceLock<IoOpen> = OnceLock::new();

thread_local! {
    /// While [`open_input_with`] opens a file on this thread: the nested
    /// opens refused so far.
    static REFUSED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Notes, while it lives, the opens [`open_only_the_file`] refuses on
/// this thread. FFmpeg opens a file and reads its header on the thread
/// that calls it.
struct RefusalWatch;

impl RefusalWatch {
    fn start() -> Self {
        REFUSED.with(|r| *r.borrow_mut() = Some(Vec::new()));
        RefusalWatch
    }

    /// The first open refused since [`start`](Self::start), as an error.
    fn finish(self) -> Option<OpenError> {
        let first = REFUSED.with(|r| r.borrow_mut().as_mut().map(std::mem::take))?;
        first
            .into_iter()
            .next()
            .map(|url| OpenError::Nested { url })
    }
}

impl Drop for RefusalWatch {
    fn drop(&mut self) {
        REFUSED.with(|r| *r.borrow_mut() = None);
    }
}

/// The `io_open` of every format context [`open_input`] makes. It lets
/// through only the open that gives the context its input: the one
/// `avformat_open_input` makes, into the context's `pb` while it has
/// none, of the URL the context was opened with. FFmpeg's own `io_open`
/// then opens it, under the context's `protocol_whitelist`. Every other
/// open, such as a playlist's segment or a MOV file's external data, is
/// refused with EPERM and logged. While the file is being opened, the
/// refusal also fails [`open_input`], naming what FFmpeg tried to open.
///
/// The closing side stays FFmpeg's own (`io_close2`, and in FFmpeg 6.1
/// the deprecated `io_close`): the only AVIOContext opened here is opened
/// by FFmpeg's own `io_open`, which those close.
///
/// Not every nested open goes through `io_open`: the concat demuxer opens
/// its files with a new format context of its own, which gets the
/// whitelists but FFmpeg's default `io_open`. That is why the demuxers are
/// limited first.
unsafe extern "C" fn open_only_the_file(
    s: *mut sys::AVFormatContext,
    pb: *mut *mut sys::AVIOContext,
    url: *const c_char,
    flags: c_int,
    options: *mut *mut sys::AVDictionary,
) -> c_int {
    let refused = sys::AVERROR(ff::util::error::EPERM);
    // A panic must not unwind into C; the open is refused.
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the arguments FFmpeg passes to io_open.
        let own = unsafe { is_own_input(s, pb, url, flags) };
        if let (true, Some(open)) = (own, FFMPEG_IO_OPEN.get()) {
            // SAFETY: FFmpeg's own io_open, with the arguments FFmpeg
            // passed to this one.
            return unsafe { open(s, pb, url, flags, options) };
        }
        let url = if url.is_null() {
            String::new()
        } else {
            // SAFETY: FFmpeg passes a NUL-terminated URL.
            unsafe { CStr::from_ptr(url) }
                .to_string_lossy()
                .into_owned()
        };
        tracing::warn!(url = %url, "refused FFmpeg's request to open another file or URL");
        REFUSED.with(|r| {
            if let Some(list) = r.borrow_mut().as_mut() {
                list.push(url);
            }
        });
        refused
    }))
    .unwrap_or(refused)
}

/// Whether an io_open call is `avformat_open_input` opening the context's
/// own input: for reading, into `s->pb` while it is NULL, the URL the
/// context has.
///
/// # Safety
///
/// The arguments FFmpeg passes to io_open: its live context, where the
/// AVIOContext goes, and a NUL-terminated URL.
unsafe fn is_own_input(
    s: *mut sys::AVFormatContext,
    pb: *mut *mut sys::AVIOContext,
    url: *const c_char,
    flags: c_int,
) -> bool {
    if s.is_null() || url.is_null() || flags & sys::AVIO_FLAG_WRITE != 0 {
        return false;
    }
    // SAFETY: `s` is FFmpeg's live context, whose `url` is NULL or a
    // NUL-terminated string; `url` is NUL-terminated.
    unsafe {
        pb == &raw mut (*s).pb
            && (*s).pb.is_null()
            && !(*s).url.is_null()
            && CStr::from_ptr(url) == CStr::from_ptr((*s).url)
    }
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

/// The timestamp of the last keyframe that the demuxer's index lists for
/// stream `index` whose data ends at or before byte `size`, and whose
/// timestamp is before `before` when given: where to seek in a file cut
/// at `size`. `None` without an index or such an entry.
pub(crate) fn index_keyframe_inside(
    ictx: &ff::format::context::Input,
    index: usize,
    size: u64,
    before: Option<i64>,
) -> Option<i64> {
    let st = ictx.stream(index)?;
    // SAFETY: as in `index_end`.
    let st = unsafe { st.as_ptr() }.cast_mut();
    // SAFETY: as above.
    let n = unsafe { sys::avformat_index_get_entries_count(st) };
    (0..n)
        .filter_map(|i| {
            // SAFETY: as in `index_end`.
            let e = unsafe { sys::avformat_index_get_entry(st, i).as_ref() }?;
            let pos = u64::try_from(e.pos).ok()?;
            let end = pos.checked_add(u64::try_from(e.size()).ok()?)?;
            (e.flags() & sys::AVINDEX_KEYFRAME != 0
                && end <= size
                && before.is_none_or(|b| e.timestamp < b))
            .then_some(e.timestamp)
        })
        .max()
}

/// Whether the demuxer marked `pkt` as one not to show
/// (`AV_PKT_FLAG_DISCARD`): a frame an edit list cuts, which is decoded
/// only for the frames after it. ffmpeg-next's flags leave the bit out.
pub fn packet_discarded(pkt: &ff::Packet) -> bool {
    use ff::codec::packet::Ref;
    // SAFETY: `pkt` wraps a live AVPacket.
    unsafe { (*pkt.as_ptr()).flags & sys::AV_PKT_FLAG_DISCARD != 0 }
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
        spherecut_core::may_block("opening a hardware decoding device");
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

/// The most pixels a decoder may make one picture of: 2^27, 16384 × 8192.
/// That is twice an 11K equirect (11520 × 5760), and more than any camera
/// Spherecut reads records (the Insta360 X5's 8K is 7680 × 3840). One
/// picture at the limit takes 192 MiB at 8-bit 4:2:0 and 1 GiB at 8 bytes
/// per pixel, which ProRes 4444 with alpha (decoded as 12-bit 4:4:4:4 in
/// 16-bit samples) and 16-bit RGBA PNG take.
///
/// Without it, FFmpeg's `max_pixels` is `INT_MAX`, but that is not what
/// stops a picture: its size check (`av_image_check_size2`) refuses one
/// whose rows, at 8 bytes per pixel plus padding, would come to 2^31
/// bytes, which is about 2^28 pixels (16384 × 16000 passes). So this
/// halves the largest picture a file can make a decoder allocate.
pub const MAX_PIXELS: i64 = 1 << 27;

/// A picture size over [`MAX_PIXELS`], which no decoder Spherecut opens
/// makes. `probe` warns of a stream of such pictures and `VideoDecoder`
/// refuses one, by its size. FFmpeg's own errors would not say why: its
/// decoders fail to open or decode with "Invalid argument", or the stream
/// is left with no size at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge {
    pub width: u32,
    pub height: u32,
}

/// The most FFmpeg pads a picture's rows to, in pixels: `STRIDE_ALIGN`,
/// which is 64 in x86-64 builds with AVX-512, less in others.
const ROW_ALIGN: u64 = 64;

impl TooLarge {
    /// `Some` when decoders limited to [`MAX_PIXELS`] may refuse a
    /// `width` × `height` picture. FFmpeg checks the limit when it
    /// allocates a picture (`ff_get_buffer`), with the width padded to its
    /// row alignment: its H.264 and MJPEG decoders refuse 16380 × 8194,
    /// fewer than 2^27 pixels, as 16384 × 8194. So the width is padded as
    /// much as FFmpeg ever pads it, which may refuse a picture within 63
    /// columns of the limit that a build with less padding, or a decoder
    /// that allocates its own pictures (libdav1d), would decode.
    pub fn of(width: u32, height: u32) -> Option<Self> {
        over_max_pixels(width, height, MAX_PIXELS).then_some(TooLarge { width, height })
    }
}

/// Whether a `width` × `height` picture is over `max_pixels` once its
/// width is padded as [`TooLarge::of`] says.
fn over_max_pixels(width: u32, height: u32, max_pixels: i64) -> bool {
    let padded = u64::from(width).next_multiple_of(ROW_ALIGN);
    i128::from(padded) * i128::from(height) > i128::from(max_pixels)
}

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = (u64::from(self.width), u64::from(self.height));
        write!(f, "{w}x{h}, ")?;
        if w * h <= MAX_PIXELS as u64 {
            write!(
                f,
                "which FFmpeg may pad to {}x{h}, ",
                w.next_multiple_of(ROW_ALIGN)
            )?;
        }
        // `too_large_pictures_are_named_by_size` holds this to MAX_PIXELS.
        write!(
            f,
            "more than the 2^27 pixels (16384x8192) per picture that Spherecut decodes"
        )
    }
}

/// Open decoder `codec` on `ctx`, limited to [`MAX_PIXELS`] per picture.
/// Every decoder Spherecut uses is opened here (clippy forbids
/// ffmpeg-next's own decoder opens elsewhere).
///
/// Its frames are cropped exactly as the stream says (H.264's frame
/// cropping, HEVC's conformance window), as FFmpeg's own ffmpeg tool has
/// cropped them since 7.1: without `AV_CODEC_FLAG_UNALIGNED`, FFmpeg keeps
/// up to 63 of the columns a stream crops on the left, so that planes stay
/// aligned, by a rule that depends on the pixel format. A device frame is
/// then downloaded and cropped the same way ([`download_into_pool`]), so
/// both give the same picture, of the size the stream's parameters say.
pub(crate) fn open_decoder(
    ctx: ff::codec::context::Context,
    codec: ff::Codec,
) -> Result<ff::decoder::Opened, ff::Error> {
    open_decoder_with(ctx, codec, MAX_PIXELS)
}

#[allow(clippy::disallowed_methods)] // The one place decoders open.
fn open_decoder_with(
    mut ctx: ff::codec::context::Context,
    codec: ff::Codec,
    max_pixels: i64,
) -> Result<ff::decoder::Opened, ff::Error> {
    // SAFETY: `ctx` owns a live AVCodecContext that is not open yet;
    // max_pixels is a plain int64 field that the decoder reads when it
    // opens and when it sizes each picture, and flags a plain int that
    // only decode.c's cropping reads this flag from (FFmpeg 6.1 to 8.1).
    unsafe {
        let c = ctx.as_mut_ptr();
        (*c).max_pixels = max_pixels;
        (*c).flags |= sys::AV_CODEC_FLAG_UNALIGNED as c_int;
    }
    ctx.decoder().open_as(codec)
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
    Ok(open_decoder(ctx, codec).map(ff::decoder::Video)?)
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
    plane_and_stride(frame, index).map(|(p, _)| p)
}

/// FFmpeg's `FF_QP2LAMBDA`: a quantizer scale times this is a lambda,
/// what `global_quality` and a frame's `quality` hold.
pub(crate) const QP2LAMBDA: i32 = 118;

/// Set `frame`'s `quality`, the lambda an encoder with a fixed quantizer
/// (`AV_CODEC_FLAG_QSCALE`) codes it at. ffmpeg-next reads it only.
pub(crate) fn set_frame_quality(frame: &mut ff::frame::Video, lambda: i32) {
    // SAFETY: `frame` is a live AVFrame we own; `quality` is a plain int.
    unsafe { (*frame.as_mut_ptr()).quality = lambda };
}

/// [`plane`], and the bytes from one row to the next (`linesize`).
pub(crate) fn plane_and_stride(frame: &ff::frame::Video, index: usize) -> Option<(&[u8], usize)> {
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
    let p = unsafe { std::slice::from_raw_parts(data, linesize.saturating_mul(rows).min(room)) };
    Some((p, linesize))
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

/// The buffer references `frame` holds, in `buf` and then
/// `extended_buf`, NULLs left out.
///
/// # Safety
///
/// `frame` is a live AVFrame, and stays so while the iterator is used.
unsafe fn buffer_refs<'a>(
    frame: *const sys::AVFrame,
) -> impl Iterator<Item = *mut sys::AVBufferRef> + 'a {
    // SAFETY: the caller's. `buf` holds NULL or references the frame
    // owns, and `extended_buf` holds `nb_extended_buf` more.
    unsafe {
        let extended = match usize::try_from((*frame).nb_extended_buf) {
            Ok(n) if n > 0 && !(*frame).extended_buf.is_null() => {
                std::slice::from_raw_parts((*frame).extended_buf, n)
            }
            _ => &[],
        };
        (*frame)
            .buf
            .iter()
            .chain(extended)
            .copied()
            .filter(|b| !b.is_null())
    }
}

/// The bytes the buffers `frame` holds take, by `AVBufferRef.size`, with
/// a buffer that two references share counted once: what keeping the
/// frame keeps, padding, alignment and the rows past its height included.
/// 0 for a frame whose data it doesn't own.
pub(crate) fn buffer_bytes(frame: &ff::frame::Video) -> usize {
    let mut seen = Vec::new();
    let mut total = 0usize;
    // SAFETY: `frame` wraps a live AVFrame, borrowed while this runs.
    for b in unsafe { buffer_refs(frame.as_ptr()) } {
        // SAFETY: a live buffer reference; plain fields. The buffer's
        // pointer is only compared.
        let (buffer, size) = unsafe { ((*b).buffer.addr(), (*b).size) };
        if !seen.contains(&buffer) {
            seen.push(buffer);
            total = total.saturating_add(size);
        }
    }
    total
}

/// How many bytes there are from `p` to the end of the buffer of `frame`
/// that holds `p`. `None` when no buffer of the frame holds it.
fn buffer_room(frame: *const sys::AVFrame, p: *const u8) -> Option<usize> {
    // SAFETY: `frame` is a live AVFrame while this runs.
    let mut bufs = unsafe { buffer_refs(frame) };
    let p = p.addr();
    bufs.find_map(|b| {
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

/// What the decoder said about a frame, read as ints (D11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameFields {
    pub format: c_int,
    pub width: u32,
    pub height: u32,
    /// `pts`, else `best_effort_timestamp`, else `None`.
    pub pts: Option<i64>,
    /// In the stream's time base; 0 when unknown.
    pub duration: i64,
    pub key: bool,
    /// FFmpeg flagged it corrupt: decoded from missing references, after
    /// a seek.
    pub corrupt: bool,
    /// The decoder concealed errors in it (`decode_error_flags`).
    pub concealed: bool,
    /// An I-frame (`pict_type`; for H.264, by its first slice).
    pub intra: bool,
    pub color: ColorCodes,
}

pub(crate) fn frame_fields(frame: &ff::frame::Video) -> FrameFields {
    let ts = |t: i64| (t != sys::AV_NOPTS_VALUE).then_some(t);
    let dim = |n: c_int| u32::try_from(n).unwrap_or(0);
    // SAFETY: `frame` wraps a live AVFrame. These are plain fields, and
    // the enum fields are read as ints.
    unsafe {
        let f = frame.as_ptr();
        FrameFields {
            format: (*f).format,
            width: dim((*f).width),
            height: dim((*f).height),
            pts: ts((*f).pts).or_else(|| ts((*f).best_effort_timestamp)),
            duration: (*f).duration,
            key: (*f).flags & sys::AV_FRAME_FLAG_KEY != 0,
            corrupt: (*f).flags & sys::AV_FRAME_FLAG_CORRUPT != 0,
            concealed: (*f).decode_error_flags != 0,
            intra: enum_int(&raw const (*f).pict_type) == PICT_I,
            color: ColorCodes {
                primaries: enum_int(&raw const (*f).color_primaries),
                transfer: enum_int(&raw const (*f).color_trc),
                matrix: enum_int(&raw const (*f).colorspace),
                range: enum_int(&raw const (*f).color_range),
                chroma_location: enum_int(&raw const (*f).chroma_location),
            },
        }
    }
}

/// Where plane `index` of `frame` starts, as `AVFrame.data` has it, for
/// tests that check a plane is handed out without a copy.
pub(crate) fn data_ptr(frame: &ff::frame::Video, index: usize) -> *const u8 {
    // SAFETY: `frame` wraps a live AVFrame; `data` is a plain array, and
    // the pointer is only returned, never read here.
    unsafe {
        (*frame.as_ptr())
            .data
            .get(index)
            .map_or(std::ptr::null(), |p| p.cast_const())
    }
}

/// Pixel-format flags that rule a format out of [`pixel_layout`]:
/// big-endian, palette, bitstream, hardware, float and Bayer formats.
const NOT_NATIVE: u64 = (sys::AV_PIX_FMT_FLAG_BE
    | sys::AV_PIX_FMT_FLAG_PAL
    | sys::AV_PIX_FMT_FLAG_BITSTREAM
    | sys::AV_PIX_FMT_FLAG_HWACCEL
    | sys::AV_PIX_FMT_FLAG_FLOAT
    | sys::AV_PIX_FMT_FLAG_BAYER) as u64;

/// How frames in pixel format `fmt` lie in memory, when they can be
/// uploaded as they are (D30): planar or semi-planar, little-endian, 8 to
/// 16 bits with every colour component at the same depth, in the low bits
/// or, for 16-bit samples, the high ones (P010). YUV with U before V,
/// planar RGB in FFmpeg's G, B, R order, or gray. An alpha component is
/// left out. `None` for every other format, which [`conversion_target`]
/// converts, and for a format this libavutil doesn't describe.
///
/// The rule reads `AVPixFmtDescriptor` as ints (D11), so it covers every
/// such format, the plan's list (`nv12`, `p010le`, `yuv420p10le`,
/// `gbrp12le`, ...) and others like it (`yuv440p`, `yuv420p9le`, `nv24`).
pub(crate) fn pixel_layout(fmt: c_int) -> Option<PixelLayout> {
    // SAFETY: accepts any value and returns NULL for unknown ones; non-NULL
    // descriptors point at static tables with no enum fields.
    let d = unsafe { c::av_pix_fmt_desc_get(fmt).as_ref() }?;
    if d.flags & NOT_NATIVE != 0 {
        return None;
    }
    let alpha = d.flags & sys::AV_PIX_FMT_FLAG_ALPHA as u64 != 0;
    // Alpha, when there is one, is the last component.
    let colour = usize::from(d.nb_components)
        .min(4)
        .checked_sub(usize::from(alpha))?;
    let comps = d.comp.get(..colour)?;
    let depth = comps.first()?.depth;
    let shift = comps.first()?.shift;
    if !(8..=16).contains(&depth) || comps.iter().any(|c| c.depth != depth || c.shift != shift) {
        return None;
    }
    let bytes = if depth > 8 {
        SampleBytes::Two
    } else {
        SampleBytes::One
    };
    let b = bytes.get() as c_int;
    let msb_aligned = shift != 0;
    if msb_aligned && (b != 2 || shift != 16 - depth) {
        return None;
    }
    // Component `c` alone in `plane`, or interleaved with another at
    // `offset` samples into each pair.
    let alone =
        |c: &sys::AVComponentDescriptor, plane| c.plane == plane && c.step == b && c.offset == 0;
    let paired = |c: &sys::AVComponentDescriptor, offset| {
        c.plane == 1 && c.step == 2 * b && c.offset == offset * b
    };
    let full = PlaneDesc {
        components: 1,
        bytes,
        log2_sub: (0, 0),
    };
    let sub = (d.log2_chroma_w, d.log2_chroma_h);
    let rgb = d.flags & sys::AV_PIX_FMT_FLAG_RGB as u64 != 0;
    let (family, planes) = match comps {
        [y] if !rgb && alone(y, 0) => (ColorFamily::Gray, vec![full]),
        // The descriptor lists R, G, B; gbrp keeps G, B and R in planes
        // 0, 1 and 2.
        [r, g, bl] if rgb && alone(g, 0) && alone(bl, 1) && alone(r, 2) => {
            (ColorFamily::Rgb, vec![full; 3])
        }
        [y, u, v] if !rgb && alone(y, 0) && alone(u, 1) && alone(v, 2) => {
            let chroma = PlaneDesc {
                log2_sub: sub,
                ..full
            };
            (ColorFamily::Yuv, vec![full, chroma, chroma])
        }
        [y, u, v] if !rgb && alone(y, 0) && paired(u, 0) && paired(v, 1) => {
            let chroma = PlaneDesc {
                components: 2,
                log2_sub: sub,
                ..full
            };
            (ColorFamily::Yuv, vec![full, chroma])
        }
        _ => return None,
    };
    Some(PixelLayout {
        name: pix_fmt_name(fmt)?,
        planes,
        family,
        bit_depth: u8::try_from(depth).ok()?,
        msb_aligned,
    })
}

/// The format [`Scaler`] converts frames in `fmt` to, when
/// [`pixel_layout`] rules `fmt` out (D30): the native planar format of the
/// same kind, at 8, 10, 12 or 16 bits, whichever holds the source's depth.
/// RGB, palette, Bayer and XYZ formats go to `gbrp*`, gray to `gray*`, and
/// YUV to `yuv420p*`, `yuv422p*` or `yuv444p*` by its subsampling, or to
/// `yuv444p*` for other subsamplings. So nothing is resampled, chroma
/// keeps its siting, and RGB doesn't go through a YUV matrix. `None` for a
/// format this libavutil doesn't describe.
pub(crate) fn conversion_target(fmt: c_int) -> Option<c_int> {
    use sys::AVPixelFormat as P;
    // SAFETY: as in `pixel_layout`.
    let d = unsafe { c::av_pix_fmt_desc_get(fmt).as_ref() }?;
    let n = usize::from(d.nb_components).min(4);
    let depth = d.comp[..n].iter().map(|c| c.depth).max()?;
    // 8, 10, 12 or 16 bits.
    let class = match depth {
        ..=8 => 0,
        9..=10 => 1,
        11..=12 => 2,
        _ => 3,
    };
    let rgb_like =
        (sys::AV_PIX_FMT_FLAG_RGB | sys::AV_PIX_FMT_FLAG_PAL | sys::AV_PIX_FMT_FLAG_BAYER) as u64;
    let alpha = d.flags & sys::AV_PIX_FMT_FLAG_ALPHA as u64 != 0;
    let gray = n - usize::from(alpha) == 1;
    let name = pix_fmt_name(fmt).unwrap_or_default();
    let family: [P; 4] = if d.flags & rgb_like != 0 || name.starts_with("xyz") {
        [
            P::AV_PIX_FMT_GBRP,
            P::AV_PIX_FMT_GBRP10LE,
            P::AV_PIX_FMT_GBRP12LE,
            P::AV_PIX_FMT_GBRP16LE,
        ]
    } else if gray {
        [
            P::AV_PIX_FMT_GRAY8,
            P::AV_PIX_FMT_GRAY10LE,
            P::AV_PIX_FMT_GRAY12LE,
            P::AV_PIX_FMT_GRAY16LE,
        ]
    } else {
        match (d.log2_chroma_w, d.log2_chroma_h) {
            (1, 1) => [
                P::AV_PIX_FMT_YUV420P,
                P::AV_PIX_FMT_YUV420P10LE,
                P::AV_PIX_FMT_YUV420P12LE,
                P::AV_PIX_FMT_YUV420P16LE,
            ],
            (1, 0) => [
                P::AV_PIX_FMT_YUV422P,
                P::AV_PIX_FMT_YUV422P10LE,
                P::AV_PIX_FMT_YUV422P12LE,
                P::AV_PIX_FMT_YUV422P16LE,
            ],
            _ => [
                P::AV_PIX_FMT_YUV444P,
                P::AV_PIX_FMT_YUV444P10LE,
                P::AV_PIX_FMT_YUV444P12LE,
                P::AV_PIX_FMT_YUV444P16LE,
            ],
        }
    };
    Some(family[class] as c_int)
}

/// Why a [`Scaler`] could not be made.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ScaleError {
    #[error("swscale does not take {0} as input")]
    Input(String),
    #[error("swscale does not make {0}")]
    Output(String),
    #[error("swscale refused to convert {from} at {width}x{height} to {to}")]
    Refused {
        from: String,
        to: String,
        width: u32,
        height: u32,
    },
}

/// A swscale context converting frames of one format and size to another
/// format at the same size, with formats as ints (D11). ffmpeg-next's own
/// `scaling::Context` takes bindgen enums and is not `Send`.
pub(crate) struct Scaler {
    ctx: *mut sys::SwsContext,
    from: c_int,
    to: c_int,
    width: u32,
    height: u32,
}

// SAFETY: the context is only used through `&mut self`, by one thread at
// a time, and swscale keeps no thread-local state for it.
unsafe impl Send for Scaler {}

impl std::fmt::Debug for Scaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scaler")
            .field("from", &pix_fmt_name(self.from))
            .field("to", &pix_fmt_name(self.to))
            .field("size", &(self.width, self.height))
            .finish()
    }
}

impl Scaler {
    /// A context for `width` × `height` frames in `from`, to `to`. The
    /// range stays what it is: a full-range source isn't squeezed to
    /// limited range (or the other way), and frames keep their colour
    /// tags.
    pub fn new(from: c_int, to: c_int, width: u32, height: u32) -> Result<Scaler, ScaleError> {
        let name = |f| pix_fmt_name(f).unwrap_or_else(|| format!("format {f}"));
        // SAFETY: both accept any value.
        if unsafe { c::sws_isSupportedInput(from) } <= 0 {
            return Err(ScaleError::Input(name(from)));
        }
        // SAFETY: as above.
        if unsafe { c::sws_isSupportedOutput(to) } <= 0 {
            return Err(ScaleError::Output(name(to)));
        }
        let refused = || ScaleError::Refused {
            from: name(from),
            to: name(to),
            width,
            height,
        };
        let (w, h) = (
            c_int::try_from(width).map_err(|_| refused())?,
            c_int::try_from(height).map_err(|_| refused())?,
        );
        // Same size both ways, so the filter only matters for chroma that
        // has to be made from neighbours (yuv411p to yuv444p, say).
        // ffmpeg-next's flags, because FFmpeg 8 made the SWS_ defines an
        // enum, which ffmpeg-sys-next binds as `SwsFlags` instead.
        use ff::software::scaling::Flags;
        let flags = (Flags::BILINEAR | Flags::ACCURATE_RND | Flags::BITEXACT).bits();
        // SAFETY: formats are checked above; NULL filters and params are
        // allowed. NULL is returned on failure.
        let ctx = unsafe {
            c::sws_getContext(
                w,
                h,
                from,
                w,
                h,
                to,
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ctx.is_null() {
            return Err(refused());
        }
        let scaler = Scaler {
            ctx,
            from,
            to,
            width,
            height,
        };
        let (mut inv, mut table) = (std::ptr::null_mut(), std::ptr::null_mut());
        let (mut src_range, mut dst_range, mut b, mut c, mut s) = (0, 0, 0, 0, 0);
        // SAFETY: `ctx` is live, and every out-pointer points at a local.
        // The tables returned belong to the context, and are passed back
        // to it at once.
        unsafe {
            if sys::sws_getColorspaceDetails(
                ctx,
                &mut inv,
                &mut src_range,
                &mut table,
                &mut dst_range,
                &mut b,
                &mut c,
                &mut s,
            ) >= 0
                && src_range != dst_range
            {
                sys::sws_setColorspaceDetails(ctx, inv, src_range, table, src_range, b, c, s);
            }
        }
        Ok(scaler)
    }

    /// The source format and size it was made for.
    pub fn source(&self) -> (c_int, u32, u32) {
        (self.from, self.width, self.height)
    }

    /// `src` converted, with its timestamps and colour tags. `src` must be
    /// in the source format and size.
    pub fn scale(&mut self, src: &ff::frame::Video) -> Result<ff::frame::Video, ff::Error> {
        let f = frame_fields(src);
        if (f.format, f.width, f.height) != self.source() {
            return Err(ff::Error::InvalidData);
        }
        let mut dst = ff::frame::Video::empty();
        // SAFETY: `dst` is a fresh frame without buffers, which
        // sws_scale_frame allocates for the format and size set here;
        // `src` is a live frame of the context's input. The properties
        // are copied onto `dst` after.
        let ret = unsafe {
            let d = dst.as_mut_ptr();
            (*d).format = self.to;
            (*d).width = self.width as c_int;
            (*d).height = self.height as c_int;
            let ret = sys::sws_scale_frame(self.ctx, d, src.as_ptr());
            if ret < 0 {
                ret
            } else {
                sys::av_frame_copy_props(d, src.as_ptr())
            }
        };
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
        Ok(dst)
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        // SAFETY: the context was made by sws_getContext and is freed once.
        unsafe { sys::sws_freeContext(self.ctx) };
    }
}

/// The audio frame fields Spherecut reads, as plain values: the sample
/// format is the int it is in C (D11), and the channel layout's order is
/// read as an int too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioFrameFields {
    pub format: c_int,
    pub rate: u32,
    pub channels: usize,
    pub samples: usize,
    pub pts: Option<i64>,
}

pub(crate) fn audio_frame_fields(frame: &ff::frame::Audio) -> AudioFrameFields {
    // SAFETY: `frame` wraps a live AVFrame; plain fields and an enum field
    // read as the int it is.
    unsafe {
        let f = frame.as_ptr();
        let pts = match (*f).pts {
            sys::AV_NOPTS_VALUE => (*f).best_effort_timestamp,
            p => p,
        };
        AudioFrameFields {
            format: (*f).format,
            rate: u32::try_from((*f).sample_rate).unwrap_or(0),
            channels: usize::try_from((*f).ch_layout.nb_channels).unwrap_or(0),
            samples: usize::try_from((*f).nb_samples).unwrap_or(0),
            pts: (pts != sys::AV_NOPTS_VALUE).then_some(pts),
        }
    }
}

/// FFmpeg's name for sample format `fmt`, such as `fltp`.
pub(crate) fn sample_fmt_name(fmt: c_int) -> Option<String> {
    // SAFETY: accepts any value; NULL for one it doesn't know.
    cstr_opt(unsafe { c::av_get_sample_fmt_name(fmt) })
}

/// Open the decoder for a codec context made from an audio stream's
/// parameters, looked up by its codec id as an int, for the reason
/// [`open_video_decoder`] gives.
pub(crate) fn open_audio_decoder(
    ctx: ff::codec::context::Context,
) -> Result<ff::decoder::Audio, ff::Error> {
    // SAFETY: `ctx` owns a live AVCodecContext; its enum fields are read as
    // ints.
    let (kind, id) = unsafe {
        let c = ctx.as_ptr();
        (
            enum_int(&raw const (*c).codec_type),
            enum_int(&raw const (*c).codec_id),
        )
    };
    if kind != sys::AVMediaType::AVMEDIA_TYPE_AUDIO as c_int {
        return Err(ff::Error::InvalidData);
    }
    // SAFETY: accepts any id; NULL when no decoder for it is built in.
    let codec = unsafe { c::avcodec_find_decoder(id) };
    if codec.is_null() {
        return Err(ff::Error::DecoderNotFound);
    }
    // SAFETY: registered codecs are static.
    let codec = unsafe { ff::Codec::wrap(codec) };
    open_decoder(ctx, codec).map(ff::decoder::Audio)
}

/// Tell an unopened decoder the time base of the packets it will get,
/// which ffmpeg-next has no setter for. libavcodec needs it to move a
/// frame's timestamp past the samples it drops (AAC's priming, D35).
pub(crate) fn set_packet_time_base(ctx: &mut ff::codec::context::Context, tb: ff::Rational) {
    // SAFETY: `ctx` owns a live AVCodecContext; a plain field.
    unsafe { (*ctx.as_mut_ptr()).pkt_timebase = tb.into() };
}

/// A channel layout a [`Resampler`] reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channels {
    /// The frame's own layout, for swresample to mix down from.
    Frame,
    /// FFmpeg's default layout for this many channels: mono, stereo.
    Default(usize),
    /// This many channels in no order, which swresample passes through
    /// as they are, one for one: first-order ambiX (D17, kept from being
    /// remixed as 4.0 surround), and any stream whose channels are kept.
    Unordered(usize),
}

/// A channel layout of our own, which FFmpeg reads.
struct Layout(sys::AVChannelLayout);

impl Layout {
    fn new(channels: Channels) -> Option<Layout> {
        let n = match channels {
            Channels::Frame => return None,
            Channels::Default(n) | Channels::Unordered(n) => c_int::try_from(n).ok()?,
        };
        // SAFETY: all zeroes is a valid AVChannelLayout: order 0 is
        // AV_CHANNEL_ORDER_UNSPEC, with no channels, mask or map.
        let mut l: sys::AVChannelLayout = unsafe { std::mem::zeroed() };
        match channels {
            // SAFETY: `l` is a valid layout to overwrite; for an order that
            // isn't custom FFmpeg allocates nothing in it.
            Channels::Default(_) => unsafe { sys::av_channel_layout_default(&mut l, n) },
            _ => l.nb_channels = n,
        }
        Some(Layout(l))
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        // SAFETY: a layout made by `new`; uninit frees nothing for its
        // orders and is safe to call on any valid layout.
        unsafe { sys::av_channel_layout_uninit(&mut self.0) };
    }
}

/// A swresample context converting one stream's decoded frames to packed
/// f32 at another rate, with sample formats as ints (D11). ffmpeg-next's
/// own resampler takes bindgen enums and, before FFmpeg 7, the bitflags
/// channel layouts.
pub(crate) struct Resampler {
    ctx: *mut sys::SwrContext,
    from: (c_int, u32, usize),
    to: (u32, usize),
}

// SAFETY: the context is only used through `&mut self`, by one thread at
// a time, and swresample keeps no thread-local state for it.
unsafe impl Send for Resampler {}

impl std::fmt::Debug for Resampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resampler")
            .field(
                "from",
                &(sample_fmt_name(self.from.0), self.from.1, self.from.2),
            )
            .field("to", &self.to)
            .finish()
    }
}

/// Packed f32, as a [`Resampler`] writes.
const SAMPLE_FMT_FLT: c_int = sys::AVSampleFormat::AV_SAMPLE_FMT_FLT as c_int;

impl Resampler {
    /// A context for frames like `frame`, read as `input`, to `rate` and
    /// `output`.
    pub fn new(
        frame: &ff::frame::Audio,
        input: Channels,
        rate: u32,
        output: Channels,
    ) -> Result<Resampler, ff::Error> {
        let f = audio_frame_fields(frame);
        let in_rate = c_int::try_from(f.rate).map_err(|_| ff::Error::InvalidData)?;
        let out_rate = c_int::try_from(rate).map_err(|_| ff::Error::InvalidData)?;
        let out_layout = Layout::new(output).ok_or(ff::Error::InvalidData)?;
        let own_in = Layout::new(input);
        let in_ptr = match &own_in {
            Some(l) => &raw const l.0,
            // SAFETY: `frame` wraps a live AVFrame; swresample copies the
            // layout it points at.
            None => unsafe { &raw const (*frame.as_ptr()).ch_layout },
        };
        let mut ctx = std::ptr::null_mut();
        // SAFETY: both layouts are valid for the call, which copies them;
        // the formats are ints FFmpeg checks; `ctx` receives a new context
        // or stays NULL.
        let ret = unsafe {
            c::swr_alloc_set_opts2(
                &mut ctx,
                &out_layout.0,
                SAMPLE_FMT_FLT,
                out_rate,
                in_ptr,
                f.format,
                in_rate,
                0,
                std::ptr::null_mut(),
            )
        };
        if ret < 0 || ctx.is_null() {
            // SAFETY: frees a context made above, or does nothing on NULL.
            unsafe { sys::swr_free(&mut ctx) };
            return Err(ff::Error::from(ret.min(-1)));
        }
        let channels = |c: Channels| match c {
            Channels::Frame => f.channels,
            Channels::Default(n) | Channels::Unordered(n) => n,
        };
        let r = Resampler {
            ctx,
            from: (f.format, f.rate, channels(input)),
            to: (rate, channels(output)),
        };
        // SAFETY: a context configured above; `r` frees it on failure.
        let ret = unsafe { sys::swr_init(r.ctx) };
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
        Ok(r)
    }

    /// Whether `frame` is in the format, rate and channel count this
    /// context reads.
    pub fn reads(&self, frame: &ff::frame::Audio) -> bool {
        let f = audio_frame_fields(frame);
        (f.format, f.rate, f.channels) == self.from
    }

    /// Converts `count` of `frame`'s samples from `skip` on, appending the
    /// output, interleaved, to `out`.
    pub fn convert(
        &mut self,
        frame: &ff::frame::Audio,
        skip: usize,
        count: usize,
        out: &mut Vec<f32>,
    ) -> Result<(), ff::Error> {
        if !self.reads(frame) {
            return Err(ff::Error::InvalidData);
        }
        let f = audio_frame_fields(frame);
        let n = f.samples.saturating_sub(skip).min(count);
        // SAFETY: accepts any int.
        let planar = unsafe { c::av_sample_fmt_is_planar(f.format) } != 0;
        // SAFETY: as above; 0 for a format FFmpeg doesn't know, which
        // swr_init has refused.
        let bytes = usize::try_from(unsafe { c::av_get_bytes_per_sample(f.format) })
            .map_err(|_| ff::Error::InvalidData)?;
        let planes = if planar { f.channels } else { 1 };
        let step = if planar { bytes } else { bytes * f.channels };
        // SAFETY: `frame` wraps a live AVFrame of `f.samples` samples in
        // `f.format` with `f.channels` channels, so `extended_data` holds
        // `planes` pointers to planes of `f.samples × step` bytes, and
        // `skip × step` is within them when skip ≤ samples.
        let input: Vec<*const u8> = unsafe {
            let data = (*frame.as_ptr()).extended_data;
            (0..planes)
                .map(|p| (*data.add(p)).cast_const().add(skip.min(f.samples) * step))
                .collect()
        };
        self.run(Some(&input), n, out)
    }

    /// The samples held back for the filter, appended to `out`; the
    /// context then starts afresh.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), ff::Error> {
        self.run(None, 0, out)
    }

    fn run(
        &mut self,
        input: Option<&[*const u8]>,
        samples: usize,
        out: &mut Vec<f32>,
    ) -> Result<(), ff::Error> {
        let in_count = c_int::try_from(samples).map_err(|_| ff::Error::InvalidData)?;
        // SAFETY: a live context; accepts any count.
        let room = unsafe { sys::swr_get_out_samples(self.ctx, in_count) };
        let room = usize::try_from(room).map_err(|_| ff::Error::from(room))?;
        let channels = self.to.1;
        let start = out.len();
        out.resize(start + room * channels, 0.0);
        let out_ptr = out[start..].as_mut_ptr().cast::<u8>();
        let in_ptr = input.map_or(std::ptr::null(), <[*const u8]>::as_ptr);
        // SAFETY: `out_ptr` has room for `room` packed frames of
        // `channels` f32s, the count swresample said it may write;
        // `in_ptr` is NULL (flush) or holds a pointer per plane to
        // `samples` samples of the format the context was made for.
        let ret = unsafe {
            c::swr_convert(
                self.ctx,
                &raw const out_ptr,
                c_int::try_from(room).unwrap_or(c_int::MAX),
                in_ptr,
                in_count,
            )
        };
        let written = match usize::try_from(ret) {
            Ok(n) => n.min(room),
            Err(_) => {
                out.truncate(start);
                return Err(ff::Error::from(ret));
            }
        };
        out.truncate(start + written * channels);
        Ok(())
    }
}

impl Drop for Resampler {
    fn drop(&mut self) {
        // SAFETY: the context was made by swr_alloc_set_opts2 and is
        // freed once.
        unsafe { sys::swr_free(&mut self.ctx) };
    }
}

/// `frame`, planar Y'CbCr, converted by swscale to packed RGB48 (R, G
/// and B per pixel, rows top to bottom), with the BT.709 matrix or
/// BT.601's and full or limited range as told rather than as the frame
/// is tagged. For the cross-check of the renderer's own conversion
/// (`encode::swscale_rgb48`).
pub(crate) fn swscale_rgb48(
    frame: &ff::frame::Video,
    bt709: bool,
    full_range: bool,
) -> Result<Vec<u16>, ScaleError> {
    // swscale.h's SWS_CS_ITU709 and SWS_CS_ITU601.
    let colorspace: c_int = if bt709 { 1 } else { 5 };
    let to = sys::AVPixelFormat::AV_PIX_FMT_RGB48LE as c_int;
    let f = frame_fields(frame);
    let mut scaler = Scaler::new(f.format, to, f.width, f.height)?;
    let refused = || ScaleError::Refused {
        from: pix_fmt_name(f.format).unwrap_or_else(|| format!("format {}", f.format)),
        to: "rgb48le".into(),
        width: f.width,
        height: f.height,
    };
    // SAFETY: the context is live; sws_getCoefficients returns a static
    // table for any value, which the context only reads. The output is
    // RGB, so its range flag says nothing.
    let set = unsafe {
        let table = sys::sws_getCoefficients(colorspace);
        sys::sws_setColorspaceDetails(
            scaler.ctx,
            table,
            c_int::from(full_range),
            table,
            1,
            0,
            1 << 16,
            1 << 16,
        )
    };
    if set < 0 {
        return Err(refused());
    }
    let rgb = scaler.scale(frame).map_err(|_| refused())?;
    let (w, h) = (f.width as usize, f.height as usize);
    let stride = rgb.stride(0);
    let data = rgb.data(0);
    let mut out = Vec::with_capacity(3 * w * h);
    for y in 0..h {
        let row = data
            .get(y * stride..y * stride + 6 * w)
            .ok_or_else(refused)?;
        out.extend(
            row.chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]])),
        );
    }
    Ok(out)
}

/// `av_seek_frame` on stream `stream` to `ts`, in its time base, to the
/// keyframe at or before it when `backward`, else at or after it.
pub(crate) fn seek_frame(
    ictx: &mut ff::format::context::Input,
    stream: usize,
    ts: i64,
    backward: bool,
) -> Result<(), ff::Error> {
    let flags = if backward {
        sys::AVSEEK_FLAG_BACKWARD
    } else {
        0
    };
    let index = c_int::try_from(stream).map_err(|_| ff::Error::StreamNotFound)?;
    // SAFETY: `ictx` owns a live, opened AVFormatContext.
    let ret = unsafe { sys::av_seek_frame(ictx.as_mut_ptr(), index, ts, flags) };
    if ret < 0 {
        return Err(ff::Error::from(ret));
    }
    Ok(())
}

/// `av_seek_frame` to byte 0 of the file, for a demuxer that can.
pub(crate) fn seek_to_byte_zero(ictx: &mut ff::format::context::Input) -> Result<(), ff::Error> {
    // SAFETY: as above; stream -1 with a byte position.
    let ret = unsafe { sys::av_seek_frame(ictx.as_mut_ptr(), -1, 0, sys::AVSEEK_FLAG_BYTE) };
    if ret < 0 {
        return Err(ff::Error::from(ret));
    }
    Ok(())
}

/// The demuxer's index of stream `index`, which `f` reads entry by entry
/// as (timestamp, flags), timestamps in the stream's time base. `None`
/// for a stream that doesn't exist.
fn with_index<T>(
    ictx: &ff::format::context::Input,
    index: usize,
    f: impl FnOnce(&mut dyn Iterator<Item = (i64, c_int)>) -> T,
) -> Option<T> {
    let st = ictx.stream(index)?;
    // SAFETY: as in `index_end`.
    let st = unsafe { st.as_ptr() }.cast_mut();
    // SAFETY: as above.
    let n = unsafe { sys::avformat_index_get_entries_count(st) };
    let mut entries = (0..n).filter_map(|i| {
        // SAFETY: as in `index_end`.
        let e = unsafe { sys::avformat_index_get_entry(st, i).as_ref() }?;
        Some((e.timestamp, e.flags()))
    });
    Some(f(&mut entries))
}

/// How many samples of stream `index` the index lists that are not
/// hidden by an edit list (`AVINDEX_DISCARD_FRAME`). For the MOV demuxer,
/// whose index lists every sample, that is the number of frames shown.
pub(crate) fn index_shown(ictx: &ff::format::context::Input, index: usize) -> u64 {
    with_index(ictx, index, |e| {
        e.filter(|(_, flags)| flags & sys::AVINDEX_DISCARD_FRAME == 0)
            .count() as u64
    })
    .unwrap_or(0)
}

/// The timestamps of every entry in the index of stream `index`, in its
/// order (by timestamp).
pub(crate) fn index_timestamps(ictx: &ff::format::context::Input, index: usize) -> Vec<i64> {
    with_index(ictx, index, |e| e.map(|(ts, _)| ts).collect()).unwrap_or_default()
}

/// The timestamp of the last entry in the index of stream `index`.
pub(crate) fn index_last(ictx: &ff::format::context::Input, index: usize) -> Option<i64> {
    with_index(ictx, index, |e| e.last().map(|(ts, _)| ts)).flatten()
}

/// The timestamp of the first entry in the index of stream `index`.
pub(crate) fn index_first(ictx: &ff::format::context::Input, index: usize) -> Option<i64> {
    with_index(ictx, index, |e| e.next().map(|(ts, _)| ts)).flatten()
}

/// The timestamp of the keyframe that `av_index_search_timestamp` names
/// for `ts` with `AVSEEK_FLAG_BACKWARD`: the last at or before it, as a
/// seek to `ts` would land. `None` without an index or such an entry.
pub(crate) fn index_keyframe_before(
    ictx: &ff::format::context::Input,
    index: usize,
    ts: i64,
) -> Option<i64> {
    let st = ictx.stream(index)?;
    // SAFETY: as in `index_end`; the search only reads the index.
    let st = unsafe { st.as_ptr() }.cast_mut();
    // SAFETY: as above.
    let i = unsafe { sys::av_index_search_timestamp(st, ts, sys::AVSEEK_FLAG_BACKWARD) };
    // SAFETY: `i` came from the search, so it is in range when it is not
    // negative.
    let e = unsafe { sys::avformat_index_get_entry(st, i).as_ref() }?;
    Some(e.timestamp)
}

/// Have the decoder skip frames no other frame refers to
/// (`AVDISCARD_NONREF`), or skip none (`AVDISCARD_DEFAULT`).
pub(crate) fn skip_nonref(dec: &mut ff::codec::context::Context, skip: bool) {
    let v = if skip {
        sys::AVDiscard::AVDISCARD_NONREF
    } else {
        sys::AVDiscard::AVDISCARD_DEFAULT
    };
    // SAFETY: `dec` owns a live AVCodecContext, whose `skip_frame` may be
    // set between packets. The value is one our headers define.
    unsafe { (*dec.as_mut_ptr()).skip_frame = v };
}

/// Copy a hardware frame into system memory (NV12 / P010) in buffers of
/// its own, as doctor's one-frame probes do; decoders download into a
/// [`DownloadPool`].
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

/// Rows of a downloaded plane start this many bytes apart, at least: the
/// pitch wgpu asks of a buffer-to-texture copy
/// (`COPY_BYTES_PER_ROW_ALIGNMENT`), so render's staging belt copies a
/// plane in one go. `write_texture` does so only when the rows are as far
/// apart as its own staging's, the row rounded up to the device's
/// `optimalBufferCopyRowPitchAlignment` (128 on lavapipe), which a
/// 256-byte pitch meets when the row is a multiple of 256 already or the
/// device asks for 256; otherwise it copies row by row (DECISIONS D30).
pub(crate) const DOWNLOAD_ROW_ALIGN: usize = 256;

/// The buffers a decoder's frames are downloaded into (M1 plan section
/// 3.5, D30): an `AVBufferPool` of buffers sized for the device frames'
/// system-memory format (`hw_frames_ctx->sw_format`) and size, each
/// holding every plane, with rows [`DOWNLOAD_ROW_ALIGN`] bytes apart. A
/// buffer goes back to the pool when the last frame using it drops, so
/// after the first frames a download allocates nothing, which
/// [`allocated`](Self::allocated) counts. A frame of another format or
/// size, as after `LayoutChanged`, rebuilds the pool; buffers of the old
/// one still in use are freed as they come back.
///
/// How each buffer is allocated depends on the device ([`Backing`]).
/// Vulkan's are mappings of their own, 2 MiB aligned, that the kernel is
/// asked to back with transparent huge pages ([`huge_alloc`]): a new 5.7K
/// P010 frame in 4 KiB pages took about 12 000 faults, which on odin
/// doubled its download while the frame cache filled (D30). Where THP is
/// off the advice does nothing, and the pages are 4 KiB as before.
/// VA-API's come from `av_buffer_alloc`, as FFmpeg's own would: in
/// mappings its 3840² downloads ran at half the speed on most runs,
/// faulting in ten times the pages, and in malloc's memory at full speed
/// on every run (odin, 2026-10-30; m1-plan amendment 18).
///
/// The one buffer for all planes suits both hwcontexts: VA-API copies
/// into any strides (`av_frame_copy` from a VAImage that FFmpeg creates,
/// has the driver fill and destroys for each frame), and Vulkan
/// copies into them through a staging buffer, or maps them for the GPU to
/// write directly when they qualify (one buffer holding every plane, at
/// the host pointer alignment), from FFmpeg 6.1 to 8.1.
pub(crate) struct DownloadPool {
    pool: *mut sys::AVBufferPool,
    /// The format, width and height the pool's buffers are laid out for,
    /// and how they are allocated.
    key: Option<(c_int, c_int, c_int, Backing)>,
    planes: usize,
    linesizes: [c_int; 4],
    offsets: [usize; 4],
    /// Pools built so far.
    builds: u64,
    /// Buffers every pool built so far has allocated: each takes a
    /// reference, which FFmpeg drops when it frees the pool, and counts
    /// in [`count`].
    allocated: Arc<AtomicU64>,
}

// SAFETY: an AVBufferPool is locked internally, and only the pool's
// owner takes buffers from it or rebuilds it (through `&mut self`).
unsafe impl Send for DownloadPool {}

impl std::fmt::Debug for DownloadPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadPool")
            .field("key", &self.key)
            .field("linesizes", &self.linesizes)
            .field("builds", &self.builds)
            .field("allocated", &self.allocated())
            .finish_non_exhaustive()
    }
}

impl Default for DownloadPool {
    fn default() -> Self {
        DownloadPool {
            pool: std::ptr::null_mut(),
            key: None,
            planes: 0,
            linesizes: [0; 4],
            offsets: [0; 4],
            builds: 0,
            allocated: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// How a [`DownloadPool`]'s buffers are allocated, by the device its
/// frames come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backing {
    /// `av_buffer_alloc`: VA-API's.
    Malloc,
    /// [`huge_alloc`]: Vulkan's, and any other device's.
    Huge,
}

impl Backing {
    /// For frames of the device type `kind` (an `AVHWDeviceType`).
    fn for_device(kind: c_int) -> Backing {
        if kind == sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI as c_int {
            Backing::Malloc
        } else {
            Backing::Huge
        }
    }
}

/// Counts a buffer in `opaque`, the pool's counter.
///
/// # Safety
///
/// `opaque` is the counter `DownloadPool::build` gave the pool, which
/// holds a reference to it until `release_counter`.
unsafe fn count(opaque: *mut c_void) {
    // SAFETY: the caller's guarantee.
    unsafe { (*opaque.cast::<AtomicU64>()).fetch_add(1, Ordering::Relaxed) };
}

/// The allocator of a [`Backing::Huge`] [`DownloadPool`]'s
/// `AVBufferPool`: [`huge_alloc`], counting each buffer in `opaque`, the
/// pool's counter. The pool calls it only when it has no buffer to hand
/// back.
unsafe extern "C" fn counted_huge(opaque: *mut c_void, size: usize) -> *mut sys::AVBufferRef {
    // SAFETY: `opaque` is the pool's counter, as `count` takes.
    unsafe { count(opaque) };
    huge_alloc(size)
}

/// [`counted_huge`] for a [`Backing::Malloc`] pool: `av_buffer_alloc`.
unsafe extern "C" fn counted_malloc(opaque: *mut c_void, size: usize) -> *mut sys::AVBufferRef {
    // SAFETY: `opaque` is the pool's counter, as `count` takes; and
    // av_buffer_alloc takes any size, NULL when out of memory.
    unsafe {
        count(opaque);
        sys::av_buffer_alloc(size)
    }
}

/// A transparent huge page, on x86-64 and on arm64 with 4 KiB pages.
const HUGE_PAGE: usize = 2 << 20;

/// A buffer of `size` bytes in a zeroed anonymous mapping of its own,
/// starting on a [`HUGE_PAGE`] boundary and rounded up to one, advised
/// `MADV_HUGEPAGE`, so that its first touch (the device's write, or
/// FFmpeg's copy) faults in 2 MiB at a time where THP is `madvise` or
/// `always`; freed with `munmap` ([`huge_free`]). NULL when out of
/// memory, as `av_buffer_alloc` would be.
fn huge_alloc(size: usize) -> *mut sys::AVBufferRef {
    let Some(len) = size.max(1).checked_next_multiple_of(HUGE_PAGE) else {
        return std::ptr::null_mut();
    };
    let Some(span) = len.checked_add(HUGE_PAGE) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a new private anonymous mapping, at an address the kernel
    // picks; nothing else refers to it.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            span,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if base == libc::MAP_FAILED {
        return std::ptr::null_mut();
    }
    // The aligned `len` bytes inside the span; the rest goes back.
    let head = base.cast::<u8>().align_offset(HUGE_PAGE);
    let tail = HUGE_PAGE - head;
    // SAFETY: `head` and `tail` are multiples of the page size (mmap
    // returns page-aligned addresses, and HUGE_PAGE is one), and each
    // range lies inside the mapping made above, which nothing uses yet.
    let data = unsafe {
        let data = base.cast::<u8>().add(head);
        if head > 0 {
            libc::munmap(base, head);
        }
        libc::munmap(data.add(len).cast(), tail);
        // Advice only: EINVAL where THP isn't built in, and nothing
        // where it is off.
        libc::madvise(data.cast(), len, libc::MADV_HUGEPAGE);
        data
    };
    // SAFETY: `data` is `len` ≥ `size` bytes, which the buffer owns from
    // here and frees with `huge_free`, given `len` as its opaque.
    let buf = unsafe {
        sys::av_buffer_create(
            data,
            size,
            Some(huge_free),
            std::ptr::without_provenance_mut(len),
            0,
        )
    };
    if buf.is_null() {
        // SAFETY: the mapping made above, which no buffer took.
        unsafe { libc::munmap(data.cast(), len) };
    }
    buf
}

/// Unmap a [`huge_alloc`] buffer: `opaque` is its mapping's length.
unsafe extern "C" fn huge_free(opaque: *mut c_void, data: *mut u8) {
    // SAFETY: `data` and the length are the mapping `huge_alloc` made for
    // this buffer, whose last reference has just gone.
    unsafe { libc::munmap(data.cast(), opaque.addr()) };
}

/// Drop the reference to its counter that a [`DownloadPool`]'s
/// `AVBufferPool` holds; FFmpeg calls it once, when it frees the pool,
/// after the last buffer has come back.
unsafe extern "C" fn release_counter(opaque: *mut c_void) {
    // SAFETY: the reference `DownloadPool::build` made for this pool with
    // `Arc::into_raw`, dropped only here.
    unsafe { drop(Arc::from_raw(opaque.cast::<AtomicU64>().cast_const())) };
}

impl Drop for DownloadPool {
    fn drop(&mut self) {
        // SAFETY: NULL or the pool this owns; FFmpeg frees it once every
        // buffer taken from it has come back.
        unsafe { sys::av_buffer_pool_uninit(&mut self.pool) };
    }
}

impl DownloadPool {
    /// Lay the pool out for `fmt` frames of `width` × `height`, its
    /// buffers allocated as `backing` says.
    fn build(
        &mut self,
        fmt: c_int,
        width: c_int,
        height: c_int,
        backing: Backing,
    ) -> Result<(), ff::Error> {
        // SAFETY: NULL or the pool this owns (see Drop).
        unsafe { sys::av_buffer_pool_uninit(&mut self.pool) };
        self.key = None;
        // SAFETY: accepts any value; negative for one it doesn't know.
        let planes = unsafe { c::av_pix_fmt_count_planes(fmt) };
        let planes = match usize::try_from(planes) {
            Ok(n @ 1..=4) => n,
            _ => return Err(ff::Error::InvalidData),
        };
        let mut linesizes = [0 as c_int; 4];
        let mut wide = [0isize; 4];
        for i in 0..planes {
            // SAFETY: accepts any format, width and plane; negative on
            // error.
            let row = unsafe { c::av_image_get_linesize(fmt, width, i as c_int) };
            let row = usize::try_from(row).map_err(|_| ff::Error::InvalidData)?;
            let aligned = row
                .checked_next_multiple_of(DOWNLOAD_ROW_ALIGN)
                .ok_or(ff::Error::InvalidData)?;
            linesizes[i] = c_int::try_from(aligned).map_err(|_| ff::Error::InvalidData)?;
            wide[i] = isize::try_from(aligned).map_err(|_| ff::Error::InvalidData)?;
        }
        let mut sizes = [0usize; 4];
        // SAFETY: four sizes out, four linesizes in, as the function takes.
        let ret =
            unsafe { c::av_image_fill_plane_sizes(sizes.as_mut_ptr(), fmt, height, wide.as_ptr()) };
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
        let mut offsets = [0usize; 4];
        let mut total = 0usize;
        for i in 0..planes {
            offsets[i] = total;
            total = total
                .checked_add(sizes[i])
                .and_then(|t| t.checked_next_multiple_of(DOWNLOAD_ROW_ALIGN))
                .ok_or(ff::Error::InvalidData)?;
        }
        // FFmpeg's padding past the last plane, which SIMD readers may
        // touch.
        let total = total
            .checked_add(sys::AV_INPUT_BUFFER_PADDING_SIZE as usize)
            .ok_or(ff::Error::InvalidData)?;
        let counter = Arc::into_raw(Arc::clone(&self.allocated))
            .cast_mut()
            .cast::<c_void>();
        let alloc = match backing {
            Backing::Malloc => counted_malloc,
            Backing::Huge => counted_huge,
        };
        // SAFETY: a size, and an allocator and a release function that
        // never unwind. The pool takes the counter's new reference, which
        // `release_counter` drops when FFmpeg frees it; NULL when out of
        // memory.
        let pool = unsafe {
            sys::av_buffer_pool_init2(total, counter, Some(alloc), Some(release_counter))
        };
        if pool.is_null() {
            // SAFETY: the reference made above, which no pool took.
            unsafe { release_counter(counter) };
            return Err(ff::Error::Other {
                errno: ff::util::error::ENOMEM,
            });
        }
        *self = DownloadPool {
            pool,
            key: Some((fmt, width, height, backing)),
            planes,
            linesizes,
            offsets,
            builds: self.builds + 1,
            allocated: Arc::clone(&self.allocated),
        };
        Ok(())
    }

    /// Buffers the pool has allocated, for every format and size it was
    /// laid out for: once it holds the frames kept alive, none more.
    pub(crate) fn allocated(&self) -> u64 {
        self.allocated.load(Ordering::Relaxed)
    }

    /// Pools built so far: 1 after the first download, one more for each
    /// change of format or size.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> u64 {
        self.builds
    }
}

/// Copy the hardware frame `src` into a buffer of `pool` (rebuilt first
/// when the frame's system-memory format, size or device is new), with its
/// properties and, as software decoding does ([`open_decoder`]), its left
/// and top cropping applied exactly: FFmpeg crops a device frame only on
/// the right and bottom.
pub(crate) fn download_into_pool(
    pool: &mut DownloadPool,
    src: &ff::frame::Video,
) -> Result<ff::frame::Video, ff::Error> {
    // SAFETY: `src` wraps a live AVFrame. A device frame's hw_frames_ctx
    // holds its AVHWFramesContext, whose sw_format is read as an int, and
    // whose device context, which it holds too, says its type, an int.
    let key = unsafe {
        let f = src.as_ptr();
        let frames = (*f).hw_frames_ctx;
        if frames.is_null() || (*frames).data.is_null() {
            return Err(ff::Error::InvalidData);
        }
        let fc = (*frames).data.cast::<sys::AVHWFramesContext>();
        let device = (*fc).device_ctx;
        if device.is_null() {
            return Err(ff::Error::InvalidData);
        }
        (
            enum_int(&raw const (*fc).sw_format),
            (*f).width,
            (*f).height,
            Backing::for_device(enum_int(&raw const (*device).type_)),
        )
    };
    if pool.key != Some(key) {
        pool.build(key.0, key.1, key.2, key.3)?;
    }
    // SAFETY: the pool is live (just built or kept); NULL when out of
    // memory.
    let buf = unsafe { sys::av_buffer_pool_get(pool.pool) };
    if buf.is_null() {
        return Err(ff::Error::Other {
            errno: ff::util::error::ENOMEM,
        });
    }
    let mut dst = ff::frame::Video::empty();
    // SAFETY: `dst` is a fresh frame, which takes over the reference to
    // `buf` (freed with it, which returns the buffer to the pool). Its
    // planes lie inside `buf` at the offsets and strides the pool was laid
    // out with for this format and size. `format` is the frames context's
    // own sw_format, so FFmpeg knows it. With `buf[0]` set,
    // av_hwframe_transfer_data copies into the planes as given.
    unsafe {
        let d = dst.as_mut_ptr();
        (*d).buf[0] = buf;
        (*d).format = key.0;
        (*d).width = key.1;
        (*d).height = key.2;
        for i in 0..pool.planes {
            (*d).data[i] = (*buf).data.add(pool.offsets[i]);
            (*d).linesize[i] = pool.linesizes[i];
        }
        let ret = sys::av_hwframe_transfer_data(d, src.as_ptr(), 0);
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
        let ret = sys::av_frame_copy_props(d, src.as_ptr());
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
        // Exactly, as software decoding crops (AV_CODEC_FLAG_UNALIGNED).
        // Aligned cropping would keep columns by NV12's rule where
        // software uses YUV420P's, and refuses P010 with AVERROR_BUG.
        let ret = sys::av_frame_apply_cropping(d, sys::AV_FRAME_CROP_UNALIGNED as c_int);
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
    }
    Ok(dst)
}

/// Copy the hardware frame `src` into system memory, as
/// [`download_into_pool`] does, through a pool of its own: for a frame
/// left on its device and read once (`VideoFrame::to_system`).
pub(crate) fn download(src: &ff::frame::Video) -> Result<ff::frame::Video, ff::Error> {
    let mut pool = DownloadPool::default();
    download_into_pool(&mut pool, src)
}

/// What a device frame becomes once downloaded by [`download_into_pool`]:
/// its frames context's system-memory format, and its size less the left
/// and top cropping still to apply. `None` when it has no frames context.
pub(crate) fn as_downloaded(frame: &ff::frame::Video, fields: FrameFields) -> Option<FrameFields> {
    // SAFETY: `frame` wraps a live AVFrame; as in `download_into_pool`.
    unsafe {
        let f = frame.as_ptr();
        let frames = (*f).hw_frames_ctx;
        if frames.is_null() || (*frames).data.is_null() {
            return None;
        }
        let fc = (*frames).data.cast::<sys::AVHWFramesContext>();
        let less = |n: u32, crop: usize| n.saturating_sub(u32::try_from(crop).unwrap_or(u32::MAX));
        Some(FrameFields {
            format: enum_int(&raw const (*fc).sw_format),
            width: less(fields.width, (*f).crop_left),
            height: less(fields.height, (*f).crop_top),
            ..fields
        })
    }
}

/// One layer of a DRM PRIME frame, as its descriptor has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrmLayer {
    pub fourcc: u32,
    /// Memory planes in the layer: 1 for a plane of NV12 or P010 as
    /// VA-API exports it with separate layers, more with a compression
    /// modifier's metadata planes.
    pub planes: usize,
    pub fd: c_int,
    pub modifier: u64,
    pub offset: u64,
    pub pitch: u64,
}

/// Map the VA-API frame `src` to DRM PRIME for reading: FFmpeg syncs the
/// surface and exports it with a layer per plane (FFmpeg's
/// `vaapi_map_to_drm_esh`). The mapped frame holds `src`'s surface and
/// the exported fds until it is freed.
pub(crate) fn map_drm_prime(src: &ff::frame::Video) -> Result<ff::frame::Video, ff::Error> {
    let mut dst = ff::frame::Video::empty();
    // SAFETY: `dst` is a fresh frame with no buffers; `src` wraps a live
    // device frame. av_hwframe_map with a DRM PRIME `format` and no
    // hw_frames_ctx maps into `dst`, which then refers to `src`'s buffers.
    unsafe {
        let d = dst.as_mut_ptr();
        (*d).format = sys::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int;
        let ret = sys::av_hwframe_map(d, src.as_ptr(), sys::AV_HWFRAME_MAP_READ as c_int);
        if ret < 0 {
            return Err(ff::Error::from(ret));
        }
    }
    Ok(dst)
}

/// The layers of a frame [`map_drm_prime`] made, each with its first
/// memory plane; `None` when it holds no descriptor or its counts are out
/// of range.
pub(crate) fn drm_layers(frame: &ff::frame::Video) -> Option<Vec<DrmLayer>> {
    // SAFETY: a DRM PRIME frame's data[0] is its AVDRMFrameDescriptor,
    // which lives as long as the frame; its counts are checked against
    // the arrays before any entry is read.
    unsafe {
        let f = frame.as_ptr();
        if (*f).format != sys::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int
            || (*f).data[0].is_null()
        {
            return None;
        }
        let desc = &*(*f).data[0].cast::<sys::AVDRMFrameDescriptor>();
        let objects = usize::try_from(desc.nb_objects).ok()?;
        let layers = usize::try_from(desc.nb_layers).ok()?;
        if objects > desc.objects.len() || layers > desc.layers.len() {
            return None;
        }
        let mut out = Vec::with_capacity(layers);
        for layer in &desc.layers[..layers] {
            let planes = usize::try_from(layer.nb_planes).ok()?;
            let plane = layer.planes.get(..planes)?.first()?;
            let object = desc.objects[..objects].get(usize::try_from(plane.object_index).ok()?)?;
            out.push(DrmLayer {
                fourcc: layer.format,
                planes,
                fd: object.fd,
                modifier: object.format_modifier,
                offset: u64::try_from(plane.offset).ok()?,
                pitch: u64::try_from(plane.pitch).ok()?,
            });
        }
        Some(out)
    }
}

/// The left and top cropping still to apply to a device frame, which
/// FFmpeg leaves (it crops a device frame only on the right and bottom).
pub(crate) fn crop_left_top(frame: &ff::frame::Video) -> (usize, usize) {
    // SAFETY: `frame` wraps a live AVFrame; two size_t fields.
    unsafe {
        let f = frame.as_ptr();
        ((*f).crop_left, (*f).crop_top)
    }
}

/// Have an unopened hardware decoder keep `n` surfaces beyond what it
/// needs itself, for frames the caller holds (`extra_hw_frames`).
pub(crate) fn set_extra_hw_frames(ctx: &mut ff::codec::context::Context, n: u8) {
    // SAFETY: `ctx` owns a live AVCodecContext that is not open yet; a
    // plain int field read when the decoder sets up its frames context.
    unsafe { (*ctx.as_mut_ptr()).extra_hw_frames = c_int::from(n) };
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
    use std::path::PathBuf;
    use std::sync::{Mutex, PoisonError};

    use tracing::subscriber::Interest;
    use tracing::{Event, Level, Metadata, Subscriber, field, span};

    use super::*;

    /// The most verbose "ffmpeg" level [`Capture`] accepts, `None` for none.
    static MAX: Mutex<Option<Level>> = Mutex::new(Some(Level::TRACE));
    /// Held by tests that set FFmpeg's log level, which is process-wide,
    /// and by tests that read FFmpeg's log through `capture_ffmpeg_log`.
    static LOG_LEVEL: Mutex<()> = Mutex::new(());
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
    fn a_device_type_this_ffmpeg_lacks_is_not_built_in() {
        use sys::AVHWDeviceType as T;
        let (vaapi, vulkan) = (
            T::AV_HWDEVICE_TYPE_VAAPI as c_int,
            T::AV_HWDEVICE_TYPE_VULKAN as c_int,
        );
        assert_eq!(built_in("vaapi", vaapi, &[vaapi, vulkan]).ok(), Some(vaapi));
        for (kind, t, built) in [
            ("vulkan", vulkan, &[vaapi][..]),
            ("vaapi", vaapi, &[vulkan][..]),
            ("nosuch", HW_NONE, &[vaapi, vulkan][..]),
        ] {
            match built_in(kind, t, built) {
                Err(e @ HwError::NotBuiltIn(_)) => assert!(
                    e.to_string().ends_with(&format!(
                        "without {} support",
                        crate::hwprobe::device_label(kind)
                    )),
                    "{e}"
                ),
                r => panic!("{kind}: {r:?}"),
            }
        }
        // This FFmpeg knows the names of types no Linux build has, and must
        // not take them for built in.
        let built = hw_device_types();
        for kind in [
            "videotoolbox",
            "d3d11va",
            "dxva2",
            "mediacodec",
            "vaapi",
            "vulkan",
        ] {
            match device_type(kind) {
                Ok(_) => assert!(built.iter().any(|b| b == kind), "{kind}: {built:?}"),
                Err(HwError::NotBuiltIn(_)) => {
                    assert!(!built.iter().any(|b| b == kind), "{kind}: {built:?}")
                }
                Err(e) => panic!("{kind}: {e}"),
            }
        }
        assert!(matches!(
            HwDevice::open("videotoolbox", None),
            Err(HwError::NotBuiltIn(_))
        ));
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
        hevc_picture(ff::format::Pixel::YUV420P, |_, _, _| 128)
    }

    /// One 512x128 frame of 4:2:0 `format` (YUV420P or YUV420P10LE),
    /// whose sample at `(x, y)` of plane `i` is `value(i, x, y)`, encoded
    /// as Annex B HEVC by libx265.
    fn hevc_picture(
        format: ff::format::Pixel,
        value: impl Fn(usize, usize, usize) -> u16,
    ) -> Vec<ff::Packet> {
        let codec = ff::encoder::find_by_name("libx265").unwrap();
        let mut enc = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .unwrap();
        enc.set_width(512);
        enc.set_height(128);
        enc.set_format(format);
        enc.set_time_base((1, 30));
        let mut opts = ff::Dictionary::new();
        opts.set("preset", "ultrafast");
        opts.set("x265-params", "log-level=error");
        let mut enc = enc.open_as_with(codec, opts).unwrap();
        let mut frame = ff::frame::Video::new(format, 512, 128);
        let wide = format == ff::format::Pixel::YUV420P10LE;
        for (i, (w, h)) in [(512, 128), (256, 64), (256, 64)].into_iter().enumerate() {
            let stride = frame.stride(i);
            for (y, row) in frame.data_mut(i).chunks_mut(stride).take(h).enumerate() {
                for x in 0..w {
                    let v = value(i, x, y);
                    if wide {
                        row[2 * x..2 * x + 2].copy_from_slice(&v.to_le_bytes());
                    } else {
                        row[x] = v as u8;
                    }
                }
            }
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
        crop(packets, left, 0)
    }

    /// `packets` through FFmpeg's hevc_metadata filter, which sets the
    /// SPS conformance window to crop `left` luma samples on the left and
    /// `top` at the top.
    fn crop(packets: Vec<ff::Packet>, left: i64, top: i64) -> Vec<ff::Packet> {
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
            if top > 0 {
                let set = sys::av_opt_set_int((*bsf).priv_data, c"crop_top".as_ptr(), top, 0);
                assert!(set >= 0);
            }
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
        let mut dec = open_decoder(ff::codec::context::Context::new_with_codec(codec), codec)
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
        let _level = LOG_LEVEL.lock().unwrap_or_else(PoisonError::into_inner);
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

        // A capture gets the lines of its own run, each whole: a line this
        // thread left unfinished before it is ended first, and one its run
        // leaves unfinished is ended inside it.
        log(sys::AV_LOG_WARNING, "capture-test before ");
        let ((), lines) = crate::hwprobe::capture_ffmpeg_log(|| {
            log(sys::AV_LOG_WARNING, "capture-test inside\n");
            log(sys::AV_LOG_WARNING, "capture-test after");
        });
        assert_eq!(lines, ["capture-test inside", "capture-test after"]);
        assert_eq!(
            captured("capture-test"),
            ["before", "inside", "after"].map(|w| (Level::WARN, format!("capture-test {w}")))
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

        // A hardware decode's capture also keeps what FFmpeg's Vulkan
        // decoder says at "verbose" when it turns a stream down, though
        // the subscriber reads only warnings, and no other verbose line.
        let ((), lines) = crate::hwprobe::capture_hw_decode_log(|| {
            // SAFETY: reads a global int.
            assert_eq!(unsafe { sys::av_log_get_level() }, sys::AV_LOG_VERBOSE);
            log(
                sys::AV_LOG_VERBOSE,
                "Decoder capabilities for hevc profile \"Main\":\n",
            );
            log(sys::AV_LOG_VERBOSE, "    Maximum level: 153 (stream 186)\n");
            log(sys::AV_LOG_VERBOSE, "    Maximum references: 17\n");
            log(sys::AV_LOG_INFO, "verbose-test info\n");
            log(
                sys::AV_LOG_ERROR,
                "verbose-test Failed setup for format vulkan: hwaccel initialisation returned \
                 error.\n",
            );
        });
        assert_eq!(
            lines,
            [
                "Decoder capabilities for hevc profile \"Main\":",
                "Maximum level: 153 (stream 186)",
                "verbose-test Failed setup for format vulkan: hwaccel initialisation returned \
                 error.",
            ]
        );
        assert_eq!(captured("verbose-test").len(), 1);
        assert!(captured("Maximum").is_empty());
        // SAFETY: reads a global int.
        assert_eq!(unsafe { sys::av_log_get_level() }, sys::AV_LOG_WARNING);
        set_max(None);
        // SAFETY: reads a global int.
        assert_eq!(unsafe { sys::av_log_get_level() }, sys::AV_LOG_QUIET);
        log(sys::AV_LOG_PANIC, "off-test\n");
        assert!(captured("off-test").is_empty());
    }

    /// A test's own directory in the system's temp dir, removed with what
    /// it holds when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("spherecut-ffi-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn write(&self, name: &str, data: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, data).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Three 256x128 frames of H.264 in MP4.
    fn clip(dir: &TempDir, name: &str) -> PathBuf {
        clip_with(dir, name, "libx264", &[])
    }

    /// Three 256x128 frames in MP4, from `encoder` with `options`.
    fn clip_with(dir: &TempDir, name: &str, encoder: &str, options: &[(&str, &str)]) -> PathBuf {
        use crate::encode::{ColorTags, Mp4Writer, PixelLayout, VideoSettings, YuvPlanes};
        let settings = VideoSettings {
            encoder: encoder.into(),
            width: 256,
            height: 128,
            rate: spherecut_core::FrameRate::FPS_30,
            layout: PixelLayout::Yuv420p,
            options: options
                .iter()
                .map(|&(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            color: ColorTags::BT709_LIMITED,
            edit_start: 0,
            time_base: None,
            crop_left: 0,
            device: None,
        };
        let mut w = Mp4Writer::create(&dir.0.join(name), &settings, None).unwrap();
        for i in 0..3 {
            let luma = vec![16 + 20 * i; 256 * 128];
            let chroma = vec![128; 128 * 64];
            w.push_video(&YuvPlanes::U8([luma, chroma.clone(), chroma]))
                .unwrap();
        }
        w.finish().unwrap()
    }

    #[test]
    fn file_urls_hold_the_path_as_it_is() {
        use std::ffi::OsStr;
        for (path, url) in [
            (&b"concat:x.mp4"[..], &b"file:concat:x.mp4"[..]),
            (
                b"http://127.0.0.1:1/x.mp4",
                b"file:http://127.0.0.1:1/x.mp4",
            ),
            (b"file:x.mp4", b"file:file:x.mp4"),
            (b"/tmp/caf\xe9 %d?.mp4", b"file:/tmp/caf\xe9 %d?.mp4"),
        ] {
            let got = file_url(Path::new(OsStr::from_bytes(path))).unwrap();
            assert_eq!(got.as_bytes(), url);
        }
    }

    #[test]
    fn opened_files_are_confined() {
        crate::init();
        let dir = TempDir::new("confined");
        let ictx = open_input(&clip(&dir, "clip.mp4"), None).unwrap();
        // SAFETY: `ictx` owns a live, open context; its whitelists are
        // NULL or strings, and the MOV demuxer's private data has the
        // enable_drefs option.
        let (protocols, formats, io_open, drefs) = unsafe {
            let s = ictx.as_ptr();
            let mut drefs = -1;
            let ret = sys::av_opt_get_int((*s).priv_data, c"enable_drefs".as_ptr(), 0, &mut drefs);
            assert!(ret >= 0);
            (
                cstr_opt((*s).protocol_whitelist),
                cstr_opt((*s).format_whitelist),
                (*s).io_open,
                drefs,
            )
        };
        assert_eq!(protocols.as_deref(), Some("file"));
        assert_eq!(
            formats.as_deref(),
            Some("mov,mp4,m4a,3gp,3g2,mj2,matroska,webm")
        );
        let ours: IoOpen = open_only_the_file;
        assert_eq!(io_open.map(|f| f as usize), Some(ours as usize));
        assert_eq!(
            drefs, 0,
            "the MOV demuxer must not follow external references"
        );
    }

    #[test]
    fn other_containers_are_refused_before_their_demuxer_runs() {
        crate::init();
        let dir = TempDir::new("refused");
        let playlist = dir.write(
            "list.m3u8",
            b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\nhttp://127.0.0.1:1/a.ts\n#EXT-X-ENDLIST\n",
        );
        match open_input(&playlist, None) {
            Err(OpenError::Container {
                format,
                description,
            }) => {
                assert_eq!(format, "hls");
                assert_eq!(description, "Apple HTTP Live Streaming");
            }
            r => panic!(
                "expected the playlist to be refused, got {:?}",
                r.map(|_| ())
            ),
        }
        assert!(matches!(open_input(&dir.0, None), Err(OpenError::NotAFile)));
        assert!(matches!(
            open_input(&dir.0.join("missing.mp4"), None),
            Err(OpenError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn nested_opens_are_refused_and_named() {
        // With the HLS demuxer let in, which Spherecut never does, its
        // segments are what io_open refuses: one outside the playlist's
        // folder, and one on the network.
        crate::init();
        let dir = TempDir::new("nested");
        std::fs::create_dir_all(dir.0.join("inside")).unwrap();
        std::fs::copy(clip(&dir, "outside.mp4"), dir.0.join("outside.ts")).unwrap();
        for (name, segment, want) in [
            // FFmpeg resolves the segment against the playlist's URL.
            ("local.m3u8", "../outside.ts", "inside/../outside.ts"),
            (
                "remote.m3u8",
                "http://127.0.0.1:1/a.ts",
                "http://127.0.0.1:1/a.ts",
            ),
        ] {
            let playlist = dir.write(
                &format!("inside/{name}"),
                format!(
                    "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\n{segment}\n#EXT-X-ENDLIST\n"
                )
                .as_bytes(),
            );
            match open_input_with(&playlist, None, &["hls"], MAX_PIXELS) {
                Err(OpenError::Nested { url }) => assert!(url.ends_with(want), "{url}"),
                r => panic!("{name}: expected a refused open, got {:?}", r.map(|_| ())),
            }
        }
    }

    #[test]
    fn the_decoders_that_find_stream_info_opens_are_limited() {
        let _level = LOG_LEVEL.lock().unwrap_or_else(PoisonError::into_inner);
        crate::init();
        let dir = TempDir::new("stream-info");
        let clip = clip(&dir, "clip.mp4");
        let (r, log) =
            crate::hwprobe::capture_ffmpeg_log(|| open_input_with(&clip, None, &DEMUXERS, 1000));
        r.unwrap();
        assert!(
            log.iter()
                .any(|l| l.contains("Picture size 256x128 exceeds specified max pixel count 1000")),
            "{log:?}"
        );
        let (r, log) = crate::hwprobe::capture_ffmpeg_log(|| open_input(&clip, None));
        r.unwrap();
        assert!(log.is_empty(), "{log:?}");
    }

    #[test]
    fn a_stream_over_the_limit_keeps_its_size() {
        // A decoder that refuses the size leaves an MJPEG stream at 0x0,
        // where H.264's parser sets the size again from the bitstream.
        // Either way the stream has the size its file gives, for
        // `TooLarge` to name.
        let _level = LOG_LEVEL.lock().unwrap_or_else(PoisonError::into_inner);
        crate::init();
        let dir = TempDir::new("over-limit");
        let mjpeg = clip_with(&dir, "mjpeg.mp4", "mjpeg", &[("strict", "-1")]);
        for clip in [clip(&dir, "h264.mp4"), mjpeg] {
            let (ictx, log) = crate::hwprobe::capture_ffmpeg_log(|| {
                open_input_with(&clip, None, &DEMUXERS, 1000).unwrap()
            });
            assert!(
                log.iter()
                    .any(|l| l.contains("exceeds specified max pixel count 1000")),
                "{}: {log:?}",
                clip.display()
            );
            let f = codecpar_fields(&ictx.stream(0).unwrap().parameters());
            assert_eq!((f.width, f.height), (256, 128), "{}", clip.display());
        }
    }

    #[test]
    fn too_large_pictures_are_named_by_size() {
        // The message says 2^27 and 16384x8192.
        assert_eq!(MAX_PIXELS, 1 << 27);
        assert_eq!(MAX_PIXELS, 16384 * 8192);
        assert_eq!(TooLarge::of(16384, 8192), None);
        assert_eq!(TooLarge::of(11520, 5760), None);
        assert_eq!(TooLarge::of(0, u32::MAX), None);
        // 16383 pads to 16384, and 16320 is padded already.
        assert_eq!(TooLarge::of(16383, 8192), None);
        assert_eq!(TooLarge::of(16320, 8224), None);
        assert_eq!(
            TooLarge::of(16384, 8193),
            Some(TooLarge {
                width: 16384,
                height: 8193
            })
        );
        assert!(TooLarge::of(u32::MAX, u32::MAX).is_some());
        assert_eq!(
            TooLarge::of(16400, 8200).unwrap().to_string(),
            "16400x8200, more than the 2^27 pixels (16384x8192) per picture that Spherecut decodes"
        );
        // Fewer than 2^27 pixels, but not once FFmpeg pads the rows.
        assert_eq!(
            TooLarge::of(16380, 8194).unwrap().to_string(),
            "16380x8194, which FFmpeg may pad to 16384x8194, more than the 2^27 pixels (16384x8192) per picture that Spherecut decodes"
        );
    }

    #[test]
    fn decoders_are_limited_to_max_pixels() {
        // The frame is 512x128: 65536 pixels.
        let decode = |max: i64| {
            let codec = ff::decoder::find_by_name("hevc").unwrap();
            let ctx = ff::codec::context::Context::new_with_codec(codec);
            let mut dec = open_decoder_with(ctx, codec, max)?.video()?;
            for p in hevc_frame() {
                dec.send_packet(&p)?;
            }
            dec.send_eof()?;
            let mut frame = ff::frame::Video::empty();
            dec.receive_frame(&mut frame).map(|()| frame.width())
        };
        assert_eq!(decode(65536).unwrap(), 512);
        assert!(decode(65535).is_err());

        let codec = ff::decoder::find_by_name("hevc").unwrap();
        let dec = open_decoder(ff::codec::context::Context::new_with_codec(codec), codec).unwrap();
        // SAFETY: an open decoder context; a plain int64 field.
        assert_eq!(unsafe { (*dec.as_ptr()).max_pixels }, MAX_PIXELS);
    }

    fn fmt(p: sys::AVPixelFormat) -> c_int {
        p as c_int
    }

    #[test]
    fn native_pixel_layouts_come_from_the_descriptor() {
        use ColorFamily::{Gray, Rgb, Yuv};
        use sys::AVPixelFormat as P;
        let plane = |components, bytes, log2_sub| PlaneDesc {
            components,
            bytes,
            log2_sub,
        };
        let (one, two) = (SampleBytes::One, SampleBytes::Two);
        let planar = |bytes, sub| {
            vec![
                plane(1, bytes, (0, 0)),
                plane(1, bytes, sub),
                plane(1, bytes, sub),
            ]
        };
        let semi = |bytes, sub| vec![plane(1, bytes, (0, 0)), plane(2, bytes, sub)];
        // Format, family, planes, depth, MSB-aligned.
        let native: Vec<(P, ColorFamily, Vec<PlaneDesc>, u8, bool)> = vec![
            (P::AV_PIX_FMT_NV12, Yuv, semi(one, (1, 1)), 8, false),
            (P::AV_PIX_FMT_NV16, Yuv, semi(one, (1, 0)), 8, false),
            (P::AV_PIX_FMT_P010LE, Yuv, semi(two, (1, 1)), 10, true),
            (P::AV_PIX_FMT_P210LE, Yuv, semi(two, (1, 0)), 10, true),
            // At 16 bits the high and low bits are the same.
            (P::AV_PIX_FMT_P016LE, Yuv, semi(two, (1, 1)), 16, false),
            (P::AV_PIX_FMT_YUV420P, Yuv, planar(one, (1, 1)), 8, false),
            (
                P::AV_PIX_FMT_YUV420P10LE,
                Yuv,
                planar(two, (1, 1)),
                10,
                false,
            ),
            (
                P::AV_PIX_FMT_YUV420P12LE,
                Yuv,
                planar(two, (1, 1)),
                12,
                false,
            ),
            (
                P::AV_PIX_FMT_YUV420P16LE,
                Yuv,
                planar(two, (1, 1)),
                16,
                false,
            ),
            (P::AV_PIX_FMT_YUV422P, Yuv, planar(one, (1, 0)), 8, false),
            (
                P::AV_PIX_FMT_YUV422P10LE,
                Yuv,
                planar(two, (1, 0)),
                10,
                false,
            ),
            (
                P::AV_PIX_FMT_YUV422P12LE,
                Yuv,
                planar(two, (1, 0)),
                12,
                false,
            ),
            (
                P::AV_PIX_FMT_YUV422P16LE,
                Yuv,
                planar(two, (1, 0)),
                16,
                false,
            ),
            (P::AV_PIX_FMT_YUV444P, Yuv, planar(one, (0, 0)), 8, false),
            (
                P::AV_PIX_FMT_YUV444P10LE,
                Yuv,
                planar(two, (0, 0)),
                10,
                false,
            ),
            (
                P::AV_PIX_FMT_YUV444P12LE,
                Yuv,
                planar(two, (0, 0)),
                12,
                false,
            ),
            (
                P::AV_PIX_FMT_YUV444P16LE,
                Yuv,
                planar(two, (0, 0)),
                16,
                false,
            ),
            (P::AV_PIX_FMT_YUVJ420P, Yuv, planar(one, (1, 1)), 8, false),
            (P::AV_PIX_FMT_YUVJ422P, Yuv, planar(one, (1, 0)), 8, false),
            (P::AV_PIX_FMT_YUVJ444P, Yuv, planar(one, (0, 0)), 8, false),
            (
                P::AV_PIX_FMT_GRAY8,
                Gray,
                vec![plane(1, one, (0, 0))],
                8,
                false,
            ),
            (
                P::AV_PIX_FMT_GRAY10LE,
                Gray,
                vec![plane(1, two, (0, 0))],
                10,
                false,
            ),
            (
                P::AV_PIX_FMT_GRAY12LE,
                Gray,
                vec![plane(1, two, (0, 0))],
                12,
                false,
            ),
            (
                P::AV_PIX_FMT_GBRP,
                Rgb,
                vec![plane(1, one, (0, 0)); 3],
                8,
                false,
            ),
            (
                P::AV_PIX_FMT_GBRP10LE,
                Rgb,
                vec![plane(1, two, (0, 0)); 3],
                10,
                false,
            ),
            (
                P::AV_PIX_FMT_GBRP12LE,
                Rgb,
                vec![plane(1, two, (0, 0)); 3],
                12,
                false,
            ),
            // Alpha planes are left out.
            (P::AV_PIX_FMT_YUVA420P, Yuv, planar(one, (1, 1)), 8, false),
            (
                P::AV_PIX_FMT_GBRAP,
                Rgb,
                vec![plane(1, one, (0, 0)); 3],
                8,
                false,
            ),
            // Others of the same shape, by the same rule.
            (P::AV_PIX_FMT_YUV440P, Yuv, planar(one, (0, 1)), 8, false),
            (P::AV_PIX_FMT_YUV420P9LE, Yuv, planar(two, (1, 1)), 9, false),
            (P::AV_PIX_FMT_NV24, Yuv, semi(one, (0, 0)), 8, false),
        ];
        for (p, family, planes, depth, msb) in native {
            let name = pix_fmt_name(fmt(p)).unwrap();
            let l = pixel_layout(fmt(p)).unwrap_or_else(|| panic!("{name} is not native"));
            assert_eq!(l.name, name);
            assert_eq!(
                (l.family, &l.planes, l.bit_depth, l.msb_aligned),
                (family, &planes, depth, msb),
                "{name}"
            );
        }
        // Packed, big-endian, palette, RGB in other orders, V before U,
        // bitstream, float, Bayer and hardware formats are converted.
        for p in [
            P::AV_PIX_FMT_YUYV422,
            P::AV_PIX_FMT_UYVY422,
            P::AV_PIX_FMT_RGB24,
            P::AV_PIX_FMT_RGBA,
            P::AV_PIX_FMT_BGR0,
            P::AV_PIX_FMT_PAL8,
            P::AV_PIX_FMT_NV21,
            P::AV_PIX_FMT_YUV420P10BE,
            P::AV_PIX_FMT_GRAY16BE,
            P::AV_PIX_FMT_P010BE,
            P::AV_PIX_FMT_MONOWHITE,
            P::AV_PIX_FMT_GBRPF32LE,
            P::AV_PIX_FMT_BAYER_RGGB8,
            P::AV_PIX_FMT_XYZ12LE,
            P::AV_PIX_FMT_X2RGB10LE,
            P::AV_PIX_FMT_VAAPI,
        ] {
            assert!(pixel_layout(fmt(p)).is_none(), "{:?}", pix_fmt_name(fmt(p)));
        }
        assert!(pixel_layout(-1).is_none());
        assert!(pixel_layout(100_000).is_none());
    }

    #[test]
    fn converted_formats_keep_their_kind_subsampling_and_depth() {
        use sys::AVPixelFormat as P;
        let cases = [
            (P::AV_PIX_FMT_YUYV422, P::AV_PIX_FMT_YUV422P),
            (P::AV_PIX_FMT_UYVY422, P::AV_PIX_FMT_YUV422P),
            (P::AV_PIX_FMT_NV21, P::AV_PIX_FMT_YUV420P),
            (P::AV_PIX_FMT_YUV420P10BE, P::AV_PIX_FMT_YUV420P10LE),
            (P::AV_PIX_FMT_Y210LE, P::AV_PIX_FMT_YUV422P10LE),
            (P::AV_PIX_FMT_YUV411P, P::AV_PIX_FMT_YUV444P),
            (P::AV_PIX_FMT_RGB24, P::AV_PIX_FMT_GBRP),
            (P::AV_PIX_FMT_RGBA, P::AV_PIX_FMT_GBRP),
            (P::AV_PIX_FMT_X2RGB10LE, P::AV_PIX_FMT_GBRP10LE),
            (P::AV_PIX_FMT_RGB48BE, P::AV_PIX_FMT_GBRP16LE),
            (P::AV_PIX_FMT_PAL8, P::AV_PIX_FMT_GBRP),
            (P::AV_PIX_FMT_BAYER_RGGB8, P::AV_PIX_FMT_GBRP),
            (P::AV_PIX_FMT_XYZ12LE, P::AV_PIX_FMT_GBRP12LE),
            (P::AV_PIX_FMT_GRAY16BE, P::AV_PIX_FMT_GRAY16LE),
            (P::AV_PIX_FMT_MONOWHITE, P::AV_PIX_FMT_GRAY8),
            (P::AV_PIX_FMT_YA8, P::AV_PIX_FMT_GRAY8),
        ];
        for (from, to) in cases {
            let name = |p| pix_fmt_name(fmt(p)).unwrap();
            assert_eq!(
                conversion_target(fmt(from)).map(|t| pix_fmt_name(t).unwrap()),
                Some(name(to)),
                "{}",
                name(from)
            );
            // Every target is native.
            assert!(pixel_layout(fmt(to)).is_some(), "{}", name(to));
        }
        assert_eq!(conversion_target(100_000), None);
    }

    #[test]
    fn swscale_repacks_exactly_and_keeps_timestamps_and_tags() {
        use sys::AVPixelFormat as P;
        let (w, h) = (6u32, 4u32);
        // UYVY: U0 Y0 V0 Y1 for each pair of pixels.
        let mut src = ff::frame::Video::new(ff::format::Pixel::UYVY422, w, h);
        let stride = src.stride(0);
        for y in 0..h as usize {
            for x in 0..w as usize / 2 {
                let o = y * stride + x * 4;
                let px = (y * 3 + x) as u8;
                src.data_mut(0)[o..o + 4].copy_from_slice(&[
                    100 + px,
                    16 + 2 * px,
                    200 - px,
                    17 + 2 * px,
                ]);
            }
        }
        src.set_pts(Some(1234));
        src.set_color_range(ff::util::color::Range::JPEG);
        let mut s =
            Scaler::new(fmt(P::AV_PIX_FMT_UYVY422), fmt(P::AV_PIX_FMT_YUV422P), w, h).unwrap();
        let out = s.scale(&src).unwrap();
        let f = frame_fields(&out);
        assert_eq!(
            (f.format, f.width, f.height),
            (fmt(P::AV_PIX_FMT_YUV422P), w, h)
        );
        assert_eq!(f.pts, Some(1234));
        assert_eq!(f.color.range, 2);
        let (luma, ls) = plane_and_stride(&out, 0).unwrap();
        let (u, us) = plane_and_stride(&out, 1).unwrap();
        let (v, vs) = plane_and_stride(&out, 2).unwrap();
        for y in 0..h as usize {
            for x in 0..w as usize / 2 {
                let px = (y * 3 + x) as u8;
                assert_eq!(u[y * us + x], 100 + px);
                assert_eq!(v[y * vs + x], 200 - px);
                assert_eq!(luma[y * ls + 2 * x], 16 + 2 * px);
                assert_eq!(luma[y * ls + 2 * x + 1], 17 + 2 * px);
            }
        }
        // A frame of another size or format is refused, not misread.
        let other = ff::frame::Video::new(ff::format::Pixel::UYVY422, w + 2, h);
        assert!(s.scale(&other).is_err());
    }

    #[test]
    fn swscale_refuses_formats_it_does_not_take() {
        use sys::AVPixelFormat as P;
        assert_eq!(
            Scaler::new(fmt(P::AV_PIX_FMT_VAAPI), fmt(P::AV_PIX_FMT_YUV420P), 64, 64).unwrap_err(),
            ScaleError::Input("vaapi".into())
        );
        assert_eq!(
            Scaler::new(fmt(P::AV_PIX_FMT_YUV420P), fmt(P::AV_PIX_FMT_VAAPI), 64, 64).unwrap_err(),
            ScaleError::Output("vaapi".into())
        );
        assert!(matches!(
            Scaler::new(
                fmt(P::AV_PIX_FMT_YUV420P),
                fmt(P::AV_PIX_FMT_YUV444P),
                0,
                64
            ),
            Err(ScaleError::Refused { .. })
        ));
    }

    /// A device frame of FFmpeg's Vulkan device on lavapipe, `w`x`h` in
    /// `sw_format`, holding hwprobe's ramp; the frames context with it.
    fn lavapipe_frame(
        sw_format: ff::format::Pixel,
        w: u32,
        h: u32,
    ) -> (
        ff::frame::Video,
        ff::frame::Video,
        crate::hwprobe::BufferRef,
    ) {
        let src = crate::hwprobe::ramp(sw_format, w, h);
        let (surface, frames) = lavapipe_upload(&src);
        (surface, src, frames)
    }

    /// A device frame of FFmpeg's Vulkan device on lavapipe holding `src`,
    /// a system-memory frame in NV12 or P010; the frames context with it.
    /// `src` must outlive the device frame's first download: with FFmpeg
    /// 6.1 on lavapipe, one dropped before it downloads as garbage.
    fn lavapipe_upload(src: &ff::frame::Video) -> (ff::frame::Video, crate::hwprobe::BufferRef) {
        let dev = HwDevice::open("vulkan", Some("llvmpipe")).unwrap_or_else(|e| {
            panic!(
                "{e}; Tier 1 needs FFmpeg's Vulkan device on Mesa's lavapipe \
                 (mesa-vulkan-drivers on Fedora and Ubuntu, vulkan-swrast on Arch)"
            )
        });
        let frames =
            crate::hwprobe::hw_frames(&dev, src.format(), src.width(), src.height()).unwrap();
        let mut surface = ff::frame::Video::empty();
        // SAFETY: `frames` is an initialised frames context, which gives
        // `surface` one of its frames; `src` is a system-memory frame of
        // its sw_format and size.
        unsafe {
            assert_eq!(
                sys::av_hwframe_get_buffer(frames.0, surface.as_mut_ptr(), 0),
                0
            );
            assert_eq!(
                sys::av_hwframe_transfer_data(surface.as_mut_ptr(), src.as_ptr(), 0),
                0
            );
        }
        (surface, frames)
    }

    /// Plane `i`'s rows of `w` bytes, without padding.
    fn rows(f: &ff::frame::Video, i: usize, w: usize, h: usize) -> Vec<u8> {
        let (p, stride) = plane_and_stride(f, i).unwrap();
        (0..h)
            .flat_map(|y| p[y * stride..y * stride + w].to_vec())
            .collect()
    }

    #[test]
    fn downloads_reuse_pooled_buffers_with_rows_256_bytes_apart() {
        // 250 bytes of luma a row, rounded up to 256.
        let (w, h) = (250, 66);
        let (surface, src, _frames) = lavapipe_frame(ff::format::Pixel::NV12, w, h);
        let mut pool = DownloadPool::default();
        let counter = Arc::clone(&pool.allocated);
        assert_eq!(pool.allocated(), 0);
        let a = download_into_pool(&mut pool, &surface).unwrap();
        assert_eq!(pool.allocated(), 1);
        // A huge_alloc buffer, the luma plane at its start.
        assert_eq!(data_ptr(&a, 0).addr() % HUGE_PAGE, 0);
        let nv12 = sys::AVPixelFormat::AV_PIX_FMT_NV12 as c_int;
        assert_eq!((frame_format(&a), a.width(), a.height()), (nv12, w, h));
        for i in 0..2 {
            let (_, stride) = plane_and_stride(&a, i).unwrap();
            assert_eq!(stride, 256, "plane {i}");
            let ph = if i == 0 { h } else { h / 2 } as usize;
            assert_eq!(rows(&a, i, w as usize, ph), rows(&src, i, w as usize, ph));
        }
        // Both planes in the one buffer, and FFmpeg's padding.
        assert_eq!(
            buffer_bytes(&a),
            256 * 66 + 256 * 33 + sys::AV_INPUT_BUFFER_PADDING_SIZE as usize
        );
        // A frame dropped gives its buffer back to the pool.
        let first = data_ptr(&a, 0);
        drop(a);
        let b = download_into_pool(&mut pool, &surface).unwrap();
        assert_eq!(data_ptr(&b, 0), first);
        assert_eq!(pool.allocated(), 1);
        // One still held isn't handed out again.
        let c = download_into_pool(&mut pool, &surface).unwrap();
        assert_ne!(data_ptr(&c, 0), first);
        assert_eq!(pool.builds(), 1);
        assert_eq!(pool.allocated(), 2);

        // Another size, as after LayoutChanged, rebuilds the pool; the
        // frames of the old one stay good.
        let (small, small_src, _small_frames) = lavapipe_frame(ff::format::Pixel::NV12, 64, 32);
        let d = download_into_pool(&mut pool, &small).unwrap();
        assert_eq!(pool.builds(), 2);
        assert_eq!((d.width(), d.height()), (64, 32));
        assert_eq!(plane_and_stride(&d, 0).unwrap().1, 256);
        assert_eq!(rows(&d, 0, 64, 32), rows(&small_src, 0, 64, 32));
        assert_eq!(
            rows(&b, 0, w as usize, h as usize),
            rows(&src, 0, w as usize, h as usize)
        );
        let again = data_ptr(&d, 0);
        assert_eq!(pool.allocated(), 3);
        drop((b, c, d));
        let e = download_into_pool(&mut pool, &small).unwrap();
        assert_eq!(data_ptr(&e, 0), again);
        assert_eq!(pool.allocated(), 3);
        // Each pool keeps the counter until FFmpeg frees it, once its last
        // buffer is back: the old pool's went with b and c, this one's
        // goes with e, after the pool itself.
        assert_eq!(Arc::strong_count(&counter), 3);
        drop(pool);
        assert_eq!(Arc::strong_count(&counter), 2);
        drop(e);
        assert_eq!(Arc::strong_count(&counter), 1);
    }

    #[test]
    fn vaapi_downloads_go_to_malloc_and_vulkans_to_huge_pages() {
        use sys::AVHWDeviceType as T;
        assert_eq!(
            Backing::for_device(T::AV_HWDEVICE_TYPE_VAAPI as c_int),
            Backing::Malloc
        );
        assert_eq!(
            Backing::for_device(T::AV_HWDEVICE_TYPE_VULKAN as c_int),
            Backing::Huge
        );
        // A malloc pool lays its buffers out as a huge one does, and
        // hands them back as they come back.
        let nv12 = sys::AVPixelFormat::AV_PIX_FMT_NV12 as c_int;
        let mut pool = DownloadPool::default();
        pool.build(nv12, 250, 66, Backing::Malloc).unwrap();
        assert_eq!(pool.linesizes[..2], [256, 256]);
        assert_eq!(pool.offsets[..2], [0, 256 * 66]);
        // SAFETY: the pool just built; each buffer is unreferenced once.
        unsafe {
            let mut a = sys::av_buffer_pool_get(pool.pool);
            assert!(!a.is_null());
            assert_eq!(
                (*a).size,
                256 * 66 + 256 * 33 + sys::AV_INPUT_BUFFER_PADDING_SIZE as usize
            );
            // av_malloc's alignment, at the least.
            assert_eq!((*a).data.addr() % 16, 0);
            let first = (*a).data;
            sys::av_buffer_unref(&mut a);
            let mut b = sys::av_buffer_pool_get(pool.pool);
            assert_eq!((*b).data, first);
            sys::av_buffer_unref(&mut b);
        }
        assert_eq!(pool.allocated(), 1);
    }

    /// The range and lines of the `/proc/self/smaps` entry for the
    /// mapping that holds `addr`.
    fn smaps_entry(addr: usize) -> Option<((usize, usize), Vec<String>)> {
        let text = std::fs::read_to_string("/proc/self/smaps").ok()?;
        let mut found: Option<((usize, usize), Vec<String>)> = None;
        for line in text.lines() {
            // An entry starts with its range; its other lines with a key.
            let range = line.split_whitespace().next().and_then(|r| {
                let (a, b) = r.split_once('-')?;
                Some((
                    usize::from_str_radix(a, 16).ok()?,
                    usize::from_str_radix(b, 16).ok()?,
                ))
            });
            match (range, &mut found) {
                (Some(_), Some(_)) => break,
                (Some(r), None) if (r.0..r.1).contains(&addr) => found = Some((r, Vec::new())),
                (None, Some((_, lines))) => lines.push(line.to_owned()),
                _ => {}
            }
        }
        found
    }

    #[test]
    fn download_buffers_are_aligned_mappings_on_huge_pages() {
        let size = 5 * 1024 * 1024 + 123;
        let mut buf = huge_alloc(size);
        assert!(!buf.is_null());
        // SAFETY: a buffer huge_alloc made, which this test owns; its
        // `size` bytes are writable.
        let data = unsafe {
            assert_eq!((*buf).size, size);
            std::ptr::write_bytes((*buf).data, 7, size);
            (*buf).data
        };
        let at = data.addr();
        assert_eq!(at % HUGE_PAGE, 0);
        // Its mapping covers whole huge pages (the kernel may have merged
        // it with a neighbour another test made the same way).
        let ((start, end), lines) = smaps_entry(at).expect("the buffer's mapping");
        assert!(
            start <= at && at + 3 * HUGE_PAGE <= end,
            "{start:#x}..{end:#x}"
        );
        let thp = std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled")
            .unwrap_or_default();
        if thp.contains("[madvise]") || thp.contains("[always]") {
            assert!(
                lines
                    .iter()
                    .any(|l| l.split_whitespace().eq(["THPeligible:", "1"])),
                "THP is {thp:?}, but the mapping isn't eligible: {lines:#?}"
            );
        }
        // SAFETY: the one reference to the buffer, dropped here.
        unsafe { sys::av_buffer_unref(&mut buf) };
    }

    #[test]
    fn a_download_crops_the_top_as_software_decoding_does() {
        // FFmpeg crops a device frame on the right and bottom only, and
        // leaves the rest for whoever downloads it.
        let (mut surface, src, _frames) = lavapipe_frame(ff::format::Pixel::NV12, 64, 36);
        // SAFETY: plain fields of a live frame.
        unsafe { (*surface.as_mut_ptr()).crop_top = 4 };
        let fields = frame_fields(&surface);
        let predicted = as_downloaded(&surface, fields).unwrap();
        let nv12 = sys::AVPixelFormat::AV_PIX_FMT_NV12 as c_int;
        assert_eq!(
            (predicted.format, predicted.width, predicted.height),
            (nv12, 64, 32)
        );
        let mut pool = DownloadPool::default();
        let f = download_into_pool(&mut pool, &surface).unwrap();
        assert_eq!((f.width(), f.height()), (64, 32));
        assert_eq!(rows(&f, 0, 64, 32), rows(&src, 0, 64, 36)[4 * 64..]);
        assert_eq!(rows(&f, 1, 64, 16), rows(&src, 1, 64, 18)[2 * 64..]);
        // A frame in system memory has no frames context to download from.
        assert!(as_downloaded(&src, frame_fields(&src)).is_none());
        assert!(download_into_pool(&mut pool, &src).is_err());
    }

    /// The only frame `packets` decode to, decoded in software as
    /// Spherecut decodes.
    fn decode_one(packets: &[ff::Packet]) -> ff::frame::Video {
        let codec = ff::decoder::find_by_name("hevc").unwrap();
        let mut dec = open_decoder(ff::codec::context::Context::new_with_codec(codec), codec)
            .and_then(|d| d.video())
            .unwrap();
        for p in packets {
            dec.send_packet(p).unwrap();
        }
        dec.send_eof().unwrap();
        let mut frame = ff::frame::Video::empty();
        dec.receive_frame(&mut frame).unwrap();
        frame
    }

    /// A 4:2:0 frame's samples, plane by plane (Y, U, V), row by row,
    /// without padding, read from YUV420P, YUV420P10LE, NV12 or P010 (whose
    /// 10 bits are at the top of each u16): the same picture gives the
    /// same samples in any of them.
    fn samples(f: &ff::frame::Video) -> [Vec<u16>; 3] {
        use sys::AVPixelFormat as P;
        let (w, h) = (f.width() as usize, f.height() as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let format = frame_format(f);
        let wide = [P::AV_PIX_FMT_YUV420P10LE, P::AV_PIX_FMT_P010LE]
            .iter()
            .any(|&p| p as c_int == format);
        let at = |i: usize, x: usize, y: usize| -> u16 {
            let (p, stride) = plane_and_stride(f, i).unwrap();
            if wide {
                let o = y * stride + 2 * x;
                u16::from_le_bytes([p[o], p[o + 1]])
            } else {
                u16::from(p[y * stride + x])
            }
        };
        let semi = [P::AV_PIX_FMT_NV12, P::AV_PIX_FMT_P010LE]
            .iter()
            .any(|&p| p as c_int == format);
        let shift = if format == P::AV_PIX_FMT_P010LE as c_int {
            6
        } else {
            0
        };
        let luma = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| at(0, x, y) >> shift)
            .collect();
        let chroma = |c: usize| {
            (0..ch)
                .flat_map(|y| (0..cw).map(move |x| (x, y)))
                .map(|(x, y)| match semi {
                    true => at(1, 2 * x + c, y) >> shift,
                    false => at(1 + c, x, y),
                })
                .collect()
        };
        [luma, chroma(0), chroma(1)]
    }

    /// `f`, a YUV420P or YUV420P10LE frame, as a device would hold it:
    /// NV12 or P010.
    fn semi_planar(f: &ff::frame::Video) -> ff::frame::Video {
        use ff::format::Pixel;
        let wide = f.format() == Pixel::YUV420P10LE;
        let format = if wide { Pixel::P010LE } else { Pixel::NV12 };
        let (w, h) = (f.width() as usize, f.height() as usize);
        let mut out = ff::frame::Video::new(format, f.width(), f.height());
        let [y, u, v] = samples(f);
        let put = |row: &mut [u8], i: usize, s: u16| {
            if wide {
                row[2 * i..2 * i + 2].copy_from_slice(&(s << 6).to_le_bytes());
            } else {
                row[i] = s as u8;
            }
        };
        let stride = out.stride(0);
        for (r, row) in out.data_mut(0).chunks_mut(stride).take(h).enumerate() {
            for x in 0..w {
                put(row, x, y[r * w + x]);
            }
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let stride = out.stride(1);
        for (r, row) in out.data_mut(1).chunks_mut(stride).take(ch).enumerate() {
            for x in 0..cw {
                put(row, 2 * x, u[r * cw + x]);
                put(row, 2 * x + 1, v[r * cw + x]);
            }
        }
        out
    }

    /// The samples of `full`, a `w`-wide picture's, from column `left` and
    /// row `top` on.
    fn window(full: &[Vec<u16>; 3], w: usize, left: usize, top: usize) -> [Vec<u16>; 3] {
        let cut = |p: &[u16], w: usize, left: usize, top: usize| {
            p.chunks(w)
                .skip(top)
                .flat_map(|row| row[left..].to_vec())
                .collect::<Vec<u16>>()
        };
        [
            cut(&full[0], w, left, top),
            cut(&full[1], w / 2, left / 2, top / 2),
            cut(&full[2], w / 2, left / 2, top / 2),
        ]
    }

    #[test]
    fn left_and_top_crops_come_out_exactly_in_software_and_downloads() {
        use ff::format::Pixel;
        // FFmpeg's aligned cropping would keep up to 63 columns of what
        // the stream crops on the left, by rules that differ between
        // YUV420P and NV12, and fail outright on P010; and it leaves device
        // frames cropped on the right and bottom only.
        let value = |i: usize, x: usize, y: usize| match i {
            0 => (x * 7 + y * 3) % 200 + 20,
            1 => (x * 5 + y) % 160 + 40,
            _ => (x + y * 9) % 120 + 60,
        } as u16;
        for (format, scale) in [(Pixel::YUV420P, 1), (Pixel::YUV420P10LE, 4)] {
            let packets = hevc_picture(format, |i, x, y| value(i, x, y) * scale);
            let full = decode_one(&packets);
            assert_eq!((full.width(), full.height()), (512, 128));
            let all = samples(&full);
            let semi = semi_planar(&full);
            let (mut surface, _frames) = lavapipe_upload(&semi);
            let mut pool = DownloadPool::default();
            for (left, top) in [
                (0u32, 0u32),
                (8, 0),
                (16, 0),
                (32, 0),
                (48, 0),
                (64, 0),
                (48, 2),
                (0, 6),
            ] {
                let what = format!("{format:?} cropped {left} on the left and {top} at the top");
                let want = (512 - left, 128 - top);
                let soft = decode_one(&crop(packets.clone(), left.into(), top.into()));
                assert_eq!((soft.width(), soft.height()), want, "{what}, in software");
                let window = window(&all, 512, left as usize, top as usize);
                assert!(samples(&soft) == window, "{what}, in software");
                // As the decoder hands over a device frame: cropped on
                // the right and bottom, the rest left for the download.
                // SAFETY: plain fields of a live frame.
                unsafe {
                    (*surface.as_mut_ptr()).crop_left = left as usize;
                    (*surface.as_mut_ptr()).crop_top = top as usize;
                }
                let predicted = as_downloaded(&surface, frame_fields(&surface)).unwrap();
                assert_eq!((predicted.width, predicted.height), want, "{what}");
                let down = download_into_pool(&mut pool, &surface)
                    .unwrap_or_else(|e| panic!("{what}, downloading: {e}"));
                assert_eq!((down.width(), down.height()), want, "{what}, downloaded");
                assert!(samples(&down) == window, "{what}, downloaded");
            }
        }
    }
}
