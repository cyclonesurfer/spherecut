//! The projection pass (M1 plan section 4.3): a slot, seen through a
//! view, into a linear `Rgba16Float` target.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use glam::{DMat3, DQuat};
use spherecut_sphere::perspective::{Lens, Projection};

use crate::error::RenderError;
use crate::frame::GpuFrame;
use crate::gpu::Gpu;
use crate::layouts;
use crate::timers::GpuTimers;

/// Where the virtual camera looks and through what lens. `orientation`
/// turns view space (+X right, +Y up, looking down −Z) into the world
/// ([`spherecut_sphere::orient`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewParams {
    pub orientation: DQuat,
    pub lens: Lens,
}

impl ViewParams {
    pub fn new(orientation: DQuat, lens: Lens) -> ViewParams {
        ViewParams { orientation, lens }
    }
}

/// How the projection samples the source's mips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Filter {
    /// Trilinear at the shader's own LOD: what `spherecut frame`, the
    /// goldens and the CPU reference use.
    #[default]
    Isotropic,
    /// `textureSampleGrad` with a 16× anisotropic sampler: sharper where a
    /// view squeezes the source one way, implementation-defined, the
    /// GUI's default.
    Anisotropic,
}

/// What the projection writes instead of the picture, for checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DebugMode {
    #[default]
    Off,
    /// `textureSample` with the hardware's derivatives: the goldens'
    /// negative control for the seam.
    ImplicitLod,
    /// Level 0 only: the negative control for minification.
    Level0,
    /// The LOD: red luma's and green chroma's as sampled, blue luma's
    /// before clamping. Read it back with [`crate::Depth::Linear`].
    LodHeatmap,
}

impl Filter {
    pub const ALL: [Filter; 2] = [Filter::Isotropic, Filter::Anisotropic];

    fn code(self) -> f64 {
        match self {
            Filter::Isotropic => 0.0,
            Filter::Anisotropic => 1.0,
        }
    }
}

impl DebugMode {
    pub const ALL: [DebugMode; 4] = [
        DebugMode::Off,
        DebugMode::ImplicitLod,
        DebugMode::Level0,
        DebugMode::LodHeatmap,
    ];

    fn code(self) -> f64 {
        match self {
            DebugMode::Off => 0.0,
            DebugMode::ImplicitLod => 1.0,
            DebugMode::Level0 => 2.0,
            DebugMode::LodHeatmap => 3.0,
        }
    }
}

/// The projection shader's `View` uniform (`shaders/project.wgsl`; a naga
/// test checks every offset).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ViewUniform {
    pub rot0: [f32; 4],
    pub rot1: [f32; 4],
    pub rot2: [f32; 4],
    pub size: [f32; 2],
    pub k: [f32; 2],
    pub proj: [u32; 2],
    pub morph: f32,
    pub _pad: f32,
}

impl ViewUniform {
    /// `view` drawn `size` pixels large.
    pub fn new(view: &ViewParams, size: (u32, u32)) -> ViewUniform {
        let m = DMat3::from_quat(view.orientation.normalize());
        let col = |c: glam::DVec3| [c.x as f32, c.y as f32, c.z as f32, 0.0];
        let lens = &view.lens;
        let morph = if lens.from == lens.to {
            0.0
        } else {
            lens.morph.clamp(0.0, 1.0)
        };
        ViewUniform {
            rot0: col(m.x_axis),
            rot1: col(m.y_axis),
            rot2: col(m.z_axis),
            size: [size.0 as f32, size.1 as f32],
            k: [k_of(lens, lens.from) as f32, k_of(lens, lens.to) as f32],
            proj: [proj_code(lens.from), proj_code(lens.to)],
            morph: morph as f32,
            _pad: 0.0,
        }
    }
}

/// The constant of `p`'s θ(ρ) at this lens's field of view, as the
/// shader's `theta_of` takes it.
fn k_of(lens: &Lens, p: Projection) -> f64 {
    let half = lens.half(p);
    match p {
        Projection::Rectilinear => half.tan(),
        Projection::Fisheye => half,
        Projection::Stereographic => (half / 2.0).tan(),
    }
}

fn proj_code(p: Projection) -> u32 {
    match p {
        Projection::Rectilinear => 0,
        Projection::Fisheye => 1,
        Projection::Stereographic => 2,
    }
}

/// The `Rgba16Float` texture a view is projected into, linear light,
/// with the uniforms and bind groups that go with it. Its texture is
/// reallocated only when a view outgrows it ([`LinearTarget::resize`]);
/// the projection draws its top-left `extent`.
#[derive(Debug)]
pub struct LinearTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    extent: (u32, u32),
    view_uniform: wgpu::Buffer,
    view_group: wgpu::BindGroup,
    pub(crate) place: wgpu::Buffer,
    pub(crate) encode_group: wgpu::BindGroup,
}

impl LinearTarget {
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

    /// A target `size` pixels large. `Invalid` for an empty size,
    /// `TooLarge` past the device's textures.
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Result<LinearTarget, RenderError> {
        gpu.check()?;
        Self::new_unchecked(gpu, size)
    }

    /// [`Self::new`] without checking the device first: for the GUI's
    /// viewer, as [`Projector::record_unchecked`].
    pub fn new_unchecked(gpu: &Gpu, size: (u32, u32)) -> Result<LinearTarget, RenderError> {
        if size.0 == 0 || size.1 == 0 {
            return Err(RenderError::Invalid(format!(
                "a {}x{} view has no pixels",
                size.0, size.1
            )));
        }
        gpu.check_size(size)?;
        let device = &gpu.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("linear target"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let view_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view uniform"),
            size: size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("view"),
            layout: &layouts::view(device),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_uniform.as_entire_binding(),
            }],
        });
        let place = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("place uniform"),
            size: size_of::<crate::encode::PlaceUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let encode_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("encode input"),
            layout: &layouts::encode_input(device),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: place.as_entire_binding(),
                },
            ],
        });
        Ok(LinearTarget {
            texture,
            view,
            extent: size,
            view_uniform,
            view_group,
            place,
            encode_group,
        })
    }

    /// Makes the drawn part `size`, reallocating only when the texture is
    /// smaller than that.
    pub fn resize(&mut self, gpu: &Gpu, size: (u32, u32)) -> Result<(), RenderError> {
        self.resize_with(gpu, size, LinearTarget::new)
    }

    /// [`Self::resize`] without checking the device first: for the GUI's
    /// viewer, as [`Projector::record_unchecked`].
    pub fn resize_unchecked(&mut self, gpu: &Gpu, size: (u32, u32)) -> Result<(), RenderError> {
        self.resize_with(gpu, size, LinearTarget::new_unchecked)
    }

    fn resize_with(
        &mut self,
        gpu: &Gpu,
        size: (u32, u32),
        make: fn(&Gpu, (u32, u32)) -> Result<LinearTarget, RenderError>,
    ) -> Result<(), RenderError> {
        let have = (self.texture.width(), self.texture.height());
        if size.0 == 0 || size.1 == 0 {
            return Err(RenderError::Invalid(format!(
                "a {}x{} view has no pixels",
                size.0, size.1
            )));
        }
        if size.0 > have.0 || size.1 > have.1 {
            *self = make(gpu, (size.0.max(have.0), size.1.max(have.1)))?;
        }
        self.extent = size;
        Ok(())
    }

    /// The part the projection draws, from the top left.
    pub fn extent(&self) -> (u32, u32) {
        self.extent
    }

    /// The whole texture, which may be larger than [`LinearTarget::extent`].
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// The projection's pipelines, one per [`Filter`] and [`DebugMode`],
/// built when first used, and its samplers. One serves every slot and
/// target of a device.
#[derive(Debug)]
pub struct Projector {
    gpu: Gpu,
    module: wgpu::ShaderModule,
    view_layout: wgpu::BindGroupLayout,
    source_layout: wgpu::BindGroupLayout,
    sampler_layout: wgpu::BindGroupLayout,
    pipelines: Mutex<HashMap<(Filter, DebugMode), wgpu::RenderPipeline>>,
    /// Indexed by whether U wraps, then by whether it is anisotropic.
    samplers: [[wgpu::BindGroup; 2]; 2],
}

impl Projector {
    pub fn new(gpu: &Gpu) -> Projector {
        let device = &gpu.device;
        let sampler_layout = layouts::sampler(device);
        let sampler = |wrap: bool, aniso: bool| {
            let s = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("projection"),
                address_mode_u: if wrap {
                    wgpu::AddressMode::Repeat
                } else {
                    wgpu::AddressMode::ClampToEdge
                },
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                anisotropy_clamp: if aniso { 16 } else { 1 },
                ..Default::default()
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("projection sampler"),
                layout: &sampler_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&s),
                }],
            })
        };
        let samplers = [
            [sampler(false, false), sampler(false, true)],
            [sampler(true, false), sampler(true, true)],
        ];
        Projector {
            gpu: gpu.clone(),
            module: layouts::module(device, "project", layouts::PROJECT_WGSL),
            view_layout: layouts::view(device),
            source_layout: layouts::source(device),
            sampler_layout,
            pipelines: Mutex::new(HashMap::new()),
            samplers,
        }
    }

    /// Builds the pipeline for `filter` and `debug` now rather than at its
    /// first draw.
    pub fn prepare(&self, filter: Filter, debug: DebugMode) {
        self.pipeline(filter, debug);
    }

    fn pipeline(&self, filter: Filter, debug: DebugMode) -> wgpu::RenderPipeline {
        let mut map = self
            .pipelines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry((filter, debug))
            .or_insert_with(|| {
                layouts::fullscreen_pipeline(
                    &self.gpu.device,
                    "project",
                    &self.module,
                    "fs_project",
                    &[("FILTER", filter.code()), ("DEBUG_MODE", debug.code())],
                    &[&self.view_layout, &self.source_layout, &self.sampler_layout],
                    LinearTarget::FORMAT,
                )
            })
            .clone()
    }

    /// Records `slot` seen through `view` into `target`'s extent, timed
    /// by `timers` when given. The view uniform is written through the
    /// queue, so it takes effect at the next submit: one projection per
    /// target per submission. `Device` when the device is lost
    /// ([`Gpu::check`]).
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        enc: &mut wgpu::CommandEncoder,
        slot: &GpuFrame,
        view: &ViewParams,
        target: &LinearTarget,
        filter: Filter,
        debug: DebugMode,
        timers: Option<&mut GpuTimers>,
    ) -> Result<(), RenderError> {
        self.gpu.check()?;
        self.record_unchecked(enc, slot, view, target, filter, debug, timers);
        Ok(())
    }

    /// [`Self::record`] without checking the device first: for the GUI's
    /// viewer, which records on the UI thread inside egui's frame, where
    /// [`Gpu::check`]'s poll and locks have no place, and which leaves the
    /// device's health to the player's upload thread (a lost device, or
    /// an error no scope caught, comes back as its `GpuFailed`).
    #[allow(clippy::too_many_arguments)]
    pub fn record_unchecked(
        &self,
        enc: &mut wgpu::CommandEncoder,
        slot: &GpuFrame,
        view: &ViewParams,
        target: &LinearTarget,
        filter: Filter,
        debug: DebugMode,
        timers: Option<&mut GpuTimers>,
    ) {
        let (w, h) = target.extent;
        let uniform = ViewUniform::new(view, (w, h));
        self.gpu
            .queue
            .write_buffer(&target.view_uniform, 0, bytemuck::bytes_of(&uniform));
        let pipeline = self.pipeline(filter, debug);
        let wraps = slot.wraps.load(Ordering::Acquire);
        let sampler =
            &self.samplers[usize::from(wraps)][usize::from(filter == Filter::Anisotropic)];
        let mut timers = timers;
        let span = timers.as_deref_mut().and_then(|t| t.begin(enc));
        {
            let mut pass = layouts::fullscreen_pass(enc, "project", &target.view);
            pass.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &target.view_group, &[]);
            pass.set_bind_group(1, &slot.bind_group, &[]);
            pass.set_bind_group(2, sampler, &[]);
            pass.draw(0..3, 0..1);
        }
        if let (Some(t), Some(span)) = (timers, span) {
            t.end(enc, span);
        }
    }
}
