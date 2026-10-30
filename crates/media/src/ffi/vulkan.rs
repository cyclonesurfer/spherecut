//! FFmpeg's Vulkan hwcontext on a device Spherecut made (slice 5b's path
//! B, `docs/design/m1-5b.md`): the device context made from the
//! renderer's raw handles, and a decoded frame left on it as a
//! [`VkPicture`].
//!
//! ffmpeg-sys-next binds `AVVulkanDeviceContext`, `AVVulkanFramesContext`
//! and `AVVkFrame` from the headers it builds against, so their layouts
//! follow the FFmpeg version. Which fields there are changes: FFmpeg 6.1
//! takes the queues in fixed fields, 7.1 adds `qf[]` (`cfg(ffmpeg_7_1)`),
//! and 9.0 drops the fixed fields and makes `AVVkFrame.access`
//! `VkAccessFlagBits2` (`cfg(ffmpeg_9_0)`). The Vulkan types are the
//! plain integers of ffmpeg-sys-next's stand-in `vulkan.h`, never Rust
//! enums.

use std::any::Any;
use std::ffi::{CString, c_char, c_int};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use ffmpeg_next as ff;
use ffmpeg_sys_next as sys;
use spherecut_core::gate::SubmitGate;
use spherecut_core::video::{PixelLayout, VkDeviceHandles, VkImageState, VkPicture, VkPlane};

use super::{HwDevice, HwError, c, device_type};

const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2: c_int = 1_000_059_000;
const VK_QUEUE_COMPUTE_BIT: u32 = 0x2;
const VK_QUEUE_TRANSFER_BIT: u32 = 0x4;
#[cfg(not(ffmpeg_7_1))]
const VK_QUEUE_VIDEO_DECODE_BIT_KHR: u32 = 0x20;
const VK_IMAGE_ASPECT_COLOR_BIT: u32 = 0x1;
/// `VK_IMAGE_ASPECT_PLANE_0_BIT`; planes 1 and 2 are the next two bits.
const VK_IMAGE_ASPECT_PLANE_0_BIT: u32 = 0x10;
/// What an access FFmpeg's fixed-size field can't hold is recorded as:
/// `VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT`, which covers
/// every access.
#[cfg(not(ffmpeg_9_0))]
const VK_ACCESS_MEMORY_READ_WRITE: u32 = 0x8000 | 0x1_0000;

/// What a shared device context holds for as long as it lives: the
/// extension names its `AVVulkanDeviceContext` points at, the device
/// itself, which outlives FFmpeg's context and so every frame of it, and
/// what its queue locks need.
struct Kept {
    _names: Vec<CString>,
    instance_extensions: Vec<*const c_char>,
    device_extensions: Vec<*const c_char>,
    _keepalive: Arc<dyn Any + Send + Sync>,
    /// The device's submit gate, which every queue lock waits at (D41).
    gate: Arc<SubmitGate>,
    /// The queues FFmpeg holds, as (family, index), and a wake for a
    /// lock waiting for one to be let go.
    held: Mutex<Vec<(u32, u32)>>,
    freed: Condvar,
}

/// The device context's `lock_queue`, which FFmpeg calls around each use
/// of a queue (a submission, a wait for idle): the queue, held by one
/// thread at a time as FFmpeg's own locks would, and then the submit
/// gate, so that FFmpeg's decodes stay out of the UI's present (D41).
/// FFmpeg 8.1 and 9.0 mark the field deprecated and still call it; a
/// later FFmpeg that stops calling it loses the gate, not the locking,
/// which it does itself then.
unsafe extern "C" fn lock_queue(ctx: *mut sys::AVHWDeviceContext, family: u32, index: u32) {
    // SAFETY: `user_opaque` is the context's `Kept` from before init until
    // `free_kept`, which FFmpeg calls after its last queue use.
    let Some(kept) = (unsafe { (*ctx).user_opaque.cast::<Kept>().as_ref() }) else {
        return;
    };
    // Nothing here panics but a poisoned lock, which is taken over; a
    // panic must not unwind into C.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut held = kept.held.lock().unwrap_or_else(PoisonError::into_inner);
        while held.contains(&(family, index)) {
            held = kept
                .freed
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
        held.push((family, index));
        drop(held);
        kept.gate.wait();
    }));
}

/// The device context's `unlock_queue`.
unsafe extern "C" fn unlock_queue(ctx: *mut sys::AVHWDeviceContext, family: u32, index: u32) {
    // SAFETY: as `lock_queue`'s.
    let Some(kept) = (unsafe { (*ctx).user_opaque.cast::<Kept>().as_ref() }) else {
        return;
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut held = kept.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.retain(|&q| q != (family, index));
        drop(held);
        kept.freed.notify_all();
    }));
}

/// The device context's `free`: called once, after FFmpeg's own uninit,
/// as the context goes.
unsafe extern "C" fn free_kept(ctx: *mut sys::AVHWDeviceContext) {
    // SAFETY: `user_opaque` is the `Kept` vulkan_shared boxed for this
    // context, and FFmpeg calls `free` once.
    unsafe {
        let kept = std::mem::replace(&mut (*ctx).user_opaque, std::ptr::null_mut());
        if !kept.is_null() {
            drop(Box::from_raw(kept.cast::<Kept>()));
        }
    }
}

/// `names` as C strings, kept in `keep`, and pointers to them.
fn c_names(
    names: &[String],
    what: &'static str,
    keep: &mut Vec<CString>,
) -> Result<Vec<*const c_char>, HwError> {
    names
        .iter()
        .map(|n| {
            let c = CString::new(n.as_str()).map_err(|_| HwError::Nul {
                what,
                name: n.clone(),
            })?;
            let p = c.as_ptr();
            keep.push(c);
            Ok(p)
        })
        .collect()
}

impl HwDevice {
    /// FFmpeg's Vulkan device context on a device Spherecut made, from its
    /// raw `handles`: FFmpeg decodes on the queues `handles` grants it,
    /// and its frames are images on that device. The context keeps
    /// `handles.keepalive`, and so the device, until the last frame of it
    /// is gone; FFmpeg never destroys a device it didn't make.
    pub fn vulkan_shared(handles: &VkDeviceHandles) -> Result<Self, HwError> {
        let r = Self::vulkan_shared_inner(handles);
        match &r {
            Ok(_) => {
                tracing::debug!(device = ?handles, "FFmpeg's Vulkan context on a shared device")
            }
            Err(e) => tracing::debug!("FFmpeg refused the shared Vulkan device: {e}"),
        }
        r
    }

    fn vulkan_shared_inner(handles: &VkDeviceHandles) -> Result<Self, HwError> {
        if !handles
            .queues
            .iter()
            .any(|g| g.flags & VK_QUEUE_COMPUTE_BIT != 0)
            || !handles
                .queues
                .iter()
                .any(|g| g.flags & VK_QUEUE_TRANSFER_BIT != 0)
        {
            return Err(HwError::Vulkan(
                "FFmpeg needs a compute and a transfer queue, and none was granted".into(),
            ));
        }
        let t = device_type("vulkan")?;
        let mut names = Vec::new();
        let instance_extensions = c_names(
            &handles.instance_extensions,
            "instance extension",
            &mut names,
        )?;
        let device_extensions =
            c_names(&handles.device_extensions, "device extension", &mut names)?;
        let kept = Box::new(Kept {
            _names: names,
            instance_extensions,
            device_extensions,
            _keepalive: handles.keepalive.clone(),
            gate: handles.gate.clone(),
            held: Mutex::new(Vec::new()),
            freed: Condvar::new(),
        });
        let count = |n: usize| {
            c_int::try_from(n).map_err(|_| HwError::Vulkan(format!("{n} names are too many")))
        };
        let nb_instance = count(kept.instance_extensions.len())?;
        let nb_device = count(kept.device_extensions.len())?;
        // SAFETY: accepts any type value; a new context we own, or NULL.
        let mut r = unsafe { c::av_hwdevice_ctx_alloc(t) };
        if r.is_null() {
            return Err(ff::Error::Other {
                errno: ff::util::error::ENOMEM,
            }
            .into());
        }
        // SAFETY: `r` is a fresh Vulkan device context, not yet
        // initialised, whose `hwctx` is its AVVulkanDeviceContext,
        // zeroed. Its fields are set before init, as the header allows.
        // The extension pointers point into `kept`, which the context owns
        // from here (`free_kept`), and the handles are valid while
        // `keepalive`, which `kept` holds, lives. On failure the one
        // reference is dropped, which frees `kept`.
        unsafe {
            let dev = (*r).data.cast::<sys::AVHWDeviceContext>();
            let hw = (*dev).hwctx.cast::<sys::AVVulkanDeviceContext>();
            (*hw).alloc = std::ptr::null();
            (*hw).get_proc_addr = std::mem::transmute::<usize, sys::PFN_vkGetInstanceProcAddr>(
                handles.get_instance_proc_addr,
            );
            (*hw).inst = std::ptr::with_exposed_provenance_mut(handles.instance);
            (*hw).phys_dev = std::ptr::with_exposed_provenance_mut(handles.physical_device);
            (*hw).act_dev = std::ptr::with_exposed_provenance_mut(handles.device);
            // FFmpeg reads the features it uses from the physical device
            // (`ff_vk_load_props`); this chain is left empty.
            (*hw).device_features.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2;
            (*hw).device_features.pNext = std::ptr::null_mut();
            (*hw).enabled_inst_extensions = kept.instance_extensions.as_ptr();
            (*hw).nb_enabled_inst_extensions = nb_instance;
            (*hw).enabled_dev_extensions = kept.device_extensions.as_ptr();
            (*hw).nb_enabled_dev_extensions = nb_device;
            (*dev).user_opaque = Box::into_raw(kept).cast();
            (*dev).free = Some(free_kept);
            // Ours instead of FFmpeg's own, which it sets only if these
            // are unset.
            (*hw).lock_queue = Some(lock_queue);
            (*hw).unlock_queue = Some(unlock_queue);
            if let Err(e) = set_queues(hw, handles) {
                sys::av_buffer_unref(&mut r);
                return Err(e);
            }
            let ret = sys::av_hwdevice_ctx_init(r);
            if ret < 0 {
                sys::av_buffer_unref(&mut r);
                return Err(HwError::Vulkan(format!(
                    "FFmpeg couldn't take the shared device: {}",
                    ff::Error::from(ret)
                )));
            }
        }
        Ok(HwDevice {
            ptr: r,
            device_type: t,
            pix_fmt: sys::AVPixelFormat::AV_PIX_FMT_VULKAN,
            kind: "vulkan".into(),
        })
    }
}

/// The granted queues, as `qf[]`.
///
/// # Safety
/// `hw` is a live, uninitialised device context's.
#[cfg(ffmpeg_7_1)]
unsafe fn set_queues(
    hw: *mut sys::AVVulkanDeviceContext,
    handles: &VkDeviceHandles,
) -> Result<(), HwError> {
    // SAFETY: as the caller promises; `qf` is a fixed array in it.
    unsafe {
        let qf = &mut (*hw).qf;
        if handles.queues.len() > qf.len() {
            return Err(HwError::Vulkan(format!(
                "{} queue families are more than FFmpeg takes",
                handles.queues.len()
            )));
        }
        for (slot, g) in qf.iter_mut().zip(&handles.queues) {
            slot.idx = c_int::try_from(g.family).unwrap_or(c_int::MAX);
            slot.num = c_int::try_from(g.count).unwrap_or(c_int::MAX);
            slot.flags = g.flags as _;
            slot.video_caps = g.video_ops as _;
        }
        (*hw).nb_qf = handles.queues.len() as c_int;
    }
    Ok(())
}

/// The granted queues, in FFmpeg 6.1's fixed fields: compute and
/// transfers from the grants that say so, decode from the one with video
/// decode. No graphics or encode queue is given.
///
/// # Safety
/// `hw` is a live, uninitialised device context's.
#[cfg(not(ffmpeg_7_1))]
unsafe fn set_queues(
    hw: *mut sys::AVVulkanDeviceContext,
    handles: &VkDeviceHandles,
) -> Result<(), HwError> {
    let pick = |bit: u32| {
        handles
            .queues
            .iter()
            .find(|g| g.flags & bit != 0)
            .map_or((0, 0), |g| {
                (
                    c_int::try_from(g.family).unwrap_or(c_int::MAX),
                    c_int::try_from(g.count).unwrap_or(c_int::MAX),
                )
            })
    };
    // SAFETY: as the caller promises; plain int fields.
    unsafe {
        (*hw).queue_family_index = 0;
        (*hw).nb_graphics_queues = 0;
        ((*hw).queue_family_comp_index, (*hw).nb_comp_queues) = pick(VK_QUEUE_COMPUTE_BIT);
        ((*hw).queue_family_tx_index, (*hw).nb_tx_queues) = pick(VK_QUEUE_TRANSFER_BIT);
        ((*hw).queue_family_decode_index, (*hw).nb_decode_queues) =
            pick(VK_QUEUE_VIDEO_DECODE_BIT_KHR);
        (*hw).queue_family_encode_index = 0;
        (*hw).nb_encode_queues = 0;
    }
    Ok(())
}

#[cfg(ffmpeg_9_0)]
fn access_in(a: sys::VkAccessFlagBits2) -> u64 {
    a
}

#[cfg(ffmpeg_9_0)]
fn access_out(a: u64) -> sys::VkAccessFlagBits2 {
    a
}

#[cfg(not(ffmpeg_9_0))]
fn access_in(a: sys::VkAccessFlagBits) -> u64 {
    // The bits of a 32-bit mask, which VkAccessFlags2 shares.
    u64::from(a as u32)
}

#[cfg(not(ffmpeg_9_0))]
fn access_out(a: u64) -> sys::VkAccessFlagBits {
    u32::try_from(a).unwrap_or(VK_ACCESS_MEMORY_READ_WRITE) as sys::VkAccessFlagBits
}

/// A decoded frame left on its Vulkan device (`HwOutput::Vulkan`): the
/// `AVFrame`, whose `data[0]` is its `AVVkFrame`, and where each plane
/// is. It holds the frame's image from FFmpeg's pool until it drops.
pub(crate) struct VkFrame {
    frame: ff::frame::Video,
    planes: Vec<VkPlane>,
    images: usize,
}

impl std::fmt::Debug for VkFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VkFrame")
            .field("planes", &self.planes)
            .field("images", &self.images)
            .finish_non_exhaustive()
    }
}

/// The frame's `AVVkFrame` and frames context, when it is a Vulkan frame
/// with both and its frames context can lock it.
///
/// # Safety
/// `f` is a live `AVFrame`.
unsafe fn vk_parts(
    f: *const sys::AVFrame,
) -> Option<(*mut sys::AVVkFrame, *mut sys::AVHWFramesContext)> {
    // SAFETY: as the caller promises; a Vulkan frame's data[0] is its
    // AVVkFrame and its hw_frames_ctx a Vulkan frames context, whose
    // hwctx is its AVVulkanFramesContext, all alive as long as it is.
    unsafe {
        if (*f).format != sys::AVPixelFormat::AV_PIX_FMT_VULKAN as c_int
            || (*f).data[0].is_null()
            || (*f).hw_frames_ctx.is_null()
            || (*(*f).hw_frames_ctx).data.is_null()
        {
            return None;
        }
        let fc = (*(*f).hw_frames_ctx).data.cast::<sys::AVHWFramesContext>();
        let vfc = (*fc).hwctx.cast::<sys::AVVulkanFramesContext>();
        if vfc.is_null() || (*vfc).lock_frame.is_none() || (*vfc).unlock_frame.is_none() {
            return None;
        }
        Some(((*f).data[0].cast::<sys::AVVkFrame>(), fc))
    }
}

impl VkFrame {
    /// `frame`, a frame on a Vulkan device, as `layout`'s planes; why
    /// not, when it isn't one or its images don't hold them as one image
    /// a plane or one multi-planar image. Each plane's size is the
    /// image's, which can be larger than the picture.
    pub(crate) fn new(
        frame: ff::frame::Video,
        layout: &PixelLayout,
    ) -> Result<VkFrame, (ff::frame::Video, String)> {
        match Self::planes_of(&frame, layout) {
            Ok((planes, images)) => Ok(VkFrame {
                frame,
                planes,
                images,
            }),
            Err(why) => Err((frame, why)),
        }
    }

    /// Where `frame`'s planes are, and how many images it has.
    fn planes_of(
        frame: &ff::frame::Video,
        layout: &PixelLayout,
    ) -> Result<(Vec<VkPlane>, usize), String> {
        // SAFETY: `frame` wraps a live AVFrame; vk_parts checks it is a
        // Vulkan frame before its AVVkFrame and frames context are read.
        let (images, size) = unsafe {
            let (vkf, fc) = vk_parts(frame.as_ptr())
                .ok_or_else(|| "it isn't a Vulkan frame FFmpeg can lock".to_owned())?;
            let images = (*vkf).img.iter().take_while(|&&i| i != 0).count();
            let size = (
                u32::try_from((*fc).width).unwrap_or(0),
                u32::try_from((*fc).height).unwrap_or(0),
            );
            (images, size)
        };
        let n = layout.planes.len();
        let aspect = |i: usize| -> Result<(usize, u32), String> {
            match images {
                1 if n > 1 && n <= 3 => Ok((0, VK_IMAGE_ASPECT_PLANE_0_BIT << i)),
                m if m == n => Ok((i, VK_IMAGE_ASPECT_COLOR_BIT)),
                m => Err(format!(
                    "its {m} images don't hold {}'s {n} planes",
                    layout.name
                )),
            }
        };
        let planes = layout
            .planes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (image, aspect) = aspect(i)?;
                Ok(VkPlane {
                    image,
                    aspect,
                    size: p.size(size.0, size.1),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((planes, images))
    }

    /// The device frame, for a download (`VideoFrame::to_system`).
    pub(crate) fn frame(&self) -> &ff::frame::Video {
        &self.frame
    }
}

/// Unlocks a frame [`VkFrame::with_locked`] locked, however it leaves.
struct Unlock {
    unlock: unsafe extern "C" fn(*mut sys::AVHWFramesContext, *mut sys::AVVkFrame),
    fc: *mut sys::AVHWFramesContext,
    vkf: *mut sys::AVVkFrame,
}

impl Drop for Unlock {
    fn drop(&mut self) {
        // SAFETY: the frame and context `with_locked` locked, still alive.
        unsafe { (self.unlock)(self.fc, self.vkf) };
    }
}

impl VkPicture for VkFrame {
    fn planes(&self) -> &[VkPlane] {
        &self.planes
    }

    fn with_locked(&self, f: &mut dyn FnMut(&mut [VkImageState])) {
        // SAFETY: `self.frame` is a Vulkan frame FFmpeg can lock
        // (checked in `new`), alive while `self` is. Its AVVkFrame's
        // fields are read and written only under its frames context's
        // lock_frame, as FFmpeg's header asks; the lock is released by
        // the guard even if `f` panics, and then nothing is written.
        unsafe {
            let Some((vkf, fc)) = vk_parts(self.frame.as_ptr()) else {
                return;
            };
            let vfc = (*fc).hwctx.cast::<sys::AVVulkanFramesContext>();
            let (Some(lock), Some(unlock)) = ((*vfc).lock_frame, (*vfc).unlock_frame) else {
                return;
            };
            lock(fc, vkf);
            let _guard = Unlock { unlock, fc, vkf };
            let mut states: Vec<VkImageState> = (0..self.images)
                .map(|i| VkImageState {
                    image: (*vkf).img[i],
                    layout: (*vkf).layout[i],
                    access: access_in((*vkf).access[i]),
                    queue_family: (*vkf).queue_family[i],
                    semaphore: (*vkf).sem[i],
                    value: (*vkf).sem_value[i],
                })
                .collect();
            f(&mut states);
            for (i, s) in states.iter().enumerate() {
                (*vkf).layout[i] = s.layout;
                (*vkf).access[i] = access_out(s.access);
                (*vkf).queue_family[i] = s.queue_family;
                (*vkf).sem_value[i] = s.value;
            }
        }
    }
}

/// A `size` frame of `layout`, a system-memory format FFmpeg names
/// `layout.name`, uploaded by FFmpeg onto `device`, a Vulkan device, from
/// `planes`: each plane's bytes and rows apart. For tests that need a
/// frame on the device without a decoder.
pub(crate) fn upload(
    device: &HwDevice,
    layout: &PixelLayout,
    size: (u32, u32),
    planes: &[(&[u8], usize)],
) -> Result<ff::frame::Video, String> {
    if planes.len() != layout.planes.len() {
        return Err(format!(
            "{} planes for {}'s {}",
            planes.len(),
            layout.name,
            layout.planes.len()
        ));
    }
    // Each plane's rows, of how many bytes, checked against its bytes.
    let rows = layout
        .planes
        .iter()
        .zip(planes)
        .enumerate()
        .map(|(i, (desc, &(bytes, stride)))| {
            let (pw, ph) = desc.size(size.0, size.1);
            let row = desc.row_bytes(pw);
            let need = stride * (ph as usize).saturating_sub(1) + row;
            if stride < row || bytes.len() < need {
                return Err(format!(
                    "plane {i}: {} bytes {stride} apart can't hold {ph} rows of {row}",
                    bytes.len()
                ));
            }
            Ok((ph as usize, row))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let name =
        CString::new(layout.name.as_str()).map_err(|_| format!("{:?} has a NUL", layout.name))?;
    // SAFETY: a NUL-terminated name; returns the format's int, or -1.
    let fmt = unsafe { c::av_get_pix_fmt(name.as_ptr()) };
    if fmt < 0 {
        return Err(format!("FFmpeg has no format {}", layout.name));
    }
    let (w, h) = (
        c_int::try_from(size.0).map_err(|e| e.to_string())?,
        c_int::try_from(size.1).map_err(|e| e.to_string())?,
    );
    let ff_err = |what: &str, ret: c_int| format!("{what}: {}", ff::Error::from(ret));
    // SAFETY: `device.ptr` is a live Vulkan device context. The frames
    // context made on it is set up before init (its enum fields written
    // as the ints they are) and released once `dst` holds its own
    // reference; `src` is a system-memory frame whose buffers
    // av_frame_get_buffer made for its format and size, so each plane
    // holds `rows` rows of `row` bytes `linesize` apart.
    unsafe {
        let mut frames = sys::av_hwframe_ctx_alloc(device.ptr);
        if frames.is_null() {
            return Err("FFmpeg couldn't make a frames context".into());
        }
        let fc = (*frames).data.cast::<sys::AVHWFramesContext>();
        (*fc).format = sys::AVPixelFormat::AV_PIX_FMT_VULKAN;
        (&raw mut (*fc).sw_format).cast::<c_int>().write(fmt);
        (*fc).width = w;
        (*fc).height = h;
        let ret = sys::av_hwframe_ctx_init(frames);
        if ret < 0 {
            sys::av_buffer_unref(&mut frames);
            return Err(ff_err("making the frames context", ret));
        }
        let mut dst = ff::frame::Video::empty();
        let ret = sys::av_hwframe_get_buffer(frames, dst.as_mut_ptr(), 0);
        sys::av_buffer_unref(&mut frames);
        if ret < 0 {
            return Err(ff_err("getting a frame on the device", ret));
        }
        let mut src = ff::frame::Video::empty();
        let s = src.as_mut_ptr();
        (*s).format = fmt;
        (*s).width = w;
        (*s).height = h;
        let ret = sys::av_frame_get_buffer(s, 0);
        if ret < 0 {
            return Err(ff_err("allocating the source frame", ret));
        }
        for (i, (&(bytes, stride), &(n, row))) in planes.iter().zip(&rows).enumerate() {
            let data = (*s).data[i];
            let line = usize::try_from((*s).linesize[i]).unwrap_or(0);
            if data.is_null() || line < row {
                return Err(format!("the source frame's plane {i} is too small"));
            }
            for y in 0..n {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(y * stride),
                    data.add(y * line),
                    row,
                );
            }
        }
        let ret = sys::av_hwframe_transfer_data(dst.as_mut_ptr(), src.as_ptr(), 0);
        if ret < 0 {
            return Err(ff_err("uploading the frame", ret));
        }
        Ok(dst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spherecut_core::video::VkQueueGrant;

    fn handles(queues: Vec<VkQueueGrant>, device_extensions: Vec<String>) -> VkDeviceHandles {
        VkDeviceHandles {
            get_instance_proc_addr: 0,
            instance: 0,
            physical_device: 0,
            device: 0,
            api_version: 0,
            instance_extensions: Vec::new(),
            device_extensions,
            queues,
            keepalive: Arc::new(()),
            gate: Default::default(),
        }
    }

    #[test]
    fn shared_devices_are_refused_before_ffmpeg_sees_them() {
        // Nothing here reaches FFmpeg: the handles are null.
        let decode_only = VkQueueGrant {
            family: 1,
            count: 1,
            flags: 0x20,
            video_ops: 0x2,
        };
        let compute_only = VkQueueGrant {
            family: 2,
            count: 1,
            flags: VK_QUEUE_COMPUTE_BIT,
            video_ops: 0,
        };
        for queues in [vec![], vec![decode_only], vec![decode_only, compute_only]] {
            let e = HwDevice::vulkan_shared(&handles(queues.clone(), Vec::new())).unwrap_err();
            assert!(
                matches!(&e, HwError::Vulkan(m) if m.contains("compute and a transfer queue")),
                "{queues:?}: {e}"
            );
        }
        let both = VkQueueGrant {
            flags: VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT,
            ..compute_only
        };
        let nul = vec!["VK_KHR_\0bad".to_owned()];
        let e = HwDevice::vulkan_shared(&handles(vec![both], nul)).unwrap_err();
        assert!(
            matches!(e, HwError::Nul { .. } | HwError::NotBuiltIn(_)),
            "{e}"
        );
    }

    #[test]
    fn frames_off_a_vulkan_device_are_not_pictures() {
        let layout =
            crate::ffi::pixel_layout(sys::AVPixelFormat::AV_PIX_FMT_NV12 as c_int).unwrap();
        let mut frame = ff::frame::Video::new(ff::format::Pixel::NV12, 16, 8);
        frame.data_mut(0).fill(0);
        let (_, why) = VkFrame::new(frame, &layout).unwrap_err();
        assert!(why.contains("isn't a Vulkan frame"), "{why}");
    }
}
