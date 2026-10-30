//! One Vulkan device for decoding and rendering (slice 5b's path B,
//! DECISIONS D25): wgpu's device, made with the extensions and queues
//! FFmpeg's Vulkan Video needs, and the copies out of FFmpeg's pictures
//! into buffers wgpu reads, on a queue of Spherecut's own.
//!
//! FFmpeg orders every use of a decoded frame through the frame's
//! timeline semaphore, under the frame's lock, and leaves the picture in
//! a video layout wgpu can't name; a wgpu submission can't wait on or
//! signal that semaphore for itself (`docs/design/m1-5b.md`). So the
//! [`Copier`] takes part in FFmpeg's protocol on its own queue, copies
//! each plane into a device-local buffer, and the uploader copies the
//! buffer into the slot as the copy path does from its staging belt.
//!
//! The render crate's second `unsafe` module, after [`crate::dmabuf`]:
//! wgpu-hal's device creation with a callback, raw Vulkan through ash on
//! the queue wgpu doesn't know, and buffers handed to wgpu.

use std::cmp::Reverse;
use std::ffi::CStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ash::vk;
use ash::vk::Handle;
use spherecut_core::video::{VkDeviceHandles, VkImageState, VkPicture, VkQueueGrant};

use crate::error::RenderError;

type Vulkan = wgpu::hal::api::Vulkan;

/// How long a copy may take before the GPU is taken to be hung.
const COPY_TIMEOUT_NS: u64 = 5_000_000_000;

/// The copier's ring of command buffers: one is recorded while the
/// others may still run.
const RING: usize = 4;

/// Free buffers kept per copier.
const KEEP_BUFFERS: usize = 8;

/// Rows and planes in a copy's buffer start at multiples of this:
/// wgpu's `COPY_BYTES_PER_ROW_ALIGNMENT`.
pub const ROW_ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// The codecs FFmpeg's Vulkan Video decodes for Spherecut.
const CODECS: vk::VideoCodecOperationFlagsKHR = vk::VideoCodecOperationFlagsKHR::from_raw(
    vk::VideoCodecOperationFlagsKHR::DECODE_H264.as_raw()
        | vk::VideoCodecOperationFlagsKHR::DECODE_H265.as_raw(),
);

/// One queue family as the device lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Family {
    pub flags: vk::QueueFlags,
    pub count: u32,
    /// Its video codec operations; empty for a family without video.
    pub video_ops: vk::VideoCodecOperationFlagsKHR,
}

/// The queues a shared device is made with besides wgpu's own (family 0,
/// queue 0, which no one else submits to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuePlan {
    /// A compute family other than wgpu's: its queue 0 is FFmpeg's (frame
    /// preparation, uploads and downloads), its queue 1 the copier's.
    pub compute: u32,
    /// A video-decode family, whose queue 0 is FFmpeg's. It may be the
    /// compute family.
    pub decode: u32,
    pub decode_ops: vk::VideoCodecOperationFlagsKHR,
}

impl QueuePlan {
    /// The queue the copier submits to: family and index.
    pub fn copy_queue(&self) -> (u32, u32) {
        (self.compute, 1)
    }

    /// Each family besides wgpu's and how many of its queues the device
    /// is made with.
    pub fn queue_counts(&self) -> Vec<(u32, u32)> {
        if self.decode == self.compute {
            vec![(self.compute, 2)]
        } else {
            vec![(self.compute, 2), (self.decode, 1)]
        }
    }

    /// The queues FFmpeg is given: queue 0 of each family, for what each
    /// is for. The copier's queue shares the compute family, so FFmpeg's
    /// frames, made `CONCURRENT` over these families, are its to read.
    pub fn grants(&self) -> Vec<VkQueueGrant> {
        let compute = (vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER).as_raw();
        let decode = vk::QueueFlags::VIDEO_DECODE_KHR.as_raw();
        let ops = self.decode_ops.as_raw();
        if self.decode == self.compute {
            return vec![VkQueueGrant {
                family: self.compute,
                count: 1,
                flags: compute | decode,
                video_ops: ops,
            }];
        }
        vec![
            VkQueueGrant {
                family: self.compute,
                count: 1,
                flags: compute,
                video_ops: 0,
            },
            VkQueueGrant {
                family: self.decode,
                count: 1,
                flags: decode,
                video_ops: ops,
            },
        ]
    }
}

/// The queues a shared device needs from `families` (the device's, in
/// its order), or why it can't have them. Family 0 is wgpu's and is
/// never planned. The decode family is the one with H.265 first, then
/// the most codecs; the compute family one without graphics first.
pub fn plan_queues(families: &[Family]) -> Result<QueuePlan, String> {
    let (decode, d) = families
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, f)| {
            f.flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR)
                && f.count >= 1
                && f.video_ops.intersects(CODECS)
        })
        .max_by_key(|(i, f)| {
            (
                f.video_ops
                    .contains(vk::VideoCodecOperationFlagsKHR::DECODE_H265),
                f.video_ops.as_raw().count_ones(),
                Reverse(*i),
            )
        })
        .ok_or_else(|| {
            if families
                .iter()
                .any(|f| f.flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR))
            {
                "no video-decode queue family besides wgpu's decodes H.264 or H.265".to_owned()
            } else {
                "the device has no video-decode queue family".to_owned()
            }
        })?;
    let compute = families
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, f)| f.flags.contains(vk::QueueFlags::COMPUTE) && f.count >= 2)
        .min_by_key(|(i, f)| (f.flags.contains(vk::QueueFlags::GRAPHICS), *i))
        .map(|(i, _)| i)
        .ok_or_else(|| {
            "no compute queue family besides wgpu's has two queues (one for FFmpeg, one for \
             the copies)"
                .to_owned()
        })?;
    Ok(QueuePlan {
        compute: compute as u32,
        decode: decode as u32,
        decode_ops: d.video_ops & (CODECS | vk::VideoCodecOperationFlagsKHR::DECODE_AV1),
    })
}

/// The device extensions a shared device adds to wgpu's, from those the
/// device has (`available`), or why it can't be one. `api_version` is
/// what the device runs at: FFmpeg calls 1.3's entry points.
pub fn plan_extensions(
    available: &[&CStr],
    api_version: u32,
) -> Result<Vec<&'static CStr>, String> {
    if api_version < vk::API_VERSION_1_3 {
        return Err(format!(
            "the device runs Vulkan {}.{}, and FFmpeg's Vulkan Video needs 1.3",
            vk::api_version_major(api_version),
            vk::api_version_minor(api_version)
        ));
    }
    let has = |name: &CStr| available.contains(&name);
    let mut add = Vec::new();
    for name in [
        ash::khr::video_queue::NAME,
        ash::khr::video_decode_queue::NAME,
    ] {
        if !has(name) {
            return Err(format!("the device lacks {}", name.to_string_lossy()));
        }
        add.push(name);
    }
    let codecs = [
        ash::khr::video_decode_h264::NAME,
        ash::khr::video_decode_h265::NAME,
    ];
    if !codecs.iter().any(|c| has(c)) {
        return Err("the device decodes neither H.264 nor H.265 through Vulkan Video".into());
    }
    let optional = [
        ash::khr::video_decode_av1::NAME,
        ash::khr::video_maintenance1::NAME,
    ];
    add.extend(codecs.into_iter().chain(optional).filter(|c| has(c)));
    Ok(add)
}

/// What a shared device gives beside wgpu's device: the handles FFmpeg
/// is made from, and the copier.
#[derive(Debug)]
pub struct SharedVideo {
    pub handles: VkDeviceHandles,
    /// The queues, where [`open_shared`] planned them.
    pub plan: Option<QueuePlan>,
    pub copier: Copier,
}

/// Opens `adapter` as a shared device with `desc`'s features and limits,
/// or says why it can't be one. Blocks on the driver.
pub fn open_shared(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue, SharedVideo), String> {
    spherecut_core::may_block("opening a shared GPU device");
    let (add, plan) = {
        // SAFETY: the guard is dropped at the end of this block, and
        // nothing destroys the adapter meanwhile.
        let hal = unsafe { adapter.as_hal::<Vulkan>() }
            .ok_or_else(|| "the adapter isn't a Vulkan one".to_owned())?;
        let shared = hal.shared_instance();
        let instance = shared.raw_instance();
        let physical = hal.raw_physical_device();
        // SAFETY: `physical` is the adapter's own, of this instance.
        let props = unsafe { instance.get_physical_device_properties(physical) };
        // wgpu-hal asks for 1.3 at most (`Instance::init`).
        let api = props
            .api_version
            .min(shared.instance_api_version())
            .min(vk::API_VERSION_1_3);
        // SAFETY: as above.
        let available = unsafe { instance.enumerate_device_extension_properties(physical) }
            .map_err(|e| format!("listing the device's extensions: {e}"))?;
        let names: Vec<&CStr> = available
            .iter()
            .filter_map(|e| e.extension_name_as_c_str().ok())
            .collect();
        let add = plan_extensions(&names, api)?;
        let plan = plan_queues(&families(instance, physical, true))?;
        (add, plan)
    };
    // SAFETY: the extensions are the device's (`plan_extensions` took them
    // from its list), and the queues are of families it has, in counts it
    // has, none of them family 0.
    let (device, queue) = unsafe { open_with(adapter, desc, &add, &plan.queue_counts()) }?;
    let shared = share(
        &device,
        plan.grants(),
        plan.copy_queue(),
        [0, plan.compute],
        Some(plan),
    )?;
    Ok((device, queue, shared))
}

/// A device for tests on a GPU with one queue, as lavapipe has: wgpu's
/// device with synchronization2 on, FFmpeg given family 0's queue 0
/// (compute and transfer) and the copier the same queue. Blocks on the
/// driver.
///
/// # Safety
///
/// wgpu, FFmpeg and the copier all submit to one queue, which Vulkan
/// requires to be used by one thread at a time; nothing orders them but
/// the caller. A test that does one thing at a time from one thread, and
/// waits for each (the copier's [`Copier::wait`], FFmpeg's own transfers,
/// which wait), meets that. Production never makes one: [`open_shared`]
/// gives each its own queue.
#[doc(hidden)]
pub unsafe fn open_single_queue(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue, SharedVideo), String> {
    spherecut_core::may_block("opening a GPU device for a one-queue test");
    // SAFETY: no extensions or queues are added.
    let (device, queue) = unsafe { open_with(adapter, desc, &[], &[]) }?;
    let grants = vec![VkQueueGrant {
        family: 0,
        count: 1,
        flags: (vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER).as_raw(),
        video_ops: 0,
    }];
    let shared = share(&device, grants, (0, 0), [0, 0], None)?;
    Ok((device, queue, shared))
}

/// A [`Gpu`](crate::Gpu) on `choice`'s adapter made by
/// [`open_single_queue`], for path B's tests on a one-queue GPU.
///
/// # Safety
///
/// [`open_single_queue`]'s: one thing at a time on the device, from one
/// thread.
#[doc(hidden)]
pub unsafe fn single_queue_gpu(choice: &crate::AdapterChoice) -> Result<crate::Gpu, RenderError> {
    let (instance, adapter, desc) = crate::Gpu::single_queue_parts(choice)?;
    // SAFETY: the caller keeps its rules.
    let (device, queue, shared) =
        unsafe { open_single_queue(&adapter, &desc) }.map_err(RenderError::Device)?;
    Ok(crate::Gpu::from_parts(instance, adapter, device, queue).with_shared(shared))
}

/// `adapter`'s device with `desc`'s features and limits, plus `add`'s
/// extensions, `queues` (family, count) besides wgpu's, and
/// synchronization2.
///
/// # Safety
///
/// `add` must be extensions the device has, and `queues` families it has
/// (not family 0, which wgpu makes itself) in counts it has.
unsafe fn open_with(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
    add: &[&'static CStr],
    queues: &[(u32, u32)],
) -> Result<(wgpu::Device, wgpu::Queue), String> {
    let open = {
        // SAFETY: the guard is dropped at the end of this block, before the
        // device is wrapped, and nothing destroys the adapter meanwhile.
        let hal = unsafe { adapter.as_hal::<Vulkan>() }
            .ok_or_else(|| "the adapter isn't a Vulkan one".to_owned())?;
        static PRIORITIES: [f32; 2] = [1.0, 1.0];
        let infos: Vec<vk::DeviceQueueCreateInfo<'static>> = queues
            .iter()
            .map(|&(family, count)| {
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(family)
                    .queue_priorities(&PRIORITIES[..count.min(2) as usize])
            })
            .collect();
        let mut sync2 =
            vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true);
        let sync2 = &mut sync2;
        let add = add.to_vec();
        let callback: Box<wgpu::hal::vulkan::CreateDeviceCallback<'_>> = Box::new(move |args| {
            for name in add {
                if !args.extensions.contains(&name) {
                    args.extensions.push(name);
                }
            }
            args.queue_create_infos.extend(infos);
            // wgpu chains its feature structs one by one, and never
            // synchronization2's, so the chain has no two of a kind.
            let info = std::mem::take(args.create_info);
            *args.create_info = info.push_next(sync2);
        });
        // SAFETY: the callback only adds: extensions and queues the caller
        // vouches for, and synchronization2, which every Vulkan 1.3 device
        // has (wgpu-hal asks for 1.3, and FFmpeg needs it).
        unsafe {
            hal.open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(callback),
            )
        }
        .map_err(|e| format!("the driver refused the shared device: {e}"))?
    };
    // SAFETY: `open` was made from this adapter with `desc`'s features and
    // limits.
    unsafe { adapter.create_device_from_hal::<Vulkan>(open, desc) }
        .map_err(|e| format!("wgpu refused the shared device: {e}"))
}

/// The queue families of `physical`, with their video operations when
/// `video` (the device has `VK_KHR_video_queue`).
fn families(instance: &ash::Instance, physical: vk::PhysicalDevice, video: bool) -> Vec<Family> {
    // SAFETY: `physical` is of `instance`.
    let n = unsafe { instance.get_physical_device_queue_family_properties2_len(physical) };
    let mut videos = vec![vk::QueueFamilyVideoPropertiesKHR::default(); n];
    let basic: Vec<(vk::QueueFlags, u32)> = {
        let mut props: Vec<vk::QueueFamilyProperties2> = videos
            .iter_mut()
            .map(|v| {
                let p = vk::QueueFamilyProperties2::default();
                if video { p.push_next(v) } else { p }
            })
            .collect();
        // SAFETY: as many structs as the count said, each a valid chain
        // (the video struct only where the extension is there).
        unsafe { instance.get_physical_device_queue_family_properties2(physical, &mut props) };
        props
            .iter()
            .map(|p| {
                let q = p.queue_family_properties;
                (q.queue_flags, q.queue_count)
            })
            .collect()
    };
    basic
        .into_iter()
        .zip(&videos)
        .map(|((flags, count), v)| Family {
            flags,
            count,
            video_ops: v.video_codec_operations,
        })
        .collect()
}

/// The handles FFmpeg is made from (given `grants`), and a copier on
/// `copy` (family, index) whose buffers are shared between `sharing`'s
/// families.
fn share(
    device: &wgpu::Device,
    grants: Vec<VkQueueGrant>,
    copy: (u32, u32),
    sharing: [u32; 2],
    plan: Option<QueuePlan>,
) -> Result<SharedVideo, String> {
    let (handles, raw, memory) = {
        // SAFETY: the guard is dropped at the end of this block; the device
        // lives on in `device`, which the handles and the copier hold.
        let hal = unsafe { device.as_hal::<Vulkan>() }
            .ok_or_else(|| "the device isn't a Vulkan one".to_owned())?;
        let shared = hal.shared_instance();
        let instance = shared.raw_instance();
        // SAFETY: the device's own physical device.
        let memory =
            unsafe { instance.get_physical_device_memory_properties(hal.raw_physical_device()) };
        let names = |list: &[&CStr]| -> Vec<String> {
            list.iter()
                .map(|n| n.to_string_lossy().into_owned())
                .collect()
        };
        let handles = VkDeviceHandles {
            get_instance_proc_addr: shared.entry().static_fn().get_instance_proc_addr as usize,
            instance: instance.handle().as_raw() as usize,
            physical_device: hal.raw_physical_device().as_raw() as usize,
            device: hal.raw_device().handle().as_raw() as usize,
            api_version: shared.instance_api_version().min(vk::API_VERSION_1_3),
            instance_extensions: names(shared.extensions()),
            device_extensions: names(hal.enabled_device_extensions()),
            queues: grants,
            keepalive: Arc::new(device.clone()),
        };
        (handles, hal.raw_device().clone(), memory)
    };
    let (family, index) = copy;
    // SAFETY: the device was made with this queue (the callers made it so).
    let queue = unsafe { raw.get_device_queue(family, index) };
    let copier = Copier::new(device.clone(), raw, memory, queue, family, sharing)
        .map_err(|e| e.to_string())?;
    Ok(SharedVideo {
        handles,
        plan,
        copier,
    })
}

/// One plane to copy: its texel size, and the part of it to copy, from
/// the top-left corner (the shown part, as the copy path uploads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneRegion {
    pub bytes_per_texel: u32,
    pub size: (u32, u32),
}

/// Where a plane is in a [`Copied`] buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferPlane {
    pub offset: u64,
    pub bytes_per_row: u32,
    pub bytes_per_texel: u32,
    pub size: (u32, u32),
}

/// Where each plane of `regions` goes in a buffer, rows and planes
/// [`ROW_ALIGN`]ed, and the buffer's size.
pub fn buffer_layout(regions: &[PlaneRegion]) -> (Vec<BufferPlane>, u64) {
    let align = |n: u64| n.div_ceil(u64::from(ROW_ALIGN)) * u64::from(ROW_ALIGN);
    let mut offset = 0u64;
    let planes = regions
        .iter()
        .map(|r| {
            let row = align(u64::from(r.size.0) * u64::from(r.bytes_per_texel));
            let p = BufferPlane {
                offset,
                bytes_per_row: row as u32,
                bytes_per_texel: r.bytes_per_texel,
                size: r.size,
            };
            offset = align(offset + row * u64::from(r.size.1));
            p
        })
        .collect();
    (planes, offset.max(u64::from(ROW_ALIGN)))
}

/// A picture's planes, copied into a buffer of the copier's. The buffer
/// goes back to the copier's pool when this is dropped, which must be
/// after the GPU is done reading it (`Queue::on_submitted_work_done`).
#[derive(Debug)]
pub struct Copied {
    pub buffer: wgpu::Buffer,
    pub planes: Vec<BufferPlane>,
    /// The copier's semaphore value the copy signals.
    value: u64,
    raw: vk::Buffer,
    free: Arc<Mutex<Vec<PooledBuffer>>>,
}

impl Drop for Copied {
    fn drop(&mut self) {
        let mut free = lock(&self.free);
        if free.len() < KEEP_BUFFERS {
            free.push(PooledBuffer {
                buffer: self.buffer.clone(),
                raw: self.raw,
            });
        }
    }
}

/// A buffer of the copier's, with its raw handle.
#[derive(Debug)]
struct PooledBuffer {
    buffer: wgpu::Buffer,
    raw: vk::Buffer,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Copies decoded pictures out of FFmpeg's frames on a queue of its own
/// (module docs). One per shared device; its methods may be called from
/// any thread, and record and submit one at a time.
#[derive(Debug)]
pub struct Copier {
    inner: Mutex<Inner>,
    free: Arc<Mutex<Vec<PooledBuffer>>>,
}

struct Inner {
    /// Holds the Vulkan device for as long as the copier lives.
    device: wgpu::Device,
    raw: ash::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    family: u32,
    /// The families a buffer is shared between: wgpu's and the copier's.
    sharing: [u32; 2],
    pool: vk::CommandPool,
    /// Signalled by each copy, one value more than the last.
    timeline: vk::Semaphore,
    last: u64,
    ring: Vec<(vk::CommandBuffer, u64)>,
    next: usize,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Copier")
            .field("family", &self.family)
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

/// `RenderError::Import` for a Vulkan call that failed.
fn vk_err(what: &str) -> impl Fn(vk::Result) -> RenderError + '_ {
    move |e| RenderError::Import(format!("{what}: {e}"))
}

impl Copier {
    /// A copier that submits to `queue`, of `family`, on `device` (whose
    /// raw device `raw` is). Its buffers are shared between the families
    /// of `sharing`.
    ///
    /// No one else may submit to `queue`: [`open_shared`] makes the
    /// copier's queue for it alone. A test with nothing else on the
    /// device may hand it wgpu's own, as lavapipe has no other.
    pub fn new(
        device: wgpu::Device,
        raw: ash::Device,
        memory: vk::PhysicalDeviceMemoryProperties,
        queue: vk::Queue,
        family: u32,
        sharing: [u32; 2],
    ) -> Result<Copier, RenderError> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: `raw` is a live device, and `family` one of its queues'.
        let pool = unsafe { raw.create_command_pool(&pool_info, None) }
            .map_err(vk_err("making the copier's command pool"))?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(RING as u32);
        // SAFETY: `pool` is the device's own.
        let buffers = unsafe { raw.allocate_command_buffers(&alloc) };
        let buffers = match buffers {
            Ok(b) => b,
            Err(e) => {
                // SAFETY: nothing was made from the pool.
                unsafe { raw.destroy_command_pool(pool, None) };
                return Err(vk_err("making the copier's command buffers")(e));
            }
        };
        let mut kind = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let info = vk::SemaphoreCreateInfo::default().push_next(&mut kind);
        // SAFETY: a valid chain; the device has timeline semaphores (wgpu
        // enables them, and FFmpeg's frames need them).
        let timeline = match unsafe { raw.create_semaphore(&info, None) } {
            Ok(s) => s,
            Err(e) => {
                // SAFETY: the buffers are the pool's, and never submitted.
                unsafe { raw.destroy_command_pool(pool, None) };
                return Err(vk_err("making the copier's semaphore")(e));
            }
        };
        Ok(Copier {
            inner: Mutex::new(Inner {
                device,
                raw,
                memory,
                queue,
                family,
                sharing,
                pool,
                timeline,
                last: 0,
                ring: buffers.into_iter().map(|b| (b, 0)).collect(),
                next: 0,
            }),
            free: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// The queue family the copier submits to.
    pub fn family(&self) -> u32 {
        lock(&self.inner).family
    }

    /// Copies `regions` of `picture`'s planes into a buffer, by FFmpeg's
    /// rules for a frame: under its lock, after each image's semaphore
    /// reaches its value, and signalling the value after once the copy is
    /// done, which FFmpeg's next use of the frame waits for. Each image is
    /// left in `TRANSFER_SRC_OPTIMAL`, as its state then says. Doesn't wait
    /// for the copy ([`wait`](Self::wait) does), and the picture may be
    /// let go at once: FFmpeg reuses its surface only after the copy.
    pub fn copy(
        &self,
        picture: &dyn VkPicture,
        regions: &[PlaneRegion],
    ) -> Result<Copied, RenderError> {
        let planes = picture.planes();
        if planes.len() != regions.len() {
            return Err(RenderError::Invalid(format!(
                "{} regions for a picture of {} planes",
                regions.len(),
                planes.len()
            )));
        }
        for (i, (p, r)) in planes.iter().zip(regions).enumerate() {
            if r.size.0 > p.size.0 || r.size.1 > p.size.1 || r.size.0 == 0 || r.size.1 == 0 {
                return Err(RenderError::Invalid(format!(
                    "plane {i}'s region is {}x{}, of a {}x{} plane",
                    r.size.0, r.size.1, p.size.0, p.size.1
                )));
            }
        }
        let (layout, size) = buffer_layout(regions);
        let buffer = self.buffer(size)?;
        let mut inner = lock(&self.inner);
        let value = inner.record_and_submit(picture, &layout, buffer.raw)?;
        Ok(Copied {
            buffer: buffer.buffer,
            planes: layout,
            value,
            raw: buffer.raw,
            free: self.free.clone(),
        })
    }

    /// Waits, on the CPU, for `copied`'s copy to be done. Blocks.
    pub fn wait(&self, copied: &Copied) -> Result<(), RenderError> {
        spherecut_core::may_block("waiting for a GPU copy");
        let (raw, timeline) = {
            let inner = lock(&self.inner);
            (inner.raw.clone(), inner.timeline)
        };
        let semaphores = [timeline];
        let values = [copied.value];
        let info = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: the semaphore is the copier's, alive while it is (the
        // caller has the copier).
        match unsafe { raw.wait_semaphores(&info, COPY_TIMEOUT_NS) } {
            Ok(()) => Ok(()),
            Err(vk::Result::TIMEOUT) => Err(RenderError::Device(format!(
                "a copy out of the decoder took over {} s",
                COPY_TIMEOUT_NS / 1_000_000_000
            ))),
            Err(e) => Err(vk_err("waiting for a copy out of the decoder")(e)),
        }
    }

    /// Has wgpu's next submission on `queue` wait for `copied`'s copy on
    /// the GPU too, at the transfer stage. Vulkan orders the copy's
    /// writes before wgpu's reads of the buffer only so: [`wait`]'s host
    /// wait shows the copy is done, but doesn't make its writes visible
    /// to a later submission. The copy is done by then, so the wait costs
    /// nothing, and whichever submission takes it (another thread's, if
    /// it submits first), wgpu chains its submissions one after another,
    /// so the one that reads the buffer is ordered after it.
    ///
    /// [`wait`]: Self::wait
    pub fn order_before(&self, queue: &wgpu::Queue, copied: &Copied) {
        let timeline = lock(&self.inner).timeline;
        // SAFETY: only stages a wait on wgpu's queue, on the copier's
        // semaphore, which lives as long as the copier, and so as the
        // device; wgpu consumes it at its next submit.
        if let Some(q) = unsafe { queue.as_hal::<Vulkan>() } {
            q.add_wait_semaphore(
                timeline,
                Some(copied.value),
                vk::PipelineStageFlags::TRANSFER,
            );
        }
    }

    /// A free buffer of `size` bytes, or a new one.
    fn buffer(&self, size: u64) -> Result<PooledBuffer, RenderError> {
        {
            let mut free = lock(&self.free);
            if let Some(i) = free.iter().position(|b| b.buffer.size() == size) {
                return Ok(free.swap_remove(i));
            }
        }
        lock(&self.inner).make_buffer(size)
    }
}

impl Inner {
    /// A device-local buffer of `size` bytes, shared between wgpu's family
    /// and the copier's, handed to wgpu (which frees it).
    fn make_buffer(&self, size: u64) -> Result<PooledBuffer, RenderError> {
        let distinct = self.sharing[0] != self.sharing[1];
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST);
        let info = if distinct {
            info.sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&self.sharing)
        } else {
            info.sharing_mode(vk::SharingMode::EXCLUSIVE)
        };
        // SAFETY: a valid create info on a live device.
        let buffer = unsafe { self.raw.create_buffer(&info, None) }
            .map_err(vk_err("making a copy buffer"))?;
        // SAFETY: `buffer` is the device's own.
        let req = unsafe { self.raw.get_buffer_memory_requirements(buffer) };
        let destroy = |e: RenderError| {
            // SAFETY: `buffer` was never used.
            unsafe { self.raw.destroy_buffer(buffer, None) };
            e
        };
        let types = &self.memory.memory_types[..self.memory.memory_type_count as usize];
        let ty = (0..types.len())
            .find(|&i| {
                req.memory_type_bits & (1 << i) != 0
                    && types[i]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .ok_or_else(|| {
                destroy(RenderError::Import(
                    "no device-local memory takes a copy buffer".into(),
                ))
            })?;
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(ty as u32);
        // SAFETY: a type the buffer takes, of the device's own.
        let memory = unsafe { self.raw.allocate_memory(&alloc, None) }.map_err(|e| {
            destroy(match e {
                vk::Result::ERROR_OUT_OF_DEVICE_MEMORY | vk::Result::ERROR_OUT_OF_HOST_MEMORY => {
                    RenderError::OutOfMemory("a copy buffer out of the decoder")
                }
                e => vk_err("allocating a copy buffer")(e),
            })
        })?;
        // SAFETY: `memory` is as large as the buffer needs, of a type it
        // takes, and bound once.
        if let Err(e) = unsafe { self.raw.bind_buffer_memory(buffer, memory, 0) } {
            // SAFETY: neither was used.
            unsafe { self.raw.free_memory(memory, None) };
            return Err(destroy(vk_err("binding a copy buffer")(e)));
        }
        // SAFETY: wgpu takes the buffer and its memory, and frees both when
        // the last handle to it goes; the copier only records copies into
        // it while it holds a handle (in its pool or a `Copied`).
        let hal =
            unsafe { wgpu::hal::vulkan::Buffer::from_raw_managed(buffer, memory, 0, req.size) };
        let desc = wgpu::BufferDescriptor {
            label: Some("decoder copy"),
            size,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        };
        // SAFETY: `hal` was made on this device, `size` bytes long, with the
        // usage `desc` says (transfers both ways), and isn't mapped.
        let wrapped = unsafe { self.device.create_buffer_from_hal::<Vulkan>(hal, &desc) };
        Ok(PooledBuffer {
            buffer: wrapped,
            raw: buffer,
        })
    }

    /// Records the copy of `picture` into `buffer` (laid out as `layout`)
    /// and submits it under the frame's lock; the copier's semaphore
    /// value it signals.
    fn record_and_submit(
        &mut self,
        picture: &dyn VkPicture,
        layout: &[BufferPlane],
        buffer: vk::Buffer,
    ) -> Result<u64, RenderError> {
        let (cmd, previous) = self.ring[self.next];
        if previous > 0 {
            self.wait_value(previous)?;
        }
        // SAFETY: `cmd` is the pool's, and its last submission is done.
        unsafe {
            self.raw
                .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())
                .map_err(vk_err("resetting the copier's command buffer"))?;
        }
        let planes = picture.planes().to_vec();
        let value = self.last + 1;
        let mut outcome: Result<(), RenderError> = Ok(());
        picture.with_locked(&mut |states: &mut [VkImageState]| {
            outcome = self.record_locked(cmd, states, &planes, layout, buffer, value);
            if outcome.is_ok() {
                for s in states.iter_mut() {
                    s.layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL.as_raw();
                    s.access = vk::AccessFlags2::TRANSFER_READ.as_raw();
                    s.value += 1;
                }
            }
        });
        outcome?;
        self.last = value;
        self.ring[self.next].1 = value;
        self.next = (self.next + 1) % self.ring.len();
        Ok(value)
    }

    /// The copy's commands and submission, with the frame locked.
    fn record_locked(
        &self,
        cmd: vk::CommandBuffer,
        states: &[VkImageState],
        planes: &[spherecut_core::video::VkPlane],
        layout: &[BufferPlane],
        buffer: vk::Buffer,
        value: u64,
    ) -> Result<(), RenderError> {
        for (i, s) in states.iter().enumerate() {
            if s.image == 0 || s.semaphore == 0 {
                return Err(RenderError::Import(format!(
                    "the decoder's image {i} has no image or semaphore"
                )));
            }
            if s.queue_family != vk::QUEUE_FAMILY_IGNORED && s.queue_family != self.family {
                return Err(RenderError::Import(format!(
                    "the decoder's image {i} belongs to queue family {}, not the copier's {}",
                    s.queue_family, self.family
                )));
            }
        }
        if let Some(p) = planes.iter().find(|p| p.image >= states.len()) {
            return Err(RenderError::Import(format!(
                "a plane is in image {} of {}",
                p.image,
                states.len()
            )));
        }
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: `cmd` was reset and isn't in use.
        unsafe { self.raw.begin_command_buffer(cmd, &begin) }
            .map_err(vk_err("recording a copy"))?;
        // Each image into TRANSFER_SRC_OPTIMAL, its planes' aspects at once.
        // The semaphore wait orders this after FFmpeg's work on it and makes
        // that work's writes available, so nothing needs to be named as the
        // barrier's source. The image is CONCURRENT over the copier's
        // family (or this family's alone), so no ownership changes.
        let barriers: Vec<vk::ImageMemoryBarrier2> = states
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let aspect = planes
                    .iter()
                    .filter(|p| p.image == i)
                    .fold(vk::ImageAspectFlags::empty(), |a, p| {
                        a | vk::ImageAspectFlags::from_raw(p.aspect)
                    });
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .src_access_mask(vk::AccessFlags2::NONE)
                    .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                    .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::from_raw(s.layout))
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(vk::Image::from_raw(s.image))
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: aspect,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
            })
            .collect();
        let dependency = vk::DependencyInfo::default().image_memory_barriers(&barriers);
        // SAFETY: `cmd` is recording; the images are live (the frame holds
        // them) and their layouts are what the frame records.
        unsafe { self.raw.cmd_pipeline_barrier2(cmd, &dependency) };
        for (p, at) in planes.iter().zip(layout) {
            let texel_row = at.bytes_per_row / at.bytes_per_texel.max(1);
            let region = vk::BufferImageCopy::default()
                .buffer_offset(at.offset)
                .buffer_row_length(texel_row)
                .buffer_image_height(at.size.1)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::from_raw(p.aspect),
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: at.size.0,
                    height: at.size.1,
                    depth: 1,
                });
            // SAFETY: the image is in TRANSFER_SRC_OPTIMAL after the barrier;
            // the region lies inside the plane (checked in `copy`) and inside
            // the buffer (`buffer_layout`).
            unsafe {
                self.raw.cmd_copy_image_to_buffer(
                    cmd,
                    vk::Image::from_raw(states[p.image].image),
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[region],
                )
            };
        }
        // SAFETY: `cmd` is recording.
        unsafe { self.raw.end_command_buffer(cmd) }.map_err(vk_err("recording a copy"))?;
        let waits: Vec<vk::SemaphoreSubmitInfo> = states
            .iter()
            .map(|s| {
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(vk::Semaphore::from_raw(s.semaphore))
                    .value(s.value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            })
            .collect();
        let signals: Vec<vk::SemaphoreSubmitInfo> = states
            .iter()
            .map(|s| {
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(vk::Semaphore::from_raw(s.semaphore))
                    .value(s.value + 1)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            })
            .chain(std::iter::once(
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(self.timeline)
                    .value(value)
                    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
            ))
            .collect();
        let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
        let submit = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&waits)
            .command_buffer_infos(&cmds)
            .signal_semaphore_infos(&signals);
        // SAFETY: the copier's queue, which only it submits to, under its
        // lock; the semaphores are timeline ones, signalled to values past
        // what they hold (the frame's at one past its last use, under its
        // lock; the copier's at one past its last).
        unsafe {
            self.raw
                .queue_submit2(self.queue, &[submit], vk::Fence::null())
                .map_err(vk_err("submitting a copy"))
        }
    }

    /// Waits for the copier's semaphore to reach `value`.
    fn wait_value(&self, value: u64) -> Result<(), RenderError> {
        let semaphores = [self.timeline];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::default()
            .semaphores(&semaphores)
            .values(&values);
        // SAFETY: the copier's own semaphore.
        unsafe { self.raw.wait_semaphores(&info, COPY_TIMEOUT_NS) }
            .map_err(vk_err("waiting for an earlier copy"))
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.last > 0 && self.wait_value(self.last).is_err() {
            tracing::error!("the copier's last copy never finished; its pool is leaked");
            return;
        }
        // SAFETY: every submission of the pool's buffers is done, and the
        // device lives on in `self.device` until after this.
        unsafe {
            self.raw.destroy_semaphore(self.timeline, None);
            self.raw.destroy_command_pool(self.pool, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(flags: vk::QueueFlags, count: u32, ops: vk::VideoCodecOperationFlagsKHR) -> Family {
        Family {
            flags,
            count,
            video_ops: ops,
        }
    }

    const GCT: vk::QueueFlags = vk::QueueFlags::from_raw(
        vk::QueueFlags::GRAPHICS.as_raw()
            | vk::QueueFlags::COMPUTE.as_raw()
            | vk::QueueFlags::TRANSFER.as_raw(),
    );
    const CT: vk::QueueFlags = vk::QueueFlags::from_raw(
        vk::QueueFlags::COMPUTE.as_raw() | vk::QueueFlags::TRANSFER.as_raw(),
    );
    const DECODE: vk::QueueFlags = vk::QueueFlags::VIDEO_DECODE_KHR;
    const NONE: vk::VideoCodecOperationFlagsKHR = vk::VideoCodecOperationFlagsKHR::NONE;
    const ALL: vk::VideoCodecOperationFlagsKHR = vk::VideoCodecOperationFlagsKHR::from_raw(
        CODECS.as_raw() | vk::VideoCodecOperationFlagsKHR::DECODE_AV1.as_raw(),
    );

    #[test]
    fn queues_are_planned_as_radv_and_anv_list_them() {
        // RADV on an RX 7600: graphics, four compute queues, decode,
        // encode.
        let radv = [
            family(GCT, 1, NONE),
            family(CT, 4, NONE),
            family(DECODE, 1, ALL),
            family(vk::QueueFlags::VIDEO_ENCODE_KHR, 1, NONE),
        ];
        let plan = plan_queues(&radv).unwrap();
        assert_eq!(
            plan,
            QueuePlan {
                compute: 1,
                decode: 2,
                decode_ops: ALL
            }
        );
        assert_eq!(plan.copy_queue(), (1, 1));
        assert_eq!(plan.queue_counts(), vec![(1, 2), (2, 1)]);
        let grants = plan.grants();
        assert_eq!(grants.len(), 2);
        assert_eq!((grants[0].family, grants[0].count), (1, 1));
        assert_eq!(grants[0].flags, CT.as_raw());
        assert_eq!((grants[1].family, grants[1].flags), (2, DECODE.as_raw()));
        assert_eq!(grants[1].video_ops, ALL.as_raw());

        // Two decode families: the one with H.265 wins, then the one with
        // more codecs, then the first.
        let h264 = vk::VideoCodecOperationFlagsKHR::DECODE_H264;
        let two = [
            family(GCT, 1, NONE),
            family(DECODE, 1, h264),
            family(CT, 2, NONE),
            family(DECODE, 2, CODECS),
            family(DECODE, 2, ALL),
        ];
        assert_eq!(plan_queues(&two).unwrap().decode, 4);
        assert_eq!(plan_queues(&two[..4]).unwrap().decode, 3);
        assert_eq!(plan_queues(&two[..3]).unwrap().decode, 1);

        // A dedicated compute family over one that also draws.
        let mixed = [
            family(GCT, 1, NONE),
            family(GCT, 2, NONE),
            family(CT, 2, NONE),
            family(DECODE, 1, CODECS),
        ];
        assert_eq!(plan_queues(&mixed).unwrap().compute, 2);
        assert_eq!(
            plan_queues(&mixed[..2]).unwrap_err(),
            "the device has no video-decode queue family"
        );

        // A family that computes and decodes alike serves both.
        let both = [family(GCT, 1, NONE), family(CT | DECODE, 2, CODECS)];
        let plan = plan_queues(&both).unwrap();
        assert_eq!((plan.compute, plan.decode), (1, 1));
        assert_eq!(plan.queue_counts(), vec![(1, 2)]);
        let grants = plan.grants();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].flags, (CT | DECODE).as_raw());
    }

    #[test]
    fn queues_are_refused_with_the_reason() {
        let lavapipe = [family(GCT | vk::QueueFlags::SPARSE_BINDING, 1, NONE)];
        assert_eq!(
            plan_queues(&lavapipe).unwrap_err(),
            "the device has no video-decode queue family"
        );
        // ANV with one compute queue: nothing for the copier.
        let anv = [
            family(GCT, 1, NONE),
            family(CT, 1, NONE),
            family(DECODE, 1, CODECS),
        ];
        assert!(
            plan_queues(&anv)
                .unwrap_err()
                .starts_with("no compute queue family besides wgpu's has two queues"),
        );
        // Decoding only on wgpu's family, or only AV1.
        let only_zero = [family(GCT | DECODE, 4, CODECS), family(CT, 4, NONE)];
        let av1 = [
            family(GCT, 1, NONE),
            family(CT, 4, NONE),
            family(DECODE, 1, vk::VideoCodecOperationFlagsKHR::DECODE_AV1),
        ];
        for families in [&only_zero[..], &av1[..]] {
            assert_eq!(
                plan_queues(families).unwrap_err(),
                "no video-decode queue family besides wgpu's decodes H.264 or H.265"
            );
        }
    }

    #[test]
    fn extensions_are_planned_from_what_the_device_has() {
        use ash::khr;
        let base = [
            khr::video_queue::NAME,
            khr::video_decode_queue::NAME,
            khr::video_decode_h265::NAME,
            khr::swapchain::NAME,
        ];
        assert_eq!(
            plan_extensions(&base, vk::API_VERSION_1_3).unwrap(),
            vec![
                khr::video_queue::NAME,
                khr::video_decode_queue::NAME,
                khr::video_decode_h265::NAME
            ]
        );
        let all = [
            khr::video_maintenance1::NAME,
            khr::video_decode_av1::NAME,
            khr::video_decode_h264::NAME,
            khr::video_decode_h265::NAME,
            khr::video_decode_queue::NAME,
            khr::video_queue::NAME,
        ];
        assert_eq!(
            plan_extensions(&all, vk::make_api_version(0, 1, 4, 318)).unwrap(),
            vec![
                khr::video_queue::NAME,
                khr::video_decode_queue::NAME,
                khr::video_decode_h264::NAME,
                khr::video_decode_h265::NAME,
                khr::video_decode_av1::NAME,
                khr::video_maintenance1::NAME
            ]
        );
        assert_eq!(
            plan_extensions(&all, vk::API_VERSION_1_2).unwrap_err(),
            "the device runs Vulkan 1.2, and FFmpeg's Vulkan Video needs 1.3"
        );
        assert_eq!(
            plan_extensions(&base[1..], vk::API_VERSION_1_3).unwrap_err(),
            "the device lacks VK_KHR_video_queue"
        );
        assert_eq!(
            plan_extensions(
                &[khr::video_queue::NAME, khr::video_decode_queue::NAME],
                vk::API_VERSION_1_3
            )
            .unwrap_err(),
            "the device decodes neither H.264 nor H.265 through Vulkan Video"
        );
    }

    /// A picture made here as FFmpeg makes its frames: images filled by a
    /// copy from a buffer, left in `GENERAL`, with a timeline semaphore at
    /// 1. One multi-planar image, or (as FFmpeg 6.1 may) one a plane.
    struct Picture {
        raw: ash::Device,
        planes: Vec<spherecut_core::video::VkPlane>,
        states: Mutex<Vec<VkImageState>>,
        made: Vec<(vk::Image, vk::DeviceMemory, vk::Semaphore)>,
    }

    impl std::fmt::Debug for Picture {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Picture")
                .field("planes", &self.planes)
                .finish_non_exhaustive()
        }
    }

    impl VkPicture for Picture {
        fn planes(&self) -> &[spherecut_core::video::VkPlane] {
            &self.planes
        }
        fn with_locked(&self, f: &mut dyn FnMut(&mut [VkImageState])) {
            f(&mut lock(&self.states));
        }
    }

    impl Drop for Picture {
        fn drop(&mut self) {
            // SAFETY: the test waited for every use of these.
            unsafe {
                self.raw.device_wait_idle().unwrap();
                for &(image, memory, semaphore) in &self.made {
                    self.raw.destroy_image(image, None);
                    self.raw.free_memory(memory, None);
                    self.raw.destroy_semaphore(semaphore, None);
                }
            }
        }
    }

    /// The raw device, queue and memory types of `gpu`'s device.
    fn raw_parts(gpu: &crate::Gpu) -> (ash::Device, vk::Queue, vk::PhysicalDeviceMemoryProperties) {
        // SAFETY: the guards are dropped here; the device outlives the test's
        // use of the clones (the test holds `gpu`).
        unsafe {
            let d = gpu.device.as_hal::<Vulkan>().unwrap();
            let q = gpu.queue.as_hal::<Vulkan>().unwrap();
            let memory = d
                .shared_instance()
                .raw_instance()
                .get_physical_device_memory_properties(d.raw_physical_device());
            (d.raw_device().clone(), q.as_raw(), memory)
        }
    }

    fn memory_type(
        memory: &vk::PhysicalDeviceMemoryProperties,
        bits: u32,
        want: vk::MemoryPropertyFlags,
    ) -> u32 {
        (0..memory.memory_type_count)
            .find(|&i| {
                bits & (1 << i) != 0
                    && memory.memory_types[i as usize]
                        .property_flags
                        .contains(want)
            })
            .unwrap()
    }

    /// An image's format and size.
    type Image = (vk::Format, (u32, u32));
    /// A plane: its image, aspect, size and bytes per texel.
    type Plane = (usize, vk::ImageAspectFlags, (u32, u32), u32);

    /// A picture whose planes hold `bytes[i]` (tightly packed rows), of
    /// `format` (one multi-planar image) or `formats` (an image a plane).
    #[allow(clippy::too_many_lines)]
    fn picture(gpu: &crate::Gpu, images: &[Image], planes: &[Plane], bytes: &[Vec<u8>]) -> Picture {
        let (raw, queue, memory) = raw_parts(gpu);
        let mut made = Vec::new();
        // SAFETY: everything below is made, used and waited for here, on a
        // device nothing else uses meanwhile (one test thread).
        unsafe {
            for &(format, (w, h)) in images {
                let info = vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: w,
                        height: h,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED);
                let image = raw.create_image(&info, None).unwrap();
                let req = raw.get_image_memory_requirements(image);
                let alloc = vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(memory_type(
                        &memory,
                        req.memory_type_bits,
                        vk::MemoryPropertyFlags::DEVICE_LOCAL,
                    ));
                let mem = raw.allocate_memory(&alloc, None).unwrap();
                raw.bind_image_memory(image, mem, 0).unwrap();
                let mut kind = vk::SemaphoreTypeCreateInfo::default()
                    .semaphore_type(vk::SemaphoreType::TIMELINE)
                    .initial_value(0);
                let sem = raw
                    .create_semaphore(
                        &vk::SemaphoreCreateInfo::default().push_next(&mut kind),
                        None,
                    )
                    .unwrap();
                made.push((image, mem, sem));
            }
            // The planes' bytes in a host-visible buffer.
            let total: usize = bytes.iter().map(Vec::len).sum();
            let info = vk::BufferCreateInfo::default()
                .size(total as u64)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let staging = raw.create_buffer(&info, None).unwrap();
            let req = raw.get_buffer_memory_requirements(staging);
            let host =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(memory_type(&memory, req.memory_type_bits, host));
            let staging_mem = raw.allocate_memory(&alloc, None).unwrap();
            raw.bind_buffer_memory(staging, staging_mem, 0).unwrap();
            let map = raw
                .map_memory(staging_mem, 0, total as u64, vk::MemoryMapFlags::empty())
                .unwrap()
                .cast::<u8>();
            let mut offsets = Vec::new();
            let mut at = 0usize;
            for b in bytes {
                std::ptr::copy_nonoverlapping(b.as_ptr(), map.add(at), b.len());
                offsets.push(at as u64);
                at += b.len();
            }
            raw.unmap_memory(staging_mem);
            let pool = raw
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default().queue_family_index(0),
                    None,
                )
                .unwrap();
            let cmd = raw
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1),
                )
                .unwrap()[0];
            raw.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default())
                .unwrap();
            let barrier = |image: vk::Image, aspect, old, new| {
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
                    .old_layout(old)
                    .new_layout(new)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: aspect,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
            };
            let aspects = |i: usize| {
                planes
                    .iter()
                    .filter(|p| p.0 == i)
                    .fold(vk::ImageAspectFlags::empty(), |a, p| a | p.1)
            };
            let to_dst: Vec<_> = made
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    barrier(
                        m.0,
                        aspects(i),
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    )
                })
                .collect();
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_dst),
            );
            for (p, off) in planes.iter().zip(&offsets) {
                let region = vk::BufferImageCopy::default()
                    .buffer_offset(*off)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: p.1,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: p.2.0,
                        height: p.2.1,
                        depth: 1,
                    });
                raw.cmd_copy_buffer_to_image(
                    cmd,
                    staging,
                    made[p.0].0,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            }
            let to_general: Vec<_> = made
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    barrier(
                        m.0,
                        aspects(i),
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::GENERAL,
                    )
                })
                .collect();
            raw.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().image_memory_barriers(&to_general),
            );
            raw.end_command_buffer(cmd).unwrap();
            let signals: Vec<_> = made
                .iter()
                .map(|m| {
                    vk::SemaphoreSubmitInfo::default()
                        .semaphore(m.2)
                        .value(1)
                        .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                })
                .collect();
            let cmds = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = vk::SubmitInfo2::default()
                .command_buffer_infos(&cmds)
                .signal_semaphore_infos(&signals);
            raw.queue_submit2(queue, &[submit], vk::Fence::null())
                .unwrap();
            raw.queue_wait_idle(queue).unwrap();
            raw.destroy_command_pool(pool, None);
            raw.destroy_buffer(staging, None);
            raw.free_memory(staging_mem, None);
        }
        let states = made
            .iter()
            .map(|m| VkImageState {
                image: m.0.as_raw(),
                layout: vk::ImageLayout::GENERAL.as_raw(),
                access: vk::AccessFlags2::TRANSFER_WRITE.as_raw(),
                queue_family: vk::QUEUE_FAMILY_IGNORED,
                semaphore: m.2.as_raw(),
                value: 1,
            })
            .collect();
        Picture {
            raw,
            planes: planes
                .iter()
                .map(|p| spherecut_core::video::VkPlane {
                    image: p.0,
                    aspect: p.1.as_raw(),
                    size: p.2,
                })
                .collect(),
            states: Mutex::new(states),
            made,
        }
    }

    /// `buffer`'s bytes, through a buffer wgpu can map.
    fn read_buffer(gpu: &crate::Gpu, buffer: &wgpu::Buffer) -> Vec<u8> {
        let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: buffer.size(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_buffer_to_buffer(buffer, 0, &out, 0, buffer.size());
        gpu.queue.submit([enc.finish()]);
        out.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        out.slice(..).get_mapped_range().unwrap().to_vec()
    }

    #[test]
    fn the_copier_copies_planes_by_ffmpegs_rules() {
        // lavapipe has one queue, so the copier shares wgpu's, one thing at
        // a time from this thread (`open_single_queue`'s rules).
        // SAFETY: as just said.
        let gpu = unsafe { single_queue_gpu(&crate::AdapterChoice::Cpu) }.unwrap();
        let shared = gpu.shared_video().unwrap().clone();
        let copier = &shared.copier;
        let (w, h) = (70u32, 34u32);
        let bytes = |n: usize, seed: u8| -> Vec<u8> {
            (0..n)
                .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
                .collect()
        };
        let p0 = vk::ImageAspectFlags::PLANE_0;
        let p1 = vk::ImageAspectFlags::PLANE_1;
        let color = vk::ImageAspectFlags::COLOR;
        let c = (w / 2, h / 2);
        let cases: Vec<(&str, Vec<Image>, Vec<Plane>)> = vec![
            (
                "NV12",
                vec![(vk::Format::G8_B8R8_2PLANE_420_UNORM, (w, h))],
                vec![(0, p0, (w, h), 1), (0, p1, c, 2)],
            ),
            (
                "P010",
                vec![(
                    vk::Format::G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
                    (w, h),
                )],
                vec![(0, p0, (w, h), 2), (0, p1, c, 4)],
            ),
            (
                "NV12 an image a plane",
                vec![(vk::Format::R8_UNORM, (w, h)), (vk::Format::R8G8_UNORM, c)],
                vec![(0, color, (w, h), 1), (1, color, c, 2)],
            ),
        ];
        for (name, images, planes) in cases {
            let data: Vec<Vec<u8>> = planes
                .iter()
                .enumerate()
                .map(|(i, p)| bytes((p.2.0 * p.2.1 * p.3) as usize, i as u8 * 7 + 1))
                .collect();
            let pic = picture(&gpu, &images, &planes, &data);
            // The whole picture, then its top half (a top-bottom frame's
            // shown eye): the second copy waits for the first's signal.
            for rows in [1, 2] {
                let regions: Vec<PlaneRegion> = planes
                    .iter()
                    .map(|p| PlaneRegion {
                        bytes_per_texel: p.3,
                        size: (p.2.0, p.2.1 / rows),
                    })
                    .collect();
                let before = lock(&pic.states).clone();
                let copied = copier.copy(&pic, &regions).unwrap();
                copier.wait(&copied).unwrap();
                let got = read_buffer(&gpu, &copied.buffer);
                for (i, (at, r)) in copied.planes.iter().zip(&regions).enumerate() {
                    let row = (r.size.0 * r.bytes_per_texel) as usize;
                    for y in 0..r.size.1 as usize {
                        let o = at.offset as usize + y * at.bytes_per_row as usize;
                        assert_eq!(
                            &got[o..o + row],
                            &data[i][y * row..(y + 1) * row],
                            "{name}, 1/{rows}: plane {i} row {y}"
                        );
                    }
                }
                // Each image's state says what the copy did, and its
                // semaphore is at the value it says.
                let after = lock(&pic.states).clone();
                for (b, a) in before.iter().zip(&after) {
                    assert_eq!(
                        a.layout,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL.as_raw(),
                        "{name}"
                    );
                    assert_eq!(a.access, vk::AccessFlags2::TRANSFER_READ.as_raw());
                    assert_eq!(a.value, b.value + 1);
                    assert_eq!(
                        (a.image, a.semaphore, a.queue_family),
                        (b.image, b.semaphore, b.queue_family)
                    );
                    // SAFETY: the picture's own semaphore, alive.
                    let now = unsafe {
                        pic.raw
                            .get_semaphore_counter_value(vk::Semaphore::from_raw(a.semaphore))
                    }
                    .unwrap();
                    assert_eq!(now, a.value, "{name}");
                }
            }
        }
    }

    #[test]
    fn the_copier_refuses_what_it_cant_copy() {
        // SAFETY: one thing at a time on the device, from this thread.
        let gpu = unsafe { single_queue_gpu(&crate::AdapterChoice::Cpu) }.unwrap();
        let shared = gpu.shared_video().unwrap().clone();
        let copier = &shared.copier;
        let planes = [
            (0, vk::ImageAspectFlags::PLANE_0, (64, 32), 1),
            (0, vk::ImageAspectFlags::PLANE_1, (32, 16), 2),
        ];
        let data = vec![vec![1u8; 64 * 32], vec![2u8; 32 * 16 * 2]];
        let pic = picture(
            &gpu,
            &[(vk::Format::G8_B8R8_2PLANE_420_UNORM, (64, 32))],
            &planes,
            &data,
        );
        let region = |size, bytes_per_texel| PlaneRegion {
            bytes_per_texel,
            size,
        };
        let good = [region((64, 32), 1), region((32, 16), 2)];
        let code = |regions: &[PlaneRegion]| copier.copy(&pic, regions).unwrap_err().code();
        assert_eq!(code(&good[..1]), "invalid_input");
        assert_eq!(code(&[region((65, 32), 1), good[1]]), "invalid_input");
        assert_eq!(code(&[good[0], region((32, 0), 2)]), "invalid_input");
        // An image another queue family owns is left as it is.
        lock(&pic.states)[0].queue_family = 7;
        let before = lock(&pic.states).clone();
        let e = copier.copy(&pic, &good).unwrap_err();
        assert_eq!(e.code(), "import_failed");
        assert!(e.to_string().contains("belongs to queue family 7"), "{e}");
        assert_eq!(*lock(&pic.states), before);
        lock(&pic.states)[0].queue_family = vk::QUEUE_FAMILY_IGNORED;
        let copied = copier.copy(&pic, &good).unwrap();
        copier.wait(&copied).unwrap();
        // Its buffer goes back to the pool, and the next copy of the size
        // takes it.
        let first = copied.buffer.clone();
        drop(copied);
        let again = copier.copy(&pic, &good).unwrap();
        assert_eq!(again.buffer, first);
        copier.wait(&again).unwrap();
    }

    #[test]
    fn lavapipe_cant_be_shared() {
        let (_, adapter, desc) =
            crate::Gpu::single_queue_parts(&crate::AdapterChoice::Cpu).unwrap();
        let e = open_shared(&adapter, &desc).unwrap_err();
        // Without Vulkan Video's extensions it stops there.
        assert_eq!(e, "the device lacks VK_KHR_video_queue");
        let gpu = crate::Gpu::headless_shared(&crate::AdapterChoice::Cpu).unwrap();
        assert_eq!(gpu.shared_video().unwrap_err(), e);
        assert!(gpu.caps.is_cpu);
        let plain = crate::Gpu::headless(&crate::AdapterChoice::Cpu).unwrap();
        assert_eq!(
            plain.shared_video().unwrap_err(),
            "the device wasn't made to be shared with the decoder"
        );
    }

    #[test]
    fn buffers_lay_planes_out_on_wgpus_row_alignment() {
        let regions = [
            PlaneRegion {
                bytes_per_texel: 1,
                size: (1920, 960),
            },
            PlaneRegion {
                bytes_per_texel: 2,
                size: (960, 480),
            },
        ];
        let (planes, size) = buffer_layout(&regions);
        assert_eq!(
            planes,
            vec![
                BufferPlane {
                    offset: 0,
                    bytes_per_row: 2048,
                    bytes_per_texel: 1,
                    size: (1920, 960)
                },
                BufferPlane {
                    offset: 2048 * 960,
                    bytes_per_row: 2048,
                    bytes_per_texel: 2,
                    size: (960, 480)
                }
            ]
        );
        assert_eq!(size, 2048 * 960 + 2048 * 480);
        // Odd sizes: each row and each plane rounded up to 256 bytes.
        let odd = [
            PlaneRegion {
                bytes_per_texel: 2,
                size: (101, 3),
            },
            PlaneRegion {
                bytes_per_texel: 4,
                size: (51, 2),
            },
        ];
        let (planes, size) = buffer_layout(&odd);
        assert_eq!((planes[0].offset, planes[0].bytes_per_row), (0, 256));
        assert_eq!((planes[1].offset, planes[1].bytes_per_row), (768, 256));
        assert_eq!(size, 768 + 512);
        assert_eq!(buffer_layout(&[]).1, 256);
    }
}
