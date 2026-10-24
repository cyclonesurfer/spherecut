//! GPU slots: one uploaded frame each (M1 plan section 4.2).

use spherecut_core::video::ColorInfo;

use crate::color::{Placement, SourceUniform, source_uniform};
use crate::error::RenderError;
use crate::gpu::Gpu;
use crate::layouts;
use crate::plane::SlotShape;

/// One plane's canonical texture and its views.
#[derive(Debug)]
pub(crate) struct PlaneTexture {
    pub(crate) texture: wgpu::Texture,
    /// Every level, for sampling.
    pub(crate) sampled: wgpu::TextureView,
    /// One view per level, for the passes that write it.
    pub(crate) levels: Vec<wgpu::TextureView>,
    /// Level n − 1 for level n's mip pass, from n = 1.
    pub(crate) mip_inputs: Vec<wgpu::BindGroup>,
}

/// A GPU slot: the canonical textures of one frame with their mip chains,
/// its `Source` uniform and the bind group the projection samples it
/// through. The upload thread fills it ([`crate::Uploader`]); the viewer,
/// `render_still` and the export draw from it. Slots are made for a
/// [`SlotShape`] and reused for every frame of that shape (slice 5 keeps
/// four, and reuses one only when nothing holds it).
///
/// Level 0 of each plane is an ordinary texture that a buffer copy, a
/// texture copy or a render pass can fill, so a frame that is already on
/// the GPU (slice 5b's DMA-BUF import or Vulkan Video decode) can fill a
/// slot without going through [`spherecut_core::video::VideoPlanes`]:
/// it writes [`GpuFrame::plane_texture`]'s level 0, records the mips with
/// [`crate::Uploader::record_mips`] and sets the uniform with
/// [`GpuFrame::set_source`], as an upload does.
#[derive(Debug)]
pub struct GpuFrame {
    shape: SlotShape,
    pub(crate) planes: Vec<PlaneTexture>,
    pub(crate) source: wgpu::Buffer,
    pub(crate) bind_group: wgpu::BindGroup,
    /// Whether U wraps for the frame last uploaded (a full, mono
    /// picture), which picks the projection's sampler.
    pub(crate) wraps: std::sync::atomic::AtomicBool,
}

impl GpuFrame {
    /// A slot for frames of `shape`. `TooLarge` when a plane doesn't fit
    /// the device; `OutOfMemory` when the GPU can't hold it, so that a
    /// caller can make do with fewer slots.
    pub fn new(gpu: &Gpu, shape: &SlotShape) -> Result<GpuFrame, RenderError> {
        gpu.check_size(shape.size)?;
        let device = &gpu.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let mip_layout = layouts::mip_input(device);
        let planes: Vec<PlaneTexture> = shape
            .planes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(["slot plane 0", "slot plane 1", "slot plane 2"][i.min(2)]),
                    size: wgpu::Extent3d {
                        width: p.size.0,
                        height: p.size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: p.levels,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: p.format.texture(),
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let sampled = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let levels: Vec<wgpu::TextureView> = (0..p.levels)
                    .map(|l| {
                        texture.create_view(&wgpu::TextureViewDescriptor {
                            base_mip_level: l,
                            mip_level_count: Some(1),
                            ..Default::default()
                        })
                    })
                    .collect();
                let mip_inputs = levels[..levels.len() - 1]
                    .iter()
                    .map(|above| {
                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("mip input"),
                            layout: &mip_layout,
                            entries: &[wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(above),
                            }],
                        })
                    })
                    .collect();
                PlaneTexture {
                    texture,
                    sampled,
                    levels,
                    mip_inputs,
                }
            })
            .collect();
        let source = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("source uniform"),
            size: size_of::<SourceUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // A plane the layout lacks (NV12's third, gray's chroma) is bound
        // to a 1×1 texture the shader reads but doesn't use.
        let filler = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("unused plane"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let view_of = |i: usize| planes.get(i).map_or(&filler, |p| &p.sampled);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("slot"),
            layout: &layouts::source(device),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: source.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view_of(0)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(view_of(1)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(view_of(2)),
                },
            ],
        });
        if pollster::block_on(scope.pop()).is_some() {
            tracing::warn!(shape = %shape.describe(), bytes = shape.texture_bytes(), "no GPU memory for a slot");
            return Err(RenderError::OutOfMemory("a GPU slot"));
        }
        tracing::debug!(shape = %shape.describe(), bytes = shape.texture_bytes(), "GPU slot");
        Ok(GpuFrame {
            shape: shape.clone(),
            planes,
            source,
            bind_group,
            wraps: std::sync::atomic::AtomicBool::new(true),
        })
    }

    pub fn shape(&self) -> &SlotShape {
        &self.shape
    }

    /// Writes the `Source` uniform for a frame of `bit_depth` bits in
    /// this slot's planes, with `color`, placed on the sphere by
    /// `placement`, and picks the sampler that goes with the placement.
    /// It takes effect at the next submit.
    pub fn set_source(&self, gpu: &Gpu, bit_depth: u8, color: &ColorInfo, placement: &Placement) {
        let shape = SlotShape {
            bit_depth,
            ..self.shape.clone()
        };
        let uniform = source_uniform(&shape, color, placement);
        gpu.queue
            .write_buffer(&self.source, 0, bytemuck::bytes_of(&uniform));
        self.wraps
            .store(placement.wraps(), std::sync::atomic::Ordering::Release);
    }

    /// Plane `i`'s canonical texture, all its levels: for a frame that
    /// fills level 0 on the GPU (see the type's docs), and for tests that
    /// read levels back.
    pub fn plane_texture(&self, i: usize) -> Option<&wgpu::Texture> {
        self.planes.get(i).map(|p| &p.texture)
    }
}
