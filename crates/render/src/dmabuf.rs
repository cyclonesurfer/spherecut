//! DMA-BUF planes into the device (slice 5b's path A, DECISIONS D25): a
//! VA-API picture that FFmpeg exported as DRM PRIME, one layer a plane,
//! each plane imported as a texture over the decoder's own memory, for
//! [`Uploader::upload_imported`](crate::Uploader::upload_imported) to copy
//! or ingest from. Nothing goes through the CPU.
//!
//! Each import is acquired from the decoder before anything reads it: a
//! queue family ownership transfer from the foreign queue family (VA-API
//! isn't a Vulkan queue) to wgpu's, recorded raw, since wgpu has no
//! barrier of its own for that ([`acquire_barrier`]).
//!
//! The render crate's first `unsafe` module (M1 plan section 2.4): the fd
//! borrowed from the frame, what the driver takes asked through ash,
//! wgpu-hal's import and wrap, and the acquire.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Deref;
use std::os::fd::BorrowedFd;

use ash::vk;
use spherecut_core::video::{DmabufLayer, VideoPlanes, fourcc};

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::plane::{PlaneFormat, SlotShape};

type Vulkan = wgpu::hal::api::Vulkan;

/// What every import is made for: copied from (an 8-bit plane, into its
/// slot's level 0) or read by the ingest pass (a deep one).
pub const USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::COPY_SRC);

/// The state wgpu is told an import starts in: both read-only uses at
/// once, which wgpu-hal takes as `VK_IMAGE_LAYOUT_GENERAL`
/// ([`imported_layout`]). [`Importer::import`] acquires each plane into
/// that layout before anything reads it, so wgpu's first barrier on the
/// import starts where the acquire left it, and keeps the decoder's
/// bytes; one out of `UNDEFINED` needn't keep them.
const IMPORTED: wgpu::TextureUses = wgpu::TextureUses::COPY_SRC.union(wgpu::TextureUses::RESOURCE);

/// The layout wgpu-hal tracks an import of `format` in from the start
/// (that of `COPY_SRC` and `RESOURCE` together, the state wgpu is told
/// it is made in), which the acquire leaves it in.
pub fn imported_layout(format: wgpu::TextureFormat) -> vk::ImageLayout {
    wgpu::hal::vulkan::conv::derive_image_layout(IMPORTED, format)
}

/// The queue family an import is acquired from. The decoder isn't a
/// Vulkan queue, so it is the foreign one where the device has
/// `VK_EXT_queue_family_foreign` (`foreign`), as every device Spherecut
/// makes has where the driver lists it ([`crate::vkshare`] makes them
/// all). Only a device made elsewhere (one wgpu opened itself) lacks it,
/// and can name only the external one, which FFmpeg's own DRM import
/// uses too, though Vulkan means that for another Vulkan instance on the
/// same driver.
pub fn acquired_from(foreign: bool) -> u32 {
    if foreign {
        vk::QUEUE_FAMILY_FOREIGN_EXT
    } else {
        vk::QUEUE_FAMILY_EXTERNAL
    }
}

/// The barrier that acquires an imported plane, `image` of `format`,
/// from the decoder ([`acquired_from`]) for `family`, wgpu's queue
/// family, its one level and layer. The layout is the same on both sides,
/// [`imported_layout`]'s `GENERAL`: the decoder left its bytes in the
/// layout their modifier gives, which Vulkan has no name for but
/// `GENERAL` (as it says outright for a transfer from D3D11 memory, which
/// may start in `GENERAL` or `UNDEFINED`), and a transition out of
/// `UNDEFINED` may drop them. The source's stage and access are the
/// releasing queue's, outside Vulkan, so none are named; the destination
/// is every later read, which wgpu's own first barrier chains from.
pub fn acquire_barrier(
    image: vk::Image,
    format: wgpu::TextureFormat,
    family: u32,
    foreign: bool,
) -> vk::ImageMemoryBarrier<'static> {
    let layout = imported_layout(format);
    vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::MEMORY_READ)
        .old_layout(layout)
        .new_layout(layout)
        .src_queue_family_index(acquired_from(foreign))
        .dst_queue_family_index(family)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
}

/// One plane's import, checked against the frame's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneImport {
    /// [`imported_format`] of the plane's.
    pub format: wgpu::TextureFormat,
    pub layer: DmabufLayer,
}

/// What a plane of `format` is imported as: an 8-bit plane as itself, a
/// deep one as the integers the ingest pass reads.
pub fn imported_format(format: PlaneFormat) -> wgpu::TextureFormat {
    format.scratch().unwrap_or(format.texture())
}

/// The DRM fourcc of a plane of `format`'s layer.
pub fn layer_fourcc(format: PlaneFormat) -> u32 {
    match format {
        PlaneFormat::R8 => fourcc::R8,
        PlaneFormat::Rg8 => fourcc::GR88,
        PlaneFormat::R16F => fourcc::R16,
        PlaneFormat::Rg16F => fourcc::GR1616,
    }
}

/// What a frame of `whole` (its shape at its full size) imports from
/// `layers`, checked before anything is imported: a layer a plane, each
/// with an fd, of its plane's fourcc and size, with rows no shorter than
/// the plane's. Asks nothing of the driver; [`Importer::import`] does.
pub fn plan(whole: &SlotShape, layers: &[DmabufLayer]) -> Result<Vec<PlaneImport>, String> {
    if layers.len() != whole.planes.len() {
        return Err(format!(
            "{} layers for {} planes",
            layers.len(),
            whole.planes.len()
        ));
    }
    whole
        .planes
        .iter()
        .zip(layers)
        .enumerate()
        .map(|(i, (p, l))| {
            if l.fd < 0 {
                return Err(format!("layer {i} has no fd"));
            }
            let want = layer_fourcc(p.format);
            if l.fourcc != want {
                return Err(format!(
                    "layer {i} is {} where {} was expected",
                    fourcc::name(l.fourcc),
                    fourcc::name(want)
                ));
            }
            if l.size != p.size {
                return Err(format!(
                    "layer {i} is {}x{} where plane {i} is {}x{}",
                    l.size.0, l.size.1, p.size.0, p.size.1
                ));
            }
            let row = u64::from(p.size.0) * u64::from(p.format.bytes_per_texel());
            if l.pitch < row {
                return Err(format!(
                    "layer {i}'s rows are {} bytes apart, under its {row}-byte rows",
                    l.pitch
                ));
            }
            Ok(PlaneImport {
                format: imported_format(p.format),
                layer: *l,
            })
        })
        .collect()
}

/// The textures [`Importer::import`] made, one a plane, over the memory of
/// the frame it borrowed: while they live, the frame does, and with it
/// the fds and the decoder's surface under them (the project review's
/// F120). They read as a slice of textures, as
/// [`Uploader::upload_imported`](crate::Uploader::upload_imported) and
/// [`StillRenderer::render_imported`](crate::StillRenderer::render_imported)
/// take them.
#[derive(Debug)]
pub struct Imported<'f> {
    textures: Vec<wgpu::Texture>,
    /// The borrow of the frame, which needn't be `Sync` for the textures
    /// to be sent.
    frame: PhantomData<&'f ()>,
}

impl Deref for Imported<'_> {
    type Target = [wgpu::Texture];

    fn deref(&self) -> &[wgpu::Texture] {
        &self.textures
    }
}

/// Imports planes into one device, remembering what the driver said of
/// each format and modifier, so it is asked once.
#[derive(Debug, Default)]
pub struct Importer {
    /// The largest image the driver imports, or why it doesn't.
    support: HashMap<(wgpu::TextureFormat, u64), Result<(u32, u32), String>>,
}

impl Importer {
    pub fn new() -> Importer {
        Importer::default()
    }

    /// Imports each plane `planes` holds as a texture of its own over the
    /// memory of `frame`, the frame whose layers they are, in [`USAGE`],
    /// and acquires them from the decoder for wgpu's queue in a submission
    /// of their own, ahead of anything that reads them. A texture holds a
    /// dup of its layer's fd, but the decoder writes the memory again once
    /// the frame is let go, so the textures borrow `frame`: it can't be
    /// let go while they live. The GPU may read them after, so the caller
    /// holds the frame on until the work that reads them is done
    /// (`Queue::on_submitted_work_done`).
    ///
    /// `Invalid` when a plane isn't one of `frame`'s layers. `Import` when
    /// the device can't import or the driver refuses the plane's format,
    /// modifier or size, with the reason; the copy path still takes the
    /// frame.
    ///
    /// The frame outlives its textures:
    ///
    /// ```
    /// # use spherecut_core::video::VideoPlanes;
    /// # use spherecut_render::dmabuf::{Importer, PlaneImport};
    /// # use spherecut_render::{Gpu, RenderError};
    /// fn upload(
    ///     gpu: &Gpu,
    ///     frame: Box<dyn VideoPlanes>,
    ///     planes: &[PlaneImport],
    /// ) -> Result<(), RenderError> {
    ///     let textures = Importer::new().import(gpu, frame.as_ref(), planes)?;
    ///     assert_eq!(textures.len(), planes.len());
    ///     drop(textures);
    ///     drop(frame);
    ///     Ok(())
    /// }
    /// ```
    ///
    /// and a frame let go while they live doesn't compile:
    ///
    /// ```compile_fail,E0505
    /// # use spherecut_core::video::VideoPlanes;
    /// # use spherecut_render::Gpu;
    /// # use spherecut_render::dmabuf::{Importer, PlaneImport};
    /// fn misuse(gpu: &Gpu, frame: Box<dyn VideoPlanes>, planes: &[PlaneImport]) {
    ///     let textures = Importer::new().import(gpu, frame.as_ref(), planes).unwrap();
    ///     drop(frame);
    ///     assert_eq!(textures.len(), planes.len());
    /// }
    /// ```
    pub fn import<'f>(
        &mut self,
        gpu: &Gpu,
        frame: &'f dyn VideoPlanes,
        planes: &[PlaneImport],
    ) -> Result<Imported<'f>, RenderError> {
        let layers = frame.dmabuf().unwrap_or_default();
        if let Some(i) = planes.iter().position(|p| !layers.contains(&p.layer)) {
            return Err(RenderError::Invalid(format!(
                "plane {i} to import isn't one of the frame's {} DMA-BUF layers",
                layers.len()
            )));
        }
        if !gpu.caps.dmabuf_import {
            return Err(RenderError::Import(
                "this device has no DMA-BUF import (VK_EXT_external_memory_dma_buf and \
                 VK_EXT_image_drm_format_modifier)"
                    .into(),
            ));
        }
        let imported = planes
            .iter()
            .map(|p| {
                self.check(gpu, p)?;
                import_one(gpu, p)
            })
            .collect::<Result<Vec<_>, RenderError>>()?;
        acquire(gpu, &imported)?;
        Ok(Imported {
            textures: imported.into_iter().map(|(_, t)| t).collect(),
            frame: PhantomData,
        })
    }

    /// `Import` unless the driver takes `p`'s format and modifier, as
    /// large as `p` is.
    fn check(&mut self, gpu: &Gpu, p: &PlaneImport) -> Result<(), RenderError> {
        let key = (p.format, p.layer.modifier);
        let max = self
            .support
            .entry(key)
            .or_insert_with(|| ask_driver(gpu, p.format, p.layer.modifier))
            .clone()
            .map_err(RenderError::Import)?;
        let (w, h) = p.layer.size;
        if w > max.0 || h > max.1 {
            return Err(RenderError::Import(format!(
                "the driver imports {:?} with modifier {:#x} up to {}x{}, not {w}x{h}",
                p.format, p.layer.modifier, max.0, max.1
            )));
        }
        Ok(())
    }
}

/// The Vulkan format wgpu-hal makes `format` as (only the four
/// [`imported_format`] gives).
fn vk_format(format: wgpu::TextureFormat) -> Option<vk::Format> {
    match format {
        wgpu::TextureFormat::R8Unorm => Some(vk::Format::R8_UNORM),
        wgpu::TextureFormat::Rg8Unorm => Some(vk::Format::R8G8_UNORM),
        wgpu::TextureFormat::R16Uint => Some(vk::Format::R16_UINT),
        wgpu::TextureFormat::Rg16Uint => Some(vk::Format::R16G16_UINT),
        _ => None,
    }
}

/// Whether the driver imports a DMA-BUF of `format` with `modifier` as
/// a single-plane image made as wgpu-hal makes it (2D, no flags, sampled
/// and copied from), and up to what size; or why not.
fn ask_driver(gpu: &Gpu, format: wgpu::TextureFormat, modifier: u64) -> Result<(u32, u32), String> {
    let what = format!("{format:?} with modifier {modifier:#x}");
    let vk_format = vk_format(format).ok_or_else(|| format!("{format:?} isn't a plane format"))?;
    // SAFETY: the guard is only read from, and dropped at the end of this
    // function; nothing here destroys the device.
    let device = unsafe { gpu.device.as_hal::<Vulkan>() }
        .ok_or_else(|| "the device isn't a Vulkan one".to_owned())?;
    let shared = device.shared_instance();
    if shared.instance_api_version() < vk::API_VERSION_1_1 {
        return Err("the Vulkan instance is 1.0, without the queries an import needs".into());
    }
    let instance = shared.raw_instance();
    let physical = device.raw_physical_device();

    // The modifiers the driver has for the format: their count, then them.
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    // SAFETY: `physical` is the device's own, and `props` is a valid chain
    // whose list has no array, which asks for the count.
    unsafe { instance.get_physical_device_format_properties2(physical, vk_format, &mut props) };
    let mut modifiers = vec![
        vk::DrmFormatModifierPropertiesEXT::default();
        list.drm_format_modifier_count as usize
    ];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
        .drm_format_modifier_properties(&mut modifiers);
    let mut props = vk::FormatProperties2::default().push_next(&mut list);
    // SAFETY: as above, with an array as long as the count it said.
    unsafe { instance.get_physical_device_format_properties2(physical, vk_format, &mut props) };
    let count = (list.drm_format_modifier_count as usize).min(modifiers.len());
    let Some(m) = modifiers[..count]
        .iter()
        .find(|m| m.drm_format_modifier == modifier)
    else {
        return Err(format!("the driver has no {what}"));
    };
    if m.drm_format_modifier_plane_count != 1 {
        return Err(format!(
            "the driver lays {what} out in {} memory planes",
            m.drm_format_modifier_plane_count
        ));
    }
    let need = vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::TRANSFER_SRC;
    if !m.drm_format_modifier_tiling_features.contains(need) {
        return Err(format!("the driver can't sample and copy from {what}"));
    }

    // Whether such an image can be imported from a DMA-BUF, and how large.
    let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(vk_format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
        .push_next(&mut modifier_info)
        .push_next(&mut external_info);
    let mut external = vk::ExternalImageFormatProperties::default();
    let mut image = vk::ImageFormatProperties2::default().push_next(&mut external);
    // SAFETY: valid chains on both sides, on the device's own physical
    // device; the driver has the modifier extension (the feature says so).
    unsafe { instance.get_physical_device_image_format_properties2(physical, &info, &mut image) }
        .map_err(|e| format!("the driver can't import {what}: {e}"))?;
    let max = image.image_format_properties.max_extent;
    let features = external.external_memory_properties.external_memory_features;
    if !features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE) {
        return Err(format!("the driver can't import {what} from a DMA-BUF"));
    }
    Ok((max.width, max.height))
}

/// Acquires `planes` (each an import's `VkImage` and its texture) from the
/// decoder for wgpu's queue ([`acquire_barrier`]), submitted on that queue
/// ahead of anything that reads them. The barrier is recorded raw into an
/// encoder of its own, since wgpu won't mix raw commands with its own in
/// one; the textures are kept until the GPU has done it.
fn acquire(gpu: &Gpu, planes: &[(vk::Image, wgpu::Texture)]) -> Result<(), RenderError> {
    let (raw, family, foreign) = acquire_parts(gpu)?;
    let barriers: Vec<vk::ImageMemoryBarrier<'_>> = planes
        .iter()
        .map(|(image, t)| acquire_barrier(*image, t.format(), family, foreign))
        .collect();
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dmabuf acquire"),
        });
    // SAFETY: the encoder is wgpu's, open, and recorded into only here; the
    // barriers name this device's images, of this queue family, which
    // live until the work is done (kept below). The foreign family is
    // named only when the device has its extension.
    let recorded = unsafe {
        enc.as_hal_mut::<Vulkan, _, _>(|e| {
            let e = e?;
            raw.cmd_pipeline_barrier(
                e.raw_handle(),
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers,
            );
            Some(())
        })
    };
    if recorded.is_none() {
        return Err(RenderError::Import(
            "wgpu's encoder isn't a Vulkan one, to acquire an import on".into(),
        ));
    }
    let keep: Vec<wgpu::Texture> = planes.iter().map(|(_, t)| t.clone()).collect();
    gpu.submit_gate().wait();
    gpu.queue.submit([enc.finish()]);
    gpu.queue.on_submitted_work_done(move || drop(keep));
    Ok(())
}

/// What an acquire on `gpu` needs: its raw device, wgpu's queue family,
/// and whether the device has `VK_EXT_queue_family_foreign`.
fn acquire_parts(gpu: &Gpu) -> Result<(ash::Device, u32, bool), RenderError> {
    // SAFETY: the guard is dropped at the end of this function, and the
    // device isn't destroyed while `gpu` lives, which is longer than the
    // raw device's clone is used.
    let device = unsafe { gpu.device.as_hal::<Vulkan>() }
        .ok_or_else(|| RenderError::Import("the device isn't a Vulkan one".into()))?;
    Ok((
        device.raw_device().clone(),
        device.queue_family_index(),
        device
            .enabled_device_extensions()
            .contains(&ash::ext::queue_family_foreign::NAME),
    ))
}

/// One plane in: its fd dup'd, imported by wgpu-hal, and wrapped for wgpu;
/// its `VkImage` too, for the acquire.
fn import_one(gpu: &Gpu, p: &PlaneImport) -> Result<(vk::Image, wgpu::Texture), RenderError> {
    let l = &p.layer;
    // SAFETY: `plan` takes only fds that aren't negative, and the frame the
    // layer came from holds its fd open while it lives, which is longer
    // than this call: `Importer::import` borrows the frame, and found the
    // layer among its own.
    let fd = unsafe { BorrowedFd::borrow_raw(l.fd) }
        .try_clone_to_owned()
        .map_err(|e| RenderError::Import(format!("duplicating a DMA-BUF's fd: {e}")))?;
    let size = wgpu::Extent3d {
        width: l.size.0,
        height: l.size.1,
        depth_or_array_layers: 1,
    };
    let hal_desc = wgpu::hal::TextureDescriptor {
        label: Some("dmabuf plane"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: p.format,
        usage: wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    let raw = {
        // SAFETY: the guard is dropped at the end of this block, and the
        // device isn't destroyed while it lives.
        let device = unsafe { gpu.device.as_hal::<Vulkan>() }
            .ok_or_else(|| RenderError::Import("the device isn't a Vulkan one".into()))?;
        // SAFETY: the device has `VULKAN_EXTERNAL_MEMORY_DMA_BUF` (the
        // caller checked `dmabuf_import`); `fd` is the DMA-BUF FFmpeg
        // exported, whose layout (modifier, pitch, offset) the layer
        // gives as FFmpeg read it from the driver; and `ask_driver` found
        // the driver takes that format and modifier for this usage, at
        // this size. The import takes the fd, or closes it on failure.
        unsafe { device.texture_from_dmabuf_fd(fd, &hal_desc, l.modifier, l.pitch, l.offset) }
            .map_err(|e| {
                RenderError::Import(format!(
                    "importing a {:?} plane with modifier {:#x}: {e}",
                    p.format, l.modifier
                ))
            })?
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("dmabuf plane"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: p.format,
        usage: USAGE,
        view_formats: &[],
    };
    // SAFETY: the handle is only named in the acquire's barrier, while the
    // texture that owns it lives.
    let image = unsafe { raw.raw_handle() };
    // SAFETY: `raw` was made on this device from `hal_desc`, `desc`'s twin
    // (the same size, format and usage); its memory holds the decoded
    // picture, so it is initialized; and `IMPORTED` is the state the
    // acquire leaves it in, which `import` records ahead of anything that
    // uses the texture.
    let texture = unsafe {
        gpu.device
            .create_texture_from_hal::<Vulkan>(raw, &desc, IMPORTED)
    };
    Ok((image, texture))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::SlotShape;
    use ash::vk::Handle;
    use spherecut_core::video::{
        Assumed, ChromaSiting, ColorCodes, ColorFamily, ColorInfo, Matrix, PixelLayout, PlaneData,
        PlaneDesc, Primaries, Range, SampleBytes, Transfer,
    };

    fn layout(bytes: SampleBytes) -> PixelLayout {
        let plane = |components, log2_sub| PlaneDesc {
            components,
            bytes,
            log2_sub,
        };
        PixelLayout {
            name: "nv12".into(),
            planes: vec![plane(1, (0, 0)), plane(2, (1, 1))],
            family: ColorFamily::Yuv,
            bit_depth: if bytes == SampleBytes::Two { 10 } else { 8 },
            msb_aligned: bytes == SampleBytes::Two,
        }
    }

    fn layer(fourcc: u32, size: (u32, u32), pitch: u64) -> DmabufLayer {
        DmabufLayer {
            fd: 7,
            fourcc,
            modifier: 0,
            offset: 0,
            pitch,
            size,
        }
    }

    /// A frame that says the decoder exported it as `layers`, with no
    /// bytes of its own: what an import borrows.
    #[derive(Debug)]
    struct Exported {
        layout: PixelLayout,
        size: (u32, u32),
        color: ColorInfo,
        layers: Vec<DmabufLayer>,
    }

    impl Exported {
        fn new(size: (u32, u32), layers: Vec<DmabufLayer>) -> Exported {
            Exported {
                layout: layout(SampleBytes::One),
                size,
                color: ColorInfo {
                    codes: ColorCodes::UNSPECIFIED,
                    range: Range::Limited,
                    matrix: Matrix::Bt709,
                    transfer: Transfer::Sdr,
                    primaries: Primaries::Bt709,
                    siting: ChromaSiting::Left,
                    assumed: Assumed::default(),
                },
                layers,
            }
        }
    }

    impl VideoPlanes for Exported {
        fn layout(&self) -> &PixelLayout {
            &self.layout
        }

        fn size(&self) -> (u32, u32) {
            self.size
        }

        fn plane(&self, _: usize) -> Option<PlaneData<'_>> {
            None
        }

        fn color(&self) -> &ColorInfo {
            &self.color
        }

        fn dmabuf(&self) -> Option<&[DmabufLayer]> {
            Some(&self.layers)
        }
    }

    #[test]
    fn a_plan_maps_layers_to_formats_and_refuses_what_doesnt_fit() {
        let nv12 = SlotShape::of(&layout(SampleBytes::One), (7680, 3840)).unwrap();
        let layers = [
            layer(fourcc::R8, (7680, 3840), 7680),
            layer(fourcc::GR88, (3840, 1920), 7680),
        ];
        let got = plan(&nv12, &layers).unwrap();
        assert_eq!(
            got.iter().map(|p| p.format).collect::<Vec<_>>(),
            [wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm]
        );
        assert_eq!(got[1].layer, layers[1]);

        let p010 = SlotShape::of(&layout(SampleBytes::Two), (5760, 2880)).unwrap();
        let deep = [
            layer(fourcc::R16, (5760, 2880), 11776),
            layer(fourcc::GR1616, (2880, 1440), 11776),
        ];
        assert_eq!(
            plan(&p010, &deep)
                .unwrap()
                .iter()
                .map(|p| p.format)
                .collect::<Vec<_>>(),
            [wgpu::TextureFormat::R16Uint, wgpu::TextureFormat::Rg16Uint]
        );

        let refused = |shape: &SlotShape, layers: &[DmabufLayer], why: &str| {
            let e = plan(shape, layers).unwrap_err();
            assert!(e.contains(why), "{e}");
        };
        refused(&nv12, &layers[..1], "1 layers for 2 planes");
        refused(&nv12, &deep, "layer 0 is R16 where R8 was expected");
        refused(
            &nv12,
            &[layers[0], layer(fourcc::GR88, (3840, 1921), 7680)],
            "layer 1 is 3840x1921 where plane 1 is 3840x1920",
        );
        refused(
            &p010,
            &[deep[0], layer(fourcc::GR1616, (2880, 1440), 11519)],
            "under its 11520-byte rows",
        );
        let mut no_fd = layers;
        no_fd[1].fd = -1;
        refused(&nv12, &no_fd, "layer 1 has no fd");
    }

    #[test]
    fn every_plane_format_has_a_fourcc_and_an_import_format() {
        for f in PlaneFormat::ALL {
            let t = imported_format(f);
            assert!(vk_format(t).is_some(), "{f:?}");
            assert_eq!(t.block_copy_size(None), Some(f.bytes_per_texel()), "{f:?}");
            let ints = matches!(
                t.sample_type(None, None),
                Some(wgpu::TextureSampleType::Uint)
            );
            assert_eq!(f.is_deep(), ints, "{f:?}");
        }
        assert_eq!(fourcc::name(layer_fourcc(PlaneFormat::Rg16F)), "GR32");
    }

    /// Why `b` isn't the acquire of a plane of `format` for `family` on a
    /// device with the foreign queue family's extension or without
    /// (`foreign`): ownership moves from the decoder's family (foreign,
    /// or external without the extension) to `family`; the layout, the
    /// same on both sides, isn't `UNDEFINED` and is the one wgpu-hal
    /// tracks the import in; the whole image, its colour aspect; and a
    /// read after it.
    fn acquire_fault(
        b: &vk::ImageMemoryBarrier<'_>,
        format: wgpu::TextureFormat,
        family: u32,
        foreign: bool,
    ) -> Result<(), String> {
        let from = if foreign {
            vk::QUEUE_FAMILY_FOREIGN_EXT
        } else {
            vk::QUEUE_FAMILY_EXTERNAL
        };
        if (b.src_queue_family_index, b.dst_queue_family_index) != (from, family) {
            return Err(format!(
                "from family {:#x} to {:#x}, not {from:#x} to {family:#x}",
                b.src_queue_family_index, b.dst_queue_family_index
            ));
        }
        let tracked = wgpu::hal::vulkan::conv::derive_image_layout(IMPORTED, format);
        if b.old_layout != b.new_layout
            || b.new_layout != tracked
            || b.old_layout == vk::ImageLayout::UNDEFINED
        {
            return Err(format!(
                "{:?} to {:?}, where wgpu tracks {tracked:?}",
                b.old_layout, b.new_layout
            ));
        }
        let r = b.subresource_range;
        if (
            r.aspect_mask,
            r.base_mip_level,
            r.level_count,
            r.base_array_layer,
            r.layer_count,
        ) != (vk::ImageAspectFlags::COLOR, 0, 1, 0, 1)
        {
            return Err(format!("{r:?} isn't the whole plane"));
        }
        if !b
            .dst_access_mask
            .intersects(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::TRANSFER_READ)
        {
            return Err(format!("{:?} reads nothing after", b.dst_access_mask));
        }
        Ok(())
    }

    #[test]
    fn an_import_is_acquired_from_the_decoder_where_wgpu_tracks_it() {
        let image = vk::Image::from_raw(0x1234);
        for f in PlaneFormat::ALL {
            let format = imported_format(f);
            // wgpu-hal tracks an import in GENERAL, which the decoder's
            // bytes are in.
            assert_eq!(imported_layout(format), vk::ImageLayout::GENERAL, "{f:?}");
            for foreign in [true, false] {
                let b = acquire_barrier(image, format, 3, foreign);
                acquire_fault(&b, format, 3, foreign).unwrap();
                assert_eq!(b.image, image);
            }
        }
        // Its controls: the barrier as wgpu alone made it before (no
        // ownership moved), out of UNDEFINED, into another layout than
        // wgpu tracks, to another family, and from the external family
        // where the foreign one is there.
        let format = wgpu::TextureFormat::R8Unorm;
        let good = acquire_barrier(image, format, 3, true);
        let fault = |b: vk::ImageMemoryBarrier<'static>| acquire_fault(&b, format, 3, true);
        let control = spherecut_testkit::control::must_fail;
        control(
            "no transfer",
            fault(good.src_queue_family_index(0).dst_queue_family_index(0)),
        );
        control(
            "out of UNDEFINED",
            fault(good.old_layout(vk::ImageLayout::UNDEFINED)),
        );
        control(
            "into TRANSFER_SRC_OPTIMAL",
            fault(good.new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)),
        );
        control("for family 0", fault(good.dst_queue_family_index(0)));
        control(
            "from the external family",
            fault(good.src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)),
        );
        control(
            "only a level of it",
            fault(good.subresource_range(vk::ImageSubresourceRange {
                level_count: 0,
                ..good.subresource_range
            })),
        );
    }

    /// A texture made as an import is, wgpu told it starts in
    /// [`IMPORTED`], without the DMA-BUF (lavapipe imports none), and its
    /// image.
    fn stand_in(gpu: &Gpu, format: wgpu::TextureFormat) -> (vk::Image, wgpu::Texture) {
        use wgpu::hal::Device as _;
        let size = wgpu::Extent3d {
            width: 64,
            height: 32,
            depth_or_array_layers: 1,
        };
        let hal_desc = wgpu::hal::TextureDescriptor {
            label: Some("stand-in import"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        // SAFETY: a valid descriptor on the live device; the guard goes at
        // the end of the statement.
        let raw = unsafe {
            gpu.device
                .as_hal::<Vulkan>()
                .unwrap()
                .create_texture(&hal_desc)
                .unwrap()
        };
        // SAFETY: as `import_one`'s.
        let image = unsafe { raw.raw_handle() };
        let desc = wgpu::TextureDescriptor {
            label: Some("stand-in import"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: USAGE,
            view_formats: &[],
        };
        // SAFETY: made from `hal_desc`, `desc`'s twin; its bytes are
        // never compared.
        let texture = unsafe {
            gpu.device
                .create_texture_from_hal::<Vulkan>(raw, &desc, IMPORTED)
        };
        (image, texture)
    }

    /// `planes` copied out by wgpu as the upload reads them, to a buffer,
    /// under a validation error scope, and waited for: the error wgpu
    /// raised, if any. The scope sees wgpu's own validation only, never
    /// a barrier recorded through `as_hal_mut`.
    fn copy_out(gpu: &Gpu, planes: &[&wgpu::Texture]) -> Option<wgpu::Error> {
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let out = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 32 * planes.len() as u64,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for (i, t) in planes.iter().enumerate() {
            enc.copy_texture_to_buffer(
                t.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &out,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 256 * 32 * i as u64,
                        bytes_per_row: Some(256),
                        rows_per_image: None,
                    },
                },
                t.size(),
            );
        }
        gpu.queue.submit([enc.finish()]);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        pollster::block_on(scope.pop())
    }

    /// The acquire names the foreign family on a shared-device maker's
    /// device and on [`Gpu::headless`]'s, and can't on one wgpu opened
    /// itself (the check and its control); then a smoke run on lavapipe,
    /// which imports no DMA-BUF: on stand-ins for the planes
    /// ([`stand_in`]) the acquire records and submits on wgpu's queue, on
    /// all three devices, and wgpu then copies the planes out without an
    /// error of its own, where a plane it can't copy from raises one (the
    /// scope's control). A wrong barrier, or the
    /// order of the acquire and wgpu's reads, is invisible here: wgpu's
    /// error scope sees its own validation only, never the raw barrier,
    /// and this box has no Khronos validation layer. Real imports, and the
    /// barrier under the validation layer, are odin's to run.
    #[test]
    fn the_acquire_names_its_family_and_submits_on_wgpus_queue() {
        // A shared-device maker's device, which has the foreign family's
        // extension where the driver lists it, as lavapipe does.
        // SAFETY: one thing at a time on the device, from this thread.
        let gpu = unsafe { crate::vkshare::single_queue_gpu(&crate::AdapterChoice::Cpu) }.unwrap();
        // The check: the acquire names the foreign family.
        let from_foreign = |gpu: &Gpu| -> Result<(), String> {
            let (_, _, foreign) = acquire_parts(gpu).map_err(|e| e.to_string())?;
            match acquired_from(foreign) {
                vk::QUEUE_FAMILY_FOREIGN_EXT => Ok(()),
                other => Err(format!("acquired from family {other:#x}")),
            }
        };
        from_foreign(&gpu).unwrap();
        // Every other device Spherecut makes names it too.
        let headless = Gpu::headless(&crate::AdapterChoice::Cpu).unwrap();
        from_foreign(&headless).unwrap();
        // Its control: a device wgpu opened itself, from the same
        // descriptor, can't name it.
        let (instance, adapter, desc) =
            Gpu::single_queue_parts(&crate::AdapterChoice::Cpu).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&desc)).unwrap();
        let plain = Gpu::from_parts(instance, adapter, device, queue);
        spherecut_testkit::control::must_fail("wgpu's own device", from_foreign(&plain));
        // The smoke run, on the shared device and the headless one (from
        // the foreign family) and on wgpu's own (from the external one).
        for gpu in [&gpu, &headless, &plain] {
            let planes = [
                stand_in(gpu, wgpu::TextureFormat::R8Unorm),
                stand_in(gpu, wgpu::TextureFormat::Rg8Unorm),
            ];
            acquire(gpu, &planes).unwrap();
            let error = copy_out(gpu, &[&planes[0].1, &planes[1].1]);
            assert!(error.is_none(), "{error:?}");
        }
        // The scope's control: a plane wgpu can't copy from.
        let uncopyable = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no COPY_SRC"),
            size: wgpu::Extent3d {
                width: 64,
                height: 32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        spherecut_testkit::control::must_fail(
            "a plane wgpu can't copy from",
            copy_out(&gpu, &[&uncopyable]).is_none(),
        );
    }

    #[test]
    fn a_device_without_import_says_so() {
        let mut gpu = Gpu::headless(&crate::AdapterChoice::Cpu).unwrap();
        let shape = SlotShape::of(&layout(SampleBytes::One), (64, 32)).unwrap();
        let layers = vec![
            layer(fourcc::R8, (64, 32), 64),
            layer(fourcc::GR88, (32, 16), 64),
        ];
        let planes = plan(&shape, &layers).unwrap();
        let frame = Exported::new((64, 32), layers);
        // This box's lavapipe has the DMA-BUF extension but not the
        // modifier one, so wgpu doesn't offer the feature (D25); odin's,
        // Mesa 26.2.5's, has both. The refusal is the importer's, so the
        // device here is made one without.
        gpu.caps.dmabuf_import = false;
        let e = Importer::new().import(&gpu, &frame, &planes).unwrap_err();
        assert!(
            matches!(&e, RenderError::Import(m) if m.contains("no DMA-BUF import")),
            "{e}"
        );
        assert_eq!(e.code(), "import_failed");
    }

    /// The frame an import borrows is the one whose memory the planes are
    /// (F120): planes of another frame are refused before anything asks
    /// the device, whose fds that frame needn't hold open.
    #[test]
    fn planes_of_another_frame_are_refused() {
        let mut gpu = Gpu::headless(&crate::AdapterChoice::Cpu).unwrap();
        // Nothing here reaches the driver: the layers' fds are made up.
        gpu.caps.dmabuf_import = false;
        let shape = SlotShape::of(&layout(SampleBytes::One), (64, 32)).unwrap();
        let layers = vec![
            layer(fourcc::R8, (64, 32), 64),
            layer(fourcc::GR88, (32, 16), 64),
        ];
        let planes = plan(&shape, &layers).unwrap();
        let foreign = |frame: &Exported| {
            let e = Importer::new()
                .import(&gpu, frame, &planes)
                .map(|t| t.len());
            matches!(&e, Err(RenderError::Invalid(m)) if m.contains("isn't one of the frame's"))
        };
        // The same layers on another fd, and a frame not exported at all.
        let other = Exported::new(
            (64, 32),
            layers.iter().map(|l| DmabufLayer { fd: 8, ..*l }).collect(),
        );
        assert!(foreign(&other), "another frame's planes");
        assert!(foreign(&Exported::new((64, 32), Vec::new())), "no layers");
        // Control: the frame's own planes pass that check, to the device's.
        let own = Exported::new((64, 32), layers);
        spherecut_testkit::control::must_fail("the frame's own planes", foreign(&own));
    }
}
