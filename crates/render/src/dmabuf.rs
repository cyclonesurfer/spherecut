//! DMA-BUF planes into the device (slice 5b's path A, DECISIONS D25): a
//! VA-API picture that FFmpeg exported as DRM PRIME, one layer a plane,
//! each plane imported as a texture over the decoder's own memory, for
//! [`Uploader::upload_imported`](crate::Uploader::upload_imported) to copy
//! or ingest from. Nothing goes through the CPU.
//!
//! The render crate's only `unsafe` (M1 plan section 2.4): the fd
//! borrowed from the frame, what the driver takes asked through ash, and
//! wgpu-hal's import and wrap.

use std::collections::HashMap;
use std::os::fd::BorrowedFd;

use ash::vk;
use spherecut_core::video::{DmabufLayer, fourcc};

use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::plane::{PlaneFormat, SlotShape};

type Vulkan = wgpu::hal::api::Vulkan;

/// What every import is made for: copied from (an 8-bit plane, into its
/// slot's level 0) or read by the ingest pass (a deep one).
pub const USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::COPY_SRC);

/// The state wgpu is told an import starts in: both read-only uses at
/// once, which wgpu-hal takes as `VK_IMAGE_LAYOUT_GENERAL`. So the first
/// use is a barrier out of `GENERAL`, which keeps the decoder's bytes, as
/// a release from a foreign queue leaves them; one out of `UNDEFINED`
/// needn't. wgpu can't express the queue family transfer itself; the
/// layouts [`plan`] takes have no compression to resolve, and Tier 2's
/// `zero_copy_matches_copy_path` holds the result to the copy path's.
const IMPORTED: wgpu::TextureUses = wgpu::TextureUses::COPY_SRC.union(wgpu::TextureUses::RESOURCE);

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
    /// frame's memory, in [`USAGE`], ready for its first use. A texture
    /// holds a dup of its layer's fd. Nothing stops the decoder writing the
    /// memory again: the caller holds the frame until the GPU is done with
    /// the work that reads it (`Queue::on_submitted_work_done`).
    ///
    /// `Import` when the device can't import or the driver refuses the
    /// plane's format, modifier or size, with the reason; the copy path
    /// still takes the frame.
    pub fn import(
        &mut self,
        gpu: &Gpu,
        planes: &[PlaneImport],
    ) -> Result<Vec<wgpu::Texture>, RenderError> {
        if !gpu.caps.dmabuf_import {
            return Err(RenderError::Import(
                "this device has no DMA-BUF import (VK_EXT_external_memory_dma_buf and \
                 VK_EXT_image_drm_format_modifier)"
                    .into(),
            ));
        }
        planes
            .iter()
            .map(|p| {
                self.check(gpu, p)?;
                import_one(gpu, p)
            })
            .collect()
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

/// One plane in: its fd dup'd, imported by wgpu-hal, and wrapped for wgpu.
fn import_one(gpu: &Gpu, p: &PlaneImport) -> Result<wgpu::Texture, RenderError> {
    let l = &p.layer;
    // SAFETY: `plan` takes only fds that aren't negative, and the frame the
    // layer came from holds its fd open while it lives, which is longer
    // than this call (the caller has the frame).
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
    // SAFETY: `raw` was made on this device from `hal_desc`, `desc`'s twin
    // (the same size, format and usage); its memory holds the decoded
    // picture, so it is initialized; and `IMPORTED` is the state its first
    // barrier starts from (see there).
    Ok(unsafe {
        gpu.device
            .create_texture_from_hal::<Vulkan>(raw, &desc, IMPORTED)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::SlotShape;
    use spherecut_core::video::{ColorFamily, PixelLayout, PlaneDesc, SampleBytes};

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

    #[test]
    fn a_device_without_import_says_so() {
        let gpu = Gpu::headless(&crate::AdapterChoice::Cpu).unwrap();
        let shape = SlotShape::of(&layout(SampleBytes::One), (64, 32)).unwrap();
        let planes = plan(
            &shape,
            &[
                layer(fourcc::R8, (64, 32), 64),
                layer(fourcc::GR88, (32, 16), 64),
            ],
        )
        .unwrap();
        // lavapipe has the DMA-BUF extension but not the modifier one, so
        // wgpu doesn't offer the feature (D25).
        assert!(!gpu.caps.dmabuf_import);
        let e = Importer::new().import(&gpu, &planes).unwrap_err();
        assert!(
            matches!(&e, RenderError::Import(m) if m.contains("no DMA-BUF import")),
            "{e}"
        );
        assert_eq!(e.code(), "import_failed");
    }
}
