//! The FFmpeg and Vulkan calls behind `spherecut doctor`'s capability
//! checks.
//!
//! A device that opens can still be useless: Fedora's Mesa VA-API driver
//! opens with H.264 and HEVC turned off, and FFmpeg's Vulkan device opens
//! on llvmpipe, which renders on the CPU. So doctor decodes and encodes one
//! small frame on each device, never falling back to software, and lists
//! the Vulkan physical devices with their type and driver. This module
//! makes those calls; `doctor` decides what the results mean.
//!
//! What FFmpeg logs while a check runs is often the only real explanation
//! ("No support for codec hevc profile 1."), so [`capture_ffmpeg_log`]
//! keeps it.
//!
//! As in `ffi`, C enums FFmpeg hands back are read as `int`s, never as
//! bindgen's Rust enums. (ffmpeg-next's encoder and frame setters still
//! build enums from values we chose; DECISIONS.md D11.) The few Vulkan
//! structs needed are declared here with only the fields that are read
//! spelled out.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Arc, Mutex, PoisonError};

use ffmpeg_sys_next as sys;
use thiserror::Error;
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Dispatch, Event, Level, Metadata, Subscriber, field, span};

use crate::decode::DecodedFrame;
use crate::ff;
use crate::ffi::{self, HwDevice, HwError, HwRefusal};

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
    #[error("{what}: {error}")]
    Ffmpeg {
        what: &'static str,
        error: ff::Error,
    },
    #[error(transparent)]
    Hw(#[from] HwError),
    #[error("{0}")]
    Vulkan(String),
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
    move |error| ProbeError::Ffmpeg { what, error }
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
    let mut ctx = ff::codec::context::Context::new_with_codec(codec);
    // One thread: FFmpeg then sets up the hardware decoder, and logs why
    // it can't, on this thread, where capture_ffmpeg_log listens.
    ctx.set_threading(ff::codec::threading::Config {
        kind: ff::codec::threading::Type::Frame,
        count: 1,
    });
    if let Some(dev) = dev {
        ffi::attach_hw_device(&mut ctx, dev)?;
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
        return Err(match (dev, ffi::hw_refusal(&dec)) {
            (Some(d), Some(HwRefusal::Withdrawn)) => ProbeError::DriverRefused {
                device: device_label(d.kind()).into(),
            },
            (Some(d), Some(HwRefusal::NotOffered)) => ProbeError::NoHwFormat {
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

/// `VkPhysicalDeviceType` values; 0 is "other".
pub(crate) mod vk_device_type {
    pub const INTEGRATED_GPU: i32 = 1;
    pub const DISCRETE_GPU: i32 = 2;
    pub const VIRTUAL_GPU: i32 = 3;
    pub const CPU: i32 = 4;
}

/// A Vulkan physical device as `vkGetPhysicalDeviceProperties` (and, from
/// Vulkan 1.2, `VkPhysicalDeviceDriverProperties`) describe it.
#[derive(Debug, Clone)]
pub(crate) struct VkDevice {
    pub name: String,
    /// `VkPhysicalDeviceType`, see [`vk_device_type`].
    pub device_type: i32,
    pub vendor_id: u32,
    pub device_id: u32,
    /// Packed as `VK_MAKE_API_VERSION`; see [`vk_version`].
    pub api_version: u32,
    pub driver_name: Option<String>,
    pub driver_info: Option<String>,
    /// FFmpeg's Vulkan device context runs on this one.
    pub ffmpeg_uses: bool,
}

/// `(major, minor, patch)` of a version packed as `VK_MAKE_API_VERSION`.
pub(crate) fn vk_version(v: u32) -> (u32, u32, u32) {
    ((v >> 22) & 0x7f, (v >> 12) & 0x3ff, v & 0xfff)
}

type VkHandle = *mut c_void;
type VoidFn = unsafe extern "C" fn();
type GetInstanceProcAddr = unsafe extern "C" fn(VkHandle, *const c_char) -> Option<VoidFn>;
type EnumeratePhysicalDevices = unsafe extern "C" fn(VkHandle, *mut u32, *mut VkHandle) -> i32;
type GetPhysicalDeviceProperties = unsafe extern "C" fn(VkHandle, *mut VkPhysicalDeviceProperties);
type GetPhysicalDeviceProperties2 =
    unsafe extern "C" fn(VkHandle, *mut VkPhysicalDeviceProperties2);

/// The start of FFmpeg's `AVVulkanDeviceContext`. These fields have had
/// these places since the struct was added; later ones changed between
/// FFmpeg versions, so they are left out.
#[repr(C)]
struct VulkanDeviceHead {
    alloc: *const c_void,
    get_proc_addr: Option<GetInstanceProcAddr>,
    inst: VkHandle,
    phys_dev: VkHandle,
}

/// `VkPhysicalDeviceProperties`, with the limits and sparse properties
/// (not read here) as opaque bytes of their C size.
#[repr(C)]
struct VkPhysicalDeviceProperties {
    api_version: u32,
    driver_version: u32,
    vendor_id: u32,
    device_id: u32,
    /// `VkPhysicalDeviceType`, as the int it is.
    device_type: i32,
    device_name: [c_char; 256],
    pipeline_cache_uuid: [u8; 16],
    /// `VkPhysicalDeviceLimits`: 504 bytes, 8-byte aligned.
    limits: [u64; 63],
    /// `VkPhysicalDeviceSparseProperties`: five `VkBool32`s.
    sparse: [u32; 5],
}

// The C sizes on 64-bit Linux, where the limits hold 64-bit `size_t`s.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<VkPhysicalDeviceProperties>() == 824);
    assert!(size_of::<VkPhysicalDeviceProperties2>() == 840);
    assert!(size_of::<VkPhysicalDeviceDriverProperties>() == 536);
};

impl VkPhysicalDeviceProperties {
    fn zeroed() -> Self {
        VkPhysicalDeviceProperties {
            api_version: 0,
            driver_version: 0,
            vendor_id: 0,
            device_id: 0,
            device_type: 0,
            device_name: [0; 256],
            pipeline_cache_uuid: [0; 16],
            limits: [0; 63],
            sparse: [0; 5],
        }
    }
}

const STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2: i32 = 1_000_059_001;
const STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES: i32 = 1_000_196_000;

#[repr(C)]
struct VkPhysicalDeviceProperties2 {
    s_type: i32,
    p_next: *mut c_void,
    properties: VkPhysicalDeviceProperties,
}

#[repr(C)]
struct VkPhysicalDeviceDriverProperties {
    s_type: i32,
    p_next: *mut c_void,
    /// `VkDriverId`, as the int it is.
    driver_id: i32,
    driver_name: [c_char; 256],
    driver_info: [c_char; 256],
    conformance_version: [u8; 4],
}

fn vulkan_err(msg: impl Into<String>) -> ProbeError {
    ProbeError::Vulkan(msg.into())
}

/// Look up the instance-level Vulkan command `name`.
///
/// # Safety
///
/// `F` must be the function pointer type of the command `name`, and `inst`
/// a live instance that `gipa` belongs to.
unsafe fn vk_command<F>(
    gipa: GetInstanceProcAddr,
    inst: VkHandle,
    name: &CStr,
) -> Result<F, ProbeError> {
    const { assert!(size_of::<F>() == size_of::<VoidFn>()) };
    // SAFETY: the caller's guarantee; `name` is NUL-terminated.
    let f = unsafe { gipa(inst, name.as_ptr()) }
        .ok_or_else(|| vulkan_err(format!("Vulkan has no {}", name.to_string_lossy())))?;
    // SAFETY: Vulkan hands out commands as PFN_vkVoidFunction, to be cast
    // to their real type, which the caller guarantees `F` is.
    Ok(unsafe { std::mem::transmute_copy::<VoidFn, F>(&f) })
}

/// A NUL-terminated string in a fixed-size Vulkan array; `None` if empty.
fn fixed_str(chars: &[c_char]) -> Option<String> {
    let bytes: Vec<u8> = chars
        .iter()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect();
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// The physical devices of the Vulkan instance behind FFmpeg's Vulkan
/// device `dev`, marking the one FFmpeg uses.
pub(crate) fn vulkan_devices(dev: &HwDevice) -> Result<Vec<VkDevice>, ProbeError> {
    if dev.kind() != "vulkan" {
        return Err(vulkan_err(format!("{} is not a Vulkan device", dev.kind())));
    }
    // SAFETY: a device reference's data is its AVHWDeviceContext, and a
    // Vulkan device's hwctx is an AVVulkanDeviceContext, which starts with
    // the fields of VulkanDeviceHead. Only pointer fields are read, and
    // the instance lives as long as `dev`.
    let head = unsafe {
        let hwdev = (*dev.as_ptr()).data.cast::<sys::AVHWDeviceContext>();
        (*hwdev).hwctx.cast::<VulkanDeviceHead>().read()
    };
    let gipa = head
        .get_proc_addr
        .ok_or_else(|| vulkan_err("FFmpeg's Vulkan device has no vkGetInstanceProcAddr"))?;
    // SAFETY: each type below matches the C prototype of the command it
    // is looked up as, and the instance is FFmpeg's, made through `gipa`.
    let (enumerate, props, props2) = unsafe {
        (
            vk_command::<EnumeratePhysicalDevices>(gipa, head.inst, c"vkEnumeratePhysicalDevices")?,
            vk_command::<GetPhysicalDeviceProperties>(
                gipa,
                head.inst,
                c"vkGetPhysicalDeviceProperties",
            )?,
            // Core in Vulkan 1.1; FFmpeg asks for at least that.
            vk_command::<GetPhysicalDeviceProperties2>(
                gipa,
                head.inst,
                c"vkGetPhysicalDeviceProperties2",
            )
            .ok(),
        )
    };
    let mut n = 0u32;
    // SAFETY: the counting form of the call, with no output array.
    let r = unsafe { enumerate(head.inst, &mut n, std::ptr::null_mut()) };
    if r < 0 {
        return Err(vulkan_err(format!(
            "vkEnumeratePhysicalDevices failed (VkResult {r})"
        )));
    }
    let mut handles: Vec<VkHandle> = vec![std::ptr::null_mut(); n as usize];
    // SAFETY: `handles` has room for the `n` handles asked for. A device
    // added since the first call makes this VK_INCOMPLETE (positive).
    let r = unsafe { enumerate(head.inst, &mut n, handles.as_mut_ptr()) };
    if r < 0 {
        return Err(vulkan_err(format!(
            "vkEnumeratePhysicalDevices failed (VkResult {r})"
        )));
    }
    handles.truncate(n as usize);
    Ok(handles
        .into_iter()
        .map(|pd| {
            let mut p = VkPhysicalDeviceProperties::zeroed();
            // SAFETY: `pd` is a physical device of this instance, and `p`
            // has the C layout and size Vulkan writes.
            unsafe { props(pd, &mut p) };
            let (mut driver_name, mut driver_info) = (None, None);
            // VkPhysicalDeviceDriverProperties is core from Vulkan 1.2.
            if let Some(props2) = props2
                && vk_version(p.api_version) >= (1, 2, 0)
            {
                let mut drv = VkPhysicalDeviceDriverProperties {
                    s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES,
                    p_next: std::ptr::null_mut(),
                    driver_id: 0,
                    driver_name: [0; 256],
                    driver_info: [0; 256],
                    conformance_version: [0; 4],
                };
                let mut p2 = VkPhysicalDeviceProperties2 {
                    s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2,
                    p_next: (&raw mut drv).cast(),
                    properties: VkPhysicalDeviceProperties::zeroed(),
                };
                // SAFETY: as above, with a pNext chain of one struct that
                // this device's Vulkan version has.
                unsafe { props2(pd, &mut p2) };
                driver_name = fixed_str(&drv.driver_name);
                driver_info = fixed_str(&drv.driver_info);
            }
            VkDevice {
                name: fixed_str(&p.device_name).unwrap_or_else(|| "(unnamed)".into()),
                device_type: p.device_type,
                vendor_id: p.vendor_id,
                device_id: p.device_id,
                api_version: p.api_version,
                driver_name,
                driver_info,
                ffmpeg_uses: pd == head.phys_dev,
            }
        })
        .collect())
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
    // A line FFmpeg left unfinished on this thread is not `f`'s, and one
    // `f` leaves unfinished is.
    ffi::end_log_line();
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
        let r = f();
        ffi::end_log_line();
        r
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

/// FFmpeg's "[hevc @ 0x55d0c0ffee00] msg" as "[hevc] msg", and the same
/// for a parent context's tag before it: the address means nothing in a
/// report.
fn without_address(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(tag) = rest.strip_prefix('[')
        && let Some((name, msg)) = tag.split_once("] ")
        && let Some((name, addr)) = name.split_once(" @ 0x")
        && addr.bytes().all(|b| b.is_ascii_hexdigit())
    {
        out += &format!("[{name}] ");
        rest = msg;
    }
    out + rest
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
    fn vulkan_versions_unpack() {
        // VK_MAKE_API_VERSION(0, 1, 4, 318), as llvmpipe reports it.
        assert_eq!(vk_version((1 << 22) | (4 << 12) | 318), (1, 4, 318));
        assert!(vk_version((1 << 22) | (2 << 12)) >= (1, 2, 0));
        assert!(vk_version((1 << 22) | (1 << 12) | 999) < (1, 2, 0));
    }

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
        // A parent context's tag comes first.
        assert_eq!(
            without_address("[mp4 @ 0xa1] [hevc @ 0xb2] [x @ 0xzz] msg"),
            "[mp4] [hevc] [x @ 0xzz] msg"
        );
        for plain in ["no prefix", "[x] no address", "[x @ 0xzz] not hex"] {
            assert_eq!(without_address(plain), plain);
        }
    }

    #[test]
    fn fixed_strings_stop_at_nul() {
        let mut a = [0 as c_char; 8];
        for (d, s) in a.iter_mut().zip(b"radv\0xy") {
            *d = *s as c_char;
        }
        assert_eq!(fixed_str(&a).as_deref(), Some("radv"));
        assert_eq!(fixed_str(&[0; 4]), None);
        // No NUL at all: the whole array.
        assert_eq!(fixed_str(&[b'a' as c_char; 3]).as_deref(), Some("aaa"));
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
